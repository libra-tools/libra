//! SQLite implementation of [`CaptureStore`] and [`AgentBridgeStore`] (`RB-06`).
//!
//! Operates directly against the 25 canonical Capture source tables declared in
//! `sql/migrations/2026111601_capture_database.sql` using single-connection
//! `PRAGMA foreign_keys = ON`, `synchronous = FULL`, `journal_mode = DELETE`,
//! and single `BEGIN IMMEDIATE` write transactions (`begin_write_transaction`).

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use async_trait::async_trait;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DatabaseTransaction, DbErr,
    QueryResult, Statement, TransactionTrait, Value,
};

use crate::{
    internal::db::{
        DatabaseRole,
        schema::{
            CAPTURE_DATABASE_SQL, CAPTURE_SCHEMA_NAME, CAPTURE_SCHEMA_VERSION,
            current_schema_version_for_role, latest_schema_version_for_role,
        },
        sqlite_pool_options,
    },
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
            BridgeEventAppendOutcome, BridgeEventStatus, BridgeLinkSourceType, BridgeLinkWrite,
            BridgeOperationStatus, BridgeSessionOpenInput, BridgeSessionState, CaptureAgentKind,
            CaptureBatchItem, CaptureBatchOutcome, CaptureBatchPayload,
            CaptureCatalogCopyReceiptRecord, CaptureCheckpointScope, CaptureCompleteness,
            CaptureCoverageClaimState, CaptureExportJobState, CaptureHashKind,
            CaptureImportIdentityState, CaptureQueueHighwater, CaptureQueueLane,
            CaptureSchemaVersionRecord, CaptureScopeState, CaptureSessionState,
            CaptureSourceChannel, CaptureStore, CaptureSubagentClaimState,
            CaptureSubagentLinkState, CaptureSubagentSourceChannel, MAX_BRIDGE_EVENT_PAYLOAD_BYTES,
            capture_sha256_hex, validate_agent_checkpoint_record, validate_agent_session_record,
            validate_audit_log_record, validate_bridge_checkpoint_record,
            validate_bridge_link_write, validate_bridge_operation_record,
            validate_bridge_session_open, validate_capture_batch_item,
            validate_checkpoint_prune_tombstone, validate_cloud_base_record,
            validate_copy_receipt_record, validate_coverage_claim_record,
            validate_coverage_conflict_record, validate_coverage_revision_record,
            validate_export_job_record, validate_import_identity_record,
            validate_import_tombstone_record, validate_incarnation_record,
            validate_non_empty_no_nul, validate_schema_version_record,
            validate_session_commit_record, validate_subagent_content_claim,
            validate_subagent_content_revision, validate_subagent_link_record,
            validate_usage_stats_record, validate_workspace_scope_audit_record,
        },
        error::{StorageBackendError, StorageResult},
        manifest::fsync_dir,
        sqlite::map_sqlite_db_err,
    },
};

const DEFAULT_SQLITE_CAPTURE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

static SQLITE_CAPTURE_TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

const CREATE_DEDUP_KEYS_SQL: &str = "\
CREATE TABLE IF NOT EXISTS capture_dedup_keys ( \
    dedup_key TEXT PRIMARY KEY \
)";

const CREATE_HIGHWATER_PRIORITY_SQL: &str = "\
CREATE TABLE IF NOT EXISTS queue_highwater_priority ( \
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
    segment_id INTEGER NOT NULL, \
    byte_offset INTEGER NOT NULL \
)";

const CREATE_HIGHWATER_NORMAL_SQL: &str = "\
CREATE TABLE IF NOT EXISTS queue_highwater_normal ( \
    singleton INTEGER PRIMARY KEY CHECK (singleton = 1), \
    segment_id INTEGER NOT NULL, \
    byte_offset INTEGER NOT NULL \
)";

fn highwater_table_name(lane: CaptureQueueLane) -> &'static str {
    match lane {
        CaptureQueueLane::Priority => "queue_highwater_priority",
        CaptureQueueLane::Normal => "queue_highwater_normal",
    }
}

fn col_err(context: &'static str) -> StorageBackendError {
    StorageBackendError::BackendInternal {
        detail: format!("sqlite capture decode failed ({context})"),
    }
}

fn get_string(row: &QueryResult, col: &'static str) -> StorageResult<String> {
    row.try_get("", col).map_err(|_| col_err(col))
}

fn get_opt_string(row: &QueryResult, col: &'static str) -> StorageResult<Option<String>> {
    row.try_get("", col).map_err(|_| col_err(col))
}

fn get_i64(row: &QueryResult, col: &'static str) -> StorageResult<i64> {
    row.try_get("", col).map_err(|_| col_err(col))
}

fn get_opt_i64(row: &QueryResult, col: &'static str) -> StorageResult<Option<i64>> {
    row.try_get("", col).map_err(|_| col_err(col))
}

fn get_opt_f64(row: &QueryResult, col: &'static str) -> StorageResult<Option<f64>> {
    row.try_get("", col).map_err(|_| col_err(col))
}

fn get_bool_int(row: &QueryResult, col: &'static str) -> StorageResult<bool> {
    Ok(get_i64(row, col)? != 0)
}

fn opt_str_val(v: &Option<String>) -> Value {
    Value::String(v.clone())
}

fn opt_i64_val(v: Option<i64>) -> Value {
    Value::BigInt(v)
}

fn opt_f64_val(v: Option<f64>) -> Value {
    Value::Double(v)
}

async fn connect_sqlite_capture(
    db_path: &Path,
    read_only: bool,
    busy_timeout: Duration,
) -> StorageResult<DatabaseConnection> {
    let filename = std::path::absolute(db_path)
        .map_err(|err| StorageBackendError::io("resolve sqlite capture path", err))?;
    if filename.to_str().is_none() {
        return Err(StorageBackendError::BackendInternal {
            detail: "sqlite capture database path is not valid UTF-8".to_string(),
        });
    }
    let mut options = ConnectOptions::new("sqlite://capture-store");
    options.max_connections(1);
    options.sqlx_logging(false);
    options.map_sqlx_sqlite_pool_opts(sqlite_pool_options);
    options.map_sqlx_sqlite_opts(move |opts| {
        let opts = opts
            .filename(&filename)
            .read_only(read_only)
            .create_if_missing(false)
            .busy_timeout(busy_timeout)
            .foreign_keys(true);
        if read_only {
            opts
        } else {
            opts.synchronous(sea_orm::sqlx::sqlite::SqliteSynchronous::Full)
                .journal_mode(sea_orm::sqlx::sqlite::SqliteJournalMode::Delete)
        }
    });
    Database::connect(options)
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "connect capture sqlite", err))
}

async fn check_sqlite_capture_schema_version(
    conn: &DatabaseConnection,
) -> StorageResult<Option<i64>> {
    let current = current_schema_version_for_role(conn, DatabaseRole::Capture)
        .await
        .map_err(|err| StorageBackendError::io("inspect capture sqlite schema version", err))?;
    let latest = latest_schema_version_for_role(DatabaseRole::Capture)
        .map_err(|err| StorageBackendError::io("lookup latest capture schema version", err))?;
    if let Some(cur) = current
        && latest.is_none_or(|max| cur > max)
    {
        return Err(StorageBackendError::SchemaTooNew {
            role: DatabaseRole::Capture,
            found: u32::try_from(cur).unwrap_or(u32::MAX),
            max_supported: latest.and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        });
    }
    Ok(current)
}

async fn apply_capture_schema_if_needed(conn: &DatabaseConnection) -> StorageResult<()> {
    let current = check_sqlite_capture_schema_version(conn).await?;
    let latest = latest_schema_version_for_role(DatabaseRole::Capture)
        .map_err(|err| StorageBackendError::io("lookup latest capture schema version", err))?;
    if current == latest && current.is_some() {
        return Ok(());
    }
    let txn = conn
        .begin()
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "begin schema bootstrap", err))?;
    let backend = txn.get_database_backend();
    txn.execute_raw(Statement::from_string(
        backend,
        CAPTURE_DATABASE_SQL.to_owned(),
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "apply capture schema SQL", err))?;
    txn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT OR IGNORE INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?)",
        [
            CAPTURE_SCHEMA_VERSION.into(),
            CAPTURE_SCHEMA_NAME.into(),
            chrono::Utc::now().to_rfc3339().into(),
        ],
    ))
    .await
    .map_err(|err| {
        map_sqlite_db_err(DatabaseRole::Capture, "insert capture schema receipt", err)
    })?;
    txn.commit()
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit capture schema", err))?;
    Ok(())
}

async fn table_exists_in_txn<C: ConnectionTrait>(
    conn: &C,
    table_name: &str,
) -> StorageResult<bool> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 AS present FROM sqlite_master WHERE type = 'table' AND name = ?",
            [table_name.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "check sqlite_master table", err)
        })?;
    Ok(row.is_some())
}

async fn ensure_batch_internal_tables(txn: &DatabaseTransaction) -> StorageResult<()> {
    let backend = txn.get_database_backend();
    for (sql, ctx) in [
        (CREATE_DEDUP_KEYS_SQL, "create capture_dedup_keys"),
        (
            CREATE_HIGHWATER_PRIORITY_SQL,
            "create queue_highwater_priority",
        ),
        (CREATE_HIGHWATER_NORMAL_SQL, "create queue_highwater_normal"),
    ] {
        txn.execute_raw(Statement::from_string(backend, sql.to_string()))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, ctx, err))?;
    }
    Ok(())
}

/// Cancellation-safe RAII guard for [`SqliteCaptureStore::create`].
///
/// Ensures temporary bootstrap files and any freshly published destination
/// file are removed if creation returns an error OR its future is cancelled
/// before returning `Ok(Self)`.
struct SqliteCreatePublishGuard {
    tmp_path: PathBuf,
    dest_path: PathBuf,
    parent: PathBuf,
    published: bool,
    committed: bool,
}

impl Drop for SqliteCreatePublishGuard {
    fn drop(&mut self) {
        if self.committed {
            return;
        }
        let _ = fs::remove_file(&self.tmp_path);
        let _ = fs::remove_file(self.tmp_path.with_extension("tmp-journal"));
        if self.published {
            let _ = fs::remove_file(&self.dest_path);
            let _ = fsync_dir(&self.parent);
        }
    }
}

/// SQLite-backed implementation of [`CaptureStore`] and [`AgentBridgeStore`].
#[derive(Debug, Clone)]
pub struct SqliteCaptureStore {
    conn: DatabaseConnection,
    path: PathBuf,
    read_only: bool,
}

impl SqliteCaptureStore {
    /// Create a brand-new SQLite capture database at `path` and bootstrap all
    /// 25 Capture source tables from `2026111601_capture_database.sql`.
    ///
    /// Bootstraps inside a sibling temporary file and atomically publishes via
    /// non-overwriting `hard_link` + `fsync_dir`. Protected by a
    /// cancellation-safe RAII guard so neither errors nor dropped futures leave
    /// behind a stale destination file.
    pub async fn create(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref().to_path_buf();
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf();
        fs::create_dir_all(&parent)
            .map_err(|err| StorageBackendError::io("create sqlite capture parent dir", err))?;
        if fs::symlink_metadata(&path).is_ok() {
            return Err(StorageBackendError::Io {
                context: "create sqlite capture file",
                kind: io::ErrorKind::AlreadyExists,
                raw_os_error: None,
            });
        }

        let seq = SQLITE_CAPTURE_TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp_path = parent.join(format!(
            ".capture.sqlite.init.{}.{seq}.tmp",
            std::process::id()
        ));
        let journal_path = tmp_path.with_extension("tmp-journal");

        // Fail closed before arming the RAII rollback guard if the sibling
        // journal sidecar already exists so we never delete an unowned file.
        if fs::symlink_metadata(&journal_path).is_ok() {
            return Err(StorageBackendError::Io {
                context: "create sqlite capture temp journal slot",
                kind: io::ErrorKind::AlreadyExists,
                raw_os_error: None,
            });
        }

        // Establish exclusive ownership of `tmp_path` BEFORE installing the RAII
        // rollback guard so a collision never deletes a pre-existing file.
        fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp_path)
            .map_err(|err| StorageBackendError::io("create sqlite capture temp file", err))?;

        let mut guard = SqliteCreatePublishGuard {
            tmp_path: tmp_path.clone(),
            dest_path: path.clone(),
            parent: parent.clone(),
            published: false,
            committed: false,
        };

        let tmp_conn =
            connect_sqlite_capture(&tmp_path, false, DEFAULT_SQLITE_CAPTURE_BUSY_TIMEOUT).await?;
        let schema_res = apply_capture_schema_if_needed(&tmp_conn).await;
        let _ = tmp_conn.close().await;
        schema_res?;
        fs::hard_link(&tmp_path, &path)
            .map_err(|err| StorageBackendError::io("publish sqlite capture file", err))?;
        guard.published = true;
        let _ = fs::remove_file(&tmp_path);
        fsync_dir(&parent)?;
        let conn =
            connect_sqlite_capture(&path, false, DEFAULT_SQLITE_CAPTURE_BUSY_TIMEOUT).await?;
        guard.committed = true;

        Ok(Self {
            conn,
            path,
            read_only: false,
        })
    }

    /// Open an existing SQLite capture database for read-write access.
    pub async fn open_read_write(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            return Err(StorageBackendError::NotFound {
                resource: "capture sqlite database".to_string(),
            });
        }
        let conn =
            connect_sqlite_capture(&path, false, DEFAULT_SQLITE_CAPTURE_BUSY_TIMEOUT).await?;
        apply_capture_schema_if_needed(&conn).await?;
        Ok(Self {
            conn,
            path,
            read_only: false,
        })
    }

    /// Open an existing SQLite capture database strictly read-only.
    pub async fn open_read_only(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path = path.as_ref().to_path_buf();
        if !path.is_file() {
            return Err(StorageBackendError::NotFound {
                resource: "capture sqlite database".to_string(),
            });
        }
        let conn = connect_sqlite_capture(&path, true, DEFAULT_SQLITE_CAPTURE_BUSY_TIMEOUT).await?;
        let _ = check_sqlite_capture_schema_version(&conn).await?;
        Ok(Self {
            conn,
            path,
            read_only: true,
        })
    }

    /// Path to the underlying SQLite capture database file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Return the live user source table names present in `sqlite_master`,
    /// excluding SQLite internal `sqlite_*` tables and segregated queue/dedup
    /// bookkeeping tables.
    pub async fn list_live_source_tables(&self) -> StorageResult<BTreeSet<String>> {
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                self.conn.get_database_backend(),
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' \
                   AND name NOT LIKE 'sqlite_%' \
                   AND name NOT IN ('capture_dedup_keys', 'queue_highwater_priority', 'queue_highwater_normal')"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list live capture source tables", err)
            })?;
        let mut names = BTreeSet::new();
        for row in rows {
            names.insert(get_string(&row, "name")?);
        }
        Ok(names)
    }

    fn ensure_writable(&self) -> StorageResult<()> {
        if self.read_only {
            return Err(StorageBackendError::BackendInternal {
                detail: "cannot mutate read-only sqlite capture store".to_string(),
            });
        }
        Ok(())
    }

    async fn begin_write(&self) -> StorageResult<DatabaseTransaction> {
        self.ensure_writable()?;
        let txn = self.conn.begin().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "begin capture write tx", err)
        })?;
        if let Err(err) = txn
            .execute_unprepared("UPDATE `schema_versions` SET `version` = `version` WHERE 0")
            .await
        {
            let _ = txn.rollback().await;
            return Err(map_sqlite_db_err(
                DatabaseRole::Capture,
                "acquire capture write lock",
                err,
            ));
        }
        Ok(txn)
    }
}

fn decode_bridge_session(row: &QueryResult) -> StorageResult<AgentBridgeSessionRecord> {
    Ok(AgentBridgeSessionRecord {
        bridge_session_id: get_string(row, "bridge_session_id")?,
        source: get_string(row, "source")?,
        repository_id: get_string(row, "repository_id")?,
        worktree_id: get_opt_string(row, "worktree_id")?,
        workspace_id: get_opt_string(row, "workspace_id")?,
        parent_session_id: get_opt_string(row, "parent_session_id")?,
        agent_id: get_opt_string(row, "agent_id")?,
        session_state: BridgeSessionState::parse(&get_string(row, "session_state")?)?,
        schema_version: get_i64(row, "schema_version")?,
        last_event_seq: get_i64(row, "last_event_seq")?,
        last_acked_seq: get_i64(row, "last_acked_seq")?,
        created_at: get_i64(row, "created_at")?,
        updated_at: get_i64(row, "updated_at")?,
    })
}

fn decode_bridge_event(row: &QueryResult) -> StorageResult<AgentBridgeEventRecord> {
    Ok(AgentBridgeEventRecord {
        event_id: get_i64(row, "event_id")?,
        bridge_session_id: get_string(row, "bridge_session_id")?,
        event_seq: get_i64(row, "event_seq")?,
        event_type: get_string(row, "event_type")?,
        payload_sha256: get_string(row, "payload_sha256")?,
        payload: get_string(row, "payload")?,
        operation_id: get_opt_string(row, "operation_id")?,
        created_at: get_i64(row, "created_at")?,
    })
}

fn decode_bridge_checkpoint(row: &QueryResult) -> StorageResult<AgentBridgeCheckpointRecord> {
    Ok(AgentBridgeCheckpointRecord {
        checkpoint_id: get_string(row, "checkpoint_id")?,
        bridge_session_id: get_string(row, "bridge_session_id")?,
        agent_checkpoint_id: get_opt_string(row, "agent_checkpoint_id")?,
        target_oid: get_opt_string(row, "target_oid")?,
        created_at: get_i64(row, "created_at")?,
    })
}

fn decode_bridge_operation(row: &QueryResult) -> StorageResult<AgentBridgeOperationRecord> {
    Ok(AgentBridgeOperationRecord {
        operation_id: get_string(row, "operation_id")?,
        bridge_session_id: get_string(row, "bridge_session_id")?,
        method: get_string(row, "method")?,
        params_digest: get_string(row, "params_digest")?,
        repository_id: get_string(row, "repository_id")?,
        workspace_id: get_opt_string(row, "workspace_id")?,
        status: BridgeOperationStatus::parse(&get_string(row, "status")?)?,
        result_digest: get_opt_string(row, "result_digest")?,
        created_at: get_i64(row, "created_at")?,
        completed_at: get_opt_i64(row, "completed_at")?,
    })
}

fn decode_bridge_link(row: &QueryResult) -> StorageResult<AgentBridgeLinkRecord> {
    Ok(AgentBridgeLinkRecord {
        link_id: get_i64(row, "link_id")?,
        bridge_session_id: get_string(row, "bridge_session_id")?,
        source_type: BridgeLinkSourceType::parse(&get_string(row, "source_type")?)?,
        source_id: get_string(row, "source_id")?,
        target_type: get_string(row, "target_type")?,
        target_id: get_string(row, "target_id")?,
        created_at: get_i64(row, "created_at")?,
    })
}

fn decode_audit_log(row: &QueryResult) -> StorageResult<AgentAuditLogRecord> {
    Ok(AgentAuditLogRecord {
        audit_id: get_string(row, "audit_id")?,
        timestamp: get_string(row, "timestamp")?,
        user_id: get_opt_string(row, "user_id")?,
        user_name: get_opt_string(row, "user_name")?,
        action: get_string(row, "action")?,
        checkpoint_id: get_string(row, "checkpoint_id")?,
        scope: get_string(row, "scope")?,
        export_path: get_opt_string(row, "export_path")?,
        justification: get_opt_string(row, "justification")?,
        granted: get_bool_int(row, "granted")?,
    })
}

fn decode_agent_session(row: &QueryResult) -> StorageResult<AgentSessionRecord> {
    Ok(AgentSessionRecord {
        session_id: get_string(row, "session_id")?,
        agent_kind: CaptureAgentKind::parse(&get_string(row, "agent_kind")?)?,
        provider_session_id: get_string(row, "provider_session_id")?,
        thread_id: get_opt_string(row, "thread_id")?,
        state: CaptureSessionState::parse(&get_string(row, "state")?)?,
        working_dir: get_string(row, "working_dir")?,
        worktree_id: get_opt_string(row, "worktree_id")?,
        parent_commit: get_opt_string(row, "parent_commit")?,
        parent_session_id: get_opt_string(row, "parent_session_id")?,
        metadata_json: get_string(row, "metadata_json")?,
        redaction_report: get_string(row, "redaction_report")?,
        started_at: get_i64(row, "started_at")?,
        last_event_at: get_i64(row, "last_event_at")?,
        stopped_at: get_opt_i64(row, "stopped_at")?,
        schema_version: get_i64(row, "schema_version")?,
        sync_revision: get_i64(row, "sync_revision")?,
        repo_id: get_opt_string(row, "repo_id")?,
        workspace_id: get_opt_string(row, "workspace_id")?,
        workspace_fence: get_opt_i64(row, "workspace_fence")?,
        scope_state: CaptureScopeState::parse(&get_string(row, "scope_state")?)?,
    })
}

fn decode_agent_checkpoint(row: &QueryResult) -> StorageResult<AgentCheckpointRecord> {
    Ok(AgentCheckpointRecord {
        checkpoint_id: get_string(row, "checkpoint_id")?,
        session_id: get_string(row, "session_id")?,
        parent_checkpoint_id: get_opt_string(row, "parent_checkpoint_id")?,
        scope: CaptureCheckpointScope::parse(&get_string(row, "scope")?)?,
        parent_commit: get_opt_string(row, "parent_commit")?,
        tree_oid: get_string(row, "tree_oid")?,
        metadata_blob_oid: get_string(row, "metadata_blob_oid")?,
        traces_commit: get_string(row, "traces_commit")?,
        tool_use_id: get_opt_string(row, "tool_use_id")?,
        subagent_session_id: get_opt_string(row, "subagent_session_id")?,
        description: get_opt_string(row, "description")?,
        created_at: get_i64(row, "created_at")?,
        sync_revision: get_i64(row, "sync_revision")?,
    })
}

fn decode_usage_stats(row: &QueryResult) -> StorageResult<AgentUsageStatsRecord> {
    Ok(AgentUsageStatsRecord {
        id: get_string(row, "id")?,
        session_id: get_opt_string(row, "session_id")?,
        thread_id: get_opt_string(row, "thread_id")?,
        agent_run_id: get_opt_string(row, "agent_run_id")?,
        run_id: get_opt_string(row, "run_id")?,
        provider: get_string(row, "provider")?,
        model: get_string(row, "model")?,
        request_kind: get_string(row, "request_kind")?,
        intent: get_opt_string(row, "intent")?,
        prompt_tokens: get_i64(row, "prompt_tokens")?,
        completion_tokens: get_i64(row, "completion_tokens")?,
        cached_tokens: get_i64(row, "cached_tokens")?,
        reasoning_tokens: get_i64(row, "reasoning_tokens")?,
        total_tokens: get_i64(row, "total_tokens")?,
        tool_call_count: get_i64(row, "tool_call_count")?,
        wall_clock_ms: get_i64(row, "wall_clock_ms")?,
        provider_latency_ms: get_opt_i64(row, "provider_latency_ms")?,
        cost_estimate_micro_dollars: get_opt_i64(row, "cost_estimate_micro_dollars")?,
        cost_usd: get_opt_f64(row, "cost_usd")?,
        usage_estimated: get_bool_int(row, "usage_estimated")?,
        started_at: get_opt_string(row, "started_at")?,
        finished_at: get_opt_string(row, "finished_at")?,
        success: get_bool_int(row, "success")?,
        error_kind: get_opt_string(row, "error_kind")?,
        schema_version: get_i64(row, "schema_version")?,
        created_at: get_string(row, "created_at")?,
        agent_name: get_opt_string(row, "agent_name")?,
        repo_id: get_opt_string(row, "repo_id")?,
        turn_id: get_opt_string(row, "turn_id")?,
        event_id: get_opt_string(row, "event_id")?,
    })
}

fn decode_workspace_scope_audit(
    row: &QueryResult,
) -> StorageResult<AgentWorkspaceScopeAuditRecord> {
    Ok(AgentWorkspaceScopeAuditRecord {
        audit_id: get_string(row, "audit_id")?,
        action: get_string(row, "action")?,
        agent_kind: get_string(row, "agent_kind")?,
        provider_session_id: get_string(row, "provider_session_id")?,
        repo_id: get_string(row, "repo_id")?,
        worktree_id: get_string(row, "worktree_id")?,
        workspace_id: get_opt_string(row, "workspace_id")?,
        workspace_fence: get_opt_i64(row, "workspace_fence")?,
        actor: get_opt_string(row, "actor")?,
        created_at: get_i64(row, "created_at")?,
    })
}

async fn exec_upsert_session<C: ConnectionTrait>(
    conn: &C,
    rec: &AgentSessionRecord,
) -> StorageResult<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session \
         (session_id, agent_kind, provider_session_id, thread_id, state, working_dir, \
          worktree_id, parent_commit, parent_session_id, metadata_json, redaction_report, \
          started_at, last_event_at, stopped_at, schema_version, sync_revision, \
          repo_id, workspace_id, workspace_fence, scope_state) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(session_id) DO UPDATE SET \
          agent_kind = excluded.agent_kind, \
          provider_session_id = excluded.provider_session_id, \
          thread_id = excluded.thread_id, \
          state = excluded.state, \
          working_dir = excluded.working_dir, \
          worktree_id = excluded.worktree_id, \
          parent_commit = excluded.parent_commit, \
          parent_session_id = excluded.parent_session_id, \
          metadata_json = excluded.metadata_json, \
          redaction_report = excluded.redaction_report, \
          started_at = excluded.started_at, \
          last_event_at = excluded.last_event_at, \
          stopped_at = excluded.stopped_at, \
          schema_version = excluded.schema_version, \
          sync_revision = excluded.sync_revision, \
          repo_id = excluded.repo_id, \
          workspace_id = excluded.workspace_id, \
          workspace_fence = excluded.workspace_fence, \
          scope_state = excluded.scope_state",
        [
            rec.session_id.clone().into(),
            rec.agent_kind.as_str().into(),
            rec.provider_session_id.clone().into(),
            opt_str_val(&rec.thread_id),
            rec.state.as_str().into(),
            rec.working_dir.clone().into(),
            opt_str_val(&rec.worktree_id),
            opt_str_val(&rec.parent_commit),
            opt_str_val(&rec.parent_session_id),
            rec.metadata_json.clone().into(),
            rec.redaction_report.clone().into(),
            rec.started_at.into(),
            rec.last_event_at.into(),
            opt_i64_val(rec.stopped_at),
            rec.schema_version.into(),
            rec.sync_revision.into(),
            opt_str_val(&rec.repo_id),
            opt_str_val(&rec.workspace_id),
            opt_i64_val(rec.workspace_fence),
            rec.scope_state.as_str().into(),
        ],
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "upsert agent_session", err))?;
    Ok(())
}

async fn exec_upsert_checkpoint<C: ConnectionTrait>(
    conn: &C,
    rec: &AgentCheckpointRecord,
) -> StorageResult<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint \
         (checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit, \
          tree_oid, metadata_blob_oid, traces_commit, tool_use_id, subagent_session_id, \
          description, created_at, sync_revision) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(checkpoint_id) DO UPDATE SET \
          session_id = excluded.session_id, \
          parent_checkpoint_id = excluded.parent_checkpoint_id, \
          scope = excluded.scope, \
          parent_commit = excluded.parent_commit, \
          tree_oid = excluded.tree_oid, \
          metadata_blob_oid = excluded.metadata_blob_oid, \
          traces_commit = excluded.traces_commit, \
          tool_use_id = excluded.tool_use_id, \
          subagent_session_id = excluded.subagent_session_id, \
          description = excluded.description, \
          created_at = excluded.created_at, \
          sync_revision = excluded.sync_revision",
        [
            rec.checkpoint_id.clone().into(),
            rec.session_id.clone().into(),
            opt_str_val(&rec.parent_checkpoint_id),
            rec.scope.as_str().into(),
            opt_str_val(&rec.parent_commit),
            rec.tree_oid.clone().into(),
            rec.metadata_blob_oid.clone().into(),
            rec.traces_commit.clone().into(),
            opt_str_val(&rec.tool_use_id),
            opt_str_val(&rec.subagent_session_id),
            opt_str_val(&rec.description),
            rec.created_at.into(),
            rec.sync_revision.into(),
        ],
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "upsert agent_checkpoint", err))?;
    Ok(())
}

async fn exec_put_session_commit<C: ConnectionTrait>(
    conn: &C,
    rec: &AgentSessionCommitRecord,
) -> StorageResult<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT OR IGNORE INTO agent_session_commit (session_id, hash_kind, commit_oid) \
         VALUES (?, ?, ?)",
        [
            rec.session_id.clone().into(),
            rec.hash_kind.as_str().into(),
            rec.commit_oid.clone().into(),
        ],
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put agent_session_commit", err))?;
    Ok(())
}

async fn exec_put_usage_stats<C: ConnectionTrait>(
    conn: &C,
    rec: &AgentUsageStatsRecord,
) -> StorageResult<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_usage_stats \
         (id, session_id, thread_id, agent_run_id, run_id, provider, model, request_kind, \
          intent, prompt_tokens, completion_tokens, cached_tokens, reasoning_tokens, \
          total_tokens, tool_call_count, wall_clock_ms, provider_latency_ms, \
          cost_estimate_micro_dollars, cost_usd, usage_estimated, started_at, finished_at, \
          success, error_kind, schema_version, created_at, agent_name, repo_id, turn_id, event_id) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
         ON CONFLICT(id) DO UPDATE SET \
          session_id = excluded.session_id, \
          thread_id = excluded.thread_id, \
          agent_run_id = excluded.agent_run_id, \
          run_id = excluded.run_id, \
          provider = excluded.provider, \
          model = excluded.model, \
          request_kind = excluded.request_kind, \
          intent = excluded.intent, \
          prompt_tokens = excluded.prompt_tokens, \
          completion_tokens = excluded.completion_tokens, \
          cached_tokens = excluded.cached_tokens, \
          reasoning_tokens = excluded.reasoning_tokens, \
          total_tokens = excluded.total_tokens, \
          tool_call_count = excluded.tool_call_count, \
          wall_clock_ms = excluded.wall_clock_ms, \
          provider_latency_ms = excluded.provider_latency_ms, \
          cost_estimate_micro_dollars = excluded.cost_estimate_micro_dollars, \
          cost_usd = excluded.cost_usd, \
          usage_estimated = excluded.usage_estimated, \
          started_at = excluded.started_at, \
          finished_at = excluded.finished_at, \
          success = excluded.success, \
          error_kind = excluded.error_kind, \
          schema_version = excluded.schema_version, \
          created_at = excluded.created_at, \
          agent_name = excluded.agent_name, \
          repo_id = excluded.repo_id, \
          turn_id = excluded.turn_id, \
          event_id = excluded.event_id",
        [
            rec.id.clone().into(),
            opt_str_val(&rec.session_id),
            opt_str_val(&rec.thread_id),
            opt_str_val(&rec.agent_run_id),
            opt_str_val(&rec.run_id),
            rec.provider.clone().into(),
            rec.model.clone().into(),
            rec.request_kind.clone().into(),
            opt_str_val(&rec.intent),
            rec.prompt_tokens.into(),
            rec.completion_tokens.into(),
            rec.cached_tokens.into(),
            rec.reasoning_tokens.into(),
            rec.total_tokens.into(),
            rec.tool_call_count.into(),
            rec.wall_clock_ms.into(),
            opt_i64_val(rec.provider_latency_ms),
            opt_i64_val(rec.cost_estimate_micro_dollars),
            opt_f64_val(rec.cost_usd),
            i64::from(rec.usage_estimated).into(),
            opt_str_val(&rec.started_at),
            opt_str_val(&rec.finished_at),
            i64::from(rec.success).into(),
            opt_str_val(&rec.error_kind),
            rec.schema_version.into(),
            rec.created_at.clone().into(),
            opt_str_val(&rec.agent_name),
            opt_str_val(&rec.repo_id),
            opt_str_val(&rec.turn_id),
            opt_str_val(&rec.event_id),
        ],
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put agent_usage_stats", err))?;
    Ok(())
}

async fn exec_append_audit_log<C: ConnectionTrait>(
    conn: &C,
    rec: &AgentAuditLogRecord,
) -> StorageResult<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_audit_log \
         (audit_id, timestamp, user_id, user_name, action, checkpoint_id, scope, \
          export_path, justification, granted) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            rec.audit_id.clone().into(),
            rec.timestamp.clone().into(),
            opt_str_val(&rec.user_id),
            opt_str_val(&rec.user_name),
            rec.action.clone().into(),
            rec.checkpoint_id.clone().into(),
            rec.scope.clone().into(),
            opt_str_val(&rec.export_path),
            opt_str_val(&rec.justification),
            i64::from(rec.granted).into(),
        ],
    ))
    .await
    .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "append agent_audit_log", err))?;
    Ok(())
}

async fn exec_append_workspace_scope_audit<C: ConnectionTrait>(
    conn: &C,
    rec: &AgentWorkspaceScopeAuditRecord,
) -> StorageResult<()> {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_workspace_scope_audit \
         (audit_id, action, agent_kind, provider_session_id, repo_id, worktree_id, \
          workspace_id, workspace_fence, actor, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        [
            rec.audit_id.clone().into(),
            rec.action.clone().into(),
            rec.agent_kind.clone().into(),
            rec.provider_session_id.clone().into(),
            rec.repo_id.clone().into(),
            rec.worktree_id.clone().into(),
            opt_str_val(&rec.workspace_id),
            opt_i64_val(rec.workspace_fence),
            opt_str_val(&rec.actor),
            rec.created_at.into(),
        ],
    ))
    .await
    .map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Capture,
            "append agent_workspace_scope_audit",
            err,
        )
    })?;
    Ok(())
}

async fn get_bridge_session_in<C: ConnectionTrait>(
    conn: &C,
    bridge_session_id: &str,
) -> StorageResult<Option<AgentBridgeSessionRecord>> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT bridge_session_id, source, repository_id, worktree_id, workspace_id, \
                    parent_session_id, agent_id, session_state, schema_version, \
                    last_event_seq, last_acked_seq, created_at, updated_at \
             FROM agent_bridge_session WHERE bridge_session_id = ?",
            [bridge_session_id.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get bridge session", err))?;
    row.as_ref().map(decode_bridge_session).transpose()
}

async fn read_highwater_in<C: ConnectionTrait>(
    conn: &C,
    lane: CaptureQueueLane,
) -> StorageResult<CaptureQueueHighwater> {
    let table = highwater_table_name(lane);
    if !table_exists_in_txn(conn, table).await? {
        return Ok(CaptureQueueHighwater::default());
    }
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT segment_id, byte_offset FROM {table} WHERE singleton = 1"),
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "read queue highwater", err))?;
    let Some(row) = row else {
        return Ok(CaptureQueueHighwater::default());
    };
    let seg_i64 = get_i64(&row, "segment_id")?;
    let off_i64 = get_i64(&row, "byte_offset")?;
    let segment_id = u32::try_from(seg_i64).map_err(|_| StorageBackendError::BackendInternal {
        detail: "corrupt sqlite queue highwater segment_id".to_string(),
    })?;
    let byte_offset = u64::try_from(off_i64).map_err(|_| StorageBackendError::BackendInternal {
        detail: "corrupt sqlite queue highwater byte_offset".to_string(),
    })?;
    Ok(CaptureQueueHighwater {
        segment_id,
        byte_offset,
    })
}

#[async_trait]
impl AgentBridgeStore for SqliteCaptureStore {
    async fn open_bridge_session(
        &self,
        input: &BridgeSessionOpenInput,
    ) -> StorageResult<(AgentBridgeSessionRecord, bool)> {
        validate_bridge_session_open(input)?;
        let txn = self.begin_write().await?;
        let backend = txn.get_database_backend();
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_bridge_session \
                 (bridge_session_id, source, repository_id, worktree_id, workspace_id, \
                  parent_session_id, agent_id, session_state, schema_version, \
                  last_event_seq, last_acked_seq, created_at, updated_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, 'open', 1, 0, 0, ?, ?) \
                 ON CONFLICT(bridge_session_id) DO NOTHING",
                [
                    input.bridge_session_id.clone().into(),
                    BRIDGE_SOURCE_DEEPSEEK_HARNESS.into(),
                    input.repository_id.clone().into(),
                    opt_str_val(&input.worktree_id),
                    opt_str_val(&input.workspace_id),
                    opt_str_val(&input.parent_session_id),
                    opt_str_val(&input.agent_id),
                    input.now_ms.into(),
                    input.now_ms.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "open agent_bridge_session", err)
            })?;
        let created = res.rows_affected() > 0;
        let row = get_bridge_session_in(&txn, &input.bridge_session_id)
            .await?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "bridge session missing after open".to_string(),
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit bridge session", err)
        })?;
        Ok((row, created))
    }

    async fn get_bridge_session(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Option<AgentBridgeSessionRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        get_bridge_session_in(&self.conn, bridge_session_id).await
    }

    async fn set_bridge_session_state(
        &self,
        bridge_session_id: &str,
        state: BridgeSessionState,
        updated_at: i64,
    ) -> StorageResult<()> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let txn = self.begin_write().await?;
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_bridge_session SET session_state = ?, updated_at = ? \
                 WHERE bridge_session_id = ?",
                [
                    state.as_str().into(),
                    updated_at.into(),
                    bridge_session_id.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "update bridge session state", err)
            })?;
        if res.rows_affected() == 0 {
            return Err(StorageBackendError::NotFound {
                resource: "agent_bridge_session".to_string(),
            });
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit bridge session state", err)
        })?;
        Ok(())
    }

    async fn delete_bridge_session(&self, bridge_session_id: &str) -> StorageResult<bool> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let txn = self.begin_write().await?;
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "DELETE FROM agent_bridge_session WHERE bridge_session_id = ?",
                [bridge_session_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "delete agent_bridge_session", err)
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit delete bridge session", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn append_and_ack_events(
        &self,
        bridge_session_id: &str,
        events: &[BridgeEventAppendInput],
        now_ms: i64,
    ) -> StorageResult<BridgeEventAppendOutcome> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let txn = self.begin_write().await?;
        let backend = txn.get_database_backend();

        let Some(session) = get_bridge_session_in(&txn, bridge_session_id).await? else {
            return Err(StorageBackendError::NotFound {
                resource: "agent_bridge_session".to_string(),
            });
        };

        let mut per_event = Vec::with_capacity(events.len());
        let mut max_present: Option<i64> = None;

        for ev in events {
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
            let inserted = txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO agent_bridge_event \
                     (bridge_session_id, event_seq, event_type, payload_sha256, payload, operation_id, created_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(bridge_session_id, event_seq) DO NOTHING",
                    [
                        bridge_session_id.into(),
                        ev.event_seq.into(),
                        ev.event_type.clone().into(),
                        digest.clone().into(),
                        ev.payload.clone().into(),
                        opt_str_val(&ev.operation_id),
                        now_ms.into(),
                    ],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Capture, "insert agent_bridge_event", err)
                })?;

            if inserted.rows_affected() > 0 {
                per_event.push(BridgeEventStatus::Accepted);
                max_present = Some(max_present.map_or(ev.event_seq, |cur| cur.max(ev.event_seq)));
                continue;
            }

            let existing = txn
                .query_one_raw(Statement::from_sql_and_values(
                    backend,
                    "SELECT payload_sha256 FROM agent_bridge_event \
                     WHERE bridge_session_id = ? AND event_seq = ?",
                    [bridge_session_id.into(), ev.event_seq.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Capture, "lookup bridge event digest", err)
                })?;
            match existing {
                Some(row) if get_string(&row, "payload_sha256")? == digest => {
                    per_event.push(BridgeEventStatus::Duplicate);
                    max_present =
                        Some(max_present.map_or(ev.event_seq, |cur| cur.max(ev.event_seq)));
                }
                Some(_) => per_event.push(BridgeEventStatus::DigestConflict),
                None => per_event.push(BridgeEventStatus::Rejected),
            }
        }

        // Advance contiguous ack cursor via recursive point probes on
        // UNIQUE(bridge_session_id, event_seq), stopping at the first gap
        // without scanning or materializing any unacknowledged tail rows.
        let base_acked = session.last_acked_seq.max(0);
        let contiguous_row = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "WITH RECURSIVE run(seq) AS ( \
                     SELECT ? AS seq \
                     UNION ALL \
                     SELECT r.seq + 1 \
                     FROM run r \
                     WHERE EXISTS ( \
                         SELECT 1 FROM agent_bridge_event \
                         WHERE bridge_session_id = ? AND event_seq = r.seq + 1 \
                     ) \
                 ) \
                 SELECT MAX(seq) AS contiguous_seq FROM run",
                [base_acked.into(), bridge_session_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "compute contiguous bridge ack", err)
            })?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "contiguous bridge ack query returned no row".to_string(),
            })?;
        let contiguous = get_i64(&contiguous_row, "contiguous_seq")?;
        let new_last_event_seq =
            max_present.map_or(session.last_event_seq, |m| session.last_event_seq.max(m));
        let new_last_acked_seq = session.last_acked_seq.max(contiguous);

        if max_present.is_some() || new_last_acked_seq > session.last_acked_seq {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_bridge_session \
                 SET last_event_seq = ?, last_acked_seq = ?, updated_at = ? \
                 WHERE bridge_session_id = ?",
                [
                    new_last_event_seq.into(),
                    new_last_acked_seq.into(),
                    now_ms.into(),
                    bridge_session_id.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "advance bridge ack cursors", err)
            })?;
        }

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit append_and_ack_events", err)
        })?;
        Ok(BridgeEventAppendOutcome {
            per_event,
            last_event_seq: new_last_event_seq,
            last_acked_seq: new_last_acked_seq,
        })
    }

    async fn get_bridge_event_by_seq(
        &self,
        bridge_session_id: &str,
        event_seq: i64,
    ) -> StorageResult<Option<AgentBridgeEventRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT event_id, bridge_session_id, event_seq, event_type, \
                        payload_sha256, payload, operation_id, created_at \
                 FROM agent_bridge_event \
                 WHERE bridge_session_id = ? AND event_seq = ?",
                [bridge_session_id.into(), event_seq.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "get bridge event by seq", err)
            })?;
        row.as_ref().map(decode_bridge_event).transpose()
    }

    async fn list_bridge_events(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeEventRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT event_id, bridge_session_id, event_seq, event_type, \
                        payload_sha256, payload, operation_id, created_at \
                 FROM agent_bridge_event \
                 WHERE bridge_session_id = ? ORDER BY event_seq ASC",
                [bridge_session_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "list bridge events", err))?;
        rows.iter().map(decode_bridge_event).collect()
    }

    async fn list_bridge_events_by_time(&self) -> StorageResult<Vec<AgentBridgeEventRecord>> {
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                self.conn.get_database_backend(),
                "SELECT event_id, bridge_session_id, event_seq, event_type, \
                        payload_sha256, payload, operation_id, created_at \
                 FROM agent_bridge_event \
                 ORDER BY created_at ASC, bridge_session_id ASC, event_seq ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list bridge events by time", err)
            })?;
        rows.iter().map(decode_bridge_event).collect()
    }

    async fn put_bridge_checkpoint(
        &self,
        record: &AgentBridgeCheckpointRecord,
    ) -> StorageResult<()> {
        validate_bridge_checkpoint_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_bridge_checkpoint \
             (checkpoint_id, bridge_session_id, agent_checkpoint_id, target_oid, created_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(checkpoint_id) DO UPDATE SET \
              bridge_session_id = excluded.bridge_session_id, \
              agent_checkpoint_id = excluded.agent_checkpoint_id, \
              target_oid = excluded.target_oid, \
              created_at = excluded.created_at",
            [
                record.checkpoint_id.clone().into(),
                record.bridge_session_id.clone().into(),
                opt_str_val(&record.agent_checkpoint_id),
                opt_str_val(&record.target_oid),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put bridge checkpoint", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit bridge checkpoint", err)
        })?;
        Ok(())
    }

    async fn get_bridge_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentBridgeCheckpointRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT checkpoint_id, bridge_session_id, agent_checkpoint_id, target_oid, created_at \
                 FROM agent_bridge_checkpoint WHERE checkpoint_id = ?",
                [checkpoint_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "get bridge checkpoint", err)
            })?;
        row.as_ref().map(decode_bridge_checkpoint).transpose()
    }

    async fn list_bridge_checkpoints(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeCheckpointRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT checkpoint_id, bridge_session_id, agent_checkpoint_id, target_oid, created_at \
                 FROM agent_bridge_checkpoint \
                 WHERE bridge_session_id = ? ORDER BY created_at ASC, checkpoint_id ASC",
                [bridge_session_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list bridge checkpoints", err)
            })?;
        rows.iter().map(decode_bridge_checkpoint).collect()
    }

    async fn put_bridge_operation(&self, record: &AgentBridgeOperationRecord) -> StorageResult<()> {
        validate_bridge_operation_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_bridge_operation \
             (operation_id, bridge_session_id, method, params_digest, repository_id, \
              workspace_id, status, result_digest, created_at, completed_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(operation_id) DO UPDATE SET \
              bridge_session_id = excluded.bridge_session_id, \
              method = excluded.method, \
              params_digest = excluded.params_digest, \
              repository_id = excluded.repository_id, \
              workspace_id = excluded.workspace_id, \
              status = excluded.status, \
              result_digest = excluded.result_digest, \
              created_at = excluded.created_at, \
              completed_at = excluded.completed_at",
            [
                record.operation_id.clone().into(),
                record.bridge_session_id.clone().into(),
                record.method.clone().into(),
                record.params_digest.clone().into(),
                record.repository_id.clone().into(),
                opt_str_val(&record.workspace_id),
                record.status.as_str().into(),
                opt_str_val(&record.result_digest),
                record.created_at.into(),
                opt_i64_val(record.completed_at),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put bridge operation", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit bridge operation", err)
        })?;
        Ok(())
    }

    async fn get_bridge_operation(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<AgentBridgeOperationRecord>> {
        validate_non_empty_no_nul(operation_id, "operation_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT operation_id, bridge_session_id, method, params_digest, repository_id, \
                        workspace_id, status, result_digest, created_at, completed_at \
                 FROM agent_bridge_operation WHERE operation_id = ?",
                [operation_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get bridge operation", err))?;
        row.as_ref().map(decode_bridge_operation).transpose()
    }

    async fn list_bridge_operations(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeOperationRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT operation_id, bridge_session_id, method, params_digest, repository_id, \
                        workspace_id, status, result_digest, created_at, completed_at \
                 FROM agent_bridge_operation \
                 WHERE bridge_session_id = ? ORDER BY created_at ASC, operation_id ASC",
                [bridge_session_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list bridge operations", err)
            })?;
        rows.iter().map(decode_bridge_operation).collect()
    }

    async fn insert_bridge_link(
        &self,
        link: &BridgeLinkWrite,
    ) -> StorageResult<(AgentBridgeLinkRecord, bool)> {
        validate_bridge_link_write(link)?;
        let txn = self.begin_write().await?;
        let backend = txn.get_database_backend();
        if get_bridge_session_in(&txn, &link.bridge_session_id)
            .await?
            .is_none()
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "agent_bridge_link foreign key violation".to_string(),
            });
        }
        // Check for an existing link BEFORE INSERT so idempotent replays do not
        // consume an AUTOINCREMENT `link_id` in `sqlite_sequence`.
        if let Some(existing_row) = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT link_id, bridge_session_id, source_type, source_id, \
                        target_type, target_id, created_at \
                 FROM agent_bridge_link \
                 WHERE source_type = ? AND source_id = ? AND target_type = ? AND target_id = ?",
                [
                    link.source_type.as_str().into(),
                    link.source_id.clone().into(),
                    link.target_type.clone().into(),
                    link.target_id.clone().into(),
                ],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "lookup bridge link", err))?
        {
            let record = decode_bridge_link(&existing_row)?;
            txn.commit().await.map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "commit bridge link", err)
            })?;
            return Ok((record, false));
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_bridge_link \
             (bridge_session_id, source_type, source_id, target_type, target_id, created_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            [
                link.bridge_session_id.clone().into(),
                link.source_type.as_str().into(),
                link.source_id.clone().into(),
                link.target_type.clone().into(),
                link.target_id.clone().into(),
                link.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "insert bridge link", err))?;
        let row = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT link_id, bridge_session_id, source_type, source_id, \
                        target_type, target_id, created_at \
                 FROM agent_bridge_link \
                 WHERE source_type = ? AND source_id = ? AND target_type = ? AND target_id = ?",
                [
                    link.source_type.as_str().into(),
                    link.source_id.clone().into(),
                    link.target_type.clone().into(),
                    link.target_id.clone().into(),
                ],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "read bridge link", err))?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "bridge link missing after insert".to_string(),
            })?;
        let record = decode_bridge_link(&row)?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit bridge link", err))?;
        Ok((record, true))
    }

    async fn list_bridge_links(
        &self,
        bridge_session_id: &str,
    ) -> StorageResult<Vec<AgentBridgeLinkRecord>> {
        validate_non_empty_no_nul(bridge_session_id, "bridge_session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT link_id, bridge_session_id, source_type, source_id, \
                        target_type, target_id, created_at \
                 FROM agent_bridge_link \
                 WHERE bridge_session_id = ? ORDER BY link_id ASC",
                [bridge_session_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "list bridge links", err))?;
        rows.iter().map(decode_bridge_link).collect()
    }
}

#[async_trait]
impl CaptureStore for SqliteCaptureStore {
    fn backend_kind(&self) -> StorageBackendKind {
        StorageBackendKind::Sqlite
    }

    fn is_read_only(&self) -> bool {
        self.read_only
    }

    async fn append_capture_batch(
        &self,
        items: &[CaptureBatchItem],
        highwater_advance: Option<(CaptureQueueLane, CaptureQueueHighwater)>,
    ) -> StorageResult<CaptureBatchOutcome> {
        for item in items {
            validate_capture_batch_item(item)?;
        }
        let txn = self.begin_write().await?;
        let backend = txn.get_database_backend();
        ensure_batch_internal_tables(&txn).await?;

        if let Some((lane, next_hw)) = highwater_advance {
            let current_hw = read_highwater_in(&txn, lane).await?;
            if next_hw < current_hw {
                return Err(StorageBackendError::CorruptCommittedFrame {
                    detail: format!(
                        "capture queue highwater regression on {} lane",
                        lane.as_str()
                    ),
                });
            }
        }

        let mut applied_count = 0usize;
        let mut deduplicated_count = 0usize;

        for item in items {
            let inserted = txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT OR IGNORE INTO capture_dedup_keys (dedup_key) VALUES (?)",
                    [item.dedup_key.clone().into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Capture, "insert capture dedup key", err)
                })?;
            if inserted.rows_affected() == 0 {
                deduplicated_count += 1;
                continue;
            }
            match &item.payload {
                CaptureBatchPayload::UpsertSession(rec) => exec_upsert_session(&txn, rec).await?,
                CaptureBatchPayload::UpsertCheckpoint(rec) => {
                    exec_upsert_checkpoint(&txn, rec).await?;
                }
                CaptureBatchPayload::UpsertSessionCommit(rec) => {
                    exec_put_session_commit(&txn, rec).await?;
                }
                CaptureBatchPayload::InsertUsageStats(rec) => {
                    exec_put_usage_stats(&txn, rec).await?;
                }
                CaptureBatchPayload::AppendAuditLog(rec) => {
                    exec_append_audit_log(&txn, rec).await?;
                }
                CaptureBatchPayload::AppendWorkspaceScopeAudit(rec) => {
                    exec_append_workspace_scope_audit(&txn, rec).await?;
                }
            }
            applied_count += 1;
        }

        let highwater = if let Some((lane, next_hw)) = highwater_advance {
            let table = highwater_table_name(lane);
            let seg = i64::from(next_hw.segment_id);
            let off = i64::try_from(next_hw.byte_offset).map_err(|_| {
                StorageBackendError::BackendInternal {
                    detail: "queue highwater byte_offset exceeds i64::MAX".to_string(),
                }
            })?;
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "INSERT INTO {table} (singleton, segment_id, byte_offset) VALUES (1, ?, ?) \
                     ON CONFLICT(singleton) DO UPDATE SET \
                      segment_id = excluded.segment_id, \
                      byte_offset = excluded.byte_offset"
                ),
                [seg.into(), off.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "update queue highwater", err)
            })?;
            Some(next_hw)
        } else {
            None
        };

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit append_capture_batch", err)
        })?;
        Ok(CaptureBatchOutcome {
            applied_count,
            deduplicated_count,
            highwater,
        })
    }

    async fn get_queue_highwater(
        &self,
        lane: CaptureQueueLane,
    ) -> StorageResult<CaptureQueueHighwater> {
        let txn = self.conn.begin().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "begin read queue highwater", err)
        })?;
        let hw = read_highwater_in(&txn, lane).await?;
        txn.rollback().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "end read queue highwater", err)
        })?;
        Ok(hw)
    }

    async fn put_schema_version(&self, record: &CaptureSchemaVersionRecord) -> StorageResult<()> {
        validate_schema_version_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO schema_versions (version, name, applied_at) VALUES (?, ?, ?) \
             ON CONFLICT(version) DO UPDATE SET \
              name = excluded.name, applied_at = excluded.applied_at",
            [
                record.version.into(),
                record.name.clone().into(),
                record.applied_at.clone().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put schema_versions", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit schema_versions", err))
    }

    async fn list_schema_versions(&self) -> StorageResult<Vec<CaptureSchemaVersionRecord>> {
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                self.conn.get_database_backend(),
                "SELECT version, name, applied_at FROM schema_versions ORDER BY version ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "list schema_versions", err))?;
        rows.iter()
            .map(|r| {
                Ok(CaptureSchemaVersionRecord {
                    version: get_i64(r, "version")?,
                    name: get_string(r, "name")?,
                    applied_at: get_string(r, "applied_at")?,
                })
            })
            .collect()
    }

    async fn put_copy_receipt(
        &self,
        record: &CaptureCatalogCopyReceiptRecord,
    ) -> StorageResult<()> {
        validate_copy_receipt_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO capture_catalog_copy_receipt (id, copied_at) VALUES (?, ?) \
             ON CONFLICT(id) DO UPDATE SET copied_at = excluded.copied_at",
            [record.id.into(), record.copied_at.clone().into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put copy receipt", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit copy receipt", err))
    }

    async fn get_copy_receipt(&self) -> StorageResult<Option<CaptureCatalogCopyReceiptRecord>> {
        let row = self
            .conn
            .query_one_raw(Statement::from_string(
                self.conn.get_database_backend(),
                "SELECT id, copied_at FROM capture_catalog_copy_receipt WHERE id = 1".to_string(),
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get copy receipt", err))?;
        row.as_ref()
            .map(|r| {
                Ok(CaptureCatalogCopyReceiptRecord {
                    id: get_i64(r, "id")?,
                    copied_at: get_string(r, "copied_at")?,
                })
            })
            .transpose()
    }

    async fn append_audit_log(&self, record: &AgentAuditLogRecord) -> StorageResult<()> {
        validate_audit_log_record(record)?;
        let txn = self.begin_write().await?;
        exec_append_audit_log(&txn, record).await?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit audit log", err))
    }

    async fn get_audit_log(&self, audit_id: &str) -> StorageResult<Option<AgentAuditLogRecord>> {
        validate_non_empty_no_nul(audit_id, "audit_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT audit_id, timestamp, user_id, user_name, action, checkpoint_id, \
                        scope, export_path, justification, granted \
                 FROM agent_audit_log WHERE audit_id = ?",
                [audit_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get audit log", err))?;
        row.as_ref().map(decode_audit_log).transpose()
    }

    async fn list_audit_logs_by_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Vec<AgentAuditLogRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT audit_id, timestamp, user_id, user_name, action, checkpoint_id, \
                        scope, export_path, justification, granted \
                 FROM agent_audit_log WHERE checkpoint_id = ? \
                 ORDER BY timestamp ASC, audit_id ASC",
                [checkpoint_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list audit logs by checkpoint", err)
            })?;
        rows.iter().map(decode_audit_log).collect()
    }

    async fn put_cloud_base(&self, record: &AgentCaptureCloudBaseRecord) -> StorageResult<()> {
        validate_cloud_base_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_capture_cloud_base (repo_id, remote_generation, updated_at) \
             VALUES (?, ?, ?) \
             ON CONFLICT(repo_id) DO UPDATE SET \
              remote_generation = excluded.remote_generation, \
              updated_at = excluded.updated_at",
            [
                record.repo_id.clone().into(),
                record.remote_generation.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put cloud base", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit cloud base", err))
    }

    async fn get_cloud_base(
        &self,
        repo_id: &str,
    ) -> StorageResult<Option<AgentCaptureCloudBaseRecord>> {
        validate_non_empty_no_nul(repo_id, "repo_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT repo_id, remote_generation, updated_at \
                 FROM agent_capture_cloud_base WHERE repo_id = ?",
                [repo_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get cloud base", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentCaptureCloudBaseRecord {
                    repo_id: get_string(r, "repo_id")?,
                    remote_generation: get_i64(r, "remote_generation")?,
                    updated_at: get_i64(r, "updated_at")?,
                })
            })
            .transpose()
    }

    async fn put_incarnation(&self, record: &AgentCaptureIncarnationRecord) -> StorageResult<()> {
        validate_incarnation_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_capture_incarnation \
             (agent_kind, provider_session_id, next_session_sync_revision, source_namespace, updated_at) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET \
              next_session_sync_revision = excluded.next_session_sync_revision, \
              source_namespace = excluded.source_namespace, \
              updated_at = excluded.updated_at",
            [
                record.agent_kind.clone().into(),
                record.provider_session_id.clone().into(),
                record.next_session_sync_revision.into(),
                record.source_namespace.clone().into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put incarnation", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit incarnation", err))
    }

    async fn get_incarnation(
        &self,
        agent_kind: &str,
        provider_session_id: &str,
    ) -> StorageResult<Option<AgentCaptureIncarnationRecord>> {
        validate_non_empty_no_nul(agent_kind, "agent_kind")?;
        validate_non_empty_no_nul(provider_session_id, "provider_session_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT agent_kind, provider_session_id, next_session_sync_revision, \
                        source_namespace, updated_at \
                 FROM agent_capture_incarnation \
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [agent_kind.into(), provider_session_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get incarnation", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentCaptureIncarnationRecord {
                    agent_kind: get_string(r, "agent_kind")?,
                    provider_session_id: get_string(r, "provider_session_id")?,
                    next_session_sync_revision: get_i64(r, "next_session_sync_revision")?,
                    source_namespace: get_string(r, "source_namespace")?,
                    updated_at: get_i64(r, "updated_at")?,
                })
            })
            .transpose()
    }

    async fn upsert_session(&self, record: &AgentSessionRecord) -> StorageResult<()> {
        validate_agent_session_record(record)?;
        let txn = self.begin_write().await?;
        exec_upsert_session(&txn, record).await?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit session", err))
    }

    async fn get_session(&self, session_id: &str) -> StorageResult<Option<AgentSessionRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT session_id, agent_kind, provider_session_id, thread_id, state, \
                        working_dir, worktree_id, parent_commit, parent_session_id, \
                        metadata_json, redaction_report, started_at, last_event_at, stopped_at, \
                        schema_version, sync_revision, repo_id, workspace_id, workspace_fence, \
                        scope_state \
                 FROM agent_session WHERE session_id = ?",
                [session_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get session", err))?;
        row.as_ref().map(decode_agent_session).transpose()
    }

    async fn list_sessions_by_started_desc(&self) -> StorageResult<Vec<AgentSessionRecord>> {
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                self.conn.get_database_backend(),
                "SELECT session_id, agent_kind, provider_session_id, thread_id, state, \
                        working_dir, worktree_id, parent_commit, parent_session_id, \
                        metadata_json, redaction_report, started_at, last_event_at, stopped_at, \
                        schema_version, sync_revision, repo_id, workspace_id, workspace_fence, \
                        scope_state \
                 FROM agent_session ORDER BY started_at DESC, session_id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "list sessions", err))?;
        rows.iter().map(decode_agent_session).collect()
    }

    async fn delete_session(&self, session_id: &str) -> StorageResult<bool> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let txn = self.begin_write().await?;
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "DELETE FROM agent_session WHERE session_id = ?",
                [session_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "delete session", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit delete session", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn upsert_checkpoint(&self, record: &AgentCheckpointRecord) -> StorageResult<()> {
        validate_agent_checkpoint_record(record)?;
        let txn = self.begin_write().await?;
        exec_upsert_checkpoint(&txn, record).await?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit checkpoint", err))
    }

    async fn get_checkpoint(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentCheckpointRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit, \
                        tree_oid, metadata_blob_oid, traces_commit, tool_use_id, \
                        subagent_session_id, description, created_at, sync_revision \
                 FROM agent_checkpoint WHERE checkpoint_id = ?",
                [checkpoint_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get checkpoint", err))?;
        row.as_ref().map(decode_agent_checkpoint).transpose()
    }

    async fn list_checkpoints_by_session(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentCheckpointRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit, \
                        tree_oid, metadata_blob_oid, traces_commit, tool_use_id, \
                        subagent_session_id, description, created_at, sync_revision \
                 FROM agent_checkpoint WHERE session_id = ? \
                 ORDER BY created_at ASC, checkpoint_id ASC",
                [session_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list checkpoints by session", err)
            })?;
        rows.iter().map(decode_agent_checkpoint).collect()
    }

    async fn delete_checkpoint(&self, checkpoint_id: &str) -> StorageResult<bool> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let txn = self.begin_write().await?;
        let res = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
                [checkpoint_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "delete checkpoint", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit delete checkpoint", err)
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn put_checkpoint_prune_tombstone(
        &self,
        record: &AgentCheckpointPruneTombstoneRecord,
    ) -> StorageResult<()> {
        validate_checkpoint_prune_tombstone(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_checkpoint_prune_tombstone (checkpoint_id, session_id, pruned_at) \
             VALUES (?, ?, ?) \
             ON CONFLICT(checkpoint_id) DO UPDATE SET \
              session_id = excluded.session_id, pruned_at = excluded.pruned_at",
            [
                record.checkpoint_id.clone().into(),
                record.session_id.clone().into(),
                record.pruned_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put prune tombstone", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit prune tombstone", err))
    }

    async fn get_checkpoint_prune_tombstone(
        &self,
        checkpoint_id: &str,
    ) -> StorageResult<Option<AgentCheckpointPruneTombstoneRecord>> {
        validate_non_empty_no_nul(checkpoint_id, "checkpoint_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT checkpoint_id, session_id, pruned_at \
                 FROM agent_checkpoint_prune_tombstone WHERE checkpoint_id = ?",
                [checkpoint_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get prune tombstone", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentCheckpointPruneTombstoneRecord {
                    checkpoint_id: get_string(r, "checkpoint_id")?,
                    session_id: get_string(r, "session_id")?,
                    pruned_at: get_i64(r, "pruned_at")?,
                })
            })
            .transpose()
    }

    async fn upsert_coverage_claim(&self, record: &AgentCoverageClaimRecord) -> StorageResult<()> {
        validate_coverage_claim_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_coverage_claim \
             (session_id, logical_turn_key, coverage_schema_version, coverage_digest, \
              completeness, revision, state, attempt_checkpoint_id, owner, lease_expires_at, \
              fence_token, checkpoint_id, traces_commit, source_channel, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(session_id, logical_turn_key, coverage_schema_version) DO UPDATE SET \
              coverage_digest = excluded.coverage_digest, \
              completeness = excluded.completeness, \
              revision = excluded.revision, \
              state = excluded.state, \
              attempt_checkpoint_id = excluded.attempt_checkpoint_id, \
              owner = excluded.owner, \
              lease_expires_at = excluded.lease_expires_at, \
              fence_token = excluded.fence_token, \
              checkpoint_id = excluded.checkpoint_id, \
              traces_commit = excluded.traces_commit, \
              source_channel = excluded.source_channel, \
              created_at = excluded.created_at, \
              updated_at = excluded.updated_at",
            [
                record.session_id.clone().into(),
                record.logical_turn_key.clone().into(),
                record.coverage_schema_version.into(),
                record.coverage_digest.clone().into(),
                record.completeness.as_str().into(),
                record.revision.into(),
                record.state.as_str().into(),
                opt_str_val(&record.attempt_checkpoint_id),
                opt_str_val(&record.owner),
                opt_i64_val(record.lease_expires_at),
                opt_i64_val(record.fence_token),
                opt_str_val(&record.checkpoint_id),
                opt_str_val(&record.traces_commit),
                record.source_channel.as_str().into(),
                record.created_at.into(),
                record.updated_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "upsert coverage claim", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit coverage claim", err))
    }

    async fn get_coverage_claim(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Option<AgentCoverageClaimRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        validate_non_empty_no_nul(logical_turn_key, "logical_turn_key")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT session_id, logical_turn_key, coverage_schema_version, coverage_digest, \
                        completeness, revision, state, attempt_checkpoint_id, owner, \
                        lease_expires_at, fence_token, checkpoint_id, traces_commit, \
                        source_channel, created_at, updated_at \
                 FROM agent_coverage_claim \
                 WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ?",
                [
                    session_id.into(),
                    logical_turn_key.into(),
                    coverage_schema_version.into(),
                ],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get coverage claim", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentCoverageClaimRecord {
                    session_id: get_string(r, "session_id")?,
                    logical_turn_key: get_string(r, "logical_turn_key")?,
                    coverage_schema_version: get_i64(r, "coverage_schema_version")?,
                    coverage_digest: get_string(r, "coverage_digest")?,
                    completeness: CaptureCompleteness::parse(&get_string(r, "completeness")?)?,
                    revision: get_i64(r, "revision")?,
                    state: CaptureCoverageClaimState::parse(&get_string(r, "state")?)?,
                    attempt_checkpoint_id: get_opt_string(r, "attempt_checkpoint_id")?,
                    owner: get_opt_string(r, "owner")?,
                    lease_expires_at: get_opt_i64(r, "lease_expires_at")?,
                    fence_token: get_opt_i64(r, "fence_token")?,
                    checkpoint_id: get_opt_string(r, "checkpoint_id")?,
                    traces_commit: get_opt_string(r, "traces_commit")?,
                    source_channel: CaptureSourceChannel::parse(&get_string(r, "source_channel")?)?,
                    created_at: get_i64(r, "created_at")?,
                    updated_at: get_i64(r, "updated_at")?,
                })
            })
            .transpose()
    }

    async fn upsert_coverage_conflict(
        &self,
        record: &AgentCoverageConflictRecord,
    ) -> StorageResult<()> {
        validate_coverage_conflict_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_coverage_conflict \
             (session_id, logical_turn_key, coverage_schema_version, incumbent_revision, \
              incumbent_digest, incumbent_checkpoint_id, incoming_digest, \
              incoming_source_channel, incoming_observed_at, incoming_canonical_json, \
              incoming_redaction_report_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(session_id, logical_turn_key, coverage_schema_version) DO UPDATE SET \
              incumbent_revision = excluded.incumbent_revision, \
              incumbent_digest = excluded.incumbent_digest, \
              incumbent_checkpoint_id = excluded.incumbent_checkpoint_id, \
              incoming_digest = excluded.incoming_digest, \
              incoming_source_channel = excluded.incoming_source_channel, \
              incoming_observed_at = excluded.incoming_observed_at, \
              incoming_canonical_json = excluded.incoming_canonical_json, \
              incoming_redaction_report_json = excluded.incoming_redaction_report_json",
            [
                record.session_id.clone().into(),
                record.logical_turn_key.clone().into(),
                record.coverage_schema_version.into(),
                record.incumbent_revision.into(),
                record.incumbent_digest.clone().into(),
                opt_str_val(&record.incumbent_checkpoint_id),
                record.incoming_digest.clone().into(),
                record.incoming_source_channel.as_str().into(),
                record.incoming_observed_at.into(),
                record.incoming_canonical_json.clone().into(),
                record.incoming_redaction_report_json.clone().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "upsert coverage conflict", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit coverage conflict", err)
        })
    }

    async fn get_coverage_conflict(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Option<AgentCoverageConflictRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        validate_non_empty_no_nul(logical_turn_key, "logical_turn_key")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT session_id, logical_turn_key, coverage_schema_version, \
                        incumbent_revision, incumbent_digest, incumbent_checkpoint_id, \
                        incoming_digest, incoming_source_channel, incoming_observed_at, \
                        incoming_canonical_json, incoming_redaction_report_json \
                 FROM agent_coverage_conflict \
                 WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ?",
                [
                    session_id.into(),
                    logical_turn_key.into(),
                    coverage_schema_version.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "get coverage conflict", err)
            })?;
        row.as_ref()
            .map(|r| {
                Ok(AgentCoverageConflictRecord {
                    session_id: get_string(r, "session_id")?,
                    logical_turn_key: get_string(r, "logical_turn_key")?,
                    coverage_schema_version: get_i64(r, "coverage_schema_version")?,
                    incumbent_revision: get_i64(r, "incumbent_revision")?,
                    incumbent_digest: get_string(r, "incumbent_digest")?,
                    incumbent_checkpoint_id: get_opt_string(r, "incumbent_checkpoint_id")?,
                    incoming_digest: get_string(r, "incoming_digest")?,
                    incoming_source_channel: CaptureSourceChannel::parse(&get_string(
                        r,
                        "incoming_source_channel",
                    )?)?,
                    incoming_observed_at: get_i64(r, "incoming_observed_at")?,
                    incoming_canonical_json: get_string(r, "incoming_canonical_json")?,
                    incoming_redaction_report_json: get_string(
                        r,
                        "incoming_redaction_report_json",
                    )?,
                })
            })
            .transpose()
    }

    async fn put_coverage_revision(
        &self,
        record: &AgentCoverageRevisionRecord,
    ) -> StorageResult<()> {
        validate_coverage_revision_record(record)?;
        let txn = self.begin_write().await?;
        let backend = txn.get_database_backend();
        if let Some(existing_row) = txn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT session_id, logical_turn_key, coverage_schema_version, revision, \
                        checkpoint_id, coverage_digest, completeness, source_channel, created_at \
                 FROM agent_coverage_revision \
                 WHERE session_id = ? AND logical_turn_key = ? \
                   AND coverage_schema_version = ? AND revision = ?",
                [
                    record.session_id.clone().into(),
                    record.logical_turn_key.clone().into(),
                    record.coverage_schema_version.into(),
                    record.revision.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Capture,
                    "check existing coverage revision",
                    err,
                )
            })?
        {
            let existing = AgentCoverageRevisionRecord {
                session_id: get_string(&existing_row, "session_id")?,
                logical_turn_key: get_string(&existing_row, "logical_turn_key")?,
                coverage_schema_version: get_i64(&existing_row, "coverage_schema_version")?,
                revision: get_i64(&existing_row, "revision")?,
                checkpoint_id: get_string(&existing_row, "checkpoint_id")?,
                coverage_digest: get_string(&existing_row, "coverage_digest")?,
                completeness: CaptureCompleteness::parse(&get_string(
                    &existing_row,
                    "completeness",
                )?)?,
                source_channel: CaptureSourceChannel::parse(&get_string(
                    &existing_row,
                    "source_channel",
                )?)?,
                created_at: get_i64(&existing_row, "created_at")?,
            };
            if existing == *record {
                txn.commit().await.map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Capture,
                        "commit idempotent coverage revision",
                        err,
                    )
                })?;
                return Ok(());
            }
            return Err(StorageBackendError::BackendInternal {
                detail: "agent_coverage_revision is append-only: conflicting revision rejected"
                    .to_string(),
            });
        }
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_coverage_revision \
             (session_id, logical_turn_key, coverage_schema_version, revision, \
              checkpoint_id, coverage_digest, completeness, source_channel, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                record.session_id.clone().into(),
                record.logical_turn_key.clone().into(),
                record.coverage_schema_version.into(),
                record.revision.into(),
                record.checkpoint_id.clone().into(),
                record.coverage_digest.clone().into(),
                record.completeness.as_str().into(),
                record.source_channel.as_str().into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put coverage revision", err))?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit coverage revision", err)
        })
    }

    async fn list_coverage_revisions(
        &self,
        session_id: &str,
        logical_turn_key: &str,
        coverage_schema_version: i64,
    ) -> StorageResult<Vec<AgentCoverageRevisionRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        validate_non_empty_no_nul(logical_turn_key, "logical_turn_key")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT session_id, logical_turn_key, coverage_schema_version, revision, \
                        checkpoint_id, coverage_digest, completeness, source_channel, created_at \
                 FROM agent_coverage_revision \
                 WHERE session_id = ? AND logical_turn_key = ? AND coverage_schema_version = ? \
                 ORDER BY revision ASC",
                [
                    session_id.into(),
                    logical_turn_key.into(),
                    coverage_schema_version.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list coverage revisions", err)
            })?;
        rows.iter()
            .map(|r| {
                Ok(AgentCoverageRevisionRecord {
                    session_id: get_string(r, "session_id")?,
                    logical_turn_key: get_string(r, "logical_turn_key")?,
                    coverage_schema_version: get_i64(r, "coverage_schema_version")?,
                    revision: get_i64(r, "revision")?,
                    checkpoint_id: get_string(r, "checkpoint_id")?,
                    coverage_digest: get_string(r, "coverage_digest")?,
                    completeness: CaptureCompleteness::parse(&get_string(r, "completeness")?)?,
                    source_channel: CaptureSourceChannel::parse(&get_string(r, "source_channel")?)?,
                    created_at: get_i64(r, "created_at")?,
                })
            })
            .collect()
    }

    async fn upsert_export_job(&self, record: &AgentExportJobRecord) -> StorageResult<()> {
        validate_export_job_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_export_job \
             (job_id, agent_kind, provider_session_id, owner, lease_expires_at, fence_token, \
              observed_generation, processed_generation, state, last_error_code, created_at, \
              updated_at, ttl_expires_at, repo_id, worktree_id, workspace_id, workspace_fence, \
              scope_state) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(job_id) DO UPDATE SET \
              agent_kind = excluded.agent_kind, \
              provider_session_id = excluded.provider_session_id, \
              owner = excluded.owner, \
              lease_expires_at = excluded.lease_expires_at, \
              fence_token = excluded.fence_token, \
              observed_generation = excluded.observed_generation, \
              processed_generation = excluded.processed_generation, \
              state = excluded.state, \
              last_error_code = excluded.last_error_code, \
              created_at = excluded.created_at, \
              updated_at = excluded.updated_at, \
              ttl_expires_at = excluded.ttl_expires_at, \
              repo_id = excluded.repo_id, \
              worktree_id = excluded.worktree_id, \
              workspace_id = excluded.workspace_id, \
              workspace_fence = excluded.workspace_fence, \
              scope_state = excluded.scope_state",
            [
                record.job_id.clone().into(),
                record.agent_kind.clone().into(),
                record.provider_session_id.clone().into(),
                opt_str_val(&record.owner),
                opt_i64_val(record.lease_expires_at),
                opt_i64_val(record.fence_token),
                record.observed_generation.into(),
                record.processed_generation.into(),
                record.state.as_str().into(),
                opt_str_val(&record.last_error_code),
                record.created_at.into(),
                record.updated_at.into(),
                record.ttl_expires_at.into(),
                opt_str_val(&record.repo_id),
                opt_str_val(&record.worktree_id),
                opt_str_val(&record.workspace_id),
                opt_i64_val(record.workspace_fence),
                record.scope_state.as_str().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "upsert export job", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit export job", err))
    }

    async fn get_export_job(&self, job_id: &str) -> StorageResult<Option<AgentExportJobRecord>> {
        validate_non_empty_no_nul(job_id, "job_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT job_id, agent_kind, provider_session_id, owner, lease_expires_at, \
                        fence_token, observed_generation, processed_generation, state, \
                        last_error_code, created_at, updated_at, ttl_expires_at, repo_id, \
                        worktree_id, workspace_id, workspace_fence, scope_state \
                 FROM agent_export_job WHERE job_id = ?",
                [job_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get export job", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentExportJobRecord {
                    job_id: get_string(r, "job_id")?,
                    agent_kind: get_string(r, "agent_kind")?,
                    provider_session_id: get_string(r, "provider_session_id")?,
                    owner: get_opt_string(r, "owner")?,
                    lease_expires_at: get_opt_i64(r, "lease_expires_at")?,
                    fence_token: get_opt_i64(r, "fence_token")?,
                    observed_generation: get_i64(r, "observed_generation")?,
                    processed_generation: get_i64(r, "processed_generation")?,
                    state: CaptureExportJobState::parse(&get_string(r, "state")?)?,
                    last_error_code: get_opt_string(r, "last_error_code")?,
                    created_at: get_i64(r, "created_at")?,
                    updated_at: get_i64(r, "updated_at")?,
                    ttl_expires_at: get_i64(r, "ttl_expires_at")?,
                    repo_id: get_opt_string(r, "repo_id")?,
                    worktree_id: get_opt_string(r, "worktree_id")?,
                    workspace_id: get_opt_string(r, "workspace_id")?,
                    workspace_fence: get_opt_i64(r, "workspace_fence")?,
                    scope_state: CaptureScopeState::parse(&get_string(r, "scope_state")?)?,
                })
            })
            .transpose()
    }

    async fn upsert_import_identity(
        &self,
        record: &AgentImportIdentityRecord,
    ) -> StorageResult<()> {
        validate_import_identity_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_import_identity \
             (identity_id, agent_kind, provider_session_id, source_kind, source_id, \
              schema_version, observed_digest, committed_digest, attempt_id, \
              attempt_checkpoint_id, next_ordinal, state, owner, lease_expires_at, \
              fence_token, last_error_code, created_at, updated_at, repo_id, worktree_id, \
              workspace_id, workspace_fence, scope_state) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(identity_id) DO UPDATE SET \
              agent_kind = excluded.agent_kind, \
              provider_session_id = excluded.provider_session_id, \
              source_kind = excluded.source_kind, \
              source_id = excluded.source_id, \
              schema_version = excluded.schema_version, \
              observed_digest = excluded.observed_digest, \
              committed_digest = excluded.committed_digest, \
              attempt_id = excluded.attempt_id, \
              attempt_checkpoint_id = excluded.attempt_checkpoint_id, \
              next_ordinal = excluded.next_ordinal, \
              state = excluded.state, \
              owner = excluded.owner, \
              lease_expires_at = excluded.lease_expires_at, \
              fence_token = excluded.fence_token, \
              last_error_code = excluded.last_error_code, \
              created_at = excluded.created_at, \
              updated_at = excluded.updated_at, \
              repo_id = excluded.repo_id, \
              worktree_id = excluded.worktree_id, \
              workspace_id = excluded.workspace_id, \
              workspace_fence = excluded.workspace_fence, \
              scope_state = excluded.scope_state",
            [
                record.identity_id.clone().into(),
                record.agent_kind.clone().into(),
                record.provider_session_id.clone().into(),
                record.source_kind.clone().into(),
                record.source_id.clone().into(),
                record.schema_version.into(),
                opt_str_val(&record.observed_digest),
                opt_str_val(&record.committed_digest),
                opt_str_val(&record.attempt_id),
                opt_str_val(&record.attempt_checkpoint_id),
                record.next_ordinal.into(),
                record.state.as_str().into(),
                opt_str_val(&record.owner),
                opt_i64_val(record.lease_expires_at),
                opt_i64_val(record.fence_token),
                opt_str_val(&record.last_error_code),
                record.created_at.into(),
                record.updated_at.into(),
                opt_str_val(&record.repo_id),
                opt_str_val(&record.worktree_id),
                opt_str_val(&record.workspace_id),
                opt_i64_val(record.workspace_fence),
                record.scope_state.as_str().into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "upsert import identity", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit import identity", err))
    }

    async fn get_import_identity(
        &self,
        identity_id: &str,
    ) -> StorageResult<Option<AgentImportIdentityRecord>> {
        validate_non_empty_no_nul(identity_id, "identity_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT identity_id, agent_kind, provider_session_id, source_kind, source_id, \
                        schema_version, observed_digest, committed_digest, attempt_id, \
                        attempt_checkpoint_id, next_ordinal, state, owner, lease_expires_at, \
                        fence_token, last_error_code, created_at, updated_at, repo_id, \
                        worktree_id, workspace_id, workspace_fence, scope_state \
                 FROM agent_import_identity WHERE identity_id = ?",
                [identity_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get import identity", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentImportIdentityRecord {
                    identity_id: get_string(r, "identity_id")?,
                    agent_kind: get_string(r, "agent_kind")?,
                    provider_session_id: get_string(r, "provider_session_id")?,
                    source_kind: get_string(r, "source_kind")?,
                    source_id: get_string(r, "source_id")?,
                    schema_version: get_i64(r, "schema_version")?,
                    observed_digest: get_opt_string(r, "observed_digest")?,
                    committed_digest: get_opt_string(r, "committed_digest")?,
                    attempt_id: get_opt_string(r, "attempt_id")?,
                    attempt_checkpoint_id: get_opt_string(r, "attempt_checkpoint_id")?,
                    next_ordinal: get_i64(r, "next_ordinal")?,
                    state: CaptureImportIdentityState::parse(&get_string(r, "state")?)?,
                    owner: get_opt_string(r, "owner")?,
                    lease_expires_at: get_opt_i64(r, "lease_expires_at")?,
                    fence_token: get_opt_i64(r, "fence_token")?,
                    last_error_code: get_opt_string(r, "last_error_code")?,
                    created_at: get_i64(r, "created_at")?,
                    updated_at: get_i64(r, "updated_at")?,
                    repo_id: get_opt_string(r, "repo_id")?,
                    worktree_id: get_opt_string(r, "worktree_id")?,
                    workspace_id: get_opt_string(r, "workspace_id")?,
                    workspace_fence: get_opt_i64(r, "workspace_fence")?,
                    scope_state: CaptureScopeState::parse(&get_string(r, "scope_state")?)?,
                })
            })
            .transpose()
    }

    async fn put_import_tombstone(&self, record: &AgentImportTombstoneRecord) -> StorageResult<()> {
        validate_import_tombstone_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_import_tombstone \
             (tombstone_id, agent_kind, provider_session_id, erased_session_id, \
              source_fingerprint, erased_at) \
             VALUES (?, ?, ?, ?, ?, ?) \
             ON CONFLICT(tombstone_id) DO UPDATE SET \
              agent_kind = excluded.agent_kind, \
              provider_session_id = excluded.provider_session_id, \
              erased_session_id = excluded.erased_session_id, \
              source_fingerprint = excluded.source_fingerprint, \
              erased_at = excluded.erased_at",
            [
                record.tombstone_id.clone().into(),
                record.agent_kind.clone().into(),
                record.provider_session_id.clone().into(),
                record.erased_session_id.clone().into(),
                opt_str_val(&record.source_fingerprint),
                record.erased_at.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "put import tombstone", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit import tombstone", err))
    }

    async fn get_import_tombstone(
        &self,
        tombstone_id: &str,
    ) -> StorageResult<Option<AgentImportTombstoneRecord>> {
        validate_non_empty_no_nul(tombstone_id, "tombstone_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT tombstone_id, agent_kind, provider_session_id, erased_session_id, \
                        source_fingerprint, erased_at \
                 FROM agent_import_tombstone WHERE tombstone_id = ?",
                [tombstone_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get import tombstone", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentImportTombstoneRecord {
                    tombstone_id: get_string(r, "tombstone_id")?,
                    agent_kind: get_string(r, "agent_kind")?,
                    provider_session_id: get_string(r, "provider_session_id")?,
                    erased_session_id: get_string(r, "erased_session_id")?,
                    source_fingerprint: get_opt_string(r, "source_fingerprint")?,
                    erased_at: get_i64(r, "erased_at")?,
                })
            })
            .transpose()
    }

    async fn list_import_tombstones(&self) -> StorageResult<Vec<AgentImportTombstoneRecord>> {
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                self.conn.get_database_backend(),
                "SELECT tombstone_id, agent_kind, provider_session_id, erased_session_id, \
                        source_fingerprint, erased_at \
                 FROM agent_import_tombstone ORDER BY tombstone_id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list import tombstones", err)
            })?;
        rows.iter()
            .map(|r| {
                Ok(AgentImportTombstoneRecord {
                    tombstone_id: get_string(r, "tombstone_id")?,
                    agent_kind: get_string(r, "agent_kind")?,
                    provider_session_id: get_string(r, "provider_session_id")?,
                    erased_session_id: get_string(r, "erased_session_id")?,
                    source_fingerprint: get_opt_string(r, "source_fingerprint")?,
                    erased_at: get_i64(r, "erased_at")?,
                })
            })
            .collect()
    }

    async fn put_session_commit(&self, record: &AgentSessionCommitRecord) -> StorageResult<()> {
        validate_session_commit_record(record)?;
        let txn = self.begin_write().await?;
        exec_put_session_commit(&txn, record).await?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit session commit", err))
    }

    async fn list_session_commits(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentSessionCommitRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT session_id, hash_kind, commit_oid \
                 FROM agent_session_commit WHERE session_id = ? \
                 ORDER BY hash_kind ASC, commit_oid ASC",
                [session_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "list session commits", err))?;
        rows.iter()
            .map(|r| {
                Ok(AgentSessionCommitRecord {
                    session_id: get_string(r, "session_id")?,
                    hash_kind: CaptureHashKind::parse(&get_string(r, "hash_kind")?)?,
                    commit_oid: get_string(r, "commit_oid")?,
                })
            })
            .collect()
    }

    async fn upsert_subagent_content_claim(
        &self,
        record: &AgentSubagentContentClaimRecord,
    ) -> StorageResult<()> {
        validate_subagent_content_claim(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_content_claim \
             (parent_session_id, provider_kind, source_key, content_schema_version, \
              current_revision, current_checkpoint_id, current_digest, state, attempt_digest, \
              attempt_checkpoint_id, owner, lease_expires_at, fence_token, created_at, \
              updated_at, revision_cursor, sync_revision) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(parent_session_id, provider_kind, source_key, content_schema_version) \
             DO UPDATE SET \
              current_revision = excluded.current_revision, \
              current_checkpoint_id = excluded.current_checkpoint_id, \
              current_digest = excluded.current_digest, \
              state = excluded.state, \
              attempt_digest = excluded.attempt_digest, \
              attempt_checkpoint_id = excluded.attempt_checkpoint_id, \
              owner = excluded.owner, \
              lease_expires_at = excluded.lease_expires_at, \
              fence_token = excluded.fence_token, \
              created_at = excluded.created_at, \
              updated_at = excluded.updated_at, \
              revision_cursor = excluded.revision_cursor, \
              sync_revision = excluded.sync_revision",
            [
                record.parent_session_id.clone().into(),
                record.provider_kind.clone().into(),
                record.source_key.clone().into(),
                record.content_schema_version.into(),
                record.current_revision.into(),
                opt_str_val(&record.current_checkpoint_id),
                opt_str_val(&record.current_digest),
                record.state.as_str().into(),
                opt_str_val(&record.attempt_digest),
                opt_str_val(&record.attempt_checkpoint_id),
                opt_str_val(&record.owner),
                opt_i64_val(record.lease_expires_at),
                record.fence_token.into(),
                record.created_at.into(),
                record.updated_at.into(),
                record.revision_cursor.into(),
                record.sync_revision.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "upsert subagent content claim", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit subagent content claim", err)
        })
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
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT parent_session_id, provider_kind, source_key, content_schema_version, \
                        current_revision, current_checkpoint_id, current_digest, state, \
                        attempt_digest, attempt_checkpoint_id, owner, lease_expires_at, \
                        fence_token, created_at, updated_at, revision_cursor, sync_revision \
                 FROM agent_subagent_content_claim \
                 WHERE parent_session_id = ? AND provider_kind = ? \
                   AND source_key = ? AND content_schema_version = ?",
                [
                    parent_session_id.into(),
                    provider_kind.into(),
                    source_key.into(),
                    content_schema_version.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "get subagent content claim", err)
            })?;
        row.as_ref()
            .map(|r| {
                Ok(AgentSubagentContentClaimRecord {
                    parent_session_id: get_string(r, "parent_session_id")?,
                    provider_kind: get_string(r, "provider_kind")?,
                    source_key: get_string(r, "source_key")?,
                    content_schema_version: get_i64(r, "content_schema_version")?,
                    current_revision: get_i64(r, "current_revision")?,
                    current_checkpoint_id: get_opt_string(r, "current_checkpoint_id")?,
                    current_digest: get_opt_string(r, "current_digest")?,
                    state: CaptureSubagentClaimState::parse(&get_string(r, "state")?)?,
                    attempt_digest: get_opt_string(r, "attempt_digest")?,
                    attempt_checkpoint_id: get_opt_string(r, "attempt_checkpoint_id")?,
                    owner: get_opt_string(r, "owner")?,
                    lease_expires_at: get_opt_i64(r, "lease_expires_at")?,
                    fence_token: get_i64(r, "fence_token")?,
                    created_at: get_i64(r, "created_at")?,
                    updated_at: get_i64(r, "updated_at")?,
                    revision_cursor: get_i64(r, "revision_cursor")?,
                    sync_revision: get_i64(r, "sync_revision")?,
                })
            })
            .transpose()
    }

    async fn put_subagent_content_revision(
        &self,
        record: &AgentSubagentContentRevisionRecord,
    ) -> StorageResult<()> {
        validate_subagent_content_revision(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_content_revision \
             (parent_session_id, provider_kind, source_key, content_schema_version, revision, \
              checkpoint_id, content_digest, source_channel, partial, created_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(parent_session_id, provider_kind, source_key, content_schema_version, revision) \
             DO UPDATE SET \
              checkpoint_id = excluded.checkpoint_id, \
              content_digest = excluded.content_digest, \
              source_channel = excluded.source_channel, \
              partial = excluded.partial, \
              created_at = excluded.created_at",
            [
                record.parent_session_id.clone().into(),
                record.provider_kind.clone().into(),
                record.source_key.clone().into(),
                record.content_schema_version.into(),
                record.revision.into(),
                record.checkpoint_id.clone().into(),
                record.content_digest.clone().into(),
                record.source_channel.as_str().into(),
                i64::from(record.partial).into(),
                record.created_at.into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "put subagent content revision", err)
        })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Capture,
                "commit subagent content revision",
                err,
            )
        })
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
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT parent_session_id, provider_kind, source_key, content_schema_version, \
                        revision, checkpoint_id, content_digest, source_channel, partial, created_at \
                 FROM agent_subagent_content_revision \
                 WHERE parent_session_id = ? AND provider_kind = ? \
                   AND source_key = ? AND content_schema_version = ? \
                 ORDER BY revision ASC",
                [
                    parent_session_id.into(),
                    provider_kind.into(),
                    source_key.into(),
                    content_schema_version.into(),
                ],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Capture,
                    "list subagent content revisions",
                    err,
                )
            })?;
        rows.iter()
            .map(|r| {
                Ok(AgentSubagentContentRevisionRecord {
                    parent_session_id: get_string(r, "parent_session_id")?,
                    provider_kind: get_string(r, "provider_kind")?,
                    source_key: get_string(r, "source_key")?,
                    content_schema_version: get_i64(r, "content_schema_version")?,
                    revision: get_i64(r, "revision")?,
                    checkpoint_id: get_string(r, "checkpoint_id")?,
                    content_digest: get_string(r, "content_digest")?,
                    source_channel: CaptureSubagentSourceChannel::parse(&get_string(
                        r,
                        "source_channel",
                    )?)?,
                    partial: get_bool_int(r, "partial")?,
                    created_at: get_i64(r, "created_at")?,
                })
            })
            .collect()
    }

    async fn upsert_subagent_link(&self, record: &AgentSubagentLinkRecord) -> StorageResult<()> {
        validate_subagent_link_record(record)?;
        let txn = self.begin_write().await?;
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_subagent_link \
             (content_checkpoint_id, parent_session_id, link_state, boundary_checkpoint_id, \
              stable_subagent_id, created_at, updated_at, sync_revision) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
             ON CONFLICT(content_checkpoint_id) DO UPDATE SET \
              parent_session_id = excluded.parent_session_id, \
              link_state = excluded.link_state, \
              boundary_checkpoint_id = excluded.boundary_checkpoint_id, \
              stable_subagent_id = excluded.stable_subagent_id, \
              created_at = excluded.created_at, \
              updated_at = excluded.updated_at, \
              sync_revision = excluded.sync_revision",
            [
                record.content_checkpoint_id.clone().into(),
                record.parent_session_id.clone().into(),
                record.link_state.as_str().into(),
                opt_str_val(&record.boundary_checkpoint_id),
                opt_str_val(&record.stable_subagent_id),
                record.created_at.into(),
                record.updated_at.into(),
                record.sync_revision.into(),
            ],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "upsert subagent link", err))?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit subagent link", err))
    }

    async fn get_subagent_link(
        &self,
        content_checkpoint_id: &str,
    ) -> StorageResult<Option<AgentSubagentLinkRecord>> {
        validate_non_empty_no_nul(content_checkpoint_id, "content_checkpoint_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT content_checkpoint_id, parent_session_id, link_state, \
                        boundary_checkpoint_id, stable_subagent_id, created_at, updated_at, \
                        sync_revision \
                 FROM agent_subagent_link WHERE content_checkpoint_id = ?",
                [content_checkpoint_id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get subagent link", err))?;
        row.as_ref()
            .map(|r| {
                Ok(AgentSubagentLinkRecord {
                    content_checkpoint_id: get_string(r, "content_checkpoint_id")?,
                    parent_session_id: get_string(r, "parent_session_id")?,
                    link_state: CaptureSubagentLinkState::parse(&get_string(r, "link_state")?)?,
                    boundary_checkpoint_id: get_opt_string(r, "boundary_checkpoint_id")?,
                    stable_subagent_id: get_opt_string(r, "stable_subagent_id")?,
                    created_at: get_i64(r, "created_at")?,
                    updated_at: get_i64(r, "updated_at")?,
                    sync_revision: get_i64(r, "sync_revision")?,
                })
            })
            .transpose()
    }

    async fn put_usage_stats(&self, record: &AgentUsageStatsRecord) -> StorageResult<()> {
        validate_usage_stats_record(record)?;
        let txn = self.begin_write().await?;
        exec_put_usage_stats(&txn, record).await?;
        txn.commit()
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "commit usage stats", err))
    }

    async fn get_usage_stats(&self, id: &str) -> StorageResult<Option<AgentUsageStatsRecord>> {
        validate_non_empty_no_nul(id, "id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT id, session_id, thread_id, agent_run_id, run_id, provider, model, \
                        request_kind, intent, prompt_tokens, completion_tokens, cached_tokens, \
                        reasoning_tokens, total_tokens, tool_call_count, wall_clock_ms, \
                        provider_latency_ms, cost_estimate_micro_dollars, cost_usd, \
                        usage_estimated, started_at, finished_at, success, error_kind, \
                        schema_version, created_at, agent_name, repo_id, turn_id, event_id \
                 FROM agent_usage_stats WHERE id = ?",
                [id.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Capture, "get usage stats", err))?;
        row.as_ref().map(decode_usage_stats).transpose()
    }

    async fn list_usage_stats_by_session(
        &self,
        session_id: &str,
    ) -> StorageResult<Vec<AgentUsageStatsRecord>> {
        validate_non_empty_no_nul(session_id, "session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT id, session_id, thread_id, agent_run_id, run_id, provider, model, \
                        request_kind, intent, prompt_tokens, completion_tokens, cached_tokens, \
                        reasoning_tokens, total_tokens, tool_call_count, wall_clock_ms, \
                        provider_latency_ms, cost_estimate_micro_dollars, cost_usd, \
                        usage_estimated, started_at, finished_at, success, error_kind, \
                        schema_version, created_at, agent_name, repo_id, turn_id, event_id \
                 FROM agent_usage_stats WHERE session_id = ? \
                 ORDER BY created_at ASC, id ASC",
                [session_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list usage stats by session", err)
            })?;
        rows.iter().map(decode_usage_stats).collect()
    }

    async fn append_workspace_scope_audit(
        &self,
        record: &AgentWorkspaceScopeAuditRecord,
    ) -> StorageResult<()> {
        validate_workspace_scope_audit_record(record)?;
        let txn = self.begin_write().await?;
        exec_append_workspace_scope_audit(&txn, record).await?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Capture, "commit workspace scope audit", err)
        })
    }

    async fn get_workspace_scope_audit(
        &self,
        audit_id: &str,
    ) -> StorageResult<Option<AgentWorkspaceScopeAuditRecord>> {
        validate_non_empty_no_nul(audit_id, "audit_id")?;
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT audit_id, action, agent_kind, provider_session_id, repo_id, worktree_id, \
                        workspace_id, workspace_fence, actor, created_at \
                 FROM agent_workspace_scope_audit WHERE audit_id = ?",
                [audit_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "get workspace scope audit", err)
            })?;
        row.as_ref().map(decode_workspace_scope_audit).transpose()
    }

    async fn list_workspace_scope_audits(
        &self,
        agent_kind: &str,
        provider_session_id: &str,
    ) -> StorageResult<Vec<AgentWorkspaceScopeAuditRecord>> {
        validate_non_empty_no_nul(agent_kind, "agent_kind")?;
        validate_non_empty_no_nul(provider_session_id, "provider_session_id")?;
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT audit_id, action, agent_kind, provider_session_id, repo_id, worktree_id, \
                        workspace_id, workspace_fence, actor, created_at \
                 FROM agent_workspace_scope_audit \
                 WHERE agent_kind = ? AND provider_session_id = ? \
                 ORDER BY created_at ASC, audit_id ASC",
                [agent_kind.into(), provider_session_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Capture, "list workspace scope audits", err)
            })?;
        rows.iter().map(decode_workspace_scope_audit).collect()
    }
}

#[allow(dead_code)]
fn _assert_db_err_import(_err: DbErr) {}

#[cfg(test)]
mod create_guard_tests {
    use super::*;

    #[test]
    fn sqlite_create_publish_guard_rolls_back_on_drop_and_keeps_on_commit() {
        let dir = tempfile::tempdir().expect("tempdir");
        let tmp = dir.path().join("tmp.db");
        let dest = dir.path().join("dest.db");
        fs::write(&tmp, b"tmp").expect("write tmp");
        fs::write(&dest, b"dest").expect("write dest");

        {
            let mut guard = SqliteCreatePublishGuard {
                tmp_path: tmp.clone(),
                dest_path: dest.clone(),
                parent: dir.path().to_path_buf(),
                published: true,
                committed: false,
            };
            guard.published = true;
        }
        assert!(!tmp.exists(), "uncommitted drop must remove temp file");
        assert!(
            !dest.exists(),
            "uncommitted drop after publish must remove freshly published destination"
        );

        fs::write(&dest, b"dest-ok").expect("write dest-ok");
        {
            let mut guard = SqliteCreatePublishGuard {
                tmp_path: tmp.clone(),
                dest_path: dest.clone(),
                parent: dir.path().to_path_buf(),
                published: true,
                committed: false,
            };
            guard.committed = true;
        }
        assert!(dest.exists(), "committed guard must preserve destination");
    }
}
