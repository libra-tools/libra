//! Durable storage for the v2 operation DAG.
//!
//! The operation row is deliberately small and redacted.  View manifests are
//! content-addressed objects, while SQLite stores the searchable DAG edges,
//! head generations, and recovery journal.  A publish therefore follows one
//! ordering rule: write immutable objects first, then publish the relational
//! rows in a write-locked transaction.

use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    fmt,
    str::FromStr,
};

use chrono::Utc;
use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, QueryResult, Statement};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    internal::{
        db::begin_write_transaction,
        operation::view::{RepoViewV2, ViewError},
    },
    utils::client_storage::ClientStorage,
};

/// The semantic kind of an operation-log entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Command,
    ExternalSnapshot,
    Undo,
    Redo,
    Restore,
    Revert,
    Reconcile,
}

impl OperationKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Command => "command",
            Self::ExternalSnapshot => "external_snapshot",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::Restore => "restore",
            Self::Revert => "revert",
            Self::Reconcile => "reconcile",
        }
    }
}

impl fmt::Display for OperationKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OperationKind {
    type Err = StoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "command" => Ok(Self::Command),
            "external_snapshot" => Ok(Self::ExternalSnapshot),
            "undo" => Ok(Self::Undo),
            "redo" => Ok(Self::Redo),
            "restore" => Ok(Self::Restore),
            "revert" => Ok(Self::Revert),
            "reconcile" => Ok(Self::Reconcile),
            _ => Err(StoreError::InvalidEnum {
                field: "operation kind",
                value: value.to_string(),
            }),
        }
    }
}

/// Lifecycle state persisted for an operation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationStatusV2 {
    Running,
    Success,
    Failed,
    Partial,
    Aborted,
}

impl OperationStatusV2 {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Success => "success",
            Self::Failed => "failed",
            Self::Partial => "partial",
            Self::Aborted => "aborted",
        }
    }
}

impl fmt::Display for OperationStatusV2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for OperationStatusV2 {
    type Err = StoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "running" => Ok(Self::Running),
            "success" => Ok(Self::Success),
            "failed" => Ok(Self::Failed),
            "partial" => Ok(Self::Partial),
            "aborted" => Ok(Self::Aborted),
            _ => Err(StoreError::InvalidEnum {
                field: "operation status",
                value: value.to_string(),
            }),
        }
    }
}

/// Searchable, redacted operation metadata.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationMetaV2 {
    pub command_name: Option<String>,
    pub description: Option<String>,
    pub args_digest: Option<String>,
    pub actor: Option<String>,
    pub causal_context_id: Option<String>,
}

/// Immutable operation payload stored alongside the operation row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct OperationV2 {
    pub op_id: String,
    pub parent_op_ids: Vec<String>,
    pub pre_view_oid: ObjectHash,
    pub post_view_oid: ObjectHash,
    pub kind: OperationKind,
    pub status: OperationStatusV2,
    pub metadata: OperationMetaV2,
    pub restores_op_id: Option<String>,
    pub reverts_op_id: Option<String>,
    pub predecessor_map_oid: Option<ObjectHash>,
}

/// Recovery journal phase.  Phases are monotonic for a given journal id.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    Reserved,
    PreView,
    Mutation,
    PostView,
    Publish,
}

impl JournalPhase {
    fn rank(self) -> u8 {
        match self {
            Self::Reserved => 0,
            Self::PreView => 1,
            Self::Mutation => 2,
            Self::PostView => 3,
            Self::Publish => 4,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Reserved => "reserved",
            Self::PreView => "pre_view",
            Self::Mutation => "mutation",
            Self::PostView => "post_view",
            Self::Publish => "publish",
        }
    }
}

impl fmt::Display for JournalPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for JournalPhase {
    type Err = StoreError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "reserved" => Ok(Self::Reserved),
            "pre_view" => Ok(Self::PreView),
            "mutation" => Ok(Self::Mutation),
            "post_view" => Ok(Self::PostView),
            "publish" => Ok(Self::Publish),
            _ => Err(StoreError::InvalidEnum {
                field: "journal phase",
                value: value.to_string(),
            }),
        }
    }
}

/// One durable recovery-journal record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    pub journal_id: String,
    pub op_id: String,
    pub phase: JournalPhase,
    pub pre_view_oid: Option<ObjectHash>,
    pub target_view_oid: Option<ObjectHash>,
    pub owner: String,
    pub updated_at: i64,
    pub recovery_payload: Option<String>,
}

/// The currently published heads and their generation numbers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OpHeadsView {
    pub heads: BTreeMap<String, u64>,
    /// Direct parent edges, keyed by child operation id.  Keeping these in
    /// the view lets working-copy freshness distinguish a sibling from a
    /// descendant without making a database query on every check.
    pub ancestors: BTreeMap<String, BTreeSet<String>>,
}

impl OpHeadsView {
    pub fn new(heads: Vec<String>) -> Result<Self, StoreError> {
        Self::with_generations(heads.into_iter().map(|head| (head, 0)).collect())
    }

    pub fn with_generations(heads: Vec<(String, u64)>) -> Result<Self, StoreError> {
        let mut view = Self::default();
        for (head, generation) in heads {
            if head.is_empty() {
                return Err(StoreError::Validation(
                    "operation head id cannot be empty".to_string(),
                ));
            }
            if view.heads.insert(head.clone(), generation).is_some() {
                return Err(StoreError::Validation(format!(
                    "duplicate operation head id '{head}'"
                )));
            }
        }
        Ok(view)
    }

    pub fn head_ids(&self) -> Vec<String> {
        self.heads.keys().cloned().collect()
    }

    pub fn generation(&self, op_id: &str) -> Option<u64> {
        self.heads.get(op_id).copied()
    }

    pub fn add_ancestor(
        &mut self,
        child_op_id: impl Into<String>,
        parent_op_id: impl Into<String>,
    ) -> Result<(), StoreError> {
        let child_op_id = child_op_id.into();
        let parent_op_id = parent_op_id.into();
        if child_op_id.is_empty() || parent_op_id.is_empty() {
            return Err(StoreError::Validation(
                "operation parent ids cannot be empty".to_string(),
            ));
        }
        self.ancestors
            .entry(child_op_id)
            .or_default()
            .insert(parent_op_id);
        Ok(())
    }

    /// Returns true when `ancestor` is the same operation or is reachable by
    /// following parent edges from `descendant`.
    pub fn is_ancestor(&self, ancestor: &str, descendant: &str) -> bool {
        if ancestor == descendant {
            return true;
        }
        let mut pending = vec![descendant.to_string()];
        let mut visited = BTreeSet::new();
        while let Some(current) = pending.pop() {
            if !visited.insert(current.clone()) {
                continue;
            }
            if let Some(parents) = self.ancestors.get(&current) {
                if parents.contains(ancestor) {
                    return true;
                }
                pending.extend(parents.iter().cloned());
            }
        }
        false
    }
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("database error: {0}")]
    Database(#[from] DbErr),
    #[error("object storage error: {0}")]
    Object(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("invalid object hash '{0}'")]
    InvalidObjectHash(String),
    #[error("invalid {field}: {value}")]
    InvalidEnum { field: &'static str, value: String },
    #[error("compare-and-swap conflict; current heads: {current_heads:?}")]
    CasConflict { current_heads: Vec<String> },
    #[error("validation error: {0}")]
    Validation(String),
    #[error("view error: {0}")]
    View(#[from] ViewError),
    #[error("operation or journal entry not found: {0}")]
    NotFound(String),
}

/// SQLite or redb plus content-addressed object storage for operation-log v2.
#[derive(Clone)]
pub struct OperationStoreV2 {
    db: Option<DatabaseConnection>,
    state_store: Option<std::sync::Arc<dyn crate::storage::git_state_store::GitStateStore>>,
    storage: ClientStorage,
    repo_id: String,
}

const HEAD_GENERATION_SENTINEL: &str = "__scope_generation__";

impl OperationStoreV2 {
    /// Construct a store without a repository id.  Use [`Self::for_repo`] or
    /// [`Self::new_for_repo`] for writes; the empty-id constructor is useful
    /// for view-only callers and keeps database/storage wiring lightweight.
    pub fn new(db: DatabaseConnection, storage: ClientStorage) -> Self {
        Self {
            db: Some(db),
            state_store: None,
            storage,
            repo_id: String::new(),
        }
    }

    pub fn new_for_repo(
        repo_id: impl Into<String>,
        db: DatabaseConnection,
        storage: ClientStorage,
    ) -> Self {
        Self {
            db: Some(db),
            state_store: None,
            storage,
            repo_id: repo_id.into(),
        }
    }

    /// Construct a backend-agnostic operation store backed by [`GitStateStore`].
    pub fn new_for_repo_with_store(
        repo_id: impl Into<String>,
        state_store: std::sync::Arc<dyn crate::storage::git_state_store::GitStateStore>,
        storage: ClientStorage,
    ) -> Self {
        Self {
            db: None,
            state_store: Some(state_store),
            storage,
            repo_id: repo_id.into(),
        }
    }

    pub fn for_repo(mut self, repo_id: impl Into<String>) -> Self {
        self.repo_id = repo_id.into();
        self
    }

    pub fn backend_kind(&self) -> crate::storage::StorageBackendKind {
        if let Some(store) = &self.state_store {
            store.backend()
        } else {
            crate::storage::StorageBackendKind::Sqlite
        }
    }

    pub fn state_store(
        &self,
    ) -> Option<&std::sync::Arc<dyn crate::storage::git_state_store::GitStateStore>> {
        self.state_store.as_ref()
    }

    pub fn db(&self) -> &DatabaseConnection {
        self.db
            .as_ref()
            .expect("OperationStoreV2::db() called on a non-SQLite store")
    }

    fn require_db(&self) -> Result<&DatabaseConnection, StoreError> {
        self.db.as_ref().ok_or_else(|| {
            StoreError::Database(DbErr::Custom(
                "OperationStoreV2 has no SQLite connection".to_string(),
            ))
        })
    }

    fn require_state_store(
        &self,
    ) -> Result<&std::sync::Arc<dyn crate::storage::git_state_store::GitStateStore>, StoreError>
    {
        self.state_store.as_ref().ok_or_else(|| {
            StoreError::Database(DbErr::Custom(
                "OperationStoreV2 has no GitStateStore handle".to_string(),
            ))
        })
    }

    pub fn storage(&self) -> ClientStorage {
        self.storage.clone()
    }

    pub fn write_view_manifest(&self, view: &RepoViewV2) -> Result<ObjectHash, StoreError> {
        view.validate_recursive_closure(|oid| self.storage.get(oid).ok())?;
        let bytes = view.to_canonical_bytes()?;
        let oid = ObjectHash::from_type_and_data(ObjectType::Blob, &bytes);
        self.storage
            .put(&oid, &bytes, ObjectType::Blob)
            .map_err(|error| StoreError::Object(error.to_string()))?;
        Ok(oid)
    }

    pub fn load_view(&self, oid: &ObjectHash) -> Result<RepoViewV2, StoreError> {
        let bytes = self
            .storage
            .get(oid)
            .map_err(|error| StoreError::Object(error.to_string()))?;
        RepoViewV2::from_canonical_bytes(&bytes).map_err(StoreError::View)
    }

    /// Load a canonical workspace snapshot referenced by a repository view.
    pub fn load_snapshot(
        &self,
        oid: &ObjectHash,
    ) -> Result<super::view::WorkspaceSnapshotV2, StoreError> {
        let bytes = self
            .storage
            .get(oid)
            .map_err(|error| StoreError::Object(error.to_string()))?;
        super::view::WorkspaceSnapshotV2::from_canonical_bytes(&bytes).map_err(StoreError::View)
    }

    /// Load an immutable object payload for a restore or doctor operation.
    pub fn load_object(&self, oid: &ObjectHash) -> Result<Vec<u8>, StoreError> {
        self.storage
            .get(oid)
            .map_err(|error| StoreError::Object(error.to_string()))
    }

    pub fn is_object_type(&self, oid: &ObjectHash, object_type: ObjectType) -> bool {
        self.storage.is_object_type(oid, object_type)
    }

    /// Like [`Self::is_object_type`], but a storage read failure stays an error.
    ///
    /// A missing object is `Ok(false)`. Only a failed presence or type read
    /// becomes [`StoreError::Object`], so callers can keep their absence text.
    pub fn is_object_type_checked(
        &self,
        oid: &ObjectHash,
        object_type: ObjectType,
    ) -> Result<bool, StoreError> {
        match self.storage.exist_checked(oid) {
            Ok(false) => Ok(false),
            Err(error) => Err(StoreError::Object(error.to_string())),
            Ok(true) => match self.storage.get_object_type(oid) {
                Ok(found) => Ok(found == object_type),
                Err(git_internal::errors::GitError::ObjectNotFound(_)) => Ok(false),
                Err(error) => Err(StoreError::Object(error.to_string())),
            },
        }
    }

    /// Store one immutable content-addressed object for a view facet.
    ///
    /// View publication uses this narrow seam for the refs facet; callers
    /// cannot mutate the store's backend or bypass the object hash check.
    pub(crate) fn write_blob(
        &self,
        oid: &ObjectHash,
        bytes: &[u8],
        object_type: ObjectType,
    ) -> Result<(), StoreError> {
        self.storage
            .put(oid, bytes, object_type)
            .map_err(|error| StoreError::Object(error.to_string()))?;
        Ok(())
    }

    pub async fn write_operation(&self, operation: &OperationV2) -> Result<(), StoreError> {
        self.write_operation_with_scope_and_restorable(
            operation,
            None,
            "repository",
            "declared",
            true,
        )
        .await
    }

    pub async fn write_operation_with_restorable(
        &self,
        operation: &OperationV2,
        restorable: bool,
    ) -> Result<(), StoreError> {
        self.write_operation_with_scope_and_restorable(
            operation,
            None,
            "repository",
            "declared",
            restorable,
        )
        .await
    }

    pub async fn write_operation_with_scope_and_restorable(
        &self,
        operation: &OperationV2,
        worktree_id: Option<&str>,
        scope_kind: &str,
        scope_provenance: &str,
        restorable: bool,
    ) -> Result<(), StoreError> {
        if self.repo_id.is_empty() {
            return Err(StoreError::Validation(
                "operation store repository id cannot be empty".to_string(),
            ));
        }
        if operation.op_id.is_empty() || operation.op_id == HEAD_GENERATION_SENTINEL {
            return Err(StoreError::Validation(
                "operation id cannot be empty".to_string(),
            ));
        }
        if !matches!(scope_kind, "main" | "linked" | "repository" | "unknown") {
            return Err(StoreError::Validation(format!(
                "invalid operation scope kind '{scope_kind}'"
            )));
        }
        if !matches!(scope_provenance, "declared" | "unknown") {
            return Err(StoreError::Validation(format!(
                "invalid operation scope provenance '{scope_provenance}'"
            )));
        }
        validate_parent_ids(&operation.op_id, &operation.parent_op_ids)?;

        let start_ts = Utc::now().timestamp_millis();
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let scope_kind_parsed =
                    crate::storage::git_state_store::OperationScopeKind::parse(scope_kind)
                        .map_err(|err| StoreError::Validation(err.to_string()))?;
                let scope_prov_parsed =
                    crate::storage::git_state_store::OperationScopeProvenance::parse(
                        scope_provenance,
                    )
                    .map_err(|err| StoreError::Validation(err.to_string()))?;
                let rec = crate::storage::git_state_store::OperationRecord {
                    op_id: operation.op_id.clone(),
                    repo_id: self.repo_id.clone(),
                    format_version: 2,
                    kind: operation.kind.to_string(),
                    status: operation.status.to_string(),
                    command_name: operation.metadata.command_name.clone(),
                    description: operation.metadata.description.clone(),
                    args_digest: operation.metadata.args_digest.clone(),
                    actor: operation.metadata.actor.clone(),
                    worktree_id: worktree_id.map(ToOwned::to_owned),
                    scope_kind: scope_kind_parsed,
                    scope_provenance: scope_prov_parsed,
                    pre_view_oid: operation.pre_view_oid.to_string(),
                    post_view_oid: operation.post_view_oid.to_string(),
                    restores_op_id: operation.restores_op_id.clone(),
                    reverts_op_id: operation.reverts_op_id.clone(),
                    predecessor_map_oid: operation.predecessor_map_oid.map(|oid| oid.to_string()),
                    causal_context_id: operation.metadata.causal_context_id.clone(),
                    restorable,
                    control_slot: None,
                    claim_owner: None,
                    start_ts,
                    end_ts: None,
                };
                store
                    .append_operation_atomic(&rec, &operation.parent_op_ids, None)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(())
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let txn = begin_write_transaction(db).await?;
                let insert_result = txn
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "INSERT INTO operation (op_id, repo_id, format_version, kind, status, \
                         command_name, description, args_digest, actor, worktree_id, scope_kind, \
                          scope_provenance, \
                          pre_view_oid, post_view_oid, restores_op_id, reverts_op_id, \
                          predecessor_map_oid, causal_context_id, restorable, start_ts, end_ts) \
                          VALUES (?, ?, 2, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, NULL)",
                        [
                            operation.op_id.clone().into(),
                            self.repo_id.clone().into(),
                            operation.kind.to_string().into(),
                            operation.status.to_string().into(),
                            operation.metadata.command_name.clone().into(),
                            operation.metadata.description.clone().into(),
                            operation.metadata.args_digest.clone().into(),
                            operation.metadata.actor.clone().into(),
                            worktree_id.map(ToOwned::to_owned).into(),
                            scope_kind.to_string().into(),
                            scope_provenance.to_string().into(),
                            operation.pre_view_oid.to_string().into(),
                            operation.post_view_oid.to_string().into(),
                            operation.restores_op_id.clone().into(),
                            operation.reverts_op_id.clone().into(),
                            operation
                                .predecessor_map_oid
                                .map(|oid| oid.to_string())
                                .into(),
                            operation.metadata.causal_context_id.clone().into(),
                            (restorable as i64).into(),
                            start_ts.into(),
                        ],
                    ))
                    .await;
                if let Err(error) = insert_result {
                    let _ = txn.rollback().await;
                    return Err(StoreError::Database(error));
                }

                for (ordinal, parent_op_id) in operation.parent_op_ids.iter().enumerate() {
                    if let Err(error) = txn
                        .execute_raw(Statement::from_sql_and_values(
                            DbBackend::Sqlite,
                            "INSERT INTO operation_parent (op_id, parent_op_id, ordinal) VALUES (?, ?, ?)",
                            [
                                operation.op_id.clone().into(),
                                parent_op_id.clone().into(),
                                (ordinal as i64).into(),
                            ],
                        ))
                        .await
                    {
                        let _ = txn.rollback().await;
                        return Err(StoreError::Database(error));
                    }
                }
                txn.commit().await?;
                Ok(())
            }
        }
    }

    pub async fn operation_is_restorable(&self, op_id: &str) -> Result<Option<bool>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let Some((rec, _)) = store
                    .get_operation(&self.repo_id, op_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?
                else {
                    return Ok(None);
                };
                Ok(Some(rec.restorable))
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let row = db
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT restorable FROM operation WHERE repo_id = ? AND op_id = ?",
                        [self.repo_id.clone().into(), op_id.to_string().into()],
                    ))
                    .await?;
                let Some(row) = row else {
                    return Ok(None);
                };
                match row.try_get::<i64>("", "restorable")? {
                    0 => Ok(Some(false)),
                    1 => Ok(Some(true)),
                    value => Err(StoreError::Validation(format!(
                        "operation '{op_id}' has invalid restorable value {value}"
                    ))),
                }
            }
        }
    }

    pub async fn operation_scope(
        &self,
        op_id: &str,
    ) -> Result<Option<(Option<String>, String, String)>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let Some((rec, _)) = store
                    .get_operation(&self.repo_id, op_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?
                else {
                    return Ok(None);
                };
                Ok(Some((
                    rec.worktree_id,
                    rec.scope_kind.as_str().to_string(),
                    rec.scope_provenance.as_str().to_string(),
                )))
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let row = db
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT worktree_id, scope_kind, scope_provenance \
                         FROM operation WHERE repo_id = ? AND op_id = ?",
                        [self.repo_id.clone().into(), op_id.to_string().into()],
                    ))
                    .await?;
                let Some(row) = row else {
                    return Ok(None);
                };
                Ok(Some((
                    row.try_get::<Option<String>>("", "worktree_id")?,
                    row.try_get::<String>("", "scope_kind")?,
                    row.try_get::<String>("", "scope_provenance")?,
                )))
            }
        }
    }

    /// Load one operation and its ordered parent edges for target validation.
    pub async fn load_operation(&self, op_id: &str) -> Result<Option<OperationV2>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let Some((rec, parent_op_ids)) = store
                    .get_operation(&self.repo_id, op_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?
                else {
                    return Ok(None);
                };
                let pre_view_oid = rec
                    .pre_view_oid
                    .parse()
                    .map_err(|_| StoreError::InvalidObjectHash(rec.pre_view_oid.clone()))?;
                let post_view_oid = rec
                    .post_view_oid
                    .parse()
                    .map_err(|_| StoreError::InvalidObjectHash(rec.post_view_oid.clone()))?;
                let predecessor_map_oid = rec
                    .predecessor_map_oid
                    .map(|v| v.parse().map_err(|_| StoreError::InvalidObjectHash(v)))
                    .transpose()?;
                Ok(Some(OperationV2 {
                    op_id: rec.op_id,
                    parent_op_ids,
                    pre_view_oid,
                    post_view_oid,
                    kind: rec.kind.parse()?,
                    status: rec.status.parse()?,
                    metadata: OperationMetaV2 {
                        command_name: rec.command_name,
                        description: rec.description,
                        args_digest: rec.args_digest,
                        actor: rec.actor,
                        causal_context_id: rec.causal_context_id,
                    },
                    restores_op_id: rec.restores_op_id,
                    reverts_op_id: rec.reverts_op_id,
                    predecessor_map_oid,
                }))
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let row = db
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT kind, status, command_name, description, args_digest, actor, \
                         pre_view_oid, post_view_oid, restores_op_id, reverts_op_id, \
                         predecessor_map_oid, causal_context_id \
                         FROM operation WHERE repo_id = ? AND op_id = ?",
                        [self.repo_id.clone().into(), op_id.to_string().into()],
                    ))
                    .await?;
                let Some(row) = row else {
                    return Ok(None);
                };
                let parent_rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT parent_op_id FROM operation_parent WHERE op_id = ? ORDER BY ordinal",
                        [op_id.to_string().into()],
                    ))
                    .await?;
                let mut parent_op_ids = Vec::with_capacity(parent_rows.len());
                for parent in parent_rows {
                    parent_op_ids.push(parent.try_get::<String>("", "parent_op_id")?);
                }

                let parse_oid = |field: &'static str| -> Result<ObjectHash, StoreError> {
                    let value = row.try_get::<String>("", field)?;
                    value
                        .parse()
                        .map_err(|_| StoreError::InvalidObjectHash(value))
                };
                let parse_optional_oid =
                    |field: &'static str| -> Result<Option<ObjectHash>, StoreError> {
                        let value = row.try_get::<Option<String>>("", field)?;
                        value
                            .map(|value| {
                                value
                                    .parse()
                                    .map_err(|_| StoreError::InvalidObjectHash(value))
                            })
                            .transpose()
                    };

                Ok(Some(OperationV2 {
                    op_id: op_id.to_string(),
                    parent_op_ids,
                    pre_view_oid: parse_oid("pre_view_oid")?,
                    post_view_oid: parse_oid("post_view_oid")?,
                    kind: row.try_get::<String>("", "kind")?.parse()?,
                    status: row.try_get::<String>("", "status")?.parse()?,
                    metadata: OperationMetaV2 {
                        command_name: row.try_get("", "command_name")?,
                        description: row.try_get("", "description")?,
                        args_digest: row.try_get("", "args_digest")?,
                        actor: row.try_get("", "actor")?,
                        causal_context_id: row.try_get("", "causal_context_id")?,
                    },
                    restores_op_id: row.try_get("", "restores_op_id")?,
                    reverts_op_id: row.try_get("", "reverts_op_id")?,
                    predecessor_map_oid: parse_optional_oid("predecessor_map_oid")?,
                }))
            }
        }
    }

    pub async fn list_operations(&self) -> Result<Vec<OperationV2>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let rows = store
                    .list_operations(&self.repo_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                let mut operations = Vec::with_capacity(rows.len());
                for (rec, parent_op_ids) in rows {
                    let pre_view_oid = rec
                        .pre_view_oid
                        .parse()
                        .map_err(|_| StoreError::InvalidObjectHash(rec.pre_view_oid.clone()))?;
                    let post_view_oid = rec
                        .post_view_oid
                        .parse()
                        .map_err(|_| StoreError::InvalidObjectHash(rec.post_view_oid.clone()))?;
                    let predecessor_map_oid = rec
                        .predecessor_map_oid
                        .map(|v| v.parse().map_err(|_| StoreError::InvalidObjectHash(v)))
                        .transpose()?;
                    operations.push(OperationV2 {
                        op_id: rec.op_id,
                        parent_op_ids,
                        pre_view_oid,
                        post_view_oid,
                        kind: rec.kind.parse()?,
                        status: rec.status.parse()?,
                        metadata: OperationMetaV2 {
                            command_name: rec.command_name,
                            description: rec.description,
                            args_digest: rec.args_digest,
                            actor: rec.actor,
                            causal_context_id: rec.causal_context_id,
                        },
                        restores_op_id: rec.restores_op_id,
                        reverts_op_id: rec.reverts_op_id,
                        predecessor_map_oid,
                    });
                }
                Ok(operations)
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                // Two bulk queries (operations + parent edges) instead of one
                // `load_operation` per row: reconcile walks the whole log, so the
                // previous N+1 pattern issued 2N+1 statements per call.
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT op_id, kind, status, command_name, description, args_digest, actor, \
                         pre_view_oid, post_view_oid, restores_op_id, reverts_op_id, \
                         predecessor_map_oid, causal_context_id \
                         FROM operation WHERE repo_id = ? ORDER BY start_ts, op_id",
                        [self.repo_id.clone().into()],
                    ))
                    .await?;
                let parent_rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT p.op_id, p.parent_op_id FROM operation_parent p \
                         JOIN operation o ON o.op_id = p.op_id \
                         WHERE o.repo_id = ? ORDER BY p.op_id, p.ordinal",
                        [self.repo_id.clone().into()],
                    ))
                    .await?;
                let mut parents_by_op: BTreeMap<String, Vec<String>> = BTreeMap::new();
                for row in parent_rows {
                    let op_id: String = row.try_get("", "op_id")?;
                    let parent_op_id: String = row.try_get("", "parent_op_id")?;
                    parents_by_op.entry(op_id).or_default().push(parent_op_id);
                }

                let mut operations = Vec::with_capacity(rows.len());
                for row in rows {
                    let op_id: String = row.try_get("", "op_id")?;
                    let parse_oid = |field: &'static str| -> Result<ObjectHash, StoreError> {
                        let value = row.try_get::<String>("", field)?;
                        value
                            .parse()
                            .map_err(|_| StoreError::InvalidObjectHash(value))
                    };
                    let parse_optional_oid =
                        |field: &'static str| -> Result<Option<ObjectHash>, StoreError> {
                            let value = row.try_get::<Option<String>>("", field)?;
                            value
                                .map(|value| {
                                    value
                                        .parse()
                                        .map_err(|_| StoreError::InvalidObjectHash(value))
                                })
                                .transpose()
                        };
                    operations.push(OperationV2 {
                        parent_op_ids: parents_by_op.remove(&op_id).unwrap_or_default(),
                        pre_view_oid: parse_oid("pre_view_oid")?,
                        post_view_oid: parse_oid("post_view_oid")?,
                        kind: row.try_get::<String>("", "kind")?.parse()?,
                        status: row.try_get::<String>("", "status")?.parse()?,
                        metadata: OperationMetaV2 {
                            command_name: row.try_get("", "command_name")?,
                            description: row.try_get("", "description")?,
                            args_digest: row.try_get("", "args_digest")?,
                            actor: row.try_get("", "actor")?,
                            causal_context_id: row.try_get("", "causal_context_id")?,
                        },
                        restores_op_id: row.try_get("", "restores_op_id")?,
                        reverts_op_id: row.try_get("", "reverts_op_id")?,
                        predecessor_map_oid: parse_optional_oid("predecessor_map_oid")?,
                        op_id,
                    });
                }
                Ok(operations)
            }
        }
    }

    /// Move a persisted operation to its terminal status after publication
    /// succeeds or a later CAS/publish step fails.
    pub async fn update_operation_status(
        &self,
        op_id: &str,
        status: OperationStatusV2,
    ) -> Result<(), StoreError> {
        if self.repo_id.is_empty() || op_id.is_empty() {
            return Err(StoreError::Validation(
                "operation status update requires repository and operation ids".to_string(),
            ));
        }
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let updated = store
                    .update_operation_status(
                        &self.repo_id,
                        op_id,
                        &status.to_string(),
                        None,
                        Some(Utc::now().timestamp_millis()),
                    )
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                if !updated {
                    return Err(StoreError::NotFound(op_id.to_string()));
                }
                Ok(())
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let updated = db
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "UPDATE operation SET status = ?, end_ts = ? WHERE repo_id = ? AND op_id = ?",
                        [
                            status.to_string().into(),
                            Utc::now().timestamp_millis().into(),
                            self.repo_id.clone().into(),
                            op_id.to_string().into(),
                        ],
                    ))
                    .await?;
                if updated.rows_affected() == 0 {
                    return Err(StoreError::NotFound(op_id.to_string()));
                }
                Ok(())
            }
        }
    }

    /// Attach the immutable post-view to a still-running operation before
    /// publication.  Keeping the row Running until head CAS and pointer
    /// advancement complete lets recovery distinguish an interrupted publish
    /// from a successful operation.
    pub async fn update_operation_post_view(
        &self,
        op_id: &str,
        post_view_oid: &ObjectHash,
    ) -> Result<(), StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let Some((existing, _)) = store
                    .get_operation(&self.repo_id, op_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?
                else {
                    return Err(StoreError::NotFound(op_id.to_string()));
                };
                let post_str = post_view_oid.to_string();
                let updated = store
                    .update_operation_status(
                        &self.repo_id,
                        op_id,
                        &existing.status,
                        Some(&post_str),
                        existing.end_ts,
                    )
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                if !updated {
                    return Err(StoreError::NotFound(op_id.to_string()));
                }
                Ok(())
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let updated = db
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "UPDATE operation SET post_view_oid = ? WHERE repo_id = ? AND op_id = ?",
                        [
                            post_view_oid.to_string().into(),
                            self.repo_id.clone().into(),
                            op_id.to_string().into(),
                        ],
                    ))
                    .await?;
                if updated.rows_affected() == 0 {
                    return Err(StoreError::NotFound(op_id.to_string()));
                }
                Ok(())
            }
        }
    }

    /// Atomically replaces the head set if its current value equals
    /// `expected_heads`.  A conflict leaves the database untouched.
    pub async fn cas_update_op_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_heads: &[String],
        new_heads: &[String],
    ) -> Result<u64, StoreError> {
        self.cas_update_op_heads_inner(repo_id, scope_key, expected_heads, new_heads, None)
            .await
    }

    /// Strict CAS with an explicit scope generation token.
    ///
    /// The generation is persisted in the same operation_head table even when
    /// the logical head set is empty, so an A -> B -> A sequence cannot pass
    /// an old expected token.
    pub async fn cas_update_op_heads_at_generation(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_generation: u64,
        expected_heads: &[String],
        new_heads: &[String],
    ) -> Result<u64, StoreError> {
        self.cas_update_op_heads_inner(
            repo_id,
            scope_key,
            expected_heads,
            new_heads,
            Some(expected_generation),
        )
        .await
    }

    async fn cas_update_op_heads_inner(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_heads: &[String],
        new_heads: &[String],
        expected_generation: Option<u64>,
    ) -> Result<u64, StoreError> {
        let expected = normalize_heads(expected_heads)?;
        let replacement = normalize_heads(new_heads)?;
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                match store
                    .cas_update_operation_heads(
                        repo_id,
                        scope_key,
                        expected_generation,
                        &expected,
                        &replacement,
                    )
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?
                {
                    crate::storage::git_state_store::OperationHeadCasOutcome::Applied {
                        generation,
                        ..
                    } => Ok(generation),
                    crate::storage::git_state_store::OperationHeadCasOutcome::CasConflict {
                        actual_heads,
                        ..
                    } => Err(StoreError::CasConflict {
                        current_heads: actual_heads,
                    }),
                }
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let txn = begin_write_transaction(db).await?;
                let current_rows = match query_head_rows(&txn, repo_id, scope_key).await {
                    Ok(rows) => rows,
                    Err(error) => {
                        let _ = txn.rollback().await;
                        return Err(error);
                    }
                };
                let current_heads: Vec<String> = current_rows
                    .iter()
                    .map(|(op_id, _)| op_id.clone())
                    .collect();
                let current_generation =
                    match query_scope_generation(&txn, repo_id, scope_key).await {
                        Ok(generation) => generation,
                        Err(error) => {
                            let _ = txn.rollback().await;
                            return Err(error);
                        }
                    };
                if current_heads != expected
                    || expected_generation
                        .is_some_and(|generation| generation != current_generation)
                {
                    let _ = txn.rollback().await;
                    return Err(StoreError::CasConflict { current_heads });
                }

                let generation = current_generation.saturating_add(1);
                if let Err(error) =
                    replace_head_rows(&txn, repo_id, scope_key, generation, &replacement).await
                {
                    let _ = txn.rollback().await;
                    return Err(error);
                }
                txn.commit().await?;
                Ok(generation)
            }
        }
    }

    /// Publish a candidate head set, preserving a concurrent candidate when
    /// the caller's expected set is stale. This is deliberately separate from
    /// strict CAS: callers opt into sibling retention instead of silently
    /// weakening the compare-and-swap contract.
    pub async fn merge_op_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
        expected_heads: &[String],
        candidate_heads: &[String],
    ) -> Result<u64, StoreError> {
        let expected = normalize_heads(expected_heads)?;
        let candidate = normalize_heads(candidate_heads)?;
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let outcome = store
                    .converge_operation_heads(repo_id, scope_key, &expected, &candidate)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(outcome.generation)
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let txn = begin_write_transaction(db).await?;
                let current_rows = match query_head_rows(&txn, repo_id, scope_key).await {
                    Ok(rows) => rows,
                    Err(error) => {
                        let _ = txn.rollback().await;
                        return Err(error);
                    }
                };
                let current_heads: Vec<String> = current_rows
                    .iter()
                    .map(|(op_id, _)| op_id.clone())
                    .collect();
                let replacement = if current_heads == expected {
                    candidate
                } else {
                    current_heads
                        .into_iter()
                        .chain(candidate)
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect()
                };
                let current_generation =
                    match query_scope_generation(&txn, repo_id, scope_key).await {
                        Ok(generation) => generation,
                        Err(error) => {
                            let _ = txn.rollback().await;
                            return Err(error);
                        }
                    };
                let generation = current_generation.saturating_add(1);
                if let Err(error) =
                    replace_head_rows(&txn, repo_id, scope_key, generation, &replacement).await
                {
                    let _ = txn.rollback().await;
                    return Err(error);
                }
                txn.commit().await?;
                Ok(generation)
            }
        }
    }

    pub async fn read_heads(
        &self,
        repo_id: &str,
        scope_key: &str,
    ) -> Result<Vec<String>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let snap = store
                    .get_operation_heads(repo_id, scope_key)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(snap.heads)
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                Ok(query_head_rows(db, repo_id, scope_key)
                    .await?
                    .into_iter()
                    .map(|(op_id, _)| op_id)
                    .collect())
            }
        }
    }

    /// Return the published operation heads across every worktree scope.
    ///
    /// The Web read model is repository-wide, so it must not silently pick a
    /// single request scope and hide concurrent heads from the caller.
    pub async fn read_all_heads(&self, repo_id: &str) -> Result<Vec<String>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let mut heads = store
                    .list_all_operation_heads(repo_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                heads.truncate(200);
                Ok(heads)
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT DISTINCT op_id FROM operation_head \
                         WHERE repo_id = ? AND op_id <> ? ORDER BY op_id LIMIT 200",
                        [repo_id.to_string().into(), HEAD_GENERATION_SENTINEL.into()],
                    ))
                    .await?;
                rows.into_iter()
                    .map(|row| {
                        row.try_get::<String>("", "op_id")
                            .map_err(StoreError::Database)
                    })
                    .collect()
            }
        }
    }

    /// Return a bounded breadth-first page of operation ids reachable from the
    /// repository's published heads. Each database read is bounded, and the
    /// traversal stops once the requested page plus one continuation row has
    /// been collected; it never materialises the full operation DAG.
    pub async fn graph_operation_ids(
        &self,
        repo_id: &str,
        depth: usize,
        limit: usize,
        offset: usize,
    ) -> Result<(Vec<String>, bool), StoreError> {
        let limit = limit.clamp(1, 200);
        let depth = depth.min(32);
        let requested = limit.saturating_add(1).min(201);
        let target = offset.saturating_add(requested);
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let mut frontier = self.read_all_heads(repo_id).await?;
                frontier.sort();
                let mut seen = HashSet::new();
                let mut ids = Vec::new();
                let mut current_depth = 0usize;
                while !frontier.is_empty() && current_depth <= depth && ids.len() < target {
                    frontier.sort();
                    let current = frontier
                        .into_iter()
                        .filter(|operation_id| seen.insert(operation_id.clone()))
                        .take(target.saturating_sub(ids.len()))
                        .collect::<Vec<_>>();
                    ids.extend(current.iter().cloned());
                    if current_depth == depth || ids.len() >= target {
                        break;
                    }
                    let mut next = BTreeSet::new();
                    for op_id in current {
                        if let Some((_, parents)) = store
                            .get_operation(repo_id, &op_id)
                            .await
                            .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?
                        {
                            for parent in parents {
                                if next.len() < 200 {
                                    next.insert(parent);
                                }
                            }
                        }
                    }
                    frontier = next.into_iter().collect();
                    current_depth = current_depth.saturating_add(1);
                }
                let has_more = ids.len() > offset.saturating_add(limit);
                let page = ids.into_iter().skip(offset).take(limit).collect();
                Ok((page, has_more))
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let head_rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT DISTINCT op_id FROM operation_head \
                         WHERE repo_id = ? AND op_id <> ? ORDER BY op_id LIMIT 200",
                        [repo_id.to_string().into(), HEAD_GENERATION_SENTINEL.into()],
                    ))
                    .await?;
                let mut frontier = head_rows
                    .into_iter()
                    .map(|row| {
                        row.try_get::<String>("", "op_id")
                            .map_err(StoreError::Database)
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                frontier.sort();
                let mut seen = HashSet::new();
                let mut ids = Vec::new();
                let mut current_depth = 0usize;
                while !frontier.is_empty() && current_depth <= depth && ids.len() < target {
                    frontier.sort();
                    let current = frontier
                        .into_iter()
                        .filter(|operation_id| seen.insert(operation_id.clone()))
                        .take(target.saturating_sub(ids.len()))
                        .collect::<Vec<_>>();
                    ids.extend(current.iter().cloned());
                    if current_depth == depth || ids.len() >= target {
                        break;
                    }

                    let placeholders = vec!["?"; current.len()].join(", ");
                    let sql = format!(
                        "SELECT p.parent_op_id FROM operation_parent p \
                         JOIN operation o ON o.op_id = p.parent_op_id \
                         WHERE o.repo_id = ? AND p.op_id IN ({placeholders}) \
                         ORDER BY p.parent_op_id LIMIT 200"
                    );
                    let mut values = Vec::with_capacity(current.len() + 1);
                    values.push(repo_id.to_string().into());
                    values.extend(current.into_iter().map(Into::into));
                    let parent_rows = db
                        .query_all_raw(Statement::from_sql_and_values(
                            DbBackend::Sqlite,
                            sql,
                            values,
                        ))
                        .await?;
                    frontier = parent_rows
                        .into_iter()
                        .map(|row| {
                            row.try_get::<String>("", "parent_op_id")
                                .map_err(StoreError::Database)
                        })
                        .collect::<Result<BTreeSet<_>, _>>()?
                        .into_iter()
                        .collect();
                    current_depth = current_depth.saturating_add(1);
                }
                let has_more = ids.len() > offset.saturating_add(limit);
                let page = ids.into_iter().skip(offset).take(limit).collect();
                Ok((page, has_more))
            }
        }
    }

    /// Whether `op_id` is referenced as a current operation head in any scope
    /// of this repository. Used by recovery to distinguish a globally orphaned
    /// running operation (safe to fail closed) from one another worktree still
    /// treats as its current head (never touch from a foreign scope).
    pub async fn operation_is_head_in_any_scope(
        &self,
        repo_id: &str,
        op_id: &str,
    ) -> Result<bool, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let heads = store
                    .list_all_operation_heads(repo_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(heads.iter().any(|h| h == op_id))
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let row = db
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT 1 FROM operation_head WHERE repo_id = ? AND op_id = ? LIMIT 1",
                        [repo_id.to_string().into(), op_id.to_string().into()],
                    ))
                    .await?;
                Ok(row.is_some())
            }
        }
    }

    pub async fn read_head_generation(
        &self,
        repo_id: &str,
        scope_key: &str,
    ) -> Result<u64, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let snap = store
                    .get_operation_heads(repo_id, scope_key)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(snap.generation)
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                query_scope_generation(db, repo_id, scope_key).await
            }
        }
    }

    pub async fn read_heads_view(
        &self,
        repo_id: &str,
        scope_key: &str,
    ) -> Result<OpHeadsView, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let snap = store
                    .get_operation_heads(repo_id, scope_key)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                let rows = snap
                    .heads
                    .into_iter()
                    .map(|op_id| (op_id, snap.generation))
                    .collect();
                let mut view = OpHeadsView::with_generations(rows)?;
                let ops = store
                    .list_operations(repo_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                for (op, parents) in ops {
                    for parent in parents {
                        view.add_ancestor(op.op_id.clone(), parent)?;
                    }
                }
                Ok(view)
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let rows = query_head_rows(db, repo_id, scope_key).await?;
                let mut view = OpHeadsView::with_generations(rows)?;
                let parent_rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT op_id, parent_op_id FROM operation_parent ORDER BY op_id, ordinal",
                        [],
                    ))
                    .await?;
                for row in parent_rows {
                    view.add_ancestor(
                        row.try_get_by_index::<String>(0)?,
                        row.try_get_by_index::<String>(1)?,
                    )?;
                }
                Ok(view)
            }
        }
    }

    pub async fn append_journal(&self, entry: &JournalEntry) -> Result<(), StoreError> {
        if entry.journal_id.is_empty() || entry.op_id.is_empty() || entry.owner.is_empty() {
            return Err(StoreError::Validation(
                "journal id, operation id, and owner cannot be empty".to_string(),
            ));
        }
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let existing = store
                    .list_operation_journals(&entry.op_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                if let Some(prev) = existing.iter().find(|r| r.journal_id == entry.journal_id) {
                    let current_phase: JournalPhase = prev.phase.parse()?;
                    if entry.phase.rank() < current_phase.rank() {
                        return Err(StoreError::Validation(format!(
                            "journal phase cannot move backwards from {current_phase} to {}",
                            entry.phase
                        )));
                    }
                }
                store
                    .upsert_operation_journal(
                        &crate::storage::git_state_store::OperationJournalRecord {
                            journal_id: entry.journal_id.clone(),
                            op_id: entry.op_id.clone(),
                            phase: entry.phase.to_string(),
                            pre_view_oid: entry.pre_view_oid.map(|oid| oid.to_string()),
                            target_view_oid: entry.target_view_oid.map(|oid| oid.to_string()),
                            owner: entry.owner.clone(),
                            updated_at: entry.updated_at,
                            recovery_payload: entry.recovery_payload.clone(),
                        },
                    )
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(())
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let txn = begin_write_transaction(db).await?;
                let current_phase = txn
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT phase FROM operation_journal WHERE journal_id = ?",
                        [entry.journal_id.clone().into()],
                    ))
                    .await?
                    .map(|row| row.try_get_by_index::<String>(0))
                    .transpose()?
                    .map(|phase| phase.parse::<JournalPhase>())
                    .transpose()?;
                if let Some(current_phase) = current_phase
                    && entry.phase.rank() < current_phase.rank()
                {
                    let _ = txn.rollback().await;
                    return Err(StoreError::Validation(format!(
                        "journal phase cannot move backwards from {current_phase} to {}",
                        entry.phase
                    )));
                }
                if let Err(error) = txn
                    .execute_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "INSERT INTO operation_journal (journal_id, op_id, phase, pre_view_oid, \
                          target_view_oid, owner, updated_at, recovery_payload) VALUES (?, ?, ?, ?, ?, ?, ?, ?) \
                          ON CONFLICT(journal_id) DO UPDATE SET op_id = excluded.op_id, \
                          phase = excluded.phase, pre_view_oid = excluded.pre_view_oid, \
                          target_view_oid = excluded.target_view_oid, owner = excluded.owner, \
                          updated_at = excluded.updated_at, recovery_payload = excluded.recovery_payload",
                        [
                            entry.journal_id.clone().into(),
                            entry.op_id.clone().into(),
                            entry.phase.to_string().into(),
                            entry.pre_view_oid.map(|oid| oid.to_string()).into(),
                            entry.target_view_oid.map(|oid| oid.to_string()).into(),
                            entry.owner.clone().into(),
                            entry.updated_at.into(),
                            entry.recovery_payload.clone().into(),
                        ],
                    ))
                    .await
                {
                    let _ = txn.rollback().await;
                    return Err(StoreError::Database(error));
                }
                txn.commit().await?;
                Ok(())
            }
        }
    }

    pub async fn read_journal(&self, op_id: &str) -> Result<Vec<JournalEntry>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let mut rows = store
                    .list_operation_journals(op_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                rows.sort_by(|a, b| {
                    a.updated_at
                        .cmp(&b.updated_at)
                        .then_with(|| a.journal_id.cmp(&b.journal_id))
                });
                rows.into_iter().map(journal_from_record).collect()
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT journal_id, op_id, phase, pre_view_oid, target_view_oid, owner, \
                          updated_at, recovery_payload FROM operation_journal WHERE op_id = ? \
                          ORDER BY updated_at ASC, journal_id ASC",
                        [op_id.into()],
                    ))
                    .await?;
                rows.into_iter().map(journal_from_row).collect()
            }
        }
    }

    pub async fn read_all_journal(&self) -> Result<Vec<JournalEntry>, StoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let rows = store
                    .list_all_operation_journals()
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                rows.into_iter().map(journal_from_record).collect()
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT journal_id, op_id, phase, pre_view_oid, target_view_oid, owner, \
                         updated_at, recovery_payload FROM operation_journal ORDER BY updated_at, journal_id",
                        [],
                    ))
                    .await?;
                rows.into_iter().map(journal_from_row).collect()
            }
        }
    }

    /// Remove a reservation whose before/after snapshots are identical.  A
    /// no-op intentionally has no Operation row, so leaving its journal entry
    /// behind would make recovery treat a successful no-op as an unfinished
    /// mutation.
    pub async fn delete_journal(&self, journal_id: &str) -> Result<(), StoreError> {
        if journal_id.is_empty() {
            return Err(StoreError::Validation(
                "journal id cannot be empty".to_string(),
            ));
        }
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let _ = store
                    .delete_operation_journal(journal_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(())
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                db.execute_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "DELETE FROM operation_journal WHERE journal_id = ?",
                    [journal_id.to_string().into()],
                ))
                .await?;
                Ok(())
            }
        }
    }

    /// Remove a reserved operation when the before/after full views are
    /// identical.  No-op commands intentionally leave neither an operation
    /// row nor its parent/journal reservation.
    pub async fn delete_operation(&self, op_id: &str) -> Result<(), StoreError> {
        if op_id.is_empty() {
            return Err(StoreError::Validation(
                "operation id cannot be empty".to_string(),
            ));
        }
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let _ = store
                    .delete_operation(&self.repo_id, op_id)
                    .await
                    .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
                Ok(())
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let txn = begin_write_transaction(db).await?;
                txn.execute_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "DELETE FROM operation_parent WHERE op_id = ?",
                    [op_id.to_string().into()],
                ))
                .await?;
                txn.execute_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "DELETE FROM operation WHERE repo_id = ? AND op_id = ?",
                    [self.repo_id.clone().into(), op_id.to_string().into()],
                ))
                .await?;
                txn.commit().await?;
                Ok(())
            }
        }
    }
}

fn journal_from_record(
    rec: crate::storage::git_state_store::OperationJournalRecord,
) -> Result<JournalEntry, StoreError> {
    Ok(JournalEntry {
        journal_id: rec.journal_id,
        op_id: rec.op_id,
        phase: rec.phase.parse()?,
        pre_view_oid: parse_optional_hash(rec.pre_view_oid)?,
        target_view_oid: parse_optional_hash(rec.target_view_oid)?,
        owner: rec.owner,
        updated_at: rec.updated_at,
        recovery_payload: rec.recovery_payload,
    })
}

/// Backend-agnostic counterpart of [`find_recent_success_by_args_digest`].
#[allow(dead_code)]
pub(crate) async fn find_recent_success_by_args_digest_with_store(
    store: &dyn crate::storage::git_state_store::GitStateStore,
    repo_id: &str,
    worktree_id: &str,
    command_name: &str,
    args_digest: &str,
) -> Result<Option<String>, StoreError> {
    let cutoff = Utc::now().timestamp_millis() - 5_000;
    let ops = store
        .list_operations(repo_id)
        .await
        .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
    let mut best: Option<(i64, String)> = None;
    for (op, _) in ops {
        let wt_matches = op.worktree_id.as_deref() == Some(worktree_id)
            || (worktree_id.is_empty() && op.worktree_id.is_none());
        if !wt_matches
            || op.command_name.as_deref() != Some(command_name)
            || op.args_digest.as_deref() != Some(args_digest)
            || op.status != "success"
        {
            continue;
        }
        let Some(end_ts) = op.end_ts else {
            continue;
        };
        if end_ts < cutoff {
            continue;
        }
        let candidate = (end_ts, op.op_id);
        if best.as_ref().is_none_or(|prev| candidate > *prev) {
            best = Some(candidate);
        }
    }
    Ok(best.map(|(_, op_id)| op_id))
}

/// Backend-agnostic counterpart of [`update_operation_args_digest`].
#[allow(dead_code)]
pub(crate) async fn update_operation_args_digest_with_store(
    store: &dyn crate::storage::git_state_store::GitStateStore,
    repo_id: &str,
    op_id: &str,
    args_digest: &str,
) -> Result<(), StoreError> {
    let updated = store
        .update_operation_args_digest(repo_id, op_id, args_digest)
        .await
        .map_err(|err| StoreError::Database(DbErr::Custom(err.to_string())))?;
    if !updated {
        return Err(StoreError::NotFound(op_id.to_string()));
    }
    Ok(())
}

/// Return the most recent successful repository operation with the same
/// command and argument digest inside the short duplicate-operation window.
///
/// This is deliberately a v2-table query.  Callers use it for idempotency
/// checks before applying a mutation; the operation row created by the v2
/// boundary is updated with the digest immediately afterwards.
pub(crate) async fn find_recent_success_by_args_digest(
    db: &DatabaseConnection,
    repo_id: &str,
    worktree_id: &str,
    command_name: &str,
    args_digest: &str,
) -> Result<Option<String>, StoreError> {
    let cutoff = Utc::now().timestamp_millis() - 5_000;
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT op_id FROM operation \
             WHERE repo_id = ? AND (worktree_id = ? OR (? = '' AND worktree_id IS NULL)) \
               AND command_name = ? AND args_digest = ? \
               AND status = 'success' AND end_ts >= ? \
               ORDER BY end_ts DESC, op_id DESC LIMIT 1",
            [
                repo_id.to_string().into(),
                worktree_id.to_string().into(),
                worktree_id.to_string().into(),
                command_name.to_string().into(),
                args_digest.to_string().into(),
                cutoff.into(),
            ],
        ))
        .await?;
    row.map(|row| row.try_get("", "op_id").map_err(StoreError::Database))
        .transpose()
}

/// Persist an idempotency digest on an already-created v2 operation row.
pub(crate) async fn update_operation_args_digest(
    db: &DatabaseConnection,
    repo_id: &str,
    op_id: &str,
    args_digest: &str,
) -> Result<(), StoreError> {
    let updated = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE operation SET args_digest = ? WHERE repo_id = ? AND op_id = ?",
            [
                args_digest.to_string().into(),
                repo_id.to_string().into(),
                op_id.to_string().into(),
            ],
        ))
        .await?;
    if updated.rows_affected() == 0 {
        return Err(StoreError::NotFound(op_id.to_string()));
    }
    Ok(())
}

async fn query_head_rows<C: ConnectionTrait>(
    db: &C,
    repo_id: &str,
    scope_key: &str,
) -> Result<Vec<(String, u64)>, StoreError> {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT op_id, generation FROM operation_head WHERE repo_id = ? AND scope_key = ? \
              AND op_id <> ? \
              ORDER BY op_id",
            [
                repo_id.into(),
                scope_key.into(),
                HEAD_GENERATION_SENTINEL.into(),
            ],
        ))
        .await?;
    rows.into_iter()
        .map(|row| {
            Ok((
                row.try_get_by_index::<String>(0)?,
                row.try_get_by_index::<i64>(1)? as u64,
            ))
        })
        .collect()
}

async fn query_scope_generation<C: ConnectionTrait>(
    db: &C,
    repo_id: &str,
    scope_key: &str,
) -> Result<u64, StoreError> {
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT generation FROM operation_head WHERE repo_id = ? AND scope_key = ? \
              AND op_id = ?",
            [
                repo_id.into(),
                scope_key.into(),
                HEAD_GENERATION_SENTINEL.into(),
            ],
        ))
        .await?;
    if let Some(row) = row {
        return Ok(row.try_get_by_index::<i64>(0)? as u64);
    }
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT COALESCE(MAX(generation), 0) FROM operation_head \
              WHERE repo_id = ? AND scope_key = ? AND op_id <> ?",
            [
                repo_id.into(),
                scope_key.into(),
                HEAD_GENERATION_SENTINEL.into(),
            ],
        ))
        .await?;
    Ok(row
        .map(|row| row.try_get_by_index::<i64>(0))
        .transpose()?
        .unwrap_or(0) as u64)
}

async fn replace_head_rows<C: ConnectionTrait>(
    db: &C,
    repo_id: &str,
    scope_key: &str,
    generation: u64,
    replacement: &[String],
) -> Result<(), StoreError> {
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "DELETE FROM operation_head WHERE repo_id = ? AND scope_key = ? AND op_id <> ?",
        [
            repo_id.into(),
            scope_key.into(),
            HEAD_GENERATION_SENTINEL.into(),
        ],
    ))
    .await?;
    for op_id in replacement {
        db.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO operation_head (repo_id, scope_key, op_id, generation) \
              VALUES (?, ?, ?, ?)",
            [
                repo_id.into(),
                scope_key.into(),
                op_id.clone().into(),
                generation.into(),
            ],
        ))
        .await?;
    }
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO operation_head (repo_id, scope_key, op_id, generation) \
          VALUES (?, ?, ?, ?) ON CONFLICT(repo_id, scope_key, op_id) \
          DO UPDATE SET generation = excluded.generation",
        [
            repo_id.into(),
            scope_key.into(),
            HEAD_GENERATION_SENTINEL.into(),
            generation.into(),
        ],
    ))
    .await?;
    Ok(())
}

fn journal_from_row(row: QueryResult) -> Result<JournalEntry, StoreError> {
    Ok(JournalEntry {
        journal_id: row.try_get_by_index(0)?,
        op_id: row.try_get_by_index(1)?,
        phase: row.try_get_by_index::<String>(2)?.parse()?,
        pre_view_oid: parse_optional_hash(row.try_get_by_index(3)?)?,
        target_view_oid: parse_optional_hash(row.try_get_by_index(4)?)?,
        owner: row.try_get_by_index(5)?,
        updated_at: row.try_get_by_index(6)?,
        recovery_payload: row.try_get_by_index(7)?,
    })
}

fn parse_optional_hash(value: Option<String>) -> Result<Option<ObjectHash>, StoreError> {
    value
        .map(|value| {
            value
                .parse()
                .map_err(|_| StoreError::InvalidObjectHash(value))
        })
        .transpose()
}

fn validate_parent_ids(op_id: &str, parent_ids: &[String]) -> Result<(), StoreError> {
    let mut seen = BTreeSet::new();
    for parent_id in parent_ids {
        if parent_id.is_empty() {
            return Err(StoreError::Validation(
                "operation parent id cannot be empty".to_string(),
            ));
        }
        if parent_id == op_id {
            return Err(StoreError::Validation(
                "operation cannot be its own parent".to_string(),
            ));
        }
        if parent_id == HEAD_GENERATION_SENTINEL {
            return Err(StoreError::Validation(
                "reserved operation head id cannot be a parent".to_string(),
            ));
        }
        if !seen.insert(parent_id) {
            return Err(StoreError::Validation(format!(
                "duplicate operation parent id '{parent_id}'"
            )));
        }
    }
    Ok(())
}

fn normalize_heads(heads: &[String]) -> Result<Vec<String>, StoreError> {
    let mut normalized = BTreeSet::new();
    for head in heads {
        if head.is_empty() || head == HEAD_GENERATION_SENTINEL {
            return Err(StoreError::Validation(
                "operation head id cannot be empty".to_string(),
            ));
        }
        if !normalized.insert(head.clone()) {
            return Err(StoreError::Validation(format!(
                "duplicate operation head id '{head}'"
            )));
        }
    }
    Ok(normalized.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use git_internal::{errors::GitError, hash::ObjectHash, internal::object::types::ObjectType};
    use sea_orm::Database;

    use super::*;
    use crate::utils::{client_storage::ClientStorage, storage::Storage};

    struct ReadFail;

    #[async_trait::async_trait]
    impl Storage for ReadFail {
        async fn get(&self, _hash: &ObjectHash) -> Result<(Vec<u8>, ObjectType), GitError> {
            Err(GitError::IOError(std::io::Error::other(
                "simulated probe outage",
            )))
        }

        async fn put(
            &self,
            _hash: &ObjectHash,
            _data: &[u8],
            _obj_type: ObjectType,
        ) -> Result<String, GitError> {
            unreachable!("store read-failure test must not store")
        }

        async fn exist(&self, _hash: &ObjectHash) -> bool {
            false
        }

        async fn search(&self, _prefix: &str) -> Vec<ObjectHash> {
            Vec::new()
        }

        async fn exist_checked(&self, _hash: &ObjectHash) -> Result<bool, GitError> {
            Err(GitError::IOError(std::io::Error::other(
                "simulated probe outage",
            )))
        }
    }

    #[tokio::test]
    async fn store_type_read_error_stays_an_object_error() {
        let database = Database::connect("sqlite::memory:")
            .await
            .expect("memory database");
        let storage =
            ClientStorage::from_test_storage(Arc::new(ReadFail), std::path::PathBuf::new());
        let store = OperationStoreV2::new(database, storage);
        let hash = ObjectHash::new_for_kind(git_internal::hash::get_hash_kind(), &[5; 20]);
        let error = store
            .is_object_type_checked(&hash, ObjectType::Commit)
            .expect_err("a type read failure must stay an error");
        let rendered = error.to_string();
        assert!(
            matches!(error, StoreError::Object(_)),
            "unexpected error: {rendered}"
        );
        assert!(
            rendered.contains("simulated probe outage"),
            "read failure must keep the storage error: {rendered}"
        );
    }

    #[test]
    fn head_ancestry_is_transitive() {
        let mut heads = OpHeadsView::new(vec!["c".to_string()]).expect("valid head");
        heads.add_ancestor("c", "b").expect("valid edge");
        heads.add_ancestor("b", "a").expect("valid edge");
        assert!(heads.is_ancestor("a", "c"));
        assert!(!heads.is_ancestor("c", "a"));
    }

    #[test]
    fn duplicate_heads_are_rejected() {
        let result = OpHeadsView::new(vec!["same".to_string(), "same".to_string()]);
        assert!(matches!(result, Err(StoreError::Validation(_))));
    }

    #[cfg(feature = "storage-redb")]
    #[tokio::test]
    async fn redb_oplog_v2_snapshot_and_restore_roundtrip() {
        use crate::{
            internal::{
                config::ConfigKv,
                head::Head,
                operation::{
                    MutationClass, RestoreEngine, RestoreWhat, WorkspaceSnapshotter,
                    WorkspaceStatePointer, run_with_operation,
                },
                worktree_scope::{RequestScope, WorktreeScope},
            },
            storage::runtime::{
                init_redb_repository_storage, open_git_state_store_read_write,
                open_git_store_read_write, open_repo_config_store_read_write,
            },
        };

        let dir = tempfile::tempdir().expect("tempdir");
        let workdir = dir.path().to_path_buf();
        let dot_libra = workdir.join(".libra");
        let objects_dir = dot_libra.join("objects");
        std::fs::create_dir_all(&objects_dir).expect("mkdir objects");
        let _paths = init_redb_repository_storage(&dot_libra)
            .await
            .expect("init redb repo");
        assert!(!dot_libra.join("libra.db").exists());

        let repo_id = "repo-rb14-oplog";
        let config_store = open_repo_config_store_read_write(&dot_libra)
            .await
            .expect("open config store");
        ConfigKv::set_with_store(&*config_store, "libra.repoid", repo_id, false)
            .await
            .expect("set repoid");
        ConfigKv::set_with_store(&*config_store, "core.bare", "false", false)
            .await
            .expect("set core.bare");
        drop(config_store);

        let git_store = open_git_store_read_write(&dot_libra)
            .await
            .expect("open git store");
        let storage = ClientStorage::try_init_local(objects_dir).expect("object storage");
        let empty_tree_oid = ObjectHash::from_type_and_data(ObjectType::Tree, &[]);
        storage
            .put(&empty_tree_oid, &[], ObjectType::Tree)
            .expect("store empty tree");
        let commit_payload = format!(
            "tree {empty_tree_oid}\nauthor Test <t@example.com> 1700000000 +0000\ncommitter Test <t@example.com> 1700000000 +0000\n\ninit\n"
        );
        let commit_oid =
            ObjectHash::from_type_and_data(ObjectType::Commit, commit_payload.as_bytes());
        storage
            .put(&commit_oid, commit_payload.as_bytes(), ObjectType::Commit)
            .expect("store commit");
        Head::update_for_scope_result_with_store(
            &*git_store,
            Head::Branch("main".to_string()),
            None,
            &WorktreeScope::Main,
        )
        .await
        .expect("set HEAD");
        crate::internal::branch::Branch::update_branch_with_store(
            &*git_store,
            "main",
            &commit_oid.to_string(),
            None,
        )
        .await
        .expect("set main branch");
        drop(git_store);

        std::fs::write(workdir.join("hello.txt"), b"v1").expect("write v1");
        git_internal::internal::index::Index::new()
            .save(dot_libra.join("index"))
            .expect("save empty index");
        let scope = RequestScope {
            scope: WorktreeScope::Main,
            workdir: workdir.clone(),
            gitdir: dot_libra.clone(),
            storage: dot_libra.clone(),
            worktree_root: workdir.clone(),
        };

        // Run a persistent v2 operation via middleware on pure redb.
        let op1 = run_with_operation(
            &scope,
            OperationMetaV2 {
                command_name: Some("add".to_string()),
                description: Some("mutate hello.txt to v2".to_string()),
                ..Default::default()
            },
            MutationClass::WorkspaceMutation,
            |_txn| {
                let target = workdir.join("hello.txt");
                async move {
                    std::fs::write(&target, b"v2").expect("write v2");
                    Ok::<_, crate::internal::operation::OperationError>(())
                }
            },
        )
        .await
        .expect("run_with_operation");
        assert!(op1.recorded);
        let op1_id = op1.operation_id.expect("op1 id");
        assert!(!dot_libra.join("libra.db").exists());

        // Inspect OpLog v2 persisted state via OperationStoreV2 backed by RedbGitStateStore.
        let state_store = open_git_state_store_read_write(&dot_libra)
            .await
            .expect("open state store");
        let op_store = OperationStoreV2::new_for_repo_with_store(
            repo_id,
            state_store.clone(),
            storage.clone(),
        );
        let loaded = op_store
            .load_operation(&op1_id)
            .await
            .expect("load op1")
            .expect("op1 present");
        assert_eq!(loaded.status, OperationStatusV2::Success);
        let heads = op_store
            .read_heads(repo_id, scope.scope.storage_key())
            .await
            .expect("read heads");
        assert_eq!(heads, vec![op1_id.clone()]);

        // Snapshotter on redb.
        let ptr = WorkspaceStatePointer::load(&scope)
            .await
            .expect("load pointer");
        let mut snap = WorkspaceSnapshotter::new(scope.clone(), ptr);
        let snap_outcome = snap.capture().await.expect("capture snapshot");
        assert_eq!(
            snap_outcome.snapshot.completeness,
            crate::internal::operation::Completeness::Full
        );

        // Restore working copy back to pre_view_oid (v1) via RestoreEngine on redb!
        let engine = RestoreEngine::new_with_store(scope.clone(), repo_id, state_store, storage);
        let receipt = engine
            .restore(&op1_id, loaded.pre_view_oid, RestoreWhat::All, false, true)
            .await
            .expect("restore to pre-view");
        assert!(receipt.new_op_id.is_some());
        assert_eq!(
            std::fs::read_to_string(workdir.join("hello.txt")).expect("read restored"),
            "v1"
        );
        assert!(
            !dot_libra.join("libra.db").exists(),
            "pure redb oplog v2 must never create libra.db"
        );
    }
}
