//! Operation service skeleton for command-level audit persistence.
//!
//! This module defines stable public types for A-6. Commit 2 introduces the
//! operation main-table base DAO methods while keeping transaction ownership in
//! callers through `*_with_conn` signatures.

use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait,
    QueryFilter, QueryOrder, QuerySelect, Select,
};
use thiserror::Error;

use crate::internal::model::{
    operation, operation_parent, operation_view, operation_view_ref, operation_view_workspace,
};

/// Stable status of an operation record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatus {
    Running,
    Succeeded,
    Failed,
    Canceled,
}

impl OperationStatus {
    fn as_db_value(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Canceled => "canceled",
        }
    }

    fn from_db_value(value: &str) -> Result<Self, OperationServiceError> {
        match value {
            "running" => Ok(Self::Running),
            "succeeded" => Ok(Self::Succeeded),
            "failed" => Ok(Self::Failed),
            "canceled" => Ok(Self::Canceled),
            other => Err(OperationServiceError::Internal(format!(
                "unknown operation status value in storage: {other}"
            ))),
        }
    }
}

/// Immutable operation record payload used by DAO/service boundaries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationRecord {
    pub op_id: String,
    pub repo_id: String,
    pub view_id: String,
    pub command_name: String,
    pub description: String,
    pub actor: String,
    pub args_digest: Option<String>,
    pub start_ts: i64,
    pub end_ts: Option<i64>,
    pub status: OperationStatus,
    /// Worktree scope the operation ran in (Part C W1 §C.9): main = `""`,
    /// linked = its stable instance id.
    pub worktree_id: String,
}

/// Parent edge for operation lineage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationParentRecord {
    pub op_id: String,
    pub parent_op_id: String,
}

/// Snapshot header for one operation view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationViewRecord {
    pub view_id: String,
    pub repo_id: String,
    pub head_kind: String,
    pub head_target: String,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationViewRefRecord {
    pub view_id: String,
    pub ref_kind: String,
    pub ref_name: String,
    pub ref_remote: Option<String>,
    pub target_oid: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationViewWorkspaceRecord {
    pub view_id: String,
    pub pointer_kind: String,
    pub pointer_value: String,
}

/// Aggregated operation graph used by compose persistence/read APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationGraphRecord {
    pub operation: OperationRecord,
    pub parents: Vec<OperationParentRecord>,
    pub view: OperationViewRecord,
    pub refs: Vec<OperationViewRefRecord>,
    pub workspace: Vec<OperationViewWorkspaceRecord>,
}

/// Lightweight operation list item for op log pagination APIs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLogListItem {
    pub op_id: String,
    pub command_name: String,
    pub description: String,
    pub actor: String,
    pub end_ts: Option<i64>,
    pub status: OperationStatus,
}

/// Generic pagination request for operation list APIs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationQueryPage {
    pub page: u64,
    pub per_page: u64,
}

impl Default for OperationQueryPage {
    fn default() -> Self {
        Self {
            page: 1,
            per_page: Self::DEFAULT_PER_PAGE,
        }
    }
}

impl OperationQueryPage {
    pub const DEFAULT_PER_PAGE: u64 = 50;
    pub const MAX_PER_PAGE: u64 = 200;

    /// Clamp invalid pagination input into a safe query range.
    pub fn normalized(self) -> Self {
        let page = if self.page == 0 { 1 } else { self.page };
        let per_page = if self.per_page == 0 {
            Self::DEFAULT_PER_PAGE
        } else {
            self.per_page.clamp(1, Self::MAX_PER_PAGE)
        };
        Self { page, per_page }
    }

    pub fn offset(self) -> u64 {
        let normalized = self.normalized();
        (normalized.page - 1) * normalized.per_page
    }
}

/// Generic paginated operation list response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationPage<T> {
    pub items: Vec<T>,
    pub page: u64,
    pub per_page: u64,
    pub total: u64,
}

#[derive(Debug, Error)]
pub enum OperationServiceError {
    #[error("invalid operation argument: {0}")]
    InvalidArgument(String),
    #[error("operation storage error: {0}")]
    Storage(String),
    #[error("operation internal error: {0}")]
    Internal(String),
}

/// Operation service placeholder.
///
/// Commit 2 adds operation main table base DAO methods. Higher-level graph
/// persistence and wrapper orchestration are introduced in later commits.
#[derive(Debug, Default)]
pub struct OperationService;

impl OperationService {
    fn apply_repo_operation_order(query: Select<operation::Entity>) -> Select<operation::Entity> {
        query
            .order_by_desc(operation::Column::EndTs)
            .order_by_desc(operation::Column::StartTs)
            .order_by_desc(operation::Column::OpId)
    }

    fn log_list_item_from_model(
        model: operation::Model,
    ) -> Result<OperationLogListItem, OperationServiceError> {
        let status = OperationStatus::from_db_value(&model.status)?;
        Ok(OperationLogListItem {
            op_id: model.op_id,
            command_name: model.command_name,
            description: model.description,
            actor: model.actor,
            end_ts: model.end_ts,
            status,
        })
    }

    fn record_from_model(
        model: operation::Model,
    ) -> Result<OperationRecord, OperationServiceError> {
        let status = OperationStatus::from_db_value(&model.status)?;
        Ok(OperationRecord {
            op_id: model.op_id,
            repo_id: model.repo_id,
            view_id: model.view_id,
            command_name: model.command_name,
            description: model.description,
            actor: model.actor,
            args_digest: model.args_digest,
            start_ts: model.start_ts,
            end_ts: model.end_ts,
            status,
            worktree_id: model.worktree_id,
        })
    }

    /// Insert one operation main record using an existing connection or transaction.
    pub async fn insert_operation_with_conn<C: ConnectionTrait>(
        db: &C,
        record: &OperationRecord,
    ) -> Result<OperationRecord, OperationServiceError> {
        Self::validate_record(record)?;

        let model = operation::ActiveModel {
            op_id: Set(record.op_id.clone()),
            repo_id: Set(record.repo_id.clone()),
            view_id: Set(record.view_id.clone()),
            command_name: Set(record.command_name.clone()),
            description: Set(record.description.clone()),
            actor: Set(record.actor.clone()),
            args_digest: Set(record.args_digest.clone()),
            start_ts: Set(record.start_ts),
            end_ts: Set(record.end_ts),
            status: Set(record.status.as_db_value().to_string()),
            worktree_id: Set(record.worktree_id.clone()),
        };

        let inserted = model.insert(db).await.map_err(|err| {
            OperationServiceError::Storage(format!(
                "failed to insert operation '{}' for repository '{}': {err}",
                record.op_id, record.repo_id
            ))
        })?;

        Self::record_from_model(inserted)
    }

    /// Find one operation main record by operation id.
    pub async fn find_operation_by_id_with_conn<C: ConnectionTrait>(
        db: &C,
        op_id: &str,
    ) -> Result<Option<OperationRecord>, OperationServiceError> {
        if op_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "op_id must not be empty".to_string(),
            ));
        }

        let model = operation::Entity::find_by_id(op_id.to_string())
            .one(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to query operation '{}' from storage: {err}",
                    op_id
                ))
            })?;

        model.map(Self::record_from_model).transpose()
    }

    /// List latest operation main records for a repository.
    pub async fn list_operations_by_repo_with_conn<C: ConnectionTrait>(
        db: &C,
        repo_id: &str,
        limit: u64,
    ) -> Result<Vec<OperationRecord>, OperationServiceError> {
        if repo_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "repo_id must not be empty".to_string(),
            ));
        }
        if limit == 0 {
            return Err(OperationServiceError::InvalidArgument(
                "limit must be greater than 0".to_string(),
            ));
        }

        let models = Self::apply_repo_operation_order(
            operation::Entity::find().filter(operation::Column::RepoId.eq(repo_id)),
        )
        .limit(limit)
        .all(db)
        .await
        .map_err(|err| {
            OperationServiceError::Storage(format!(
                "failed to list operations for repository '{}': {err}",
                repo_id
            ))
        })?;

        models
            .into_iter()
            .map(Self::record_from_model)
            .collect::<Result<Vec<_>, _>>()
    }

    /// List operation log items by repository with pagination ordered by end_ts desc.
    pub async fn list_operations_by_repo_paginated_with_conn<C: ConnectionTrait>(
        db: &C,
        repo_id: &str,
        query: OperationQueryPage,
    ) -> Result<OperationPage<OperationLogListItem>, OperationServiceError> {
        Self::list_operations_by_repo_and_command_paginated_with_conn(db, repo_id, None, query)
            .await
    }

    /// List operation log items by repository and optional command with pagination.
    pub async fn list_operations_by_repo_and_command_paginated_with_conn<C: ConnectionTrait>(
        db: &C,
        repo_id: &str,
        command_name: Option<&str>,
        query: OperationQueryPage,
    ) -> Result<OperationPage<OperationLogListItem>, OperationServiceError> {
        if repo_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "repo_id must not be empty".to_string(),
            ));
        }

        let command_name = command_name
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let query = query.normalized();
        let mut count_query =
            operation::Entity::find().filter(operation::Column::RepoId.eq(repo_id));
        if let Some(command_name) = command_name {
            count_query = count_query.filter(operation::Column::CommandName.eq(command_name));
        }
        let total = count_query.count(db).await.map_err(|err| {
            OperationServiceError::Storage(format!(
                "failed to count operation logs for repository '{}': {err}",
                repo_id
            ))
        })?;

        let mut list_query =
            operation::Entity::find().filter(operation::Column::RepoId.eq(repo_id));
        if let Some(command_name) = command_name {
            list_query = list_query.filter(operation::Column::CommandName.eq(command_name));
        }
        let models = Self::apply_repo_operation_order(list_query)
            .offset(query.offset())
            .limit(query.per_page)
            .all(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to list operation logs for repository '{}': {err}",
                    repo_id
                ))
            })?;

        let items = models
            .into_iter()
            .map(Self::log_list_item_from_model)
            .collect::<Result<Vec<_>, _>>()?;

        Ok(Self::new_page(items, query, total))
    }

    fn parent_from_model(
        model: operation_parent::Model,
    ) -> Result<OperationParentRecord, OperationServiceError> {
        let parent = OperationParentRecord {
            op_id: model.op_id,
            parent_op_id: model.parent_op_id,
        };
        Self::validate_parent_record(&parent)?;
        Ok(parent)
    }

    pub fn validate_parent_record(
        record: &OperationParentRecord,
    ) -> Result<(), OperationServiceError> {
        if record.op_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "op_id must not be empty".to_string(),
            ));
        }
        if record.parent_op_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "parent_op_id must not be empty".to_string(),
            ));
        }
        if record.op_id == record.parent_op_id {
            return Err(OperationServiceError::InvalidArgument(
                "op_id must not equal parent_op_id".to_string(),
            ));
        }
        Ok(())
    }

    /// Insert one operation parent edge.
    pub async fn insert_parent_with_conn<C: ConnectionTrait>(
        db: &C,
        record: &OperationParentRecord,
    ) -> Result<OperationParentRecord, OperationServiceError> {
        Self::validate_parent_record(record)?;

        let model = operation_parent::ActiveModel {
            op_id: Set(record.op_id.clone()),
            parent_op_id: Set(record.parent_op_id.clone()),
        };

        let inserted = model.insert(db).await.map_err(|err| {
            OperationServiceError::Storage(format!(
                "failed to insert operation parent edge ('{}' -> '{}'): {err}",
                record.op_id, record.parent_op_id
            ))
        })?;

        Self::parent_from_model(inserted)
    }

    /// List parent edges of one operation, ordered by parent operation id.
    pub async fn list_parents_with_conn<C: ConnectionTrait>(
        db: &C,
        op_id: &str,
    ) -> Result<Vec<OperationParentRecord>, OperationServiceError> {
        if op_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "op_id must not be empty".to_string(),
            ));
        }

        let models = operation_parent::Entity::find()
            .filter(operation_parent::Column::OpId.eq(op_id))
            .order_by_asc(operation_parent::Column::ParentOpId)
            .all(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to list parent edges for operation '{}': {err}",
                    op_id
                ))
            })?;

        models
            .into_iter()
            .map(Self::parent_from_model)
            .collect::<Result<Vec<_>, _>>()
    }

    fn view_from_model(
        model: operation_view::Model,
    ) -> Result<OperationViewRecord, OperationServiceError> {
        let view = OperationViewRecord {
            view_id: model.view_id,
            repo_id: model.repo_id,
            head_kind: model.head_kind,
            head_target: model.head_target,
            created_at: model.created_at,
        };
        Self::validate_view_record(&view)?;
        Ok(view)
    }

    pub fn validate_view_record(record: &OperationViewRecord) -> Result<(), OperationServiceError> {
        if record.view_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "view_id must not be empty".to_string(),
            ));
        }
        if record.repo_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "repo_id must not be empty".to_string(),
            ));
        }
        if record.head_kind.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "head_kind must not be empty".to_string(),
            ));
        }
        if record.head_target.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "head_target must not be empty".to_string(),
            ));
        }
        if record.created_at < 0 {
            return Err(OperationServiceError::InvalidArgument(
                "created_at must be a unix timestamp in seconds".to_string(),
            ));
        }
        Ok(())
    }

    /// Insert one operation view snapshot.
    pub async fn insert_view_with_conn<C: ConnectionTrait>(
        db: &C,
        record: &OperationViewRecord,
    ) -> Result<OperationViewRecord, OperationServiceError> {
        Self::validate_view_record(record)?;

        let model = operation_view::ActiveModel {
            view_id: Set(record.view_id.clone()),
            repo_id: Set(record.repo_id.clone()),
            head_kind: Set(record.head_kind.clone()),
            head_target: Set(record.head_target.clone()),
            created_at: Set(record.created_at),
        };

        let inserted = model.insert(db).await.map_err(|err| {
            OperationServiceError::Storage(format!(
                "failed to insert operation view '{}' for repository '{}': {err}",
                record.view_id, record.repo_id
            ))
        })?;

        Self::view_from_model(inserted)
    }

    /// Find one operation view by operation id.
    pub async fn find_view_by_operation_with_conn<C: ConnectionTrait>(
        db: &C,
        op_id: &str,
    ) -> Result<Option<OperationViewRecord>, OperationServiceError> {
        if op_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "op_id must not be empty".to_string(),
            ));
        }

        let op_model = operation::Entity::find_by_id(op_id.to_string())
            .one(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to query operation '{}' from storage: {err}",
                    op_id
                ))
            })?;

        let Some(op_model) = op_model else {
            return Ok(None);
        };

        let view_model = operation_view::Entity::find_by_id(op_model.view_id)
            .one(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to query operation view by operation '{}': {err}",
                    op_id
                ))
            })?;

        view_model.map(Self::view_from_model).transpose()
    }

    fn view_ref_from_model(
        model: operation_view_ref::Model,
    ) -> Result<OperationViewRefRecord, OperationServiceError> {
        let record = OperationViewRefRecord {
            view_id: model.view_id,
            ref_kind: model.ref_kind,
            ref_name: model.ref_name,
            ref_remote: if model.ref_remote.is_empty() {
                None
            } else {
                Some(model.ref_remote)
            },
            target_oid: model.target_oid,
        };
        Self::validate_view_ref_record(&record)?;
        Ok(record)
    }

    pub fn validate_view_ref_record(
        record: &OperationViewRefRecord,
    ) -> Result<(), OperationServiceError> {
        if record.view_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "view_id must not be empty".to_string(),
            ));
        }
        if record.ref_kind.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "ref_kind must not be empty".to_string(),
            ));
        }
        if record.ref_name.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "ref_name must not be empty".to_string(),
            ));
        }
        if record.target_oid.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "target_oid must not be empty".to_string(),
            ));
        }
        Ok(())
    }
    pub async fn replace_view_refs_with_conn<C: ConnectionTrait>(
        db: &C,
        view_id: &str,
        refs: &[OperationViewRefRecord],
    ) -> Result<Vec<OperationViewRefRecord>, OperationServiceError> {
        if view_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "view_id must not be empty".to_string(),
            ));
        }
        for record in refs {
            Self::validate_view_ref_record(record)?;
            if record.view_id != view_id {
                return Err(OperationServiceError::InvalidArgument(
                    "all view refs must use the same view_id".to_string(),
                ));
            }
        }
        operation_view_ref::Entity::delete_many()
            .filter(operation_view_ref::Column::ViewId.eq(view_id))
            .exec(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to clear view refs for view '{}': {err}",
                    view_id
                ))
            })?;

        let mut inserted = Vec::with_capacity(refs.len());
        for record in refs {
            let model = operation_view_ref::ActiveModel {
                view_id: Set(record.view_id.clone()),
                ref_kind: Set(record.ref_kind.clone()),
                ref_name: Set(record.ref_name.clone()),
                ref_remote: Set(record.ref_remote.clone().unwrap_or_default()),
                target_oid: Set(record.target_oid.clone()),
            };

            let saved = model.insert(db).await.map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to insert view ref '{}:{}' for view '{}': {err}",
                    record.ref_kind, record.ref_name, view_id
                ))
            })?;
            inserted.push(Self::view_ref_from_model(saved)?);
        }
        Ok(inserted)
    }
    pub async fn list_view_refs_with_conn<C: ConnectionTrait>(
        db: &C,
        view_id: &str,
    ) -> Result<Vec<OperationViewRefRecord>, OperationServiceError> {
        if view_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "view_id must not be empty".to_string(),
            ));
        }
        let models = operation_view_ref::Entity::find()
            .filter(operation_view_ref::Column::ViewId.eq(view_id))
            .order_by_asc(operation_view_ref::Column::RefKind)
            .order_by_asc(operation_view_ref::Column::RefName)
            .order_by_asc(operation_view_ref::Column::RefRemote)
            .all(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to list view refs for view '{}': {err}",
                    view_id
                ))
            })?;

        models
            .into_iter()
            .map(Self::view_ref_from_model)
            .collect::<Result<Vec<_>, _>>()
    }

    fn view_workspace_from_model(
        model: operation_view_workspace::Model,
    ) -> Result<OperationViewWorkspaceRecord, OperationServiceError> {
        let record = OperationViewWorkspaceRecord {
            view_id: model.view_id,
            pointer_kind: model.pointer_kind,
            pointer_value: model.pointer_value,
        };
        Self::validate_view_workspace_record(&record)?;
        Ok(record)
    }

    pub fn validate_view_workspace_record(
        record: &OperationViewWorkspaceRecord,
    ) -> Result<(), OperationServiceError> {
        if record.view_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "view_id must not be empty".to_string(),
            ));
        }
        if record.pointer_kind.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "pointer_kind must not be empty".to_string(),
            ));
        }
        if record.pointer_value.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "pointer_value must not be empty".to_string(),
            ));
        }
        Ok(())
    }

    /// Upsert one workspace pointer snapshot under one operation view.
    pub async fn upsert_workspace_snapshot_with_conn<C: ConnectionTrait>(
        db: &C,
        record: &OperationViewWorkspaceRecord,
    ) -> Result<OperationViewWorkspaceRecord, OperationServiceError> {
        Self::validate_view_workspace_record(record)?;

        let existing = operation_view_workspace::Entity::find_by_id((
            record.view_id.clone(),
            record.pointer_kind.clone(),
        ))
        .one(db)
        .await
        .map_err(|err| {
            OperationServiceError::Storage(format!(
                "failed to query workspace pointer '{}:{}': {err}",
                record.view_id, record.pointer_kind
            ))
        })?;

        let saved = if let Some(existing) = existing {
            let model = operation_view_workspace::ActiveModel {
                view_id: Set(existing.view_id),
                pointer_kind: Set(existing.pointer_kind),
                pointer_value: Set(record.pointer_value.clone()),
            };

            model.update(db).await.map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to update workspace pointer '{}:{}': {err}",
                    record.view_id, record.pointer_kind
                ))
            })?
        } else {
            let model = operation_view_workspace::ActiveModel {
                view_id: Set(record.view_id.clone()),
                pointer_kind: Set(record.pointer_kind.clone()),
                pointer_value: Set(record.pointer_value.clone()),
            };

            model.insert(db).await.map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to insert workspace pointer '{}:{}': {err}",
                    record.view_id, record.pointer_kind
                ))
            })?
        };

        Self::view_workspace_from_model(saved)
    }

    /// List all workspace pointer snapshots for one operation view.
    pub async fn list_workspace_snapshots_with_conn<C: ConnectionTrait>(
        db: &C,
        view_id: &str,
    ) -> Result<Vec<OperationViewWorkspaceRecord>, OperationServiceError> {
        if view_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "view_id must not be empty".to_string(),
            ));
        }

        let models = operation_view_workspace::Entity::find()
            .filter(operation_view_workspace::Column::ViewId.eq(view_id))
            .order_by_asc(operation_view_workspace::Column::PointerKind)
            .all(db)
            .await
            .map_err(|err| {
                OperationServiceError::Storage(format!(
                    "failed to list workspace pointers for view '{}': {err}",
                    view_id
                ))
            })?;

        models
            .into_iter()
            .map(Self::view_workspace_from_model)
            .collect::<Result<Vec<_>, _>>()
    }

    fn validate_graph_record(graph: &OperationGraphRecord) -> Result<(), OperationServiceError> {
        Self::validate_record(&graph.operation)?;
        Self::validate_view_record(&graph.view)?;

        if graph.operation.view_id != graph.view.view_id {
            return Err(OperationServiceError::InvalidArgument(
                "operation.view_id must equal view.view_id".to_string(),
            ));
        }
        if graph.operation.repo_id != graph.view.repo_id {
            return Err(OperationServiceError::InvalidArgument(
                "operation.repo_id must equal view.repo_id".to_string(),
            ));
        }

        for parent in &graph.parents {
            Self::validate_parent_record(parent)?;
            if parent.op_id != graph.operation.op_id {
                return Err(OperationServiceError::InvalidArgument(
                    "all parent edges must belong to operation.op_id".to_string(),
                ));
            }
        }

        for record in &graph.refs {
            Self::validate_view_ref_record(record)?;
            if record.view_id != graph.view.view_id {
                return Err(OperationServiceError::InvalidArgument(
                    "all view refs must belong to view.view_id".to_string(),
                ));
            }
        }

        for record in &graph.workspace {
            Self::validate_view_workspace_record(record)?;
            if record.view_id != graph.view.view_id {
                return Err(OperationServiceError::InvalidArgument(
                    "all workspace snapshots must belong to view.view_id".to_string(),
                ));
            }
        }

        Ok(())
    }

    /// Persist one full operation graph using a caller-owned connection/transaction.
    pub async fn persist_operation_graph_with_conn<C: ConnectionTrait>(
        db: &C,
        graph: &OperationGraphRecord,
    ) -> Result<OperationGraphRecord, OperationServiceError> {
        Self::validate_graph_record(graph)?;

        let operation = Self::insert_operation_with_conn(db, &graph.operation).await?;
        for parent in &graph.parents {
            Self::insert_parent_with_conn(db, parent).await?;
        }
        let view = Self::insert_view_with_conn(db, &graph.view).await?;
        Self::replace_view_refs_with_conn(db, &view.view_id, &graph.refs).await?;
        for snapshot in &graph.workspace {
            Self::upsert_workspace_snapshot_with_conn(db, snapshot).await?;
        }

        let parents = Self::list_parents_with_conn(db, &operation.op_id).await?;
        let refs = Self::list_view_refs_with_conn(db, &view.view_id).await?;
        let workspace = Self::list_workspace_snapshots_with_conn(db, &view.view_id).await?;

        Ok(OperationGraphRecord {
            operation,
            parents,
            view,
            refs,
            workspace,
        })
    }

    /// Read one full operation graph by operation id.
    pub async fn load_restore_view_by_operation_with_conn<C: ConnectionTrait>(
        db: &C,
        op_id: &str,
    ) -> Result<Option<OperationGraphRecord>, OperationServiceError> {
        let operation = match Self::find_operation_by_id_with_conn(db, op_id).await? {
            Some(record) => record,
            None => return Ok(None),
        };

        let parents = Self::list_parents_with_conn(db, &operation.op_id).await?;
        let view = Self::find_view_by_operation_with_conn(db, &operation.op_id)
            .await?
            .ok_or_else(|| {
                OperationServiceError::Storage(format!(
                    "operation '{}' references missing view '{}'",
                    operation.op_id, operation.view_id
                ))
            })?;
        let refs = Self::list_view_refs_with_conn(db, &view.view_id).await?;
        let workspace = Self::list_workspace_snapshots_with_conn(db, &view.view_id).await?;

        Ok(Some(OperationGraphRecord {
            operation,
            parents,
            view,
            refs,
            workspace,
        }))
    }

    pub fn validate_record(record: &OperationRecord) -> Result<(), OperationServiceError> {
        if record.op_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "op_id must not be empty".to_string(),
            ));
        }
        if record.repo_id.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "repo_id must not be empty".to_string(),
            ));
        }
        if record.command_name.trim().is_empty() {
            return Err(OperationServiceError::InvalidArgument(
                "command_name must not be empty".to_string(),
            ));
        }
        if record.start_ts < 0 {
            return Err(OperationServiceError::InvalidArgument(
                "start_ts must be a unix timestamp in seconds".to_string(),
            ));
        }
        if let Some(end_ts) = record.end_ts
            && end_ts < record.start_ts
        {
            return Err(OperationServiceError::InvalidArgument(
                "end_ts must be greater than or equal to start_ts".to_string(),
            ));
        }

        Ok(())
    }

    pub fn normalize_query_page(query: OperationQueryPage) -> OperationQueryPage {
        query.normalized()
    }

    pub fn new_page<T>(items: Vec<T>, query: OperationQueryPage, total: u64) -> OperationPage<T> {
        let normalized = query.normalized();
        OperationPage {
            items,
            page: normalized.page,
            per_page: normalized.per_page,
            total,
        }
    }
}

/*
#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

    use super::{
        OperationGraphRecord, OperationParentRecord, OperationQueryPage, OperationRecord,
        OperationService, OperationServiceError, OperationStatus, OperationViewRecord,
        OperationViewRefRecord, OperationViewWorkspaceRecord,
    };

    fn sample_record() -> OperationRecord {
        OperationRecord {
            op_id: "op_1".to_string(),
            repo_id: "repo_1".to_string(),
            view_id: "view_1".to_string(),
            command_name: "commit".to_string(),
            description: "commit message".to_string(),
            actor: "alice".to_string(),
            args_digest: Some("sha256:abcd".to_string()),
            start_ts: 100,
            end_ts: Some(120),
            status: OperationStatus::Succeeded,
            worktree_id: String::new(),
        }
    }

    #[test]
    fn commit1_normalize_query_page_clamps_to_limits() {
        let normalized = OperationService::normalize_query_page(OperationQueryPage {
            page: 0,
            per_page: 999,
        });
        assert_eq!(normalized.page, 1);
        assert_eq!(normalized.per_page, OperationQueryPage::MAX_PER_PAGE);

        let normalized = OperationService::normalize_query_page(OperationQueryPage {
            page: 3,
            per_page: 20,
        });
        assert_eq!(normalized.page, 3);
        assert_eq!(normalized.per_page, 20);
    }

    #[test]
    fn commit1_validate_record_rejects_invalid_timestamps() {
        let mut record = sample_record();
        record.end_ts = Some(99);

        let error = OperationService::validate_record(&record).unwrap_err();
        assert!(error
            .to_string()
            .contains("end_ts must be greater than or equal to start_ts"));
    }

    #[tokio::test]
    async fn commit2_insert_rejects_invalid_record() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let mut record = sample_record();
        record.op_id = " ".to_string();

        let error = OperationService::insert_operation_with_conn(&db, &record)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[tokio::test]
    async fn commit2_find_rejects_empty_op_id() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let error = OperationService::find_operation_by_id_with_conn(&db, " ")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[tokio::test]
    async fn commit2_list_rejects_invalid_arguments() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let error = OperationService::list_operations_by_repo_with_conn(&db, "", 1)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));

        let error = OperationService::list_operations_by_repo_with_conn(&db, "repo_1", 0)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[tokio::test]
    async fn commit3_insert_parent_rejects_invalid_edge() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let edge = OperationParentRecord {
            op_id: " ".to_string(),
            parent_op_id: "op_0".to_string(),
        };
        let error = OperationService::insert_parent_with_conn(&db, &edge)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));

        let edge = OperationParentRecord {
            op_id: "op_1".to_string(),
            parent_op_id: "op_1".to_string(),
        };
        let error = OperationService::insert_parent_with_conn(&db, &edge)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[tokio::test]
    async fn commit3_list_parents_rejects_empty_op_id() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let error = OperationService::list_parents_with_conn(&db, " ")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[tokio::test]
    async fn commit3_insert_and_list_parents_roundtrip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS operation_parent (
                op_id TEXT NOT NULL,
                parent_op_id TEXT NOT NULL,
                PRIMARY KEY (op_id, parent_op_id)
            );
            "#,
        ))
        .await
        .unwrap();

        let p1 = OperationParentRecord {
            op_id: "op_2".to_string(),
            parent_op_id: "op_0".to_string(),
        };
        let p2 = OperationParentRecord {
            op_id: "op_2".to_string(),
            parent_op_id: "op_1".to_string(),
        };
        OperationService::insert_parent_with_conn(&db, &p1)
            .await
            .unwrap();
        OperationService::insert_parent_with_conn(&db, &p2)
            .await
            .unwrap();

        let parents = OperationService::list_parents_with_conn(&db, "op_2")
            .await
            .unwrap();
        assert_eq!(parents.len(), 2);
        assert_eq!(parents[0].parent_op_id, "op_0");
        assert_eq!(parents[1].parent_op_id, "op_1");
    }

    #[tokio::test]
    async fn commit4_insert_view_rejects_invalid_record() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let view = OperationViewRecord {
            view_id: " ".to_string(),
            repo_id: "repo_1".to_string(),
            head_kind: "branch".to_string(),
            head_target: "main".to_string(),
            created_at: 100,
        };
        let error = OperationService::insert_view_with_conn(&db, &view)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[tokio::test]
    async fn commit4_find_view_rejects_empty_op_id() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let error = OperationService::find_view_by_operation_with_conn(&db, " ")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[test]
    fn commit4_validate_view_accepts_valid_record() {
        let view = OperationViewRecord {
            view_id: "view_10".to_string(),
            repo_id: "repo_1".to_string(),
            head_kind: "branch".to_string(),
            head_target: "main".to_string(),
            created_at: 101,
        };

        OperationService::validate_view_record(&view).unwrap();
    }

    #[tokio::test]
    async fn commit5_replace_view_refs_rejects_invalid_record() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let refs = vec![OperationViewRefRecord {
            view_id: "view_1".to_string(),
            ref_kind: "branch".to_string(),
            ref_name: "main".to_string(),
            ref_remote: None,
            target_oid: " ".to_string(),
        }];
        let error = OperationService::replace_view_refs_with_conn(&db, "view_1", &refs)
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[tokio::test]
    async fn commit5_list_view_refs_rejects_empty_view_id() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let error = OperationService::list_view_refs_with_conn(&db, " ")
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            OperationServiceError::InvalidArgument(_)
        ));
    }

    #[test]
    fn commit5_validate_view_ref_accepts_valid_record() {
        let record = OperationViewRefRecord {
            view_id: "view_2".to_string(),
            ref_kind: "branch".to_string(),
            ref_name: "main".to_string(),
            ref_remote: None,
            target_oid: "oid-main".to_string(),
        };

        OperationService::validate_view_ref_record(&record).unwrap();
    }

    #[tokio::test]
    async fn commit6_view_workspace_validation_and_roundtrip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();

        let invalid_snapshot = OperationViewWorkspaceRecord {
            view_id: "view_1".to_string(),
            pointer_kind: "worktree".to_string(),
            pointer_value: " ".to_string(),
        };
        let error = OperationService::upsert_workspace_snapshot_with_conn(&db, &invalid_snapshot)
            .await
            .unwrap_err();
        assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

        let error = OperationService::find_workspace_snapshot_with_conn(&db, " ")
            .await
            .unwrap_err();
        assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            r#"
            CREATE TABLE IF NOT EXISTS operation_view_workspace (
                view_id TEXT NOT NULL,
                pointer_kind TEXT NOT NULL,
                pointer_value TEXT NOT NULL,
                PRIMARY KEY (view_id, pointer_kind)
            );
            "#,
        ))
        .await
        .unwrap();

        let index_snapshot = OperationViewWorkspaceRecord {
            view_id: "view_3".to_string(),
            pointer_kind: "index".to_string(),
            pointer_value: "oid-index-v1".to_string(),
        };
        let worktree_snapshot = OperationViewWorkspaceRecord {
            view_id: "view_3".to_string(),
            pointer_kind: "worktree".to_string(),
            pointer_value: "oid-worktree-v1".to_string(),
        };

        OperationService::upsert_workspace_snapshot_with_conn(&db, &index_snapshot)
            .await
            .unwrap();
        OperationService::upsert_workspace_snapshot_with_conn(&db, &worktree_snapshot)
            .await
            .unwrap();

        let updated_index = OperationViewWorkspaceRecord {
            pointer_value: "oid-index-v2".to_string(),
            ..index_snapshot.clone()
        };
        OperationService::upsert_workspace_snapshot_with_conn(&db, &updated_index)
            .await
            .unwrap();

        let snapshots = OperationService::find_workspace_snapshot_with_conn(&db, "view_3")
            .await
            .unwrap();
        assert_eq!(snapshots.len(), 2);
        assert_eq!(snapshots[0].pointer_kind, "index");
        assert_eq!(snapshots[0].pointer_value, "oid-index-v2");
        assert_eq!(snapshots[1].pointer_kind, "worktree");
        assert_eq!(snapshots[1].pointer_value, "oid-worktree-v1");
    }

    #[tokio::test]
    async fn commit7_persist_and_find_operation_graph_roundtrip() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let ddl = concat!(
            "CREATE TABLE IF NOT EXISTS operation(op_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,view_id TEXT NOT NULL,command_name TEXT NOT NULL,description TEXT NOT NULL,actor TEXT NOT NULL,args_digest TEXT,start_ts INTEGER NOT NULL,end_ts INTEGER,status TEXT NOT NULL,worktree_id TEXT NOT NULL DEFAULT '');",
            "CREATE TABLE IF NOT EXISTS operation_parent(op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id));",
            "CREATE TABLE IF NOT EXISTS operation_view(view_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,head_kind TEXT NOT NULL,head_target TEXT NOT NULL,created_at INTEGER NOT NULL);",
            "CREATE TABLE IF NOT EXISTS operation_view_ref(view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote));",
            "CREATE TABLE IF NOT EXISTS operation_view_workspace(view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind));"
        );
        db.execute(Statement::from_string(DbBackend::Sqlite, ddl))
            .await
            .unwrap();

        let graph = OperationGraphRecord {
            operation: OperationRecord {
                op_id: "op_7".to_string(),
                repo_id: "repo_7".to_string(),
                view_id: "view_7".to_string(),
                command_name: "merge".to_string(),
                description: "merge feature into main".to_string(),
                actor: "alice".to_string(),
                args_digest: Some("sha256:commit7".to_string()),
                start_ts: 200,
                end_ts: Some(205),
                status: OperationStatus::Succeeded,
                worktree_id: String::new(),
            },
            parents: vec![OperationParentRecord {
                op_id: "op_7".to_string(),
                parent_op_id: "op_6".to_string(),
            }],
            view: OperationViewRecord {
                view_id: "view_7".to_string(),
                repo_id: "repo_7".to_string(),
                head_kind: "branch".to_string(),
                head_target: "main".to_string(),
                created_at: 205,
            },
            refs: vec![OperationViewRefRecord {
                view_id: "view_7".to_string(),
                ref_kind: "branch".to_string(),
                ref_name: "main".to_string(),
                ref_remote: None,
                target_oid: "oid-main".to_string(),
            }],
            workspace: vec![OperationViewWorkspaceRecord {
                view_id: "view_7".to_string(),
                pointer_kind: "index".to_string(),
                pointer_value: "oid-index".to_string(),
            }],
        };

        let saved = OperationService::persist_operation_graph_with_conn(&db, &graph)
            .await
            .unwrap();
        let loaded = OperationService::load_restore_view_by_operation_with_conn(&db, "op_7")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(saved.operation.op_id, "op_7");
        assert_eq!(loaded.parents.len(), 1);
        assert_eq!(loaded.refs.len(), 1);
        assert_eq!(loaded.workspace.len(), 1);

        let mut bad_graph = graph.clone();
        bad_graph.view.view_id = "view_8".to_string();
        let error = OperationService::persist_operation_graph_with_conn(&db, &bad_graph)
            .await
            .unwrap_err();
        assert!(matches!(error, OperationServiceError::InvalidArgument(_)));
    }

    #[tokio::test]
    async fn commit8_paginated_operation_log_query() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let error = OperationService::list_operations_by_repo_paginated_with_conn(
            &db,
            " ",
            OperationQueryPage::default(),
        )
        .await
        .unwrap_err();
        assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE IF NOT EXISTS operation(op_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,view_id TEXT NOT NULL,command_name TEXT NOT NULL,description TEXT NOT NULL,actor TEXT NOT NULL,args_digest TEXT,start_ts INTEGER NOT NULL,end_ts INTEGER,status TEXT NOT NULL,worktree_id TEXT NOT NULL DEFAULT '');",
        ))
        .await
        .unwrap();

        for (op_id, end_ts) in [("op_a", 120), ("op_b", 220), ("op_c", 180)] {
            let record = OperationRecord {
                op_id: op_id.to_string(),
                repo_id: "repo_8".to_string(),
                view_id: format!("view_{op_id}"),
                command_name: "commit".to_string(),
                description: format!("desc_{op_id}"),
                actor: "alice".to_string(),
                args_digest: None,
                start_ts: end_ts - 10,
                end_ts: Some(end_ts),
                status: OperationStatus::Succeeded,
                worktree_id: String::new(),
            };
            OperationService::insert_operation_with_conn(&db, &record)
                .await
                .unwrap();
        }

        let page1 = OperationService::list_operations_by_repo_paginated_with_conn(
            &db,
            "repo_8",
            OperationQueryPage {
                page: 1,
                per_page: 2,
            },
        )
        .await
        .unwrap();
        assert_eq!(page1.total, 3);
        assert_eq!(page1.items.len(), 2);
        assert_eq!(page1.items[0].op_id, "op_b");
        assert_eq!(page1.items[1].op_id, "op_c");

        let page2 = OperationService::list_operations_by_repo_paginated_with_conn(
            &db,
            "repo_8",
            OperationQueryPage {
                page: 2,
                per_page: 2,
            },
        )
        .await
        .unwrap();
        assert_eq!(page2.total, 3);
        assert_eq!(page2.items.len(), 1);
        assert_eq!(page2.items[0].op_id, "op_a");
    }
}
*/
