//! Crash-safe restoration of a v2 workspace snapshot.
//!
//! Restore is deliberately an explicit state transition.  It never edits an
//! existing operation; it writes a new Restore operation and advances the
//! scoped operation head with the same generation CAS used by normal
//! mutations.  The journal remains durable until the operation reaches its
//! terminal status, so `op doctor` can distinguish an interrupted restore
//! from a completed one.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    path::{Path, PathBuf},
    str::FromStr,
    time::{SystemTime, UNIX_EPOCH},
};

use clap::ValueEnum;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        tree::{Tree, TreeItemMode},
        types::ObjectType,
    },
};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use uuid::Uuid;

use super::{
    Completeness, FacetCapture, FacetName, FacetRestoreCtx, HeadState, JournalEntry, JournalPhase,
    OperationKind, OperationMetaV2, OperationStatusV2, OperationStoreV2, OperationV2,
    PinnedRequestScope, RepoViewV2, RestorePolicy, WorkspaceSnapshotV2, WorkspaceSnapshotter,
    WorkspaceStatePointer, middleware::ScopeLease, view::REPO_VIEW_SCHEMA_VERSION,
};
use crate::{
    internal::{
        head::Head,
        operation::{PointerError, facets::registry_for_scope},
        worktree_scope::WorktreeScope,
    },
    utils::client_storage::ClientStorage,
};

/// The state facets that a restore may touch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum RestoreWhat {
    /// Restore every captured facet and the worktree contents.
    #[default]
    All,
    /// Restore only files in the working copy.
    WorkingCopy,
    /// Restore only the byte-exact raw index.
    Index,
    /// Restore only in-progress sequencer state.
    Sequencer,
    /// Restore only sparse-view state.
    Sparse,
    /// Restore only the current worktree HEAD pointer.
    Head,
}

#[derive(Debug, Error)]
pub enum RestoreError {
    #[error("restore target view is missing workspace '{0}'")]
    WorkspaceMissing(String),
    #[error("restore target snapshot is not fully restorable")]
    IncompleteSnapshot,
    #[error("restore target is not valid for this worktree: {0}")]
    WrongWorkspace(String),
    #[error("restore object {oid} could not be loaded: {detail}")]
    Object { oid: ObjectHash, detail: String },
    #[error("restore I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("restore facet failed: {0}")]
    Facet(String),
    #[error("restore storage failed: {0}")]
    Storage(String),
    #[error("restore CAS failed: {0}")]
    Cas(String),
    #[error("restore target HEAD is not allowed without explicit confirmation")]
    HeadConfirmationRequired,
    #[error("restore cannot proceed with an incomplete target view")]
    IncompleteView,
}

/// Machine-readable result of a restore attempt.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReceipt {
    pub target_op_id: String,
    pub target_view_oid: ObjectHash,
    pub new_op_id: Option<String>,
    pub workspace_id: String,
    pub what: RestoreWhat,
    pub dry_run: bool,
    pub restored_facets: Vec<String>,
    pub changed_paths: usize,
}

/// v2 restore coordinator for one pinned worktree.
#[derive(Clone)]
pub struct RestoreEngine {
    scope: PinnedRequestScope,
    repo_id: String,
    store: OperationStoreV2,
}

struct CurrentRestoreState {
    view_oid: ObjectHash,
    snapshot_oid: ObjectHash,
    snapshot: WorkspaceSnapshotV2,
    pointer: WorkspaceStatePointer,
}

struct DryRunSnapshot {
    snapshot: WorkspaceSnapshotV2,
    storage: ClientStorage,
    _scratch: tempfile::TempDir,
}

#[allow(clippy::too_many_arguments)]
impl RestoreEngine {
    pub fn new(
        scope: PinnedRequestScope,
        repo_id: impl Into<String>,
        db: sea_orm::DatabaseConnection,
        storage: ClientStorage,
    ) -> Self {
        let repo_id = repo_id.into();
        Self {
            scope,
            store: OperationStoreV2::new_for_repo(repo_id.clone(), db, storage),
            repo_id,
        }
    }

    pub fn store(&self) -> &OperationStoreV2 {
        &self.store
    }

    pub fn repo_id(&self) -> &str {
        &self.repo_id
    }

    pub fn scope_key(&self) -> String {
        self.scope.scope.storage_key().to_string()
    }

    /// Restore one view.  A target with multiple workspaces is rejected unless
    /// the caller explicitly opts into repository-wide semantics; this engine
    /// still restores only the pinned worktree.
    pub async fn restore(
        &self,
        target_op_id: impl Into<String>,
        target_view_oid: ObjectHash,
        what: RestoreWhat,
        dry_run: bool,
        confirm_repo_wide: bool,
    ) -> Result<RestoreReceipt, RestoreError> {
        self.restore_with_kind(
            target_op_id,
            target_view_oid,
            OperationKind::Restore,
            what,
            dry_run,
            confirm_repo_wide,
        )
        .await
    }

    /// Apply a state transition while preserving the target operation's
    /// append-only provenance. Undo/redo/revert use this seam to publish their
    /// own operation kind instead of masquerading as a plain restore.
    pub async fn restore_with_kind(
        &self,
        target_op_id: impl Into<String>,
        target_view_oid: ObjectHash,
        kind: OperationKind,
        what: RestoreWhat,
        dry_run: bool,
        confirm_repo_wide: bool,
    ) -> Result<RestoreReceipt, RestoreError> {
        self.restore_with_relationship(
            target_op_id,
            target_view_oid,
            kind,
            what,
            dry_run,
            confirm_repo_wide,
            None,
        )
        .await
    }

    pub async fn restore_with_relationship(
        &self,
        target_op_id: impl Into<String>,
        target_view_oid: ObjectHash,
        kind: OperationKind,
        what: RestoreWhat,
        dry_run: bool,
        confirm_repo_wide: bool,
        reverts_op_id: Option<String>,
    ) -> Result<RestoreReceipt, RestoreError> {
        self.restore_with_relationship_and_expected_head(
            target_op_id,
            target_view_oid,
            kind,
            what,
            dry_run,
            confirm_repo_wide,
            reverts_op_id,
            None,
        )
        .await
    }

    pub async fn restore_with_expected_head(
        &self,
        target_op_id: impl Into<String>,
        target_view_oid: ObjectHash,
        kind: OperationKind,
        what: RestoreWhat,
        dry_run: bool,
        confirm_repo_wide: bool,
        expected_head: String,
    ) -> Result<RestoreReceipt, RestoreError> {
        self.restore_with_relationship_and_expected_head(
            target_op_id,
            target_view_oid,
            kind,
            what,
            dry_run,
            confirm_repo_wide,
            None,
            Some(expected_head),
        )
        .await
    }

    pub async fn restore_with_relationship_and_expected_head(
        &self,
        target_op_id: impl Into<String>,
        target_view_oid: ObjectHash,
        kind: OperationKind,
        what: RestoreWhat,
        dry_run: bool,
        confirm_repo_wide: bool,
        reverts_op_id: Option<String>,
        expected_head: Option<String>,
    ) -> Result<RestoreReceipt, RestoreError> {
        self.restore_with_relationship_and_expected_head_inner(
            target_op_id,
            target_view_oid,
            kind,
            what,
            dry_run,
            confirm_repo_wide,
            reverts_op_id,
            expected_head,
        )
        .await
    }

    async fn restore_with_relationship_and_expected_head_inner(
        &self,
        target_op_id: impl Into<String>,
        target_view_oid: ObjectHash,
        kind: OperationKind,
        what: RestoreWhat,
        dry_run: bool,
        confirm_repo_wide: bool,
        reverts_op_id: Option<String>,
        expected_head: Option<String>,
    ) -> Result<RestoreReceipt, RestoreError> {
        let target_op_id = target_op_id.into();
        let target_operation = self
            .store
            .load_operation(&target_op_id)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?
            .ok_or_else(|| {
                RestoreError::Storage(format!("operation '{target_op_id}' not found"))
            })?;
        if target_operation.status != OperationStatusV2::Success {
            return Err(RestoreError::Storage(format!(
                "operation '{target_op_id}' is not a completed success"
            )));
        }
        if kind != OperationKind::Revert
            && target_operation.post_view_oid != target_view_oid
            && target_operation.pre_view_oid != target_view_oid
        {
            return Err(RestoreError::WrongWorkspace(format!(
                "operation '{target_op_id}' does not publish target view {target_view_oid}"
            )));
        }
        let view = self
            .store
            .load_view(&target_view_oid)
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        view.validate_recursive_closure(|oid| self.store.load_object(oid).ok())
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if view.repo_id != self.repo_id {
            return Err(RestoreError::WrongWorkspace(format!(
                "target belongs to repository '{}', expected '{}'",
                view.repo_id, self.repo_id
            )));
        }
        let workspace_id = workspace_id(&self.scope);
        if view.workspaces.len() != 1 && !confirm_repo_wide {
            return Err(RestoreError::HeadConfirmationRequired);
        }
        let snapshot_oid = view
            .workspaces
            .get(&workspace_id)
            .copied()
            .ok_or_else(|| RestoreError::WorkspaceMissing(workspace_id.clone()))?;
        let snapshot = self
            .store
            .load_snapshot(&snapshot_oid)
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if snapshot.workspace_id != workspace_id {
            return Err(RestoreError::WrongWorkspace(snapshot.workspace_id));
        }
        if snapshot.completeness != Completeness::Full {
            return Err(RestoreError::IncompleteSnapshot);
        }
        let selected = selected_facets(what);
        let changed_paths = if dry_run {
            let current = self.capture_current_snapshot(0).await?;
            count_changed_paths(
                &current.storage,
                &self.store,
                &current.snapshot,
                &snapshot,
                what,
            )?
        } else {
            0
        };
        let mut receipt = RestoreReceipt {
            target_op_id,
            target_view_oid,
            new_op_id: None,
            workspace_id: workspace_id.clone(),
            what,
            dry_run,
            restored_facets: selected.iter().map(|name| name.to_string()).collect(),
            changed_paths,
        };
        if dry_run {
            return Ok(receipt);
        }

        // Hold the same process-wide scope pin and file lease used by normal
        // operation transactions for the complete read/mutate/publish span.
        // The final generation CAS remains a guard against non-cooperating
        // writers, while the lease prevents cooperative writers from editing
        // the worktree between the freshness check and the filesystem swap.
        let _scope_guard = WorktreeScope::pin_request_scope(self.scope.workdir.clone());
        let _repo_lease = if confirm_repo_wide && what == RestoreWhat::All {
            Some(
                ScopeLease::acquire_repository(&self.scope, &self.repo_id)
                    .await
                    .map_err(|error| RestoreError::Storage(error.to_string()))?,
            )
        } else {
            None
        };
        let _lease = ScopeLease::acquire(&self.scope, &self.repo_id)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let scope_key = self.scope.scope.storage_key().to_string();
        self.recover_interrupted_operations(&scope_key).await?;
        let heads = self
            .store
            .read_heads_view(&self.repo_id, &scope_key)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if let Some(expected_head) = expected_head
            && heads.head_ids() != vec![expected_head]
        {
            return Err(RestoreError::Cas(
                "current operation head changed after transition preflight".to_string(),
            ));
        }
        let generation = self
            .store
            .read_head_generation(&self.repo_id, &scope_key)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let current = self.capture_current_state(generation).await?;
        receipt.changed_paths =
            count_changed_paths(&self.store, &self.store, &current.snapshot, &snapshot, what)?;
        let restore_refs = confirm_repo_wide && what == RestoreWhat::All;
        let op_id = Uuid::now_v7().to_string();
        let owner = format!("pid-{}", std::process::id());
        let command_name = match kind {
            OperationKind::Undo => "op undo",
            OperationKind::Redo => "op redo",
            OperationKind::Revert => "op revert",
            OperationKind::Restore => "op restore",
            _ => "op restore",
        };
        let operation = OperationV2 {
            op_id: op_id.clone(),
            parent_op_ids: heads.head_ids(),
            pre_view_oid: current.view_oid,
            post_view_oid: target_view_oid,
            kind,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2 {
                command_name: Some(command_name.to_string()),
                description: Some(format!("{command_name} to {}", receipt.target_op_id)),
                actor: Some("libra-user".to_string()),
                args_digest: Some(receipt.target_op_id.clone()),
                ..Default::default()
            },
            restores_op_id: Some(receipt.target_op_id.clone()),
            reverts_op_id,
            predecessor_map_oid: None,
        };
        self.store
            .write_operation(&operation)
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if let Err(error) = self
            .append_journal(
                &op_id,
                JournalPhase::Reserved,
                current.view_oid,
                target_view_oid,
                owner.clone(),
                restore_refs,
            )
            .await
        {
            return Err(self.mark_failed(&op_id, error).await);
        }
        if let Err(error) = self
            .append_journal(
                &op_id,
                JournalPhase::PreView,
                current.view_oid,
                target_view_oid,
                owner.clone(),
                restore_refs,
            )
            .await
        {
            return Err(self.mark_failed(&op_id, error).await);
        }
        if let Err(error) = self
            .append_journal(
                &op_id,
                JournalPhase::Mutation,
                current.view_oid,
                target_view_oid,
                owner.clone(),
                restore_refs,
            )
            .await
        {
            return Err(self.mark_failed(&op_id, error).await);
        }

        let apply_result = async {
            self.apply_snapshot(&snapshot, what, confirm_repo_wide)
                .await?;
            if restore_refs {
                self.restore_references(&view).await?;
            }
            Ok::<(), RestoreError>(())
        }
        .await;
        if let Err(error) = apply_result {
            return Err(self
                .fail_and_rollback(
                    &op_id,
                    current.view_oid,
                    &current.snapshot,
                    restore_refs,
                    error,
                )
                .await);
        }
        let post = match self.capture_current_state(generation).await {
            Ok(post) => post,
            Err(error) => {
                return Err(self
                    .fail_and_rollback(
                        &op_id,
                        current.view_oid,
                        &current.snapshot,
                        restore_refs,
                        error,
                    )
                    .await);
            }
        };
        let post_manifest = match self.store.load_view(&post.view_oid) {
            Ok(manifest) => manifest,
            Err(error) => {
                return Err(self
                    .fail_and_rollback(
                        &op_id,
                        current.view_oid,
                        &current.snapshot,
                        restore_refs,
                        RestoreError::Storage(error.to_string()),
                    )
                    .await);
            }
        };
        let mut published_view = view.clone();
        published_view
            .workspaces
            .insert(workspace_id.clone(), post.snapshot_oid);
        if !(confirm_repo_wide && what == RestoreWhat::All) {
            published_view.refs_facet_oid = post_manifest.refs_facet_oid;
        }
        let post_view_oid = match self.store.write_view_manifest(&published_view) {
            Ok(oid) => oid,
            Err(error) => {
                return Err(self
                    .fail_and_rollback(
                        &op_id,
                        current.view_oid,
                        &current.snapshot,
                        restore_refs,
                        RestoreError::Storage(error.to_string()),
                    )
                    .await);
            }
        };
        let post_snapshot_oid = post.snapshot_oid;
        if let Err(error) = self
            .store
            .update_operation_post_view(&op_id, &post_view_oid)
            .await
        {
            return Err(self
                .fail_and_rollback(
                    &op_id,
                    current.view_oid,
                    &current.snapshot,
                    restore_refs,
                    RestoreError::Storage(error.to_string()),
                )
                .await);
        }
        if let Err(error) = self
            .append_journal(
                &op_id,
                JournalPhase::PostView,
                current.view_oid,
                post_view_oid,
                owner.clone(),
                restore_refs,
            )
            .await
        {
            return Err(self
                .fail_and_rollback(
                    &op_id,
                    current.view_oid,
                    &current.snapshot,
                    restore_refs,
                    error,
                )
                .await);
        }
        let new_generation = match self
            .store
            .cas_update_op_heads_at_generation(
                &self.repo_id,
                &scope_key,
                generation,
                &operation.parent_op_ids,
                std::slice::from_ref(&op_id),
            )
            .await
        {
            Ok(generation) => generation,
            Err(error) => {
                return Err(self
                    .fail_without_rollback(
                        &op_id,
                        RestoreError::Cas(format!(
                            "{error}; concurrent head changed, physical state was not force-rolled back"
                        )),
                    )
                    .await);
            }
        };
        if let Err(error) = self
            .append_journal(
                &op_id,
                JournalPhase::Publish,
                current.view_oid,
                post_view_oid,
                owner,
                restore_refs,
            )
            .await
        {
            return Err(self
                .rollback_published(
                    &op_id,
                    current.view_oid,
                    &current.snapshot,
                    &current.pointer,
                    &scope_key,
                    new_generation,
                    &operation.parent_op_ids,
                    restore_refs,
                    error,
                )
                .await);
        }
        let mut pointer =
            WorkspaceStatePointer::new(op_id.clone(), post_snapshot_oid, new_generation);
        pointer.last_content_oid = Some(post_snapshot_oid);
        if let Err(error) = pointer.save(&self.scope).await {
            return Err(self
                .rollback_published(
                    &op_id,
                    current.view_oid,
                    &current.snapshot,
                    &current.pointer,
                    &scope_key,
                    new_generation,
                    &operation.parent_op_ids,
                    restore_refs,
                    RestoreError::Storage(error.to_string()),
                )
                .await);
        }
        if let Err(error) = self
            .store
            .update_operation_status(&op_id, OperationStatusV2::Success)
            .await
        {
            return Err(self
                .rollback_published(
                    &op_id,
                    current.view_oid,
                    &current.snapshot,
                    &current.pointer,
                    &scope_key,
                    new_generation,
                    &operation.parent_op_ids,
                    restore_refs,
                    RestoreError::Storage(error.to_string()),
                )
                .await);
        }
        receipt.new_op_id = Some(op_id);
        Ok(receipt)
    }

    async fn capture_current_state(
        &self,
        generation: u64,
    ) -> Result<CurrentRestoreState, RestoreError> {
        let pointer = match WorkspaceStatePointer::load(&self.scope).await {
            Ok(pointer) => pointer,
            Err(PointerError::Missing(_)) => WorkspaceStatePointer::new(
                "bootstrap",
                ObjectHash::from_type_and_data(ObjectType::Blob, b"libra-restore-bootstrap"),
                generation,
            ),
            Err(error) => return Err(RestoreError::Storage(error.to_string())),
        };
        let outcome = WorkspaceSnapshotter::new(self.scope.clone(), pointer.clone())
            .capture()
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if outcome.snapshot.completeness != Completeness::Full {
            return Err(RestoreError::IncompleteView);
        }
        let refs = self.capture_reference_state().await?;
        let refs_bytes = serde_json::to_vec(&serde_json::json!({
            "schema_version": 1,
            "references": refs,
        }))
        .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let refs_oid = ObjectHash::from_type_and_data(ObjectType::Blob, &refs_bytes);
        self.store
            .write_blob(&refs_oid, &refs_bytes, ObjectType::Blob)
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let view = RepoViewV2 {
            schema_version: REPO_VIEW_SCHEMA_VERSION,
            repo_id: self.repo_id.clone(),
            refs_facet_oid: refs_oid,
            workspaces: [(outcome.snapshot.workspace_id.clone(), outcome.snapshot_oid)]
                .into_iter()
                .collect(),
            change_roots: Vec::new(),
            extension_facets: Default::default(),
        };
        let view_oid = self
            .store
            .write_view_manifest(&view)
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        Ok(CurrentRestoreState {
            view_oid,
            snapshot_oid: outcome.snapshot_oid,
            snapshot: outcome.snapshot,
            pointer,
        })
    }

    async fn capture_current_snapshot(
        &self,
        generation: u64,
    ) -> Result<DryRunSnapshot, RestoreError> {
        let pointer = match WorkspaceStatePointer::load(&self.scope).await {
            Ok(pointer) => pointer,
            Err(PointerError::Missing(_)) => WorkspaceStatePointer::new(
                "bootstrap",
                ObjectHash::from_type_and_data(ObjectType::Blob, b"libra-restore-bootstrap"),
                generation,
            ),
            Err(error) => return Err(RestoreError::Storage(error.to_string())),
        };
        let scratch = tempfile::tempdir().map_err(RestoreError::Io)?;
        let scratch_storage = ClientStorage::init_local(scratch.path().join("objects"));
        let outcome = WorkspaceSnapshotter::new(self.scope.clone(), pointer)
            .with_storage(scratch_storage.clone())
            .capture()
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if outcome.snapshot.completeness != Completeness::Full {
            return Err(RestoreError::IncompleteView);
        }
        Ok(DryRunSnapshot {
            snapshot: outcome.snapshot,
            storage: scratch_storage,
            _scratch: scratch,
        })
    }

    async fn capture_reference_state(&self) -> Result<serde_json::Value, RestoreError> {
        let rows = self
            .store
            .db()
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT id, name, kind, `commit`, remote, worktree_id FROM reference ORDER BY id",
            ))
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let mut references = Vec::with_capacity(rows.len());
        for row in rows {
            references.push(serde_json::json!({
                "id": row.try_get_by_index::<i64>(0).map_err(|error| RestoreError::Storage(error.to_string()))?,
                "name": row.try_get_by_index::<Option<String>>(1).map_err(|error| RestoreError::Storage(error.to_string()))?,
                "kind": row.try_get_by_index::<String>(2).map_err(|error| RestoreError::Storage(error.to_string()))?,
                "commit": row.try_get_by_index::<Option<String>>(3).map_err(|error| RestoreError::Storage(error.to_string()))?,
                "remote": row.try_get_by_index::<Option<String>>(4).map_err(|error| RestoreError::Storage(error.to_string()))?,
                "worktree_id": row.try_get_by_index::<Option<String>>(5).map_err(|error| RestoreError::Storage(error.to_string()))?,
            }));
        }
        Ok(serde_json::Value::Array(references))
    }

    async fn fail_and_rollback(
        &self,
        op_id: &str,
        pre_view_oid: ObjectHash,
        snapshot: &WorkspaceSnapshotV2,
        restore_refs: bool,
        error: RestoreError,
    ) -> RestoreError {
        let mut rollback = self
            .apply_snapshot(snapshot, RestoreWhat::All, true)
            .await
            .err();
        if rollback.is_none() && restore_refs {
            rollback = match self.store.load_view(&pre_view_oid) {
                Ok(view) => self.restore_references(&view).await.err(),
                Err(error) => Some(RestoreError::Storage(error.to_string())),
            };
        }
        let status = self
            .store
            .update_operation_status(op_id, OperationStatusV2::Failed)
            .await;
        match (rollback, status) {
            (None, Ok(())) => error,
            (rollback, status) => RestoreError::Storage(format!(
                "{error}; rollback/status failed: rollback={rollback:?}, status={status:?}"
            )),
        }
    }

    async fn mark_failed(&self, op_id: &str, error: RestoreError) -> RestoreError {
        match self
            .store
            .update_operation_status(op_id, OperationStatusV2::Failed)
            .await
        {
            Ok(()) => error,
            Err(status) => RestoreError::Storage(format!(
                "{error}; failed to mark operation failed: {status}"
            )),
        }
    }

    async fn fail_without_rollback(&self, op_id: &str, error: RestoreError) -> RestoreError {
        self.mark_failed(op_id, error).await
    }

    async fn rollback_published(
        &self,
        op_id: &str,
        pre_view_oid: ObjectHash,
        snapshot: &WorkspaceSnapshotV2,
        pointer: &WorkspaceStatePointer,
        scope_key: &str,
        generation: u64,
        parents: &[String],
        restore_refs: bool,
        error: RestoreError,
    ) -> RestoreError {
        let mut filesystem = self
            .apply_snapshot(snapshot, RestoreWhat::All, true)
            .await
            .err();
        if filesystem.is_none() && restore_refs {
            filesystem = match self.store.load_view(&pre_view_oid) {
                Ok(view) => self.restore_references(&view).await.err(),
                Err(error) => Some(RestoreError::Storage(error.to_string())),
            };
        }
        let pointer_restore = pointer.save(&self.scope).await.err();
        let heads = self
            .store
            .cas_update_op_heads_at_generation(
                &self.repo_id,
                scope_key,
                generation,
                &[op_id.to_string()],
                parents,
            )
            .await
            .err();
        let status = self
            .store
            .update_operation_status(op_id, OperationStatusV2::Failed)
            .await;
        match (filesystem, heads, pointer_restore, status) {
            (None, None, None, Ok(())) => error,
            (filesystem, heads, pointer_restore, status) => RestoreError::Storage(format!(
                "{error}; published rollback failed: filesystem={filesystem:?}, heads={heads:?}, pointer={pointer_restore:?}, status={status:?}"
            )),
        }
    }

    async fn restore_references(&self, view: &RepoViewV2) -> Result<(), RestoreError> {
        let bytes = self
            .store
            .load_object(&view.refs_facet_oid)
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let value: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        if value
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
        {
            return Err(RestoreError::Storage(
                "refs facet has unsupported schema_version".to_string(),
            ));
        }
        let references = value
            .get("references")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                RestoreError::Storage("refs facet has no references array".to_string())
            })?;
        let mut ids = BTreeSet::new();
        for reference in references {
            let id = reference
                .get("id")
                .and_then(serde_json::Value::as_i64)
                .filter(|id| *id >= 0)
                .ok_or_else(|| {
                    RestoreError::Storage("refs facet entry has invalid id".to_string())
                })?;
            if !ids.insert(id) {
                return Err(RestoreError::Storage(format!(
                    "refs facet contains duplicate id {id}"
                )));
            }
            let kind = reference
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RestoreError::Storage("refs facet entry has no kind".to_string()))?;
            if !matches!(kind, "Head" | "Branch" | "Tag") {
                return Err(RestoreError::Storage(format!(
                    "refs facet contains unsupported kind {kind:?}"
                )));
            }
            let name = reference.get("name").and_then(serde_json::Value::as_str);
            if name.is_some_and(str::is_empty) || (kind != "Head" && name.is_none()) {
                return Err(RestoreError::Storage(
                    "refs facet entry has an empty name".to_string(),
                ));
            }
            let remote = reference.get("remote").and_then(serde_json::Value::as_str);
            if remote.is_some_and(str::is_empty) {
                return Err(RestoreError::Storage(
                    "refs facet entry has an empty remote".to_string(),
                ));
            }
            let worktree_id = reference
                .get("worktree_id")
                .and_then(serde_json::Value::as_str);
            if worktree_id.is_some_and(str::is_empty) {
                return Err(RestoreError::Storage(
                    "refs facet entry has an empty worktree_id".to_string(),
                ));
            }
            let commit = reference.get("commit").and_then(serde_json::Value::as_str);
            match kind {
                "Head" if name.is_some() && (commit.is_some() || remote.is_some()) => {
                    return Err(RestoreError::Storage(
                        "symbolic Head ref must not have commit or remote".to_string(),
                    ));
                }
                "Head" if name.is_none() && commit.is_none() => {
                    return Err(RestoreError::Storage(
                        "detached Head ref must have a commit".to_string(),
                    ));
                }
                "Head" => {}
                "Tag" if commit.is_none() || remote.is_some() || worktree_id.is_some() => {
                    return Err(RestoreError::Storage(
                        "Tag ref has invalid commit, remote, or worktree scope".to_string(),
                    ));
                }
                "Tag" => {}
                "Branch" if commit.is_none() || worktree_id.is_some() => {
                    return Err(RestoreError::Storage(
                        "Branch ref has invalid commit or worktree scope".to_string(),
                    ));
                }
                "Branch" => {}
                _ => unreachable!(),
            }
            if let Some(commit) = commit {
                let oid = ObjectHash::from_str(commit).map_err(|error| {
                    RestoreError::Storage(format!(
                        "refs facet contains invalid commit oid: {error}"
                    ))
                })?;
                self.store
                    .load_object(&oid)
                    .map_err(|error| RestoreError::Object {
                        oid,
                        detail: error.to_string(),
                    })?;
                if !self.store.is_object_type(&oid, ObjectType::Commit) {
                    return Err(RestoreError::Object {
                        oid,
                        detail: "refs facet commit oid does not reference a commit object"
                            .to_string(),
                    });
                }
            }
        }
        let txn = self
            .store
            .db()
            .begin()
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        txn.execute_raw(Statement::from_string(
            DbBackend::Sqlite,
            "DELETE FROM reference",
        ))
        .await
        .map_err(|error| RestoreError::Storage(error.to_string()))?;
        for reference in references {
            let id = reference
                .get("id")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| RestoreError::Storage("refs facet entry has no id".to_string()))?;
            let name = reference
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let kind = reference
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| RestoreError::Storage("refs facet entry has no kind".to_string()))?;
            let commit = reference
                .get("commit")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let remote = reference
                .get("remote")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            let worktree_id = reference
                .get("worktree_id")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string);
            txn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO reference (id, name, kind, `commit`, remote, worktree_id) \
                 VALUES (?, ?, ?, ?, ?, ?)",
                [
                    id.into(),
                    name.into(),
                    kind.to_string().into(),
                    commit.into(),
                    remote.into(),
                    worktree_id.into(),
                ],
            ))
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        }
        txn.commit()
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        Ok(())
    }

    pub async fn recover_interrupted_operations(
        &self,
        scope_key: &str,
    ) -> Result<(), RestoreError> {
        let operations = self
            .store
            .list_operations()
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        let journals = self
            .store
            .read_all_journal()
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))?;
        for operation in operations.into_iter().filter(|operation| {
            operation.status == OperationStatusV2::Running
                && matches!(
                    operation.kind,
                    OperationKind::Restore
                        | OperationKind::Undo
                        | OperationKind::Redo
                        | OperationKind::Revert
                )
        }) {
            let Some(journal) = journals
                .iter()
                .filter(|journal| journal.op_id == operation.op_id)
                .max_by_key(|journal| journal.updated_at)
            else {
                return Err(self
                    .mark_failed(
                        &operation.op_id,
                        RestoreError::Storage(
                            "interrupted restore has no recovery journal".to_string(),
                        ),
                    )
                    .await);
            };
            let payload = journal
                .recovery_payload
                .as_deref()
                .and_then(|payload| serde_json::from_str::<serde_json::Value>(payload).ok());
            let workspace_id = workspace_id(&self.scope);
            if payload
                .as_ref()
                .and_then(|payload| payload.get("workspace_id"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|owner| owner != workspace_id)
            {
                continue;
            }
            let pre_view = self
                .store
                .load_view(&operation.pre_view_oid)
                .map_err(|error| RestoreError::Storage(error.to_string()))?;
            let Some(snapshot_oid) = pre_view.workspaces.get(&workspace_id).copied() else {
                continue;
            };
            let snapshot = self
                .store
                .load_snapshot(&snapshot_oid)
                .map_err(|error| RestoreError::Storage(error.to_string()))?;
            let restore_refs = payload
                .and_then(|payload| {
                    payload
                        .get("restore_refs")
                        .and_then(serde_json::Value::as_bool)
                })
                .unwrap_or(false);
            let heads = self
                .store
                .read_heads_view(&self.repo_id, scope_key)
                .await
                .map_err(|error| RestoreError::Storage(error.to_string()))?;
            let published = journal.phase == JournalPhase::Publish
                || heads.head_ids() == vec![operation.op_id.clone()];
            if published {
                if heads.head_ids() != vec![operation.op_id.clone()] {
                    return Err(self
                        .mark_failed(
                            &operation.op_id,
                            RestoreError::Cas(
                                "interrupted published restore is no longer current".to_string(),
                            ),
                        )
                        .await);
                }
                let generation = self
                    .store
                    .read_head_generation(&self.repo_id, scope_key)
                    .await
                    .map_err(|error| RestoreError::Storage(error.to_string()))?;
                let mut pointer = WorkspaceStatePointer::new(
                    operation
                        .parent_op_ids
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "bootstrap".to_string()),
                    snapshot_oid,
                    snapshot.worktree_generation,
                );
                pointer.last_content_oid = Some(snapshot_oid);
                return Err(self
                    .rollback_published(
                        &operation.op_id,
                        operation.pre_view_oid,
                        &snapshot,
                        &pointer,
                        scope_key,
                        generation,
                        &operation.parent_op_ids,
                        restore_refs,
                        RestoreError::Storage(
                            "recovered interrupted published restore".to_string(),
                        ),
                    )
                    .await);
            } else {
                return Err(self
                    .fail_and_rollback(
                        &operation.op_id,
                        operation.pre_view_oid,
                        &snapshot,
                        restore_refs,
                        RestoreError::Storage("recovered interrupted restore".to_string()),
                    )
                    .await);
            }
        }
        Ok(())
    }

    async fn append_journal(
        &self,
        op_id: &str,
        phase: JournalPhase,
        pre_view_oid: ObjectHash,
        target_view_oid: ObjectHash,
        owner: String,
        restore_refs: bool,
    ) -> Result<(), RestoreError> {
        self.store
            .append_journal(&JournalEntry {
                journal_id: op_id.to_string(),
                op_id: op_id.to_string(),
                phase,
                pre_view_oid: Some(pre_view_oid),
                target_view_oid: Some(target_view_oid),
                owner,
                updated_at: now_millis(),
                recovery_payload: Some(
                    serde_json::json!({
                        "kind": "restore",
                        "restore_refs": restore_refs,
                        "workspace_id": workspace_id(&self.scope),
                    })
                    .to_string(),
                ),
            })
            .await
            .map_err(|error| RestoreError::Storage(error.to_string()))
    }

    async fn apply_snapshot(
        &self,
        snapshot: &WorkspaceSnapshotV2,
        what: RestoreWhat,
        confirm_repo_wide: bool,
    ) -> Result<(), RestoreError> {
        let _scope_guard = WorktreeScope::pin_request_scope(self.scope.workdir.clone());
        let storage = ClientStorage::init_local(self.scope.storage.join("objects"));
        let names = selected_facets(what);
        if names.contains(&FacetName::from("working_copy")) {
            restore_working_copy(
                &storage,
                &snapshot.working_copy_tree_oid,
                &self.scope.worktree_root,
            )?;
        }
        let registry = registry_for_scope(self.scope.clone(), storage)
            .map_err(|error| RestoreError::Facet(error.to_string()))?;
        let mut ctx = FacetRestoreCtx {
            repo_id: Some(self.repo_id.clone()),
            workspace_id: Some(snapshot.workspace_id.clone()),
        };
        for name in names
            .iter()
            .filter(|name| name.as_str() != "working_copy" && name.as_str() != "head")
        {
            let Some(facet) = registry.get(name) else {
                return Err(RestoreError::Facet(format!(
                    "facet '{name}' is not registered"
                )));
            };
            if snapshot
                .facet_restore_policies
                .get(name)
                .is_some_and(|policy| *policy == RestorePolicy::NeverRestore)
            {
                continue;
            }
            let capture = capture_for_snapshot(name, snapshot);
            facet
                .restore(&capture, &mut ctx)
                .map_err(|error| RestoreError::Facet(error.to_string()))?;
        }
        if names.contains(&FacetName::from("head")) {
            if !confirm_repo_wide && snapshot.workspace_id != workspace_id(&self.scope) {
                return Err(RestoreError::HeadConfirmationRequired);
            }
            let head = match &snapshot.head {
                HeadState::Symbolic { reference } => Head::Branch(reference.clone()),
                HeadState::Detached { oid } => Head::Detached(*oid),
            };
            Head::update_result_with_conn(self.store.db(), head, None)
                .await
                .map_err(|error| RestoreError::Storage(error.to_string()))?;
        }
        Ok(())
    }
}

fn selected_facets(what: RestoreWhat) -> Vec<FacetName> {
    match what {
        RestoreWhat::All => vec![
            FacetName::from("working_copy"),
            FacetName::from("index"),
            FacetName::from("sequencer"),
            FacetName::from("sparse"),
            FacetName::from("head"),
        ],
        RestoreWhat::WorkingCopy => vec![FacetName::from("working_copy")],
        RestoreWhat::Index => vec![FacetName::from("index")],
        RestoreWhat::Sequencer => vec![FacetName::from("sequencer")],
        RestoreWhat::Sparse => vec![FacetName::from("sparse")],
        RestoreWhat::Head => vec![FacetName::from("head")],
    }
}

fn capture_for_snapshot(name: &FacetName, snapshot: &WorkspaceSnapshotV2) -> FacetCapture {
    let payload_oid = match name.as_str() {
        "index" => Some(snapshot.raw_index_blob_oid),
        "sequencer" => snapshot.sequencer_facet_oid,
        "sparse" => snapshot.sparse_facet_oid,
        _ => None,
    };
    FacetCapture {
        facet: name.clone(),
        schema_version: 1,
        payload_oid,
        meta: serde_json::json!({"present": payload_oid.is_some()}),
    }
}

trait RestoreObjectSource {
    fn load_restore_object(&self, oid: &ObjectHash) -> Result<Vec<u8>, RestoreError>;
}

impl RestoreObjectSource for OperationStoreV2 {
    fn load_restore_object(&self, oid: &ObjectHash) -> Result<Vec<u8>, RestoreError> {
        self.load_object(oid).map_err(|error| RestoreError::Object {
            oid: *oid,
            detail: error.to_string(),
        })
    }
}

impl RestoreObjectSource for ClientStorage {
    fn load_restore_object(&self, oid: &ObjectHash) -> Result<Vec<u8>, RestoreError> {
        self.get(oid).map_err(|error| RestoreError::Object {
            oid: *oid,
            detail: error.to_string(),
        })
    }
}

fn count_changed_paths<C: RestoreObjectSource, T: RestoreObjectSource>(
    current_store: &C,
    target_store: &T,
    current: &WorkspaceSnapshotV2,
    target: &WorkspaceSnapshotV2,
    what: RestoreWhat,
) -> Result<usize, RestoreError> {
    let mut changed = BTreeSet::new();
    if matches!(what, RestoreWhat::All | RestoreWhat::WorkingCopy) {
        let current_tree = collect_tree_entries(current_store, &current.working_copy_tree_oid, "")?;
        let target_tree = collect_tree_entries(target_store, &target.working_copy_tree_oid, "")?;
        for path in current_tree.keys().chain(target_tree.keys()) {
            if current_tree.get(path) != target_tree.get(path) {
                changed.insert(path.clone());
            }
        }
        let current_manifest =
            collect_manifest_paths(current_store, &current.untracked_manifest_oid)?;
        let target_manifest = collect_manifest_paths(target_store, &target.untracked_manifest_oid)?;
        for path in current_manifest.symmetric_difference(&target_manifest) {
            changed.insert(path.clone());
        }
    }
    let mut facet_changes = 0;
    if matches!(what, RestoreWhat::All | RestoreWhat::Index)
        && current.index_tree_oid != target.index_tree_oid
    {
        facet_changes += 1;
    }
    if matches!(what, RestoreWhat::All | RestoreWhat::Sequencer)
        && current.sequencer_facet_oid != target.sequencer_facet_oid
    {
        facet_changes += 1;
    }
    if matches!(what, RestoreWhat::All | RestoreWhat::Sparse)
        && current.sparse_facet_oid != target.sparse_facet_oid
    {
        facet_changes += 1;
    }
    if matches!(what, RestoreWhat::All | RestoreWhat::Head) && current.head != target.head {
        facet_changes += 1;
    }
    Ok(changed.len() + facet_changes)
}

fn collect_tree_entries<S: RestoreObjectSource>(
    store: &S,
    tree_oid: &ObjectHash,
    prefix: &str,
) -> Result<BTreeMap<String, (ObjectHash, TreeItemMode)>, RestoreError> {
    let mut entries = BTreeMap::new();
    let bytes = store.load_restore_object(tree_oid)?;
    let tree = Tree::from_bytes(&bytes, *tree_oid)
        .map_err(|error| RestoreError::Storage(error.to_string()))?;
    for item in tree.tree_items {
        let path = if prefix.is_empty() {
            item.name.clone()
        } else {
            format!("{prefix}/{}", item.name)
        };
        if item.mode == TreeItemMode::Tree {
            entries.extend(collect_tree_entries(store, &item.id, &path)?);
        } else {
            entries.insert(path, (item.id, item.mode));
        }
    }
    Ok(entries)
}

fn collect_manifest_paths<S: RestoreObjectSource>(
    store: &S,
    manifest_oid: &ObjectHash,
) -> Result<BTreeSet<String>, RestoreError> {
    let manifest = store.load_restore_object(manifest_oid)?;
    let value: serde_json::Value = serde_json::from_slice(&manifest)
        .map_err(|error| RestoreError::Storage(error.to_string()))?;
    Ok(value
        .get("files")
        .and_then(serde_json::Value::as_object)
        .map(|files| files.keys().cloned().collect())
        .unwrap_or_default())
}

fn restore_working_copy(
    storage: &ClientStorage,
    tree_oid: &ObjectHash,
    root: &Path,
) -> Result<(), RestoreError> {
    recover_restore_transactions(root)?;
    // Resolve every object before touching the existing worktree.  A missing
    // blob must be a normal restore error, never a partially destructive
    // restore.
    let mut leaves = Vec::new();
    collect_storage_tree(storage, tree_oid, Path::new(""), &mut leaves)?;
    let transaction_root = root
        .join(".libra")
        .join(format!("operation-restore-{}", Uuid::now_v7()));
    let stage = transaction_root.join("stage");
    let backup = transaction_root.join("backup");
    fs::create_dir_all(&stage)?;
    fs::create_dir_all(&backup)?;

    let mut moved_current = Vec::new();
    let mut installed = Vec::new();
    let result = (|| {
        for (path, mode, bytes) in &leaves {
            let destination = stage.join(path);
            if let Some(parent) = destination.parent() {
                fs::create_dir_all(parent)?;
            }
            write_restore_leaf(&destination, *mode, bytes)?;
        }

        let mut entries = fs::read_dir(root)?
            .map(|entry| entry.map(|entry| (entry.file_name(), entry.path())))
            .collect::<Result<Vec<_>, io::Error>>()?;
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        for (name, path) in entries {
            if is_private_worktree_entry(&name) {
                continue;
            }
            fs::rename(&path, backup.join(&name))?;
            moved_current.push(name);
        }

        let install_result = (|| {
            let mut staged_entries = fs::read_dir(&stage)?
                .map(|entry| entry.map(|entry| (entry.file_name(), entry.path())))
                .collect::<Result<Vec<_>, io::Error>>()?;
            staged_entries.sort_by(|left, right| left.0.cmp(&right.0));
            let manifest = staged_entries
                .iter()
                .map(|(name, _)| name.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            fs::write(
                transaction_root.join("manifest.json"),
                serde_json::to_vec(&manifest).map_err(io::Error::other)?,
            )?;
            for (name, path) in staged_entries {
                fs::rename(&path, root.join(&name))?;
                installed.push(name);
            }
            Ok::<(), io::Error>(())
        })();
        install_result?;
        Ok::<(), io::Error>(())
    })();

    match result {
        Ok(()) => fs::remove_dir_all(&transaction_root).map_err(RestoreError::Io),
        Err(primary) => {
            let cleanup = remove_named_entries(root, &installed)
                .and_then(|()| restore_named_entries(&backup, root, &moved_current))
                .and_then(|()| fs::remove_dir_all(&transaction_root));
            match cleanup {
                Ok(()) => Err(RestoreError::Io(primary)),
                Err(rollback) => Err(RestoreError::Storage(format!(
                    "working-copy restore failed: {primary}; rollback failed: {rollback}"
                ))),
            }
        }
    }
}

/// Recover a worktree swap left behind by a process interruption.  The swap
/// only moves top-level entries into a private backup directory, so the
/// recovery can restore the pre-swap names without guessing file contents.
pub fn recover_restore_transactions(root: &Path) -> Result<usize, RestoreError> {
    let metadata_root = root.join(".libra");
    let mut recovered = 0;
    let entries = match fs::read_dir(&metadata_root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(RestoreError::Io(error)),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("operation-restore-") || !entry.file_type()?.is_dir()
        {
            continue;
        }
        let transaction_root = entry.path();
        let backup = transaction_root.join("backup");
        let stage = transaction_root.join("stage");
        let mut names = Vec::new();
        for directory in [&backup, &stage] {
            if let Ok(items) = fs::read_dir(directory) {
                for item in items {
                    names.push(item?.file_name());
                }
            }
        }
        let manifest_path = transaction_root.join("manifest.json");
        if let Ok(bytes) = fs::read(manifest_path) {
            // The manifest is written before any staged entry is installed.
            // If a crash leaves it truncated, the backup/stage directory scan
            // above still contains the complete set needed for recovery.
            if let Ok(manifest) = serde_json::from_slice::<Vec<String>>(&bytes) {
                names.extend(manifest.into_iter().map(std::ffi::OsString::from));
            }
        }
        names.sort();
        names.dedup();
        remove_named_entries(root, &names)?;
        if backup.is_dir() {
            restore_named_entries(&backup, root, &names)?;
        }
        fs::remove_dir_all(transaction_root)?;
        recovered += 1;
    }
    Ok(recovered)
}

fn write_restore_leaf(
    destination: &Path,
    mode: TreeItemMode,
    bytes: &[u8],
) -> Result<(), io::Error> {
    if mode == TreeItemMode::Link {
        #[cfg(unix)]
        std::os::unix::fs::symlink(String::from_utf8_lossy(bytes).as_ref(), destination)?;
        #[cfg(windows)]
        std::os::windows::fs::symlink_file(String::from_utf8_lossy(bytes).as_ref(), destination)?;
        #[cfg(not(any(unix, windows)))]
        fs::write(destination, bytes)?;
    } else {
        fs::write(destination, bytes)?;
        #[cfg(unix)]
        if mode == TreeItemMode::BlobExecutable {
            use std::os::unix::fs::PermissionsExt;
            let mut permissions = fs::metadata(destination)?.permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(destination, permissions)?;
        }
    }
    Ok(())
}

fn collect_storage_tree(
    storage: &ClientStorage,
    tree_oid: &ObjectHash,
    prefix: &Path,
    leaves: &mut Vec<(PathBuf, TreeItemMode, Vec<u8>)>,
) -> Result<(), RestoreError> {
    let bytes = storage
        .get(tree_oid)
        .map_err(|error| RestoreError::Object {
            oid: *tree_oid,
            detail: error.to_string(),
        })?;
    let tree = Tree::from_bytes(&bytes, *tree_oid)
        .map_err(|error| RestoreError::Storage(error.to_string()))?;
    for item in tree.tree_items {
        if item.name.is_empty()
            || item.name == "."
            || item.name == ".."
            || item.name.contains('/')
            || item.name.contains('\\')
        {
            return Err(RestoreError::Storage(format!(
                "invalid restore tree entry name {:?}",
                item.name
            )));
        }
        let path = prefix.join(&item.name);
        if item.mode == TreeItemMode::Tree {
            collect_storage_tree(storage, &item.id, &path, leaves)?;
        } else {
            let bytes = storage
                .get(&item.id)
                .map_err(|error| RestoreError::Object {
                    oid: item.id,
                    detail: error.to_string(),
                })?;
            leaves.push((path, item.mode, bytes));
        }
    }
    Ok(())
}

fn is_private_worktree_entry(name: &std::ffi::OsStr) -> bool {
    name == ".libra" || name == ".git"
}

fn remove_named_entries(root: &Path, names: &[std::ffi::OsString]) -> Result<(), io::Error> {
    for name in names {
        let path = root.join(name);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            fs::remove_dir_all(path)?;
        } else {
            fs::remove_file(path)?;
        }
    }
    Ok(())
}

fn restore_named_entries(
    backup: &Path,
    root: &Path,
    names: &[std::ffi::OsString],
) -> Result<(), io::Error> {
    for name in names {
        let source = backup.join(name);
        if fs::symlink_metadata(&source).is_ok() {
            fs::rename(source, root.join(name))?;
        }
    }
    Ok(())
}

fn workspace_id(scope: &PinnedRequestScope) -> String {
    scope.scope.worktree_id().unwrap_or("main").to_string()
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use std::fs;

    use git_internal::internal::object::tree::TreeItem;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::{db, operation::CapturePolicy},
        utils::client_storage::ClientStorage,
    };

    #[test]
    fn selected_all_restores_every_mutable_surface() {
        let names = selected_facets(RestoreWhat::All);
        assert_eq!(
            names,
            vec![
                FacetName::from("working_copy"),
                FacetName::from("index"),
                FacetName::from("sequencer"),
                FacetName::from("sparse"),
                FacetName::from("head")
            ]
        );
    }

    #[test]
    fn restore_what_is_machine_readable() {
        let value = serde_json::to_string(&RestoreWhat::WorkingCopy).expect("serialize");
        assert_eq!(value, "\"working_copy\"");
    }

    #[test]
    fn missing_target_blob_does_not_touch_existing_worktree() {
        let root = tempdir().expect("worktree");
        fs::create_dir_all(root.path().join(".libra")).expect("metadata dir");
        fs::write(root.path().join("keep.txt"), b"keep").expect("existing file");
        let storage = ClientStorage::init_local(root.path().join("objects"));
        let missing = ObjectHash::from_type_and_data(ObjectType::Blob, b"missing");
        let tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            missing,
            "target.txt".to_string(),
        )])
        .expect("tree");
        let tree_bytes = tree.to_data().expect("tree bytes");
        let tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, &tree_bytes);
        storage
            .put(&tree_oid, &tree_bytes, ObjectType::Tree)
            .expect("tree object");
        let result = restore_working_copy(&storage, &tree_oid, root.path());
        assert!(result.is_err());
        assert_eq!(
            fs::read(root.path().join("keep.txt")).expect("keep file"),
            b"keep"
        );
        assert!(!root.path().join("target.txt").exists());
    }

    #[test]
    fn successful_worktree_swap_preserves_libra_metadata() {
        let root = tempdir().expect("worktree");
        fs::create_dir_all(root.path().join(".libra")).expect("metadata dir");
        fs::write(root.path().join("old.txt"), b"old").expect("old file");
        fs::write(root.path().join(".libra/pointer"), b"private").expect("private file");
        let storage = ClientStorage::init_local(root.path().join("objects"));
        let blob = ObjectHash::from_type_and_data(ObjectType::Blob, b"new");
        storage
            .put(&blob, b"new", ObjectType::Blob)
            .expect("blob object");
        let tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            blob,
            "new.txt".to_string(),
        )])
        .expect("tree");
        let tree_bytes = tree.to_data().expect("tree bytes");
        let tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, &tree_bytes);
        storage
            .put(&tree_oid, &tree_bytes, ObjectType::Tree)
            .expect("tree object");
        restore_working_copy(&storage, &tree_oid, root.path()).expect("swap");
        assert!(!root.path().join("old.txt").exists());
        assert_eq!(
            fs::read(root.path().join("new.txt")).expect("new file"),
            b"new"
        );
        assert_eq!(
            fs::read(root.path().join(".libra/pointer")).expect("private file"),
            b"private"
        );
    }

    #[test]
    fn interrupted_worktree_swap_is_recovered_from_backup() {
        let root = tempdir().expect("worktree");
        let transaction = root.path().join(".libra/operation-restore-interrupted");
        fs::create_dir_all(transaction.join("backup")).expect("backup directory");
        fs::create_dir_all(transaction.join("stage")).expect("stage directory");
        fs::write(root.path().join("old.txt"), b"new contents").expect("installed target");
        fs::write(transaction.join("backup/old.txt"), b"old contents").expect("backup");
        fs::write(transaction.join("manifest.json"), br#"["new.txt"]"#).expect("manifest");

        assert_eq!(
            recover_restore_transactions(root.path()).expect("recover"),
            1
        );
        assert_eq!(
            fs::read(root.path().join("old.txt")).expect("restored old file"),
            b"old contents"
        );
        assert!(!root.path().join("new.txt").exists());
        assert!(!transaction.exists());
    }

    #[test]
    fn truncated_manifest_falls_back_to_swap_directories() {
        let root = tempdir().expect("worktree");
        let transaction = root.path().join(".libra/operation-restore-truncated");
        fs::create_dir_all(transaction.join("backup")).expect("backup directory");
        fs::create_dir_all(transaction.join("stage")).expect("stage directory");
        fs::write(root.path().join("new.txt"), b"installed target").expect("installed target");
        fs::write(transaction.join("backup/old.txt"), b"old contents").expect("backup");
        fs::write(transaction.join("stage/new.txt"), b"staged target").expect("stage");
        fs::write(transaction.join("manifest.json"), b"[\"new.txt\"").expect("manifest");

        assert_eq!(
            recover_restore_transactions(root.path()).expect("recover"),
            1
        );
        assert_eq!(
            fs::read(root.path().join("old.txt")).expect("restored old file"),
            b"old contents"
        );
        assert!(!root.path().join("new.txt").exists());
        assert!(!transaction.exists());
    }

    #[tokio::test]
    async fn changed_paths_counts_only_the_delta() {
        let dir = tempdir().expect("store directory");
        let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
            .await
            .expect("database");
        let storage = ClientStorage::init_local(dir.path().join("objects"));
        let store = OperationStoreV2::new_for_repo("repo", database, storage);
        let same = ObjectHash::from_type_and_data(ObjectType::Blob, b"same");
        let added = ObjectHash::from_type_and_data(ObjectType::Blob, b"added");
        store
            .write_blob(&same, b"same", ObjectType::Blob)
            .expect("same blob");
        store
            .write_blob(&added, b"added", ObjectType::Blob)
            .expect("added blob");
        let current_tree = Tree::from_tree_items(vec![TreeItem::new(
            TreeItemMode::Blob,
            same,
            "same.txt".to_string(),
        )])
        .expect("current tree");
        let target_tree = Tree::from_tree_items(vec![
            TreeItem::new(TreeItemMode::Blob, same, "same.txt".to_string()),
            TreeItem::new(TreeItemMode::Blob, added, "added.txt".to_string()),
        ])
        .expect("target tree");
        let current_bytes = current_tree.to_data().expect("current tree bytes");
        let target_bytes = target_tree.to_data().expect("target tree bytes");
        let current_tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, &current_bytes);
        let target_tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, &target_bytes);
        store
            .write_blob(&current_tree_oid, &current_bytes, ObjectType::Tree)
            .expect("current tree object");
        store
            .write_blob(&target_tree_oid, &target_bytes, ObjectType::Tree)
            .expect("target tree object");
        let manifest = br#"{"schema_version":1,"files":{}}"#;
        let manifest_oid = ObjectHash::from_type_and_data(ObjectType::Blob, manifest);
        store
            .write_blob(&manifest_oid, manifest, ObjectType::Blob)
            .expect("manifest");
        let head = HeadState::Detached { oid: same };
        let current = WorkspaceSnapshotV2 {
            schema_version: 2,
            workspace_id: "main".to_string(),
            head: head.clone(),
            index_tree_oid: same,
            raw_index_blob_oid: same,
            working_copy_tree_oid: current_tree_oid,
            untracked_manifest_oid: manifest_oid,
            sparse_facet_oid: None,
            sequencer_facet_oid: None,
            worktree_generation: 0,
            capture_policy: CapturePolicy::TrackedAndUntracked,
            completeness: Completeness::Full,
            facet_restore_policies: Default::default(),
        };
        let mut target = current.clone();
        target.working_copy_tree_oid = target_tree_oid;
        assert_eq!(
            count_changed_paths(&store, &store, &current, &target, RestoreWhat::WorkingCopy,)
                .expect("delta count"),
            1
        );
    }
}
