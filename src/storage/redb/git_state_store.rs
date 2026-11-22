//! `redb 5.0` implementation of [`GitStateStore`] (`RB-05`).
//!
//! Stores the 21 workflow, worktree/workspace lease, OpLog v2, change-lineage,
//! dirty-cache, overlay-layer, and sparse-view repository tables plus
//! same-transaction secondary index `change_revision_by_commit` and monotonic
//! sequence table `git_state_sequences` inside `git.redb` using versioned
//! `[schema_ver: u16_le][postcard]` envelopes (`crate::storage::codec`).

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use async_trait::async_trait;
use redb::{
    Database, ReadOnlyDatabase, ReadTransaction, ReadableTable, TableDefinition, TableHandle,
    WriteTransaction,
};
use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        backend::StorageBackendKind,
        codec::{decode_versioned, encode_versioned},
        error::{StorageBackendError, StorageResult},
        git_state_store::{
            AiDecisionProposalRecord, AiFinalDecisionRecord, AiIndexIntentContextFrameRecord,
            AiIndexIntentPlanRecord, AiIndexIntentTaskRecord, AiIndexPlanStepTaskRecord,
            AiIndexRunEventRecord, AiIndexRunPatchsetRecord, AiIndexTaskRunRecord,
            AiLiveContextWindowRecord, AiOperationLinkRecord, AiRiskScoreBreakdownRecord,
            AiSchedulerPlanHeadRecord, AiSchedulerSelectedPlanRecord, AiSchedulerStateRecord,
            AiThreadIntentRecord, AiThreadParticipantRecord, AiThreadProviderMetadataRecord,
            AiThreadRecord, AiValidationReportRecord, ApprovedPermissionRecord,
            AutomationLogRecord, BisectStateRecord, BridgeIndexPublicationRecord,
            ChangeIdentityRecord, ChangePredecessorRecord, ChangeRevisionRecord, GitStateStore,
            INITIAL_WORKTREE_REGISTRY_CAPABILITIES, LayerPathRecord, LayerRecord,
            MEMORY_BY_PATH_INDEX_NAME, MemoryEpisodeEvidenceRecord, MemoryEpisodePathRecord,
            MemoryEpisodeRecord, MemoryEpisodeSearchDocRecord, MemoryProjectionStateRecord,
            MemorySourceKind, OP_HEAD_GENERATION_SENTINEL, OperationHeadCasOutcome,
            OperationHeadConvergeOutcome, OperationHeadsSnapshot, OperationJournalRecord,
            OperationRecord, RB05_WORKFLOW_STATE_SOURCE_TABLES,
            RB05B_AI_MEMORY_BRIDGE_SOURCE_TABLES, RebaseStateRecord, SequenceStateRecord,
            SourceCallLogRecord, SparseViewSnapshot, WorkingDirtyEntryRecord,
            WorkingDirtyMetaRecord, WorkspaceLeaseAcquireOutcome, WorkspaceLeaseMutationOutcome,
            WorkspaceRecordKind, WorkspaceRecordRow, WorkspaceRecordState, WorktreeIntentRecord,
            WorktreeIntentWrite, WorktreeLifecycleRecord, invert_ended_at, normalize_op_heads,
            validate_ai_decision_proposal, validate_ai_final_decision,
            validate_ai_index_intent_context_frame, validate_ai_index_intent_plan,
            validate_ai_index_intent_task, validate_ai_index_plan_step_task,
            validate_ai_index_run_event, validate_ai_index_run_patchset,
            validate_ai_index_task_run, validate_ai_live_context_window,
            validate_ai_operation_link, validate_ai_risk_score_breakdown,
            validate_ai_scheduler_plan_head, validate_ai_scheduler_selected_plan,
            validate_ai_scheduler_state, validate_ai_thread, validate_ai_thread_intent,
            validate_ai_thread_participant, validate_ai_thread_provider_metadata,
            validate_ai_validation_report, validate_approved_permission, validate_automation_log,
            validate_bisect_state, validate_bridge_operation_id, validate_change_identity,
            validate_change_predecessor, validate_change_revision, validate_layer_path_record,
            validate_layer_record, validate_memory_code_path, validate_memory_episode,
            validate_memory_episode_evidence, validate_memory_episode_path,
            validate_memory_projection_state, validate_no_nul, validate_operation_journal,
            validate_operation_record, validate_opt_hex_oid, validate_opt_no_nul,
            validate_rebase_state, validate_sequence_state, validate_source_call_log,
            validate_working_dirty_entry, validate_working_dirty_meta, validate_workspace_record,
            validate_worktree_intent_write, validate_worktree_lifecycle,
        },
        git_store::{
            BridgePublicationPhase, BridgePublicationRecord, validate_hex_oid,
            validate_non_empty_identifier,
        },
        manifest::fsync_dir,
        meta::{begin_checked_read, begin_checked_write_blocking, init_meta_schema},
        opener::{
            RedbRepairContext, begin_write_bounded_blocking, create_redb_writable,
            open_redb_read_only, open_redb_writable,
        },
        redb::{map_redb_commit_err, map_redb_storage_err, map_redb_table_err},
    },
};

/// Current postcard codec version for `RB-05` state tables.
pub const GIT_STATE_CODEC_VERSION: u16 = 1;

/// Default bounded write-lock timeout for [`RedbGitStateStore`] (`30s`).
pub const DEFAULT_GIT_STATE_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

const CHANGE_REVISION_BY_COMMIT_INDEX_NAME: &str = "change_revision_by_commit";
const GIT_STATE_SEQ_TABLE_NAME: &str = "git_state_sequences";
const SEQ_KEY_WORKTREE_INTENT: &str = "worktree_intent_journal";

// 21 RB-05 source tables
pub const SEQUENCE_STATE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("sequence_state");
pub const REBASE_STATE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("rebase_state");
pub const BISECT_STATE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("bisect_state");
pub const WORKTREE_REGISTRY_CAPABILITY_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new("worktree_registry_capability");
pub const WORKTREE_LIFECYCLE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("worktree_lifecycle");
pub const WORKTREE_INTENT_JOURNAL_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new("worktree_intent_journal");
pub const WORKSPACE_RECORD_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("workspace_record");
pub const OPERATION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("operation");
pub const OPERATION_PARENT_TABLE: TableDefinition<'static, (&str, i64), &[u8]> =
    TableDefinition::new("operation_parent");
pub const OPERATION_HEAD_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new("operation_head");
pub const OPERATION_JOURNAL_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("operation_journal");
pub const CHANGE_IDENTITY_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("change_identity");
pub const CHANGE_REVISION_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("change_revision");
pub const CHANGE_PREDECESSOR_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new("change_predecessor");
pub const AI_OPERATION_LINK_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_operation_link");
pub const WORKING_DIRTY_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new("working_dirty");
pub const WORKING_DIRTY_META_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("working_dirty_meta");
pub const LAYER_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("layer");
pub const LAYER_PATH_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("layer_path");
pub const SPARSE_VIEW_TABLE: TableDefinition<'static, (&str, i64), &[u8]> =
    TableDefinition::new("sparse_view");
pub const SPARSE_VIEW_META_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("sparse_view_meta");

// Internal sequence + secondary index tables (distinct from source table count)
pub const CHANGE_REVISION_BY_COMMIT_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new(CHANGE_REVISION_BY_COMMIT_INDEX_NAME);
pub const GIT_STATE_SEQ_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new(GIT_STATE_SEQ_TABLE_NAME);

// 29 RB-05b source tables (19 `ai_*`, 3 governance/audit, 5 `memory_*`, 2 bridge publication)
pub const AI_THREAD_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_thread");
pub const AI_THREAD_PARTICIPANT_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new("ai_thread_participant");
pub const AI_THREAD_INTENT_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_thread_intent");
pub const AI_THREAD_PROVIDER_METADATA_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_thread_provider_metadata");
pub const AI_SCHEDULER_STATE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_scheduler_state");
pub const AI_SCHEDULER_PLAN_HEAD_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_scheduler_plan_head");
pub const AI_SCHEDULER_SELECTED_PLAN_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_scheduler_selected_plan");
pub const AI_LIVE_CONTEXT_WINDOW_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_live_context_window");
pub const AI_INDEX_INTENT_PLAN_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_index_intent_plan");
pub const AI_INDEX_INTENT_TASK_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_index_intent_task");
pub const AI_INDEX_PLAN_STEP_TASK_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_index_plan_step_task");
pub const AI_INDEX_TASK_RUN_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_index_task_run");
pub const AI_INDEX_RUN_EVENT_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_index_run_event");
pub const AI_INDEX_RUN_PATCHSET_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("ai_index_run_patchset");
pub const AI_INDEX_INTENT_CONTEXT_FRAME_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new("ai_index_intent_context_frame");
pub const AI_VALIDATION_REPORT_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_validation_report");
pub const AI_RISK_SCORE_BREAKDOWN_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_risk_score_breakdown");
pub const AI_DECISION_PROPOSAL_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_decision_proposal");
pub const AI_FINAL_DECISION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("ai_final_decision");
pub const APPROVED_PERMISSION_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new("approved_permission");
pub const AUTOMATION_LOG_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("automation_log");
pub const SOURCE_CALL_LOG_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("source_call_log");
pub const MEMORY_EPISODE_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("memory_episode");
pub const MEMORY_EPISODE_EVIDENCE_TABLE: TableDefinition<'static, (&str, i64), &[u8]> =
    TableDefinition::new("memory_episode_evidence");
pub const MEMORY_PROJECTION_STATE_TABLE: TableDefinition<'static, (&str, &str), &[u8]> =
    TableDefinition::new("memory_projection_state");
pub const MEMORY_EPISODE_PATH_TABLE: TableDefinition<'static, (&str, &[u8]), &[u8]> =
    TableDefinition::new("memory_episode_path");
pub const MEMORY_EPISODE_SEARCH_DOC_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new("memory_episode_search_doc");
/// Shared wire-for-wire with [`crate::storage::git_store::redb::BRIDGE_PUBLICATION_TABLE`].
pub const AGENT_BRIDGE_PUBLICATION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_bridge_publication");
pub const AGENT_BRIDGE_INDEX_PUBLICATION_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new("agent_bridge_index_publication");

/// Same-transaction secondary index for `memory_episode_path` ordered by
/// `(code_path, invert_ended_at(ended_at), episode_id)` -> `(ended_at DESC, episode_id ASC)`.
pub const MEMORY_BY_PATH_TABLE: TableDefinition<'static, (&[u8], u64, &str), &[u8]> =
    TableDefinition::new(MEMORY_BY_PATH_INDEX_NAME);

/// Wire-compatible value layout matching `RedbGitStore::BRIDGE_PUBLICATION_TABLE` (`RB-04`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BridgePublicationStoredRow {
    commit_oid: String,
    phase: BridgePublicationPhase,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct BridgeIndexPublicationStoredRow {
    index_digest: String,
    phase: BridgePublicationPhase,
}

static GIT_STATE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct UnitMarker;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct SeqCounter {
    last_id: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StringValue {
    value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct U64Value {
    value: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct BoolValue {
    value: bool,
}

fn rb05_all_table_names() -> Vec<&'static str> {
    let mut names = Vec::with_capacity(RB05_WORKFLOW_STATE_SOURCE_TABLES.len() + 2);
    names.extend_from_slice(&RB05_WORKFLOW_STATE_SOURCE_TABLES);
    names.push(CHANGE_REVISION_BY_COMMIT_INDEX_NAME);
    names.push(GIT_STATE_SEQ_TABLE_NAME);
    names
}

fn classify_rb05_table_layout(existing: &[String]) -> StorageResult<bool> {
    let expected = rb05_all_table_names();
    let present = expected
        .iter()
        .filter(|name| existing.iter().any(|e| e == *name))
        .count();
    if present == expected.len() {
        Ok(true)
    } else if present == 0 {
        Ok(false)
    } else {
        let missing = expected
            .iter()
            .find(|name| !existing.iter().any(|e| e == *name))
            .copied()
            .unwrap_or("unknown");
        Err(StorageBackendError::BackendInternal {
            detail: format!(
                "redb git state store has incomplete RB-05 schema; missing `{missing}`"
            ),
        })
    }
}

fn initialize_rb05_state_tables(wtx: &WriteTransaction) -> StorageResult<()> {
    let _ = wtx
        .open_table(SEQUENCE_STATE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init sequence_state"))?;
    let _ = wtx
        .open_table(REBASE_STATE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init rebase_state"))?;
    let _ = wtx
        .open_table(BISECT_STATE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init bisect_state"))?;
    {
        let mut cap_table = wtx
            .open_table(WORKTREE_REGISTRY_CAPABILITY_TABLE)
            .map_err(|err| map_redb_table_err(err, "init worktree_registry_capability"))?;
        let marker = encode_versioned(GIT_STATE_CODEC_VERSION, &UnitMarker)?;
        for ver in INITIAL_WORKTREE_REGISTRY_CAPABILITIES {
            cap_table
                .insert(ver, marker.as_slice())
                .map_err(|err| map_redb_storage_err(err, "seed worktree_registry_capability"))?;
        }
    }
    let _ = wtx
        .open_table(WORKTREE_LIFECYCLE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init worktree_lifecycle"))?;
    let _ = wtx
        .open_table(WORKTREE_INTENT_JOURNAL_TABLE)
        .map_err(|err| map_redb_table_err(err, "init worktree_intent_journal"))?;
    let _ = wtx
        .open_table(WORKSPACE_RECORD_TABLE)
        .map_err(|err| map_redb_table_err(err, "init workspace_record"))?;
    let _ = wtx
        .open_table(OPERATION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init operation"))?;
    let _ = wtx
        .open_table(OPERATION_PARENT_TABLE)
        .map_err(|err| map_redb_table_err(err, "init operation_parent"))?;
    let _ = wtx
        .open_table(OPERATION_HEAD_TABLE)
        .map_err(|err| map_redb_table_err(err, "init operation_head"))?;
    let _ = wtx
        .open_table(OPERATION_JOURNAL_TABLE)
        .map_err(|err| map_redb_table_err(err, "init operation_journal"))?;
    let _ = wtx
        .open_table(CHANGE_IDENTITY_TABLE)
        .map_err(|err| map_redb_table_err(err, "init change_identity"))?;
    let _ = wtx
        .open_table(CHANGE_REVISION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init change_revision"))?;
    let _ = wtx
        .open_table(CHANGE_REVISION_BY_COMMIT_TABLE)
        .map_err(|err| map_redb_table_err(err, "init change_revision_by_commit"))?;
    let _ = wtx
        .open_table(CHANGE_PREDECESSOR_TABLE)
        .map_err(|err| map_redb_table_err(err, "init change_predecessor"))?;
    let _ = wtx
        .open_table(AI_OPERATION_LINK_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_operation_link"))?;
    let _ = wtx
        .open_table(WORKING_DIRTY_TABLE)
        .map_err(|err| map_redb_table_err(err, "init working_dirty"))?;
    let _ = wtx
        .open_table(WORKING_DIRTY_META_TABLE)
        .map_err(|err| map_redb_table_err(err, "init working_dirty_meta"))?;
    let _ = wtx
        .open_table(LAYER_TABLE)
        .map_err(|err| map_redb_table_err(err, "init layer"))?;
    let _ = wtx
        .open_table(LAYER_PATH_TABLE)
        .map_err(|err| map_redb_table_err(err, "init layer_path"))?;
    let _ = wtx
        .open_table(SPARSE_VIEW_TABLE)
        .map_err(|err| map_redb_table_err(err, "init sparse_view"))?;
    let _ = wtx
        .open_table(SPARSE_VIEW_META_TABLE)
        .map_err(|err| map_redb_table_err(err, "init sparse_view_meta"))?;
    {
        let mut seq_table = wtx
            .open_table(GIT_STATE_SEQ_TABLE)
            .map_err(|err| map_redb_table_err(err, "init git_state_sequences"))?;
        let zero = encode_versioned(GIT_STATE_CODEC_VERSION, &SeqCounter { last_id: 0 })?;
        seq_table
            .insert(SEQ_KEY_WORKTREE_INTENT, zero.as_slice())
            .map_err(|err| map_redb_storage_err(err, "seed worktree_intent_journal sequence"))?;
    }
    Ok(())
}

fn rb05b_new_table_names() -> Vec<&'static str> {
    let mut names = Vec::with_capacity(RB05B_AI_MEMORY_BRIDGE_SOURCE_TABLES.len());
    for name in RB05B_AI_MEMORY_BRIDGE_SOURCE_TABLES {
        // `agent_bridge_publication` may already exist on pre-RB-05b `RB-04`/`RB-04b`
        // stores, so classify it alongside the 28 new RB-05b source tables + `MEMORY_BY_PATH`.
        if name != "agent_bridge_publication" {
            names.push(name);
        }
    }
    names.push(MEMORY_BY_PATH_INDEX_NAME);
    names
}

fn classify_rb05b_table_layout(existing: &[String]) -> StorageResult<bool> {
    let expected = rb05b_new_table_names();
    let present = expected
        .iter()
        .filter(|name| existing.iter().any(|e| e == *name))
        .count();
    if present == expected.len() {
        if !existing.iter().any(|e| e == "agent_bridge_publication") {
            return Err(StorageBackendError::BackendInternal {
                detail: "redb git state store has incomplete RB-05b schema; missing `agent_bridge_publication`"
                    .to_string(),
            });
        }
        Ok(true)
    } else if present == 0 {
        Ok(false)
    } else {
        let missing = expected
            .iter()
            .find(|name| !existing.iter().any(|e| e == *name))
            .copied()
            .unwrap_or("unknown");
        Err(StorageBackendError::BackendInternal {
            detail: format!(
                "redb git state store has incomplete RB-05b schema; missing `{missing}`"
            ),
        })
    }
}

fn initialize_rb05b_state_tables(wtx: &WriteTransaction) -> StorageResult<()> {
    let _ = wtx
        .open_table(AI_THREAD_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_thread"))?;
    let _ = wtx
        .open_table(AI_THREAD_PARTICIPANT_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_thread_participant"))?;
    let _ = wtx
        .open_table(AI_THREAD_INTENT_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_thread_intent"))?;
    let _ = wtx
        .open_table(AI_THREAD_PROVIDER_METADATA_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_thread_provider_metadata"))?;
    let _ = wtx
        .open_table(AI_SCHEDULER_STATE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_scheduler_state"))?;
    let _ = wtx
        .open_table(AI_SCHEDULER_PLAN_HEAD_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_scheduler_plan_head"))?;
    let _ = wtx
        .open_table(AI_SCHEDULER_SELECTED_PLAN_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_scheduler_selected_plan"))?;
    let _ = wtx
        .open_table(AI_LIVE_CONTEXT_WINDOW_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_live_context_window"))?;
    let _ = wtx
        .open_table(AI_INDEX_INTENT_PLAN_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_index_intent_plan"))?;
    let _ = wtx
        .open_table(AI_INDEX_INTENT_TASK_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_index_intent_task"))?;
    let _ = wtx
        .open_table(AI_INDEX_PLAN_STEP_TASK_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_index_plan_step_task"))?;
    let _ = wtx
        .open_table(AI_INDEX_TASK_RUN_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_index_task_run"))?;
    let _ = wtx
        .open_table(AI_INDEX_RUN_EVENT_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_index_run_event"))?;
    let _ = wtx
        .open_table(AI_INDEX_RUN_PATCHSET_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_index_run_patchset"))?;
    let _ = wtx
        .open_table(AI_INDEX_INTENT_CONTEXT_FRAME_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_index_intent_context_frame"))?;
    let _ = wtx
        .open_table(AI_VALIDATION_REPORT_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_validation_report"))?;
    let _ = wtx
        .open_table(AI_RISK_SCORE_BREAKDOWN_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_risk_score_breakdown"))?;
    let _ = wtx
        .open_table(AI_DECISION_PROPOSAL_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_decision_proposal"))?;
    let _ = wtx
        .open_table(AI_FINAL_DECISION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init ai_final_decision"))?;
    let _ = wtx
        .open_table(APPROVED_PERMISSION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init approved_permission"))?;
    let _ = wtx
        .open_table(AUTOMATION_LOG_TABLE)
        .map_err(|err| map_redb_table_err(err, "init automation_log"))?;
    let _ = wtx
        .open_table(SOURCE_CALL_LOG_TABLE)
        .map_err(|err| map_redb_table_err(err, "init source_call_log"))?;
    let _ = wtx
        .open_table(MEMORY_EPISODE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init memory_episode"))?;
    let _ = wtx
        .open_table(MEMORY_EPISODE_EVIDENCE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init memory_episode_evidence"))?;
    let _ = wtx
        .open_table(MEMORY_PROJECTION_STATE_TABLE)
        .map_err(|err| map_redb_table_err(err, "init memory_projection_state"))?;
    let _ = wtx
        .open_table(MEMORY_EPISODE_PATH_TABLE)
        .map_err(|err| map_redb_table_err(err, "init memory_episode_path"))?;
    let _ = wtx
        .open_table(MEMORY_BY_PATH_TABLE)
        .map_err(|err| map_redb_table_err(err, "init MEMORY_BY_PATH"))?;
    let _ = wtx
        .open_table(MEMORY_EPISODE_SEARCH_DOC_TABLE)
        .map_err(|err| map_redb_table_err(err, "init memory_episode_search_doc"))?;
    // Open idempotently without disturbing any pre-existing `RB-04` bridge rows.
    let _ = wtx
        .open_table(AGENT_BRIDGE_PUBLICATION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init agent_bridge_publication"))?;
    let _ = wtx
        .open_table(AGENT_BRIDGE_INDEX_PUBLICATION_TABLE)
        .map_err(|err| map_redb_table_err(err, "init agent_bridge_index_publication"))?;
    Ok(())
}

fn verify_rb05_tables_read(rtx: &ReadTransaction) -> StorageResult<(bool, bool)> {
    let existing: Vec<String> = rtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb git state tables"))?
        .map(|h| h.name().to_string())
        .collect();
    let has_rb05 = classify_rb05_table_layout(&existing)?;
    let has_rb05b = classify_rb05b_table_layout(&existing)?;
    Ok((has_rb05, has_rb05b))
}

fn verify_rb05_tables_write(wtx: &WriteTransaction) -> StorageResult<()> {
    let existing: Vec<String> = wtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb git state tables for write"))?
        .map(|h| h.name().to_string())
        .collect();
    if !classify_rb05_table_layout(&existing)? || !classify_rb05b_table_layout(&existing)? {
        return Err(StorageBackendError::BackendInternal {
            detail: "redb git state store requires RB-05/RB-05b table initialization before write"
                .to_string(),
        });
    }
    Ok(())
}

fn upgrade_rb05_tables_if_needed(db: &Database, timeout: Duration) -> StorageResult<()> {
    let rtx = begin_checked_read(db, DatabaseRole::Repository)?;
    let (has_rb05, has_rb05b) = verify_rb05_tables_read(&rtx)?;
    drop(rtx);
    if has_rb05 && has_rb05b {
        return Ok(());
    }
    let wtx = begin_checked_write_blocking(db, DatabaseRole::Repository, timeout)?;
    let existing: Vec<String> = wtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list redb tables for RB-05/RB-05b upgrade"))?
        .map(|h| h.name().to_string())
        .collect();
    let cur_rb05 = classify_rb05_table_layout(&existing)?;
    let cur_rb05b = classify_rb05b_table_layout(&existing)?;
    if !cur_rb05 || !cur_rb05b {
        if !cur_rb05 {
            initialize_rb05_state_tables(&wtx)?;
        }
        if !cur_rb05b {
            initialize_rb05b_state_tables(&wtx)?;
        }
        wtx.commit()
            .map_err(|err| map_redb_commit_err(err, "commit RB-05/RB-05b redb schema upgrade"))?;
    }
    Ok(())
}

fn open_opt_table<K: redb::Key + 'static, V: redb::Value + 'static>(
    rtx: &ReadTransaction,
    definition: TableDefinition<'_, K, V>,
    ctx: &'static str,
) -> StorageResult<Option<redb::ReadOnlyTable<K, V>>> {
    match rtx.open_table(definition) {
        Ok(table) => Ok(Some(table)),
        Err(redb::TableError::TableDoesNotExist(_)) => Ok(None),
        Err(err) => Err(map_redb_table_err(err, ctx)),
    }
}

fn next_worktree_intent_id(wtx: &WriteTransaction) -> StorageResult<i64> {
    let mut seq_table = wtx
        .open_table(GIT_STATE_SEQ_TABLE)
        .map_err(|err| map_redb_table_err(err, "open git_state_sequences"))?;
    let current = seq_table
        .get(SEQ_KEY_WORKTREE_INTENT)
        .map_err(|err| map_redb_storage_err(err, "read worktree_intent sequence"))?
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "missing worktree_intent_journal sequence counter".to_string(),
        })?;
    let prev: SeqCounter = decode_versioned(current.value(), GIT_STATE_CODEC_VERSION)?;
    drop(current);
    let next = prev
        .last_id
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "worktree_intent_journal sequence counter overflowed i64".to_string(),
        })?;
    let encoded = encode_versioned(GIT_STATE_CODEC_VERSION, &SeqCounter { last_id: next })?;
    seq_table
        .insert(SEQ_KEY_WORKTREE_INTENT, encoded.as_slice())
        .map_err(|err| map_redb_storage_err(err, "advance worktree_intent sequence"))?;
    Ok(next)
}

fn read_op_heads_from_table<
    T: ReadableTable<(&'static str, &'static str, &'static str), &'static [u8]>,
>(
    table: &T,
    repo_id: &str,
    scope_key: &str,
) -> StorageResult<OperationHeadsSnapshot> {
    let mut generation = 0u64;
    let mut heads = Vec::new();
    let range = table
        .range((repo_id, scope_key, "")..)
        .map_err(|err| map_redb_storage_err(err, "range scan operation_head"))?;
    for entry in range {
        let (k, v) = entry.map_err(|err| map_redb_storage_err(err, "read operation_head"))?;
        let (found_repo, found_scope, op_id) = k.value();
        if found_repo != repo_id || found_scope != scope_key {
            break;
        }
        let row_gen: U64Value = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
        if row_gen.value > generation {
            generation = row_gen.value;
        }
        if op_id != OP_HEAD_GENERATION_SENTINEL {
            heads.push(op_id.to_string());
        }
    }
    Ok(OperationHeadsSnapshot { generation, heads })
}

fn insert_operation_records_in_wtx(
    wtx: &redb::WriteTransaction,
    op_owned: &OperationRecord,
    parents_owned: &[String],
    journal_owned: Option<&OperationJournalRecord>,
) -> StorageResult<()> {
    let mut op_table = wtx
        .open_table(OPERATION_TABLE)
        .map_err(|err| map_redb_table_err(err, "open operation"))?;
    if op_table
        .get(op_owned.op_id.as_str())
        .map_err(|err| map_redb_storage_err(err, "check duplicate op_id"))?
        .is_some()
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "duplicate operation op_id".to_string(),
        });
    }

    // Enforce SQLite's `idx_operation_control_slot`:
    // UNIQUE(repo_id, COALESCE(worktree_id, '')) WHERE status = 'running' AND control_slot IS NOT NULL
    if op_owned.status == "running" && op_owned.control_slot.is_some() {
        let target_wt = op_owned.worktree_id.as_deref().unwrap_or("");
        let mut slot_conflict = false;
        {
            let iter = op_table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "scan operation control slots"))?;
            for entry in iter {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read operation row"))?;
                let existing: OperationRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if existing.repo_id == op_owned.repo_id
                    && existing.status == "running"
                    && existing.control_slot.is_some()
                    && existing.worktree_id.as_deref().unwrap_or("") == target_wt
                {
                    slot_conflict = true;
                    break;
                }
            }
        }
        if slot_conflict {
            return Err(StorageBackendError::BackendInternal {
                detail: "active running operation control_slot already held for scope".to_string(),
            });
        }
    }

    let op_bytes = encode_versioned(GIT_STATE_CODEC_VERSION, op_owned)?;
    op_table
        .insert(op_owned.op_id.as_str(), op_bytes.as_slice())
        .map_err(|err| map_redb_storage_err(err, "insert operation"))?;
    drop(op_table);

    let mut parent_table = wtx
        .open_table(OPERATION_PARENT_TABLE)
        .map_err(|err| map_redb_table_err(err, "open operation_parent"))?;
    for (ordinal, parent_id) in parents_owned.iter().enumerate() {
        let ord_i64 = i64::try_from(ordinal).unwrap_or(i64::MAX);
        let p_bytes = encode_versioned(
            GIT_STATE_CODEC_VERSION,
            &StringValue {
                value: parent_id.clone(),
            },
        )?;
        parent_table
            .insert((op_owned.op_id.as_str(), ord_i64), p_bytes.as_slice())
            .map_err(|err| map_redb_storage_err(err, "insert operation_parent"))?;
    }
    drop(parent_table);

    if let Some(jrn) = journal_owned {
        let mut jrn_table = wtx
            .open_table(OPERATION_JOURNAL_TABLE)
            .map_err(|err| map_redb_table_err(err, "open operation_journal"))?;
        if jrn_table
            .get(jrn.journal_id.as_str())
            .map_err(|err| map_redb_storage_err(err, "check duplicate journal_id"))?
            .is_some()
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "duplicate operation_journal journal_id".to_string(),
            });
        }
        let j_bytes = encode_versioned(GIT_STATE_CODEC_VERSION, jrn)?;
        jrn_table
            .insert(jrn.journal_id.as_str(), j_bytes.as_slice())
            .map_err(|err| map_redb_storage_err(err, "insert operation_journal"))?;
    }

    Ok(())
}

fn converge_op_heads_in_wtx(
    wtx: &redb::WriteTransaction,
    repo_id: &str,
    scope_key: &str,
    expected: &[String],
    replacement: &[String],
) -> StorageResult<OperationHeadConvergeOutcome> {
    let mut table = wtx
        .open_table(OPERATION_HEAD_TABLE)
        .map_err(|err| map_redb_table_err(err, "open operation_head"))?;
    let current = read_op_heads_from_table(&table, repo_id, scope_key)?;
    let converged_cleanly = current.heads == expected;
    let expected_set: BTreeSet<&str> = expected.iter().map(String::as_str).collect();
    let mut merged: BTreeSet<String> = current
        .heads
        .into_iter()
        .filter(|h| !expected_set.contains(h.as_str()))
        .collect();
    merged.extend(replacement.iter().cloned());
    let merged_vec: Vec<String> = merged.into_iter().collect();
    let next_gen =
        current
            .generation
            .checked_add(1)
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "operation_head generation overflowed u64".to_string(),
            })?;
    replace_op_heads_in_table(&mut table, repo_id, scope_key, next_gen, &merged_vec)?;
    Ok(OperationHeadConvergeOutcome {
        generation: next_gen,
        heads: merged_vec,
        converged_cleanly,
    })
}

fn replace_op_heads_in_table(
    table: &mut redb::Table<'_, (&'static str, &'static str, &'static str), &'static [u8]>,
    repo_id: &str,
    scope_key: &str,
    generation: u64,
    heads: &[String],
) -> StorageResult<()> {
    let mut old_keys = Vec::new();
    {
        let range = table
            .range((repo_id, scope_key, "")..)
            .map_err(|err| map_redb_storage_err(err, "scan old operation_head rows"))?;
        for entry in range {
            let (k, _) =
                entry.map_err(|err| map_redb_storage_err(err, "read old operation_head key"))?;
            let (found_repo, found_scope, op_id) = k.value();
            if found_repo != repo_id || found_scope != scope_key {
                break;
            }
            old_keys.push(op_id.to_string());
        }
    }
    for op_id in old_keys {
        let _ = table
            .remove((repo_id, scope_key, op_id.as_str()))
            .map_err(|err| map_redb_storage_err(err, "remove old operation_head row"))?;
    }

    let gen_bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &U64Value { value: generation })?;
    table
        .insert(
            (repo_id, scope_key, OP_HEAD_GENERATION_SENTINEL),
            gen_bytes.as_slice(),
        )
        .map_err(|err| map_redb_storage_err(err, "insert operation_head sentinel"))?;
    for head in heads {
        table
            .insert((repo_id, scope_key, head.as_str()), gen_bytes.as_slice())
            .map_err(|err| map_redb_storage_err(err, "insert operation_head row"))?;
    }
    Ok(())
}

struct TempPathGuard(Option<PathBuf>);

impl Drop for TempPathGuard {
    fn drop(&mut self) {
        if let Some(p) = self.0.take() {
            let _ = fs::remove_file(p);
        }
    }
}

#[derive(Clone)]
enum RedbGitStateHandle {
    ReadWrite(Arc<Database>),
    ReadOnly(Arc<ReadOnlyDatabase>),
}

/// `redb 5.0` implementation of [`GitStateStore`] (`RB-05`).
#[derive(Clone)]
pub struct RedbGitStateStore {
    handle: RedbGitStateHandle,
    write_timeout: Duration,
}

impl std::fmt::Debug for RedbGitStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedbGitStateStore")
            .field("read_only", &self.is_read_only())
            .field("write_timeout", &self.write_timeout)
            .finish()
    }
}

impl RedbGitStateStore {
    /// Atomically create a brand-new `git.redb` store with `META_SCHEMA(Repository)`
    /// and all `RB-05` state tables initialized (`0600` on Unix, `hard_link` publish).
    pub fn create(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref();
        if fs::symlink_metadata(path).is_ok() {
            return Err(StorageBackendError::Io {
                context: "create redb git state store",
                kind: io::ErrorKind::AlreadyExists,
                raw_os_error: None,
            });
        }
        let parent = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };
        let file_name = path.file_name().and_then(|n| n.to_str()).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: "redb git state store path must end with valid UTF-8 filename".to_string(),
            }
        })?;
        let seq = GIT_STATE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = parent.join(format!("{file_name}.{}.{seq}.tmp", std::process::id()));
        let mut tmp_guard = TempPathGuard(Some(tmp_path.clone()));

        {
            let mut opts = fs::OpenOptions::new();
            opts.read(true).write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
                opts.mode(crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE)
                    .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
                let file = opts.open(&tmp_path).map_err(|err| {
                    StorageBackendError::io("stage redb git state store temp file", err)
                })?;
                file.set_permissions(fs::Permissions::from_mode(
                    crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE,
                ))
                .map_err(|err| {
                    StorageBackendError::io("fchmod staged redb git state store to 0600", err)
                })?;
                drop(file);
            }
            #[cfg(not(unix))]
            {
                let file = opts.open(&tmp_path).map_err(|err| {
                    StorageBackendError::io("stage redb git state store temp file", err)
                })?;
                drop(file);
            }
            let tmp_db = create_redb_writable(
                &tmp_path,
                DatabaseRole::Repository,
                RedbRepairContext::WriteCommand,
            )?;
            let wtx = begin_write_bounded_blocking(
                &tmp_db,
                DatabaseRole::Repository,
                DEFAULT_GIT_STATE_WRITE_TIMEOUT,
            )?;
            init_meta_schema(&wtx, DatabaseRole::Repository)?;
            initialize_rb05_state_tables(&wtx)?;
            initialize_rb05b_state_tables(&wtx)?;
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit initial redb git state schema"))?;
        }

        fs::hard_link(&tmp_path, path)
            .map_err(|err| StorageBackendError::io("publish staged redb git state store", err))?;
        fs::remove_file(&tmp_path).map_err(|err| {
            StorageBackendError::io("unlink staged redb git state store alias", err)
        })?;
        tmp_guard.0 = None;
        fsync_dir(parent)?;

        Self::open_read_write(path)
    }

    /// Open an existing `git.redb` writable (safely upgrading a pre-RB-05 store).
    pub fn open_read_write(path: impl AsRef<Path>) -> StorageResult<Self> {
        Self::open_read_write_with_context(path, RedbRepairContext::WriteCommand)
    }

    /// Open an existing `git.redb` writable under an explicit [`RedbRepairContext`].
    pub fn open_read_write_with_context(
        path: impl AsRef<Path>,
        context: RedbRepairContext,
    ) -> StorageResult<Self> {
        let db = open_redb_writable(path, DatabaseRole::Repository, context)?;
        upgrade_rb05_tables_if_needed(&db, DEFAULT_GIT_STATE_WRITE_TIMEOUT)?;
        Ok(Self {
            handle: RedbGitStateHandle::ReadWrite(Arc::new(db)),
            write_timeout: DEFAULT_GIT_STATE_WRITE_TIMEOUT,
        })
    }

    /// Open an existing `git.redb` strictly read-only without mutating disk.
    pub fn open_read_only(path: impl AsRef<Path>) -> StorageResult<Self> {
        let db = open_redb_read_only(path, DatabaseRole::Repository)?;
        let rtx = begin_checked_read(&db, DatabaseRole::Repository)?;
        let _ = verify_rb05_tables_read(&rtx)?;
        drop(rtx);
        Ok(Self {
            handle: RedbGitStateHandle::ReadOnly(Arc::new(db)),
            write_timeout: DEFAULT_GIT_STATE_WRITE_TIMEOUT,
        })
    }

    /// Wrap an already-opened writable `Arc<Database>` for [`DatabaseRole::Repository`].
    pub fn from_database(db: Arc<Database>) -> StorageResult<Self> {
        upgrade_rb05_tables_if_needed(db.as_ref(), DEFAULT_GIT_STATE_WRITE_TIMEOUT)?;
        Ok(Self {
            handle: RedbGitStateHandle::ReadWrite(db),
            write_timeout: DEFAULT_GIT_STATE_WRITE_TIMEOUT,
        })
    }

    #[must_use]
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    #[must_use]
    pub fn is_read_only(&self) -> bool {
        matches!(self.handle, RedbGitStateHandle::ReadOnly(_))
    }

    /// Returns all currently materialized redb table names in this database.
    pub async fn list_table_names(&self) -> StorageResult<Vec<String>> {
        self.run_read(|rtx| {
            let mut names: Vec<String> = rtx
                .list_tables()
                .map_err(|err| map_redb_storage_err(err, "list redb git state table names"))?
                .map(|h| h.name().to_string())
                .collect();
            names.sort();
            Ok(names)
        })
        .await
    }

    fn writable_db(&self) -> StorageResult<Arc<Database>> {
        match &self.handle {
            RedbGitStateHandle::ReadWrite(db) => Ok(Arc::clone(db)),
            RedbGitStateHandle::ReadOnly(_) => Err(StorageBackendError::BackendInternal {
                detail: "cannot mutate a read-only git state store".to_string(),
            }),
        }
    }

    async fn run_read<T, F>(&self, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&ReadTransaction) -> StorageResult<T> + Send + 'static,
    {
        let handle = self.handle.clone();
        tokio::task::spawn_blocking(move || {
            let rtx = match &handle {
                RedbGitStateHandle::ReadWrite(db) => {
                    begin_checked_read(db.as_ref(), DatabaseRole::Repository)?
                }
                RedbGitStateHandle::ReadOnly(db) => {
                    begin_checked_read(db.as_ref(), DatabaseRole::Repository)?
                }
            };
            let _ = verify_rb05_tables_read(&rtx)?;
            op(&rtx)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("redb git state read task failed: {err}"),
        })?
    }

    async fn run_write_conditional<T, F>(&self, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&WriteTransaction) -> StorageResult<(bool, T)> + Send + 'static,
    {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum CancelState {
            Pending,
            Cancelled,
            Committing,
        }

        struct CancelOnDrop {
            state: Arc<Mutex<CancelState>>,
            armed: bool,
        }

        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                if self.armed
                    && let Ok(mut guard) = self.state.lock()
                    && *guard == CancelState::Pending
                {
                    *guard = CancelState::Cancelled;
                }
            }
        }

        let db = self.writable_db()?;
        let timeout = self.write_timeout;
        let state = Arc::new(Mutex::new(CancelState::Pending));
        let mut guard = CancelOnDrop {
            state: Arc::clone(&state),
            armed: true,
        };
        let worker_state = Arc::clone(&state);

        let result = tokio::task::spawn_blocking(move || {
            let is_cancelled = || -> bool {
                worker_state
                    .lock()
                    .map(|s| *s == CancelState::Cancelled)
                    .unwrap_or(true)
            };
            if is_cancelled() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "redb git state write cancelled before starting".to_string(),
                });
            }
            let wtx = begin_checked_write_blocking(&db, DatabaseRole::Repository, timeout)?;
            if is_cancelled() {
                drop(wtx);
                return Err(StorageBackendError::BackendInternal {
                    detail: "redb git state write cancelled after acquiring write lock".to_string(),
                });
            }
            verify_rb05_tables_write(&wtx)?;
            let (should_commit, value) = op(&wtx)?;
            if !should_commit {
                drop(wtx);
                return Ok(value);
            }
            {
                let mut st =
                    worker_state
                        .lock()
                        .map_err(|_| StorageBackendError::BackendInternal {
                            detail: "redb git state cancellation state poisoned".to_string(),
                        })?;
                if *st == CancelState::Cancelled {
                    drop(wtx);
                    return Err(StorageBackendError::BackendInternal {
                        detail: "redb git state write cancelled before commit".to_string(),
                    });
                }
                *st = CancelState::Committing;
            }
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit redb git state write"))?;
            Ok(value)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("redb git state write task failed: {err}"),
        })?;
        guard.armed = false;
        result
    }
}

#[async_trait]
impl GitStateStore for RedbGitStateStore {
    fn backend(&self) -> StorageBackendKind {
        StorageBackendKind::Redb
    }

    async fn get_sequence_state(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<SequenceStateRecord>> {
        validate_no_nul(worktree_id, "sequence_state worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, SEQUENCE_STATE_TABLE, "open sequence_state")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "get sequence_state"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn put_sequence_state(&self, record: &SequenceStateRecord) -> StorageResult<()> {
        validate_sequence_state(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(SEQUENCE_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open sequence_state"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.worktree_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert sequence_state"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn delete_sequence_state(&self, worktree_id: &str) -> StorageResult<bool> {
        validate_no_nul(worktree_id, "sequence_state worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(SEQUENCE_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open sequence_state"))?;
            let removed = table
                .remove(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "delete sequence_state"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn list_sequence_states(&self) -> StorageResult<Vec<SequenceStateRecord>> {
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, SEQUENCE_STATE_TABLE, "open sequence_state")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter sequence_state"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read sequence_state"))?;
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn get_rebase_state(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<RebaseStateRecord>> {
        validate_no_nul(worktree_id, "rebase_state worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, REBASE_STATE_TABLE, "open rebase_state")? else {
                return Ok(None);
            };
            let Some(g) = table
                .get(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "get rebase_state"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn put_rebase_state(&self, record: &RebaseStateRecord) -> StorageResult<()> {
        validate_rebase_state(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(REBASE_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open rebase_state"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.worktree_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert rebase_state"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn delete_rebase_state(&self, worktree_id: &str) -> StorageResult<bool> {
        validate_no_nul(worktree_id, "rebase_state worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(REBASE_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open rebase_state"))?;
            let removed = table
                .remove(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "delete rebase_state"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn list_rebase_states(&self) -> StorageResult<Vec<RebaseStateRecord>> {
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, REBASE_STATE_TABLE, "open rebase_state")? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter rebase_state"))?
            {
                let (_, v) = entry.map_err(|err| map_redb_storage_err(err, "read rebase_state"))?;
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn get_bisect_state(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<BisectStateRecord>> {
        validate_no_nul(worktree_id, "bisect_state worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, BISECT_STATE_TABLE, "open bisect_state")? else {
                return Ok(None);
            };
            let Some(g) = table
                .get(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "get bisect_state"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn put_bisect_state(&self, record: &BisectStateRecord) -> StorageResult<()> {
        validate_bisect_state(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(BISECT_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open bisect_state"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.worktree_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert bisect_state"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn delete_bisect_state(&self, worktree_id: &str) -> StorageResult<bool> {
        validate_no_nul(worktree_id, "bisect_state worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(BISECT_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open bisect_state"))?;
            let removed = table
                .remove(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "delete bisect_state"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn list_bisect_states(&self) -> StorageResult<Vec<BisectStateRecord>> {
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, BISECT_STATE_TABLE, "open bisect_state")? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter bisect_state"))?
            {
                let (_, v) = entry.map_err(|err| map_redb_storage_err(err, "read bisect_state"))?;
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn record_worktree_registry_capability(&self, version: i64) -> StorageResult<()> {
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKTREE_REGISTRY_CAPABILITY_TABLE)
                .map_err(|err| map_redb_table_err(err, "open worktree_registry_capability"))?;
            if table
                .get(version)
                .map_err(|err| map_redb_storage_err(err, "get worktree_registry_capability"))?
                .is_some()
            {
                return Ok((false, ()));
            }
            let marker = encode_versioned(GIT_STATE_CODEC_VERSION, &UnitMarker)?;
            table
                .insert(version, marker.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert worktree_registry_capability"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_worktree_registry_capabilities(&self) -> StorageResult<Vec<i64>> {
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                WORKTREE_REGISTRY_CAPABILITY_TABLE,
                "open worktree_registry_capability",
            )?
            else {
                return Ok(INITIAL_WORKTREE_REGISTRY_CAPABILITIES.to_vec());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter worktree_registry_capability"))?
            {
                let (k, _) = entry.map_err(|err| {
                    map_redb_storage_err(err, "read worktree_registry_capability")
                })?;
                out.push(k.value());
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_worktree_lifecycle(
        &self,
        record: &WorktreeLifecycleRecord,
    ) -> StorageResult<()> {
        validate_worktree_lifecycle(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKTREE_LIFECYCLE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open worktree_lifecycle"))?;
            let existing: Option<WorktreeLifecycleRecord> = match table
                .get(rec.worktree_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read worktree_lifecycle"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let mut next = rec;
            if let Some(prev) = existing {
                next.created_at = prev.created_at;
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &next)?;
            table
                .insert(next.worktree_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert worktree_lifecycle"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_worktree_lifecycle(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<WorktreeLifecycleRecord>> {
        validate_non_empty_identifier(worktree_id, "worktree_lifecycle worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, WORKTREE_LIFECYCLE_TABLE, "open worktree_lifecycle")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "get worktree_lifecycle"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn delete_worktree_lifecycle(&self, worktree_id: &str) -> StorageResult<bool> {
        validate_non_empty_identifier(worktree_id, "worktree_lifecycle worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKTREE_LIFECYCLE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open worktree_lifecycle"))?;
            let removed = table
                .remove(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "delete worktree_lifecycle"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn list_worktree_lifecycles(&self) -> StorageResult<Vec<WorktreeLifecycleRecord>> {
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, WORKTREE_LIFECYCLE_TABLE, "open worktree_lifecycle")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter worktree_lifecycle"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read worktree_lifecycle"))?;
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn append_worktree_intent(
        &self,
        intent: &WorktreeIntentWrite,
    ) -> StorageResult<WorktreeIntentRecord> {
        validate_worktree_intent_write(intent)?;
        let req = intent.clone();
        self.run_write_conditional(move |wtx| {
            let id = next_worktree_intent_id(wtx)?;
            let record = WorktreeIntentRecord {
                id,
                op: req.op,
                worktree_id: req.worktree_id,
                payload: req.payload,
                stage: req.stage,
                created_at: req.created_at,
            };
            let mut table = wtx
                .open_table(WORKTREE_INTENT_JOURNAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open worktree_intent_journal"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &record)?;
            table
                .insert(id, bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert worktree_intent_journal"))?;
            Ok((true, record))
        })
        .await
    }

    async fn update_worktree_intent_stage(
        &self,
        id: i64,
        stage: Option<&str>,
    ) -> StorageResult<bool> {
        validate_opt_no_nul(stage, "worktree_intent_journal stage")?;
        let stage_owned = stage.map(str::to_string);
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKTREE_INTENT_JOURNAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open worktree_intent_journal"))?;
            let existing: Option<WorktreeIntentRecord> = match table
                .get(id)
                .map_err(|err| map_redb_storage_err(err, "read worktree_intent_journal"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let Some(mut record) = existing else {
                return Ok((false, false));
            };
            record.stage = stage_owned;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &record)?;
            table
                .insert(id, bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "update worktree_intent_journal"))?;
            Ok((true, true))
        })
        .await
    }

    async fn resolve_worktree_intent(&self, id: i64) -> StorageResult<bool> {
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKTREE_INTENT_JOURNAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open worktree_intent_journal"))?;
            let removed = table
                .remove(id)
                .map_err(|err| map_redb_storage_err(err, "delete worktree_intent_journal"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn list_worktree_intents(&self) -> StorageResult<Vec<WorktreeIntentRecord>> {
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                WORKTREE_INTENT_JOURNAL_TABLE,
                "open worktree_intent_journal",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter worktree_intent_journal"))?
            {
                let (_, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read worktree_intent_journal"))?;
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn acquire_workspace_lease(
        &self,
        record: &WorkspaceRecordRow,
    ) -> StorageResult<WorkspaceLeaseAcquireOutcome> {
        validate_workspace_record(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKSPACE_RECORD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open workspace_record"))?;
            if table
                .get(rec.workspace_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "check workspace_record pk"))?
                .is_some()
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "workspace_record workspace_id already exists".to_string(),
                });
            }

            if rec.state.holds_identity() {
                let mut linked_conflict: Option<String> = None;
                let mut path_conflict_id: Option<String> = None;
                {
                    let iter = table
                        .iter()
                        .map_err(|err| map_redb_storage_err(err, "scan workspace_record"))?;
                    for entry in iter {
                        let (_, v) = entry
                            .map_err(|err| map_redb_storage_err(err, "read workspace_record"))?;
                        let row: WorkspaceRecordRow =
                            decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        if row.repo_id != rec.repo_id || !row.state.holds_identity() {
                            continue;
                        }
                        if rec.kind == WorkspaceRecordKind::Linked
                            && row.kind == WorkspaceRecordKind::Linked
                            && row.worktree_id == rec.worktree_id
                            && linked_conflict.is_none()
                        {
                            linked_conflict = Some(row.workspace_id.clone());
                        }
                        if row.path == rec.path && path_conflict_id.is_none() {
                            path_conflict_id = Some(row.workspace_id.clone());
                        }
                    }
                }
                if let Some(holder) = linked_conflict {
                    return Ok((
                        false,
                        WorkspaceLeaseAcquireOutcome::Conflict {
                            conflicting_workspace_id: holder,
                            path_conflict: false,
                        },
                    ));
                }
                if let Some(holder) = path_conflict_id {
                    return Ok((
                        false,
                        WorkspaceLeaseAcquireOutcome::Conflict {
                            conflicting_workspace_id: holder,
                            path_conflict: true,
                        },
                    ));
                }
            }

            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.workspace_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert workspace_record"))?;
            Ok((true, WorkspaceLeaseAcquireOutcome::Acquired(Box::new(rec))))
        })
        .await
    }

    async fn renew_workspace_lease(
        &self,
        workspace_id: &str,
        expected_owner: &str,
        expected_fence: i64,
        new_expires_at: Option<i64>,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome> {
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        validate_non_empty_identifier(expected_owner, "workspace_record lease_owner")?;
        let ws_id = workspace_id.to_string();
        let exp_owner = expected_owner.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKSPACE_RECORD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open workspace_record"))?;
            let existing: Option<WorkspaceRecordRow> = match table
                .get(ws_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read workspace_record"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let Some(mut current) = existing else {
                return Ok((false, WorkspaceLeaseMutationOutcome::NotFound));
            };
            if !current.state.holds_identity()
                || current.lease_owner.as_deref() != Some(exp_owner.as_str())
                || current.lease_fence != expected_fence
            {
                return Ok((
                    false,
                    WorkspaceLeaseMutationOutcome::LeaseLost {
                        actual_state: current.state,
                        actual_owner: current.lease_owner,
                        actual_fence: current.lease_fence,
                    },
                ));
            }
            current.lease_expires_at = new_expires_at;
            current.updated_at = updated_at;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &current)?;
            table
                .insert(ws_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "renew workspace_record"))?;
            Ok((
                true,
                WorkspaceLeaseMutationOutcome::Updated(Box::new(current)),
            ))
        })
        .await
    }

    async fn transition_workspace_lease(
        &self,
        workspace_id: &str,
        expected_owner: &str,
        expected_fence: i64,
        new_state: WorkspaceRecordState,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome> {
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        validate_non_empty_identifier(expected_owner, "workspace_record lease_owner")?;
        let ws_id = workspace_id.to_string();
        let exp_owner = expected_owner.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKSPACE_RECORD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open workspace_record"))?;
            let existing: Option<WorkspaceRecordRow> = match table
                .get(ws_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read workspace_record"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let Some(mut current) = existing else {
                return Ok((false, WorkspaceLeaseMutationOutcome::NotFound));
            };
            if !current.state.holds_identity()
                || current.lease_owner.as_deref() != Some(exp_owner.as_str())
                || current.lease_fence != expected_fence
            {
                return Ok((
                    false,
                    WorkspaceLeaseMutationOutcome::LeaseLost {
                        actual_state: current.state,
                        actual_owner: current.lease_owner,
                        actual_fence: current.lease_fence,
                    },
                ));
            }
            current.state = new_state;
            current.updated_at = updated_at;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &current)?;
            table
                .insert(ws_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "transition workspace_record"))?;
            Ok((
                true,
                WorkspaceLeaseMutationOutcome::Updated(Box::new(current)),
            ))
        })
        .await
    }

    async fn reclaim_expired_workspace_lease(
        &self,
        workspace_id: &str,
        now_ts: i64,
        new_owner: &str,
        new_expires_at: Option<i64>,
        new_state: WorkspaceRecordState,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome> {
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        validate_non_empty_identifier(new_owner, "workspace_record new_owner")?;
        let ws_id = workspace_id.to_string();
        let owner_owned = new_owner.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKSPACE_RECORD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open workspace_record"))?;
            let existing: Option<WorkspaceRecordRow> = match table
                .get(ws_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read workspace_record"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let Some(mut current) = existing else {
                return Ok((false, WorkspaceLeaseMutationOutcome::NotFound));
            };
            let is_expired = current.lease_expires_at.is_some_and(|exp| exp <= now_ts);
            if !current.state.holds_identity() || !is_expired {
                return Ok((
                    false,
                    WorkspaceLeaseMutationOutcome::LeaseLost {
                        actual_state: current.state,
                        actual_owner: current.lease_owner,
                        actual_fence: current.lease_fence,
                    },
                ));
            }
            current.lease_fence = current.lease_fence.checked_add(1).ok_or_else(|| {
                StorageBackendError::BackendInternal {
                    detail: "workspace_record lease_fence overflowed i64".to_string(),
                }
            })?;
            current.state = new_state;
            current.lease_owner = Some(owner_owned);
            current.lease_expires_at = new_expires_at;
            current.updated_at = updated_at;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &current)?;
            table
                .insert(ws_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "reclaim workspace_record"))?;
            Ok((
                true,
                WorkspaceLeaseMutationOutcome::Updated(Box::new(current)),
            ))
        })
        .await
    }

    async fn get_workspace_record(
        &self,
        workspace_id: &str,
    ) -> StorageResult<Option<WorkspaceRecordRow>> {
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        let ws_id = workspace_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, WORKSPACE_RECORD_TABLE, "open workspace_record")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(ws_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "get workspace_record"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn list_workspace_records(
        &self,
        repo_id: &str,
        state_filter: Option<WorkspaceRecordState>,
    ) -> StorageResult<Vec<WorkspaceRecordRow>> {
        validate_non_empty_identifier(repo_id, "workspace_record repo_id")?;
        let repo_owned = repo_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, WORKSPACE_RECORD_TABLE, "open workspace_record")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter workspace_record"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read workspace_record"))?;
                let row: WorkspaceRecordRow = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if row.repo_id != repo_owned {
                    continue;
                }
                if let Some(st) = state_filter
                    && row.state != st
                {
                    continue;
                }
                out.push(row);
            }
            Ok(out)
        })
        .await
    }

    async fn append_operation_atomic(
        &self,
        op: &OperationRecord,
        parent_op_ids: &[String],
        journal: Option<&OperationJournalRecord>,
    ) -> StorageResult<()> {
        validate_operation_record(op, parent_op_ids)?;
        if let Some(jrn) = journal {
            validate_operation_journal(jrn)?;
            if jrn.op_id != op.op_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "operation_journal op_id must match operation op_id".to_string(),
                });
            }
        }
        let op_owned = op.clone();
        let parents_owned = parent_op_ids.to_vec();
        let journal_owned = journal.cloned();

        self.run_write_conditional(move |wtx| {
            insert_operation_records_in_wtx(
                wtx,
                &op_owned,
                &parents_owned,
                journal_owned.as_ref(),
            )?;
            Ok((true, ()))
        })
        .await
    }

    async fn append_operation_and_converge_heads(
        &self,
        op: &OperationRecord,
        parent_op_ids: &[String],
        journal: Option<&OperationJournalRecord>,
        scope_key: &str,
        expected_heads: &[String],
        replacement_heads: &[String],
    ) -> StorageResult<OperationHeadConvergeOutcome> {
        validate_operation_record(op, parent_op_ids)?;
        validate_non_empty_identifier(scope_key, "operation_head scope_key")?;
        if let Some(jrn) = journal {
            validate_operation_journal(jrn)?;
            if jrn.op_id != op.op_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "operation_journal op_id must match operation op_id".to_string(),
                });
            }
        }
        let expected = normalize_op_heads(expected_heads)?;
        let replacement = normalize_op_heads(replacement_heads)?;
        let op_owned = op.clone();
        let parents_owned = parent_op_ids.to_vec();
        let journal_owned = journal.cloned();
        let scope_owned = scope_key.to_string();

        self.run_write_conditional(move |wtx| {
            insert_operation_records_in_wtx(
                wtx,
                &op_owned,
                &parents_owned,
                journal_owned.as_ref(),
            )?;
            let outcome = converge_op_heads_in_wtx(
                wtx,
                &op_owned.repo_id,
                &scope_owned,
                &expected,
                &replacement,
            )?;
            Ok((true, outcome))
        })
        .await
    }

    async fn update_operation_status(
        &self,
        repo_id: &str,
        op_id: &str,
        status: &str,
        post_view_oid: Option<&str>,
        end_ts: Option<i64>,
    ) -> StorageResult<bool> {
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        validate_non_empty_identifier(op_id, "operation op_id")?;
        validate_non_empty_identifier(status, "operation status")?;
        validate_opt_hex_oid(post_view_oid, "operation post_view_oid")?;
        let repo_owned = repo_id.to_string();
        let op_owned = op_id.to_string();
        let status_owned = status.to_string();
        let post_owned = post_view_oid.map(str::to_string);

        self.run_write_conditional(move |wtx| {
            let mut op_table = wtx
                .open_table(OPERATION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open operation"))?;
            let existing: Option<OperationRecord> = match op_table
                .get(op_owned.as_str())
                .map_err(|err| map_redb_storage_err(err, "read operation"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let Some(mut row) = existing else {
                return Ok((false, false));
            };
            if row.repo_id != repo_owned {
                return Ok((false, false));
            }
            if status_owned == "running" && row.control_slot.is_some() {
                let target_wt = row.worktree_id.as_deref().unwrap_or("");
                let mut slot_conflict = false;
                {
                    let iter = op_table
                        .iter()
                        .map_err(|err| map_redb_storage_err(err, "scan operation control slots"))?;
                    for entry in iter {
                        let (_, v) =
                            entry.map_err(|err| map_redb_storage_err(err, "read operation row"))?;
                        let other: OperationRecord =
                            decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        if other.op_id != row.op_id
                            && other.repo_id == row.repo_id
                            && other.status == "running"
                            && other.control_slot.is_some()
                            && other.worktree_id.as_deref().unwrap_or("") == target_wt
                        {
                            slot_conflict = true;
                            break;
                        }
                    }
                }
                if slot_conflict {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "active running operation control_slot already held for scope"
                            .to_string(),
                    });
                }
            }
            row.status = status_owned;
            if let Some(oid) = post_owned {
                row.post_view_oid = oid;
            }
            row.end_ts = end_ts;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &row)?;
            op_table
                .insert(op_owned.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "update operation"))?;
            Ok((true, true))
        })
        .await
    }

    async fn get_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> StorageResult<Option<(OperationRecord, Vec<String>)>> {
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        validate_non_empty_identifier(op_id, "operation op_id")?;
        let repo_owned = repo_id.to_string();
        let op_owned = op_id.to_string();

        self.run_read(move |rtx| {
            let Some(op_table) = open_opt_table(rtx, OPERATION_TABLE, "open operation")? else {
                return Ok(None);
            };
            let Some(g) = op_table
                .get(op_owned.as_str())
                .map_err(|err| map_redb_storage_err(err, "get operation"))?
            else {
                return Ok(None);
            };
            let op: OperationRecord = decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?;
            if op.repo_id != repo_owned {
                return Ok(None);
            }
            let mut parents = Vec::new();
            if let Some(p_table) =
                open_opt_table(rtx, OPERATION_PARENT_TABLE, "open operation_parent")?
            {
                let range = p_table
                    .range((op_owned.as_str(), i64::MIN)..=(op_owned.as_str(), i64::MAX))
                    .map_err(|err| map_redb_storage_err(err, "scan operation_parent"))?;
                for entry in range {
                    let (_, v) =
                        entry.map_err(|err| map_redb_storage_err(err, "read operation_parent"))?;
                    let sv: StringValue = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                    parents.push(sv.value);
                }
            }
            Ok(Some((op, parents)))
        })
        .await
    }

    async fn list_operations(
        &self,
        repo_id: &str,
    ) -> StorageResult<Vec<(OperationRecord, Vec<String>)>> {
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        let repo_owned = repo_id.to_string();

        self.run_read(move |rtx| {
            let Some(op_table) = open_opt_table(rtx, OPERATION_TABLE, "open operation")? else {
                return Ok(Vec::new());
            };
            let p_table = open_opt_table(rtx, OPERATION_PARENT_TABLE, "open operation_parent")?;
            let mut ops = Vec::new();
            for entry in op_table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter operation"))?
            {
                let (_, v) = entry.map_err(|err| map_redb_storage_err(err, "read operation"))?;
                let op: OperationRecord = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if op.repo_id == repo_owned {
                    ops.push(op);
                }
            }
            ops.sort_by(|a, b| {
                a.start_ts
                    .cmp(&b.start_ts)
                    .then_with(|| a.op_id.cmp(&b.op_id))
            });
            let mut out = Vec::with_capacity(ops.len());
            for op in ops {
                let mut parents = Vec::new();
                if let Some(ref pt) = p_table {
                    let range = pt
                        .range((op.op_id.as_str(), i64::MIN)..=(op.op_id.as_str(), i64::MAX))
                        .map_err(|err| map_redb_storage_err(err, "scan operation_parent"))?;
                    for entry in range {
                        let (_, v) = entry
                            .map_err(|err| map_redb_storage_err(err, "read operation_parent"))?;
                        let sv: StringValue = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        parents.push(sv.value);
                    }
                }
                out.push((op, parents));
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_operation_journal(&self, entry: &OperationJournalRecord) -> StorageResult<()> {
        validate_operation_journal(entry)?;
        let jrn = entry.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(OPERATION_JOURNAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open operation_journal"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &jrn)?;
            table
                .insert(jrn.journal_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert operation_journal"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_operation_journals(
        &self,
        op_id: &str,
    ) -> StorageResult<Vec<OperationJournalRecord>> {
        validate_non_empty_identifier(op_id, "operation_journal op_id")?;
        let op_owned = op_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, OPERATION_JOURNAL_TABLE, "open operation_journal")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter operation_journal"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read operation_journal"))?;
                let row: OperationJournalRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if row.op_id == op_owned {
                    out.push(row);
                }
            }
            out.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.journal_id.cmp(&b.journal_id))
            });
            Ok(out)
        })
        .await
    }

    async fn delete_operation_journal(&self, journal_id: &str) -> StorageResult<bool> {
        validate_non_empty_identifier(journal_id, "operation_journal journal_id")?;
        let jid = journal_id.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(OPERATION_JOURNAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open operation_journal"))?;
            let removed = table
                .remove(jid.as_str())
                .map_err(|err| map_redb_storage_err(err, "delete operation_journal"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn get_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
    ) -> StorageResult<OperationHeadsSnapshot> {
        validate_non_empty_identifier(repo_id, "operation_head repo_id")?;
        validate_non_empty_identifier(scope_key, "operation_head scope_key")?;
        let repo_owned = repo_id.to_string();
        let scope_owned = scope_key.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, OPERATION_HEAD_TABLE, "open operation_head")?
            else {
                return Ok(OperationHeadsSnapshot {
                    generation: 0,
                    heads: Vec::new(),
                });
            };
            read_op_heads_from_table(&table, &repo_owned, &scope_owned)
        })
        .await
    }

    async fn cas_update_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_generation: Option<u64>,
        expected_heads: &[String],
        new_heads: &[String],
    ) -> StorageResult<OperationHeadCasOutcome> {
        validate_non_empty_identifier(repo_id, "operation_head repo_id")?;
        validate_non_empty_identifier(scope_key, "operation_head scope_key")?;
        let expected = normalize_op_heads(expected_heads)?;
        let replacement = normalize_op_heads(new_heads)?;
        let repo_owned = repo_id.to_string();
        let scope_owned = scope_key.to_string();

        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(OPERATION_HEAD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open operation_head"))?;
            let current = read_op_heads_from_table(&table, &repo_owned, &scope_owned)?;
            if current.heads != expected
                || expected_generation.is_some_and(|exp| exp != current.generation)
            {
                return Ok((
                    false,
                    OperationHeadCasOutcome::CasConflict {
                        actual_generation: current.generation,
                        actual_heads: current.heads,
                    },
                ));
            }
            let next_gen = current.generation.checked_add(1).ok_or_else(|| {
                StorageBackendError::BackendInternal {
                    detail: "operation_head generation overflowed u64".to_string(),
                }
            })?;
            replace_op_heads_in_table(
                &mut table,
                &repo_owned,
                &scope_owned,
                next_gen,
                &replacement,
            )?;
            Ok((
                true,
                OperationHeadCasOutcome::Applied {
                    generation: next_gen,
                    heads: replacement,
                },
            ))
        })
        .await
    }

    async fn converge_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_heads: &[String],
        replacement_heads: &[String],
    ) -> StorageResult<OperationHeadConvergeOutcome> {
        validate_non_empty_identifier(repo_id, "operation_head repo_id")?;
        validate_non_empty_identifier(scope_key, "operation_head scope_key")?;
        let expected = normalize_op_heads(expected_heads)?;
        let replacement = normalize_op_heads(replacement_heads)?;
        let repo_owned = repo_id.to_string();
        let scope_owned = scope_key.to_string();

        self.run_write_conditional(move |wtx| {
            let outcome =
                converge_op_heads_in_wtx(wtx, &repo_owned, &scope_owned, &expected, &replacement)?;
            Ok((true, outcome))
        })
        .await
    }

    async fn upsert_change_identity(&self, record: &ChangeIdentityRecord) -> StorageResult<()> {
        validate_change_identity(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(CHANGE_IDENTITY_TABLE)
                .map_err(|err| map_redb_table_err(err, "open change_identity"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.change_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert change_identity"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_change_identity(
        &self,
        change_id: &str,
    ) -> StorageResult<Option<ChangeIdentityRecord>> {
        validate_non_empty_identifier(change_id, "change_identity change_id")?;
        let id = change_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, CHANGE_IDENTITY_TABLE, "open change_identity")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(id.as_str())
                .map_err(|err| map_redb_storage_err(err, "get change_identity"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn list_change_identities_by_prefix(
        &self,
        repo_id: &str,
        prefix: &str,
    ) -> StorageResult<Vec<ChangeIdentityRecord>> {
        validate_non_empty_identifier(repo_id, "change_identity repo_id")?;
        validate_no_nul(prefix, "change_identity prefix")?;
        let repo_owned = repo_id.to_string();
        let prefix_owned = prefix.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, CHANGE_IDENTITY_TABLE, "open change_identity")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range(prefix_owned.as_str()..)
                .map_err(|err| map_redb_storage_err(err, "range scan change_identity"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read change_identity"))?;
                if !k.value().starts_with(prefix_owned.as_str()) {
                    break;
                }
                let rec: ChangeIdentityRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.repo_id == repo_owned {
                    out.push(rec);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_change_revision(&self, record: &ChangeRevisionRecord) -> StorageResult<()> {
        validate_change_revision(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut rev_table = wtx
                .open_table(CHANGE_REVISION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open change_revision"))?;
            let mut idx_table = wtx
                .open_table(CHANGE_REVISION_BY_COMMIT_TABLE)
                .map_err(|err| map_redb_table_err(err, "open change_revision_by_commit"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            let marker = encode_versioned(GIT_STATE_CODEC_VERSION, &UnitMarker)?;
            rev_table
                .insert(
                    (rec.change_id.as_str(), rec.commit_oid.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert change_revision"))?;
            idx_table
                .insert(
                    (rec.commit_oid.as_str(), rec.change_id.as_str()),
                    marker.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert change_revision_by_commit"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_change_revisions(
        &self,
        change_id: &str,
    ) -> StorageResult<Vec<ChangeRevisionRecord>> {
        validate_non_empty_identifier(change_id, "change_revision change_id")?;
        let cid = change_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, CHANGE_REVISION_TABLE, "open change_revision")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((cid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "range scan change_revision"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read change_revision"))?;
                if k.value().0 != cid {
                    break;
                }
                let rec: ChangeRevisionRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.revision_ordinal
                    .cmp(&b.revision_ordinal)
                    .then_with(|| a.commit_oid.cmp(&b.commit_oid))
            });
            Ok(out)
        })
        .await
    }

    async fn find_change_revisions_by_commit(
        &self,
        commit_oid: &str,
    ) -> StorageResult<Vec<ChangeRevisionRecord>> {
        validate_hex_oid(commit_oid, "change_revision commit_oid")?;
        let oid = commit_oid.to_string();
        self.run_read(move |rtx| {
            let Some(idx_table) = open_opt_table(
                rtx,
                CHANGE_REVISION_BY_COMMIT_TABLE,
                "open change_revision_by_commit",
            )?
            else {
                return Ok(Vec::new());
            };
            let Some(rev_table) =
                open_opt_table(rtx, CHANGE_REVISION_TABLE, "open change_revision")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = idx_table
                .range((oid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan change_revision_by_commit"))?;
            for entry in range {
                let (k, _) = entry
                    .map_err(|err| map_redb_storage_err(err, "read change_revision_by_commit"))?;
                let (found_oid, change_id) = k.value();
                if found_oid != oid {
                    break;
                }
                let guard = rev_table
                    .get((change_id, found_oid))
                    .map_err(|err| map_redb_storage_err(err, "get change_revision via index"))?
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "change_revision_by_commit references missing change_revision row"
                            .to_string(),
                    })?;
                out.push(decode_versioned(guard.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_change_predecessor(
        &self,
        record: &ChangePredecessorRecord,
    ) -> StorageResult<()> {
        validate_change_predecessor(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(CHANGE_PREDECESSOR_TABLE)
                .map_err(|err| map_redb_table_err(err, "open change_predecessor"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (
                        rec.successor_oid.as_str(),
                        rec.predecessor_oid.as_str(),
                        rec.op_id.as_str(),
                    ),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert change_predecessor"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_change_predecessors(
        &self,
        successor_oid: &str,
    ) -> StorageResult<Vec<ChangePredecessorRecord>> {
        validate_hex_oid(successor_oid, "change_predecessor successor_oid")?;
        let oid = successor_oid.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, CHANGE_PREDECESSOR_TABLE, "open change_predecessor")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((oid.as_str(), "", "")..)
                .map_err(|err| map_redb_storage_err(err, "scan change_predecessor"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read change_predecessor"))?;
                if k.value().0 != oid {
                    break;
                }
                let rec: ChangePredecessorRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.ordinal
                    .cmp(&b.ordinal)
                    .then_with(|| a.op_id.cmp(&b.op_id))
                    .then_with(|| a.predecessor_oid.cmp(&b.predecessor_oid))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_operation_link(&self, record: &AiOperationLinkRecord) -> StorageResult<()> {
        validate_ai_operation_link(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_OPERATION_LINK_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_operation_link"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.operation_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_operation_link"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_ai_operation_link(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<AiOperationLinkRecord>> {
        validate_non_empty_identifier(operation_id, "ai_operation_link operation_id")?;
        let id = operation_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_OPERATION_LINK_TABLE, "open ai_operation_link")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(id.as_str())
                .map_err(|err| map_redb_storage_err(err, "get ai_operation_link"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn list_ai_operation_links_by_change(
        &self,
        repo_id: &str,
        change_id: &str,
    ) -> StorageResult<Vec<AiOperationLinkRecord>> {
        validate_non_empty_identifier(repo_id, "ai_operation_link repo_id")?;
        validate_non_empty_identifier(change_id, "ai_operation_link change_id")?;
        let repo_owned = repo_id.to_string();
        let ch_owned = change_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_OPERATION_LINK_TABLE, "open ai_operation_link")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter ai_operation_link"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_operation_link"))?;
                let rec: AiOperationLinkRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.repo_id == repo_owned && rec.change_id.as_deref() == Some(ch_owned.as_str())
                {
                    out.push(rec);
                }
            }
            Ok(out)
        })
        .await
    }

    async fn replace_working_dirty_snapshot(
        &self,
        meta: &WorkingDirtyMetaRecord,
        entries: &[WorkingDirtyEntryRecord],
    ) -> StorageResult<()> {
        validate_working_dirty_meta(meta)?;
        for e in entries {
            validate_working_dirty_entry(e)?;
            if e.worktree_id != meta.worktree_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "working_dirty entry worktree_id must match meta worktree_id"
                        .to_string(),
                });
            }
        }
        let meta_owned = meta.clone();
        let entries_owned = entries.to_vec();

        self.run_write_conditional(move |wtx| {
            let mut dirty_table = wtx
                .open_table(WORKING_DIRTY_TABLE)
                .map_err(|err| map_redb_table_err(err, "open working_dirty"))?;
            let mut old_keys = Vec::new();
            {
                let range = dirty_table
                    .range((meta_owned.worktree_id.as_str(), "", "")..)
                    .map_err(|err| map_redb_storage_err(err, "scan old working_dirty"))?;
                for entry in range {
                    let (k, _) = entry
                        .map_err(|err| map_redb_storage_err(err, "read old working_dirty key"))?;
                    let (wt, p, kind) = k.value();
                    if wt != meta_owned.worktree_id {
                        break;
                    }
                    old_keys.push((p.to_string(), kind.to_string()));
                }
            }
            for (p, kind) in old_keys {
                let _ = dirty_table
                    .remove((meta_owned.worktree_id.as_str(), p.as_str(), kind.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "remove old working_dirty"))?;
            }
            for e in entries_owned {
                let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &e)?;
                dirty_table
                    .insert(
                        (e.worktree_id.as_str(), e.path.as_str(), e.kind.as_str()),
                        bytes.as_slice(),
                    )
                    .map_err(|err| map_redb_storage_err(err, "insert working_dirty"))?;
            }
            drop(dirty_table);

            let mut meta_table = wtx
                .open_table(WORKING_DIRTY_META_TABLE)
                .map_err(|err| map_redb_table_err(err, "open working_dirty_meta"))?;
            let meta_bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &meta_owned)?;
            meta_table
                .insert(meta_owned.worktree_id.as_str(), meta_bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert working_dirty_meta"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn upsert_working_dirty_entry(
        &self,
        entry: &WorkingDirtyEntryRecord,
    ) -> StorageResult<()> {
        validate_working_dirty_entry(entry)?;
        let e = entry.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(WORKING_DIRTY_TABLE)
                .map_err(|err| map_redb_table_err(err, "open working_dirty"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &e)?;
            table
                .insert(
                    (e.worktree_id.as_str(), e.path.as_str(), e.kind.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert working_dirty"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_working_dirty_meta(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<WorkingDirtyMetaRecord>> {
        validate_no_nul(worktree_id, "working_dirty_meta worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, WORKING_DIRTY_META_TABLE, "open working_dirty_meta")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "get working_dirty_meta"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn list_working_dirty_entries(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Vec<WorkingDirtyEntryRecord>> {
        validate_no_nul(worktree_id, "working_dirty worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, WORKING_DIRTY_TABLE, "open working_dirty")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((wt.as_str(), "", "")..)
                .map_err(|err| map_redb_storage_err(err, "scan working_dirty"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read working_dirty"))?;
                if k.value().0 != wt {
                    break;
                }
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_layer(&self, record: &LayerRecord) -> StorageResult<()> {
        validate_layer_record(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(LAYER_TABLE)
                .map_err(|err| map_redb_table_err(err, "open layer"))?;
            let existing: Option<LayerRecord> = match table
                .get((rec.worktree_id.as_str(), rec.name.as_str()))
                .map_err(|err| map_redb_storage_err(err, "read layer"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let mut next = rec;
            if let Some(prev) = existing {
                next.created_at = prev.created_at;
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &next)?;
            table
                .insert(
                    (next.worktree_id.as_str(), next.name.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert layer"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_layers(&self, worktree_id: &str) -> StorageResult<Vec<LayerRecord>> {
        validate_no_nul(worktree_id, "layer worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, LAYER_TABLE, "open layer")? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((wt.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan layer"))?;
            for entry in range {
                let (k, v) = entry.map_err(|err| map_redb_storage_err(err, "read layer"))?;
                if k.value().0 != wt {
                    break;
                }
                let rec: LayerRecord = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.priority
                    .cmp(&b.priority)
                    .then_with(|| a.name.cmp(&b.name))
            });
            Ok(out)
        })
        .await
    }

    async fn delete_layer(&self, worktree_id: &str, name: &str) -> StorageResult<bool> {
        validate_no_nul(worktree_id, "layer worktree_id")?;
        validate_non_empty_identifier(name, "layer name")?;
        let wt = worktree_id.to_string();
        let name_owned = name.to_string();

        self.run_write_conditional(move |wtx| {
            let mut lp_table = wtx
                .open_table(LAYER_PATH_TABLE)
                .map_err(|err| map_redb_table_err(err, "open layer_path"))?;
            let mut to_remove = Vec::new();
            {
                let range = lp_table
                    .range((wt.as_str(), "")..)
                    .map_err(|err| map_redb_storage_err(err, "scan layer_path"))?;
                for entry in range {
                    let (k, v) =
                        entry.map_err(|err| map_redb_storage_err(err, "read layer_path"))?;
                    if k.value().0 != wt {
                        break;
                    }
                    let rec: LayerPathRecord =
                        decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                    if rec.layer_name == name_owned {
                        to_remove.push(k.value().1.to_string());
                    }
                }
            }
            let removed_paths = !to_remove.is_empty();
            for p in to_remove {
                let _ = lp_table
                    .remove((wt.as_str(), p.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "remove layer_path"))?;
            }
            drop(lp_table);

            let mut layer_table = wtx
                .open_table(LAYER_TABLE)
                .map_err(|err| map_redb_table_err(err, "open layer"))?;
            let removed_layer = layer_table
                .remove((wt.as_str(), name_owned.as_str()))
                .map_err(|err| map_redb_storage_err(err, "delete layer"))?
                .is_some();
            Ok((removed_layer || removed_paths, removed_layer))
        })
        .await
    }

    async fn upsert_layer_path(&self, record: &LayerPathRecord) -> StorageResult<()> {
        validate_layer_path_record(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(LAYER_PATH_TABLE)
                .map_err(|err| map_redb_table_err(err, "open layer_path"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.worktree_id.as_str(), rec.path.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert layer_path"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_layer_paths(
        &self,
        worktree_id: &str,
        layer_name: Option<&str>,
    ) -> StorageResult<Vec<LayerPathRecord>> {
        validate_no_nul(worktree_id, "layer_path worktree_id")?;
        if let Some(name) = layer_name {
            validate_non_empty_identifier(name, "layer_path layer_name")?;
        }
        let wt = worktree_id.to_string();
        let name_filter = layer_name.map(str::to_string);
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, LAYER_PATH_TABLE, "open layer_path")? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((wt.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan layer_path"))?;
            for entry in range {
                let (k, v) = entry.map_err(|err| map_redb_storage_err(err, "read layer_path"))?;
                if k.value().0 != wt {
                    break;
                }
                let rec: LayerPathRecord = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if let Some(expected) = name_filter.as_deref()
                    && rec.layer_name != expected
                {
                    continue;
                }
                out.push(rec);
            }
            Ok(out)
        })
        .await
    }

    async fn replace_sparse_view(
        &self,
        worktree_id: &str,
        enabled: bool,
        patterns: &[String],
    ) -> StorageResult<()> {
        validate_no_nul(worktree_id, "sparse_view worktree_id")?;
        for pat in patterns {
            validate_non_empty_identifier(pat, "sparse_view pattern")?;
        }
        let wt = worktree_id.to_string();
        let pats = patterns.to_vec();

        self.run_write_conditional(move |wtx| {
            let mut sv_table = wtx
                .open_table(SPARSE_VIEW_TABLE)
                .map_err(|err| map_redb_table_err(err, "open sparse_view"))?;
            let mut old_ords = Vec::new();
            {
                let range = sv_table
                    .range((wt.as_str(), i64::MIN)..=(wt.as_str(), i64::MAX))
                    .map_err(|err| map_redb_storage_err(err, "scan old sparse_view"))?;
                for entry in range {
                    let (k, _) =
                        entry.map_err(|err| map_redb_storage_err(err, "read old sparse_view"))?;
                    old_ords.push(k.value().1);
                }
            }
            for ord in old_ords {
                let _ = sv_table
                    .remove((wt.as_str(), ord))
                    .map_err(|err| map_redb_storage_err(err, "remove old sparse_view"))?;
            }
            for (ordinal, pat) in pats.iter().enumerate() {
                let ord_i64 = i64::try_from(ordinal).unwrap_or(i64::MAX);
                let bytes =
                    encode_versioned(GIT_STATE_CODEC_VERSION, &StringValue { value: pat.clone() })?;
                sv_table
                    .insert((wt.as_str(), ord_i64), bytes.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert sparse_view"))?;
            }
            drop(sv_table);

            let mut meta_table = wtx
                .open_table(SPARSE_VIEW_META_TABLE)
                .map_err(|err| map_redb_table_err(err, "open sparse_view_meta"))?;
            let meta_bytes =
                encode_versioned(GIT_STATE_CODEC_VERSION, &BoolValue { value: enabled })?;
            meta_table
                .insert(wt.as_str(), meta_bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert sparse_view_meta"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_sparse_view(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<SparseViewSnapshot>> {
        validate_no_nul(worktree_id, "sparse_view worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_read(move |rtx| {
            let Some(meta_table) =
                open_opt_table(rtx, SPARSE_VIEW_META_TABLE, "open sparse_view_meta")?
            else {
                return Ok(None);
            };
            let Some(g) = meta_table
                .get(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "get sparse_view_meta"))?
            else {
                return Ok(None);
            };
            let enabled: BoolValue = decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?;
            let mut patterns = Vec::new();
            if let Some(sv_table) = open_opt_table(rtx, SPARSE_VIEW_TABLE, "open sparse_view")? {
                let range = sv_table
                    .range((wt.as_str(), i64::MIN)..=(wt.as_str(), i64::MAX))
                    .map_err(|err| map_redb_storage_err(err, "scan sparse_view"))?;
                for entry in range {
                    let (_, v) =
                        entry.map_err(|err| map_redb_storage_err(err, "read sparse_view"))?;
                    let sv: StringValue = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                    patterns.push(sv.value);
                }
            }
            Ok(Some(SparseViewSnapshot {
                worktree_id: wt,
                enabled: enabled.value,
                patterns,
            }))
        })
        .await
    }

    async fn clear_sparse_view(&self, worktree_id: &str) -> StorageResult<()> {
        validate_no_nul(worktree_id, "sparse_view worktree_id")?;
        let wt = worktree_id.to_string();
        self.run_write_conditional(move |wtx| {
            let mut sv_table = wtx
                .open_table(SPARSE_VIEW_TABLE)
                .map_err(|err| map_redb_table_err(err, "open sparse_view"))?;
            let mut old_ords = Vec::new();
            {
                let range = sv_table
                    .range((wt.as_str(), i64::MIN)..=(wt.as_str(), i64::MAX))
                    .map_err(|err| map_redb_storage_err(err, "scan sparse_view for clear"))?;
                for entry in range {
                    let (k, _) = entry
                        .map_err(|err| map_redb_storage_err(err, "read sparse_view for clear"))?;
                    old_ords.push(k.value().1);
                }
            }
            let had_rows = !old_ords.is_empty();
            for ord in old_ords {
                let _ = sv_table
                    .remove((wt.as_str(), ord))
                    .map_err(|err| map_redb_storage_err(err, "remove sparse_view"))?;
            }
            drop(sv_table);

            let mut meta_table = wtx
                .open_table(SPARSE_VIEW_META_TABLE)
                .map_err(|err| map_redb_table_err(err, "open sparse_view_meta"))?;
            let had_meta = meta_table
                .remove(wt.as_str())
                .map_err(|err| map_redb_storage_err(err, "remove sparse_view_meta"))?
                .is_some();
            Ok((had_rows || had_meta, ()))
        })
        .await
    }

    // -------------------------------------------------------------------------
    // RB-05b AC1: 19 `ai_*` Tables
    // -------------------------------------------------------------------------

    async fn upsert_ai_thread(&self, record: &AiThreadRecord) -> StorageResult<()> {
        validate_ai_thread(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_THREAD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_thread"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.thread_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_thread"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_ai_thread(&self, thread_id: &str) -> StorageResult<Option<AiThreadRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, AI_THREAD_TABLE, "open ai_thread")? else {
                return Ok(None);
            };
            let Some(g) = table
                .get(tid.as_str())
                .map_err(|err| map_redb_storage_err(err, "get ai_thread"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn list_ai_threads(
        &self,
        archived_filter: Option<bool>,
    ) -> StorageResult<Vec<AiThreadRecord>> {
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, AI_THREAD_TABLE, "open ai_thread")? else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter ai_thread"))?
            {
                let (_, v) = entry.map_err(|err| map_redb_storage_err(err, "read ai_thread"))?;
                let rec: AiThreadRecord = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if archived_filter.is_none_or(|f| rec.archived == f) {
                    out.push(rec);
                }
            }
            out.sort_by(|a, b| {
                b.updated_at
                    .cmp(&a.updated_at)
                    .then_with(|| a.thread_id.cmp(&b.thread_id))
            });
            Ok(out)
        })
        .await
    }

    async fn delete_ai_thread(&self, thread_id: &str) -> StorageResult<bool> {
        validate_non_empty_identifier(thread_id, "ai_thread thread_id")?;
        let tid = thread_id.to_string();
        self.run_write_conditional(move |wtx| {
            // Cascade delete all 11 child tables referencing `ai_thread(thread_id)`.
            {
                let mut t = wtx
                    .open_table(AI_THREAD_PARTICIPANT_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open ai_thread_participant"))?;
                let keys: Vec<(String, String)> = {
                    let mut acc = Vec::new();
                    let range = t
                        .range((tid.as_str(), "", "")..)
                        .map_err(|err| map_redb_storage_err(err, "scan ai_thread_participant"))?;
                    for entry in range {
                        let (k, _) = entry.map_err(|err| {
                            map_redb_storage_err(err, "read ai_thread_participant")
                        })?;
                        let (t_id, ak, aid) = k.value();
                        if t_id != tid {
                            break;
                        }
                        acc.push((ak.to_string(), aid.to_string()));
                    }
                    acc
                };
                for (ak, aid) in keys {
                    let _ = t
                        .remove((tid.as_str(), ak.as_str(), aid.as_str()))
                        .map_err(|err| map_redb_storage_err(err, "delete ai_thread_participant"))?;
                }
            }
            delete_str_pair_prefix(wtx, AI_THREAD_INTENT_TABLE, &tid, "ai_thread_intent")?;
            {
                let mut t = wtx
                    .open_table(AI_THREAD_PROVIDER_METADATA_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open ai_thread_provider_metadata"))?;
                let _ = t.remove(tid.as_str()).map_err(|err| {
                    map_redb_storage_err(err, "delete ai_thread_provider_metadata")
                })?;
            }
            {
                let mut t = wtx
                    .open_table(AI_SCHEDULER_STATE_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open ai_scheduler_state"))?;
                let _ = t
                    .remove(tid.as_str())
                    .map_err(|err| map_redb_storage_err(err, "delete ai_scheduler_state"))?;
            }
            delete_str_pair_prefix(
                wtx,
                AI_SCHEDULER_PLAN_HEAD_TABLE,
                &tid,
                "ai_scheduler_plan_head",
            )?;
            delete_str_pair_prefix(
                wtx,
                AI_SCHEDULER_SELECTED_PLAN_TABLE,
                &tid,
                "ai_scheduler_selected_plan",
            )?;
            delete_str_pair_prefix(
                wtx,
                AI_LIVE_CONTEXT_WINDOW_TABLE,
                &tid,
                "ai_live_context_window",
            )?;
            delete_by_thread_filter::<AiValidationReportRecord>(
                wtx,
                AI_VALIDATION_REPORT_TABLE,
                &tid,
                |r| r.thread_id.as_str(),
                "ai_validation_report",
            )?;
            delete_by_thread_filter::<AiRiskScoreBreakdownRecord>(
                wtx,
                AI_RISK_SCORE_BREAKDOWN_TABLE,
                &tid,
                |r| r.thread_id.as_str(),
                "ai_risk_score_breakdown",
            )?;
            delete_by_thread_filter::<AiDecisionProposalRecord>(
                wtx,
                AI_DECISION_PROPOSAL_TABLE,
                &tid,
                |r| r.thread_id.as_str(),
                "ai_decision_proposal",
            )?;
            delete_by_thread_filter::<AiFinalDecisionRecord>(
                wtx,
                AI_FINAL_DECISION_TABLE,
                &tid,
                |r| r.thread_id.as_str(),
                "ai_final_decision",
            )?;
            let mut thread_table = wtx
                .open_table(AI_THREAD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_thread"))?;
            let removed = thread_table
                .remove(tid.as_str())
                .map_err(|err| map_redb_storage_err(err, "delete ai_thread"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn upsert_ai_thread_participant(
        &self,
        record: &AiThreadParticipantRecord,
    ) -> StorageResult<()> {
        validate_ai_thread_participant(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_THREAD_PARTICIPANT_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_thread_participant"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (
                        rec.thread_id.as_str(),
                        rec.actor_kind.as_str(),
                        rec.actor_id.as_str(),
                    ),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_thread_participant"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_thread_participants(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiThreadParticipantRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread_participant thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_THREAD_PARTICIPANT_TABLE,
                "open ai_thread_participant",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((tid.as_str(), "", "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_thread_participant"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_thread_participant"))?;
                if k.value().0 != tid {
                    break;
                }
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_thread_intent(&self, record: &AiThreadIntentRecord) -> StorageResult<()> {
        validate_ai_thread_intent(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_THREAD_INTENT_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_thread_intent"))?;
            // Enforce SQLite's `uq_ai_thread_intent_intent (intent_id)` and
            // `idx_ai_thread_intent_thread_ordinal (thread_id, ordinal)`.
            {
                let iter = table
                    .iter()
                    .map_err(|err| map_redb_storage_err(err, "scan ai_thread_intent"))?;
                for entry in iter {
                    let (_, v) =
                        entry.map_err(|err| map_redb_storage_err(err, "read ai_thread_intent"))?;
                    let other: AiThreadIntentRecord =
                        decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                    if other.intent_id == rec.intent_id && other.thread_id != rec.thread_id {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "ai_thread_intent intent_id already linked to another thread"
                                .to_string(),
                        });
                    }
                    if other.thread_id == rec.thread_id
                        && other.ordinal == rec.ordinal
                        && other.intent_id != rec.intent_id
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "duplicate (thread_id, ordinal) in ai_thread_intent"
                                .to_string(),
                        });
                    }
                }
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.thread_id.as_str(), rec.intent_id.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_thread_intent"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_thread_intents(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiThreadIntentRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread_intent thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, AI_THREAD_INTENT_TABLE, "open ai_thread_intent")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((tid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_thread_intent"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_thread_intent"))?;
                if k.value().0 != tid {
                    break;
                }
                let rec: AiThreadIntentRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.ordinal
                    .cmp(&b.ordinal)
                    .then_with(|| a.intent_id.cmp(&b.intent_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_thread_provider_metadata(
        &self,
        record: &AiThreadProviderMetadataRecord,
    ) -> StorageResult<()> {
        validate_ai_thread_provider_metadata(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_THREAD_PROVIDER_METADATA_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_thread_provider_metadata"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.thread_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_thread_provider_metadata"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_ai_thread_provider_metadata(
        &self,
        thread_id: &str,
    ) -> StorageResult<Option<AiThreadProviderMetadataRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread_provider_metadata thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_THREAD_PROVIDER_METADATA_TABLE,
                "open ai_thread_provider_metadata",
            )?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(tid.as_str())
                .map_err(|err| map_redb_storage_err(err, "get ai_thread_provider_metadata"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn upsert_ai_scheduler_state(
        &self,
        record: &AiSchedulerStateRecord,
    ) -> StorageResult<()> {
        validate_ai_scheduler_state(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_SCHEDULER_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_scheduler_state"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.thread_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_scheduler_state"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_ai_scheduler_state(
        &self,
        thread_id: &str,
    ) -> StorageResult<Option<AiSchedulerStateRecord>> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_state thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_SCHEDULER_STATE_TABLE, "open ai_scheduler_state")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(tid.as_str())
                .map_err(|err| map_redb_storage_err(err, "get ai_scheduler_state"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn replace_ai_scheduler_plan_heads(
        &self,
        thread_id: &str,
        heads: &[AiSchedulerPlanHeadRecord],
    ) -> StorageResult<()> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_plan_head thread_id")?;
        let mut seen_plans = BTreeSet::new();
        let mut seen_ords = BTreeSet::new();
        for h in heads {
            validate_ai_scheduler_plan_head(h)?;
            if h.thread_id != thread_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "ai_scheduler_plan_head thread_id mismatch".to_string(),
                });
            }
            if !seen_plans.insert(h.plan_id.as_str()) || !seen_ords.insert(h.ordinal) {
                return Err(StorageBackendError::BackendInternal {
                    detail: "duplicate plan_id or ordinal in ai_scheduler_plan_head".to_string(),
                });
            }
        }
        let tid = thread_id.to_string();
        let owned = heads.to_vec();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &tid)?;
            delete_str_pair_prefix(
                wtx,
                AI_SCHEDULER_PLAN_HEAD_TABLE,
                &tid,
                "ai_scheduler_plan_head",
            )?;
            let mut table = wtx
                .open_table(AI_SCHEDULER_PLAN_HEAD_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_scheduler_plan_head"))?;
            for h in &owned {
                let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, h)?;
                table
                    .insert((h.thread_id.as_str(), h.plan_id.as_str()), bytes.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert ai_scheduler_plan_head"))?;
            }
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_scheduler_plan_heads(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiSchedulerPlanHeadRecord>> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_plan_head thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_SCHEDULER_PLAN_HEAD_TABLE,
                "open ai_scheduler_plan_head",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((tid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_scheduler_plan_head"))?;
            for entry in range {
                let (k, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read ai_scheduler_plan_head"))?;
                if k.value().0 != tid {
                    break;
                }
                let rec: AiSchedulerPlanHeadRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.ordinal
                    .cmp(&b.ordinal)
                    .then_with(|| a.plan_id.cmp(&b.plan_id))
            });
            Ok(out)
        })
        .await
    }

    async fn replace_ai_scheduler_selected_plans(
        &self,
        thread_id: &str,
        plans: &[AiSchedulerSelectedPlanRecord],
    ) -> StorageResult<()> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_selected_plan thread_id")?;
        let mut seen_plans = BTreeSet::new();
        let mut seen_ords = BTreeSet::new();
        for p in plans {
            validate_ai_scheduler_selected_plan(p)?;
            if p.thread_id != thread_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "ai_scheduler_selected_plan thread_id mismatch".to_string(),
                });
            }
            if !seen_plans.insert(p.plan_id.as_str()) || !seen_ords.insert(p.ordinal) {
                return Err(StorageBackendError::BackendInternal {
                    detail: "duplicate plan_id or ordinal in ai_scheduler_selected_plan"
                        .to_string(),
                });
            }
        }
        let tid = thread_id.to_string();
        let owned = plans.to_vec();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &tid)?;
            delete_str_pair_prefix(
                wtx,
                AI_SCHEDULER_SELECTED_PLAN_TABLE,
                &tid,
                "ai_scheduler_selected_plan",
            )?;
            let mut table = wtx
                .open_table(AI_SCHEDULER_SELECTED_PLAN_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_scheduler_selected_plan"))?;
            for p in &owned {
                let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, p)?;
                table
                    .insert((p.thread_id.as_str(), p.plan_id.as_str()), bytes.as_slice())
                    .map_err(|err| {
                        map_redb_storage_err(err, "insert ai_scheduler_selected_plan")
                    })?;
            }
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_scheduler_selected_plans(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiSchedulerSelectedPlanRecord>> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_selected_plan thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_SCHEDULER_SELECTED_PLAN_TABLE,
                "open ai_scheduler_selected_plan",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((tid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_scheduler_selected_plan"))?;
            for entry in range {
                let (k, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read ai_scheduler_selected_plan"))?;
                if k.value().0 != tid {
                    break;
                }
                let rec: AiSchedulerSelectedPlanRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.ordinal
                    .cmp(&b.ordinal)
                    .then_with(|| a.plan_id.cmp(&b.plan_id))
            });
            Ok(out)
        })
        .await
    }

    async fn replace_ai_live_context_window(
        &self,
        thread_id: &str,
        entries: &[AiLiveContextWindowRecord],
    ) -> StorageResult<()> {
        validate_non_empty_identifier(thread_id, "ai_live_context_window thread_id")?;
        let mut seen_frames = BTreeSet::new();
        let mut seen_pos = BTreeSet::new();
        for e in entries {
            validate_ai_live_context_window(e)?;
            if e.thread_id != thread_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "ai_live_context_window thread_id mismatch".to_string(),
                });
            }
            if !seen_frames.insert(e.context_frame_id.as_str()) || !seen_pos.insert(e.position) {
                return Err(StorageBackendError::BackendInternal {
                    detail: "duplicate context_frame_id or position in ai_live_context_window"
                        .to_string(),
                });
            }
        }
        let tid = thread_id.to_string();
        let owned = entries.to_vec();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &tid)?;
            delete_str_pair_prefix(
                wtx,
                AI_LIVE_CONTEXT_WINDOW_TABLE,
                &tid,
                "ai_live_context_window",
            )?;
            let mut table = wtx
                .open_table(AI_LIVE_CONTEXT_WINDOW_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_live_context_window"))?;
            for e in &owned {
                let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, e)?;
                table
                    .insert(
                        (e.thread_id.as_str(), e.context_frame_id.as_str()),
                        bytes.as_slice(),
                    )
                    .map_err(|err| map_redb_storage_err(err, "insert ai_live_context_window"))?;
            }
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_live_context_window(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiLiveContextWindowRecord>> {
        validate_non_empty_identifier(thread_id, "ai_live_context_window thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_LIVE_CONTEXT_WINDOW_TABLE,
                "open ai_live_context_window",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((tid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_live_context_window"))?;
            for entry in range {
                let (k, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read ai_live_context_window"))?;
                if k.value().0 != tid {
                    break;
                }
                let rec: AiLiveContextWindowRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then_with(|| a.context_frame_id.cmp(&b.context_frame_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_index_intent_plan(
        &self,
        record: &AiIndexIntentPlanRecord,
    ) -> StorageResult<()> {
        validate_ai_index_intent_plan(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_INDEX_INTENT_PLAN_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_index_intent_plan"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.intent_id.as_str(), rec.plan_id.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_index_intent_plan"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_index_intent_plans(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentPlanRecord>> {
        validate_non_empty_identifier(intent_id, "ai_index_intent_plan intent_id")?;
        let iid = intent_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_INDEX_INTENT_PLAN_TABLE, "open ai_index_intent_plan")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((iid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_index_intent_plan"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_index_intent_plan"))?;
                if k.value().0 != iid {
                    break;
                }
                let rec: AiIndexIntentPlanRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.plan_id.cmp(&b.plan_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_index_intent_task(
        &self,
        record: &AiIndexIntentTaskRecord,
    ) -> StorageResult<()> {
        validate_ai_index_intent_task(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_INDEX_INTENT_TASK_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_index_intent_task"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.intent_id.as_str(), rec.task_id.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_index_intent_task"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_index_intent_tasks(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentTaskRecord>> {
        validate_non_empty_identifier(intent_id, "ai_index_intent_task intent_id")?;
        let iid = intent_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_INDEX_INTENT_TASK_TABLE, "open ai_index_intent_task")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((iid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_index_intent_task"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_index_intent_task"))?;
                if k.value().0 != iid {
                    break;
                }
                let rec: AiIndexIntentTaskRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.task_id.cmp(&b.task_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_index_plan_step_task(
        &self,
        record: &AiIndexPlanStepTaskRecord,
    ) -> StorageResult<()> {
        validate_ai_index_plan_step_task(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_INDEX_PLAN_STEP_TASK_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_index_plan_step_task"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.plan_id.as_str(), rec.task_id.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_index_plan_step_task"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_index_plan_step_tasks(
        &self,
        plan_id: &str,
    ) -> StorageResult<Vec<AiIndexPlanStepTaskRecord>> {
        validate_non_empty_identifier(plan_id, "ai_index_plan_step_task plan_id")?;
        let pid = plan_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_INDEX_PLAN_STEP_TASK_TABLE,
                "open ai_index_plan_step_task",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((pid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_index_plan_step_task"))?;
            for entry in range {
                let (k, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read ai_index_plan_step_task"))?;
                if k.value().0 != pid {
                    break;
                }
                let rec: AiIndexPlanStepTaskRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.step_id
                    .cmp(&b.step_id)
                    .then_with(|| a.created_at.cmp(&b.created_at))
                    .then_with(|| a.task_id.cmp(&b.task_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_index_task_run(&self, record: &AiIndexTaskRunRecord) -> StorageResult<()> {
        validate_ai_index_task_run(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_INDEX_TASK_RUN_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_index_task_run"))?;
            if rec.is_latest {
                let mut to_clear: Vec<AiIndexTaskRunRecord> = Vec::new();
                {
                    let range = table
                        .range((rec.task_id.as_str(), "")..)
                        .map_err(|err| map_redb_storage_err(err, "scan ai_index_task_run"))?;
                    for entry in range {
                        let (k, v) = entry
                            .map_err(|err| map_redb_storage_err(err, "read ai_index_task_run"))?;
                        if k.value().0 != rec.task_id {
                            break;
                        }
                        let mut existing: AiIndexTaskRunRecord =
                            decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        if existing.is_latest && existing.run_id != rec.run_id {
                            existing.is_latest = false;
                            to_clear.push(existing);
                        }
                    }
                }
                for row in to_clear {
                    let b = encode_versioned(GIT_STATE_CODEC_VERSION, &row)?;
                    table
                        .insert((row.task_id.as_str(), row.run_id.as_str()), b.as_slice())
                        .map_err(|err| {
                            map_redb_storage_err(err, "clear previous latest ai_index_task_run")
                        })?;
                }
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.task_id.as_str(), rec.run_id.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_index_task_run"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_index_task_runs(
        &self,
        task_id: &str,
    ) -> StorageResult<Vec<AiIndexTaskRunRecord>> {
        validate_non_empty_identifier(task_id, "ai_index_task_run task_id")?;
        let tid = task_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_INDEX_TASK_RUN_TABLE, "open ai_index_task_run")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((tid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_index_task_run"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_index_task_run"))?;
                if k.value().0 != tid {
                    break;
                }
                let rec: AiIndexTaskRunRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.run_id.cmp(&b.run_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_index_run_event(&self, record: &AiIndexRunEventRecord) -> StorageResult<()> {
        validate_ai_index_run_event(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_INDEX_RUN_EVENT_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_index_run_event"))?;
            if rec.is_latest {
                let mut to_clear: Vec<AiIndexRunEventRecord> = Vec::new();
                {
                    let range = table
                        .range((rec.run_id.as_str(), "")..)
                        .map_err(|err| map_redb_storage_err(err, "scan ai_index_run_event"))?;
                    for entry in range {
                        let (k, v) = entry
                            .map_err(|err| map_redb_storage_err(err, "read ai_index_run_event"))?;
                        if k.value().0 != rec.run_id {
                            break;
                        }
                        let mut existing: AiIndexRunEventRecord =
                            decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        if existing.is_latest && existing.event_id != rec.event_id {
                            existing.is_latest = false;
                            to_clear.push(existing);
                        }
                    }
                }
                for row in to_clear {
                    let b = encode_versioned(GIT_STATE_CODEC_VERSION, &row)?;
                    table
                        .insert((row.run_id.as_str(), row.event_id.as_str()), b.as_slice())
                        .map_err(|err| {
                            map_redb_storage_err(err, "clear previous latest ai_index_run_event")
                        })?;
                }
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.run_id.as_str(), rec.event_id.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_index_run_event"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_index_run_events(
        &self,
        run_id: &str,
    ) -> StorageResult<Vec<AiIndexRunEventRecord>> {
        validate_non_empty_identifier(run_id, "ai_index_run_event run_id")?;
        let rid = run_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_INDEX_RUN_EVENT_TABLE, "open ai_index_run_event")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((rid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_index_run_event"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_index_run_event"))?;
                if k.value().0 != rid {
                    break;
                }
                let rec: AiIndexRunEventRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.event_id.cmp(&b.event_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_index_run_patchset(
        &self,
        record: &AiIndexRunPatchsetRecord,
    ) -> StorageResult<()> {
        validate_ai_index_run_patchset(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_INDEX_RUN_PATCHSET_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_index_run_patchset"))?;
            if rec.is_latest {
                let mut to_clear: Vec<AiIndexRunPatchsetRecord> = Vec::new();
                {
                    let range = table
                        .range((rec.run_id.as_str(), "")..)
                        .map_err(|err| map_redb_storage_err(err, "scan ai_index_run_patchset"))?;
                    for entry in range {
                        let (k, v) = entry.map_err(|err| {
                            map_redb_storage_err(err, "read ai_index_run_patchset")
                        })?;
                        if k.value().0 != rec.run_id {
                            break;
                        }
                        let mut existing: AiIndexRunPatchsetRecord =
                            decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        if existing.is_latest && existing.patchset_id != rec.patchset_id {
                            existing.is_latest = false;
                            to_clear.push(existing);
                        }
                    }
                }
                for row in to_clear {
                    let b = encode_versioned(GIT_STATE_CODEC_VERSION, &row)?;
                    table
                        .insert(
                            (row.run_id.as_str(), row.patchset_id.as_str()),
                            b.as_slice(),
                        )
                        .map_err(|err| {
                            map_redb_storage_err(err, "clear previous latest ai_index_run_patchset")
                        })?;
                }
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.run_id.as_str(), rec.patchset_id.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_index_run_patchset"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_index_run_patchsets(
        &self,
        run_id: &str,
    ) -> StorageResult<Vec<AiIndexRunPatchsetRecord>> {
        validate_non_empty_identifier(run_id, "ai_index_run_patchset run_id")?;
        let rid = run_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_INDEX_RUN_PATCHSET_TABLE,
                "open ai_index_run_patchset",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((rid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_index_run_patchset"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_index_run_patchset"))?;
                if k.value().0 != rid {
                    break;
                }
                let rec: AiIndexRunPatchsetRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.sequence
                    .cmp(&b.sequence)
                    .then_with(|| a.patchset_id.cmp(&b.patchset_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_index_intent_context_frame(
        &self,
        record: &AiIndexIntentContextFrameRecord,
    ) -> StorageResult<()> {
        validate_ai_index_intent_context_frame(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AI_INDEX_INTENT_CONTEXT_FRAME_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_index_intent_context_frame"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (
                        rec.intent_id.as_str(),
                        rec.context_frame_id.as_str(),
                        rec.relation_kind.as_str(),
                    ),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert ai_index_intent_context_frame"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_index_intent_context_frames(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentContextFrameRecord>> {
        validate_non_empty_identifier(intent_id, "ai_index_intent_context_frame intent_id")?;
        let iid = intent_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_INDEX_INTENT_CONTEXT_FRAME_TABLE,
                "open ai_index_intent_context_frame",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((iid.as_str(), "", "")..)
                .map_err(|err| map_redb_storage_err(err, "scan ai_index_intent_context_frame"))?;
            for entry in range {
                let (k, v) = entry.map_err(|err| {
                    map_redb_storage_err(err, "read ai_index_intent_context_frame")
                })?;
                if k.value().0 != iid {
                    break;
                }
                let rec: AiIndexIntentContextFrameRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.relation_kind
                    .cmp(&b.relation_kind)
                    .then_with(|| a.created_at.cmp(&b.created_at))
                    .then_with(|| a.context_frame_id.cmp(&b.context_frame_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_validation_report(
        &self,
        record: &AiValidationReportRecord,
    ) -> StorageResult<()> {
        validate_ai_validation_report(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_VALIDATION_REPORT_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_validation_report"))?;
            if rec.is_latest {
                clear_latest_for_thread::<AiValidationReportRecord>(
                    &mut table,
                    &rec.thread_id,
                    &rec.report_id,
                    |r| (&r.thread_id, &r.report_id, r.is_latest),
                    |r| r.is_latest = false,
                    "ai_validation_report",
                )?;
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.report_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_validation_report"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_validation_reports(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiValidationReportRecord>> {
        validate_non_empty_identifier(thread_id, "ai_validation_report thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_VALIDATION_REPORT_TABLE, "open ai_validation_report")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter ai_validation_report"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_validation_report"))?;
                let rec: AiValidationReportRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.thread_id == tid {
                    out.push(rec);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.report_id.cmp(&b.report_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_risk_score_breakdown(
        &self,
        record: &AiRiskScoreBreakdownRecord,
    ) -> StorageResult<()> {
        validate_ai_risk_score_breakdown(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_RISK_SCORE_BREAKDOWN_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_risk_score_breakdown"))?;
            if rec.is_latest {
                clear_latest_for_thread::<AiRiskScoreBreakdownRecord>(
                    &mut table,
                    &rec.thread_id,
                    &rec.breakdown_id,
                    |r| (&r.thread_id, &r.breakdown_id, r.is_latest),
                    |r| r.is_latest = false,
                    "ai_risk_score_breakdown",
                )?;
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.breakdown_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_risk_score_breakdown"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_risk_score_breakdowns(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiRiskScoreBreakdownRecord>> {
        validate_non_empty_identifier(thread_id, "ai_risk_score_breakdown thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AI_RISK_SCORE_BREAKDOWN_TABLE,
                "open ai_risk_score_breakdown",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter ai_risk_score_breakdown"))?
            {
                let (_, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read ai_risk_score_breakdown"))?;
                let rec: AiRiskScoreBreakdownRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.thread_id == tid {
                    out.push(rec);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.breakdown_id.cmp(&b.breakdown_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_decision_proposal(
        &self,
        record: &AiDecisionProposalRecord,
    ) -> StorageResult<()> {
        validate_ai_decision_proposal(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_DECISION_PROPOSAL_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_decision_proposal"))?;
            if rec.is_latest {
                clear_latest_for_thread::<AiDecisionProposalRecord>(
                    &mut table,
                    &rec.thread_id,
                    &rec.proposal_id,
                    |r| (&r.thread_id, &r.proposal_id, r.is_latest),
                    |r| r.is_latest = false,
                    "ai_decision_proposal",
                )?;
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.proposal_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_decision_proposal"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_decision_proposals(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiDecisionProposalRecord>> {
        validate_non_empty_identifier(thread_id, "ai_decision_proposal thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_DECISION_PROPOSAL_TABLE, "open ai_decision_proposal")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter ai_decision_proposal"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_decision_proposal"))?;
                let rec: AiDecisionProposalRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.thread_id == tid {
                    out.push(rec);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.proposal_id.cmp(&b.proposal_id))
            });
            Ok(out)
        })
        .await
    }

    async fn upsert_ai_final_decision(&self, record: &AiFinalDecisionRecord) -> StorageResult<()> {
        validate_ai_final_decision(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_ai_thread_exists_in_wtx(wtx, &rec.thread_id)?;
            let mut table = wtx
                .open_table(AI_FINAL_DECISION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open ai_final_decision"))?;
            if rec.is_latest {
                clear_latest_for_thread::<AiFinalDecisionRecord>(
                    &mut table,
                    &rec.thread_id,
                    &rec.decision_id,
                    |r| (&r.thread_id, &r.decision_id, r.is_latest),
                    |r| r.is_latest = false,
                    "ai_final_decision",
                )?;
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.decision_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert ai_final_decision"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_ai_final_decisions(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiFinalDecisionRecord>> {
        validate_non_empty_identifier(thread_id, "ai_final_decision thread_id")?;
        let tid = thread_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, AI_FINAL_DECISION_TABLE, "open ai_final_decision")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter ai_final_decision"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read ai_final_decision"))?;
                let rec: AiFinalDecisionRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.thread_id == tid {
                    out.push(rec);
                }
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.decision_id.cmp(&b.decision_id))
            });
            Ok(out)
        })
        .await
    }

    // -------------------------------------------------------------------------
    // RB-05b AC2 & AC3: 5 `memory_*` Tables + `MEMORY_BY_PATH` Secondary Index
    // -------------------------------------------------------------------------

    async fn upsert_memory_episode(&self, record: &MemoryEpisodeRecord) -> StorageResult<()> {
        validate_memory_episode(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(MEMORY_EPISODE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open memory_episode"))?;
            // Enforce SQLite's `UNIQUE(repo_id, source_kind, source_key)`.
            {
                let iter = table
                    .iter()
                    .map_err(|err| map_redb_storage_err(err, "scan memory_episode"))?;
                for entry in iter {
                    let (_, v) =
                        entry.map_err(|err| map_redb_storage_err(err, "read memory_episode"))?;
                    let existing: MemoryEpisodeRecord =
                        decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                    if existing.episode_id != rec.episode_id
                        && existing.repo_id == rec.repo_id
                        && existing.source_kind == rec.source_kind
                        && existing.source_key == rec.source_key
                    {
                        return Err(StorageBackendError::BackendInternal {
                            detail:
                                "duplicate (repo_id, source_kind, source_key) in memory_episode"
                                    .to_string(),
                        });
                    }
                }
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.episode_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert memory_episode"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_memory_episode(
        &self,
        episode_id: &str,
    ) -> StorageResult<Option<MemoryEpisodeRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode episode_id")?;
        let eid = episode_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, MEMORY_EPISODE_TABLE, "open memory_episode")?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(eid.as_str())
                .map_err(|err| map_redb_storage_err(err, "get memory_episode"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn list_memory_episodes(&self, repo_id: &str) -> StorageResult<Vec<MemoryEpisodeRecord>> {
        validate_non_empty_identifier(repo_id, "memory_episode repo_id")?;
        let rid = repo_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, MEMORY_EPISODE_TABLE, "open memory_episode")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter memory_episode"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read memory_episode"))?;
                let rec: MemoryEpisodeRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.repo_id == rid {
                    out.push(rec);
                }
            }
            out.sort_by(|a, b| {
                b.ended_at
                    .cmp(&a.ended_at)
                    .then_with(|| a.episode_id.cmp(&b.episode_id))
            });
            Ok(out)
        })
        .await
    }

    async fn delete_memory_episode(&self, episode_id: &str) -> StorageResult<bool> {
        validate_non_empty_identifier(episode_id, "memory_episode episode_id")?;
        let eid = episode_id.to_string();
        self.run_write_conditional(move |wtx| {
            // Cascade delete `memory_episode_evidence`.
            {
                let mut ev_table = wtx
                    .open_table(MEMORY_EPISODE_EVIDENCE_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open memory_episode_evidence"))?;
                let mut ords = Vec::new();
                {
                    let range = ev_table
                        .range((eid.as_str(), i64::MIN)..=(eid.as_str(), i64::MAX))
                        .map_err(|err| map_redb_storage_err(err, "scan memory_episode_evidence"))?;
                    for entry in range {
                        let (k, _) = entry.map_err(|err| {
                            map_redb_storage_err(err, "read memory_episode_evidence")
                        })?;
                        ords.push(k.value().1);
                    }
                }
                for ord in ords {
                    let _ = ev_table.remove((eid.as_str(), ord)).map_err(|err| {
                        map_redb_storage_err(err, "delete memory_episode_evidence")
                    })?;
                }
            }

            // Cascade delete `memory_episode_path` AND same-tx `MEMORY_BY_PATH` index entries.
            {
                let mut path_table = wtx
                    .open_table(MEMORY_EPISODE_PATH_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open memory_episode_path"))?;
                let mut to_remove: Vec<(Vec<u8>, u64)> = Vec::new();
                {
                    let range = path_table
                        .range((eid.as_str(), &[][..])..)
                        .map_err(|err| map_redb_storage_err(err, "scan memory_episode_path"))?;
                    for entry in range {
                        let (k, v) = entry
                            .map_err(|err| map_redb_storage_err(err, "read memory_episode_path"))?;
                        if k.value().0 != eid {
                            break;
                        }
                        let rec: MemoryEpisodePathRecord =
                            decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        to_remove.push((rec.code_path, invert_ended_at(rec.ended_at)));
                    }
                }
                let mut idx_table = wtx
                    .open_table(MEMORY_BY_PATH_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open MEMORY_BY_PATH"))?;
                for (code_path, inv_ts) in to_remove {
                    let _ = path_table
                        .remove((eid.as_str(), code_path.as_slice()))
                        .map_err(|err| map_redb_storage_err(err, "delete memory_episode_path"))?;
                    let _ = idx_table
                        .remove((code_path.as_slice(), inv_ts, eid.as_str()))
                        .map_err(|err| map_redb_storage_err(err, "delete MEMORY_BY_PATH"))?;
                }
            }

            // Cascade delete `memory_episode_search_doc`.
            {
                let mut doc_table = wtx
                    .open_table(MEMORY_EPISODE_SEARCH_DOC_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open memory_episode_search_doc"))?;
                let mut to_remove = Vec::new();
                {
                    for entry in doc_table.iter().map_err(|err| {
                        map_redb_storage_err(err, "scan memory_episode_search_doc")
                    })? {
                        let (k, v) = entry.map_err(|err| {
                            map_redb_storage_err(err, "read memory_episode_search_doc")
                        })?;
                        let rec: MemoryEpisodeSearchDocRecord =
                            decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                        if rec.episode_id == eid {
                            to_remove.push(k.value());
                        }
                    }
                }
                for rid in to_remove {
                    let _ = doc_table.remove(rid).map_err(|err| {
                        map_redb_storage_err(err, "delete memory_episode_search_doc")
                    })?;
                }
            }

            let mut ep_table = wtx
                .open_table(MEMORY_EPISODE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open memory_episode"))?;
            let removed = ep_table
                .remove(eid.as_str())
                .map_err(|err| map_redb_storage_err(err, "delete memory_episode"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn replace_memory_episode_evidence(
        &self,
        episode_id: &str,
        evidence: &[MemoryEpisodeEvidenceRecord],
    ) -> StorageResult<()> {
        validate_non_empty_identifier(episode_id, "memory_episode_evidence episode_id")?;
        let mut seen_ords = BTreeSet::new();
        for ev in evidence {
            validate_memory_episode_evidence(ev)?;
            if ev.episode_id != episode_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "memory_episode_evidence episode_id mismatch".to_string(),
                });
            }
            if !seen_ords.insert(ev.ordinal) {
                return Err(StorageBackendError::BackendInternal {
                    detail: "duplicate ordinal in memory_episode_evidence".to_string(),
                });
            }
        }
        let eid = episode_id.to_string();
        let owned = evidence.to_vec();
        self.run_write_conditional(move |wtx| {
            ensure_memory_episode_exists_in_wtx(wtx, &eid)?;
            let mut table = wtx
                .open_table(MEMORY_EPISODE_EVIDENCE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open memory_episode_evidence"))?;
            let mut old_ords = Vec::new();
            {
                let range = table
                    .range((eid.as_str(), i64::MIN)..=(eid.as_str(), i64::MAX))
                    .map_err(|err| map_redb_storage_err(err, "scan memory_episode_evidence"))?;
                for entry in range {
                    let (k, _) = entry
                        .map_err(|err| map_redb_storage_err(err, "read memory_episode_evidence"))?;
                    old_ords.push(k.value().1);
                }
            }
            for ord in old_ords {
                let _ = table
                    .remove((eid.as_str(), ord))
                    .map_err(|err| map_redb_storage_err(err, "clear memory_episode_evidence"))?;
            }
            for ev in &owned {
                let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, ev)?;
                table
                    .insert((ev.episode_id.as_str(), ev.ordinal), bytes.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert memory_episode_evidence"))?;
            }
            Ok((true, ()))
        })
        .await
    }

    async fn list_memory_episode_evidence(
        &self,
        episode_id: &str,
    ) -> StorageResult<Vec<MemoryEpisodeEvidenceRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode_evidence episode_id")?;
        let eid = episode_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                MEMORY_EPISODE_EVIDENCE_TABLE,
                "open memory_episode_evidence",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((eid.as_str(), i64::MIN)..=(eid.as_str(), i64::MAX))
                .map_err(|err| map_redb_storage_err(err, "scan memory_episode_evidence"))?;
            for entry in range {
                let (_, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read memory_episode_evidence"))?;
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_memory_projection_state(
        &self,
        record: &MemoryProjectionStateRecord,
    ) -> StorageResult<()> {
        validate_memory_projection_state(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(MEMORY_PROJECTION_STATE_TABLE)
                .map_err(|err| map_redb_table_err(err, "open memory_projection_state"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (rec.repo_id.as_str(), rec.source_kind.as_str()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert memory_projection_state"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn get_memory_projection_state(
        &self,
        repo_id: &str,
        source_kind: MemorySourceKind,
    ) -> StorageResult<Option<MemoryProjectionStateRecord>> {
        validate_non_empty_identifier(repo_id, "memory_projection_state repo_id")?;
        let rid = repo_id.to_string();
        let sk = source_kind.as_str();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                MEMORY_PROJECTION_STATE_TABLE,
                "open memory_projection_state",
            )?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get((rid.as_str(), sk))
                .map_err(|err| map_redb_storage_err(err, "get memory_projection_state"))?
            else {
                return Ok(None);
            };
            decode_versioned(g.value(), GIT_STATE_CODEC_VERSION).map(Some)
        })
        .await
    }

    async fn list_memory_projection_states(
        &self,
        repo_id: &str,
    ) -> StorageResult<Vec<MemoryProjectionStateRecord>> {
        validate_non_empty_identifier(repo_id, "memory_projection_state repo_id")?;
        let rid = repo_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                MEMORY_PROJECTION_STATE_TABLE,
                "open memory_projection_state",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((rid.as_str(), "")..)
                .map_err(|err| map_redb_storage_err(err, "scan memory_projection_state"))?;
            for entry in range {
                let (k, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read memory_projection_state"))?;
                if k.value().0 != rid {
                    break;
                }
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_memory_episode_path(
        &self,
        record: &MemoryEpisodePathRecord,
    ) -> StorageResult<()> {
        validate_memory_episode_path(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            ensure_memory_episode_exists_in_wtx(wtx, &rec.episode_id)?;
            let mut path_table = wtx
                .open_table(MEMORY_EPISODE_PATH_TABLE)
                .map_err(|err| map_redb_table_err(err, "open memory_episode_path"))?;
            let old_ended_at = match path_table
                .get((rec.episode_id.as_str(), rec.code_path.as_slice()))
                .map_err(|err| map_redb_storage_err(err, "read prior memory_episode_path"))?
            {
                Some(g) => {
                    let prev: MemoryEpisodePathRecord =
                        decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?;
                    Some(prev.ended_at)
                }
                None => None,
            };
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            path_table
                .insert(
                    (rec.episode_id.as_str(), rec.code_path.as_slice()),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert memory_episode_path"))?;
            drop(path_table);

            let mut idx_table = wtx
                .open_table(MEMORY_BY_PATH_TABLE)
                .map_err(|err| map_redb_table_err(err, "open MEMORY_BY_PATH"))?;
            if let Some(old_ts) = old_ended_at
                && old_ts != rec.ended_at
            {
                let _ = idx_table
                    .remove((
                        rec.code_path.as_slice(),
                        invert_ended_at(old_ts),
                        rec.episode_id.as_str(),
                    ))
                    .map_err(|err| map_redb_storage_err(err, "remove stale MEMORY_BY_PATH"))?;
            }
            let marker = encode_versioned(GIT_STATE_CODEC_VERSION, &UnitMarker)?;
            idx_table
                .insert(
                    (
                        rec.code_path.as_slice(),
                        invert_ended_at(rec.ended_at),
                        rec.episode_id.as_str(),
                    ),
                    marker.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert MEMORY_BY_PATH"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_memory_episode_paths(
        &self,
        episode_id: &str,
    ) -> StorageResult<Vec<MemoryEpisodePathRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode_path episode_id")?;
        let eid = episode_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, MEMORY_EPISODE_PATH_TABLE, "open memory_episode_path")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((eid.as_str(), &[][..])..)
                .map_err(|err| map_redb_storage_err(err, "scan memory_episode_path"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read memory_episode_path"))?;
                if k.value().0 != eid {
                    break;
                }
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    async fn query_memory_paths_by_path(
        &self,
        code_path: &[u8],
        since_ended_at: Option<i64>,
        until_ended_at: Option<i64>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<MemoryEpisodePathRecord>> {
        validate_memory_code_path(code_path)?;
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        if let (Some(since), Some(until)) = (since_ended_at, until_ended_at)
            && since > until
        {
            return Ok(Vec::new());
        }
        let target_path = code_path.to_vec();
        let inv_start = until_ended_at.map(invert_ended_at).unwrap_or(0);
        let inv_end = since_ended_at.map(invert_ended_at).unwrap_or(u64::MAX);
        self.run_read(move |rtx| {
            let Some(idx_table) = open_opt_table(rtx, MEMORY_BY_PATH_TABLE, "open MEMORY_BY_PATH")?
            else {
                return Ok(Vec::new());
            };
            let Some(path_table) =
                open_opt_table(rtx, MEMORY_EPISODE_PATH_TABLE, "open memory_episode_path")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = idx_table
                .range((target_path.as_slice(), inv_start, "")..)
                .map_err(|err| map_redb_storage_err(err, "scan MEMORY_BY_PATH"))?;
            for entry in range {
                let (k, _) =
                    entry.map_err(|err| map_redb_storage_err(err, "read MEMORY_BY_PATH"))?;
                let (found_path, found_inv_ts, ep_id) = k.value();
                if found_path != target_path.as_slice() || found_inv_ts > inv_end {
                    break;
                }
                let guard = path_table
                    .get((ep_id, found_path))
                    .map_err(|err| map_redb_storage_err(err, "get memory_episode_path via index"))?
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "MEMORY_BY_PATH references missing memory_episode_path row"
                            .to_string(),
                    })?;
                out.push(decode_versioned(guard.value(), GIT_STATE_CODEC_VERSION)?);
                if let Some(lim) = limit
                    && out.len() >= lim
                {
                    break;
                }
            }
            Ok(out)
        })
        .await
    }

    async fn upsert_memory_episode_search_doc(
        &self,
        episode_id: &str,
        title: &str,
        body: &str,
        paths_text: &str,
    ) -> StorageResult<MemoryEpisodeSearchDocRecord> {
        validate_non_empty_identifier(episode_id, "memory_episode_search_doc episode_id")?;
        validate_no_nul(title, "memory_episode_search_doc title")?;
        validate_no_nul(body, "memory_episode_search_doc body")?;
        validate_no_nul(paths_text, "memory_episode_search_doc paths_text")?;
        let eid = episode_id.to_string();
        let title_owned = title.to_string();
        let body_owned = body.to_string();
        let paths_owned = paths_text.to_string();
        self.run_write_conditional(move |wtx| {
            ensure_memory_episode_exists_in_wtx(wtx, &eid)?;
            let mut table = wtx
                .open_table(MEMORY_EPISODE_SEARCH_DOC_TABLE)
                .map_err(|err| map_redb_table_err(err, "open memory_episode_search_doc"))?;
            let mut existing_rowid: Option<i64> = None;
            {
                for entry in table
                    .iter()
                    .map_err(|err| map_redb_storage_err(err, "scan memory_episode_search_doc"))?
                {
                    let (k, v) = entry.map_err(|err| {
                        map_redb_storage_err(err, "read memory_episode_search_doc")
                    })?;
                    let row: MemoryEpisodeSearchDocRecord =
                        decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                    if row.episode_id == eid {
                        existing_rowid = Some(k.value());
                        break;
                    }
                }
            }
            let rowid = match existing_rowid {
                Some(rid) => rid,
                None => {
                    let last_id = table
                        .last()
                        .map_err(|err| {
                            map_redb_storage_err(err, "read last memory_episode_search_doc rowid")
                        })?
                        .map(|(k, _)| k.value())
                        .unwrap_or(0);
                    last_id
                        .checked_add(1)
                        .ok_or_else(|| StorageBackendError::BackendInternal {
                            detail: "memory_episode_search_doc rowid overflowed i64".to_string(),
                        })?
                }
            };
            let rec = MemoryEpisodeSearchDocRecord {
                rowid,
                episode_id: eid,
                title: title_owned,
                body: body_owned,
                paths_text: paths_owned,
            };
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rowid, bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "upsert memory_episode_search_doc"))?;
            Ok((true, rec))
        })
        .await
    }

    async fn get_memory_episode_search_doc(
        &self,
        episode_id: &str,
    ) -> StorageResult<Option<MemoryEpisodeSearchDocRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode_search_doc episode_id")?;
        let eid = episode_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                MEMORY_EPISODE_SEARCH_DOC_TABLE,
                "open memory_episode_search_doc",
            )?
            else {
                return Ok(None);
            };
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter memory_episode_search_doc"))?
            {
                let (_, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read memory_episode_search_doc"))?;
                let rec: MemoryEpisodeSearchDocRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if rec.episode_id == eid {
                    return Ok(Some(rec));
                }
            }
            Ok(None)
        })
        .await
    }

    async fn list_memory_episode_search_docs(
        &self,
    ) -> StorageResult<Vec<MemoryEpisodeSearchDocRecord>> {
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                MEMORY_EPISODE_SEARCH_DOC_TABLE,
                "open memory_episode_search_doc",
            )?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter memory_episode_search_doc"))?
            {
                let (_, v) = entry
                    .map_err(|err| map_redb_storage_err(err, "read memory_episode_search_doc"))?;
                out.push(decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?);
            }
            Ok(out)
        })
        .await
    }

    // -------------------------------------------------------------------------
    // RB-05b AC4: Governance & Audit (`approved_permission`, `automation_log`, `source_call_log`)
    // -------------------------------------------------------------------------

    async fn upsert_approved_permission(
        &self,
        record: &ApprovedPermissionRecord,
    ) -> StorageResult<()> {
        validate_approved_permission(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(APPROVED_PERMISSION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open approved_permission"))?;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(
                    (
                        rec.project_id.as_str(),
                        rec.permission.as_str(),
                        rec.pattern.as_str(),
                    ),
                    bytes.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert approved_permission"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_approved_permissions(
        &self,
        project_id: &str,
    ) -> StorageResult<Vec<ApprovedPermissionRecord>> {
        validate_non_empty_identifier(project_id, "approved_permission project_id")?;
        let pid = project_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) =
                open_opt_table(rtx, APPROVED_PERMISSION_TABLE, "open approved_permission")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            let range = table
                .range((pid.as_str(), "", "")..)
                .map_err(|err| map_redb_storage_err(err, "scan approved_permission"))?;
            for entry in range {
                let (k, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read approved_permission"))?;
                if k.value().0 != pid {
                    break;
                }
                let rec: ApprovedPermissionRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                out.push(rec);
            }
            out.sort_by(|a, b| {
                a.created_at
                    .cmp(&b.created_at)
                    .then_with(|| a.permission.cmp(&b.permission))
                    .then_with(|| a.pattern.cmp(&b.pattern))
            });
            Ok(out)
        })
        .await
    }

    async fn delete_approved_permission(
        &self,
        project_id: &str,
        permission: &str,
        pattern: &str,
    ) -> StorageResult<bool> {
        validate_non_empty_identifier(project_id, "approved_permission project_id")?;
        validate_non_empty_identifier(permission, "approved_permission permission")?;
        validate_non_empty_identifier(pattern, "approved_permission pattern")?;
        let pid = project_id.to_string();
        let perm = permission.to_string();
        let pat = pattern.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(APPROVED_PERMISSION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open approved_permission"))?;
            let removed = table
                .remove((pid.as_str(), perm.as_str(), pat.as_str()))
                .map_err(|err| map_redb_storage_err(err, "delete approved_permission"))?
                .is_some();
            Ok((removed, removed))
        })
        .await
    }

    async fn append_automation_log(&self, record: &AutomationLogRecord) -> StorageResult<()> {
        validate_automation_log(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AUTOMATION_LOG_TABLE)
                .map_err(|err| map_redb_table_err(err, "open automation_log"))?;
            if table
                .get(rec.id.as_str())
                .map_err(|err| map_redb_storage_err(err, "check duplicate automation_log id"))?
                .is_some()
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "duplicate automation_log id".to_string(),
                });
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert automation_log"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_automation_logs(
        &self,
        rule_id: Option<&str>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<AutomationLogRecord>> {
        if let Some(r) = rule_id {
            validate_non_empty_identifier(r, "automation_log rule_id")?;
        }
        if limit == Some(0) {
            return Ok(Vec::new());
        }
        let filter = rule_id.map(str::to_string);
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, AUTOMATION_LOG_TABLE, "open automation_log")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter automation_log"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read automation_log"))?;
                let rec: AutomationLogRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if filter.as_deref().is_none_or(|f| rec.rule_id == f) {
                    out.push(rec);
                }
            }
            out.sort_by(|a, b| {
                b.finished_at
                    .cmp(&a.finished_at)
                    .then_with(|| a.id.cmp(&b.id))
            });
            if let Some(lim) = limit {
                out.truncate(lim);
            }
            Ok(out)
        })
        .await
    }

    async fn append_source_call_log(&self, record: &SourceCallLogRecord) -> StorageResult<()> {
        validate_source_call_log(record)?;
        let rec = record.clone();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(SOURCE_CALL_LOG_TABLE)
                .map_err(|err| map_redb_table_err(err, "open source_call_log"))?;
            if table
                .get(rec.id.as_str())
                .map_err(|err| map_redb_storage_err(err, "check duplicate source_call_log id"))?
                .is_some()
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "duplicate source_call_log id".to_string(),
                });
            }
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &rec)?;
            table
                .insert(rec.id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert source_call_log"))?;
            Ok((true, ()))
        })
        .await
    }

    async fn list_source_call_logs(
        &self,
        session_id: Option<&str>,
        agent_run_id: Option<&str>,
    ) -> StorageResult<Vec<SourceCallLogRecord>> {
        if let Some(s) = session_id {
            validate_non_empty_identifier(s, "source_call_log session_id")?;
        }
        if let Some(r) = agent_run_id {
            validate_non_empty_identifier(r, "source_call_log agent_run_id")?;
        }
        let s_filter = session_id.map(str::to_string);
        let r_filter = agent_run_id.map(str::to_string);
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(rtx, SOURCE_CALL_LOG_TABLE, "open source_call_log")?
            else {
                return Ok(Vec::new());
            };
            let mut out = Vec::new();
            for entry in table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iter source_call_log"))?
            {
                let (_, v) =
                    entry.map_err(|err| map_redb_storage_err(err, "read source_call_log"))?;
                let rec: SourceCallLogRecord =
                    decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
                if s_filter.as_deref().is_none_or(|f| rec.session_id == f)
                    && r_filter
                        .as_deref()
                        .is_none_or(|f| rec.agent_run_id.as_deref() == Some(f))
                {
                    out.push(rec);
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

    // -------------------------------------------------------------------------
    // RB-05b AC5: Bridge Two-Phase Publication (`prepared` -> `published`)
    // -------------------------------------------------------------------------

    async fn prepare_bridge_commit_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<BridgePublicationRecord> {
        validate_bridge_operation_id(operation_id)?;
        validate_hex_oid(commit_oid, "agent_bridge_publication commit_oid")?;
        let op_id = operation_id.to_string();
        let oid = commit_oid.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AGENT_BRIDGE_PUBLICATION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open agent_bridge_publication"))?;
            let existing: Option<BridgePublicationStoredRow> = match table
                .get(op_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read agent_bridge_publication"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            match existing {
                Some(row) if row.commit_oid == oid => Ok((
                    false,
                    BridgePublicationRecord {
                        operation_id: op_id,
                        commit_oid: row.commit_oid,
                        phase: row.phase,
                    },
                )),
                Some(_) => Err(StorageBackendError::BackendInternal {
                    detail: "bridge commit publication already bound to a different commit"
                        .to_string(),
                }),
                None => {
                    let stored = BridgePublicationStoredRow {
                        commit_oid: oid.clone(),
                        phase: BridgePublicationPhase::Prepared,
                    };
                    let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &stored)?;
                    table
                        .insert(op_id.as_str(), bytes.as_slice())
                        .map_err(|err| {
                            map_redb_storage_err(err, "insert prepared agent_bridge_publication")
                        })?;
                    Ok((
                        true,
                        BridgePublicationRecord {
                            operation_id: op_id,
                            commit_oid: oid,
                            phase: BridgePublicationPhase::Prepared,
                        },
                    ))
                }
            }
        })
        .await
    }

    async fn publish_bridge_commit_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<BridgePublicationRecord> {
        validate_bridge_operation_id(operation_id)?;
        validate_hex_oid(commit_oid, "agent_bridge_publication commit_oid")?;
        let op_id = operation_id.to_string();
        let oid = commit_oid.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AGENT_BRIDGE_PUBLICATION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open agent_bridge_publication"))?;
            let existing: Option<BridgePublicationStoredRow> = match table
                .get(op_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read agent_bridge_publication"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let row = existing.ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "cannot publish bridge commit publication before prepare phase".to_string(),
            })?;
            if row.commit_oid != oid {
                return Err(StorageBackendError::BackendInternal {
                    detail: "bridge commit publication already bound to a different commit"
                        .to_string(),
                });
            }
            if row.phase == BridgePublicationPhase::Published {
                return Ok((
                    false,
                    BridgePublicationRecord {
                        operation_id: op_id,
                        commit_oid: oid,
                        phase: BridgePublicationPhase::Published,
                    },
                ));
            }
            let stored = BridgePublicationStoredRow {
                commit_oid: oid.clone(),
                phase: BridgePublicationPhase::Published,
            };
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &stored)?;
            table
                .insert(op_id.as_str(), bytes.as_slice())
                .map_err(|err| map_redb_storage_err(err, "publish agent_bridge_publication"))?;
            Ok((
                true,
                BridgePublicationRecord {
                    operation_id: op_id,
                    commit_oid: oid,
                    phase: BridgePublicationPhase::Published,
                },
            ))
        })
        .await
    }

    async fn get_bridge_commit_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgePublicationRecord>> {
        validate_bridge_operation_id(operation_id)?;
        let op_id = operation_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AGENT_BRIDGE_PUBLICATION_TABLE,
                "open agent_bridge_publication",
            )?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(op_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "get agent_bridge_publication"))?
            else {
                return Ok(None);
            };
            let stored: BridgePublicationStoredRow =
                decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?;
            Ok(Some(BridgePublicationRecord {
                operation_id: op_id,
                commit_oid: stored.commit_oid,
                phase: stored.phase,
            }))
        })
        .await
    }

    async fn prepare_bridge_index_publication(
        &self,
        operation_id: &str,
        index_digest: &str,
    ) -> StorageResult<BridgeIndexPublicationRecord> {
        validate_bridge_operation_id(operation_id)?;
        validate_non_empty_identifier(index_digest, "agent_bridge_index_publication index_digest")?;
        let op_id = operation_id.to_string();
        let digest = index_digest.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AGENT_BRIDGE_INDEX_PUBLICATION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open agent_bridge_index_publication"))?;
            let existing: Option<BridgeIndexPublicationStoredRow> = match table
                .get(op_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read agent_bridge_index_publication"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            match existing {
                Some(row) if row.index_digest == digest => Ok((
                    false,
                    BridgeIndexPublicationRecord {
                        operation_id: op_id,
                        index_digest: row.index_digest,
                        phase: row.phase,
                    },
                )),
                Some(_) => Err(StorageBackendError::BackendInternal {
                    detail: "bridge index publication already bound to a different digest"
                        .to_string(),
                }),
                None => {
                    let stored = BridgeIndexPublicationStoredRow {
                        index_digest: digest.clone(),
                        phase: BridgePublicationPhase::Prepared,
                    };
                    let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &stored)?;
                    table
                        .insert(op_id.as_str(), bytes.as_slice())
                        .map_err(|err| {
                            map_redb_storage_err(
                                err,
                                "insert prepared agent_bridge_index_publication",
                            )
                        })?;
                    Ok((
                        true,
                        BridgeIndexPublicationRecord {
                            operation_id: op_id,
                            index_digest: digest,
                            phase: BridgePublicationPhase::Prepared,
                        },
                    ))
                }
            }
        })
        .await
    }

    async fn publish_bridge_index_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgeIndexPublicationRecord>> {
        validate_bridge_operation_id(operation_id)?;
        let op_id = operation_id.to_string();
        self.run_write_conditional(move |wtx| {
            let mut table = wtx
                .open_table(AGENT_BRIDGE_INDEX_PUBLICATION_TABLE)
                .map_err(|err| map_redb_table_err(err, "open agent_bridge_index_publication"))?;
            let existing: Option<BridgeIndexPublicationStoredRow> = match table
                .get(op_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "read agent_bridge_index_publication"))?
            {
                Some(g) => Some(decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?),
                None => None,
            };
            let Some(mut row) = existing else {
                return Ok((false, None));
            };
            row.phase = BridgePublicationPhase::Published;
            let bytes = encode_versioned(GIT_STATE_CODEC_VERSION, &row)?;
            table
                .insert(op_id.as_str(), bytes.as_slice())
                .map_err(|err| {
                    map_redb_storage_err(err, "publish agent_bridge_index_publication")
                })?;
            Ok((
                true,
                Some(BridgeIndexPublicationRecord {
                    operation_id: op_id,
                    index_digest: row.index_digest,
                    phase: BridgePublicationPhase::Published,
                }),
            ))
        })
        .await
    }

    async fn get_bridge_index_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgeIndexPublicationRecord>> {
        validate_bridge_operation_id(operation_id)?;
        let op_id = operation_id.to_string();
        self.run_read(move |rtx| {
            let Some(table) = open_opt_table(
                rtx,
                AGENT_BRIDGE_INDEX_PUBLICATION_TABLE,
                "open agent_bridge_index_publication",
            )?
            else {
                return Ok(None);
            };
            let Some(g) = table
                .get(op_id.as_str())
                .map_err(|err| map_redb_storage_err(err, "get agent_bridge_index_publication"))?
            else {
                return Ok(None);
            };
            let stored: BridgeIndexPublicationStoredRow =
                decode_versioned(g.value(), GIT_STATE_CODEC_VERSION)?;
            Ok(Some(BridgeIndexPublicationRecord {
                operation_id: op_id,
                index_digest: stored.index_digest,
                phase: stored.phase,
            }))
        })
        .await
    }
}

fn ensure_ai_thread_exists_in_wtx(wtx: &WriteTransaction, thread_id: &str) -> StorageResult<()> {
    let table = wtx
        .open_table(AI_THREAD_TABLE)
        .map_err(|err| map_redb_table_err(err, "open ai_thread"))?;
    if table
        .get(thread_id)
        .map_err(|err| map_redb_storage_err(err, "check ai_thread parent"))?
        .is_none()
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "referenced ai_thread does not exist".to_string(),
        });
    }
    Ok(())
}

fn ensure_memory_episode_exists_in_wtx(
    wtx: &WriteTransaction,
    episode_id: &str,
) -> StorageResult<()> {
    let table = wtx
        .open_table(MEMORY_EPISODE_TABLE)
        .map_err(|err| map_redb_table_err(err, "open memory_episode"))?;
    if table
        .get(episode_id)
        .map_err(|err| map_redb_storage_err(err, "check memory_episode parent"))?
        .is_none()
    {
        return Err(StorageBackendError::BackendInternal {
            detail: "referenced memory_episode does not exist".to_string(),
        });
    }
    Ok(())
}

fn delete_str_pair_prefix(
    wtx: &WriteTransaction,
    def: TableDefinition<'static, (&'static str, &'static str), &'static [u8]>,
    prefix: &str,
    label: &'static str,
) -> StorageResult<()> {
    let mut table = wtx
        .open_table(def)
        .map_err(|err| map_redb_table_err(err, label))?;
    let mut keys = Vec::new();
    {
        let range = table
            .range((prefix, "")..)
            .map_err(|err| map_redb_storage_err(err, label))?;
        for entry in range {
            let (k, _) = entry.map_err(|err| map_redb_storage_err(err, label))?;
            let (p, second) = k.value();
            if p != prefix {
                break;
            }
            keys.push(second.to_string());
        }
    }
    for second in keys {
        let _ = table
            .remove((prefix, second.as_str()))
            .map_err(|err| map_redb_storage_err(err, label))?;
    }
    Ok(())
}

fn delete_by_thread_filter<T: serde::de::DeserializeOwned>(
    wtx: &WriteTransaction,
    def: TableDefinition<'static, &'static str, &'static [u8]>,
    thread_id: &str,
    get_thread: impl Fn(&T) -> &str,
    label: &'static str,
) -> StorageResult<()> {
    let mut table = wtx
        .open_table(def)
        .map_err(|err| map_redb_table_err(err, label))?;
    let mut keys = Vec::new();
    {
        for entry in table
            .iter()
            .map_err(|err| map_redb_storage_err(err, label))?
        {
            let (k, v) = entry.map_err(|err| map_redb_storage_err(err, label))?;
            let row: T = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
            if get_thread(&row) == thread_id {
                keys.push(k.value().to_string());
            }
        }
    }
    for key in keys {
        let _ = table
            .remove(key.as_str())
            .map_err(|err| map_redb_storage_err(err, label))?;
    }
    Ok(())
}

fn clear_latest_for_thread<T: Serialize + serde::de::DeserializeOwned>(
    table: &mut redb::Table<'_, &'static str, &'static [u8]>,
    thread_id: &str,
    keep_id: &str,
    inspect: impl Fn(&T) -> (&str, &str, bool),
    clear_flag: impl Fn(&mut T),
    label: &'static str,
) -> StorageResult<()> {
    let mut to_update: Vec<(String, T)> = Vec::new();
    {
        for entry in table
            .iter()
            .map_err(|err| map_redb_storage_err(err, label))?
        {
            let (_, v) = entry.map_err(|err| map_redb_storage_err(err, label))?;
            let mut row: T = decode_versioned(v.value(), GIT_STATE_CODEC_VERSION)?;
            let (row_tid, row_id, is_latest) = inspect(&row);
            if row_tid == thread_id && is_latest && row_id != keep_id {
                let id_owned = row_id.to_string();
                clear_flag(&mut row);
                to_update.push((id_owned, row));
            }
        }
    }
    for (id, row) in to_update {
        let b = encode_versioned(GIT_STATE_CODEC_VERSION, &row)?;
        table
            .insert(id.as_str(), b.as_slice())
            .map_err(|err| map_redb_storage_err(err, label))?;
    }
    Ok(())
}
