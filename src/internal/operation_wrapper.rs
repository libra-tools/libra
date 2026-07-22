//! Transaction wrapper contract for operation-level audit logging.
//!
//! Commit 1 introduces only stable wrapper-facing types that are required by
//! A-5: metadata, snapshot scope, wrapper result, and stage-specific errors.
//! Commit 2 adds transaction skeleton execution (begin -> business -> commit)
//! without snapshot capture/persistence.

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use chrono::Utc;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DatabaseTransaction, DbErr, EntityTrait, QueryFilter,
    TransactionTrait,
};
use thiserror::Error;
use tokio::time::sleep;
use uuid::Uuid;

use crate::internal::{
    branch::Branch,
    db::get_db_conn_instance,
    head::Head,
    model::reference,
    operation::{
        OperationGraphRecord, OperationParentRecord, OperationQueryPage, OperationRecord,
        OperationService, OperationStatus, OperationViewRecord, OperationViewRefRecord,
        OperationViewWorkspaceRecord,
    },
};

const PARENT_RESOLUTION_PAGE_SIZE: u64 = 200;
const DEDUP_WINDOW_SECS: i64 = 5;
const SQLITE_BUSY_MAX_RETRIES: usize = 8;
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 25;

static ACTIVE_OPERATION_KEYS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentSelectionMode {
    SingleLatestSuccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentSelectionResult {
    pub selected: Vec<String>,
    pub scanned_pages: u64,
    pub scanned_items: u64,
    pub success_candidates: u64,
    pub mode: ParentSelectionMode,
}

/// Required command metadata captured by `with_operation_log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationMeta {
    pub command_name: String,
    pub description: String,
    pub actor: String,
    pub repo_id: String,
    pub args_digest: Option<String>,
}

impl OperationMeta {
    /// Validate required fields before entering transaction orchestration.
    pub fn validate(&self) -> Result<(), OperationError> {
        if self.command_name.trim().is_empty() {
            return Err(OperationError::validation("command_name must not be empty"));
        }
        if self.description.trim().is_empty() {
            return Err(OperationError::validation("description must not be empty"));
        }
        if self.actor.trim().is_empty() {
            return Err(OperationError::validation("actor must not be empty"));
        }
        if self.repo_id.trim().is_empty() {
            return Err(OperationError::validation("repo_id must not be empty"));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationParentPolicy {
    pub allow_multi_parent: bool,
    pub max_parents: usize,
}

impl Default for OperationParentPolicy {
    fn default() -> Self {
        Self {
            allow_multi_parent: false,
            max_parents: 1,
        }
    }
}

/// Controls which parts of the final repository view should be captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationScope {
    pub include_refs: bool,
    pub include_workspace: bool,
    pub include_remote_tracking: bool,
    pub parent_policy: OperationParentPolicy,
}

impl Default for OperationScope {
    fn default() -> Self {
        Self {
            include_refs: true,
            include_workspace: true,
            include_remote_tracking: false,
            parent_policy: OperationParentPolicy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentSelectionMetrics {
    pub resolver_mode: ParentSelectionMode,
    pub scanned_pages: u64,
    pub scanned_items: u64,
    pub success_candidates: u64,
    pub selected_parent_count: u64,
    pub selection_latency_us: u64,
}

/// Wrapper return shape: business result and operation identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationResult<T> {
    pub payload: T,
    pub op_id: String,
    pub view_id: String,
    pub end_ts: i64,
    pub view: OperationViewSnapshot,
    pub parent_metrics: ParentSelectionMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationViewSnapshot {
    pub head_kind: String,
    pub head_target: String,
    pub refs: Vec<OperationViewRefRecord>,
    pub workspace: Vec<OperationViewWorkspaceRecord>,
}

/// Stage-specific failures for with_operation_log.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OperationError {
    #[error("invalid operation metadata: {0}")]
    Validation(String),
    #[error("failed to begin operation transaction: {0}")]
    Begin(String),
    #[error("operation business write failed: {0}")]
    Business(String),
    #[error("failed to capture operation snapshot: {0}")]
    Snapshot(String),
    #[error("failed to persist operation record: {0}")]
    Persist(String),
    #[error("failed to commit operation transaction: {0}")]
    Commit(String),
    #[error("failed to rollback operation transaction: {0}")]
    Rollback(String),
}

impl OperationError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    pub fn begin(message: impl Into<String>) -> Self {
        Self::Begin(message.into())
    }

    pub fn business(message: impl Into<String>) -> Self {
        Self::Business(message.into())
    }

    pub fn snapshot(message: impl Into<String>) -> Self {
        Self::Snapshot(message.into())
    }

    pub fn persist(message: impl Into<String>) -> Self {
        Self::Persist(message.into())
    }

    pub fn commit(message: impl Into<String>) -> Self {
        Self::Commit(message.into())
    }

    pub fn rollback(message: impl Into<String>) -> Self {
        Self::Rollback(message.into())
    }
}

fn is_sqlite_busy_operation_error(err: &OperationError) -> bool {
    match err {
        OperationError::Begin(message)
        | OperationError::Snapshot(message)
        | OperationError::Persist(message)
        | OperationError::Commit(message)
        | OperationError::Rollback(message)
        | OperationError::Business(message) => {
            message.contains("database is locked") || message.contains("database schema is locked")
        }
        OperationError::Validation(_) => false,
    }
}

/// Part C W1 (§C.9): the dedup key includes the WORKTREE scope — the same
/// command with identical arguments run concurrently in two worktrees is two
/// legitimate operations, not a duplicate submission.
fn operation_dedup_key(meta: &OperationMeta, scope_key: &str) -> Option<String> {
    meta.args_digest
        .as_ref()
        .filter(|digest| !digest.trim().is_empty())
        .map(|digest| {
            format!(
                "{}::{}::{}::{}",
                meta.repo_id,
                scope_key,
                meta.command_name,
                digest.trim()
            )
        })
}

struct ActiveDedupGuard {
    key: String,
}

impl Drop for ActiveDedupGuard {
    fn drop(&mut self) {
        if let Some(lock) = ACTIVE_OPERATION_KEYS.get()
            && let Ok(mut keys) = lock.lock()
        {
            keys.remove(&self.key);
        }
    }
}

fn try_acquire_active_dedup_guard(key: String) -> Result<ActiveDedupGuard, OperationError> {
    let lock = ACTIVE_OPERATION_KEYS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut keys = lock
        .lock()
        .map_err(|_| OperationError::begin("failed to lock active operation key set"))?;
    if keys.contains(&key) {
        return Err(OperationError::business(format!(
            "duplicate operation in progress for key '{}'",
            key
        )));
    }
    keys.insert(key.clone());
    Ok(ActiveDedupGuard { key })
}

async fn ensure_not_recent_duplicate_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    meta: &OperationMeta,
    now_ts: i64,
    scope_key: &str,
) -> Result<(), OperationError> {
    let Some(digest) = meta
        .args_digest
        .as_ref()
        .map(|v| v.trim())
        .filter(|v| !v.is_empty())
    else {
        return Ok(());
    };

    let records = OperationService::list_operations_by_repo_with_conn(db, &meta.repo_id, 50)
        .await
        .map_err(|err| {
            OperationError::begin(format!(
                "failed to query recent operations for repository '{}': {err}",
                meta.repo_id
            ))
        })?;

    let duplicated = records.into_iter().any(|record| {
        // Part C W1 (§C.9): only THIS worktree's history counts toward the
        // duplicate window.
        record.worktree_id == scope_key
            && record.command_name == meta.command_name
            && record.args_digest.as_deref().map(str::trim) == Some(digest)
            && record.status == OperationStatus::Succeeded
            && record
                .end_ts
                .map(|end_ts| now_ts.saturating_sub(end_ts) <= DEDUP_WINDOW_SECS)
                .unwrap_or(false)
    });

    if duplicated {
        return Err(OperationError::business(format!(
            "duplicate operation rejected within {}s window for command '{}'",
            DEDUP_WINDOW_SECS, meta.command_name
        )));
    }

    Ok(())
}

fn validate_parent_policy(policy: OperationParentPolicy) -> Result<(), OperationError> {
    if policy.max_parents == 0 {
        return Err(OperationError::validation(
            "parent_policy.max_parents must be greater than 0",
        ));
    }
    if !policy.allow_multi_parent && policy.max_parents > 1 {
        return Err(OperationError::validation(
            "parent_policy.max_parents must be 1 when allow_multi_parent is false",
        ));
    }
    Ok(())
}

/// Execute one business write closure in a transaction and return operation ids.
///
/// Commit 2 scope:
/// 1. Validate metadata.
/// 2. Begin transaction.
/// 3. Execute business closure.
/// 4. Commit on success, rollback on business failure.
///
/// Snapshot capture and operation graph persistence are added in later commits.
pub async fn with_operation_log<T, F>(
    meta: OperationMeta,
    scope: OperationScope,
    operation: F,
) -> Result<OperationResult<T>, OperationError>
where
    for<'b> F: FnOnce(
        &'b DatabaseTransaction,
    ) -> Pin<Box<dyn Future<Output = Result<T, DbErr>> + Send + 'b>>,
    F: Send + 'static,
{
    let db = get_db_conn_instance().await;
    with_operation_log_with_conn(&db, meta, scope, operation).await
}

/// Same as [`with_operation_log`] but uses caller-provided database connection.
///
/// This helper is designed for tests and advanced internal callers.
pub async fn with_operation_log_with_conn<T, F>(
    db: &DatabaseConnection,
    meta: OperationMeta,
    scope: OperationScope,
    operation: F,
) -> Result<OperationResult<T>, OperationError>
where
    for<'b> F: FnOnce(
        &'b DatabaseTransaction,
    ) -> Pin<Box<dyn Future<Output = Result<T, DbErr>> + Send + 'b>>,
    F: Send + 'static,
{
    meta.validate()?;
    validate_parent_policy(scope.parent_policy)?;

    let op_id = Uuid::now_v7().to_string();
    let view_id = Uuid::now_v7().to_string();
    let start_ts = Utc::now().timestamp();

    let scope_key = crate::internal::worktree_scope::WorktreeScope::current()
        .storage_key()
        .to_string();
    let _active_dedup_guard = operation_dedup_key(&meta, &scope_key)
        .map(try_acquire_active_dedup_guard)
        .transpose()?;

    ensure_not_recent_duplicate_with_conn(db, &meta, start_ts, &scope_key).await?;

    let mut txn = None;
    let mut parent_selection = None;
    for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
        let opened = db.begin().await.map_err(|err| {
            OperationError::begin(format!(
                "failed to open operation transaction for command '{}': {err}",
                meta.command_name
            ))
        });

        let opened = match opened {
            Ok(v) => v,
            Err(err)
                if is_sqlite_busy_operation_error(&err) && attempt < SQLITE_BUSY_MAX_RETRIES =>
            {
                sleep(Duration::from_millis(
                    SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                ))
                .await;
                continue;
            }
            Err(err) => return Err(err),
        };

        let selection_started_at = Instant::now();
        let selected = resolve_parent_selection_with_conn(
            &opened,
            &meta.repo_id,
            ParentSelectionMode::SingleLatestSuccess,
        )
        .await;

        match selected {
            Ok(result) => {
                parent_selection =
                    Some((result, selection_started_at.elapsed().as_micros() as u64));
                txn = Some(opened);
                break;
            }
            Err(err)
                if is_sqlite_busy_operation_error(&err) && attempt < SQLITE_BUSY_MAX_RETRIES =>
            {
                let _ = opened.rollback().await;
                sleep(Duration::from_millis(
                    SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                ))
                .await;
            }
            Err(err) => {
                let _ = opened.rollback().await;
                return Err(err);
            }
        }
    }

    let txn = txn.ok_or_else(|| {
        OperationError::begin("failed to initialize operation transaction after retries")
    })?;
    let (parent_selection, selection_latency_us) = parent_selection
        .ok_or_else(|| OperationError::begin("failed to resolve parent selection after retries"))?;
    let selected_parents = parent_selection
        .selected
        .into_iter()
        .take(scope.parent_policy.max_parents)
        .collect::<Vec<_>>();

    let payload = match operation(&txn).await {
        Ok(payload) => payload,
        Err(err) => {
            txn.rollback().await.map_err(|rollback_err| {
                OperationError::rollback(format!(
                    "business step failed with '{err}', and rollback also failed: {rollback_err}"
                ))
            })?;
            return Err(OperationError::business(format!(
                "command '{}' business write failed: {err}",
                meta.command_name
            )));
        }
    };

    let end_ts = Utc::now().timestamp();
    let view = collect_final_view_with_conn(&txn, &meta.repo_id, &view_id, scope)
        .await
        .map_err(|err| {
            OperationError::snapshot(format!(
                "failed to collect final transactional view for command '{}': {err}",
                meta.command_name
            ))
        })?;

    let selected_parent_count = selected_parents.len() as u64;
    let parent_metrics = ParentSelectionMetrics {
        resolver_mode: parent_selection.mode,
        scanned_pages: parent_selection.scanned_pages,
        scanned_items: parent_selection.scanned_items,
        success_candidates: parent_selection.success_candidates,
        selected_parent_count,
        selection_latency_us,
    };
    let operation_record = OperationRecord {
        op_id: op_id.clone(),
        repo_id: meta.repo_id.clone(),
        view_id: view_id.clone(),
        command_name: meta.command_name.clone(),
        description: format!(
            "{} | resolver_mode={} scanned_pages={} scanned_items={} success_candidates={} selected_parents={} selection_latency_us={}",
            meta.description,
            match parent_metrics.resolver_mode {
                ParentSelectionMode::SingleLatestSuccess => "single_latest_success",
            },
            parent_metrics.scanned_pages,
            parent_metrics.scanned_items,
            parent_metrics.success_candidates,
            parent_metrics.selected_parent_count,
            parent_metrics.selection_latency_us,
        ),
        actor: meta.actor.clone(),
        args_digest: meta.args_digest.clone(),
        start_ts,
        end_ts: Some(end_ts),
        status: OperationStatus::Succeeded,
        worktree_id: scope_key.clone(),
    };
    let parents = selected_parents
        .into_iter()
        .map(|parent| OperationParentRecord {
            op_id: op_id.clone(),
            parent_op_id: parent,
        })
        .collect::<Vec<_>>();
    let graph = OperationGraphRecord {
        operation: operation_record,
        parents,
        view: OperationViewRecord {
            view_id: view_id.clone(),
            repo_id: meta.repo_id.clone(),
            head_kind: view.head_kind.clone(),
            head_target: view.head_target.clone(),
            created_at: end_ts,
        },
        refs: view.refs.clone(),
        workspace: view.workspace.clone(),
    };

    let persist_result = OperationService::persist_operation_graph_with_conn(&txn, &graph).await;
    if let Err(err) = persist_result {
        let persist_message = format!(
            "failed to persist operation graph for command '{}': {err}",
            meta.command_name
        );
        match txn.rollback().await {
            Ok(()) => return Err(OperationError::persist(persist_message)),
            Err(rollback_err) => {
                return Err(OperationError::rollback(format!(
                    "{persist_message}; rollback after persist failure also failed: {rollback_err}"
                )));
            }
        }
    }

    txn.commit().await.map_err(|err| {
        OperationError::commit(format!(
            "failed to commit operation transaction for command '{}': {err}",
            meta.command_name
        ))
    })?;

    Ok(OperationResult {
        payload,
        op_id,
        view_id,
        end_ts,
        view,
        parent_metrics,
    })
}

/// Resolve parent operations using a stable strategy entrypoint.
///
/// v1 uses single-parent latest-success strategy. The result keeps a vector
/// shape to reserve forward-compatible multi-parent extension.
pub async fn resolve_parent_selection_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
    mode: ParentSelectionMode,
) -> Result<ParentSelectionResult, OperationError> {
    if repo_id.trim().is_empty() {
        return Err(OperationError::validation("repo_id must not be empty"));
    }

    let mut page: u64 = 1;
    let mut scanned_pages = 0;
    let mut scanned_items = 0;

    loop {
        let records = OperationService::list_operations_by_repo_paginated_with_conn(
            db,
            repo_id,
            OperationQueryPage {
                page,
                per_page: PARENT_RESOLUTION_PAGE_SIZE,
            },
        )
        .await
        .map_err(|err| {
            OperationError::begin(format!(
                "failed to resolve parent operation for repository '{}': {err}",
                repo_id
            ))
        })?;

        scanned_pages += 1;
        let items_len = records.items.len() as u64;
        scanned_items += items_len;

        let mut success_candidates = 0;
        let mut selected_parent = None;
        for item in records.items {
            if item.status == OperationStatus::Succeeded {
                success_candidates += 1;
                if selected_parent.is_none() {
                    selected_parent = Some(item.op_id);
                }
            }
        }

        if let Some(parent) = selected_parent {
            return Ok(ParentSelectionResult {
                selected: vec![parent],
                scanned_pages,
                scanned_items,
                success_candidates,
                mode,
            });
        }

        if items_len < records.per_page {
            return Ok(ParentSelectionResult {
                selected: Vec::new(),
                scanned_pages,
                scanned_items,
                success_candidates,
                mode,
            });
        }

        page += 1;
    }
}

/// Resolve the most recent successful operation in a repository for v1 parent strategy.
///
/// The resolver scans recent operations in reverse chronological order and returns the
/// first successful operation id, or `None` when no successful parent exists.
pub async fn resolve_parent_operation_id_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
) -> Result<Option<String>, OperationError> {
    let selection =
        resolve_parent_selection_with_conn(db, repo_id, ParentSelectionMode::SingleLatestSuccess)
            .await?;
    Ok(selection.selected.first().cloned())
}

async fn collect_final_view_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
    view_id: &str,
    scope: OperationScope,
) -> Result<OperationViewSnapshot, DbErr> {
    let head = Head::current_result_with_conn(db).await.map_err(|err| {
        DbErr::Custom(format!(
            "failed to resolve head while collecting operation view: {err}"
        ))
    })?;

    let (head_kind, head_target) = match head {
        Head::Branch(name) => ("branch".to_string(), name),
        Head::Detached(hash) => ("detached".to_string(), hash.to_string()),
    };

    let refs = if scope.include_refs {
        let mut records = Vec::new();

        let local_branches = Branch::list_branches_result_with_conn(db, None)
            .await
            .map_err(|err| DbErr::Custom(format!("failed to list local branches: {err}")))?;
        for branch in local_branches {
            records.push(OperationViewRefRecord {
                view_id: view_id.to_string(),
                ref_kind: "branch".to_string(),
                ref_name: branch.name,
                ref_remote: None,
                target_oid: branch.commit.to_string(),
            });
        }

        if scope.include_remote_tracking {
            let remote_refs = reference::Entity::find()
                .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
                .filter(reference::Column::Remote.is_not_null())
                .all(db)
                .await?;
            for remote_ref in remote_refs {
                let Some(name) = remote_ref.name else {
                    continue;
                };
                let Some(commit) = remote_ref.commit else {
                    continue;
                };
                records.push(OperationViewRefRecord {
                    view_id: view_id.to_string(),
                    ref_kind: "remote_branch".to_string(),
                    ref_name: name,
                    ref_remote: remote_ref.remote,
                    target_oid: commit,
                });
            }
        }

        records
    } else {
        Vec::new()
    };

    let workspace = if scope.include_workspace {
        vec![OperationViewWorkspaceRecord {
            view_id: view_id.to_string(),
            pointer_kind: "head".to_string(),
            pointer_value: head_target.clone(),
        }]
    } else {
        Vec::new()
    };

    let _ = repo_id;

    Ok(OperationViewSnapshot {
        head_kind,
        head_target,
        refs,
        workspace,
    })
}

/*
#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sea_orm::{
        ConnectionTrait, Database, DbBackend, DbErr, Statement,
    };

    use super::{
        ensure_not_recent_duplicate_with_conn, operation_dedup_key,
        resolve_parent_operation_id_with_conn, OperationError, OperationMeta, OperationScope,
        with_operation_log_with_conn,
    };
    use crate::internal::operation::{OperationRecord, OperationService, OperationStatus};

    fn valid_meta() -> OperationMeta {
        OperationMeta {
            command_name: "commit".to_string(),
            description: "record snapshot".to_string(),
            actor: "alice".to_string(),
            repo_id: "repo_1".to_string(),
            args_digest: Some("sha256:abcd".to_string()),
        }
    }

    #[test]
    fn meta_validation_rejects_empty_fields() {
        let mut meta = valid_meta();
        meta.command_name = " ".to_string();
        assert!(matches!(meta.validate(), Err(OperationError::Validation(_))));

        let mut meta = valid_meta();
        meta.repo_id = " ".to_string();
        assert!(matches!(meta.validate(), Err(OperationError::Validation(_))));
    }

    #[test]
    fn scope_default_matches_a5_contract() {
        let scope = OperationScope::default();
        assert!(scope.include_refs);
        assert!(scope.include_workspace);
        assert!(!scope.include_remote_tracking);
    }

    /// Part C W1 (§C.9): the same command + args submitted from ANOTHER
    /// worktree within the 5s window is NOT a duplicate — the window is
    /// scoped by `worktree_id`. The same scope IS still rejected.
    #[tokio::test]
    async fn concurrent_identical_control_actions_in_two_worktrees_not_deduped() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        let now = Utc::now().timestamp();
        // Worktree "wt-a" just succeeded with this exact command + digest.
        let mut record = sample_record("op_prev", OperationStatus::Succeeded, now - 1);
        record.worktree_id = "wt-a".to_string();
        OperationService::insert_operation_with_conn(&db, &record)
            .await
            .unwrap();

        // The identical submission from MAIN ("") passes...
        ensure_not_recent_duplicate_with_conn(&db, &valid_meta(), now, "")
            .await
            .expect("another worktree's history must not dedup this scope");
        // ...and from a third worktree too.
        ensure_not_recent_duplicate_with_conn(&db, &valid_meta(), now, "wt-b")
            .await
            .expect("scopes are independent");
        // The SAME scope within the window is still rejected.
        let err = ensure_not_recent_duplicate_with_conn(&db, &valid_meta(), now, "wt-a")
            .await
            .expect_err("same-scope duplicate within the window is rejected");
        assert!(matches!(err, OperationError::Business(_)));
    }

    /// Part C W1 (§C.9): the in-process active-key set is scope-aware too —
    /// the key embeds the worktree id, so identical metas from different
    /// scopes never collide (`op restore`'s digest is the target op id,
    /// identical in every worktree that restores it).
    #[test]
    fn op_restore_dedup_key_is_scope_aware() {
        let mut meta = valid_meta();
        meta.command_name = "op-restore".to_string();
        meta.args_digest = Some("op_0123".to_string());
        let main_key = operation_dedup_key(&meta, "").expect("key");
        let linked_key = operation_dedup_key(&meta, "wt-a").expect("key");
        assert_ne!(main_key, linked_key, "scope must discriminate the key");
        assert!(linked_key.contains("::wt-a::"));
    }

    fn sample_record(op_id: &str, status: OperationStatus, end_ts: i64) -> OperationRecord {
        OperationRecord {
            op_id: op_id.to_string(),
            repo_id: "repo_1".to_string(),
            view_id: format!("view_{op_id}"),
            command_name: "commit".to_string(),
            description: format!("desc_{op_id}"),
            actor: "alice".to_string(),
            args_digest: Some("sha256:abcd".to_string()),
            start_ts: end_ts - 5,
            end_ts: Some(end_ts),
            status,
            worktree_id: String::new(),
        }
    }

    async fn create_operation_table(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            r#"
            CREATE TABLE operation (
                op_id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                view_id TEXT NOT NULL,
                command_name TEXT NOT NULL,
                description TEXT NOT NULL,
                actor TEXT NOT NULL,
                args_digest TEXT,
                start_ts INTEGER NOT NULL,
                end_ts INTEGER,
                status TEXT NOT NULL,
                worktree_id TEXT NOT NULL DEFAULT ''
            )
            "#
            .to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_operation_graph_tables_missing_view(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE operation_parent (op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE operation_view_ref (view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE operation_view_workspace (view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind))".to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_operation_graph_tables(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE operation_parent (op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE operation_view (view_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,head_kind TEXT NOT NULL,head_target TEXT NOT NULL,created_at INTEGER NOT NULL)".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE operation_view_ref (view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE operation_view_workspace (view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind))".to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_reference_table_without_head(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT,kind TEXT NOT NULL,\"commit\" TEXT,remote TEXT)".to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_reference_table_with_head(db: &sea_orm::DatabaseConnection) {
        create_reference_table_without_head(db).await;
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Head', NULL, NULL)"
                .to_string(),
        ))
        .await
        .unwrap();
    }


    #[tokio::test]
    async fn resolve_parent_operation_picks_latest_successful_record() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;

        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_old_success", OperationStatus::Succeeded, 10),
        )
        .await
        .unwrap();
        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_new_failed", OperationStatus::Failed, 30),
        )
        .await
        .unwrap();
        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_latest_success", OperationStatus::Succeeded, 40),
        )
        .await
        .unwrap();

        let parent = resolve_parent_operation_id_with_conn(&db, "repo_1")
            .await
            .unwrap();

        assert_eq!(parent.as_deref(), Some("op_latest_success"));
    }

    #[tokio::test]
    async fn resolve_parent_operation_returns_none_when_no_success_exists() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;

        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_failed", OperationStatus::Failed, 10),
        )
        .await
        .unwrap();
        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_running", OperationStatus::Running, 20),
        )
        .await
        .unwrap();

        let parent = resolve_parent_operation_id_with_conn(&db, "repo_1")
            .await
            .unwrap();

        assert!(parent.is_none());
    }

    async fn create_tx_probe_table(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE tx_probe (id INTEGER PRIMARY KEY)".to_string(),
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn with_operation_log_returns_payload_and_ids_on_success() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();
        let before = Utc::now().timestamp();
        let result = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) }),
        )
        .await
        .unwrap();
        let after = Utc::now().timestamp();

        assert_eq!(result.payload, "ok");
        assert!(!result.op_id.is_empty());
        assert!(!result.view_id.is_empty());
        assert!(result.end_ts >= before);
        assert!(result.end_ts <= after);

        let op = OperationService::find_operation_by_id_with_conn(&db, &result.op_id)
            .await
            .unwrap()
            .unwrap();
        let persisted_end_ts = op.end_ts.expect("persisted operation must have end_ts");
        assert!(op.start_ts <= persisted_end_ts);
    }

    #[tokio::test]
    async fn with_operation_log_captures_final_view_and_persists_graph() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();

        let parent_seed = sample_record("op_seed_success", OperationStatus::Succeeded, 10);
        OperationService::insert_operation_with_conn(&db, &parent_seed)
            .await
            .unwrap();

        let result = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) }),
        )
        .await
        .unwrap();

        assert_eq!(result.payload, "ok");
        assert_eq!(result.view.head_kind, "branch");
        assert_eq!(result.view.head_target, "main");
        assert_eq!(result.view.workspace.len(), 1);
        assert_eq!(result.view.workspace[0].pointer_kind, "head");
        assert_eq!(result.view.workspace[0].pointer_value, "main");
        assert_eq!(result.view.refs.len(), 1);
        assert_eq!(result.view.refs[0].ref_kind, "branch");
        assert_eq!(result.view.refs[0].ref_name, "main");
        assert_eq!(result.view.refs[0].target_oid, "1111111111111111111111111111111111111111");

        let graph = OperationService::load_restore_view_by_operation_with_conn(&db, &result.op_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(graph.operation.view_id, result.view_id);
        assert_eq!(graph.view.head_kind, "branch");
        assert_eq!(graph.view.head_target, "main");
        assert_eq!(graph.refs.len(), 1);
        assert_eq!(graph.workspace.len(), 1);
        assert_eq!(graph.parents.len(), 1);
        assert_eq!(graph.parents[0].parent_op_id, "op_seed_success");
    }

    #[tokio::test]
    async fn with_operation_log_rolls_back_when_persist_fails() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables_missing_view(&db).await;
        create_reference_table_with_head(&db).await;
        create_tx_probe_table(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();

        let error = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |txn| {
                Box::pin(async move {
                    txn.execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "INSERT INTO tx_probe(id) VALUES(2)".to_string(),
                    ))
                    .await?;
                    Ok::<_, DbErr>(())
                })
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, OperationError::Persist(_) | OperationError::Rollback(_)));

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM tx_probe WHERE id = 2".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let count: i64 = row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn with_operation_log_rolls_back_on_snapshot_failure() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_without_head(&db).await;
        create_tx_probe_table(&db).await;

        let error = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |txn| {
                Box::pin(async move {
                    txn.execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "INSERT INTO tx_probe(id) VALUES(3)".to_string(),
                    ))
                    .await?;
                    Ok::<_, DbErr>(())
                })
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, OperationError::Snapshot(_)));

        let tx_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM tx_probe WHERE id = 3".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let tx_count: i64 = tx_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(tx_count, 0);

        let op_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM operation".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let op_count: i64 = op_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(op_count, 0);

        let view_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM operation_view".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let view_count: i64 = view_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(view_count, 0);

        let parent_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM operation_parent".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let parent_count: i64 = parent_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(parent_count, 0);
    }

    #[tokio::test]
    async fn with_operation_log_builds_parent_chain_and_restore_graphs() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();

        let first = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("first".to_string()) }),
        )
        .await
        .unwrap();

        let second = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("second".to_string()) }),
        )
        .await
        .unwrap();

        let first_graph = OperationService::load_restore_view_by_operation_with_conn(&db, &first.op_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_graph.parents.len(), 0);

        let second_graph = OperationService::load_restore_view_by_operation_with_conn(&db, &second.op_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second_graph.parents.len(), 1);
        assert_eq!(second_graph.parents[0].parent_op_id, first.op_id);
        assert_eq!(second_graph.refs.len(), 1);
        assert_eq!(second_graph.workspace.len(), 1);
    }

    #[tokio::test]
    async fn with_operation_log_rolls_back_on_business_failure() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;
        create_tx_probe_table(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();
        let error = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |txn| {
                Box::pin(async move {
                    txn.execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "INSERT INTO tx_probe(id) VALUES(1)".to_string(),
                    ))
                    .await?;
                    Err::<(), DbErr>(DbErr::Custom("boom".to_string()))
                })
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, OperationError::Business(_)));

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM tx_probe".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let count: i64 = row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(count, 0);

        let op_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM operation".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let op_count: i64 = op_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(op_count, 0);

        let view_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM operation_view".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let view_count: i64 = view_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(view_count, 0);

        let parent_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM operation_parent".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let parent_count: i64 = parent_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(parent_count, 0);
    }
}
*/
