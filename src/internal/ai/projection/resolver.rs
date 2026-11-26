//! Read path for `ai_index_task_run`, including `base_commit_ref`.

use git_internal::hash::HashKind;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};

use crate::internal::{
    ai::util::{RepoCommitRef, RepoCommitRefStore},
    model::ai_index_task_run,
};

/// Resolved task→run index row as seen by B3-10 consumers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedTaskRun {
    pub task_id: String,
    pub run_id: String,
    pub is_latest: bool,
    pub created_at: i64,
    pub base_commit_ref: Option<String>,
}

/// Load one `(task_id, run_id)` pair from the projection table.
pub async fn resolve_task_run<C: ConnectionTrait>(
    conn: &C,
    task_id: &str,
    run_id: &str,
) -> Result<Option<ResolvedTaskRun>, sea_orm::DbErr> {
    let row = ai_index_task_run::Entity::find()
        .filter(ai_index_task_run::Column::TaskId.eq(task_id))
        .filter(ai_index_task_run::Column::RunId.eq(run_id))
        .one(conn)
        .await?;
    Ok(row.map(|model| ResolvedTaskRun {
        task_id: model.task_id,
        run_id: model.run_id,
        is_latest: model.is_latest,
        created_at: model.created_at,
        base_commit_ref: model.base_commit_ref,
    }))
}

/// Load one `(task_id, run_id)` pair from `ai_index_task_run` via
/// [`crate::storage::GitStateStore`].
pub async fn resolve_task_run_with_store(
    store: &dyn crate::storage::GitStateStore,
    task_id: &str,
    run_id: &str,
) -> crate::storage::StorageResult<Option<ResolvedTaskRun>> {
    let rows = store.list_ai_index_task_runs(task_id).await?;
    Ok(rows
        .into_iter()
        .find(|row| row.run_id == run_id)
        .map(|row| ResolvedTaskRun {
            task_id: row.task_id,
            run_id: row.run_id,
            is_latest: row.is_latest,
            created_at: row.created_at,
            base_commit_ref: row.base_commit_ref,
        }))
}

/// List every projected run for `task_id` from `ai_index_task_run` via
/// [`crate::storage::GitStateStore`].
pub async fn list_task_runs_with_store(
    store: &dyn crate::storage::GitStateStore,
    task_id: &str,
) -> crate::storage::StorageResult<Vec<ResolvedTaskRun>> {
    let rows = store.list_ai_index_task_runs(task_id).await?;
    Ok(rows
        .into_iter()
        .map(|row| ResolvedTaskRun {
            task_id: row.task_id,
            run_id: row.run_id,
            is_latest: row.is_latest,
            created_at: row.created_at,
            base_commit_ref: row.base_commit_ref,
        })
        .collect())
}

/// Load the latest projected run (`is_latest == true`) for `task_id` via
/// [`crate::storage::GitStateStore`].
pub async fn resolve_latest_task_run_with_store(
    store: &dyn crate::storage::GitStateStore,
    task_id: &str,
) -> crate::storage::StorageResult<Option<ResolvedTaskRun>> {
    let rows = store.list_ai_index_task_runs(task_id).await?;
    Ok(rows
        .into_iter()
        .find(|row| row.is_latest)
        .map(|row| ResolvedTaskRun {
            task_id: row.task_id,
            run_id: row.run_id,
            is_latest: row.is_latest,
            created_at: row.created_at,
            base_commit_ref: row.base_commit_ref,
        }))
}

impl ResolvedTaskRun {
    /// Parse `base_commit_ref` under an explicit repository kind (B3-10).
    ///
    /// Tagged values are preferred; empty/NULL yields `Ok(None)`. Malformed or
    /// kind-mismatched values fail closed.
    pub fn typed_base_commit(&self, repo_kind: HashKind) -> Result<Option<RepoCommitRef>, String> {
        RepoCommitRefStore::read(self.base_commit_ref.as_deref(), None, repo_kind)
    }
}
