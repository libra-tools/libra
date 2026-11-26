//! SQLite implementation of [`GitStateStore`] (`RB-05`).
//!
//! Operates directly against the 21 canonical workflow, worktree/workspace,
//! OpLog v2, change-lineage, and dirty/layer/sparse tables inside single
//! `BEGIN IMMEDIATE` write transactions (`begin_write_transaction`).

use std::{collections::BTreeSet, path::Path, time::Duration};

use async_trait::async_trait;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, QueryResult, Statement,
    TransactionTrait, Value,
};

use crate::{
    internal::db::{
        DatabaseRole, begin_write_transaction,
        schema::{create_database_for_role, establish_connection_for_role},
        sqlite_pool_options,
    },
    storage::{
        backend::StorageBackendKind,
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
            LayerPathRecord, LayerRecord, MemoryEpisodeEvidenceRecord, MemoryEpisodePathRecord,
            MemoryEpisodeRecord, MemoryEpisodeSearchDocRecord, MemoryEvidenceKind,
            MemoryLinkConfidence, MemoryOutcome, MemoryPathChangeKind, MemoryProjectionStateRecord,
            MemoryResolutionStatus, MemorySourceKind, OP_HEAD_GENERATION_SENTINEL,
            OperationHeadCasOutcome, OperationHeadConvergeOutcome, OperationHeadsSnapshot,
            OperationJournalRecord, OperationRecord, OperationScopeKind, OperationScopeProvenance,
            RebaseStateRecord, SequenceStateRecord, SourceCallLogRecord, SparseViewSnapshot,
            WorkingDirtyEntryRecord, WorkingDirtyMetaRecord, WorkspaceLeaseAcquireOutcome,
            WorkspaceLeaseMutationOutcome, WorkspaceRecordKind, WorkspaceRecordOwnerKind,
            WorkspaceRecordRow, WorkspaceRecordState, WorktreeIntentOp, WorktreeIntentRecord,
            WorktreeIntentWrite, WorktreeLifecycleRecord, WorktreeLifecycleState,
            normalize_op_heads, validate_ai_decision_proposal, validate_ai_final_decision,
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
        sqlite::map_sqlite_db_err,
    },
};

const CREATE_BRIDGE_PUBLICATION_SQL: &str = "\
CREATE TABLE IF NOT EXISTS agent_bridge_publication ( \
    operation_id TEXT PRIMARY KEY, \
    commit_oid TEXT NOT NULL, \
    phase TEXT NOT NULL DEFAULT 'published' \
)";

const CREATE_BRIDGE_INDEX_PUBLICATION_SQL: &str = "\
CREATE TABLE IF NOT EXISTS agent_bridge_index_publication ( \
    operation_id TEXT PRIMARY KEY, \
    index_digest TEXT NOT NULL, \
    phase TEXT NOT NULL DEFAULT 'prepared' \
)";

async fn ensure_bridge_publication_tables(conn: &DatabaseConnection) -> StorageResult<()> {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_string(
        backend,
        CREATE_BRIDGE_PUBLICATION_SQL.to_string(),
    ))
    .await
    .map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "create agent_bridge_publication table",
            err,
        )
    })?;
    let col_row = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT COUNT(*) FROM pragma_table_info('agent_bridge_publication') WHERE name = 'phase'"
                .to_string(),
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "inspect agent_bridge_publication columns",
                err,
            )
        })?;
    let count: i64 = col_row
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "failed to inspect agent_bridge_publication phase column".to_string(),
        })?
        .try_get_by_index(0)
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode agent_bridge_publication column count",
                err,
            )
        })?;
    if count == 0 {
        conn.execute_raw(Statement::from_string(
            backend,
            "ALTER TABLE agent_bridge_publication ADD COLUMN phase TEXT NOT NULL DEFAULT 'published'"
                .to_string(),
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "add phase column to agent_bridge_publication",
                err,
            )
        })?;
    }
    conn.execute_raw(Statement::from_string(
        backend,
        CREATE_BRIDGE_INDEX_PUBLICATION_SQL.to_string(),
    ))
    .await
    .map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "create agent_bridge_index_publication table",
            err,
        )
    })?;
    Ok(())
}

async fn insert_operation_records_in_txn<C: ConnectionTrait>(
    txn: &C,
    op: &OperationRecord,
    parent_op_ids: &[String],
    journal: Option<&OperationJournalRecord>,
) -> StorageResult<()> {
    let backend = txn.get_database_backend();
    txn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO operation \
         (op_id, repo_id, format_version, kind, status, command_name, description, \
          args_digest, actor, worktree_id, scope_kind, scope_provenance, pre_view_oid, \
          post_view_oid, restores_op_id, reverts_op_id, predecessor_map_oid, \
          causal_context_id, restorable, control_slot, claim_owner, start_ts, end_ts) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            op.op_id.clone().into(),
            op.repo_id.clone().into(),
            op.format_version.into(),
            op.kind.clone().into(),
            op.status.clone().into(),
            op.command_name.clone().into(),
            op.description.clone().into(),
            op.args_digest.clone().into(),
            op.actor.clone().into(),
            op.worktree_id.clone().into(),
            op.scope_kind.as_str().into(),
            op.scope_provenance.as_str().into(),
            op.pre_view_oid.clone().into(),
            op.post_view_oid.clone().into(),
            op.restores_op_id.clone().into(),
            op.reverts_op_id.clone().into(),
            op.predecessor_map_oid.clone().into(),
            op.causal_context_id.clone().into(),
            i64::from(op.restorable).into(),
            op.control_slot.clone().into(),
            op.claim_owner.clone().into(),
            op.start_ts.into(),
            op.end_ts.into(),
        ],
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "insert operation row", err))?;

    for (ordinal, parent_op_id) in parent_op_ids.iter().enumerate() {
        let ord_i64 = i64::try_from(ordinal).unwrap_or(i64::MAX);
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO operation_parent (op_id, parent_op_id, ordinal) VALUES (?, ?, ?)",
            [
                op.op_id.clone().into(),
                parent_op_id.clone().into(),
                ord_i64.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "insert operation_parent row", err)
        })?;
    }

    if let Some(jrn) = journal {
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO operation_journal \
             (journal_id, op_id, phase, pre_view_oid, target_view_oid, owner, updated_at, recovery_payload) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            [
                jrn.journal_id.clone().into(),
                jrn.op_id.clone().into(),
                jrn.phase.clone().into(),
                jrn.pre_view_oid.clone().into(),
                jrn.target_view_oid.clone().into(),
                jrn.owner.clone().into(),
                jrn.updated_at.into(),
                jrn.recovery_payload.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "insert operation_journal row", err)
        })?;
    }

    Ok(())
}

async fn converge_operation_heads_in_txn<C: ConnectionTrait>(
    txn: &C,
    repo_id: &str,
    scope_key: &str,
    expected: &[String],
    replacement: &[String],
) -> StorageResult<OperationHeadConvergeOutcome> {
    let current = query_operation_heads_in_conn(txn, repo_id, scope_key).await?;
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
    replace_operation_heads_in_conn(txn, repo_id, scope_key, next_gen, &merged_vec).await?;
    Ok(OperationHeadConvergeOutcome {
        generation: next_gen,
        heads: merged_vec,
        converged_cleanly,
    })
}

/// SQLite-backed implementation of [`GitStateStore`].
#[derive(Clone)]
pub struct SqliteGitStateStore {
    conn: DatabaseConnection,
    read_only: bool,
}

impl std::fmt::Debug for SqliteGitStateStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteGitStateStore")
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

impl SqliteGitStateStore {
    /// Create a brand-new repository SQLite database at `path` with full schema migrations.
    pub async fn create(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path_str =
            path.as_ref()
                .to_str()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "sqlite repository database path must be valid UTF-8".to_string(),
                })?;
        let conn = create_database_for_role(path_str, DatabaseRole::Repository)
            .await
            .map_err(|err| StorageBackendError::io("create sqlite git state store", err))?;
        ensure_bridge_publication_tables(&conn).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                path.as_ref(),
                std::fs::Permissions::from_mode(
                    crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE,
                ),
            )
            .map_err(|err| StorageBackendError::io("chmod sqlite git state store to 0600", err))?;
        }
        Ok(Self {
            conn,
            read_only: false,
        })
    }

    /// Open an existing writable repository SQLite database at `path`.
    pub async fn open(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path_str =
            path.as_ref()
                .to_str()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "sqlite repository database path must be valid UTF-8".to_string(),
                })?;
        let conn = establish_connection_for_role(path_str, DatabaseRole::Repository)
            .await
            .map_err(|err| StorageBackendError::io("open sqlite git state store", err))?;
        ensure_bridge_publication_tables(&conn).await?;
        Ok(Self {
            conn,
            read_only: false,
        })
    }

    /// Open an existing repository SQLite database strictly read-only.
    pub async fn open_read_only(path: impl AsRef<Path>) -> StorageResult<Self> {
        let filename = std::path::absolute(path.as_ref()).map_err(|err| {
            StorageBackendError::io("resolve sqlite git state read-only path", err)
        })?;
        if filename.to_str().is_none() {
            return Err(StorageBackendError::BackendInternal {
                detail: "sqlite repository database path must be valid UTF-8".to_string(),
            });
        }
        let mut options = ConnectOptions::new("sqlite://storage-git-state-readonly");
        options.max_connections(1);
        options.sqlx_logging(false);
        options.map_sqlx_sqlite_pool_opts(sqlite_pool_options);
        options.map_sqlx_sqlite_opts(move |opts| {
            opts.filename(&filename)
                .read_only(true)
                .create_if_missing(false)
                .busy_timeout(Duration::from_secs(30))
        });
        let conn = Database::connect(options).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "open sqlite git state store read-only",
                err,
            )
        })?;
        Ok(Self {
            conn,
            read_only: true,
        })
    }

    /// Wrap an already-opened repository [`DatabaseConnection`].
    pub fn from_connection(conn: DatabaseConnection, read_only: bool) -> Self {
        Self { conn, read_only }
    }

    fn ensure_writable(&self) -> StorageResult<()> {
        if self.read_only {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot mutate a read-only git state store".to_string(),
            });
        }
        Ok(())
    }
}

fn col<T: sea_orm::TryGetable>(
    row: &QueryResult,
    idx: usize,
    ctx: &'static str,
) -> StorageResult<T> {
    row.try_get_by_index(idx)
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, ctx, err))
}

fn decode_sequence_state(row: &QueryResult) -> StorageResult<SequenceStateRecord> {
    Ok(SequenceStateRecord {
        worktree_id: col(row, 0, "sequence_state.worktree_id")?,
        kind: col(row, 1, "sequence_state.kind")?,
        head_name: col(row, 2, "sequence_state.head_name")?,
        head_orig: col(row, 3, "sequence_state.head_orig")?,
        current_oid: col(row, 4, "sequence_state.current_oid")?,
        todo: col(row, 5, "sequence_state.todo")?,
        payload: col(row, 6, "sequence_state.payload")?,
        updated_at: col(row, 7, "sequence_state.updated_at")?,
    })
}

fn decode_rebase_state(row: &QueryResult) -> StorageResult<RebaseStateRecord> {
    let autosquash: i64 = col(row, 9, "rebase_state.autosquash")?;
    Ok(RebaseStateRecord {
        worktree_id: col(row, 0, "rebase_state.worktree_id")?,
        head_name: col(row, 1, "rebase_state.head_name")?,
        onto: col(row, 2, "rebase_state.onto")?,
        orig_head: col(row, 3, "rebase_state.orig_head")?,
        current_head: col(row, 4, "rebase_state.current_head")?,
        todo: col(row, 5, "rebase_state.todo")?,
        todo_actions: col(row, 6, "rebase_state.todo_actions")?,
        done: col(row, 7, "rebase_state.done")?,
        stopped_sha: col(row, 8, "rebase_state.stopped_sha")?,
        autosquash: autosquash != 0,
        empty_mode: col(row, 10, "rebase_state.empty_mode")?,
    })
}

fn decode_bisect_state(row: &QueryResult) -> StorageResult<BisectStateRecord> {
    let completed: i64 = col(row, 8, "bisect_state.completed")?;
    let first_parent: i64 = col(row, 9, "bisect_state.first_parent")?;
    Ok(BisectStateRecord {
        worktree_id: col(row, 0, "bisect_state.worktree_id")?,
        orig_head: col(row, 1, "bisect_state.orig_head")?,
        orig_head_name: col(row, 2, "bisect_state.orig_head_name")?,
        bad: col(row, 3, "bisect_state.bad")?,
        good: col(row, 4, "bisect_state.good")?,
        current: col(row, 5, "bisect_state.current")?,
        skipped: col(row, 6, "bisect_state.skipped")?,
        steps: col(row, 7, "bisect_state.steps")?,
        completed: completed != 0,
        first_parent: first_parent != 0,
    })
}

fn decode_worktree_lifecycle(row: &QueryResult) -> StorageResult<WorktreeLifecycleRecord> {
    let state_str: String = col(row, 1, "worktree_lifecycle.state")?;
    Ok(WorktreeLifecycleRecord {
        worktree_id: col(row, 0, "worktree_lifecycle.worktree_id")?,
        state: WorktreeLifecycleState::parse(&state_str)?,
        path: col(row, 2, "worktree_lifecycle.path")?,
        reason: col(row, 3, "worktree_lifecycle.reason")?,
        created_at: col(row, 4, "worktree_lifecycle.created_at")?,
        updated_at: col(row, 5, "worktree_lifecycle.updated_at")?,
    })
}

fn decode_worktree_intent(row: &QueryResult) -> StorageResult<WorktreeIntentRecord> {
    let op_str: String = col(row, 1, "worktree_intent_journal.op")?;
    Ok(WorktreeIntentRecord {
        id: col(row, 0, "worktree_intent_journal.id")?,
        op: WorktreeIntentOp::parse(&op_str)?,
        worktree_id: col(row, 2, "worktree_intent_journal.worktree_id")?,
        payload: col(row, 3, "worktree_intent_journal.payload")?,
        stage: col(row, 4, "worktree_intent_journal.stage")?,
        created_at: col(row, 5, "worktree_intent_journal.created_at")?,
    })
}

fn decode_workspace_record(row: &QueryResult) -> StorageResult<WorkspaceRecordRow> {
    let kind_str: String = col(row, 2, "workspace_record.kind")?;
    let owner_kind_str: String = col(row, 5, "workspace_record.owner_kind")?;
    let state_str: String = col(row, 11, "workspace_record.state")?;
    Ok(WorkspaceRecordRow {
        workspace_id: col(row, 0, "workspace_record.workspace_id")?,
        repo_id: col(row, 1, "workspace_record.repo_id")?,
        kind: WorkspaceRecordKind::parse(&kind_str)?,
        worktree_id: col(row, 3, "workspace_record.worktree_id")?,
        path: col(row, 4, "workspace_record.path")?,
        owner_kind: WorkspaceRecordOwnerKind::parse(&owner_kind_str)?,
        owner_id: col(row, 6, "workspace_record.owner_id")?,
        task_id: col(row, 7, "workspace_record.task_id")?,
        session_id: col(row, 8, "workspace_record.session_id")?,
        base_commit: col(row, 9, "workspace_record.base_commit")?,
        branch: col(row, 10, "workspace_record.branch")?,
        state: WorkspaceRecordState::parse(&state_str)?,
        lease_owner: col(row, 12, "workspace_record.lease_owner")?,
        lease_fence: col(row, 13, "workspace_record.lease_fence")?,
        lease_expires_at: col(row, 14, "workspace_record.lease_expires_at")?,
        created_at: col(row, 15, "workspace_record.created_at")?,
        updated_at: col(row, 16, "workspace_record.updated_at")?,
    })
}

fn decode_operation_record(row: &QueryResult) -> StorageResult<OperationRecord> {
    let scope_kind_str: String = col(row, 10, "operation.scope_kind")?;
    let scope_prov_str: String = col(row, 11, "operation.scope_provenance")?;
    let restorable: i64 = col(row, 18, "operation.restorable")?;
    Ok(OperationRecord {
        op_id: col(row, 0, "operation.op_id")?,
        repo_id: col(row, 1, "operation.repo_id")?,
        format_version: col(row, 2, "operation.format_version")?,
        kind: col(row, 3, "operation.kind")?,
        status: col(row, 4, "operation.status")?,
        command_name: col(row, 5, "operation.command_name")?,
        description: col(row, 6, "operation.description")?,
        args_digest: col(row, 7, "operation.args_digest")?,
        actor: col(row, 8, "operation.actor")?,
        worktree_id: col(row, 9, "operation.worktree_id")?,
        scope_kind: OperationScopeKind::parse(&scope_kind_str)?,
        scope_provenance: OperationScopeProvenance::parse(&scope_prov_str)?,
        pre_view_oid: col(row, 12, "operation.pre_view_oid")?,
        post_view_oid: col(row, 13, "operation.post_view_oid")?,
        restores_op_id: col(row, 14, "operation.restores_op_id")?,
        reverts_op_id: col(row, 15, "operation.reverts_op_id")?,
        predecessor_map_oid: col(row, 16, "operation.predecessor_map_oid")?,
        causal_context_id: col(row, 17, "operation.causal_context_id")?,
        restorable: restorable != 0,
        control_slot: col(row, 19, "operation.control_slot")?,
        claim_owner: col(row, 20, "operation.claim_owner")?,
        start_ts: col(row, 21, "operation.start_ts")?,
        end_ts: col(row, 22, "operation.end_ts")?,
    })
}

fn decode_operation_journal(row: &QueryResult) -> StorageResult<OperationJournalRecord> {
    Ok(OperationJournalRecord {
        journal_id: col(row, 0, "operation_journal.journal_id")?,
        op_id: col(row, 1, "operation_journal.op_id")?,
        phase: col(row, 2, "operation_journal.phase")?,
        pre_view_oid: col(row, 3, "operation_journal.pre_view_oid")?,
        target_view_oid: col(row, 4, "operation_journal.target_view_oid")?,
        owner: col(row, 5, "operation_journal.owner")?,
        updated_at: col(row, 6, "operation_journal.updated_at")?,
        recovery_payload: col(row, 7, "operation_journal.recovery_payload")?,
    })
}

fn decode_change_identity(row: &QueryResult) -> StorageResult<ChangeIdentityRecord> {
    Ok(ChangeIdentityRecord {
        change_id: col(row, 0, "change_identity.change_id")?,
        repo_id: col(row, 1, "change_identity.repo_id")?,
        origin: col(row, 2, "change_identity.origin")?,
        created_op_id: col(row, 3, "change_identity.created_op_id")?,
        created_at: col(row, 4, "change_identity.created_at")?,
    })
}

fn decode_change_revision(row: &QueryResult) -> StorageResult<ChangeRevisionRecord> {
    Ok(ChangeRevisionRecord {
        change_id: col(row, 0, "change_revision.change_id")?,
        commit_oid: col(row, 1, "change_revision.commit_oid")?,
        created_op_id: col(row, 2, "change_revision.created_op_id")?,
        visibility: col(row, 3, "change_revision.visibility")?,
        revision_ordinal: col(row, 4, "change_revision.revision_ordinal")?,
    })
}

fn decode_change_predecessor(row: &QueryResult) -> StorageResult<ChangePredecessorRecord> {
    Ok(ChangePredecessorRecord {
        successor_oid: col(row, 0, "change_predecessor.successor_oid")?,
        predecessor_oid: col(row, 1, "change_predecessor.predecessor_oid")?,
        op_id: col(row, 2, "change_predecessor.op_id")?,
        relation_kind: col(row, 3, "change_predecessor.relation_kind")?,
        ordinal: col(row, 4, "change_predecessor.ordinal")?,
    })
}

fn decode_ai_operation_link(row: &QueryResult) -> StorageResult<AiOperationLinkRecord> {
    Ok(AiOperationLinkRecord {
        operation_id: col(row, 0, "ai_operation_link.operation_id")?,
        session_id: col(row, 1, "ai_operation_link.session_id")?,
        run_id: col(row, 2, "ai_operation_link.run_id")?,
        tool_invocation_id: col(row, 3, "ai_operation_link.tool_invocation_id")?,
        intent_id: col(row, 4, "ai_operation_link.intent_id")?,
        repo_id: col(row, 5, "ai_operation_link.repo_id")?,
        worktree_id: col(row, 6, "ai_operation_link.worktree_id")?,
        workspace_id: col(row, 7, "ai_operation_link.workspace_id")?,
        lease_generation: col(row, 8, "ai_operation_link.lease_generation")?,
        config_provenance_digest: col(row, 9, "ai_operation_link.config_provenance_digest")?,
        redaction_version: col(row, 10, "ai_operation_link.redaction_version")?,
        change_id: col(row, 11, "ai_operation_link.change_id")?,
    })
}

fn decode_working_dirty_entry(row: &QueryResult) -> StorageResult<WorkingDirtyEntryRecord> {
    Ok(WorkingDirtyEntryRecord {
        worktree_id: col(row, 0, "working_dirty.worktree_id")?,
        path: col(row, 1, "working_dirty.path")?,
        kind: col(row, 2, "working_dirty.kind")?,
        source: col(row, 3, "working_dirty.source")?,
        marked_at: col(row, 4, "working_dirty.marked_at")?,
        verified_at: col(row, 5, "working_dirty.verified_at")?,
    })
}

fn decode_working_dirty_meta(row: &QueryResult) -> StorageResult<WorkingDirtyMetaRecord> {
    Ok(WorkingDirtyMetaRecord {
        worktree_id: col(row, 0, "working_dirty_meta.worktree_id")?,
        state: col(row, 1, "working_dirty_meta.state")?,
        index_fingerprint: col(row, 2, "working_dirty_meta.index_fingerprint")?,
        head_oid: col(row, 3, "working_dirty_meta.head_oid")?,
        scanned_at: col(row, 4, "working_dirty_meta.scanned_at")?,
        scan_lock_pid: col(row, 5, "working_dirty_meta.scan_lock_pid")?,
        scan_lock_at: col(row, 6, "working_dirty_meta.scan_lock_at")?,
    })
}

fn decode_layer_record(row: &QueryResult) -> StorageResult<LayerRecord> {
    let enabled: i64 = col(row, 4, "layer.enabled")?;
    Ok(LayerRecord {
        worktree_id: col(row, 0, "layer.worktree_id")?,
        name: col(row, 1, "layer.name")?,
        source: col(row, 2, "layer.source")?,
        priority: col(row, 3, "layer.priority")?,
        enabled: enabled != 0,
        created_at: col(row, 5, "layer.created_at")?,
        updated_at: col(row, 6, "layer.updated_at")?,
    })
}

fn decode_layer_path_record(row: &QueryResult) -> StorageResult<LayerPathRecord> {
    Ok(LayerPathRecord {
        worktree_id: col(row, 0, "layer_path.worktree_id")?,
        layer_name: col(row, 1, "layer_path.layer_name")?,
        path: col(row, 2, "layer_path.path")?,
        content_hash: col(row, 3, "layer_path.content_hash")?,
        materialized_at: col(row, 4, "layer_path.materialized_at")?,
    })
}

const WORKSPACE_SELECT_COLS: &str = "workspace_id, repo_id, kind, worktree_id, path, owner_kind, \
    owner_id, task_id, session_id, base_commit, branch, state, lease_owner, lease_fence, \
    lease_expires_at, created_at, updated_at";

const OPERATION_SELECT_COLS: &str = "op_id, repo_id, format_version, kind, status, command_name, \
    description, args_digest, actor, worktree_id, scope_kind, scope_provenance, pre_view_oid, \
    post_view_oid, restores_op_id, reverts_op_id, predecessor_map_oid, causal_context_id, \
    restorable, control_slot, claim_owner, start_ts, end_ts";

async fn query_workspace_in_conn<C: ConnectionTrait>(
    conn: &C,
    workspace_id: &str,
) -> StorageResult<Option<WorkspaceRecordRow>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            format!("SELECT {WORKSPACE_SELECT_COLS} FROM workspace_record WHERE workspace_id = ?"),
            [workspace_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "query workspace_record", err)
        })?;
    row.as_ref().map(decode_workspace_record).transpose()
}

async fn query_operation_heads_in_conn<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    scope_key: &str,
) -> StorageResult<OperationHeadsSnapshot> {
    let backend = conn.get_database_backend();
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            backend,
            "SELECT op_id, generation FROM operation_head \
             WHERE repo_id = ? AND scope_key = ? ORDER BY op_id ASC",
            [repo_id.into(), scope_key.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "query operation_head", err))?;
    let mut generation = 0u64;
    let mut heads = Vec::new();
    for row in rows {
        let op_id: String = col(&row, 0, "operation_head.op_id")?;
        let gen_i64: i64 = col(&row, 1, "operation_head.generation")?;
        let gen_u64 = u64::try_from(gen_i64).map_err(|_| StorageBackendError::BackendInternal {
            detail: "operation_head generation was negative".to_string(),
        })?;
        if gen_u64 > generation {
            generation = gen_u64;
        }
        if op_id != OP_HEAD_GENERATION_SENTINEL {
            heads.push(op_id);
        }
    }
    Ok(OperationHeadsSnapshot { generation, heads })
}

async fn replace_operation_heads_in_conn<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    scope_key: &str,
    generation: u64,
    heads: &[String],
) -> StorageResult<()> {
    let backend = conn.get_database_backend();
    let gen_i64 = i64::try_from(generation).map_err(|_| StorageBackendError::BackendInternal {
        detail: "operation_head generation overflowed i64".to_string(),
    })?;
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "DELETE FROM operation_head WHERE repo_id = ? AND scope_key = ?",
        [repo_id.into(), scope_key.into()],
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "clear operation_head", err))?;

    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO operation_head (repo_id, scope_key, op_id, generation) VALUES (?, ?, ?, ?)",
        [
            repo_id.into(),
            scope_key.into(),
            OP_HEAD_GENERATION_SENTINEL.into(),
            gen_i64.into(),
        ],
    ))
    .await
    .map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "insert operation_head sentinel",
            err,
        )
    })?;

    for head in heads {
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO operation_head (repo_id, scope_key, op_id, generation) VALUES (?, ?, ?, ?)",
            [
                repo_id.into(),
                scope_key.into(),
                head.clone().into(),
                gen_i64.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "insert operation_head", err))?;
    }
    Ok(())
}

#[async_trait]
impl GitStateStore for SqliteGitStateStore {
    fn backend(&self) -> StorageBackendKind {
        StorageBackendKind::Sqlite
    }

    async fn get_sequence_state(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<SequenceStateRecord>> {
        validate_no_nul(worktree_id, "sequence_state worktree_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, kind, head_name, head_orig, current_oid, todo, payload, updated_at \
                 FROM sequence_state WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "get sequence_state", err))?;
        row.as_ref().map(decode_sequence_state).transpose()
    }

    async fn put_sequence_state(&self, record: &SequenceStateRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_sequence_state(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin put_sequence_state", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO sequence_state \
             (worktree_id, kind, head_name, head_orig, current_oid, todo, payload, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id) DO UPDATE SET \
               kind = excluded.kind, head_name = excluded.head_name, \
               head_orig = excluded.head_orig, current_oid = excluded.current_oid, \
               todo = excluded.todo, payload = excluded.payload, updated_at = excluded.updated_at",
            [
                record.worktree_id.clone().into(),
                record.kind.clone().into(),
                record.head_name.clone().into(),
                record.head_orig.clone().into(),
                record.current_oid.clone().into(),
                record.todo.clone().into(),
                record.payload.clone().into(),
                record.updated_at.clone().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "upsert sequence_state", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit put_sequence_state", err)
        })
    }

    async fn delete_sequence_state(&self, worktree_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_no_nul(worktree_id, "sequence_state worktree_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin delete_sequence_state", err)
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM sequence_state WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "delete sequence_state", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit delete_sequence_state",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_sequence_states(&self) -> StorageResult<Vec<SequenceStateRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT worktree_id, kind, head_name, head_orig, current_oid, todo, payload, updated_at \
                 FROM sequence_state ORDER BY worktree_id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list sequence_state", err)
            })?;
        rows.iter().map(decode_sequence_state).collect()
    }

    async fn get_rebase_state(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<RebaseStateRecord>> {
        validate_no_nul(worktree_id, "rebase_state worktree_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, head_name, onto, orig_head, current_head, todo, \
                        todo_actions, done, stopped_sha, autosquash, empty_mode \
                 FROM rebase_state WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "get rebase_state", err))?;
        row.as_ref().map(decode_rebase_state).transpose()
    }

    async fn put_rebase_state(&self, record: &RebaseStateRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_rebase_state(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin put_rebase_state", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO rebase_state \
             (worktree_id, head_name, onto, orig_head, current_head, todo, todo_actions, done, \
              stopped_sha, autosquash, empty_mode) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id) DO UPDATE SET \
               head_name = excluded.head_name, onto = excluded.onto, \
               orig_head = excluded.orig_head, current_head = excluded.current_head, \
               todo = excluded.todo, todo_actions = excluded.todo_actions, \
               done = excluded.done, stopped_sha = excluded.stopped_sha, \
               autosquash = excluded.autosquash, empty_mode = excluded.empty_mode",
            [
                record.worktree_id.clone().into(),
                record.head_name.clone().into(),
                record.onto.clone().into(),
                record.orig_head.clone().into(),
                record.current_head.clone().into(),
                record.todo.clone().into(),
                record.todo_actions.clone().into(),
                record.done.clone().into(),
                record.stopped_sha.clone().into(),
                i64::from(record.autosquash).into(),
                record.empty_mode.clone().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "upsert rebase_state", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit put_rebase_state", err)
        })
    }

    async fn delete_rebase_state(&self, worktree_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_no_nul(worktree_id, "rebase_state worktree_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin delete_rebase_state", err)
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM rebase_state WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "delete rebase_state", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit delete_rebase_state", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_rebase_states(&self) -> StorageResult<Vec<RebaseStateRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT worktree_id, head_name, onto, orig_head, current_head, todo, \
                        todo_actions, done, stopped_sha, autosquash, empty_mode \
                 FROM rebase_state ORDER BY worktree_id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "list rebase_state", err))?;
        rows.iter().map(decode_rebase_state).collect()
    }

    async fn get_bisect_state(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<BisectStateRecord>> {
        validate_no_nul(worktree_id, "bisect_state worktree_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, orig_head, orig_head_name, bad, good, current, skipped, \
                        steps, completed, first_parent \
                 FROM bisect_state WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "get bisect_state", err))?;
        row.as_ref().map(decode_bisect_state).transpose()
    }

    async fn put_bisect_state(&self, record: &BisectStateRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_bisect_state(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin put_bisect_state", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO bisect_state \
             (worktree_id, orig_head, orig_head_name, bad, good, current, skipped, steps, \
              completed, first_parent) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id) DO UPDATE SET \
               orig_head = excluded.orig_head, orig_head_name = excluded.orig_head_name, \
               bad = excluded.bad, good = excluded.good, current = excluded.current, \
               skipped = excluded.skipped, steps = excluded.steps, \
               completed = excluded.completed, first_parent = excluded.first_parent",
            [
                record.worktree_id.clone().into(),
                record.orig_head.clone().into(),
                record.orig_head_name.clone().into(),
                record.bad.clone().into(),
                record.good.clone().into(),
                record.current.clone().into(),
                record.skipped.clone().into(),
                record.steps.into(),
                i64::from(record.completed).into(),
                i64::from(record.first_parent).into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "upsert bisect_state", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit put_bisect_state", err)
        })
    }

    async fn delete_bisect_state(&self, worktree_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_no_nul(worktree_id, "bisect_state worktree_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin delete_bisect_state", err)
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM bisect_state WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "delete bisect_state", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit delete_bisect_state", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_bisect_states(&self) -> StorageResult<Vec<BisectStateRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT worktree_id, orig_head, orig_head_name, bad, good, current, skipped, \
                        steps, completed, first_parent \
                 FROM bisect_state ORDER BY worktree_id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "list bisect_state", err))?;
        rows.iter().map(decode_bisect_state).collect()
    }

    async fn record_worktree_registry_capability(&self, version: i64) -> StorageResult<()> {
        self.ensure_writable()?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin record_worktree_registry_capability",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT OR IGNORE INTO worktree_registry_capability (version) VALUES (?)",
            [version.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "insert worktree_registry_capability",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit record_worktree_registry_capability",
                err,
            )
        })
    }

    async fn list_worktree_registry_capabilities(&self) -> StorageResult<Vec<i64>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT version FROM worktree_registry_capability ORDER BY version ASC".to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list worktree_registry_capability",
                    err,
                )
            })?;
        rows.iter()
            .map(|r| col(r, 0, "worktree_registry_capability.version"))
            .collect()
    }

    async fn upsert_worktree_lifecycle(
        &self,
        record: &WorktreeLifecycleRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_worktree_lifecycle(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_worktree_lifecycle",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO worktree_lifecycle \
             (worktree_id, state, path, reason, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id) DO UPDATE SET \
               state = excluded.state, path = excluded.path, reason = excluded.reason, \
               updated_at = excluded.updated_at",
            [
                record.worktree_id.clone().into(),
                record.state.as_str().into(),
                record.path.clone().into(),
                record.reason.clone().into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert worktree_lifecycle", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_worktree_lifecycle",
                err,
            )
        })
    }

    async fn get_worktree_lifecycle(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<WorktreeLifecycleRecord>> {
        validate_non_empty_identifier(worktree_id, "worktree_lifecycle worktree_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, state, path, reason, created_at, updated_at \
                 FROM worktree_lifecycle WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get worktree_lifecycle", err)
            })?;
        row.as_ref().map(decode_worktree_lifecycle).transpose()
    }

    async fn delete_worktree_lifecycle(&self, worktree_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(worktree_id, "worktree_lifecycle worktree_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin delete_worktree_lifecycle",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM worktree_lifecycle WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "delete worktree_lifecycle", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit delete_worktree_lifecycle",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_worktree_lifecycles(&self) -> StorageResult<Vec<WorktreeLifecycleRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT worktree_id, state, path, reason, created_at, updated_at \
                 FROM worktree_lifecycle ORDER BY worktree_id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list worktree_lifecycle", err)
            })?;
        rows.iter().map(decode_worktree_lifecycle).collect()
    }

    async fn append_worktree_intent(
        &self,
        intent: &WorktreeIntentWrite,
    ) -> StorageResult<WorktreeIntentRecord> {
        self.ensure_writable()?;
        validate_worktree_intent_write(intent)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin append_worktree_intent",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO worktree_intent_journal (op, worktree_id, payload, stage, created_at) \
                 VALUES (?, ?, ?, ?, ?)",
                [
                    intent.op.as_str().into(),
                    intent.worktree_id.clone().into(),
                    intent.payload.clone().into(),
                    intent.stage.clone().into(),
                    intent.created_at.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "insert worktree_intent_journal",
                    err,
                )
            })?;
        let id = i64::try_from(res.last_insert_id()).map_err(|_| {
            StorageBackendError::BackendInternal {
                detail: "worktree_intent_journal last_insert_id overflowed i64".to_string(),
            }
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit append_worktree_intent",
                err,
            )
        })?;
        Ok(WorktreeIntentRecord {
            id,
            op: intent.op,
            worktree_id: intent.worktree_id.clone(),
            payload: intent.payload.clone(),
            stage: intent.stage.clone(),
            created_at: intent.created_at,
        })
    }

    async fn update_worktree_intent_stage(
        &self,
        id: i64,
        stage: Option<&str>,
    ) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_opt_no_nul(stage, "worktree_intent_journal stage")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin update_worktree_intent_stage",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE worktree_intent_journal SET stage = ? WHERE id = ?",
                [stage.map(str::to_string).into(), id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "update worktree_intent_journal stage",
                    err,
                )
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit update_worktree_intent_stage",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn resolve_worktree_intent(&self, id: i64) -> StorageResult<bool> {
        self.ensure_writable()?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin resolve_worktree_intent",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM worktree_intent_journal WHERE id = ?",
                [id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "delete worktree_intent_journal",
                    err,
                )
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit resolve_worktree_intent",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_worktree_intents(&self) -> StorageResult<Vec<WorktreeIntentRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT id, op, worktree_id, payload, stage, created_at \
                 FROM worktree_intent_journal ORDER BY id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list worktree_intent_journal",
                    err,
                )
            })?;
        rows.iter().map(decode_worktree_intent).collect()
    }

    async fn acquire_workspace_lease(
        &self,
        record: &WorkspaceRecordRow,
    ) -> StorageResult<WorkspaceLeaseAcquireOutcome> {
        self.ensure_writable()?;
        validate_workspace_record(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin acquire_workspace_lease",
                err,
            )
        })?;
        let backend = txn.get_database_backend();

        if query_workspace_in_conn(&txn, &record.workspace_id)
            .await?
            .is_some()
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "workspace_record workspace_id already exists".to_string(),
            });
        }

        if record.state.holds_identity() {
            if record.kind == WorkspaceRecordKind::Linked
                && let Some(wt) = record.worktree_id.as_deref()
                && let Some(holder) = txn
                    .query_one_raw(Statement::from_sql_and_values(
                        backend,
                        "SELECT workspace_id FROM workspace_record \
                         WHERE repo_id = ? AND kind = 'linked' AND worktree_id = ? \
                           AND state IN ('provisioning', 'active', 'releasing') LIMIT 1",
                        [record.repo_id.clone().into(), wt.into()],
                    ))
                    .await
                    .map_err(|err| {
                        map_sqlite_db_err(
                            DatabaseRole::Repository,
                            "check workspace linked live identity",
                            err,
                        )
                    })?
            {
                let holder_id: String = col(&holder, 0, "workspace_record.workspace_id")?;
                drop(txn);
                return Ok(WorkspaceLeaseAcquireOutcome::Conflict {
                    conflicting_workspace_id: holder_id,
                    path_conflict: false,
                });
            }

            if let Some(holder) = txn
                .query_one_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT workspace_id FROM workspace_record \
                     WHERE repo_id = ? AND path = ? \
                       AND state IN ('provisioning', 'active', 'releasing') LIMIT 1",
                    [record.repo_id.clone().into(), record.path.clone().into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "check workspace live path uniqueness",
                        err,
                    )
                })?
            {
                let holder_id: String = col(&holder, 0, "workspace_record.workspace_id")?;
                drop(txn);
                return Ok(WorkspaceLeaseAcquireOutcome::Conflict {
                    conflicting_workspace_id: holder_id,
                    path_conflict: true,
                });
            }
        }

        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO workspace_record \
             (workspace_id, repo_id, kind, worktree_id, path, owner_kind, owner_id, task_id, \
              session_id, base_commit, branch, state, lease_owner, lease_fence, \
              lease_expires_at, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                record.workspace_id.clone().into(),
                record.repo_id.clone().into(),
                record.kind.as_str().into(),
                record.worktree_id.clone().into(),
                record.path.clone().into(),
                record.owner_kind.as_str().into(),
                record.owner_id.clone().into(),
                record.task_id.clone().into(),
                record.session_id.clone().into(),
                record.base_commit.clone().into(),
                record.branch.clone().into(),
                record.state.as_str().into(),
                record.lease_owner.clone().into(),
                record.lease_fence.into(),
                record.lease_expires_at.into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "insert workspace_record", err)
        })?;

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit acquire_workspace_lease",
                err,
            )
        })?;
        Ok(WorkspaceLeaseAcquireOutcome::Acquired(Box::new(
            record.clone(),
        )))
    }

    async fn renew_workspace_lease(
        &self,
        workspace_id: &str,
        expected_owner: &str,
        expected_fence: i64,
        new_expires_at: Option<i64>,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome> {
        self.ensure_writable()?;
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        validate_non_empty_identifier(expected_owner, "workspace_record lease_owner")?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin renew_workspace_lease", err)
        })?;
        let Some(mut current) = query_workspace_in_conn(&txn, workspace_id).await? else {
            drop(txn);
            return Ok(WorkspaceLeaseMutationOutcome::NotFound);
        };
        if !current.state.holds_identity()
            || current.lease_owner.as_deref() != Some(expected_owner)
            || current.lease_fence != expected_fence
        {
            drop(txn);
            return Ok(WorkspaceLeaseMutationOutcome::LeaseLost {
                actual_state: current.state,
                actual_owner: current.lease_owner,
                actual_fence: current.lease_fence,
            });
        }
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE workspace_record SET lease_expires_at = ?, updated_at = ? \
             WHERE workspace_id = ? AND lease_owner = ? AND lease_fence = ?",
            [
                new_expires_at.into(),
                updated_at.into(),
                workspace_id.into(),
                expected_owner.into(),
                expected_fence.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "renew workspace lease", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit renew_workspace_lease",
                err,
            )
        })?;
        current.lease_expires_at = new_expires_at;
        current.updated_at = updated_at;
        Ok(WorkspaceLeaseMutationOutcome::Updated(Box::new(current)))
    }

    async fn transition_workspace_lease(
        &self,
        workspace_id: &str,
        expected_owner: &str,
        expected_fence: i64,
        new_state: WorkspaceRecordState,
        updated_at: i64,
    ) -> StorageResult<WorkspaceLeaseMutationOutcome> {
        self.ensure_writable()?;
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        validate_non_empty_identifier(expected_owner, "workspace_record lease_owner")?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin transition_workspace_lease",
                err,
            )
        })?;
        let Some(mut current) = query_workspace_in_conn(&txn, workspace_id).await? else {
            drop(txn);
            return Ok(WorkspaceLeaseMutationOutcome::NotFound);
        };
        if !current.state.holds_identity()
            || current.lease_owner.as_deref() != Some(expected_owner)
            || current.lease_fence != expected_fence
        {
            drop(txn);
            return Ok(WorkspaceLeaseMutationOutcome::LeaseLost {
                actual_state: current.state,
                actual_owner: current.lease_owner,
                actual_fence: current.lease_fence,
            });
        }
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE workspace_record SET state = ?, updated_at = ? \
             WHERE workspace_id = ? AND lease_owner = ? AND lease_fence = ?",
            [
                new_state.as_str().into(),
                updated_at.into(),
                workspace_id.into(),
                expected_owner.into(),
                expected_fence.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "transition workspace lease", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit transition_workspace_lease",
                err,
            )
        })?;
        current.state = new_state;
        current.updated_at = updated_at;
        Ok(WorkspaceLeaseMutationOutcome::Updated(Box::new(current)))
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
        self.ensure_writable()?;
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        validate_non_empty_identifier(new_owner, "workspace_record new_owner")?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin reclaim_expired_workspace_lease",
                err,
            )
        })?;
        let Some(mut current) = query_workspace_in_conn(&txn, workspace_id).await? else {
            drop(txn);
            return Ok(WorkspaceLeaseMutationOutcome::NotFound);
        };
        let is_expired = current.lease_expires_at.is_some_and(|exp| exp <= now_ts);
        if !current.state.holds_identity() || !is_expired {
            drop(txn);
            return Ok(WorkspaceLeaseMutationOutcome::LeaseLost {
                actual_state: current.state,
                actual_owner: current.lease_owner,
                actual_fence: current.lease_fence,
            });
        }
        let next_fence = current.lease_fence.checked_add(1).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: "workspace_record lease_fence overflowed i64".to_string(),
            }
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE workspace_record \
             SET state = ?, lease_owner = ?, lease_fence = ?, lease_expires_at = ?, updated_at = ? \
             WHERE workspace_id = ? AND lease_fence = ?",
            [
                new_state.as_str().into(),
                new_owner.into(),
                next_fence.into(),
                new_expires_at.into(),
                updated_at.into(),
                workspace_id.into(),
                current.lease_fence.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "reclaim expired workspace lease",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit reclaim_expired_workspace_lease",
                err,
            )
        })?;
        current.state = new_state;
        current.lease_owner = Some(new_owner.to_string());
        current.lease_fence = next_fence;
        current.lease_expires_at = new_expires_at;
        current.updated_at = updated_at;
        Ok(WorkspaceLeaseMutationOutcome::Updated(Box::new(current)))
    }

    async fn get_workspace_record(
        &self,
        workspace_id: &str,
    ) -> StorageResult<Option<WorkspaceRecordRow>> {
        validate_non_empty_identifier(workspace_id, "workspace_record workspace_id")?;
        query_workspace_in_conn(&self.conn, workspace_id).await
    }

    async fn list_workspace_records(
        &self,
        repo_id: &str,
        state_filter: Option<WorkspaceRecordState>,
    ) -> StorageResult<Vec<WorkspaceRecordRow>> {
        validate_non_empty_identifier(repo_id, "workspace_record repo_id")?;
        let backend = self.conn.get_database_backend();
        let stmt = match state_filter {
            Some(state) => Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT {WORKSPACE_SELECT_COLS} FROM workspace_record \
                     WHERE repo_id = ? AND state = ? ORDER BY workspace_id ASC"
                ),
                [repo_id.into(), state.as_str().into()],
            ),
            None => Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT {WORKSPACE_SELECT_COLS} FROM workspace_record \
                     WHERE repo_id = ? ORDER BY workspace_id ASC"
                ),
                [repo_id.into()],
            ),
        };
        let rows = self.conn.query_all_raw(stmt).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "list workspace_record", err)
        })?;
        rows.iter().map(decode_workspace_record).collect()
    }

    async fn append_operation_atomic(
        &self,
        op: &OperationRecord,
        parent_op_ids: &[String],
        journal: Option<&OperationJournalRecord>,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_operation_record(op, parent_op_ids)?;
        if let Some(jrn) = journal {
            validate_operation_journal(jrn)?;
            if jrn.op_id != op.op_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "operation_journal op_id must match operation op_id".to_string(),
                });
            }
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin append_operation_atomic",
                err,
            )
        })?;
        insert_operation_records_in_txn(&txn, op, parent_op_ids, journal).await?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit append_operation_atomic",
                err,
            )
        })
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
        self.ensure_writable()?;
        validate_operation_record(op, parent_op_ids)?;
        if scope_key.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation_head scope_key must not contain NUL bytes".to_string(),
            });
        }
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

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin append_operation_and_converge_heads",
                err,
            )
        })?;
        insert_operation_records_in_txn(&txn, op, parent_op_ids, journal).await?;
        let outcome =
            converge_operation_heads_in_txn(&txn, &op.repo_id, scope_key, &expected, &replacement)
                .await?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit append_operation_and_converge_heads",
                err,
            )
        })?;
        Ok(outcome)
    }

    async fn update_operation_status(
        &self,
        repo_id: &str,
        op_id: &str,
        status: &str,
        post_view_oid: Option<&str>,
        end_ts: Option<i64>,
    ) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        validate_non_empty_identifier(op_id, "operation op_id")?;
        validate_non_empty_identifier(status, "operation status")?;
        validate_opt_hex_oid(post_view_oid, "operation post_view_oid")?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin update_operation_status",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = match post_view_oid {
            Some(oid) => txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE operation SET status = ?, post_view_oid = ?, end_ts = ? \
                     WHERE repo_id = ? AND op_id = ?",
                    [
                        status.into(),
                        oid.into(),
                        end_ts.into(),
                        repo_id.into(),
                        op_id.into(),
                    ],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "update operation status", err)
                })?,
            None => txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE operation SET status = ?, end_ts = ? WHERE repo_id = ? AND op_id = ?",
                    [status.into(), end_ts.into(), repo_id.into(), op_id.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "update operation status", err)
                })?,
        };
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit update_operation_status",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn update_operation_args_digest(
        &self,
        repo_id: &str,
        op_id: &str,
        args_digest: &str,
    ) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        validate_non_empty_identifier(op_id, "operation op_id")?;
        validate_no_nul(args_digest, "operation args_digest")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin update_operation_args_digest",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE operation SET args_digest = ? WHERE repo_id = ? AND op_id = ?",
                [args_digest.into(), repo_id.into(), op_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "update operation args_digest",
                    err,
                )
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit update_operation_args_digest",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn delete_operation(&self, repo_id: &str, op_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        validate_non_empty_identifier(op_id, "operation op_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin delete_operation", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM operation_parent WHERE op_id = ?",
            [op_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "delete operation_parent", err)
        })?;
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM operation WHERE repo_id = ? AND op_id = ?",
                [repo_id.into(), op_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "delete operation", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit delete_operation", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn get_operation(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> StorageResult<Option<(OperationRecord, Vec<String>)>> {
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        validate_non_empty_identifier(op_id, "operation op_id")?;
        let txn = self.conn.begin().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin get_operation snapshot",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let Some(row) = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT {OPERATION_SELECT_COLS} FROM operation WHERE repo_id = ? AND op_id = ?"
                ),
                [repo_id.into(), op_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "get operation", err))?
        else {
            return Ok(None);
        };
        let op = decode_operation_record(&row)?;
        let parent_rows = txn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT parent_op_id FROM operation_parent WHERE op_id = ? ORDER BY ordinal ASC",
                [op_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list operation_parent", err)
            })?;
        let parents = parent_rows
            .iter()
            .map(|r| col(r, 0, "operation_parent.parent_op_id"))
            .collect::<StorageResult<Vec<String>>>()?;
        Ok(Some((op, parents)))
    }

    async fn list_operations(
        &self,
        repo_id: &str,
    ) -> StorageResult<Vec<(OperationRecord, Vec<String>)>> {
        validate_non_empty_identifier(repo_id, "operation repo_id")?;
        let txn = self.conn.begin().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin list_operations snapshot",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let op_rows = txn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT {OPERATION_SELECT_COLS} FROM operation \
                     WHERE repo_id = ? ORDER BY start_ts ASC, op_id ASC"
                ),
                [repo_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "list operation", err))?;

        let parent_rows = txn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT p.op_id, p.parent_op_id FROM operation_parent p \
                 JOIN operation o ON o.op_id = p.op_id \
                 WHERE o.repo_id = ? ORDER BY p.op_id ASC, p.ordinal ASC",
                [repo_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list operation_parent bulk", err)
            })?;
        let mut parents_map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for row in parent_rows {
            let op_id: String = col(&row, 0, "operation_parent.op_id")?;
            let parent_id: String = col(&row, 1, "operation_parent.parent_op_id")?;
            parents_map.entry(op_id).or_default().push(parent_id);
        }

        let mut out = Vec::with_capacity(op_rows.len());
        for row in op_rows {
            let op = decode_operation_record(&row)?;
            let parents = parents_map.remove(&op.op_id).unwrap_or_default();
            out.push((op, parents));
        }
        Ok(out)
    }

    async fn upsert_operation_journal(&self, entry: &OperationJournalRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_operation_journal(entry)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_operation_journal",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO operation_journal \
             (journal_id, op_id, phase, pre_view_oid, target_view_oid, owner, updated_at, recovery_payload) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(journal_id) DO UPDATE SET \
               op_id = excluded.op_id, phase = excluded.phase, \
               pre_view_oid = excluded.pre_view_oid, target_view_oid = excluded.target_view_oid, \
               owner = excluded.owner, updated_at = excluded.updated_at, \
               recovery_payload = excluded.recovery_payload",
            [
                entry.journal_id.clone().into(),
                entry.op_id.clone().into(),
                entry.phase.clone().into(),
                entry.pre_view_oid.clone().into(),
                entry.target_view_oid.clone().into(),
                entry.owner.clone().into(),
                entry.updated_at.into(),
                entry.recovery_payload.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert operation_journal", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_operation_journal",
                err,
            )
        })
    }

    async fn list_operation_journals(
        &self,
        op_id: &str,
    ) -> StorageResult<Vec<OperationJournalRecord>> {
        validate_non_empty_identifier(op_id, "operation_journal op_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT journal_id, op_id, phase, pre_view_oid, target_view_oid, owner, \
                        updated_at, recovery_payload \
                 FROM operation_journal WHERE op_id = ? \
                 ORDER BY updated_at DESC, journal_id ASC",
                [op_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list operation_journal", err)
            })?;
        rows.iter().map(decode_operation_journal).collect()
    }

    async fn list_all_operation_journals(&self) -> StorageResult<Vec<OperationJournalRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT journal_id, op_id, phase, pre_view_oid, target_view_oid, owner, \
                        updated_at, recovery_payload \
                 FROM operation_journal ORDER BY updated_at ASC, journal_id ASC",
                [],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list all operation_journal", err)
            })?;
        rows.iter().map(decode_operation_journal).collect()
    }

    async fn delete_operation_journal(&self, journal_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(journal_id, "operation_journal journal_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin delete_operation_journal",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM operation_journal WHERE journal_id = ?",
                [journal_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "delete operation_journal", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit delete_operation_journal",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn list_all_operation_heads(&self, repo_id: &str) -> StorageResult<Vec<String>> {
        validate_non_empty_identifier(repo_id, "operation_head repo_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT DISTINCT op_id FROM operation_head \
                 WHERE repo_id = ? AND op_id <> ? ORDER BY op_id ASC",
                [repo_id.into(), OP_HEAD_GENERATION_SENTINEL.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list all operation_head", err)
            })?;
        rows.iter()
            .map(|row| col(row, 0, "operation_head.op_id"))
            .collect()
    }

    async fn get_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
    ) -> StorageResult<OperationHeadsSnapshot> {
        validate_non_empty_identifier(repo_id, "operation_head repo_id")?;
        if scope_key.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation_head scope_key must not contain NUL bytes".to_string(),
            });
        }
        query_operation_heads_in_conn(&self.conn, repo_id, scope_key).await
    }

    async fn cas_update_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_generation: Option<u64>,
        expected_heads: &[String],
        new_heads: &[String],
    ) -> StorageResult<OperationHeadCasOutcome> {
        self.ensure_writable()?;
        validate_non_empty_identifier(repo_id, "operation_head repo_id")?;
        if scope_key.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation_head scope_key must not contain NUL bytes".to_string(),
            });
        }
        let expected = normalize_op_heads(expected_heads)?;
        let replacement = normalize_op_heads(new_heads)?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin cas_update_operation_heads",
                err,
            )
        })?;
        let current = query_operation_heads_in_conn(&txn, repo_id, scope_key).await?;
        if current.heads != expected
            || expected_generation.is_some_and(|exp| exp != current.generation)
        {
            drop(txn);
            return Ok(OperationHeadCasOutcome::CasConflict {
                actual_generation: current.generation,
                actual_heads: current.heads,
            });
        }
        let next_gen = current.generation.checked_add(1).ok_or_else(|| {
            StorageBackendError::BackendInternal {
                detail: "operation_head generation overflowed u64".to_string(),
            }
        })?;
        replace_operation_heads_in_conn(&txn, repo_id, scope_key, next_gen, &replacement).await?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit cas_update_operation_heads",
                err,
            )
        })?;
        Ok(OperationHeadCasOutcome::Applied {
            generation: next_gen,
            heads: replacement,
        })
    }

    async fn converge_operation_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_heads: &[String],
        replacement_heads: &[String],
    ) -> StorageResult<OperationHeadConvergeOutcome> {
        self.ensure_writable()?;
        validate_non_empty_identifier(repo_id, "operation_head repo_id")?;
        if scope_key.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "operation_head scope_key must not contain NUL bytes".to_string(),
            });
        }
        let expected = normalize_op_heads(expected_heads)?;
        let replacement = normalize_op_heads(replacement_heads)?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin converge_operation_heads",
                err,
            )
        })?;
        let outcome =
            converge_operation_heads_in_txn(&txn, repo_id, scope_key, &expected, &replacement)
                .await?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit converge_operation_heads",
                err,
            )
        })?;
        Ok(outcome)
    }

    async fn upsert_change_identity(&self, record: &ChangeIdentityRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_change_identity(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_change_identity",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO change_identity (change_id, repo_id, origin, created_op_id, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(change_id) DO UPDATE SET \
               repo_id = excluded.repo_id, origin = excluded.origin, \
               created_op_id = excluded.created_op_id, created_at = excluded.created_at",
            [
                record.change_id.clone().into(),
                record.repo_id.clone().into(),
                record.origin.clone().into(),
                record.created_op_id.clone().into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert change_identity", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_change_identity",
                err,
            )
        })
    }

    async fn get_change_identity(
        &self,
        change_id: &str,
    ) -> StorageResult<Option<ChangeIdentityRecord>> {
        validate_non_empty_identifier(change_id, "change_identity change_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT change_id, repo_id, origin, created_op_id, created_at \
                 FROM change_identity WHERE change_id = ?",
                [change_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get change_identity", err)
            })?;
        row.as_ref().map(decode_change_identity).transpose()
    }

    async fn list_change_identities_by_prefix(
        &self,
        repo_id: &str,
        prefix: &str,
    ) -> StorageResult<Vec<ChangeIdentityRecord>> {
        validate_non_empty_identifier(repo_id, "change_identity repo_id")?;
        validate_no_nul(prefix, "change_identity prefix")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT change_id, repo_id, origin, created_op_id, created_at \
                 FROM change_identity WHERE repo_id = ? ORDER BY change_id ASC",
                [repo_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list change_identity", err)
            })?;
        let mut out = Vec::new();
        for row in rows {
            let rec = decode_change_identity(&row)?;
            if rec.change_id.starts_with(prefix) {
                out.push(rec);
            }
        }
        Ok(out)
    }

    async fn upsert_change_revision(&self, record: &ChangeRevisionRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_change_revision(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_change_revision",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO change_revision \
             (change_id, commit_oid, created_op_id, visibility, revision_ordinal) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(change_id, commit_oid) DO UPDATE SET \
               created_op_id = excluded.created_op_id, visibility = excluded.visibility, \
               revision_ordinal = excluded.revision_ordinal",
            [
                record.change_id.clone().into(),
                record.commit_oid.clone().into(),
                record.created_op_id.clone().into(),
                record.visibility.clone().into(),
                record.revision_ordinal.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert change_revision", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_change_revision",
                err,
            )
        })
    }

    async fn list_change_revisions(
        &self,
        change_id: &str,
    ) -> StorageResult<Vec<ChangeRevisionRecord>> {
        validate_non_empty_identifier(change_id, "change_revision change_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT change_id, commit_oid, created_op_id, visibility, revision_ordinal \
                 FROM change_revision WHERE change_id = ? \
                 ORDER BY revision_ordinal ASC, commit_oid ASC",
                [change_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list change_revision", err)
            })?;
        rows.iter().map(decode_change_revision).collect()
    }

    async fn find_change_revisions_by_commit(
        &self,
        commit_oid: &str,
    ) -> StorageResult<Vec<ChangeRevisionRecord>> {
        validate_hex_oid(commit_oid, "change_revision commit_oid")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT change_id, commit_oid, created_op_id, visibility, revision_ordinal \
                 FROM change_revision WHERE commit_oid = ? ORDER BY change_id ASC",
                [commit_oid.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "find change_revision by commit",
                    err,
                )
            })?;
        rows.iter().map(decode_change_revision).collect()
    }

    async fn upsert_change_predecessor(
        &self,
        record: &ChangePredecessorRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_change_predecessor(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_change_predecessor",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO change_predecessor \
             (successor_oid, predecessor_oid, op_id, relation_kind, ordinal) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(successor_oid, predecessor_oid, op_id) DO UPDATE SET \
               relation_kind = excluded.relation_kind, ordinal = excluded.ordinal",
            [
                record.successor_oid.clone().into(),
                record.predecessor_oid.clone().into(),
                record.op_id.clone().into(),
                record.relation_kind.clone().into(),
                record.ordinal.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert change_predecessor", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_change_predecessor",
                err,
            )
        })
    }

    async fn list_change_predecessors(
        &self,
        successor_oid: &str,
    ) -> StorageResult<Vec<ChangePredecessorRecord>> {
        validate_hex_oid(successor_oid, "change_predecessor successor_oid")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT successor_oid, predecessor_oid, op_id, relation_kind, ordinal \
                 FROM change_predecessor WHERE successor_oid = ? \
                 ORDER BY ordinal ASC, op_id ASC, predecessor_oid ASC",
                [successor_oid.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list change_predecessor", err)
            })?;
        rows.iter().map(decode_change_predecessor).collect()
    }

    async fn upsert_ai_operation_link(&self, record: &AiOperationLinkRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_operation_link(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_operation_link",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_operation_link \
             (operation_id, session_id, run_id, tool_invocation_id, intent_id, repo_id, \
              worktree_id, workspace_id, lease_generation, config_provenance_digest, \
              redaction_version, change_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(operation_id) DO UPDATE SET \
               session_id = excluded.session_id, run_id = excluded.run_id, \
               tool_invocation_id = excluded.tool_invocation_id, intent_id = excluded.intent_id, \
               repo_id = excluded.repo_id, worktree_id = excluded.worktree_id, \
               workspace_id = excluded.workspace_id, lease_generation = excluded.lease_generation, \
               config_provenance_digest = excluded.config_provenance_digest, \
               redaction_version = excluded.redaction_version, change_id = excluded.change_id",
            [
                record.operation_id.clone().into(),
                record.session_id.clone().into(),
                record.run_id.clone().into(),
                record.tool_invocation_id.clone().into(),
                record.intent_id.clone().into(),
                record.repo_id.clone().into(),
                record.worktree_id.clone().into(),
                record.workspace_id.clone().into(),
                record.lease_generation.into(),
                record.config_provenance_digest.clone().into(),
                record.redaction_version.clone().into(),
                record.change_id.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_operation_link", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_operation_link",
                err,
            )
        })
    }

    async fn get_ai_operation_link(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<AiOperationLinkRecord>> {
        validate_non_empty_identifier(operation_id, "ai_operation_link operation_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT operation_id, session_id, run_id, tool_invocation_id, intent_id, repo_id, \
                        worktree_id, workspace_id, lease_generation, config_provenance_digest, \
                        redaction_version, change_id \
                 FROM ai_operation_link WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get ai_operation_link", err)
            })?;
        row.as_ref().map(decode_ai_operation_link).transpose()
    }

    async fn list_ai_operation_links_by_change(
        &self,
        repo_id: &str,
        change_id: &str,
    ) -> StorageResult<Vec<AiOperationLinkRecord>> {
        validate_non_empty_identifier(repo_id, "ai_operation_link repo_id")?;
        validate_non_empty_identifier(change_id, "ai_operation_link change_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT operation_id, session_id, run_id, tool_invocation_id, intent_id, repo_id, \
                        worktree_id, workspace_id, lease_generation, config_provenance_digest, \
                        redaction_version, change_id \
                 FROM ai_operation_link WHERE repo_id = ? AND change_id = ? \
                 ORDER BY operation_id ASC",
                [repo_id.into(), change_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list ai_operation_link by change",
                    err,
                )
            })?;
        rows.iter().map(decode_ai_operation_link).collect()
    }

    async fn delete_pending_ai_operation_link(&self, operation_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(operation_id, "ai_operation_link operation_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin delete_pending_ai_operation_link",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM ai_operation_link WHERE operation_id = ? AND change_id IS NULL",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "delete pending ai_operation_link",
                    err,
                )
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit delete_pending_ai_operation_link",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn replace_working_dirty_snapshot(
        &self,
        meta: &WorkingDirtyMetaRecord,
        entries: &[WorkingDirtyEntryRecord],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_working_dirty_meta(meta)?;
        for entry in entries {
            validate_working_dirty_entry(entry)?;
            if entry.worktree_id != meta.worktree_id {
                return Err(StorageBackendError::BackendInternal {
                    detail: "working_dirty entry worktree_id must match meta worktree_id"
                        .to_string(),
                });
            }
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin replace_working_dirty_snapshot",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM working_dirty WHERE worktree_id = ?",
            [meta.worktree_id.clone().into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "clear working_dirty", err))?;

        for chunk in entries.chunks(150) {
            let mut sql = String::from(
                "INSERT INTO working_dirty \
                 (worktree_id, path, kind, source, marked_at, verified_at) VALUES ",
            );
            let mut values = Vec::with_capacity(chunk.len() * 6);
            for (idx, e) in chunk.iter().enumerate() {
                if idx != 0 {
                    sql.push_str(", ");
                }
                sql.push_str("(?, ?, ?, ?, ?, ?)");
                values.extend([
                    Value::from(e.worktree_id.clone()),
                    Value::from(e.path.clone()),
                    Value::from(e.kind.clone()),
                    Value::from(e.source.clone()),
                    Value::from(e.marked_at.clone()),
                    Value::from(e.verified_at.clone()),
                ]);
            }
            sql.push_str(
                " ON CONFLICT(worktree_id, path, kind) DO UPDATE SET \
                 source = excluded.source, marked_at = excluded.marked_at, \
                 verified_at = excluded.verified_at",
            );
            txn.execute_raw(Statement::from_sql_and_values(backend, sql, values))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "insert working_dirty rows", err)
                })?;
        }

        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO working_dirty_meta \
             (worktree_id, state, index_fingerprint, head_oid, scanned_at, scan_lock_pid, scan_lock_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id) DO UPDATE SET \
               state = excluded.state, index_fingerprint = excluded.index_fingerprint, \
               head_oid = excluded.head_oid, scanned_at = excluded.scanned_at, \
               scan_lock_pid = excluded.scan_lock_pid, scan_lock_at = excluded.scan_lock_at",
            [
                meta.worktree_id.clone().into(),
                meta.state.clone().into(),
                meta.index_fingerprint.clone().into(),
                meta.head_oid.clone().into(),
                meta.scanned_at.clone().into(),
                meta.scan_lock_pid.into(),
                meta.scan_lock_at.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert working_dirty_meta", err)
        })?;

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit replace_working_dirty_snapshot",
                err,
            )
        })
    }

    async fn upsert_working_dirty_entry(
        &self,
        entry: &WorkingDirtyEntryRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_working_dirty_entry(entry)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_working_dirty_entry",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO working_dirty \
             (worktree_id, path, kind, source, marked_at, verified_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id, path, kind) DO UPDATE SET \
               source = excluded.source, marked_at = excluded.marked_at, \
               verified_at = excluded.verified_at",
            [
                entry.worktree_id.clone().into(),
                entry.path.clone().into(),
                entry.kind.clone().into(),
                entry.source.clone().into(),
                entry.marked_at.clone().into(),
                entry.verified_at.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert working_dirty entry", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_working_dirty_entry",
                err,
            )
        })
    }

    async fn get_working_dirty_meta(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<WorkingDirtyMetaRecord>> {
        validate_no_nul(worktree_id, "working_dirty_meta worktree_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, state, index_fingerprint, head_oid, scanned_at, \
                        scan_lock_pid, scan_lock_at \
                 FROM working_dirty_meta WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get working_dirty_meta", err)
            })?;
        row.as_ref().map(decode_working_dirty_meta).transpose()
    }

    async fn list_working_dirty_entries(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Vec<WorkingDirtyEntryRecord>> {
        validate_no_nul(worktree_id, "working_dirty worktree_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, path, kind, source, marked_at, verified_at \
                 FROM working_dirty WHERE worktree_id = ? ORDER BY path ASC, kind ASC",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list working_dirty", err)
            })?;
        rows.iter().map(decode_working_dirty_entry).collect()
    }

    async fn upsert_layer(&self, record: &LayerRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_layer_record(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin upsert_layer", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO layer \
             (worktree_id, name, source, priority, enabled, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id, name) DO UPDATE SET \
               source = excluded.source, priority = excluded.priority, \
               enabled = excluded.enabled, updated_at = excluded.updated_at",
            [
                record.worktree_id.clone().into(),
                record.name.clone().into(),
                record.source.clone().into(),
                record.priority.into(),
                i64::from(record.enabled).into(),
                record.created_at.clone().into(),
                record.updated_at.clone().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "upsert layer", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "commit upsert_layer", err))
    }

    async fn list_layers(&self, worktree_id: &str) -> StorageResult<Vec<LayerRecord>> {
        validate_no_nul(worktree_id, "layer worktree_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, name, source, priority, enabled, created_at, updated_at \
                 FROM layer WHERE worktree_id = ? ORDER BY priority ASC, name ASC",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "list layer", err))?;
        rows.iter().map(decode_layer_record).collect()
    }

    async fn delete_layer(&self, worktree_id: &str, name: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_no_nul(worktree_id, "layer worktree_id")?;
        validate_non_empty_identifier(name, "layer name")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin delete_layer", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM layer_path WHERE worktree_id = ? AND layer_name = ?",
            [worktree_id.into(), name.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "delete layer_path", err))?;
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM layer WHERE worktree_id = ? AND name = ?",
                [worktree_id.into(), name.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "delete layer", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit delete_layer", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn upsert_layer_path(&self, record: &LayerPathRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_layer_path_record(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin upsert_layer_path", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO layer_path \
             (worktree_id, layer_name, path, content_hash, materialized_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(worktree_id, path) DO UPDATE SET \
               layer_name = excluded.layer_name, content_hash = excluded.content_hash, \
               materialized_at = excluded.materialized_at",
            [
                record.worktree_id.clone().into(),
                record.layer_name.clone().into(),
                record.path.clone().into(),
                record.content_hash.clone().into(),
                record.materialized_at.clone().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "upsert layer_path", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit upsert_layer_path", err)
        })
    }

    async fn replace_layer_paths(
        &self,
        worktree_id: &str,
        records: &[LayerPathRecord],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_no_nul(worktree_id, "layer_path worktree_id")?;
        for rec in records {
            validate_layer_path_record(rec)?;
        }
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin replace_layer_paths", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM layer_path WHERE worktree_id = ?",
            [worktree_id.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "clear layer_path", err))?;
        for record in records {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO layer_path \
                 (worktree_id, layer_name, path, content_hash, materialized_at) \
                 VALUES (?, ?, ?, ?, ?)",
                [
                    worktree_id.into(),
                    record.layer_name.clone().into(),
                    record.path.clone().into(),
                    record.content_hash.clone().into(),
                    record.materialized_at.clone().into(),
                ],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "insert layer_path", err))?;
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit replace_layer_paths", err)
        })
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
        let backend = self.conn.get_database_backend();
        let stmt = match layer_name {
            Some(name) => Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, layer_name, path, content_hash, materialized_at \
                 FROM layer_path WHERE worktree_id = ? AND layer_name = ? ORDER BY path ASC",
                [worktree_id.into(), name.into()],
            ),
            None => Statement::from_sql_and_values(
                backend,
                "SELECT worktree_id, layer_name, path, content_hash, materialized_at \
                 FROM layer_path WHERE worktree_id = ? ORDER BY path ASC",
                [worktree_id.into()],
            ),
        };
        let rows =
            self.conn.query_all_raw(stmt).await.map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list layer_path", err)
            })?;
        rows.iter().map(decode_layer_path_record).collect()
    }

    async fn replace_sparse_view(
        &self,
        worktree_id: &str,
        enabled: bool,
        patterns: &[String],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_no_nul(worktree_id, "sparse_view worktree_id")?;
        for pat in patterns {
            validate_non_empty_identifier(pat, "sparse_view pattern")?;
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin replace_sparse_view", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM sparse_view WHERE worktree_id = ?",
            [worktree_id.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "clear sparse_view", err))?;

        for (ordinal, pat) in patterns.iter().enumerate() {
            let ord_i64 = i64::try_from(ordinal).unwrap_or(i64::MAX);
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO sparse_view (worktree_id, pattern, ordinal) VALUES (?, ?, ?)",
                [worktree_id.into(), pat.clone().into(), ord_i64.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "insert sparse_view", err)
            })?;
        }

        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO sparse_view_meta (worktree_id, enabled) VALUES (?, ?) \
             ON CONFLICT(worktree_id) DO UPDATE SET enabled = excluded.enabled",
            [worktree_id.into(), i64::from(enabled).into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert sparse_view_meta", err)
        })?;

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit replace_sparse_view", err)
        })
    }

    async fn get_sparse_view(
        &self,
        worktree_id: &str,
    ) -> StorageResult<Option<SparseViewSnapshot>> {
        validate_no_nul(worktree_id, "sparse_view worktree_id")?;
        let txn = self.conn.begin().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin get_sparse_view snapshot",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let Some(meta_row) = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT enabled FROM sparse_view_meta WHERE worktree_id = ?",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get sparse_view_meta", err)
            })?
        else {
            return Ok(None);
        };
        let enabled_i64: i64 = col(&meta_row, 0, "sparse_view_meta.enabled")?;
        let pat_rows = txn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT pattern FROM sparse_view WHERE worktree_id = ? ORDER BY ordinal ASC",
                [worktree_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "list sparse_view", err))?;
        let patterns = pat_rows
            .iter()
            .map(|r| col(r, 0, "sparse_view.pattern"))
            .collect::<StorageResult<Vec<String>>>()?;
        Ok(Some(SparseViewSnapshot {
            worktree_id: worktree_id.to_string(),
            enabled: enabled_i64 != 0,
            patterns,
        }))
    }

    async fn clear_sparse_view(&self, worktree_id: &str) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_no_nul(worktree_id, "sparse_view worktree_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin clear_sparse_view", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM sparse_view WHERE worktree_id = ?",
            [worktree_id.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "delete sparse_view", err))?;
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM sparse_view_meta WHERE worktree_id = ?",
            [worktree_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "delete sparse_view_meta", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit clear_sparse_view", err)
        })
    }

    // -------------------------------------------------------------------------
    // RB-05b AC1: 19 `ai_*` Tables
    // -------------------------------------------------------------------------

    async fn upsert_ai_thread(&self, record: &AiThreadRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_thread(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin upsert_ai_thread", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_thread \
             (thread_id, title, owner_kind, owner_id, owner_display_name, current_intent_id, \
              latest_intent_id, metadata_json, archived, version, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(thread_id) DO UPDATE SET \
               title = excluded.title, owner_kind = excluded.owner_kind, \
               owner_id = excluded.owner_id, owner_display_name = excluded.owner_display_name, \
               current_intent_id = excluded.current_intent_id, \
               latest_intent_id = excluded.latest_intent_id, \
               metadata_json = excluded.metadata_json, archived = excluded.archived, \
               version = excluded.version, created_at = excluded.created_at, \
               updated_at = excluded.updated_at",
            [
                record.thread_id.clone().into(),
                record.title.clone().into(),
                record.owner_kind.clone().into(),
                record.owner_id.clone().into(),
                record.owner_display_name.clone().into(),
                record.current_intent_id.clone().into(),
                record.latest_intent_id.clone().into(),
                record.metadata_json.clone().into(),
                i64::from(record.archived).into(),
                record.version.into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_thread", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit upsert_ai_thread", err)
        })
    }

    async fn get_ai_thread(&self, thread_id: &str) -> StorageResult<Option<AiThreadRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread thread_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, title, owner_kind, owner_id, owner_display_name, \
                        current_intent_id, latest_intent_id, metadata_json, archived, \
                        version, created_at, updated_at \
                 FROM ai_thread WHERE thread_id = ?",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "get ai_thread", err))?;
        row.as_ref().map(decode_ai_thread).transpose()
    }

    async fn list_ai_threads(
        &self,
        archived_filter: Option<bool>,
    ) -> StorageResult<Vec<AiThreadRecord>> {
        let backend = self.conn.get_database_backend();
        let stmt = match archived_filter {
            Some(archived) => Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, title, owner_kind, owner_id, owner_display_name, \
                        current_intent_id, latest_intent_id, metadata_json, archived, \
                        version, created_at, updated_at \
                 FROM ai_thread WHERE archived = ? ORDER BY updated_at DESC, thread_id ASC",
                [i64::from(archived).into()],
            ),
            None => Statement::from_string(
                backend,
                "SELECT thread_id, title, owner_kind, owner_id, owner_display_name, \
                        current_intent_id, latest_intent_id, metadata_json, archived, \
                        version, created_at, updated_at \
                 FROM ai_thread ORDER BY updated_at DESC, thread_id ASC"
                    .to_string(),
            ),
        };
        let rows =
            self.conn.query_all_raw(stmt).await.map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_thread", err)
            })?;
        rows.iter().map(decode_ai_thread).collect()
    }

    async fn delete_ai_thread(&self, thread_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(thread_id, "ai_thread thread_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin delete_ai_thread", err)
        })?;
        let backend = txn.get_database_backend();
        for sql in [
            "DELETE FROM ai_thread_participant WHERE thread_id = ?",
            "DELETE FROM ai_thread_intent WHERE thread_id = ?",
            "DELETE FROM ai_thread_provider_metadata WHERE thread_id = ?",
            "DELETE FROM ai_scheduler_state WHERE thread_id = ?",
            "DELETE FROM ai_scheduler_plan_head WHERE thread_id = ?",
            "DELETE FROM ai_scheduler_selected_plan WHERE thread_id = ?",
            "DELETE FROM ai_live_context_window WHERE thread_id = ?",
            "DELETE FROM ai_validation_report WHERE thread_id = ?",
            "DELETE FROM ai_risk_score_breakdown WHERE thread_id = ?",
            "DELETE FROM ai_decision_proposal WHERE thread_id = ?",
            "DELETE FROM ai_final_decision WHERE thread_id = ?",
        ] {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                sql,
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "cascade delete ai_thread child",
                    err,
                )
            })?;
        }
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM ai_thread WHERE thread_id = ?",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "delete ai_thread", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit delete_ai_thread", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn upsert_ai_thread_participant(
        &self,
        record: &AiThreadParticipantRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_thread_participant(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_thread_participant",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_thread_participant \
             (thread_id, actor_kind, actor_id, actor_display_name, role, joined_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(thread_id, actor_kind, actor_id) DO UPDATE SET \
               actor_display_name = excluded.actor_display_name, \
               role = excluded.role, joined_at = excluded.joined_at",
            [
                record.thread_id.clone().into(),
                record.actor_kind.clone().into(),
                record.actor_id.clone().into(),
                record.actor_display_name.clone().into(),
                record.role.clone().into(),
                record.joined_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "upsert ai_thread_participant",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_thread_participant",
                err,
            )
        })
    }

    async fn list_ai_thread_participants(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiThreadParticipantRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread_participant thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, actor_kind, actor_id, actor_display_name, role, joined_at \
                 FROM ai_thread_participant WHERE thread_id = ? \
                 ORDER BY actor_kind ASC, actor_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_thread_participant", err)
            })?;
        rows.iter().map(decode_ai_thread_participant).collect()
    }

    async fn upsert_ai_thread_intent(&self, record: &AiThreadIntentRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_thread_intent(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_thread_intent",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_thread_intent \
             (thread_id, intent_id, ordinal, is_head, linked_at, link_reason) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(thread_id, intent_id) DO UPDATE SET \
               ordinal = excluded.ordinal, is_head = excluded.is_head, \
               linked_at = excluded.linked_at, link_reason = excluded.link_reason",
            [
                record.thread_id.clone().into(),
                record.intent_id.clone().into(),
                record.ordinal.into(),
                i64::from(record.is_head).into(),
                record.linked_at.into(),
                record.link_reason.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_thread_intent", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_thread_intent",
                err,
            )
        })
    }

    async fn list_ai_thread_intents(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiThreadIntentRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread_intent thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, intent_id, ordinal, is_head, linked_at, link_reason \
                 FROM ai_thread_intent WHERE thread_id = ? \
                 ORDER BY ordinal ASC, intent_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_thread_intent", err)
            })?;
        rows.iter().map(decode_ai_thread_intent).collect()
    }

    async fn upsert_ai_thread_provider_metadata(
        &self,
        record: &AiThreadProviderMetadataRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_thread_provider_metadata(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_thread_provider_metadata",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_thread_provider_metadata \
             (thread_id, legacy_session_id, provider_thread_id, provider_kind, metadata_json, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(thread_id) DO UPDATE SET \
               legacy_session_id = excluded.legacy_session_id, \
               provider_thread_id = excluded.provider_thread_id, \
               provider_kind = excluded.provider_kind, \
               metadata_json = excluded.metadata_json, updated_at = excluded.updated_at",
            [
                record.thread_id.clone().into(),
                record.legacy_session_id.clone().into(),
                record.provider_thread_id.clone().into(),
                record.provider_kind.clone().into(),
                record.metadata_json.clone().into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "upsert ai_thread_provider_metadata",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_thread_provider_metadata",
                err,
            )
        })
    }

    async fn get_ai_thread_provider_metadata(
        &self,
        thread_id: &str,
    ) -> StorageResult<Option<AiThreadProviderMetadataRecord>> {
        validate_non_empty_identifier(thread_id, "ai_thread_provider_metadata thread_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, legacy_session_id, provider_thread_id, provider_kind, \
                        metadata_json, updated_at \
                 FROM ai_thread_provider_metadata WHERE thread_id = ?",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "get ai_thread_provider_metadata",
                    err,
                )
            })?;
        row.as_ref()
            .map(decode_ai_thread_provider_metadata)
            .transpose()
    }

    async fn upsert_ai_scheduler_state(
        &self,
        record: &AiSchedulerStateRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_scheduler_state(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_scheduler_state",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_scheduler_state \
             (thread_id, selected_plan_id, active_task_id, active_run_id, metadata_json, version, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(thread_id) DO UPDATE SET \
               selected_plan_id = excluded.selected_plan_id, \
               active_task_id = excluded.active_task_id, \
               active_run_id = excluded.active_run_id, \
               metadata_json = excluded.metadata_json, \
               version = excluded.version, updated_at = excluded.updated_at",
            [
                record.thread_id.clone().into(),
                record.selected_plan_id.clone().into(),
                record.active_task_id.clone().into(),
                record.active_run_id.clone().into(),
                record.metadata_json.clone().into(),
                record.version.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_scheduler_state", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_scheduler_state",
                err,
            )
        })
    }

    async fn get_ai_scheduler_state(
        &self,
        thread_id: &str,
    ) -> StorageResult<Option<AiSchedulerStateRecord>> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_state thread_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, selected_plan_id, active_task_id, active_run_id, \
                        metadata_json, version, updated_at \
                 FROM ai_scheduler_state WHERE thread_id = ?",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get ai_scheduler_state", err)
            })?;
        row.as_ref().map(decode_ai_scheduler_state).transpose()
    }

    async fn replace_ai_scheduler_plan_heads(
        &self,
        thread_id: &str,
        heads: &[AiSchedulerPlanHeadRecord],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
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
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin replace_ai_scheduler_plan_heads",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, thread_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM ai_scheduler_plan_head WHERE thread_id = ?",
            [thread_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "clear ai_scheduler_plan_head",
                err,
            )
        })?;
        for h in heads {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO ai_scheduler_plan_head (thread_id, plan_id, ordinal) VALUES (?, ?, ?)",
                [
                    h.thread_id.clone().into(),
                    h.plan_id.clone().into(),
                    h.ordinal.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "insert ai_scheduler_plan_head",
                    err,
                )
            })?;
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit replace_ai_scheduler_plan_heads",
                err,
            )
        })
    }

    async fn list_ai_scheduler_plan_heads(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiSchedulerPlanHeadRecord>> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_plan_head thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, plan_id, ordinal FROM ai_scheduler_plan_head \
                 WHERE thread_id = ? ORDER BY ordinal ASC, plan_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_scheduler_plan_head", err)
            })?;
        rows.iter().map(decode_ai_scheduler_plan_head).collect()
    }

    async fn replace_ai_scheduler_selected_plans(
        &self,
        thread_id: &str,
        plans: &[AiSchedulerSelectedPlanRecord],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
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
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin replace_ai_scheduler_selected_plans",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, thread_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM ai_scheduler_selected_plan WHERE thread_id = ?",
            [thread_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "clear ai_scheduler_selected_plan",
                err,
            )
        })?;
        for p in plans {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO ai_scheduler_selected_plan (thread_id, plan_id, ordinal) VALUES (?, ?, ?)",
                [
                    p.thread_id.clone().into(),
                    p.plan_id.clone().into(),
                    p.ordinal.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "insert ai_scheduler_selected_plan",
                    err,
                )
            })?;
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit replace_ai_scheduler_selected_plans",
                err,
            )
        })
    }

    async fn list_ai_scheduler_selected_plans(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiSchedulerSelectedPlanRecord>> {
        validate_non_empty_identifier(thread_id, "ai_scheduler_selected_plan thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, plan_id, ordinal FROM ai_scheduler_selected_plan \
                 WHERE thread_id = ? ORDER BY ordinal ASC, plan_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list ai_scheduler_selected_plan",
                    err,
                )
            })?;
        rows.iter().map(decode_ai_scheduler_selected_plan).collect()
    }

    async fn replace_ai_live_context_window(
        &self,
        thread_id: &str,
        entries: &[AiLiveContextWindowRecord],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
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
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin replace_ai_live_context_window",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, thread_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM ai_live_context_window WHERE thread_id = ?",
            [thread_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "clear ai_live_context_window",
                err,
            )
        })?;
        for e in entries {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO ai_live_context_window \
                 (thread_id, context_frame_id, position, source_kind, pin_kind, inserted_at) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                [
                    e.thread_id.clone().into(),
                    e.context_frame_id.clone().into(),
                    e.position.into(),
                    e.source_kind.clone().into(),
                    e.pin_kind.clone().into(),
                    e.inserted_at.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "insert ai_live_context_window",
                    err,
                )
            })?;
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit replace_ai_live_context_window",
                err,
            )
        })
    }

    async fn list_ai_live_context_window(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiLiveContextWindowRecord>> {
        validate_non_empty_identifier(thread_id, "ai_live_context_window thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT thread_id, context_frame_id, position, source_kind, pin_kind, inserted_at \
                 FROM ai_live_context_window WHERE thread_id = ? \
                 ORDER BY position ASC, context_frame_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_live_context_window", err)
            })?;
        rows.iter().map(decode_ai_live_context_window).collect()
    }

    async fn upsert_ai_index_intent_plan(
        &self,
        record: &AiIndexIntentPlanRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_index_intent_plan(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_index_intent_plan",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_index_intent_plan (intent_id, plan_id, created_at) \
             VALUES (?, ?, ?) \
             ON CONFLICT(intent_id, plan_id) DO UPDATE SET created_at = excluded.created_at",
            [
                record.intent_id.clone().into(),
                record.plan_id.clone().into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_index_intent_plan", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_index_intent_plan",
                err,
            )
        })
    }

    async fn list_ai_index_intent_plans(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentPlanRecord>> {
        validate_non_empty_identifier(intent_id, "ai_index_intent_plan intent_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT intent_id, plan_id, created_at FROM ai_index_intent_plan \
                 WHERE intent_id = ? ORDER BY created_at ASC, plan_id ASC",
                [intent_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_index_intent_plan", err)
            })?;
        rows.iter().map(decode_ai_index_intent_plan).collect()
    }

    async fn upsert_ai_index_intent_task(
        &self,
        record: &AiIndexIntentTaskRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_index_intent_task(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_index_intent_task",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_index_intent_task \
             (intent_id, task_id, parent_task_id, origin_step_id, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(intent_id, task_id) DO UPDATE SET \
               parent_task_id = excluded.parent_task_id, \
               origin_step_id = excluded.origin_step_id, created_at = excluded.created_at",
            [
                record.intent_id.clone().into(),
                record.task_id.clone().into(),
                record.parent_task_id.clone().into(),
                record.origin_step_id.clone().into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_index_intent_task", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_index_intent_task",
                err,
            )
        })
    }

    async fn list_ai_index_intent_tasks(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentTaskRecord>> {
        validate_non_empty_identifier(intent_id, "ai_index_intent_task intent_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT intent_id, task_id, parent_task_id, origin_step_id, created_at \
                 FROM ai_index_intent_task WHERE intent_id = ? \
                 ORDER BY created_at ASC, task_id ASC",
                [intent_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_index_intent_task", err)
            })?;
        rows.iter().map(decode_ai_index_intent_task).collect()
    }

    async fn upsert_ai_index_plan_step_task(
        &self,
        record: &AiIndexPlanStepTaskRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_index_plan_step_task(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_index_plan_step_task",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_index_plan_step_task (plan_id, task_id, step_id, created_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(plan_id, task_id) DO UPDATE SET \
               step_id = excluded.step_id, created_at = excluded.created_at",
            [
                record.plan_id.clone().into(),
                record.task_id.clone().into(),
                record.step_id.clone().into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "upsert ai_index_plan_step_task",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_index_plan_step_task",
                err,
            )
        })
    }

    async fn list_ai_index_plan_step_tasks(
        &self,
        plan_id: &str,
    ) -> StorageResult<Vec<AiIndexPlanStepTaskRecord>> {
        validate_non_empty_identifier(plan_id, "ai_index_plan_step_task plan_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT plan_id, task_id, step_id, created_at FROM ai_index_plan_step_task \
                 WHERE plan_id = ? ORDER BY step_id ASC, created_at ASC, task_id ASC",
                [plan_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list ai_index_plan_step_task",
                    err,
                )
            })?;
        rows.iter().map(decode_ai_index_plan_step_task).collect()
    }

    async fn upsert_ai_index_task_run(&self, record: &AiIndexTaskRunRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_index_task_run(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_index_task_run",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        if record.is_latest {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE ai_index_task_run SET is_latest = 0 \
                 WHERE task_id = ? AND is_latest = 1 AND run_id <> ?",
                [record.task_id.clone().into(), record.run_id.clone().into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "clear previous latest ai_index_task_run",
                    err,
                )
            })?;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_index_task_run \
             (task_id, run_id, is_latest, created_at, base_commit_ref) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(task_id, run_id) DO UPDATE SET \
               is_latest = excluded.is_latest, created_at = excluded.created_at, \
               base_commit_ref = excluded.base_commit_ref",
            [
                record.task_id.clone().into(),
                record.run_id.clone().into(),
                i64::from(record.is_latest).into(),
                record.created_at.into(),
                record.base_commit_ref.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_index_task_run", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_index_task_run",
                err,
            )
        })
    }

    async fn list_ai_index_task_runs(
        &self,
        task_id: &str,
    ) -> StorageResult<Vec<AiIndexTaskRunRecord>> {
        validate_non_empty_identifier(task_id, "ai_index_task_run task_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT task_id, run_id, is_latest, created_at, base_commit_ref \
                 FROM ai_index_task_run WHERE task_id = ? \
                 ORDER BY created_at ASC, run_id ASC",
                [task_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_index_task_run", err)
            })?;
        rows.iter().map(decode_ai_index_task_run).collect()
    }

    async fn upsert_ai_index_run_event(&self, record: &AiIndexRunEventRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_index_run_event(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_index_run_event",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        if record.is_latest {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE ai_index_run_event SET is_latest = 0 \
                 WHERE run_id = ? AND is_latest = 1 AND event_id <> ?",
                [record.run_id.clone().into(), record.event_id.clone().into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "clear previous latest ai_index_run_event",
                    err,
                )
            })?;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_index_run_event (run_id, event_id, event_kind, is_latest, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(run_id, event_id) DO UPDATE SET \
               event_kind = excluded.event_kind, is_latest = excluded.is_latest, \
               created_at = excluded.created_at",
            [
                record.run_id.clone().into(),
                record.event_id.clone().into(),
                record.event_kind.clone().into(),
                i64::from(record.is_latest).into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_index_run_event", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_index_run_event",
                err,
            )
        })
    }

    async fn list_ai_index_run_events(
        &self,
        run_id: &str,
    ) -> StorageResult<Vec<AiIndexRunEventRecord>> {
        validate_non_empty_identifier(run_id, "ai_index_run_event run_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT run_id, event_id, event_kind, is_latest, created_at \
                 FROM ai_index_run_event WHERE run_id = ? \
                 ORDER BY created_at ASC, event_id ASC",
                [run_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_index_run_event", err)
            })?;
        rows.iter().map(decode_ai_index_run_event).collect()
    }

    async fn upsert_ai_index_run_patchset(
        &self,
        record: &AiIndexRunPatchsetRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_index_run_patchset(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_index_run_patchset",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        if record.is_latest {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE ai_index_run_patchset SET is_latest = 0 \
                 WHERE run_id = ? AND is_latest = 1 AND patchset_id <> ?",
                [
                    record.run_id.clone().into(),
                    record.patchset_id.clone().into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "clear previous latest ai_index_run_patchset",
                    err,
                )
            })?;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_index_run_patchset \
             (run_id, patchset_id, sequence, is_latest, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(run_id, patchset_id) DO UPDATE SET \
               sequence = excluded.sequence, is_latest = excluded.is_latest, \
               created_at = excluded.created_at",
            [
                record.run_id.clone().into(),
                record.patchset_id.clone().into(),
                record.sequence.into(),
                i64::from(record.is_latest).into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "upsert ai_index_run_patchset",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_index_run_patchset",
                err,
            )
        })
    }

    async fn list_ai_index_run_patchsets(
        &self,
        run_id: &str,
    ) -> StorageResult<Vec<AiIndexRunPatchsetRecord>> {
        validate_non_empty_identifier(run_id, "ai_index_run_patchset run_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT run_id, patchset_id, sequence, is_latest, created_at \
                 FROM ai_index_run_patchset WHERE run_id = ? \
                 ORDER BY sequence ASC, patchset_id ASC",
                [run_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_index_run_patchset", err)
            })?;
        rows.iter().map(decode_ai_index_run_patchset).collect()
    }

    async fn upsert_ai_index_intent_context_frame(
        &self,
        record: &AiIndexIntentContextFrameRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_index_intent_context_frame(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_index_intent_context_frame",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_index_intent_context_frame \
             (intent_id, context_frame_id, relation_kind, created_at) \
             VALUES (?, ?, ?, ?) \
             ON CONFLICT(intent_id, context_frame_id, relation_kind) DO UPDATE SET \
               created_at = excluded.created_at",
            [
                record.intent_id.clone().into(),
                record.context_frame_id.clone().into(),
                record.relation_kind.clone().into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "upsert ai_index_intent_context_frame",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_index_intent_context_frame",
                err,
            )
        })
    }

    async fn list_ai_index_intent_context_frames(
        &self,
        intent_id: &str,
    ) -> StorageResult<Vec<AiIndexIntentContextFrameRecord>> {
        validate_non_empty_identifier(intent_id, "ai_index_intent_context_frame intent_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT intent_id, context_frame_id, relation_kind, created_at \
                 FROM ai_index_intent_context_frame WHERE intent_id = ? \
                 ORDER BY relation_kind ASC, created_at ASC, context_frame_id ASC",
                [intent_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list ai_index_intent_context_frame",
                    err,
                )
            })?;
        rows.iter()
            .map(decode_ai_index_intent_context_frame)
            .collect()
    }

    async fn upsert_ai_validation_report(
        &self,
        record: &AiValidationReportRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_validation_report(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_validation_report",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        if record.is_latest {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE ai_validation_report SET is_latest = 0 \
                 WHERE thread_id = ? AND is_latest = 1 AND report_id <> ?",
                [
                    record.thread_id.clone().into(),
                    record.report_id.clone().into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "clear previous latest ai_validation_report",
                    err,
                )
            })?;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_validation_report \
             (report_id, thread_id, run_id, policy_version, stale, is_latest, summary_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(report_id) DO UPDATE SET \
               thread_id = excluded.thread_id, run_id = excluded.run_id, \
               policy_version = excluded.policy_version, stale = excluded.stale, \
               is_latest = excluded.is_latest, summary_json = excluded.summary_json, \
               created_at = excluded.created_at, updated_at = excluded.updated_at",
            [
                record.report_id.clone().into(),
                record.thread_id.clone().into(),
                record.run_id.clone().into(),
                record.policy_version.clone().into(),
                i64::from(record.stale).into(),
                i64::from(record.is_latest).into(),
                record.summary_json.clone().into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_validation_report", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_validation_report",
                err,
            )
        })
    }

    async fn list_ai_validation_reports(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiValidationReportRecord>> {
        validate_non_empty_identifier(thread_id, "ai_validation_report thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT report_id, thread_id, run_id, policy_version, stale, is_latest, \
                        summary_json, created_at, updated_at \
                 FROM ai_validation_report WHERE thread_id = ? \
                 ORDER BY created_at ASC, report_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_validation_report", err)
            })?;
        rows.iter().map(decode_ai_validation_report).collect()
    }

    async fn upsert_ai_risk_score_breakdown(
        &self,
        record: &AiRiskScoreBreakdownRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_risk_score_breakdown(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_risk_score_breakdown",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        if record.is_latest {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE ai_risk_score_breakdown SET is_latest = 0 \
                 WHERE thread_id = ? AND is_latest = 1 AND breakdown_id <> ?",
                [
                    record.thread_id.clone().into(),
                    record.breakdown_id.clone().into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "clear previous latest ai_risk_score_breakdown",
                    err,
                )
            })?;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_risk_score_breakdown \
             (breakdown_id, thread_id, validation_report_id, policy_version, stale, is_latest, summary_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(breakdown_id) DO UPDATE SET \
               thread_id = excluded.thread_id, \
               validation_report_id = excluded.validation_report_id, \
               policy_version = excluded.policy_version, stale = excluded.stale, \
               is_latest = excluded.is_latest, summary_json = excluded.summary_json, \
               created_at = excluded.created_at, updated_at = excluded.updated_at",
            [
                record.breakdown_id.clone().into(),
                record.thread_id.clone().into(),
                record.validation_report_id.clone().into(),
                record.policy_version.clone().into(),
                i64::from(record.stale).into(),
                i64::from(record.is_latest).into(),
                record.summary_json.clone().into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "upsert ai_risk_score_breakdown",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_risk_score_breakdown",
                err,
            )
        })
    }

    async fn list_ai_risk_score_breakdowns(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiRiskScoreBreakdownRecord>> {
        validate_non_empty_identifier(thread_id, "ai_risk_score_breakdown thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT breakdown_id, thread_id, validation_report_id, policy_version, stale, \
                        is_latest, summary_json, created_at, updated_at \
                 FROM ai_risk_score_breakdown WHERE thread_id = ? \
                 ORDER BY created_at ASC, breakdown_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list ai_risk_score_breakdown",
                    err,
                )
            })?;
        rows.iter().map(decode_ai_risk_score_breakdown).collect()
    }

    async fn upsert_ai_decision_proposal(
        &self,
        record: &AiDecisionProposalRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_decision_proposal(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_decision_proposal",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        if record.is_latest {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE ai_decision_proposal SET is_latest = 0 \
                 WHERE thread_id = ? AND is_latest = 1 AND proposal_id <> ?",
                [
                    record.thread_id.clone().into(),
                    record.proposal_id.clone().into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "clear previous latest ai_decision_proposal",
                    err,
                )
            })?;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_decision_proposal \
             (proposal_id, thread_id, validation_report_id, risk_score_breakdown_id, \
              policy_version, stale, is_latest, summary_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(proposal_id) DO UPDATE SET \
               thread_id = excluded.thread_id, \
               validation_report_id = excluded.validation_report_id, \
               risk_score_breakdown_id = excluded.risk_score_breakdown_id, \
               policy_version = excluded.policy_version, stale = excluded.stale, \
               is_latest = excluded.is_latest, summary_json = excluded.summary_json, \
               created_at = excluded.created_at, updated_at = excluded.updated_at",
            [
                record.proposal_id.clone().into(),
                record.thread_id.clone().into(),
                record.validation_report_id.clone().into(),
                record.risk_score_breakdown_id.clone().into(),
                record.policy_version.clone().into(),
                i64::from(record.stale).into(),
                i64::from(record.is_latest).into(),
                record.summary_json.clone().into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_decision_proposal", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_decision_proposal",
                err,
            )
        })
    }

    async fn list_ai_decision_proposals(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiDecisionProposalRecord>> {
        validate_non_empty_identifier(thread_id, "ai_decision_proposal thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT proposal_id, thread_id, validation_report_id, risk_score_breakdown_id, \
                        policy_version, stale, is_latest, summary_json, created_at, updated_at \
                 FROM ai_decision_proposal WHERE thread_id = ? \
                 ORDER BY created_at ASC, proposal_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_decision_proposal", err)
            })?;
        rows.iter().map(decode_ai_decision_proposal).collect()
    }

    async fn upsert_ai_final_decision(&self, record: &AiFinalDecisionRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_ai_final_decision(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_ai_final_decision",
                err,
            )
        })?;
        ensure_ai_thread_exists_in_txn(&txn, &record.thread_id).await?;
        let backend = txn.get_database_backend();
        if record.is_latest {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE ai_final_decision SET is_latest = 0 \
                 WHERE thread_id = ? AND is_latest = 1 AND decision_id <> ?",
                [
                    record.thread_id.clone().into(),
                    record.decision_id.clone().into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "clear previous latest ai_final_decision",
                    err,
                )
            })?;
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO ai_final_decision \
             (decision_id, thread_id, decision_proposal_id, validation_report_id, \
              policy_version, verdict, stale, is_latest, summary_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(decision_id) DO UPDATE SET \
               thread_id = excluded.thread_id, \
               decision_proposal_id = excluded.decision_proposal_id, \
               validation_report_id = excluded.validation_report_id, \
               policy_version = excluded.policy_version, verdict = excluded.verdict, \
               stale = excluded.stale, is_latest = excluded.is_latest, \
               summary_json = excluded.summary_json, created_at = excluded.created_at, \
               updated_at = excluded.updated_at",
            [
                record.decision_id.clone().into(),
                record.thread_id.clone().into(),
                record.decision_proposal_id.clone().into(),
                record.validation_report_id.clone().into(),
                record.policy_version.clone().into(),
                record.verdict.clone().into(),
                i64::from(record.stale).into(),
                i64::from(record.is_latest).into(),
                record.summary_json.clone().into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert ai_final_decision", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_ai_final_decision",
                err,
            )
        })
    }

    async fn list_ai_final_decisions(
        &self,
        thread_id: &str,
    ) -> StorageResult<Vec<AiFinalDecisionRecord>> {
        validate_non_empty_identifier(thread_id, "ai_final_decision thread_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT decision_id, thread_id, decision_proposal_id, validation_report_id, \
                        policy_version, verdict, stale, is_latest, summary_json, \
                        created_at, updated_at \
                 FROM ai_final_decision WHERE thread_id = ? \
                 ORDER BY created_at ASC, decision_id ASC",
                [thread_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list ai_final_decision", err)
            })?;
        rows.iter().map(decode_ai_final_decision).collect()
    }

    // -------------------------------------------------------------------------
    // RB-05b AC2 & AC3: 5 `memory_*` Tables + `MEMORY_BY_PATH` Query
    // -------------------------------------------------------------------------

    async fn upsert_memory_episode(&self, record: &MemoryEpisodeRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_memory_episode(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin upsert_memory_episode", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO memory_episode \
             (episode_id, repo_id, source_kind, source_key, outcome, actor, started_at, \
              ended_at, anchor_commit, change_id, title, body, content_digest, producer, rules_version) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(episode_id) DO UPDATE SET \
               repo_id = excluded.repo_id, source_kind = excluded.source_kind, \
               source_key = excluded.source_key, outcome = excluded.outcome, \
               actor = excluded.actor, started_at = excluded.started_at, \
               ended_at = excluded.ended_at, anchor_commit = excluded.anchor_commit, \
               change_id = excluded.change_id, title = excluded.title, body = excluded.body, \
               content_digest = excluded.content_digest, producer = excluded.producer, \
               rules_version = excluded.rules_version",
            [
                record.episode_id.clone().into(),
                record.repo_id.clone().into(),
                record.source_kind.as_str().into(),
                record.source_key.clone().into(),
                record.outcome.as_str().into(),
                record.actor.clone().into(),
                record.started_at.into(),
                record.ended_at.into(),
                record.anchor_commit.clone().into(),
                record.change_id.clone().into(),
                record.title.clone().into(),
                record.body.clone().into(),
                record.content_digest.clone().into(),
                record.producer.clone().into(),
                record.rules_version.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "upsert memory_episode", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_memory_episode",
                err,
            )
        })
    }

    async fn get_memory_episode(
        &self,
        episode_id: &str,
    ) -> StorageResult<Option<MemoryEpisodeRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode episode_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT episode_id, repo_id, source_kind, source_key, outcome, actor, \
                        started_at, ended_at, anchor_commit, change_id, title, body, \
                        content_digest, producer, rules_version \
                 FROM memory_episode WHERE episode_id = ?",
                [episode_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get memory_episode", err)
            })?;
        row.as_ref().map(decode_memory_episode).transpose()
    }

    async fn list_memory_episodes(&self, repo_id: &str) -> StorageResult<Vec<MemoryEpisodeRecord>> {
        validate_non_empty_identifier(repo_id, "memory_episode repo_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT episode_id, repo_id, source_kind, source_key, outcome, actor, \
                        started_at, ended_at, anchor_commit, change_id, title, body, \
                        content_digest, producer, rules_version \
                 FROM memory_episode WHERE repo_id = ? \
                 ORDER BY ended_at DESC, episode_id ASC",
                [repo_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list memory_episode", err)
            })?;
        rows.iter().map(decode_memory_episode).collect()
    }

    async fn delete_memory_episode(&self, episode_id: &str) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(episode_id, "memory_episode episode_id")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin delete_memory_episode", err)
        })?;
        let backend = txn.get_database_backend();
        for sql in [
            "DELETE FROM memory_episode_evidence WHERE episode_id = ?",
            "DELETE FROM memory_episode_path WHERE episode_id = ?",
            "DELETE FROM memory_episode_search_doc WHERE episode_id = ?",
        ] {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                sql,
                [episode_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "cascade delete memory_episode child",
                    err,
                )
            })?;
        }
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM memory_episode WHERE episode_id = ?",
                [episode_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "delete memory_episode", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit delete_memory_episode",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn replace_memory_episode_evidence(
        &self,
        episode_id: &str,
        evidence: &[MemoryEpisodeEvidenceRecord],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
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
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin replace_memory_episode_evidence",
                err,
            )
        })?;
        ensure_memory_episode_exists_in_txn(&txn, episode_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM memory_episode_evidence WHERE episode_id = ?",
            [episode_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "clear memory_episode_evidence",
                err,
            )
        })?;
        for ev in evidence {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO memory_episode_evidence \
                 (episode_id, ordinal, kind, ref_id, link_confidence, resolution_status) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                [
                    ev.episode_id.clone().into(),
                    ev.ordinal.into(),
                    ev.kind.as_str().into(),
                    ev.ref_id.clone().into(),
                    ev.link_confidence.as_str().into(),
                    ev.resolution_status.as_str().into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "insert memory_episode_evidence",
                    err,
                )
            })?;
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit replace_memory_episode_evidence",
                err,
            )
        })
    }

    async fn list_memory_episode_evidence(
        &self,
        episode_id: &str,
    ) -> StorageResult<Vec<MemoryEpisodeEvidenceRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode_evidence episode_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT episode_id, ordinal, kind, ref_id, link_confidence, resolution_status \
                 FROM memory_episode_evidence WHERE episode_id = ? ORDER BY ordinal ASC",
                [episode_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list memory_episode_evidence",
                    err,
                )
            })?;
        rows.iter().map(decode_memory_episode_evidence).collect()
    }

    async fn upsert_memory_projection_state(
        &self,
        record: &MemoryProjectionStateRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_memory_projection_state(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_memory_projection_state",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO memory_projection_state \
             (repo_id, source_kind, cursor_json, fingerprint, rules_version, schema_version, \
              horizon_truncated, revoked_count, aged_out_count, rebuilt_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(repo_id, source_kind) DO UPDATE SET \
               cursor_json = excluded.cursor_json, fingerprint = excluded.fingerprint, \
               rules_version = excluded.rules_version, schema_version = excluded.schema_version, \
               horizon_truncated = excluded.horizon_truncated, \
               revoked_count = excluded.revoked_count, aged_out_count = excluded.aged_out_count, \
               rebuilt_at = excluded.rebuilt_at",
            [
                record.repo_id.clone().into(),
                record.source_kind.as_str().into(),
                record.cursor_json.clone().into(),
                record.fingerprint.clone().into(),
                record.rules_version.into(),
                record.schema_version.into(),
                i64::from(record.horizon_truncated).into(),
                record.revoked_count.into(),
                record.aged_out_count.into(),
                record.rebuilt_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "upsert memory_projection_state",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_memory_projection_state",
                err,
            )
        })
    }

    async fn get_memory_projection_state(
        &self,
        repo_id: &str,
        source_kind: MemorySourceKind,
    ) -> StorageResult<Option<MemoryProjectionStateRecord>> {
        validate_non_empty_identifier(repo_id, "memory_projection_state repo_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT repo_id, source_kind, cursor_json, fingerprint, rules_version, \
                        schema_version, horizon_truncated, revoked_count, aged_out_count, rebuilt_at \
                 FROM memory_projection_state WHERE repo_id = ? AND source_kind = ?",
                [repo_id.into(), source_kind.as_str().into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get memory_projection_state", err)
            })?;
        row.as_ref().map(decode_memory_projection_state).transpose()
    }

    async fn list_memory_projection_states(
        &self,
        repo_id: &str,
    ) -> StorageResult<Vec<MemoryProjectionStateRecord>> {
        validate_non_empty_identifier(repo_id, "memory_projection_state repo_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT repo_id, source_kind, cursor_json, fingerprint, rules_version, \
                        schema_version, horizon_truncated, revoked_count, aged_out_count, rebuilt_at \
                 FROM memory_projection_state WHERE repo_id = ? ORDER BY source_kind ASC",
                [repo_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list memory_projection_state", err)
            })?;
        rows.iter().map(decode_memory_projection_state).collect()
    }

    async fn upsert_memory_episode_path(
        &self,
        record: &MemoryEpisodePathRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_memory_episode_path(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_memory_episode_path",
                err,
            )
        })?;
        ensure_memory_episode_exists_in_txn(&txn, &record.episode_id).await?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO memory_episode_path \
             (episode_id, code_path, change_kind, blob_oid_at_end, ended_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(episode_id, code_path) DO UPDATE SET \
               change_kind = excluded.change_kind, \
               blob_oid_at_end = excluded.blob_oid_at_end, ended_at = excluded.ended_at",
            [
                record.episode_id.clone().into(),
                Value::Bytes(Some(record.code_path.clone())),
                record.change_kind.as_str().into(),
                record.blob_oid_at_end.clone().into(),
                record.ended_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert memory_episode_path", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_memory_episode_path",
                err,
            )
        })
    }

    async fn list_memory_episode_paths(
        &self,
        episode_id: &str,
    ) -> StorageResult<Vec<MemoryEpisodePathRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode_path episode_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT episode_id, code_path, change_kind, blob_oid_at_end, ended_at \
                 FROM memory_episode_path WHERE episode_id = ? ORDER BY code_path ASC",
                [episode_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list memory_episode_path", err)
            })?;
        rows.iter().map(decode_memory_episode_path).collect()
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
        let backend = self.conn.get_database_backend();
        let mut sql = String::from(
            "SELECT episode_id, code_path, change_kind, blob_oid_at_end, ended_at \
             FROM memory_episode_path WHERE code_path = ?",
        );
        let mut params: Vec<Value> = vec![Value::Bytes(Some(code_path.to_vec()))];
        if let Some(since) = since_ended_at {
            sql.push_str(" AND ended_at >= ?");
            params.push(since.into());
        }
        if let Some(until) = until_ended_at {
            sql.push_str(" AND ended_at <= ?");
            params.push(until.into());
        }
        sql.push_str(" ORDER BY ended_at DESC, episode_id ASC");
        if let Some(lim) = limit {
            let lim_i64 = i64::try_from(lim).unwrap_or(i64::MAX);
            sql.push_str(" LIMIT ?");
            params.push(lim_i64.into());
        }
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(backend, sql, params))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "query memory_episode_path by path",
                    err,
                )
            })?;
        rows.iter().map(decode_memory_episode_path).collect()
    }

    async fn upsert_memory_episode_search_doc(
        &self,
        episode_id: &str,
        title: &str,
        body: &str,
        paths_text: &str,
    ) -> StorageResult<MemoryEpisodeSearchDocRecord> {
        self.ensure_writable()?;
        validate_non_empty_identifier(episode_id, "memory_episode_search_doc episode_id")?;
        validate_no_nul(title, "memory_episode_search_doc title")?;
        validate_no_nul(body, "memory_episode_search_doc body")?;
        validate_no_nul(paths_text, "memory_episode_search_doc paths_text")?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_memory_episode_search_doc",
                err,
            )
        })?;
        ensure_memory_episode_exists_in_txn(&txn, episode_id).await?;
        let backend = txn.get_database_backend();
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT rowid FROM memory_episode_search_doc WHERE episode_id = ?",
                [episode_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "lookup memory_episode_search_doc rowid",
                    err,
                )
            })?;

        let rowid: i64 = if let Some(row) = existing {
            let rid: i64 = col(&row, 0, "memory_episode_search_doc.rowid")?;
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE memory_episode_search_doc SET title = ?, body = ?, paths_text = ? \
                 WHERE rowid = ?",
                [title.into(), body.into(), paths_text.into(), rid.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "update memory_episode_search_doc",
                    err,
                )
            })?;
            rid
        } else {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO memory_episode_search_doc (episode_id, title, body, paths_text) \
                 VALUES (?, ?, ?, ?)",
                [
                    episode_id.into(),
                    title.into(),
                    body.into(),
                    paths_text.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "insert memory_episode_search_doc",
                    err,
                )
            })?;
            let row = txn
                .query_one_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT rowid FROM memory_episode_search_doc WHERE episode_id = ?",
                    [episode_id.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "read inserted memory_episode_search_doc rowid",
                        err,
                    )
                })?
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "inserted memory_episode_search_doc row missing".to_string(),
                })?;
            col(&row, 0, "memory_episode_search_doc.rowid")?
        };

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_memory_episode_search_doc",
                err,
            )
        })?;
        Ok(MemoryEpisodeSearchDocRecord {
            rowid,
            episode_id: episode_id.to_string(),
            title: title.to_string(),
            body: body.to_string(),
            paths_text: paths_text.to_string(),
        })
    }

    async fn get_memory_episode_search_doc(
        &self,
        episode_id: &str,
    ) -> StorageResult<Option<MemoryEpisodeSearchDocRecord>> {
        validate_non_empty_identifier(episode_id, "memory_episode_search_doc episode_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT rowid, episode_id, title, body, paths_text \
                 FROM memory_episode_search_doc WHERE episode_id = ?",
                [episode_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "get memory_episode_search_doc",
                    err,
                )
            })?;
        row.as_ref()
            .map(decode_memory_episode_search_doc)
            .transpose()
    }

    async fn list_memory_episode_search_docs(
        &self,
    ) -> StorageResult<Vec<MemoryEpisodeSearchDocRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT rowid, episode_id, title, body, paths_text \
                 FROM memory_episode_search_doc ORDER BY rowid ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list memory_episode_search_doc",
                    err,
                )
            })?;
        rows.iter().map(decode_memory_episode_search_doc).collect()
    }

    // -------------------------------------------------------------------------
    // RB-05b AC4: Governance & Audit (`approved_permission`, `automation_log`, `source_call_log`)
    // -------------------------------------------------------------------------

    async fn upsert_approved_permission(
        &self,
        record: &ApprovedPermissionRecord,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_approved_permission(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_approved_permission",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO approved_permission \
             (project_id, permission, pattern, created_at, source_worktree_id, \
              source_session_id, source_workspace_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(project_id, permission, pattern) DO UPDATE SET \
               created_at = excluded.created_at, \
               source_worktree_id = excluded.source_worktree_id, \
               source_session_id = excluded.source_session_id, \
               source_workspace_id = excluded.source_workspace_id",
            [
                record.project_id.clone().into(),
                record.permission.clone().into(),
                record.pattern.clone().into(),
                record.created_at.into(),
                record.source_worktree_id.clone().into(),
                record.source_session_id.clone().into(),
                record.source_workspace_id.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "upsert approved_permission", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_approved_permission",
                err,
            )
        })
    }

    async fn list_approved_permissions(
        &self,
        project_id: &str,
    ) -> StorageResult<Vec<ApprovedPermissionRecord>> {
        validate_non_empty_identifier(project_id, "approved_permission project_id")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT project_id, permission, pattern, created_at, source_worktree_id, \
                        source_session_id, source_workspace_id \
                 FROM approved_permission WHERE project_id = ? \
                 ORDER BY created_at ASC, permission ASC, pattern ASC",
                [project_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list approved_permission", err)
            })?;
        rows.iter().map(decode_approved_permission).collect()
    }

    async fn delete_approved_permission(
        &self,
        project_id: &str,
        permission: &str,
        pattern: &str,
    ) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_non_empty_identifier(project_id, "approved_permission project_id")?;
        validate_non_empty_identifier(permission, "approved_permission permission")?;
        validate_non_empty_identifier(pattern, "approved_permission pattern")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin delete_approved_permission",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM approved_permission \
                 WHERE project_id = ? AND permission = ? AND pattern = ?",
                [project_id.into(), permission.into(), pattern.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "delete approved_permission", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit delete_approved_permission",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn append_automation_log(&self, record: &AutomationLogRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_automation_log(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin append_automation_log", err)
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO automation_log \
             (id, rule_id, trigger_kind, action_kind, status, message, started_at, \
              finished_at, details_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                record.id.clone().into(),
                record.rule_id.clone().into(),
                record.trigger_kind.clone().into(),
                record.action_kind.clone().into(),
                record.status.clone().into(),
                record.message.clone().into(),
                record.started_at.clone().into(),
                record.finished_at.clone().into(),
                record.details_json.clone().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "insert automation_log", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit append_automation_log",
                err,
            )
        })
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
        let backend = self.conn.get_database_backend();
        let mut sql = String::from(
            "SELECT id, rule_id, trigger_kind, action_kind, status, message, started_at, \
                    finished_at, details_json \
             FROM automation_log",
        );
        let mut params: Vec<Value> = Vec::new();
        if let Some(r) = rule_id {
            sql.push_str(" WHERE rule_id = ?");
            params.push(r.into());
        }
        sql.push_str(" ORDER BY finished_at DESC, id ASC");
        if let Some(lim) = limit {
            let lim_i64 = i64::try_from(lim).unwrap_or(i64::MAX);
            sql.push_str(" LIMIT ?");
            params.push(lim_i64.into());
        }
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(backend, sql, params))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list automation_log", err)
            })?;
        rows.iter().map(decode_automation_log).collect()
    }

    async fn append_source_call_log(&self, record: &SourceCallLogRecord) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_source_call_log(record)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin append_source_call_log",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO source_call_log \
             (id, session_id, source_slug, tool_name, registered_tool_name, tool_call_id, \
              credential_ref, latency_ms, input_bytes, output_bytes, cost_estimate_micros, \
              approval_decision, state_namespace, success, created_at, agent_run_id) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                record.id.clone().into(),
                record.session_id.clone().into(),
                record.source_slug.clone().into(),
                record.tool_name.clone().into(),
                record.registered_tool_name.clone().into(),
                record.tool_call_id.clone().into(),
                record.credential_ref.clone().into(),
                record.latency_ms.into(),
                record.input_bytes.into(),
                record.output_bytes.into(),
                record.cost_estimate_micros.into(),
                record.approval_decision.clone().into(),
                record.state_namespace.clone().into(),
                i64::from(record.success).into(),
                record.created_at.clone().into(),
                record.agent_run_id.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "insert source_call_log", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit append_source_call_log",
                err,
            )
        })
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
        let backend = self.conn.get_database_backend();
        let mut sql = String::from(
            "SELECT id, session_id, source_slug, tool_name, registered_tool_name, tool_call_id, \
                    credential_ref, latency_ms, input_bytes, output_bytes, cost_estimate_micros, \
                    approval_decision, state_namespace, success, created_at, agent_run_id \
             FROM source_call_log",
        );
        let mut clauses = Vec::new();
        let mut params: Vec<Value> = Vec::new();
        if let Some(s) = session_id {
            clauses.push("session_id = ?");
            params.push(s.into());
        }
        if let Some(r) = agent_run_id {
            clauses.push("agent_run_id = ?");
            params.push(r.into());
        }
        if !clauses.is_empty() {
            sql.push_str(" WHERE ");
            sql.push_str(&clauses.join(" AND "));
        }
        sql.push_str(" ORDER BY created_at ASC, id ASC");
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(backend, sql, params))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list source_call_log", err)
            })?;
        rows.iter().map(decode_source_call_log).collect()
    }

    // -------------------------------------------------------------------------
    // RB-05b AC5: Bridge Two-Phase Publication (`prepared` -> `published`)
    // -------------------------------------------------------------------------

    async fn prepare_bridge_commit_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<BridgePublicationRecord> {
        self.ensure_writable()?;
        validate_bridge_operation_id(operation_id)?;
        validate_hex_oid(commit_oid, "agent_bridge_publication commit_oid")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin prepare_bridge_commit_publication",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT commit_oid, phase FROM agent_bridge_publication WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "read agent_bridge_publication",
                    err,
                )
            })?;
        let rec = match existing {
            Some(row) => {
                let existing_oid: String = col(&row, 0, "agent_bridge_publication.commit_oid")?;
                let existing_phase_str: String = col(&row, 1, "agent_bridge_publication.phase")?;
                let existing_phase = parse_bridge_phase(&existing_phase_str)?;
                if existing_oid != commit_oid {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "bridge commit publication already bound to a different commit"
                            .to_string(),
                    });
                }
                BridgePublicationRecord {
                    operation_id: operation_id.to_string(),
                    commit_oid: existing_oid,
                    phase: existing_phase,
                }
            }
            None => {
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO agent_bridge_publication (operation_id, commit_oid, phase) \
                     VALUES (?, ?, 'prepared')",
                    [operation_id.into(), commit_oid.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "insert prepared agent_bridge_publication",
                        err,
                    )
                })?;
                BridgePublicationRecord {
                    operation_id: operation_id.to_string(),
                    commit_oid: commit_oid.to_string(),
                    phase: BridgePublicationPhase::Prepared,
                }
            }
        };
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit prepare_bridge_commit_publication",
                err,
            )
        })?;
        Ok(rec)
    }

    async fn publish_bridge_commit_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<BridgePublicationRecord> {
        self.ensure_writable()?;
        validate_bridge_operation_id(operation_id)?;
        validate_hex_oid(commit_oid, "agent_bridge_publication commit_oid")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin publish_bridge_commit_publication",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT commit_oid, phase FROM agent_bridge_publication WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "read agent_bridge_publication",
                    err,
                )
            })?;
        let row = existing.ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "cannot publish bridge commit publication before prepare phase".to_string(),
        })?;
        let existing_oid: String = col(&row, 0, "agent_bridge_publication.commit_oid")?;
        if existing_oid != commit_oid {
            return Err(StorageBackendError::BackendInternal {
                detail: "bridge commit publication already bound to a different commit".to_string(),
            });
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE agent_bridge_publication SET phase = 'published' \
             WHERE operation_id = ? AND commit_oid = ?",
            [operation_id.into(), commit_oid.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "publish agent_bridge_publication",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit publish_bridge_commit_publication",
                err,
            )
        })?;
        Ok(BridgePublicationRecord {
            operation_id: operation_id.to_string(),
            commit_oid: commit_oid.to_string(),
            phase: BridgePublicationPhase::Published,
        })
    }

    async fn get_bridge_commit_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgePublicationRecord>> {
        validate_bridge_operation_id(operation_id)?;
        let txn = self.conn.begin().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin get_bridge_commit_publication snapshot",
                err,
            )
        })?;
        let exists = table_exists_in_txn(&txn, "agent_bridge_publication").await?;
        if !exists {
            return Ok(None);
        }
        let backend = txn.get_database_backend();
        let phase_col_count: i64 = txn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT COUNT(*) FROM pragma_table_info('agent_bridge_publication') WHERE name = 'phase'"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "inspect agent_bridge_publication phase column",
                    err,
                )
            })?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "failed to inspect agent_bridge_publication phase column".to_string(),
            })?
            .try_get_by_index(0)
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "decode agent_bridge_publication phase column count",
                    err,
                )
            })?;

        if phase_col_count == 0 {
            let Some(row) = txn
                .query_one_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT commit_oid FROM agent_bridge_publication WHERE operation_id = ?",
                    [operation_id.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "get legacy agent_bridge_publication",
                        err,
                    )
                })?
            else {
                return Ok(None);
            };
            let commit_oid: String = col(&row, 0, "agent_bridge_publication.commit_oid")?;
            return Ok(Some(BridgePublicationRecord {
                operation_id: operation_id.to_string(),
                commit_oid,
                phase: BridgePublicationPhase::Published,
            }));
        }

        let Some(row) = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT commit_oid, phase FROM agent_bridge_publication WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "get agent_bridge_publication",
                    err,
                )
            })?
        else {
            return Ok(None);
        };
        let commit_oid: String = col(&row, 0, "agent_bridge_publication.commit_oid")?;
        let phase_str: String = col(&row, 1, "agent_bridge_publication.phase")?;
        Ok(Some(BridgePublicationRecord {
            operation_id: operation_id.to_string(),
            commit_oid,
            phase: parse_bridge_phase(&phase_str)?,
        }))
    }

    async fn prepare_bridge_index_publication(
        &self,
        operation_id: &str,
        index_digest: &str,
    ) -> StorageResult<BridgeIndexPublicationRecord> {
        self.ensure_writable()?;
        validate_bridge_operation_id(operation_id)?;
        validate_non_empty_identifier(index_digest, "agent_bridge_index_publication index_digest")?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin prepare_bridge_index_publication",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let existing = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT index_digest, phase FROM agent_bridge_index_publication \
                 WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "read agent_bridge_index_publication",
                    err,
                )
            })?;
        let rec = match existing {
            Some(row) => {
                let existing_digest: String =
                    col(&row, 0, "agent_bridge_index_publication.index_digest")?;
                let existing_phase_str: String =
                    col(&row, 1, "agent_bridge_index_publication.phase")?;
                let existing_phase = parse_bridge_phase(&existing_phase_str)?;
                if existing_digest != index_digest {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "bridge index publication already bound to a different digest"
                            .to_string(),
                    });
                }
                BridgeIndexPublicationRecord {
                    operation_id: operation_id.to_string(),
                    index_digest: existing_digest,
                    phase: existing_phase,
                }
            }
            None => {
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO agent_bridge_index_publication (operation_id, index_digest, phase) \
                     VALUES (?, ?, 'prepared')",
                    [operation_id.into(), index_digest.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "insert prepared agent_bridge_index_publication",
                        err,
                    )
                })?;
                BridgeIndexPublicationRecord {
                    operation_id: operation_id.to_string(),
                    index_digest: index_digest.to_string(),
                    phase: BridgePublicationPhase::Prepared,
                }
            }
        };
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit prepare_bridge_index_publication",
                err,
            )
        })?;
        Ok(rec)
    }

    async fn publish_bridge_index_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgeIndexPublicationRecord>> {
        self.ensure_writable()?;
        validate_bridge_operation_id(operation_id)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin publish_bridge_index_publication",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let Some(row) = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT index_digest FROM agent_bridge_index_publication WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "read agent_bridge_index_publication for publish",
                    err,
                )
            })?
        else {
            drop(txn);
            return Ok(None);
        };
        let index_digest: String = col(&row, 0, "agent_bridge_index_publication.index_digest")?;
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE agent_bridge_index_publication SET phase = 'published' WHERE operation_id = ?",
            [operation_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "publish agent_bridge_index_publication",
                err,
            )
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit publish_bridge_index_publication",
                err,
            )
        })?;
        Ok(Some(BridgeIndexPublicationRecord {
            operation_id: operation_id.to_string(),
            index_digest,
            phase: BridgePublicationPhase::Published,
        }))
    }

    async fn get_bridge_index_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgeIndexPublicationRecord>> {
        validate_bridge_operation_id(operation_id)?;
        let txn = self.conn.begin().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin get_bridge_index_publication snapshot",
                err,
            )
        })?;
        let exists = table_exists_in_txn(&txn, "agent_bridge_index_publication").await?;
        if !exists {
            return Ok(None);
        }
        let backend = txn.get_database_backend();
        let Some(row) = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT index_digest, phase FROM agent_bridge_index_publication \
                 WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "get agent_bridge_index_publication",
                    err,
                )
            })?
        else {
            return Ok(None);
        };
        let index_digest: String = col(&row, 0, "agent_bridge_index_publication.index_digest")?;
        let phase_str: String = col(&row, 1, "agent_bridge_index_publication.phase")?;
        Ok(Some(BridgeIndexPublicationRecord {
            operation_id: operation_id.to_string(),
            index_digest,
            phase: parse_bridge_phase(&phase_str)?,
        }))
    }
}

async fn table_exists_in_txn<C: ConnectionTrait>(txn: &C, table: &str) -> StorageResult<bool> {
    let backend = txn.get_database_backend();
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
            [table.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "check sqlite table existence",
                err,
            )
        })?;
    Ok(row.is_some())
}

async fn ensure_ai_thread_exists_in_txn<C: ConnectionTrait>(
    txn: &C,
    thread_id: &str,
) -> StorageResult<()> {
    let backend = txn.get_database_backend();
    let exists = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM ai_thread WHERE thread_id = ? LIMIT 1",
            [thread_id.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "check ai_thread parent", err))?
        .is_some();
    if !exists {
        return Err(StorageBackendError::BackendInternal {
            detail: "referenced ai_thread does not exist".to_string(),
        });
    }
    Ok(())
}

async fn ensure_memory_episode_exists_in_txn<C: ConnectionTrait>(
    txn: &C,
    episode_id: &str,
) -> StorageResult<()> {
    let backend = txn.get_database_backend();
    let exists = txn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT 1 FROM memory_episode WHERE episode_id = ? LIMIT 1",
            [episode_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "check memory_episode parent", err)
        })?
        .is_some();
    if !exists {
        return Err(StorageBackendError::BackendInternal {
            detail: "referenced memory_episode does not exist".to_string(),
        });
    }
    Ok(())
}

fn parse_bridge_phase(raw: &str) -> StorageResult<BridgePublicationPhase> {
    match raw {
        "prepared" => Ok(BridgePublicationPhase::Prepared),
        "published" => Ok(BridgePublicationPhase::Published),
        _ => Err(StorageBackendError::BackendInternal {
            detail: "invalid bridge publication phase value".to_string(),
        }),
    }
}

fn decode_ai_thread(row: &QueryResult) -> StorageResult<AiThreadRecord> {
    let archived: i64 = col(row, 8, "ai_thread.archived")?;
    Ok(AiThreadRecord {
        thread_id: col(row, 0, "ai_thread.thread_id")?,
        title: col(row, 1, "ai_thread.title")?,
        owner_kind: col(row, 2, "ai_thread.owner_kind")?,
        owner_id: col(row, 3, "ai_thread.owner_id")?,
        owner_display_name: col(row, 4, "ai_thread.owner_display_name")?,
        current_intent_id: col(row, 5, "ai_thread.current_intent_id")?,
        latest_intent_id: col(row, 6, "ai_thread.latest_intent_id")?,
        metadata_json: col(row, 7, "ai_thread.metadata_json")?,
        archived: archived != 0,
        version: col(row, 9, "ai_thread.version")?,
        created_at: col(row, 10, "ai_thread.created_at")?,
        updated_at: col(row, 11, "ai_thread.updated_at")?,
    })
}

fn decode_ai_thread_participant(row: &QueryResult) -> StorageResult<AiThreadParticipantRecord> {
    Ok(AiThreadParticipantRecord {
        thread_id: col(row, 0, "ai_thread_participant.thread_id")?,
        actor_kind: col(row, 1, "ai_thread_participant.actor_kind")?,
        actor_id: col(row, 2, "ai_thread_participant.actor_id")?,
        actor_display_name: col(row, 3, "ai_thread_participant.actor_display_name")?,
        role: col(row, 4, "ai_thread_participant.role")?,
        joined_at: col(row, 5, "ai_thread_participant.joined_at")?,
    })
}

fn decode_ai_thread_intent(row: &QueryResult) -> StorageResult<AiThreadIntentRecord> {
    let is_head: i64 = col(row, 3, "ai_thread_intent.is_head")?;
    Ok(AiThreadIntentRecord {
        thread_id: col(row, 0, "ai_thread_intent.thread_id")?,
        intent_id: col(row, 1, "ai_thread_intent.intent_id")?,
        ordinal: col(row, 2, "ai_thread_intent.ordinal")?,
        is_head: is_head != 0,
        linked_at: col(row, 4, "ai_thread_intent.linked_at")?,
        link_reason: col(row, 5, "ai_thread_intent.link_reason")?,
    })
}

fn decode_ai_thread_provider_metadata(
    row: &QueryResult,
) -> StorageResult<AiThreadProviderMetadataRecord> {
    Ok(AiThreadProviderMetadataRecord {
        thread_id: col(row, 0, "ai_thread_provider_metadata.thread_id")?,
        legacy_session_id: col(row, 1, "ai_thread_provider_metadata.legacy_session_id")?,
        provider_thread_id: col(row, 2, "ai_thread_provider_metadata.provider_thread_id")?,
        provider_kind: col(row, 3, "ai_thread_provider_metadata.provider_kind")?,
        metadata_json: col(row, 4, "ai_thread_provider_metadata.metadata_json")?,
        updated_at: col(row, 5, "ai_thread_provider_metadata.updated_at")?,
    })
}

fn decode_ai_scheduler_state(row: &QueryResult) -> StorageResult<AiSchedulerStateRecord> {
    Ok(AiSchedulerStateRecord {
        thread_id: col(row, 0, "ai_scheduler_state.thread_id")?,
        selected_plan_id: col(row, 1, "ai_scheduler_state.selected_plan_id")?,
        active_task_id: col(row, 2, "ai_scheduler_state.active_task_id")?,
        active_run_id: col(row, 3, "ai_scheduler_state.active_run_id")?,
        metadata_json: col(row, 4, "ai_scheduler_state.metadata_json")?,
        version: col(row, 5, "ai_scheduler_state.version")?,
        updated_at: col(row, 6, "ai_scheduler_state.updated_at")?,
    })
}

fn decode_ai_scheduler_plan_head(row: &QueryResult) -> StorageResult<AiSchedulerPlanHeadRecord> {
    Ok(AiSchedulerPlanHeadRecord {
        thread_id: col(row, 0, "ai_scheduler_plan_head.thread_id")?,
        plan_id: col(row, 1, "ai_scheduler_plan_head.plan_id")?,
        ordinal: col(row, 2, "ai_scheduler_plan_head.ordinal")?,
    })
}

fn decode_ai_scheduler_selected_plan(
    row: &QueryResult,
) -> StorageResult<AiSchedulerSelectedPlanRecord> {
    Ok(AiSchedulerSelectedPlanRecord {
        thread_id: col(row, 0, "ai_scheduler_selected_plan.thread_id")?,
        plan_id: col(row, 1, "ai_scheduler_selected_plan.plan_id")?,
        ordinal: col(row, 2, "ai_scheduler_selected_plan.ordinal")?,
    })
}

fn decode_ai_live_context_window(row: &QueryResult) -> StorageResult<AiLiveContextWindowRecord> {
    Ok(AiLiveContextWindowRecord {
        thread_id: col(row, 0, "ai_live_context_window.thread_id")?,
        context_frame_id: col(row, 1, "ai_live_context_window.context_frame_id")?,
        position: col(row, 2, "ai_live_context_window.position")?,
        source_kind: col(row, 3, "ai_live_context_window.source_kind")?,
        pin_kind: col(row, 4, "ai_live_context_window.pin_kind")?,
        inserted_at: col(row, 5, "ai_live_context_window.inserted_at")?,
    })
}

fn decode_ai_index_intent_plan(row: &QueryResult) -> StorageResult<AiIndexIntentPlanRecord> {
    Ok(AiIndexIntentPlanRecord {
        intent_id: col(row, 0, "ai_index_intent_plan.intent_id")?,
        plan_id: col(row, 1, "ai_index_intent_plan.plan_id")?,
        created_at: col(row, 2, "ai_index_intent_plan.created_at")?,
    })
}

fn decode_ai_index_intent_task(row: &QueryResult) -> StorageResult<AiIndexIntentTaskRecord> {
    Ok(AiIndexIntentTaskRecord {
        intent_id: col(row, 0, "ai_index_intent_task.intent_id")?,
        task_id: col(row, 1, "ai_index_intent_task.task_id")?,
        parent_task_id: col(row, 2, "ai_index_intent_task.parent_task_id")?,
        origin_step_id: col(row, 3, "ai_index_intent_task.origin_step_id")?,
        created_at: col(row, 4, "ai_index_intent_task.created_at")?,
    })
}

fn decode_ai_index_plan_step_task(row: &QueryResult) -> StorageResult<AiIndexPlanStepTaskRecord> {
    Ok(AiIndexPlanStepTaskRecord {
        plan_id: col(row, 0, "ai_index_plan_step_task.plan_id")?,
        task_id: col(row, 1, "ai_index_plan_step_task.task_id")?,
        step_id: col(row, 2, "ai_index_plan_step_task.step_id")?,
        created_at: col(row, 3, "ai_index_plan_step_task.created_at")?,
    })
}

fn decode_ai_index_task_run(row: &QueryResult) -> StorageResult<AiIndexTaskRunRecord> {
    let is_latest: i64 = col(row, 2, "ai_index_task_run.is_latest")?;
    Ok(AiIndexTaskRunRecord {
        task_id: col(row, 0, "ai_index_task_run.task_id")?,
        run_id: col(row, 1, "ai_index_task_run.run_id")?,
        is_latest: is_latest != 0,
        created_at: col(row, 3, "ai_index_task_run.created_at")?,
        base_commit_ref: col(row, 4, "ai_index_task_run.base_commit_ref")?,
    })
}

fn decode_ai_index_run_event(row: &QueryResult) -> StorageResult<AiIndexRunEventRecord> {
    let is_latest: i64 = col(row, 3, "ai_index_run_event.is_latest")?;
    Ok(AiIndexRunEventRecord {
        run_id: col(row, 0, "ai_index_run_event.run_id")?,
        event_id: col(row, 1, "ai_index_run_event.event_id")?,
        event_kind: col(row, 2, "ai_index_run_event.event_kind")?,
        is_latest: is_latest != 0,
        created_at: col(row, 4, "ai_index_run_event.created_at")?,
    })
}

fn decode_ai_index_run_patchset(row: &QueryResult) -> StorageResult<AiIndexRunPatchsetRecord> {
    let is_latest: i64 = col(row, 3, "ai_index_run_patchset.is_latest")?;
    Ok(AiIndexRunPatchsetRecord {
        run_id: col(row, 0, "ai_index_run_patchset.run_id")?,
        patchset_id: col(row, 1, "ai_index_run_patchset.patchset_id")?,
        sequence: col(row, 2, "ai_index_run_patchset.sequence")?,
        is_latest: is_latest != 0,
        created_at: col(row, 4, "ai_index_run_patchset.created_at")?,
    })
}

fn decode_ai_index_intent_context_frame(
    row: &QueryResult,
) -> StorageResult<AiIndexIntentContextFrameRecord> {
    Ok(AiIndexIntentContextFrameRecord {
        intent_id: col(row, 0, "ai_index_intent_context_frame.intent_id")?,
        context_frame_id: col(row, 1, "ai_index_intent_context_frame.context_frame_id")?,
        relation_kind: col(row, 2, "ai_index_intent_context_frame.relation_kind")?,
        created_at: col(row, 3, "ai_index_intent_context_frame.created_at")?,
    })
}

fn decode_ai_validation_report(row: &QueryResult) -> StorageResult<AiValidationReportRecord> {
    let stale: i64 = col(row, 4, "ai_validation_report.stale")?;
    let is_latest: i64 = col(row, 5, "ai_validation_report.is_latest")?;
    Ok(AiValidationReportRecord {
        report_id: col(row, 0, "ai_validation_report.report_id")?,
        thread_id: col(row, 1, "ai_validation_report.thread_id")?,
        run_id: col(row, 2, "ai_validation_report.run_id")?,
        policy_version: col(row, 3, "ai_validation_report.policy_version")?,
        stale: stale != 0,
        is_latest: is_latest != 0,
        summary_json: col(row, 6, "ai_validation_report.summary_json")?,
        created_at: col(row, 7, "ai_validation_report.created_at")?,
        updated_at: col(row, 8, "ai_validation_report.updated_at")?,
    })
}

fn decode_ai_risk_score_breakdown(row: &QueryResult) -> StorageResult<AiRiskScoreBreakdownRecord> {
    let stale: i64 = col(row, 4, "ai_risk_score_breakdown.stale")?;
    let is_latest: i64 = col(row, 5, "ai_risk_score_breakdown.is_latest")?;
    Ok(AiRiskScoreBreakdownRecord {
        breakdown_id: col(row, 0, "ai_risk_score_breakdown.breakdown_id")?,
        thread_id: col(row, 1, "ai_risk_score_breakdown.thread_id")?,
        validation_report_id: col(row, 2, "ai_risk_score_breakdown.validation_report_id")?,
        policy_version: col(row, 3, "ai_risk_score_breakdown.policy_version")?,
        stale: stale != 0,
        is_latest: is_latest != 0,
        summary_json: col(row, 6, "ai_risk_score_breakdown.summary_json")?,
        created_at: col(row, 7, "ai_risk_score_breakdown.created_at")?,
        updated_at: col(row, 8, "ai_risk_score_breakdown.updated_at")?,
    })
}

fn decode_ai_decision_proposal(row: &QueryResult) -> StorageResult<AiDecisionProposalRecord> {
    let stale: i64 = col(row, 5, "ai_decision_proposal.stale")?;
    let is_latest: i64 = col(row, 6, "ai_decision_proposal.is_latest")?;
    Ok(AiDecisionProposalRecord {
        proposal_id: col(row, 0, "ai_decision_proposal.proposal_id")?,
        thread_id: col(row, 1, "ai_decision_proposal.thread_id")?,
        validation_report_id: col(row, 2, "ai_decision_proposal.validation_report_id")?,
        risk_score_breakdown_id: col(row, 3, "ai_decision_proposal.risk_score_breakdown_id")?,
        policy_version: col(row, 4, "ai_decision_proposal.policy_version")?,
        stale: stale != 0,
        is_latest: is_latest != 0,
        summary_json: col(row, 7, "ai_decision_proposal.summary_json")?,
        created_at: col(row, 8, "ai_decision_proposal.created_at")?,
        updated_at: col(row, 9, "ai_decision_proposal.updated_at")?,
    })
}

fn decode_ai_final_decision(row: &QueryResult) -> StorageResult<AiFinalDecisionRecord> {
    let stale: i64 = col(row, 6, "ai_final_decision.stale")?;
    let is_latest: i64 = col(row, 7, "ai_final_decision.is_latest")?;
    Ok(AiFinalDecisionRecord {
        decision_id: col(row, 0, "ai_final_decision.decision_id")?,
        thread_id: col(row, 1, "ai_final_decision.thread_id")?,
        decision_proposal_id: col(row, 2, "ai_final_decision.decision_proposal_id")?,
        validation_report_id: col(row, 3, "ai_final_decision.validation_report_id")?,
        policy_version: col(row, 4, "ai_final_decision.policy_version")?,
        verdict: col(row, 5, "ai_final_decision.verdict")?,
        stale: stale != 0,
        is_latest: is_latest != 0,
        summary_json: col(row, 8, "ai_final_decision.summary_json")?,
        created_at: col(row, 9, "ai_final_decision.created_at")?,
        updated_at: col(row, 10, "ai_final_decision.updated_at")?,
    })
}

fn decode_memory_episode(row: &QueryResult) -> StorageResult<MemoryEpisodeRecord> {
    let sk: String = col(row, 2, "memory_episode.source_kind")?;
    let outcome: String = col(row, 4, "memory_episode.outcome")?;
    Ok(MemoryEpisodeRecord {
        episode_id: col(row, 0, "memory_episode.episode_id")?,
        repo_id: col(row, 1, "memory_episode.repo_id")?,
        source_kind: MemorySourceKind::parse(&sk)?,
        source_key: col(row, 3, "memory_episode.source_key")?,
        outcome: MemoryOutcome::parse(&outcome)?,
        actor: col(row, 5, "memory_episode.actor")?,
        started_at: col(row, 6, "memory_episode.started_at")?,
        ended_at: col(row, 7, "memory_episode.ended_at")?,
        anchor_commit: col(row, 8, "memory_episode.anchor_commit")?,
        change_id: col(row, 9, "memory_episode.change_id")?,
        title: col(row, 10, "memory_episode.title")?,
        body: col(row, 11, "memory_episode.body")?,
        content_digest: col(row, 12, "memory_episode.content_digest")?,
        producer: col(row, 13, "memory_episode.producer")?,
        rules_version: col(row, 14, "memory_episode.rules_version")?,
    })
}

fn decode_memory_episode_evidence(row: &QueryResult) -> StorageResult<MemoryEpisodeEvidenceRecord> {
    let kind: String = col(row, 2, "memory_episode_evidence.kind")?;
    let conf: String = col(row, 4, "memory_episode_evidence.link_confidence")?;
    let status: String = col(row, 5, "memory_episode_evidence.resolution_status")?;
    Ok(MemoryEpisodeEvidenceRecord {
        episode_id: col(row, 0, "memory_episode_evidence.episode_id")?,
        ordinal: col(row, 1, "memory_episode_evidence.ordinal")?,
        kind: MemoryEvidenceKind::parse(&kind)?,
        ref_id: col(row, 3, "memory_episode_evidence.ref_id")?,
        link_confidence: MemoryLinkConfidence::parse(&conf)?,
        resolution_status: MemoryResolutionStatus::parse(&status)?,
    })
}

fn decode_memory_projection_state(row: &QueryResult) -> StorageResult<MemoryProjectionStateRecord> {
    let sk: String = col(row, 1, "memory_projection_state.source_kind")?;
    let trunc: i64 = col(row, 6, "memory_projection_state.horizon_truncated")?;
    Ok(MemoryProjectionStateRecord {
        repo_id: col(row, 0, "memory_projection_state.repo_id")?,
        source_kind: MemorySourceKind::parse(&sk)?,
        cursor_json: col(row, 2, "memory_projection_state.cursor_json")?,
        fingerprint: col(row, 3, "memory_projection_state.fingerprint")?,
        rules_version: col(row, 4, "memory_projection_state.rules_version")?,
        schema_version: col(row, 5, "memory_projection_state.schema_version")?,
        horizon_truncated: trunc != 0,
        revoked_count: col(row, 7, "memory_projection_state.revoked_count")?,
        aged_out_count: col(row, 8, "memory_projection_state.aged_out_count")?,
        rebuilt_at: col(row, 9, "memory_projection_state.rebuilt_at")?,
    })
}

fn decode_memory_episode_path(row: &QueryResult) -> StorageResult<MemoryEpisodePathRecord> {
    let ck: String = col(row, 2, "memory_episode_path.change_kind")?;
    Ok(MemoryEpisodePathRecord {
        episode_id: col(row, 0, "memory_episode_path.episode_id")?,
        code_path: col(row, 1, "memory_episode_path.code_path")?,
        change_kind: MemoryPathChangeKind::parse(&ck)?,
        blob_oid_at_end: col(row, 3, "memory_episode_path.blob_oid_at_end")?,
        ended_at: col(row, 4, "memory_episode_path.ended_at")?,
    })
}

fn decode_memory_episode_search_doc(
    row: &QueryResult,
) -> StorageResult<MemoryEpisodeSearchDocRecord> {
    Ok(MemoryEpisodeSearchDocRecord {
        rowid: col(row, 0, "memory_episode_search_doc.rowid")?,
        episode_id: col(row, 1, "memory_episode_search_doc.episode_id")?,
        title: col(row, 2, "memory_episode_search_doc.title")?,
        body: col(row, 3, "memory_episode_search_doc.body")?,
        paths_text: col(row, 4, "memory_episode_search_doc.paths_text")?,
    })
}

fn decode_approved_permission(row: &QueryResult) -> StorageResult<ApprovedPermissionRecord> {
    Ok(ApprovedPermissionRecord {
        project_id: col(row, 0, "approved_permission.project_id")?,
        permission: col(row, 1, "approved_permission.permission")?,
        pattern: col(row, 2, "approved_permission.pattern")?,
        created_at: col(row, 3, "approved_permission.created_at")?,
        source_worktree_id: col(row, 4, "approved_permission.source_worktree_id")?,
        source_session_id: col(row, 5, "approved_permission.source_session_id")?,
        source_workspace_id: col(row, 6, "approved_permission.source_workspace_id")?,
    })
}

fn decode_automation_log(row: &QueryResult) -> StorageResult<AutomationLogRecord> {
    Ok(AutomationLogRecord {
        id: col(row, 0, "automation_log.id")?,
        rule_id: col(row, 1, "automation_log.rule_id")?,
        trigger_kind: col(row, 2, "automation_log.trigger_kind")?,
        action_kind: col(row, 3, "automation_log.action_kind")?,
        status: col(row, 4, "automation_log.status")?,
        message: col(row, 5, "automation_log.message")?,
        started_at: col(row, 6, "automation_log.started_at")?,
        finished_at: col(row, 7, "automation_log.finished_at")?,
        details_json: col(row, 8, "automation_log.details_json")?,
    })
}

fn decode_source_call_log(row: &QueryResult) -> StorageResult<SourceCallLogRecord> {
    let success: i64 = col(row, 13, "source_call_log.success")?;
    Ok(SourceCallLogRecord {
        id: col(row, 0, "source_call_log.id")?,
        session_id: col(row, 1, "source_call_log.session_id")?,
        source_slug: col(row, 2, "source_call_log.source_slug")?,
        tool_name: col(row, 3, "source_call_log.tool_name")?,
        registered_tool_name: col(row, 4, "source_call_log.registered_tool_name")?,
        tool_call_id: col(row, 5, "source_call_log.tool_call_id")?,
        credential_ref: col(row, 6, "source_call_log.credential_ref")?,
        latency_ms: col(row, 7, "source_call_log.latency_ms")?,
        input_bytes: col(row, 8, "source_call_log.input_bytes")?,
        output_bytes: col(row, 9, "source_call_log.output_bytes")?,
        cost_estimate_micros: col(row, 10, "source_call_log.cost_estimate_micros")?,
        approval_decision: col(row, 11, "source_call_log.approval_decision")?,
        state_namespace: col(row, 12, "source_call_log.state_namespace")?,
        success: success != 0,
        created_at: col(row, 14, "source_call_log.created_at")?,
        agent_run_id: col(row, 15, "source_call_log.agent_run_id")?,
    })
}
