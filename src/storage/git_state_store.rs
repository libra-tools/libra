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
    git_store::{
        BridgePublicationPhase, BridgePublicationRecord, is_valid_bridge_operation_id,
        validate_hex_oid, validate_non_empty_identifier,
    },
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

/// Static manifest of the 4 `ConfigStore` / `MetadataStore` repository source
/// tables (`RB-03`).
pub const CONFIG_STORE_REPO_SOURCE_TABLES: [&str; 4] =
    ["schema_versions", "config", "config_kv", "metadata_kv"];

/// Static manifest of the 7 `GitStore` core repository source tables
/// (`RB-04` + `RB-04b`).
pub const GIT_STORE_CORE_SOURCE_TABLES: [&str; 7] = [
    "reference",
    "reflog",
    "notes",
    "object_index",
    "object_obliteration",
    "revision_ordinal",
    "revision_ordinal_meta",
];

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

/// Static manifest of the 29 AI, memory, governance/audit, and bridge
/// two-phase publication repository source tables completed in `RB-05b`
/// (28 newly added to `GitStateStore` plus `agent_bridge_publication` shared
/// with `GitStore`).
pub const RB05B_AI_MEMORY_BRIDGE_SOURCE_TABLES: [&str; 29] = [
    // 19 `ai_*` tables
    "ai_thread",
    "ai_thread_participant",
    "ai_thread_intent",
    "ai_thread_provider_metadata",
    "ai_scheduler_state",
    "ai_scheduler_plan_head",
    "ai_scheduler_selected_plan",
    "ai_live_context_window",
    "ai_index_intent_plan",
    "ai_index_intent_task",
    "ai_index_plan_step_task",
    "ai_index_task_run",
    "ai_index_run_event",
    "ai_index_run_patchset",
    "ai_index_intent_context_frame",
    "ai_validation_report",
    "ai_risk_score_breakdown",
    "ai_decision_proposal",
    "ai_final_decision",
    // 3 governance & audit tables
    "automation_log",
    "approved_permission",
    "source_call_log",
    // 5 `memory_*` projection tables
    "memory_episode",
    "memory_episode_evidence",
    "memory_projection_state",
    "memory_episode_path",
    "memory_episode_search_doc",
    // 2 bridge two-phase publication tables
    "agent_bridge_publication",
    "agent_bridge_index_publication",
];

/// The 7 retired repository tables explicitly excluded from the 61 active
/// persistent repository source table manifest (`plan-20261121`清单 A).
pub const RETIRED_REPOSITORY_SOURCE_TABLES: [&str; 7] = [
    "legacy_operation",
    "legacy_operation_parent",
    "legacy_operation_view",
    "legacy_operation_view_ref",
    "legacy_operation_view_workspace",
    "cherry_pick_state",
    "revert_sequence",
];

/// Exact count of active persistent Repository source tables (`4 + 7 + 21 + 29 = 61`).
pub const REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT: usize = 61;

/// Canonical same-transaction redb secondary index name for `memory_episode_path`
/// lookups ordered by `(code_path, ended_at DESC, episode_id ASC)` (`RB-05b`).
pub const MEMORY_BY_PATH_INDEX_NAME: &str = "MEMORY_BY_PATH";

/// Returns the deduplicated, lexicographically sorted manifest of all 61 active
/// persistent Repository source tables across `ConfigStore`, `GitStore`, and
/// `GitStateStore`.
#[must_use]
pub fn repository_active_source_tables() -> Vec<&'static str> {
    let mut set = BTreeSet::new();
    set.extend(CONFIG_STORE_REPO_SOURCE_TABLES);
    set.extend(GIT_STORE_CORE_SOURCE_TABLES);
    set.extend(RB05_WORKFLOW_STATE_SOURCE_TABLES);
    set.extend(RB05B_AI_MEMORY_BRIDGE_SOURCE_TABLES);
    set.into_iter().collect()
}

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
// Group 6 (`RB-05b` AC1): 19 `ai_*` Thread, Scheduler, Runtime-Contract & Index Tables
// =============================================================================

/// One row in `ai_thread`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiThreadRecord {
    pub thread_id: String,
    pub title: Option<String>,
    pub owner_kind: String,
    pub owner_id: String,
    pub owner_display_name: Option<String>,
    pub current_intent_id: Option<String>,
    pub latest_intent_id: Option<String>,
    pub metadata_json: Option<String>,
    pub archived: bool,
    pub version: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One row in `ai_thread_participant`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiThreadParticipantRecord {
    pub thread_id: String,
    pub actor_kind: String,
    pub actor_id: String,
    pub actor_display_name: Option<String>,
    pub role: String,
    pub joined_at: i64,
}

/// One row in `ai_thread_intent`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiThreadIntentRecord {
    pub thread_id: String,
    pub intent_id: String,
    pub ordinal: i64,
    pub is_head: bool,
    pub linked_at: i64,
    pub link_reason: String,
}

/// One row in `ai_thread_provider_metadata`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiThreadProviderMetadataRecord {
    pub thread_id: String,
    pub legacy_session_id: Option<String>,
    pub provider_thread_id: Option<String>,
    pub provider_kind: Option<String>,
    pub metadata_json: Option<String>,
    pub updated_at: i64,
}

/// One row in `ai_scheduler_state`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSchedulerStateRecord {
    pub thread_id: String,
    pub selected_plan_id: Option<String>,
    pub active_task_id: Option<String>,
    pub active_run_id: Option<String>,
    pub metadata_json: Option<String>,
    pub version: i64,
    pub updated_at: i64,
}

/// One row in `ai_scheduler_plan_head`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSchedulerPlanHeadRecord {
    pub thread_id: String,
    pub plan_id: String,
    pub ordinal: i64,
}

/// One row in `ai_scheduler_selected_plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSchedulerSelectedPlanRecord {
    pub thread_id: String,
    pub plan_id: String,
    pub ordinal: i64,
}

/// One row in `ai_live_context_window`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiLiveContextWindowRecord {
    pub thread_id: String,
    pub context_frame_id: String,
    pub position: i64,
    pub source_kind: String,
    pub pin_kind: Option<String>,
    pub inserted_at: i64,
}

/// One row in `ai_index_intent_plan`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexIntentPlanRecord {
    pub intent_id: String,
    pub plan_id: String,
    pub created_at: i64,
}

/// One row in `ai_index_intent_task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexIntentTaskRecord {
    pub intent_id: String,
    pub task_id: String,
    pub parent_task_id: Option<String>,
    pub origin_step_id: Option<String>,
    pub created_at: i64,
}

/// One row in `ai_index_plan_step_task`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexPlanStepTaskRecord {
    pub plan_id: String,
    pub task_id: String,
    pub step_id: String,
    pub created_at: i64,
}

/// One row in `ai_index_task_run` (including additive `base_commit_ref` from `2026090701`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexTaskRunRecord {
    pub task_id: String,
    pub run_id: String,
    pub is_latest: bool,
    pub created_at: i64,
    pub base_commit_ref: Option<String>,
}

/// One row in `ai_index_run_event`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexRunEventRecord {
    pub run_id: String,
    pub event_id: String,
    pub event_kind: String,
    pub is_latest: bool,
    pub created_at: i64,
}

/// One row in `ai_index_run_patchset`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexRunPatchsetRecord {
    pub run_id: String,
    pub patchset_id: String,
    pub sequence: i64,
    pub is_latest: bool,
    pub created_at: i64,
}

/// One row in `ai_index_intent_context_frame`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiIndexIntentContextFrameRecord {
    pub intent_id: String,
    pub context_frame_id: String,
    pub relation_kind: String,
    pub created_at: i64,
}

/// One row in `ai_validation_report`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiValidationReportRecord {
    pub report_id: String,
    pub thread_id: String,
    pub run_id: Option<String>,
    pub policy_version: String,
    pub stale: bool,
    pub is_latest: bool,
    pub summary_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One row in `ai_risk_score_breakdown`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRiskScoreBreakdownRecord {
    pub breakdown_id: String,
    pub thread_id: String,
    pub validation_report_id: Option<String>,
    pub policy_version: String,
    pub stale: bool,
    pub is_latest: bool,
    pub summary_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One row in `ai_decision_proposal`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiDecisionProposalRecord {
    pub proposal_id: String,
    pub thread_id: String,
    pub validation_report_id: Option<String>,
    pub risk_score_breakdown_id: Option<String>,
    pub policy_version: String,
    pub stale: bool,
    pub is_latest: bool,
    pub summary_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

/// One row in `ai_final_decision` (`2026053101`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiFinalDecisionRecord {
    pub decision_id: String,
    pub thread_id: String,
    pub decision_proposal_id: Option<String>,
    pub validation_report_id: Option<String>,
    pub policy_version: String,
    pub verdict: String,
    pub stale: bool,
    pub is_latest: bool,
    pub summary_json: String,
    pub created_at: i64,
    pub updated_at: i64,
}

// =============================================================================
// Group 7 (`RB-05b` AC2 & AC3): 5 `memory_*` Tables + `MEMORY_BY_PATH` Index
// =============================================================================

/// Storage spelling matching `memory_episode.source_kind` and `memory_projection_state.source_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemorySourceKind {
    Commit,
    AgentSession,
    AgentRun,
    BridgeOperation,
}

impl MemorySourceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::AgentSession => "agent_session",
            Self::AgentRun => "agent_run",
            Self::BridgeOperation => "bridge_operation",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "commit" => Ok(Self::Commit),
            "agent_session" => Ok(Self::AgentSession),
            "agent_run" => Ok(Self::AgentRun),
            "bridge_operation" => Ok(Self::BridgeOperation),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid memory source_kind value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `memory_episode.outcome`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryOutcome {
    Succeeded,
    Failed,
    Aborted,
    Partial,
    Unknown,
}

impl MemoryOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Aborted => "aborted",
            Self::Partial => "partial",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "aborted" => Ok(Self::Aborted),
            "partial" => Ok(Self::Partial),
            "unknown" => Ok(Self::Unknown),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid memory_episode outcome value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `memory_episode_evidence.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryEvidenceKind {
    Commit,
    Checkpoint,
    ReviewRun,
    Operation,
}

impl MemoryEvidenceKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Checkpoint => "checkpoint",
            Self::ReviewRun => "review_run",
            Self::Operation => "operation",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "commit" => Ok(Self::Commit),
            "checkpoint" => Ok(Self::Checkpoint),
            "review_run" => Ok(Self::ReviewRun),
            "operation" => Ok(Self::Operation),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid memory_episode_evidence kind value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `memory_episode_evidence.link_confidence`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryLinkConfidence {
    Identity,
    Operation,
    Temporal,
}

impl MemoryLinkConfidence {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Identity => "identity",
            Self::Operation => "operation",
            Self::Temporal => "temporal",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "identity" => Ok(Self::Identity),
            "operation" => Ok(Self::Operation),
            "temporal" => Ok(Self::Temporal),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid memory_episode_evidence link_confidence value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `memory_episode_evidence.resolution_status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryResolutionStatus {
    Resolved,
    Unresolved,
}

impl MemoryResolutionStatus {
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
                detail: "invalid memory_episode_evidence resolution_status value".to_string(),
            }),
        }
    }
}

/// Storage spelling matching `memory_episode_path.change_kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum MemoryPathChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
}

impl MemoryPathChangeKind {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Modified => "modified",
            Self::Deleted => "deleted",
            Self::Renamed => "renamed",
        }
    }

    pub fn parse(raw: &str) -> StorageResult<Self> {
        match raw {
            "added" => Ok(Self::Added),
            "modified" => Ok(Self::Modified),
            "deleted" => Ok(Self::Deleted),
            "renamed" => Ok(Self::Renamed),
            _ => Err(StorageBackendError::BackendInternal {
                detail: "invalid memory_episode_path change_kind value".to_string(),
            }),
        }
    }
}

/// One row in `memory_episode` (`2026092901`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEpisodeRecord {
    pub episode_id: String,
    pub repo_id: String,
    pub source_kind: MemorySourceKind,
    pub source_key: String,
    pub outcome: MemoryOutcome,
    pub actor: Option<String>,
    pub started_at: i64,
    pub ended_at: i64,
    pub anchor_commit: Option<String>,
    pub change_id: Option<String>,
    pub title: String,
    pub body: String,
    pub content_digest: String,
    pub producer: String,
    pub rules_version: i64,
}

/// One row in `memory_episode_evidence` (`2026092901`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEpisodeEvidenceRecord {
    pub episode_id: String,
    pub ordinal: i64,
    pub kind: MemoryEvidenceKind,
    pub ref_id: String,
    pub link_confidence: MemoryLinkConfidence,
    pub resolution_status: MemoryResolutionStatus,
}

/// One row in `memory_projection_state` (`2026092901`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryProjectionStateRecord {
    pub repo_id: String,
    pub source_kind: MemorySourceKind,
    pub cursor_json: String,
    pub fingerprint: String,
    pub rules_version: i64,
    pub schema_version: i64,
    pub horizon_truncated: bool,
    pub revoked_count: i64,
    pub aged_out_count: i64,
    pub rebuilt_at: i64,
}

/// One row in `memory_episode_path` (`2026092902`).
///
/// `code_path` is raw Git path bytes (non-empty and must not start with `b'/'`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEpisodePathRecord {
    pub episode_id: String,
    pub code_path: Vec<u8>,
    pub change_kind: MemoryPathChangeKind,
    pub blob_oid_at_end: Option<String>,
    pub ended_at: i64,
}

/// One row in `memory_episode_search_doc` (`2026092902`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryEpisodeSearchDocRecord {
    pub rowid: i64,
    pub episode_id: String,
    pub title: String,
    pub body: String,
    pub paths_text: String,
}

// =============================================================================
// Group 8 (`RB-05b` AC4): Governance & Audit (`approved_permission`, `automation_log`, `source_call_log`)
// =============================================================================

/// One row in `approved_permission` (`2026050601` + `2026081301`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovedPermissionRecord {
    pub project_id: String,
    pub permission: String,
    pub pattern: String,
    pub created_at: i64,
    pub source_worktree_id: String,
    pub source_session_id: String,
    pub source_workspace_id: String,
}

/// One row in `automation_log` (`2026050301`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AutomationLogRecord {
    pub id: String,
    pub rule_id: String,
    pub trigger_kind: String,
    pub action_kind: String,
    pub status: String,
    pub message: String,
    pub started_at: String,
    pub finished_at: String,
    pub details_json: String,
}

/// One row in `source_call_log` (`2026052301` + `2026060201`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceCallLogRecord {
    pub id: String,
    pub session_id: String,
    pub source_slug: String,
    pub tool_name: String,
    pub registered_tool_name: String,
    pub tool_call_id: String,
    pub credential_ref: Option<String>,
    pub latency_ms: Option<i64>,
    pub input_bytes: i64,
    pub output_bytes: i64,
    pub cost_estimate_micros: Option<i64>,
    pub approval_decision: Option<String>,
    pub state_namespace: String,
    pub success: bool,
    pub created_at: String,
    pub agent_run_id: Option<String>,
}

// =============================================================================
// Group 9 (`RB-05b` AC5): Bridge Two-Phase Index Publication
// =============================================================================

/// One row in `agent_bridge_index_publication` (`agent_bridge/publication.rs:36`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeIndexPublicationRecord {
    pub operation_id: String,
    pub index_digest: String,
    pub phase: BridgePublicationPhase,
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

pub(crate) fn validate_ai_thread(rec: &AiThreadRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_thread thread_id")?;
    validate_opt_no_nul(rec.title.as_deref(), "ai_thread title")?;
    validate_non_empty_identifier(&rec.owner_kind, "ai_thread owner_kind")?;
    validate_non_empty_identifier(&rec.owner_id, "ai_thread owner_id")?;
    validate_opt_no_nul(
        rec.owner_display_name.as_deref(),
        "ai_thread owner_display_name",
    )?;
    validate_opt_no_nul(
        rec.current_intent_id.as_deref(),
        "ai_thread current_intent_id",
    )?;
    validate_opt_no_nul(
        rec.latest_intent_id.as_deref(),
        "ai_thread latest_intent_id",
    )?;
    validate_opt_no_nul(rec.metadata_json.as_deref(), "ai_thread metadata_json")
}

pub(crate) fn validate_ai_thread_participant(rec: &AiThreadParticipantRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_thread_participant thread_id")?;
    validate_non_empty_identifier(&rec.actor_kind, "ai_thread_participant actor_kind")?;
    validate_non_empty_identifier(&rec.actor_id, "ai_thread_participant actor_id")?;
    validate_opt_no_nul(
        rec.actor_display_name.as_deref(),
        "ai_thread_participant actor_display_name",
    )?;
    validate_non_empty_identifier(&rec.role, "ai_thread_participant role")
}

pub(crate) fn validate_ai_thread_intent(rec: &AiThreadIntentRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_thread_intent thread_id")?;
    validate_non_empty_identifier(&rec.intent_id, "ai_thread_intent intent_id")?;
    validate_non_empty_identifier(&rec.link_reason, "ai_thread_intent link_reason")
}

pub(crate) fn validate_ai_thread_provider_metadata(
    rec: &AiThreadProviderMetadataRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_thread_provider_metadata thread_id")?;
    validate_opt_no_nul(
        rec.legacy_session_id.as_deref(),
        "ai_thread_provider_metadata legacy_session_id",
    )?;
    validate_opt_no_nul(
        rec.provider_thread_id.as_deref(),
        "ai_thread_provider_metadata provider_thread_id",
    )?;
    validate_opt_no_nul(
        rec.provider_kind.as_deref(),
        "ai_thread_provider_metadata provider_kind",
    )?;
    validate_opt_no_nul(
        rec.metadata_json.as_deref(),
        "ai_thread_provider_metadata metadata_json",
    )
}

pub(crate) fn validate_ai_scheduler_state(rec: &AiSchedulerStateRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_scheduler_state thread_id")?;
    validate_opt_no_nul(
        rec.selected_plan_id.as_deref(),
        "ai_scheduler_state selected_plan_id",
    )?;
    validate_opt_no_nul(
        rec.active_task_id.as_deref(),
        "ai_scheduler_state active_task_id",
    )?;
    validate_opt_no_nul(
        rec.active_run_id.as_deref(),
        "ai_scheduler_state active_run_id",
    )?;
    validate_opt_no_nul(
        rec.metadata_json.as_deref(),
        "ai_scheduler_state metadata_json",
    )
}

pub(crate) fn validate_ai_scheduler_plan_head(
    rec: &AiSchedulerPlanHeadRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_scheduler_plan_head thread_id")?;
    validate_non_empty_identifier(&rec.plan_id, "ai_scheduler_plan_head plan_id")
}

pub(crate) fn validate_ai_scheduler_selected_plan(
    rec: &AiSchedulerSelectedPlanRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_scheduler_selected_plan thread_id")?;
    validate_non_empty_identifier(&rec.plan_id, "ai_scheduler_selected_plan plan_id")
}

pub(crate) fn validate_ai_live_context_window(
    rec: &AiLiveContextWindowRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.thread_id, "ai_live_context_window thread_id")?;
    validate_non_empty_identifier(
        &rec.context_frame_id,
        "ai_live_context_window context_frame_id",
    )?;
    validate_non_empty_identifier(&rec.source_kind, "ai_live_context_window source_kind")?;
    validate_opt_no_nul(rec.pin_kind.as_deref(), "ai_live_context_window pin_kind")
}

pub(crate) fn validate_ai_index_intent_plan(rec: &AiIndexIntentPlanRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.intent_id, "ai_index_intent_plan intent_id")?;
    validate_non_empty_identifier(&rec.plan_id, "ai_index_intent_plan plan_id")
}

pub(crate) fn validate_ai_index_intent_task(rec: &AiIndexIntentTaskRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.intent_id, "ai_index_intent_task intent_id")?;
    validate_non_empty_identifier(&rec.task_id, "ai_index_intent_task task_id")?;
    validate_opt_no_nul(
        rec.parent_task_id.as_deref(),
        "ai_index_intent_task parent_task_id",
    )?;
    validate_opt_no_nul(
        rec.origin_step_id.as_deref(),
        "ai_index_intent_task origin_step_id",
    )
}

pub(crate) fn validate_ai_index_plan_step_task(
    rec: &AiIndexPlanStepTaskRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.plan_id, "ai_index_plan_step_task plan_id")?;
    validate_non_empty_identifier(&rec.task_id, "ai_index_plan_step_task task_id")?;
    validate_non_empty_identifier(&rec.step_id, "ai_index_plan_step_task step_id")
}

pub(crate) fn validate_ai_index_task_run(rec: &AiIndexTaskRunRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.task_id, "ai_index_task_run task_id")?;
    validate_non_empty_identifier(&rec.run_id, "ai_index_task_run run_id")?;
    validate_opt_no_nul(
        rec.base_commit_ref.as_deref(),
        "ai_index_task_run base_commit_ref",
    )
}

pub(crate) fn validate_ai_index_run_event(rec: &AiIndexRunEventRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.run_id, "ai_index_run_event run_id")?;
    validate_non_empty_identifier(&rec.event_id, "ai_index_run_event event_id")?;
    validate_non_empty_identifier(&rec.event_kind, "ai_index_run_event event_kind")
}

pub(crate) fn validate_ai_index_run_patchset(rec: &AiIndexRunPatchsetRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.run_id, "ai_index_run_patchset run_id")?;
    validate_non_empty_identifier(&rec.patchset_id, "ai_index_run_patchset patchset_id")
}

pub(crate) fn validate_ai_index_intent_context_frame(
    rec: &AiIndexIntentContextFrameRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.intent_id, "ai_index_intent_context_frame intent_id")?;
    validate_non_empty_identifier(
        &rec.context_frame_id,
        "ai_index_intent_context_frame context_frame_id",
    )?;
    validate_non_empty_identifier(
        &rec.relation_kind,
        "ai_index_intent_context_frame relation_kind",
    )
}

pub(crate) fn validate_ai_validation_report(rec: &AiValidationReportRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.report_id, "ai_validation_report report_id")?;
    validate_non_empty_identifier(&rec.thread_id, "ai_validation_report thread_id")?;
    validate_opt_no_nul(rec.run_id.as_deref(), "ai_validation_report run_id")?;
    validate_non_empty_identifier(&rec.policy_version, "ai_validation_report policy_version")?;
    validate_no_nul(&rec.summary_json, "ai_validation_report summary_json")
}

pub(crate) fn validate_ai_risk_score_breakdown(
    rec: &AiRiskScoreBreakdownRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.breakdown_id, "ai_risk_score_breakdown breakdown_id")?;
    validate_non_empty_identifier(&rec.thread_id, "ai_risk_score_breakdown thread_id")?;
    validate_opt_no_nul(
        rec.validation_report_id.as_deref(),
        "ai_risk_score_breakdown validation_report_id",
    )?;
    validate_non_empty_identifier(
        &rec.policy_version,
        "ai_risk_score_breakdown policy_version",
    )?;
    validate_no_nul(&rec.summary_json, "ai_risk_score_breakdown summary_json")
}

pub(crate) fn validate_ai_decision_proposal(rec: &AiDecisionProposalRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.proposal_id, "ai_decision_proposal proposal_id")?;
    validate_non_empty_identifier(&rec.thread_id, "ai_decision_proposal thread_id")?;
    validate_opt_no_nul(
        rec.validation_report_id.as_deref(),
        "ai_decision_proposal validation_report_id",
    )?;
    validate_opt_no_nul(
        rec.risk_score_breakdown_id.as_deref(),
        "ai_decision_proposal risk_score_breakdown_id",
    )?;
    validate_non_empty_identifier(&rec.policy_version, "ai_decision_proposal policy_version")?;
    validate_no_nul(&rec.summary_json, "ai_decision_proposal summary_json")
}

pub(crate) fn validate_ai_final_decision(rec: &AiFinalDecisionRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.decision_id, "ai_final_decision decision_id")?;
    validate_non_empty_identifier(&rec.thread_id, "ai_final_decision thread_id")?;
    validate_opt_no_nul(
        rec.decision_proposal_id.as_deref(),
        "ai_final_decision decision_proposal_id",
    )?;
    validate_opt_no_nul(
        rec.validation_report_id.as_deref(),
        "ai_final_decision validation_report_id",
    )?;
    validate_non_empty_identifier(&rec.policy_version, "ai_final_decision policy_version")?;
    validate_non_empty_identifier(&rec.verdict, "ai_final_decision verdict")?;
    validate_no_nul(&rec.summary_json, "ai_final_decision summary_json")
}

pub(crate) fn validate_memory_episode(rec: &MemoryEpisodeRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.episode_id, "memory_episode episode_id")?;
    validate_non_empty_identifier(&rec.repo_id, "memory_episode repo_id")?;
    validate_non_empty_identifier(&rec.source_key, "memory_episode source_key")?;
    validate_opt_no_nul(rec.actor.as_deref(), "memory_episode actor")?;
    validate_opt_hex_oid(rec.anchor_commit.as_deref(), "memory_episode anchor_commit")?;
    validate_opt_no_nul(rec.change_id.as_deref(), "memory_episode change_id")?;
    validate_no_nul(&rec.title, "memory_episode title")?;
    validate_no_nul(&rec.body, "memory_episode body")?;
    validate_non_empty_identifier(&rec.content_digest, "memory_episode content_digest")?;
    validate_non_empty_identifier(&rec.producer, "memory_episode producer")
}

pub(crate) fn validate_memory_episode_evidence(
    rec: &MemoryEpisodeEvidenceRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.episode_id, "memory_episode_evidence episode_id")?;
    validate_non_empty_identifier(&rec.ref_id, "memory_episode_evidence ref_id")
}

pub(crate) fn validate_memory_projection_state(
    rec: &MemoryProjectionStateRecord,
) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.repo_id, "memory_projection_state repo_id")?;
    validate_no_nul(&rec.cursor_json, "memory_projection_state cursor_json")?;
    validate_non_empty_identifier(&rec.fingerprint, "memory_projection_state fingerprint")
}

pub(crate) fn validate_memory_code_path(code_path: &[u8]) -> StorageResult<()> {
    if code_path.is_empty() || code_path[0] == b'/' {
        return Err(StorageBackendError::BackendInternal {
            detail: "memory_episode_path code_path must be non-empty and relative (must not start with '/')"
                .to_string(),
        });
    }
    Ok(())
}

pub(crate) fn validate_memory_episode_path(rec: &MemoryEpisodePathRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.episode_id, "memory_episode_path episode_id")?;
    validate_memory_code_path(&rec.code_path)?;
    validate_opt_hex_oid(
        rec.blob_oid_at_end.as_deref(),
        "memory_episode_path blob_oid_at_end",
    )
}

pub(crate) fn validate_approved_permission(rec: &ApprovedPermissionRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.project_id, "approved_permission project_id")?;
    validate_non_empty_identifier(&rec.permission, "approved_permission permission")?;
    validate_non_empty_identifier(&rec.pattern, "approved_permission pattern")?;
    validate_no_nul(
        &rec.source_worktree_id,
        "approved_permission source_worktree_id",
    )?;
    validate_no_nul(
        &rec.source_session_id,
        "approved_permission source_session_id",
    )?;
    validate_no_nul(
        &rec.source_workspace_id,
        "approved_permission source_workspace_id",
    )
}

pub(crate) fn validate_automation_log(rec: &AutomationLogRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.id, "automation_log id")?;
    validate_non_empty_identifier(&rec.rule_id, "automation_log rule_id")?;
    validate_non_empty_identifier(&rec.trigger_kind, "automation_log trigger_kind")?;
    validate_non_empty_identifier(&rec.action_kind, "automation_log action_kind")?;
    validate_non_empty_identifier(&rec.status, "automation_log status")?;
    validate_no_nul(&rec.message, "automation_log message")?;
    validate_non_empty_identifier(&rec.started_at, "automation_log started_at")?;
    validate_non_empty_identifier(&rec.finished_at, "automation_log finished_at")?;
    validate_no_nul(&rec.details_json, "automation_log details_json")
}

pub(crate) fn validate_source_call_log(rec: &SourceCallLogRecord) -> StorageResult<()> {
    validate_non_empty_identifier(&rec.id, "source_call_log id")?;
    validate_non_empty_identifier(&rec.session_id, "source_call_log session_id")?;
    validate_non_empty_identifier(&rec.source_slug, "source_call_log source_slug")?;
    validate_non_empty_identifier(&rec.tool_name, "source_call_log tool_name")?;
    validate_non_empty_identifier(
        &rec.registered_tool_name,
        "source_call_log registered_tool_name",
    )?;
    validate_non_empty_identifier(&rec.tool_call_id, "source_call_log tool_call_id")?;
    validate_opt_no_nul(
        rec.credential_ref.as_deref(),
        "source_call_log credential_ref",
    )?;
    validate_opt_no_nul(
        rec.approval_decision.as_deref(),
        "source_call_log approval_decision",
    )?;
    validate_non_empty_identifier(&rec.state_namespace, "source_call_log state_namespace")?;
    validate_non_empty_identifier(&rec.created_at, "source_call_log created_at")?;
    validate_opt_no_nul(rec.agent_run_id.as_deref(), "source_call_log agent_run_id")
}

pub(crate) fn validate_bridge_operation_id(operation_id: &str) -> StorageResult<()> {
    if !is_valid_bridge_operation_id(operation_id) {
        return Err(StorageBackendError::BackendInternal {
            detail: "bridge publication operation_id is empty or contains invalid characters"
                .to_string(),
        });
    }
    Ok(())
}

/// Order-reversing bijection `i64 -> u64` so ascending B-tree iteration over
/// `(code_path, invert_ended_at(ended_at), episode_id)` visits `ended_at DESC,
/// episode_id ASC` without overflow even at `i64::MIN` / `i64::MAX`.
#[cfg(feature = "storage-redb")]
#[must_use]
pub(crate) const fn invert_ended_at(ts: i64) -> u64 {
    !((ts as u64) ^ (1u64 << 63))
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

    // -------------------------------------------------------------------------
    // RB-05b AC1: 19 `ai_*` Thread, Scheduler, Runtime-Contract & Index Tables
    // -------------------------------------------------------------------------

    async fn upsert_ai_thread(&self, record: &AiThreadRecord) -> StorageResult<()>;
    async fn get_ai_thread(&self, thread_id: &str) -> StorageResult<Option<AiThreadRecord>>;
    async fn list_ai_threads(
        &self,
        archived_filter: Option<bool>,
    ) -> StorageResult<Vec<AiThreadRecord>>;
    async fn delete_ai_thread(&self, thread_id: &str) -> StorageResult<bool>;

    async fn upsert_ai_thread_participant(
        &self,
        record: &AiThreadParticipantRecord,
    ) -> StorageResult<()>;
    async fn list_ai_thread_participants(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiThreadParticipantRecord>>;

    async fn upsert_ai_thread_intent(&self, record: &AiThreadIntentRecord) -> StorageResult<()>;
    async fn list_ai_thread_intents(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiThreadIntentRecord>>;

    async fn upsert_ai_thread_provider_metadata(
        &self,
        record: &AiThreadProviderMetadataRecord,
    ) -> StorageResult<()>;
    async fn get_ai_thread_provider_metadata(
        &self,
        thread_id: &str,
    ) -> StorageResult<Option<AiThreadProviderMetadataRecord>>;

    async fn upsert_ai_scheduler_state(&self, record: &AiSchedulerStateRecord)
    -> StorageResult<()>;
    async fn get_ai_scheduler_state(
        &self,
        thread_id: &str,
    ) -> StorageResult<Option<AiSchedulerStateRecord>>;

    async fn replace_ai_scheduler_plan_heads(
        &self,
        thread_id: &str,
        heads: &[AiSchedulerPlanHeadRecord],
    ) -> StorageResult<()>;
    async fn list_ai_scheduler_plan_heads(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiSchedulerPlanHeadRecord>>;

    async fn replace_ai_scheduler_selected_plans(
        &self,
        thread_id: &str,
        plans: &[AiSchedulerSelectedPlanRecord],
    ) -> StorageResult<()>;
    async fn list_ai_scheduler_selected_plans(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiSchedulerSelectedPlanRecord>>;

    async fn replace_ai_live_context_window(
        &self,
        thread_id: &str,
        entries: &[AiLiveContextWindowRecord],
    ) -> StorageResult<()>;
    async fn list_ai_live_context_window(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiLiveContextWindowRecord>>;

    async fn upsert_ai_index_intent_plan(
        &self,
        record: &AiIndexIntentPlanRecord,
    ) -> StorageResult<()>;
    async fn list_ai_index_intent_plans(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentPlanRecord>>;

    async fn upsert_ai_index_intent_task(
        &self,
        record: &AiIndexIntentTaskRecord,
    ) -> StorageResult<()>;
    async fn list_ai_index_intent_tasks(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentTaskRecord>>;

    async fn upsert_ai_index_plan_step_task(
        &self,
        record: &AiIndexPlanStepTaskRecord,
    ) -> StorageResult<()>;
    async fn list_ai_index_plan_step_tasks(
        &self,
        plan_id: &str,
    ) -> StorageResult<Vec<AiIndexPlanStepTaskRecord>>;

    async fn upsert_ai_index_task_run(&self, record: &AiIndexTaskRunRecord) -> StorageResult<()>;
    async fn list_ai_index_task_runs(
        &self,
        task_id: &str,
    ) -> StorageResult<Vec<AiIndexTaskRunRecord>>;

    async fn upsert_ai_index_run_event(&self, record: &AiIndexRunEventRecord) -> StorageResult<()>;
    async fn list_ai_index_run_events(
        &self,
        run_id: &str,
    ) -> StorageResult<Vec<AiIndexRunEventRecord>>;

    async fn upsert_ai_index_run_patchset(
        &self,
        record: &AiIndexRunPatchsetRecord,
    ) -> StorageResult<()>;
    async fn list_ai_index_run_patchsets(
        &self,
        run_id: &str,
    ) -> StorageResult<Vec<AiIndexRunPatchsetRecord>>;

    async fn upsert_ai_index_intent_context_frame(
        &self,
        record: &AiIndexIntentContextFrameRecord,
    ) -> StorageResult<()>;
    async fn list_ai_index_intent_context_frames(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentContextFrameRecord>>;

    async fn upsert_ai_validation_report(
        &self,
        record: &AiValidationReportRecord,
    ) -> StorageResult<()>;
    async fn list_ai_validation_reports(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiValidationReportRecord>>;

    async fn upsert_ai_risk_score_breakdown(
        &self,
        record: &AiRiskScoreBreakdownRecord,
    ) -> StorageResult<()>;
    async fn list_ai_risk_score_breakdowns(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiRiskScoreBreakdownRecord>>;

    async fn upsert_ai_decision_proposal(
        &self,
        record: &AiDecisionProposalRecord,
    ) -> StorageResult<()>;
    async fn list_ai_decision_proposals(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiDecisionProposalRecord>>;

    async fn upsert_ai_final_decision(&self, record: &AiFinalDecisionRecord) -> StorageResult<()>;
    async fn list_ai_final_decisions(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiFinalDecisionRecord>>;

    // -------------------------------------------------------------------------
    // RB-05b AC2 & AC3: 5 `memory_*` Tables + `MEMORY_BY_PATH` Secondary Index
    // -------------------------------------------------------------------------

    async fn upsert_memory_episode(&self, record: &MemoryEpisodeRecord) -> StorageResult<()>;
    async fn get_memory_episode(
        &self,
        episode_id: &str,
    ) -> StorageResult<Option<MemoryEpisodeRecord>>;
    async fn list_memory_episodes(&self, repo_id: &str) -> StorageResult<Vec<MemoryEpisodeRecord>>;
    async fn delete_memory_episode(&self, episode_id: &str) -> StorageResult<bool>;

    async fn replace_memory_episode_evidence(
        &self,
        episode_id: &str,
        evidence: &[MemoryEpisodeEvidenceRecord],
    ) -> StorageResult<()>;
    async fn list_memory_episode_evidence(
        &self,
        episode_id: &str,
    ) -> StorageResult<Vec<MemoryEpisodeEvidenceRecord>>;

    async fn upsert_memory_projection_state(
        &self,
        record: &MemoryProjectionStateRecord,
    ) -> StorageResult<()>;
    async fn get_memory_projection_state(
        &self,
        repo_id: &str,
        source_kind: MemorySourceKind,
    ) -> StorageResult<Option<MemoryProjectionStateRecord>>;
    async fn list_memory_projection_states(
        &self,
        repo_id: &str,
    ) -> StorageResult<Vec<MemoryProjectionStateRecord>>;

    async fn upsert_memory_episode_path(
        &self,
        record: &MemoryEpisodePathRecord,
    ) -> StorageResult<()>;
    async fn list_memory_episode_paths(
        &self,
        episode_id: &str,
    ) -> StorageResult<Vec<MemoryEpisodePathRecord>>;
    /// Queries `memory_episode_path` via the `MEMORY_BY_PATH` path + time
    /// secondary index ordered strictly by `(ended_at DESC, episode_id ASC)`.
    async fn query_memory_paths_by_path(
        &self,
        code_path: &[u8],
        since_ended_at: Option<i64>,
        until_ended_at: Option<i64>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<MemoryEpisodePathRecord>>;

    async fn upsert_memory_episode_search_doc(
        &self,
        episode_id: &str,
        title: &str,
        body: &str,
        paths_text: &str,
    ) -> StorageResult<MemoryEpisodeSearchDocRecord>;
    async fn get_memory_episode_search_doc(
        &self,
        episode_id: &str,
    ) -> StorageResult<Option<MemoryEpisodeSearchDocRecord>>;
    async fn list_memory_episode_search_docs(
        &self,
    ) -> StorageResult<Vec<MemoryEpisodeSearchDocRecord>>;

    // -------------------------------------------------------------------------
    // RB-05b AC4: Governance & Audit (`approved_permission`, `automation_log`, `source_call_log`)
    // -------------------------------------------------------------------------

    async fn upsert_approved_permission(
        &self,
        record: &ApprovedPermissionRecord,
    ) -> StorageResult<()>;
    async fn list_approved_permissions(
        &self,
        project_id: &str,
    ) -> StorageResult<Vec<ApprovedPermissionRecord>>;
    async fn delete_approved_permission(
        &self,
        project_id: &str,
        permission: &str,
        pattern: &str,
    ) -> StorageResult<bool>;

    async fn append_automation_log(&self, record: &AutomationLogRecord) -> StorageResult<()>;
    async fn list_automation_logs(
        &self,
        rule_id: Option<&str>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<AutomationLogRecord>>;

    async fn append_source_call_log(&self, record: &SourceCallLogRecord) -> StorageResult<()>;
    async fn list_source_call_logs(
        &self,
        session_id: Option<&str>,
        agent_run_id: Option<&str>,
    ) -> StorageResult<Vec<SourceCallLogRecord>>;

    // -------------------------------------------------------------------------
    // RB-05b AC5: Bridge Two-Phase Publication (`prepared` -> `published`)
    // -------------------------------------------------------------------------

    async fn prepare_bridge_commit_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<BridgePublicationRecord>;
    async fn publish_bridge_commit_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<BridgePublicationRecord>;
    async fn get_bridge_commit_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgePublicationRecord>>;

    async fn prepare_bridge_index_publication(
        &self,
        operation_id: &str,
        index_digest: &str,
    ) -> StorageResult<BridgeIndexPublicationRecord>;
    async fn publish_bridge_index_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgeIndexPublicationRecord>>;
    async fn get_bridge_index_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgeIndexPublicationRecord>>;
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

    async fn run_all_rb05b_ac_assertions(stores: &[Arc<dyn GitStateStore>]) {
        assert!(!stores.is_empty());
        let repo_id = "repo-rb05b-001";

        // =====================================================================
        // AC1: All 19 `ai_*` thread, scheduler, runtime-contract & index tables
        // =====================================================================
        let thread1 = AiThreadRecord {
            thread_id: "thr-001".to_string(),
            title: Some("Storage refactor thread".to_string()),
            owner_kind: "human".to_string(),
            owner_id: "user-1".to_string(),
            owner_display_name: Some("Eli".to_string()),
            current_intent_id: Some("int-002".to_string()),
            latest_intent_id: Some("int-002".to_string()),
            metadata_json: Some("{\"priority\":\"high\"}".to_string()),
            archived: false,
            version: 2,
            created_at: 1_795_400_000,
            updated_at: 1_795_400_100,
        };
        let thread_temp = AiThreadRecord {
            thread_id: "thr-cascade".to_string(),
            title: Some("Cascade delete probe".to_string()),
            owner_kind: "agent".to_string(),
            owner_id: "bot-9".to_string(),
            owner_display_name: None,
            current_intent_id: None,
            latest_intent_id: None,
            metadata_json: None,
            archived: true,
            version: 1,
            created_at: 1_795_400_050,
            updated_at: 1_795_400_050,
        };
        let part1 = AiThreadParticipantRecord {
            thread_id: "thr-001".to_string(),
            actor_kind: "agent".to_string(),
            actor_id: "codex-1".to_string(),
            actor_display_name: Some("Codex".to_string()),
            role: "reviewer".to_string(),
            joined_at: 1_795_400_005,
        };
        let intent1 = AiThreadIntentRecord {
            thread_id: "thr-001".to_string(),
            intent_id: "int-001".to_string(),
            ordinal: 0,
            is_head: false,
            linked_at: 1_795_400_010,
            link_reason: "initial".to_string(),
        };
        let intent2 = AiThreadIntentRecord {
            thread_id: "thr-001".to_string(),
            intent_id: "int-002".to_string(),
            ordinal: 1,
            is_head: true,
            linked_at: 1_795_400_020,
            link_reason: "follow-up".to_string(),
        };
        let prov_meta = AiThreadProviderMetadataRecord {
            thread_id: "thr-001".to_string(),
            legacy_session_id: Some("legacy-s1".to_string()),
            provider_thread_id: Some("prov-t1".to_string()),
            provider_kind: Some("openai".to_string()),
            metadata_json: Some("{\"model\":\"o3\"}".to_string()),
            updated_at: 1_795_400_030,
        };
        let sched_state = AiSchedulerStateRecord {
            thread_id: "thr-001".to_string(),
            selected_plan_id: Some("plan-002".to_string()),
            active_task_id: Some("task-001".to_string()),
            active_run_id: Some("run-002".to_string()),
            metadata_json: Some("{\"phase\":\"verify\"}".to_string()),
            version: 3,
            updated_at: 1_795_400_040,
        };
        let plan_heads = vec![
            AiSchedulerPlanHeadRecord {
                thread_id: "thr-001".to_string(),
                plan_id: "plan-001".to_string(),
                ordinal: 0,
            },
            AiSchedulerPlanHeadRecord {
                thread_id: "thr-001".to_string(),
                plan_id: "plan-002".to_string(),
                ordinal: 1,
            },
        ];
        let selected_plans = vec![AiSchedulerSelectedPlanRecord {
            thread_id: "thr-001".to_string(),
            plan_id: "plan-002".to_string(),
            ordinal: 0,
        }];
        let context_window = vec![
            AiLiveContextWindowRecord {
                thread_id: "thr-001".to_string(),
                context_frame_id: "frame-001".to_string(),
                position: 0,
                source_kind: "user_pin".to_string(),
                pin_kind: Some("hard".to_string()),
                inserted_at: 1_795_400_011,
            },
            AiLiveContextWindowRecord {
                thread_id: "thr-001".to_string(),
                context_frame_id: "frame-002".to_string(),
                position: 1,
                source_kind: "auto".to_string(),
                pin_kind: None,
                inserted_at: 1_795_400_012,
            },
        ];
        let idx_ip = AiIndexIntentPlanRecord {
            intent_id: "int-001".to_string(),
            plan_id: "plan-001".to_string(),
            created_at: 1_795_400_015,
        };
        let idx_it = AiIndexIntentTaskRecord {
            intent_id: "int-001".to_string(),
            task_id: "task-001".to_string(),
            parent_task_id: None,
            origin_step_id: Some("step-001".to_string()),
            created_at: 1_795_400_016,
        };
        let idx_pst = AiIndexPlanStepTaskRecord {
            plan_id: "plan-001".to_string(),
            task_id: "task-001".to_string(),
            step_id: "step-001".to_string(),
            created_at: 1_795_400_017,
        };
        let tr1 = AiIndexTaskRunRecord {
            task_id: "task-001".to_string(),
            run_id: "run-001".to_string(),
            is_latest: true,
            created_at: 1_795_400_020,
            base_commit_ref: Some(format!("sha1:{}", sha1(1))),
        };
        let tr2 = AiIndexTaskRunRecord {
            task_id: "task-001".to_string(),
            run_id: "run-002".to_string(),
            is_latest: true,
            created_at: 1_795_400_025,
            base_commit_ref: Some(format!("sha256:{}", sha256(2))),
        };
        let ev1 = AiIndexRunEventRecord {
            run_id: "run-002".to_string(),
            event_id: "ev-001".to_string(),
            event_kind: "tool_call".to_string(),
            is_latest: true,
            created_at: 1_795_400_026,
        };
        let ev2 = AiIndexRunEventRecord {
            run_id: "run-002".to_string(),
            event_id: "ev-002".to_string(),
            event_kind: "finished".to_string(),
            is_latest: true,
            created_at: 1_795_400_027,
        };
        let ps1 = AiIndexRunPatchsetRecord {
            run_id: "run-002".to_string(),
            patchset_id: "ps-001".to_string(),
            sequence: 1,
            is_latest: true,
            created_at: 1_795_400_028,
        };
        let ps2 = AiIndexRunPatchsetRecord {
            run_id: "run-002".to_string(),
            patchset_id: "ps-002".to_string(),
            sequence: 2,
            is_latest: true,
            created_at: 1_795_400_029,
        };
        let icf = AiIndexIntentContextFrameRecord {
            intent_id: "int-001".to_string(),
            context_frame_id: "frame-001".to_string(),
            relation_kind: "seed".to_string(),
            created_at: 1_795_400_011,
        };
        let vr1 = AiValidationReportRecord {
            report_id: "vr-001".to_string(),
            thread_id: "thr-001".to_string(),
            run_id: Some("run-001".to_string()),
            policy_version: "v1".to_string(),
            stale: true,
            is_latest: true,
            summary_json: "{\"ok\":false}".to_string(),
            created_at: 1_795_400_030,
            updated_at: 1_795_400_031,
        };
        let vr2 = AiValidationReportRecord {
            report_id: "vr-002".to_string(),
            thread_id: "thr-001".to_string(),
            run_id: Some("run-002".to_string()),
            policy_version: "v1".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"ok\":true}".to_string(),
            created_at: 1_795_400_035,
            updated_at: 1_795_400_036,
        };
        let risk1 = AiRiskScoreBreakdownRecord {
            breakdown_id: "rs-001".to_string(),
            thread_id: "thr-001".to_string(),
            validation_report_id: Some("vr-001".to_string()),
            policy_version: "v1".to_string(),
            stale: true,
            is_latest: true,
            summary_json: "{\"score\":80}".to_string(),
            created_at: 1_795_400_032,
            updated_at: 1_795_400_032,
        };
        let risk2 = AiRiskScoreBreakdownRecord {
            breakdown_id: "rs-002".to_string(),
            thread_id: "thr-001".to_string(),
            validation_report_id: Some("vr-002".to_string()),
            policy_version: "v1".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"score\":12}".to_string(),
            created_at: 1_795_400_037,
            updated_at: 1_795_400_037,
        };
        let prop1 = AiDecisionProposalRecord {
            proposal_id: "dp-001".to_string(),
            thread_id: "thr-001".to_string(),
            validation_report_id: Some("vr-001".to_string()),
            risk_score_breakdown_id: Some("rs-001".to_string()),
            policy_version: "v1".to_string(),
            stale: true,
            is_latest: true,
            summary_json: "{\"route\":\"human\"}".to_string(),
            created_at: 1_795_400_033,
            updated_at: 1_795_400_033,
        };
        let prop2 = AiDecisionProposalRecord {
            proposal_id: "dp-002".to_string(),
            thread_id: "thr-001".to_string(),
            validation_report_id: Some("vr-002".to_string()),
            risk_score_breakdown_id: Some("rs-002".to_string()),
            policy_version: "v1".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"route\":\"auto_accept\"}".to_string(),
            created_at: 1_795_400_038,
            updated_at: 1_795_400_038,
        };
        let dec1 = AiFinalDecisionRecord {
            decision_id: "fd-001".to_string(),
            thread_id: "thr-001".to_string(),
            decision_proposal_id: Some("dp-001".to_string()),
            validation_report_id: Some("vr-001".to_string()),
            policy_version: "v1".to_string(),
            verdict: "request_changes".to_string(),
            stale: true,
            is_latest: true,
            summary_json: "{\"final\":false}".to_string(),
            created_at: 1_795_400_034,
            updated_at: 1_795_400_034,
        };
        let dec2 = AiFinalDecisionRecord {
            decision_id: "fd-002".to_string(),
            thread_id: "thr-001".to_string(),
            decision_proposal_id: Some("dp-002".to_string()),
            validation_report_id: Some("vr-002".to_string()),
            policy_version: "v1".to_string(),
            verdict: "approved".to_string(),
            stale: false,
            is_latest: true,
            summary_json: "{\"final\":true}".to_string(),
            created_at: 1_795_400_039,
            updated_at: 1_795_400_039,
        };

        for store in stores {
            // Reject orphan child referencing missing `ai_thread`.
            assert!(store.upsert_ai_thread_participant(&part1).await.is_err());

            store.upsert_ai_thread(&thread1).await.expect("upsert thr1");
            store
                .upsert_ai_thread(&thread_temp)
                .await
                .expect("upsert thr_temp");
            assert_eq!(
                store.get_ai_thread("thr-001").await.expect("get thr1"),
                Some(thread1.clone())
            );
            assert_eq!(
                store
                    .list_ai_threads(Some(false))
                    .await
                    .expect("list active threads"),
                vec![thread1.clone()]
            );

            store
                .upsert_ai_thread_participant(&part1)
                .await
                .expect("upsert participant");
            store
                .upsert_ai_thread_intent(&intent1)
                .await
                .expect("upsert intent1");
            store
                .upsert_ai_thread_intent(&intent2)
                .await
                .expect("upsert intent2");
            // Cross-thread duplicate `intent_id` must be rejected identically on both backends.
            let cross_dup_intent = AiThreadIntentRecord {
                thread_id: "thr-cascade".to_string(),
                intent_id: "int-001".to_string(),
                ordinal: 0,
                is_head: true,
                linked_at: 1_795_400_021,
                link_reason: "dup".to_string(),
            };
            assert!(
                store
                    .upsert_ai_thread_intent(&cross_dup_intent)
                    .await
                    .is_err()
            );
            store
                .upsert_ai_thread_provider_metadata(&prov_meta)
                .await
                .expect("upsert prov meta");
            store
                .upsert_ai_scheduler_state(&sched_state)
                .await
                .expect("upsert sched state");
            store
                .replace_ai_scheduler_plan_heads("thr-001", &plan_heads)
                .await
                .expect("replace plan heads");
            store
                .replace_ai_scheduler_selected_plans("thr-001", &selected_plans)
                .await
                .expect("replace selected plans");
            store
                .replace_ai_live_context_window("thr-001", &context_window)
                .await
                .expect("replace context window");
            store
                .upsert_ai_index_intent_plan(&idx_ip)
                .await
                .expect("upsert idx_ip");
            store
                .upsert_ai_index_intent_task(&idx_it)
                .await
                .expect("upsert idx_it");
            store
                .upsert_ai_index_plan_step_task(&idx_pst)
                .await
                .expect("upsert idx_pst");
            store
                .upsert_ai_index_task_run(&tr1)
                .await
                .expect("upsert tr1");
            store
                .upsert_ai_index_task_run(&tr2)
                .await
                .expect("upsert tr2 clears tr1 is_latest");
            let runs = store
                .list_ai_index_task_runs("task-001")
                .await
                .expect("list task runs");
            assert_eq!(runs.len(), 2);
            assert!(!runs[0].is_latest && runs[1].is_latest);

            store
                .upsert_ai_index_run_event(&ev1)
                .await
                .expect("upsert ev1");
            store
                .upsert_ai_index_run_event(&ev2)
                .await
                .expect("upsert ev2");
            let events = store
                .list_ai_index_run_events("run-002")
                .await
                .expect("list run events");
            assert!(!events[0].is_latest && events[1].is_latest);

            store
                .upsert_ai_index_run_patchset(&ps1)
                .await
                .expect("upsert ps1");
            store
                .upsert_ai_index_run_patchset(&ps2)
                .await
                .expect("upsert ps2");
            let patchsets = store
                .list_ai_index_run_patchsets("run-002")
                .await
                .expect("list run patchsets");
            assert!(!patchsets[0].is_latest && patchsets[1].is_latest);

            store
                .upsert_ai_index_intent_context_frame(&icf)
                .await
                .expect("upsert icf");

            store
                .upsert_ai_validation_report(&vr1)
                .await
                .expect("upsert vr1");
            store
                .upsert_ai_validation_report(&vr2)
                .await
                .expect("upsert vr2");
            let vrs = store
                .list_ai_validation_reports("thr-001")
                .await
                .expect("list vr");
            assert!(!vrs[0].is_latest && vrs[1].is_latest);

            store
                .upsert_ai_risk_score_breakdown(&risk1)
                .await
                .expect("upsert risk1");
            store
                .upsert_ai_risk_score_breakdown(&risk2)
                .await
                .expect("upsert risk2");
            let risks = store
                .list_ai_risk_score_breakdowns("thr-001")
                .await
                .expect("list risk");
            assert!(!risks[0].is_latest && risks[1].is_latest);

            store
                .upsert_ai_decision_proposal(&prop1)
                .await
                .expect("upsert prop1");
            store
                .upsert_ai_decision_proposal(&prop2)
                .await
                .expect("upsert prop2");
            let props = store
                .list_ai_decision_proposals("thr-001")
                .await
                .expect("list prop");
            assert!(!props[0].is_latest && props[1].is_latest);

            store
                .upsert_ai_final_decision(&dec1)
                .await
                .expect("upsert dec1");
            store
                .upsert_ai_final_decision(&dec2)
                .await
                .expect("upsert dec2");
            let decs = store
                .list_ai_final_decisions("thr-001")
                .await
                .expect("list dec");
            assert!(!decs[0].is_latest && decs[1].is_latest);

            // Cascade delete `thr-cascade` with a child participant + report.
            let cascade_part = AiThreadParticipantRecord {
                thread_id: "thr-cascade".to_string(),
                ..part1.clone()
            };
            store
                .upsert_ai_thread_participant(&cascade_part)
                .await
                .expect("upsert cascade child");
            assert!(
                store
                    .delete_ai_thread("thr-cascade")
                    .await
                    .expect("delete cascade thread")
            );
            assert!(
                store
                    .list_ai_thread_participants("thr-cascade")
                    .await
                    .expect("cascade cleared")
                    .is_empty()
            );
        }

        // =====================================================================
        // AC2 & AC3: 5 `memory_*` tables + `MEMORY_BY_PATH` path+time index
        // =====================================================================
        let ep1 = MemoryEpisodeRecord {
            episode_id: "ep-001".to_string(),
            repo_id: repo_id.to_string(),
            source_kind: MemorySourceKind::Commit,
            source_key: sha1(501),
            outcome: MemoryOutcome::Succeeded,
            actor: Some("eli".to_string()),
            started_at: 1000,
            ended_at: 1100,
            anchor_commit: Some(sha1(501)),
            change_id: Some("chg-m1".to_string()),
            title: "Initial storage commit".to_string(),
            body: "Added redb opener".to_string(),
            content_digest: sha256(501),
            producer: "derived-v1".to_string(),
            rules_version: 1,
        };
        let ep2 = MemoryEpisodeRecord {
            episode_id: "ep-002".to_string(),
            repo_id: repo_id.to_string(),
            source_kind: MemorySourceKind::AgentRun,
            source_key: "run-502".to_string(),
            outcome: MemoryOutcome::Partial,
            actor: Some("agent-1".to_string()),
            started_at: 2000,
            ended_at: 2500,
            anchor_commit: Some(sha1(502)),
            change_id: None,
            title: "Follow-up refactor".to_string(),
            body: "Refined git state store".to_string(),
            content_digest: sha256(502),
            producer: "derived-v1".to_string(),
            rules_version: 1,
        };
        let ep3 = MemoryEpisodeRecord {
            episode_id: "ep-003".to_string(),
            repo_id: repo_id.to_string(),
            source_kind: MemorySourceKind::BridgeOperation,
            source_key: "op-503".to_string(),
            outcome: MemoryOutcome::Succeeded,
            actor: None,
            started_at: 2400,
            ended_at: 2500, // tie on ended_at with ep-002 to verify deterministic tie-break!
            anchor_commit: None,
            change_id: None,
            title: "Bridge tie timestamp".to_string(),
            body: "Tied timestamp ordering test".to_string(),
            content_digest: sha256(503),
            producer: "derived-v1".to_string(),
            rules_version: 1,
        };
        let ep_del = MemoryEpisodeRecord {
            episode_id: "ep-delete-me".to_string(),
            repo_id: repo_id.to_string(),
            source_kind: MemorySourceKind::AgentSession,
            source_key: "sess-del".to_string(),
            outcome: MemoryOutcome::Aborted,
            actor: None,
            started_at: 3000,
            ended_at: 3500,
            anchor_commit: None,
            change_id: None,
            title: "Will be deleted".to_string(),
            body: "Cascade removes MEMORY_BY_PATH entry".to_string(),
            content_digest: sha256(504),
            producer: "derived-v1".to_string(),
            rules_version: 1,
        };

        let shared_path = b"src/storage/git_state_store.rs".to_vec();
        let non_utf8_path = vec![
            b'd', b'a', b't', b'a', b'/', 0xff, 0xfe, b'.', b'b', b'i', b'n',
        ];

        let ev_list = vec![
            MemoryEpisodeEvidenceRecord {
                episode_id: "ep-001".to_string(),
                ordinal: 0,
                kind: MemoryEvidenceKind::Commit,
                ref_id: sha1(501),
                link_confidence: MemoryLinkConfidence::Identity,
                resolution_status: MemoryResolutionStatus::Resolved,
            },
            MemoryEpisodeEvidenceRecord {
                episode_id: "ep-001".to_string(),
                ordinal: 1,
                kind: MemoryEvidenceKind::Operation,
                ref_id: "op-001".to_string(),
                link_confidence: MemoryLinkConfidence::Operation,
                resolution_status: MemoryResolutionStatus::Unresolved,
            },
        ];
        let proj_state = MemoryProjectionStateRecord {
            repo_id: repo_id.to_string(),
            source_kind: MemorySourceKind::Commit,
            cursor_json: "{\"head\":\"abc\"}".to_string(),
            fingerprint: "fp-v1".to_string(),
            rules_version: 1,
            schema_version: 2,
            horizon_truncated: true,
            revoked_count: 3,
            aged_out_count: 1,
            rebuilt_at: 1_795_401_000,
        };

        for store in stores {
            store.upsert_memory_episode(&ep1).await.expect("ep1");
            store.upsert_memory_episode(&ep2).await.expect("ep2");
            store.upsert_memory_episode(&ep3).await.expect("ep3");
            store.upsert_memory_episode(&ep_del).await.expect("ep_del");

            // Reject duplicate natural key `(repo_id, source_kind, source_key)` on distinct episode_id.
            let dup_ep = MemoryEpisodeRecord {
                episode_id: "ep-dup".to_string(),
                ..ep1.clone()
            };
            assert!(store.upsert_memory_episode(&dup_ep).await.is_err());

            // Reject leading '/' path per SQLite CHECK constraint.
            assert!(
                store
                    .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                        episode_id: "ep-001".to_string(),
                        code_path: b"/abs/forbidden.rs".to_vec(),
                        change_kind: MemoryPathChangeKind::Added,
                        blob_oid_at_end: None,
                        ended_at: 1100,
                    })
                    .await
                    .is_err()
            );

            store
                .replace_memory_episode_evidence("ep-001", &ev_list)
                .await
                .expect("replace evidence");
            store
                .upsert_memory_projection_state(&proj_state)
                .await
                .expect("upsert proj state");

            // Insert paths including a timestamp update (to prove old MEMORY_BY_PATH entry is pruned)
            // and a raw non-UTF-8 Git path.
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-001".to_string(),
                    code_path: shared_path.clone(),
                    change_kind: MemoryPathChangeKind::Added,
                    blob_oid_at_end: Some(sha1(601)),
                    ended_at: 900,
                })
                .await
                .expect("insert ep1 path old ts");
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-001".to_string(),
                    code_path: shared_path.clone(),
                    change_kind: MemoryPathChangeKind::Modified,
                    blob_oid_at_end: Some(sha1(602)),
                    ended_at: 1100,
                })
                .await
                .expect("update ep1 path new ts");
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-002".to_string(),
                    code_path: shared_path.clone(),
                    change_kind: MemoryPathChangeKind::Modified,
                    blob_oid_at_end: Some(sha1(603)),
                    ended_at: 2500,
                })
                .await
                .expect("insert ep2 path");
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-003".to_string(),
                    code_path: shared_path.clone(),
                    change_kind: MemoryPathChangeKind::Renamed,
                    blob_oid_at_end: None,
                    ended_at: 2500,
                })
                .await
                .expect("insert ep3 tied path");
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-002".to_string(),
                    code_path: non_utf8_path.clone(),
                    change_kind: MemoryPathChangeKind::Added,
                    blob_oid_at_end: Some(sha256(604)),
                    ended_at: -50, // negative timestamp verifies signed order inversion
                })
                .await
                .expect("insert non-utf8 negative ts path");
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-delete-me".to_string(),
                    code_path: shared_path.clone(),
                    change_kind: MemoryPathChangeKind::Deleted,
                    blob_oid_at_end: None,
                    ended_at: 3500,
                })
                .await
                .expect("insert ep_del path");

            let doc1 = store
                .upsert_memory_episode_search_doc("ep-001", "T1", "B1", "src/storage")
                .await
                .expect("insert doc1");
            let doc1_upd = store
                .upsert_memory_episode_search_doc("ep-001", "T1-upd", "B1-upd", "src/storage")
                .await
                .expect("update doc1 keeps rowid");
            assert_eq!(doc1.rowid, doc1_upd.rowid);
            let _doc_del = store
                .upsert_memory_episode_search_doc("ep-delete-me", "TD", "BD", "src/storage")
                .await
                .expect("insert doc_del");

            // Delete `ep-delete-me` and verify cascade purges `MEMORY_BY_PATH` & `search_doc`.
            assert!(
                store
                    .delete_memory_episode("ep-delete-me")
                    .await
                    .expect("delete ep_del")
            );
            assert_eq!(
                store
                    .get_memory_episode_search_doc("ep-delete-me")
                    .await
                    .expect("deleted search doc"),
                None
            );

            // AC3: `MEMORY_BY_PATH` exact ordering: `(ended_at DESC, episode_id ASC)`.
            let all_for_path = store
                .query_memory_paths_by_path(&shared_path, None, None, None)
                .await
                .expect("query MEMORY_BY_PATH all");
            let ids: Vec<&str> = all_for_path.iter().map(|r| r.episode_id.as_str()).collect();
            assert_eq!(ids, vec!["ep-002", "ep-003", "ep-001"]);

            // Explicitly verify stale timestamp `900` for `ep-001` was pruned from `MEMORY_BY_PATH`.
            assert!(
                store
                    .query_memory_paths_by_path(&shared_path, Some(800), Some(950), None)
                    .await
                    .expect("old timestamp window must be empty")
                    .is_empty()
            );

            // Re-update an existing `(ep-001, shared_path)` timestamp from 1100 -> 3000 AFTER
            // `ep-002`/`ep-003` exist, proving stale index pruning & re-ordering across both windows.
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-001".to_string(),
                    code_path: shared_path.clone(),
                    change_kind: MemoryPathChangeKind::Modified,
                    blob_oid_at_end: Some(sha1(605)),
                    ended_at: 3000,
                })
                .await
                .expect("promote ep1 ended_at 1100 -> 3000");
            assert!(
                store
                    .query_memory_paths_by_path(&shared_path, Some(1000), Some(1200), None)
                    .await
                    .expect("previous 1100 window must now be empty")
                    .is_empty()
            );
            let promoted_win = store
                .query_memory_paths_by_path(&shared_path, Some(2900), Some(3100), None)
                .await
                .expect("new 3000 window must contain ep-001");
            assert_eq!(promoted_win.len(), 1);
            assert_eq!(promoted_win[0].episode_id, "ep-001");
            assert_eq!(promoted_win[0].ended_at, 3000);
            let reordered = store
                .query_memory_paths_by_path(&shared_path, None, None, None)
                .await
                .expect("query MEMORY_BY_PATH after timestamp promotion");
            let reordered_ids: Vec<&str> =
                reordered.iter().map(|r| r.episode_id.as_str()).collect();
            assert_eq!(reordered_ids, vec!["ep-001", "ep-002", "ep-003"]);

            // Move `ep-001` back to 1100 and verify deterministic bounded range + limit query.
            store
                .upsert_memory_episode_path(&MemoryEpisodePathRecord {
                    episode_id: "ep-001".to_string(),
                    code_path: shared_path.clone(),
                    change_kind: MemoryPathChangeKind::Modified,
                    blob_oid_at_end: Some(sha1(602)),
                    ended_at: 1100,
                })
                .await
                .expect("restore ep1 ended_at 3000 -> 1100");
            assert!(
                store
                    .query_memory_paths_by_path(&shared_path, Some(2900), Some(3100), None)
                    .await
                    .expect("pruned 3000 window must now be empty")
                    .is_empty()
            );
            let windowed = store
                .query_memory_paths_by_path(&shared_path, Some(1000), Some(2500), Some(2))
                .await
                .expect("query MEMORY_BY_PATH windowed");
            let win_ids: Vec<&str> = windowed.iter().map(|r| r.episode_id.as_str()).collect();
            assert_eq!(win_ids, vec!["ep-002", "ep-003"]);

            let non_utf8_res = store
                .query_memory_paths_by_path(&non_utf8_path, Some(-100), Some(0), None)
                .await
                .expect("query non-utf8 negative ts");
            assert_eq!(non_utf8_res.len(), 1);
            assert_eq!(non_utf8_res[0].episode_id, "ep-002");
        }

        // =====================================================================
        // AC4: Governance & Audit (`approved_permission`, `automation_log`, `source_call_log`)
        // =====================================================================
        let perm1 = ApprovedPermissionRecord {
            project_id: "proj-001".to_string(),
            permission: "bash".to_string(),
            pattern: "cargo nextest*".to_string(),
            created_at: 1_795_402_000,
            source_worktree_id: "wt-alpha".to_string(),
            source_session_id: "sess-1".to_string(),
            source_workspace_id: "ws-1".to_string(),
        };
        let perm2 = ApprovedPermissionRecord {
            project_id: "proj-001".to_string(),
            permission: "edit".to_string(),
            pattern: "src/storage/**".to_string(),
            created_at: 1_795_402_005,
            source_worktree_id: String::new(),
            source_session_id: "sess-2".to_string(),
            source_workspace_id: String::new(),
        };
        let auto1 = AutomationLogRecord {
            id: "auto-001".to_string(),
            rule_id: "rule-fmt".to_string(),
            trigger_kind: "pre_commit".to_string(),
            action_kind: "run_cmd".to_string(),
            status: "succeeded".to_string(),
            message: "formatted cleanly".to_string(),
            started_at: "2026-11-22T19:00:00Z".to_string(),
            finished_at: "2026-11-22T19:00:02Z".to_string(),
            details_json: "{\"exit\":0}".to_string(),
        };
        let auto2 = AutomationLogRecord {
            id: "auto-002".to_string(),
            rule_id: "rule-fmt".to_string(),
            trigger_kind: "pre_push".to_string(),
            action_kind: "run_cmd".to_string(),
            status: "failed".to_string(),
            message: "clippy warning".to_string(),
            started_at: "2026-11-22T19:05:00Z".to_string(),
            finished_at: "2026-11-22T19:05:09Z".to_string(),
            details_json: "{\"exit\":101}".to_string(),
        };
        let scl1 = SourceCallLogRecord {
            id: "scl-001".to_string(),
            session_id: "sess-1".to_string(),
            source_slug: "github".to_string(),
            tool_name: "get_pr".to_string(),
            registered_tool_name: "github__get_pr".to_string(),
            tool_call_id: "call-1".to_string(),
            credential_ref: Some("vault:gh-1".to_string()),
            latency_ms: Some(142),
            input_bytes: 128,
            output_bytes: 2048,
            cost_estimate_micros: Some(500),
            approval_decision: Some("always".to_string()),
            state_namespace: "default".to_string(),
            success: true,
            created_at: "2026-11-22T19:10:00Z".to_string(),
            agent_run_id: Some("run-sub-1".to_string()),
        };
        let scl2 = SourceCallLogRecord {
            id: "scl-002".to_string(),
            session_id: "sess-1".to_string(),
            source_slug: "github".to_string(),
            tool_name: "list_issues".to_string(),
            registered_tool_name: "github__list_issues".to_string(),
            tool_call_id: "call-2".to_string(),
            credential_ref: None,
            latency_ms: None,
            input_bytes: 64,
            output_bytes: 0,
            cost_estimate_micros: None,
            approval_decision: None,
            state_namespace: "default".to_string(),
            success: false,
            created_at: "2026-11-22T19:11:00Z".to_string(),
            agent_run_id: None,
        };

        for store in stores {
            store
                .upsert_approved_permission(&perm1)
                .await
                .expect("upsert perm1");
            store
                .upsert_approved_permission(&perm2)
                .await
                .expect("upsert perm2");
            assert_eq!(
                store
                    .list_approved_permissions("proj-001")
                    .await
                    .expect("list perms"),
                vec![perm1.clone(), perm2.clone()]
            );
            assert!(
                store
                    .delete_approved_permission("proj-001", "edit", "src/storage/**")
                    .await
                    .expect("delete perm2")
            );
            store
                .upsert_approved_permission(&perm2)
                .await
                .expect("restore perm2");

            store
                .append_automation_log(&auto1)
                .await
                .expect("append auto1");
            store
                .append_automation_log(&auto2)
                .await
                .expect("append auto2");
            assert!(store.append_automation_log(&auto1).await.is_err());
            assert_eq!(
                store
                    .list_automation_logs(Some("rule-fmt"), Some(1))
                    .await
                    .expect("latest auto log"),
                vec![auto2.clone()]
            );

            store
                .append_source_call_log(&scl1)
                .await
                .expect("append scl1");
            store
                .append_source_call_log(&scl2)
                .await
                .expect("append scl2");
            assert!(store.append_source_call_log(&scl1).await.is_err());
            assert_eq!(
                store
                    .list_source_call_logs(Some("sess-1"), Some("run-sub-1"))
                    .await
                    .expect("filtered scl"),
                vec![scl1.clone()]
            );
        }

        // =====================================================================
        // AC5: Two-phase `agent_bridge_publication` & `agent_bridge_index_publication`
        // =====================================================================
        let commit_oid = sha1(777);
        let other_oid = sha1(888);
        for store in stores {
            assert_eq!(
                store
                    .get_bridge_commit_publication("bridge-op-01")
                    .await
                    .expect("absent commit pub"),
                None
            );
            // Publishing before prepare MUST fail closed and leave no row behind!
            assert!(
                store
                    .publish_bridge_commit_publication("bridge-op-01", &commit_oid)
                    .await
                    .is_err(),
                "publish_bridge_commit_publication must reject unprepared operation_id"
            );
            assert_eq!(
                store
                    .get_bridge_commit_publication("bridge-op-01")
                    .await
                    .expect("still absent after rejected publish"),
                None
            );
            let prep_c = store
                .prepare_bridge_commit_publication("bridge-op-01", &commit_oid)
                .await
                .expect("prepare commit pub");
            assert_eq!(prep_c.phase, BridgePublicationPhase::Prepared);

            // Idempotent re-prepare with same commit_oid succeeds; different oid fails closed!
            assert_eq!(
                store
                    .prepare_bridge_commit_publication("bridge-op-01", &commit_oid)
                    .await
                    .expect("idempotent prepare"),
                prep_c
            );
            assert!(
                store
                    .prepare_bridge_commit_publication("bridge-op-01", &other_oid)
                    .await
                    .is_err()
            );

            // Promote `prepared` -> `published`.
            let pub_c = store
                .publish_bridge_commit_publication("bridge-op-01", &commit_oid)
                .await
                .expect("publish commit pub");
            assert_eq!(pub_c.phase, BridgePublicationPhase::Published);

            // Re-preparing an already-published binding MUST NOT downgrade `Published` to `Prepared`!
            assert_eq!(
                store
                    .prepare_bridge_commit_publication("bridge-op-01", &commit_oid)
                    .await
                    .expect("prepare after publish stays published")
                    .phase,
                BridgePublicationPhase::Published
            );
            assert!(
                store
                    .publish_bridge_commit_publication("bridge-op-01", &other_oid)
                    .await
                    .is_err()
            );

            // Index publication two-phase: absent publish -> None; prepare -> Prepared -> publish -> Published.
            assert_eq!(
                store
                    .publish_bridge_index_publication("bridge-idx-01")
                    .await
                    .expect("publish missing idx"),
                None
            );
            let prep_i = store
                .prepare_bridge_index_publication("bridge-idx-01", "sha256:idx-v1")
                .await
                .expect("prepare idx pub");
            assert_eq!(prep_i.phase, BridgePublicationPhase::Prepared);
            // Idempotent re-prepare with same digest succeeds; different digest fails closed!
            assert_eq!(
                store
                    .prepare_bridge_index_publication("bridge-idx-01", "sha256:idx-v1")
                    .await
                    .expect("idempotent prepare idx pub"),
                prep_i
            );
            assert!(
                store
                    .prepare_bridge_index_publication("bridge-idx-01", "sha256:idx-conflict")
                    .await
                    .is_err(),
                "prepare_bridge_index_publication must reject conflicting digest"
            );
            assert_eq!(
                store
                    .get_bridge_index_publication("bridge-idx-01")
                    .await
                    .expect("get prepared idx"),
                Some(prep_i)
            );
            let pub_i = store
                .publish_bridge_index_publication("bridge-idx-01")
                .await
                .expect("publish idx pub")
                .expect("idx row present");
            assert_eq!(pub_i.phase, BridgePublicationPhase::Published);
            assert_eq!(pub_i.index_digest, "sha256:idx-v1");
            // Re-preparing an already-published index binding MUST preserve Published (no downgrade)
            // and reject a different digest!
            assert_eq!(
                store
                    .prepare_bridge_index_publication("bridge-idx-01", "sha256:idx-v1")
                    .await
                    .expect("prepare after publish stays published"),
                pub_i
            );
            assert!(
                store
                    .prepare_bridge_index_publication("bridge-idx-01", "sha256:idx-v2")
                    .await
                    .is_err(),
                "prepare after publish with different digest must fail closed"
            );
            assert_eq!(
                store
                    .get_bridge_index_publication("bridge-idx-01")
                    .await
                    .expect("get published idx"),
                Some(pub_i)
            );
        }

        // Exact cross-backend equivalence diff when both SQLite and redb stores are paired.
        if stores.len() == 2 {
            let sq = &stores[0];
            let rb = &stores[1];
            assert_eq!(
                sq.list_ai_threads(None).await.expect("sq"),
                rb.list_ai_threads(None).await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_thread_participants("thr-001").await.expect("sq"),
                rb.list_ai_thread_participants("thr-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_thread_intents("thr-001").await.expect("sq"),
                rb.list_ai_thread_intents("thr-001").await.expect("rb")
            );
            assert_eq!(
                sq.get_ai_thread_provider_metadata("thr-001")
                    .await
                    .expect("sq"),
                rb.get_ai_thread_provider_metadata("thr-001")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.get_ai_scheduler_state("thr-001").await.expect("sq"),
                rb.get_ai_scheduler_state("thr-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_scheduler_plan_heads("thr-001")
                    .await
                    .expect("sq"),
                rb.list_ai_scheduler_plan_heads("thr-001")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_ai_scheduler_selected_plans("thr-001")
                    .await
                    .expect("sq"),
                rb.list_ai_scheduler_selected_plans("thr-001")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_ai_live_context_window("thr-001").await.expect("sq"),
                rb.list_ai_live_context_window("thr-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_index_intent_plans("int-001").await.expect("sq"),
                rb.list_ai_index_intent_plans("int-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_index_intent_tasks("int-001").await.expect("sq"),
                rb.list_ai_index_intent_tasks("int-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_index_plan_step_tasks("plan-001")
                    .await
                    .expect("sq"),
                rb.list_ai_index_plan_step_tasks("plan-001")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_ai_index_task_runs("task-001").await.expect("sq"),
                rb.list_ai_index_task_runs("task-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_index_run_events("run-002").await.expect("sq"),
                rb.list_ai_index_run_events("run-002").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_index_run_patchsets("run-002").await.expect("sq"),
                rb.list_ai_index_run_patchsets("run-002").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_index_intent_context_frames("int-001")
                    .await
                    .expect("sq"),
                rb.list_ai_index_intent_context_frames("int-001")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_ai_validation_reports("thr-001").await.expect("sq"),
                rb.list_ai_validation_reports("thr-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_risk_score_breakdowns("thr-001")
                    .await
                    .expect("sq"),
                rb.list_ai_risk_score_breakdowns("thr-001")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_ai_decision_proposals("thr-001").await.expect("sq"),
                rb.list_ai_decision_proposals("thr-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_ai_final_decisions("thr-001").await.expect("sq"),
                rb.list_ai_final_decisions("thr-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_memory_episodes(repo_id).await.expect("sq"),
                rb.list_memory_episodes(repo_id).await.expect("rb")
            );
            assert_eq!(
                sq.list_memory_episode_evidence("ep-001").await.expect("sq"),
                rb.list_memory_episode_evidence("ep-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_memory_projection_states(repo_id).await.expect("sq"),
                rb.list_memory_projection_states(repo_id).await.expect("rb")
            );
            assert_eq!(
                sq.list_memory_episode_paths("ep-001").await.expect("sq"),
                rb.list_memory_episode_paths("ep-001").await.expect("rb")
            );
            assert_eq!(
                sq.query_memory_paths_by_path(&shared_path, None, None, None)
                    .await
                    .expect("sq"),
                rb.query_memory_paths_by_path(&shared_path, None, None, None)
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.list_memory_episode_search_docs().await.expect("sq"),
                rb.list_memory_episode_search_docs().await.expect("rb")
            );
            assert_eq!(
                sq.list_approved_permissions("proj-001").await.expect("sq"),
                rb.list_approved_permissions("proj-001").await.expect("rb")
            );
            assert_eq!(
                sq.list_automation_logs(None, None).await.expect("sq"),
                rb.list_automation_logs(None, None).await.expect("rb")
            );
            assert_eq!(
                sq.list_source_call_logs(None, None).await.expect("sq"),
                rb.list_source_call_logs(None, None).await.expect("rb")
            );
            assert_eq!(
                sq.get_bridge_commit_publication("bridge-op-01")
                    .await
                    .expect("sq"),
                rb.get_bridge_commit_publication("bridge-op-01")
                    .await
                    .expect("rb")
            );
            assert_eq!(
                sq.get_bridge_index_publication("bridge-idx-01")
                    .await
                    .expect("sq"),
                rb.get_bridge_index_publication("bridge-idx-01")
                    .await
                    .expect("rb")
            );
        }
    }

    /// `RB-05b` (AC1–AC5): Dual-backend equivalence across all 19 `ai_*`,
    /// 5 `memory_*` (+ `MEMORY_BY_PATH`), 3 governance/audit, and 2 bridge
    /// two-phase publication tables + `RedbGitStore` interoperability check.
    #[cfg(all(feature = "storage-sqlite", feature = "storage-redb"))]
    #[tokio::test]
    async fn dual_backend_28_ai_memory_bridge_tables_equivalence() {
        let dir = tempdir().expect("create RB-05b tempdir");
        let sqlite_store = SqliteGitStateStore::create(dir.path().join("libra.db"))
            .await
            .expect("create SqliteGitStateStore");
        let redb_path = dir.path().join("git.redb");
        let redb_store = RedbGitStateStore::create(&redb_path).expect("create RedbGitStateStore");
        let stores: [Arc<dyn GitStateStore>; 2] = [Arc::new(sqlite_store), Arc::new(redb_store)];

        run_all_rb05b_ac_assertions(&stores).await;

        // Verify wire-level interoperability of `agent_bridge_publication` between
        // `RedbGitStore` (`RB-04`) and `RedbGitStateStore` (`RB-05b`) on the SAME `git.redb`!
        let shared_path = dir.path().join("shared_bridge_interop.redb");
        let git_store = crate::storage::git_store::RedbGitStore::create(&shared_path)
            .expect("create RedbGitStore");
        let head_ref = crate::storage::git_store::RefKey::LocalHead { worktree_id: None };
        let main_ref = crate::storage::git_store::RefKey::LocalBranch {
            name: "main".to_string(),
        };
        crate::storage::git_store::GitStore::update_ref_cas(
            &git_store,
            &head_ref,
            None,
            Some(&crate::storage::git_store::RefTarget::Symbolic(
                "main".to_string(),
            )),
            &[],
        )
        .await
        .expect("point HEAD at main");
        let commit1 = sha1(999);
        crate::storage::git_store::GitStore::update_ref_cas_with_bridge_publication(
            &git_store,
            &main_ref,
            None,
            Some(&crate::storage::git_store::RefTarget::Direct(
                commit1.clone(),
            )),
            &[],
            &BridgePublicationRecord {
                operation_id: "interop-op-01".to_string(),
                commit_oid: commit1.clone(),
                phase: BridgePublicationPhase::Published,
            },
        )
        .await
        .expect("publish bridge op via RedbGitStore");
        drop(git_store);

        // Read-only `RedbGitStateStore` BEFORE RB-05b upgrade reads `RedbGitStore`'s binding cleanly.
        let state_ro = RedbGitStateStore::open_read_only(&shared_path)
            .expect("read-only reopen pre-RB-05b git.redb");
        assert_eq!(
            state_ro
                .get_bridge_commit_publication("interop-op-01")
                .await
                .expect("read interop bridge pub before upgrade"),
            Some(BridgePublicationRecord {
                operation_id: "interop-op-01".to_string(),
                commit_oid: commit1.clone(),
                phase: BridgePublicationPhase::Published,
            })
        );
        drop(state_ro);

        // Writable upgrade preserves `RedbGitStore`'s bridge binding and enables `RB-05b` writes!
        let state_rw =
            RedbGitStateStore::open_read_write(&shared_path).expect("upgrade git.redb to RB-05b");
        assert_eq!(
            state_rw
                .get_bridge_commit_publication("interop-op-01")
                .await
                .expect("preserved bridge pub after upgrade"),
            Some(BridgePublicationRecord {
                operation_id: "interop-op-01".to_string(),
                commit_oid: commit1,
                phase: BridgePublicationPhase::Published,
            })
        );
        let prep_commit = sha1(1000);
        let prep = state_rw
            .prepare_bridge_commit_publication("interop-op-02", &prep_commit)
            .await
            .expect("prepare op-02 via RedbGitStateStore");
        assert_eq!(prep.phase, BridgePublicationPhase::Prepared);
        drop(state_rw);

        // Reopen with `RedbGitStore` and promote `interop-op-02` atomically with ref update!
        let git_store2 = crate::storage::git_store::RedbGitStore::open_read_write(&shared_path)
            .expect("reopen RedbGitStore after RB-05b");
        let _ = crate::storage::git_store::GitStore::update_ref_cas_with_bridge_publication(
            &git_store2,
            &main_ref,
            Some(&crate::storage::git_store::RefTarget::Direct(sha1(999))),
            Some(&crate::storage::git_store::RefTarget::Direct(
                prep_commit.clone(),
            )),
            &[],
            &BridgePublicationRecord {
                operation_id: "interop-op-02".to_string(),
                commit_oid: prep_commit.clone(),
                phase: BridgePublicationPhase::Published,
            },
        )
        .await
        .expect("promote op-02 via RedbGitStore");
        assert_eq!(
            crate::storage::git_store::GitStore::get_bridge_publication(
                &git_store2,
                "interop-op-02"
            )
            .await
            .expect("verify promoted op-02"),
            Some(BridgePublicationRecord {
                operation_id: "interop-op-02".to_string(),
                commit_oid: prep_commit,
                phase: BridgePublicationPhase::Published,
            })
        );
    }

    /// `RB-05b` (AC6): Static & live-disk manifest guard asserting that
    /// `ConfigStore` (4) + `GitStore` (7) + `GitStateStore` (21 + 29 = 50)
    /// matches the exact **61** active persistent Repository source tables from
    /// baseline — excluding the 7 retired tables and all `META_*`/secondary
    /// index internal tables. Runs on dual-feature, pure-redb, and pure-sqlite.
    #[tokio::test]
    async fn repository_61_source_tables_manifest_complete() {
        // 1. Disjointness & exact cardinality across the 4 storage domain slices.
        assert_eq!(CONFIG_STORE_REPO_SOURCE_TABLES.len(), 4);
        assert_eq!(GIT_STORE_CORE_SOURCE_TABLES.len(), 7);
        assert_eq!(RB05_WORKFLOW_STATE_SOURCE_TABLES.len(), 21);
        assert_eq!(RB05B_AI_MEMORY_BRIDGE_SOURCE_TABLES.len(), 29);

        let manifest = repository_active_source_tables();
        assert_eq!(
            manifest.len(),
            REPOSITORY_ACTIVE_SOURCE_TABLES_COUNT,
            "expected exactly 61 distinct active Repository source tables (4 + 7 + 21 + 29)"
        );

        let manifest_set: BTreeSet<&str> = manifest.iter().copied().collect();

        // 2. Zero overlap with the 7 retired tables.
        for retired in RETIRED_REPOSITORY_SOURCE_TABLES {
            assert!(
                !manifest_set.contains(retired),
                "retired table `{retired}` must NOT appear in the 61 active source tables"
            );
        }

        // 3. Zero contamination from `META_*`, `QUEUE_HIGHWATER_*`, or internal/secondary index tables.
        for name in &manifest {
            let upper = name.to_ascii_uppercase();
            assert!(
                !upper.starts_with("META_") && !upper.starts_with("QUEUE_HIGHWATER_"),
                "internal metadata table `{name}` must NOT be counted as a source table"
            );
        }
        for internal_or_index in [
            "META_SCHEMA",
            "meta_schema",
            "META_GENERATION",
            "meta_generation",
            "META_CROSS_STORE_INTENT",
            "meta_cross_store_intent",
            "QUEUE_HIGHWATER_PRIORITY",
            "QUEUE_HIGHWATER_NORMAL",
            "REFS_BY_WORKTREE",
            "OBJECT_BY_PACK",
            "object_by_pack",
            "reflog_by_ref",
            "revision_ordinal_by_oid",
            "config_kv_by_key",
            "config_sequences",
            "change_revision_by_commit",
            "git_sequences",
            "git_state_sequences",
            MEMORY_BY_PATH_INDEX_NAME,
        ] {
            assert!(
                !manifest_set.contains(internal_or_index),
                "internal/index table `{internal_or_index}` must NOT be counted as a source table"
            );
        }

        // 4. Exact name-for-name equality against repository baseline:
        //    `MUTABLE_STATE_OWNERSHIP` (minus 24 capture-only tables and 7 retired tables)
        //    union the 5 zero-authority `memory_*` projection tables = exact 61!
        let capture_only: BTreeSet<&str> = [
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
        ]
        .into_iter()
        .collect();
        let retired_set: BTreeSet<&str> = RETIRED_REPOSITORY_SOURCE_TABLES.into_iter().collect();
        let memory_projection_5: BTreeSet<&str> = [
            "memory_episode",
            "memory_episode_evidence",
            "memory_projection_state",
            "memory_episode_path",
            "memory_episode_search_doc",
        ]
        .into_iter()
        .collect();

        let mut baseline: BTreeSet<&str> =
            crate::internal::mutable_state_ownership::MUTABLE_STATE_OWNERSHIP
                .iter()
                .map(|s| s.table)
                .filter(|t| !capture_only.contains(t) && !retired_set.contains(t))
                .collect();
        baseline.extend(memory_projection_5);

        assert_eq!(
            manifest_set, baseline,
            "61-table manifest diverged from MUTABLE_STATE_OWNERSHIP + memory_* baseline"
        );

        // 5. Under `storage-redb`, verify live on-disk redb initialization materializes
        //    all 50 `GitStateStore` source tables + `MEMORY_BY_PATH` & `change_revision_by_commit`,
        //    and that combined with `RedbGitStore` & `RedbConfigStore` covers all 61 source tables.
        #[cfg(feature = "storage-redb")]
        {
            use ::redb::TableHandle;

            let dir = tempdir().expect("create manifest redb tempdir");
            let cfg_redb = dir.path().join("cfg_repo.redb");
            let git_redb = dir.path().join("git.redb");

            // Initialize ConfigStore (4 source tables) and GitStore (7) + GitStateStore (50).
            let cfg_store = crate::storage::redb::RedbConfigStore::create(
                &cfg_redb,
                crate::internal::db::schema::DatabaseRole::Repository,
            )
            .expect("create RedbConfigStore(Repository)");
            drop(cfg_store);
            let cfg_ro = crate::storage::opener::open_redb_read_only(
                &cfg_redb,
                crate::internal::db::schema::DatabaseRole::Repository,
            )
            .expect("open cfg_repo.redb read-only");
            let cfg_rtx = crate::storage::meta::begin_checked_read(
                &cfg_ro,
                crate::internal::db::schema::DatabaseRole::Repository,
            )
            .expect("begin cfg read tx");
            let cfg_tables: BTreeSet<String> = cfg_rtx
                .list_tables()
                .expect("list cfg_repo.redb tables")
                .map(|h| h.name().to_string())
                .collect();
            drop(cfg_rtx);
            drop(cfg_ro);

            let git_store = crate::storage::git_store::RedbGitStore::create(&git_redb)
                .expect("create RedbGitStore");
            drop(git_store);
            let state_store = RedbGitStateStore::open_read_write(&git_redb)
                .expect("upgrade git.redb with RedbGitStateStore RB-05+RB-05b tables");

            let git_tables: BTreeSet<String> = state_store
                .list_table_names()
                .await
                .expect("list live git.redb tables")
                .into_iter()
                .collect();

            let mut all_live_tables = cfg_tables;
            all_live_tables.extend(git_tables.iter().cloned());

            // Every one of the 61 active source tables MUST be materialized across
            // RedbConfigStore (4) + RedbGitStore (7) + RedbGitStateStore (50).
            for expected in &manifest {
                assert!(
                    all_live_tables.contains(*expected),
                    "live redb repository stores missing active source table `{expected}`"
                );
            }
            // Required internal & secondary index tables are present and kept strictly separate.
            let known_internal_and_index: BTreeSet<&str> = [
                crate::storage::meta::META_SCHEMA_TABLE.name(),
                "config_kv_by_key",
                "config_sequences",
                "reflog_by_ref",
                "git_sequences",
                "object_by_pack",
                "revision_ordinal_by_oid",
                "change_revision_by_commit",
                "git_state_sequences",
                MEMORY_BY_PATH_INDEX_NAME,
            ]
            .into_iter()
            .collect();
            for required_internal in &known_internal_and_index {
                assert!(
                    all_live_tables.contains(*required_internal),
                    "live redb repository stores missing expected internal/index table `{required_internal}`"
                );
            }
            // Strict bidirectional equality: live tables minus known internal/index tables == exact 61!
            let live_source_tables: BTreeSet<&str> = all_live_tables
                .iter()
                .map(String::as_str)
                .filter(|t| !known_internal_and_index.contains(t))
                .collect();
            assert_eq!(
                live_source_tables, manifest_set,
                "live redb repository source tables diverged from the 61-table manifest"
            );

            // Also run the full RB-05b AC1–AC5 suite on pure-redb!
            let redb_only_store: [Arc<dyn GitStateStore>; 1] = [Arc::new(state_store)];
            run_all_rb05b_ac_assertions(&redb_only_store).await;
        }
    }

    /// `RB-05b` single-feature SQLite regression smoke test covering AC1–AC5
    /// and read-only tolerance of absent on-demand bridge publication tables.
    #[cfg(feature = "storage-sqlite")]
    #[tokio::test]
    async fn sqlite_ai_memory_bridge_tables_smoke() {
        let dir = tempdir().expect("create RB-05b sqlite smoke tempdir");
        let db_path = dir.path().join("libra.db");
        let sqlite_store = SqliteGitStateStore::create(&db_path)
            .await
            .expect("create SqliteGitStateStore");
        assert_eq!(sqlite_store.backend(), StorageBackendKind::Sqlite);
        let stores: [Arc<dyn GitStateStore>; 1] = [Arc::new(sqlite_store)];
        run_all_rb05b_ac_assertions(&stores).await;

        // Verify read-only open on a fresh SQLite DB without bridge tables returns `Ok(None)`.
        let bare_path = dir.path().join("bare_no_bridge.db");
        let bare_conn = crate::internal::db::schema::create_database_for_role(
            bare_path.to_str().expect("utf8 path"),
            crate::internal::db::schema::DatabaseRole::Repository,
        )
        .await
        .expect("create bare sqlite repo db");
        drop(bare_conn);
        let ro = SqliteGitStateStore::open_read_only(&bare_path)
            .await
            .expect("open_read_only bare sqlite db");
        assert_eq!(
            ro.get_bridge_commit_publication("absent-op")
                .await
                .expect("absent commit table ok"),
            None
        );
        assert_eq!(
            ro.get_bridge_index_publication("absent-op")
                .await
                .expect("absent index table ok"),
            None
        );
        drop(ro);

        // Verify read-only open on a legacy 2-column `agent_bridge_publication` table
        // (without the `phase` column) succeeds and defaults `phase` to `Published`.
        {
            use sea_orm::{ConnectionTrait, DatabaseBackend, Statement};
            let legacy_conn = crate::internal::db::schema::establish_connection_for_role(
                bare_path.to_str().expect("utf8 path"),
                crate::internal::db::schema::DatabaseRole::Repository,
            )
            .await
            .expect("open bare sqlite db to create legacy 2-col bridge table");
            legacy_conn
                .execute_raw(Statement::from_string(
                    DatabaseBackend::Sqlite,
                    "CREATE TABLE agent_bridge_publication (operation_id TEXT PRIMARY KEY, commit_oid TEXT NOT NULL)"
                        .to_string(),
                ))
                .await
                .expect("create legacy 2-column agent_bridge_publication");
            legacy_conn
                .execute_raw(Statement::from_sql_and_values(
                    DatabaseBackend::Sqlite,
                    "INSERT INTO agent_bridge_publication (operation_id, commit_oid) VALUES (?, ?)",
                    ["legacy-op-01".into(), sha1(4242).into()],
                ))
                .await
                .expect("insert legacy bridge publication row");
        }
        let ro_legacy = SqliteGitStateStore::open_read_only(&bare_path)
            .await
            .expect("open_read_only legacy 2-col bridge db");
        assert_eq!(
            ro_legacy
                .get_bridge_commit_publication("legacy-op-01")
                .await
                .expect("read legacy 2-col bridge publication"),
            Some(BridgePublicationRecord {
                operation_id: "legacy-op-01".to_string(),
                commit_oid: sha1(4242),
                phase: BridgePublicationPhase::Published,
            })
        );
    }
}
