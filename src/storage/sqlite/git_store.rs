//! SQLite implementation of [`GitStore`] (`RB-04`, `RB-04b`).
//!
//! Operates directly against the canonical repository SQLite tables
//! (`reference`, `reflog`, `object_index`, `object_obliteration`, `notes`,
//! `revision_ordinal`, `revision_ordinal_meta`, and `agent_bridge_publication`)
//! inside single `BEGIN IMMEDIATE` write transactions (`begin_write_transaction`).

use std::{
    collections::BTreeSet,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, EntityTrait, ModelTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
    Value,
};

use crate::{
    internal::{
        db::{
            DatabaseRole, begin_write_transaction,
            schema::{create_database_for_role, establish_connection_for_role},
            sqlite_pool_options, sqlite_schema_contains,
        },
        model::{
            reference::{self, ConfigKind},
            reflog,
        },
    },
    storage::{
        backend::StorageBackendKind,
        error::{StorageBackendError, StorageResult},
        git_store::{
            BridgePublicationPhase, BridgePublicationRecord, GitStore, NoteBatchOutcome, NoteCasOp,
            NoteRecord, NoteWriteOutcome, ObjectHashKind, ObjectIndexRecord,
            ObjectObliterationRecord, ObliterationRequest, ObliterationState, REFLOG_HEAD_REF_NAME,
            RefCasOutcome, RefKey, RefRecord, RefTarget, ReflogEntry, ReflogWrite,
            RevisionOrdinalAppendOutcome, RevisionOrdinalMeta, is_valid_bridge_operation_id,
            obliteration_fenced_read_error, obliteration_fenced_write_error,
            validate_bridge_publication_binding, validate_hex_oid, validate_non_empty_identifier,
            validate_note_cas_op, validate_notes_ref, validate_object_index_record,
            validate_obliteration_request, validate_oid_for_hash_kind, validate_ref_key,
            validate_ref_target, validate_reflog_writes_for_key, validate_revision_ordinal_append,
            validate_revision_ordinal_entries, validate_worktree_filter,
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

/// SQLite-backed implementation of [`GitStore`].
#[derive(Clone)]
pub struct SqliteGitStore {
    conn: DatabaseConnection,
    read_only: bool,
}

impl std::fmt::Debug for SqliteGitStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteGitStore")
            .field("read_only", &self.read_only)
            .finish_non_exhaustive()
    }
}

async fn ensure_bridge_publication_table(conn: &DatabaseConnection) -> StorageResult<()> {
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
    Ok(())
}

impl SqliteGitStore {
    /// Create a brand-new repository SQLite database at `path` and initialize
    /// all repository schema tables (including `agent_bridge_publication`).
    pub async fn create(path: impl AsRef<Path>) -> StorageResult<Self> {
        let path_str =
            path.as_ref()
                .to_str()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "sqlite repository database path must be valid UTF-8".to_string(),
                })?;
        let conn = create_database_for_role(path_str, DatabaseRole::Repository)
            .await
            .map_err(|err| StorageBackendError::io("create sqlite git store", err))?;
        ensure_bridge_publication_table(&conn).await?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                path.as_ref(),
                std::fs::Permissions::from_mode(
                    crate::storage::config_store::REPOSITORY_CONFIG_FILE_MODE,
                ),
            )
            .map_err(|err| StorageBackendError::io("chmod sqlite git store to 0600", err))?;
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
            .map_err(|err| StorageBackendError::io("open sqlite git store", err))?;
        ensure_bridge_publication_table(&conn).await?;
        Ok(Self {
            conn,
            read_only: false,
        })
    }

    /// Open an existing repository SQLite database strictly read-only without
    /// creating files or running DDL migrations.
    pub async fn open_read_only(path: impl AsRef<Path>) -> StorageResult<Self> {
        let filename = std::path::absolute(path.as_ref())
            .map_err(|err| StorageBackendError::io("resolve sqlite git read-only path", err))?;
        if filename.to_str().is_none() {
            return Err(StorageBackendError::BackendInternal {
                detail: "sqlite repository database path must be valid UTF-8".to_string(),
            });
        }
        let mut options = ConnectOptions::new("sqlite://storage-git-readonly");
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
                "open sqlite git store read-only",
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
                detail: "cannot mutate a read-only git store".to_string(),
            });
        }
        Ok(())
    }

    async fn update_ref_cas_inner(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: Option<&BridgePublicationRecord>,
    ) -> StorageResult<RefCasOutcome> {
        self.ensure_writable()?;
        validate_ref_key(key)?;
        if let Some(exp) = expected {
            validate_ref_target(key, exp)?;
        }
        if let Some(new_t) = new_target {
            validate_ref_target(key, new_t)?;
        }
        validate_reflog_writes_for_key(key, reflogs)?;
        if let Some(pub_rec) = publication {
            validate_bridge_publication_binding(key, expected, new_target, pub_rec)?;
            ensure_bridge_publication_table(&self.conn).await?;
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin git ref cas transaction",
                err,
            )
        })?;

        let existing_model = find_ref_model_in_conn(&txn, key).await?;
        let actual_target = match &existing_model {
            Some(model) => Some(decode_target_from_model(key, model)?),
            None => None,
        };

        if actual_target.as_ref() != expected {
            drop(txn);
            return Ok(RefCasOutcome::CasConflict {
                actual: actual_target,
            });
        }

        // Resolve symbolic `HEAD` when binding a bridge publication so switching
        // `Symbolic(branch -> X)` to `Direct(X)` cannot publish a record without
        // actually moving `HEAD` to a new commit OID.
        if let (Some(pub_rec), Some(RefTarget::Symbolic(sym_branch))) =
            (publication, &actual_target)
        {
            let branch_key = RefKey::LocalBranch {
                name: sym_branch.clone(),
            };
            if let Some(branch_model) = find_ref_model_in_conn(&txn, &branch_key).await?
                && let RefTarget::Direct(resolved_oid) =
                    decode_target_from_model(&branch_key, &branch_model)?
                && resolved_oid == pub_rec.commit_oid
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "bridge publication requires HEAD to move to a new commit OID"
                        .to_string(),
                });
            }
        }

        // When updating a `LocalBranch`, verify any accompanying `"HEAD"` reflog
        // entries belong to a worktree whose `LocalHead` points symbolically to
        // this branch, and when binding a bridge publication verify the branch
        // is actually the live symbolic target of a worktree's `LocalHead`.
        if let RefKey::LocalBranch { name: branch_name } = key {
            for entry in reflogs {
                if entry.ref_name == REFLOG_HEAD_REF_NAME {
                    let head_key = RefKey::LocalHead {
                        worktree_id: entry.worktree_id.clone(),
                    };
                    let head_model = find_ref_model_in_conn(&txn, &head_key).await?;
                    let head_target = match &head_model {
                        Some(m) => Some(decode_target_from_model(&head_key, m)?),
                        None => None,
                    };
                    if head_target.as_ref() != Some(&RefTarget::Symbolic(branch_name.clone())) {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "HEAD reflog worktree_id does not point symbolically to the LocalBranch being updated"
                                .to_string(),
                        });
                    }
                }
            }
            if publication.is_some() {
                let symbolic_holders = reference::Entity::find()
                    .filter(reference::Column::Kind.eq(ConfigKind::Head))
                    .filter(reference::Column::Remote.is_null())
                    .filter(reference::Column::Name.eq(branch_name.as_str()))
                    .all(&txn)
                    .await
                    .map_err(|err| {
                        map_sqlite_db_err(
                            DatabaseRole::Repository,
                            "verify symbolic HEAD for bridge publication",
                            err,
                        )
                    })?;
                if symbolic_holders.is_empty() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "bridge publication requires the updated LocalBranch to be checked out by HEAD"
                            .to_string(),
                    });
                }
            }
        }

        // Enforce worktree symbolic HEAD exclusivity consistently with SQLite's
        // `idx_name_kind` and `RedbGitStore`: no two worktrees may hold symbolic
        // `HEAD` pointing to the same branch simultaneously.
        if let (
            RefKey::LocalHead {
                worktree_id: target_wt,
            },
            Some(RefTarget::Symbolic(branch_name)),
        ) = (key, new_target)
        {
            let conflicting = reference::Entity::find()
                .filter(reference::Column::Kind.eq(ConfigKind::Head))
                .filter(reference::Column::Remote.is_null())
                .filter(reference::Column::Name.eq(branch_name.as_str()))
                .all(&txn)
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "check duplicate symbolic HEAD across worktrees",
                        err,
                    )
                })?;
            if conflicting
                .iter()
                .any(|row| row.worktree_id.as_deref() != target_wt.as_deref())
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "branch is already checked out as symbolic HEAD in another worktree"
                        .to_string(),
                });
            }
        }

        // Apply ref mutation.
        match (existing_model, new_target) {
            (Some(model), Some(target)) => {
                let (name_col, commit_col) = encode_columns_for_update(key, target);
                let mut active: reference::ActiveModel = model.into();
                active.name = Set(name_col);
                active.commit = Set(commit_col);
                active.update(&txn).await.map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "update reference row", err)
                })?;
            }
            (None, Some(target)) => {
                let active = build_insert_model(key, target);
                active.insert(&txn).await.map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "insert reference row", err)
                })?;
            }
            (Some(model), None) => {
                model.delete(&txn).await.map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "delete reference row", err)
                })?;
            }
            (None, None) => {}
        }

        // Append reflog entries atomically in the same transaction.
        for entry in reflogs {
            let active = reflog::ActiveModel {
                ref_name: Set(entry.ref_name.clone()),
                old_oid: Set(entry.old_oid.clone()),
                new_oid: Set(entry.new_oid.clone()),
                timestamp: Set(entry.timestamp),
                committer_name: Set(entry.committer_name.clone()),
                committer_email: Set(entry.committer_email.clone()),
                action: Set(entry.action.clone()),
                message: Set(entry.message.clone()),
                worktree_id: Set(entry.worktree_id.clone()),
                ..Default::default()
            };
            active.insert(&txn).await.map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "insert reflog row", err)
            })?;
        }

        // Bind `agent_bridge_publication` atomically in the same transaction.
        if let Some(pub_rec) = publication {
            let backend = txn.get_database_backend();
            let existing_pub =
                query_bridge_publication_in_conn(&txn, &pub_rec.operation_id).await?;
            match existing_pub {
                Some(prev) if prev.commit_oid == pub_rec.commit_oid => {
                    txn.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "UPDATE agent_bridge_publication SET phase = ? WHERE operation_id = ? AND commit_oid = ?",
                        [
                            pub_rec.phase.as_str().into(),
                            pub_rec.operation_id.clone().into(),
                            pub_rec.commit_oid.clone().into(),
                        ],
                    ))
                    .await
                    .map_err(|err| {
                        map_sqlite_db_err(
                            DatabaseRole::Repository,
                            "update agent_bridge_publication row",
                            err,
                        )
                    })?;
                }
                Some(_) => {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "bridge operation is already bound to a different commit; refusing to publish"
                            .to_string(),
                    });
                }
                None => {
                    txn.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "INSERT INTO agent_bridge_publication (operation_id, commit_oid, phase) VALUES (?, ?, ?)",
                        [
                            pub_rec.operation_id.clone().into(),
                            pub_rec.commit_oid.clone().into(),
                            pub_rec.phase.as_str().into(),
                        ],
                    ))
                    .await
                    .map_err(|err| {
                        map_sqlite_db_err(
                            DatabaseRole::Repository,
                            "insert agent_bridge_publication row",
                            err,
                        )
                    })?;
                }
            }
        }

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit git ref cas transaction",
                err,
            )
        })?;

        Ok(RefCasOutcome::Applied {
            previous: actual_target,
            current: new_target.cloned(),
        })
    }
}

async fn find_ref_model_in_conn<C: ConnectionTrait>(
    conn: &C,
    key: &RefKey,
) -> StorageResult<Option<reference::Model>> {
    let query = match key {
        RefKey::LocalHead { worktree_id } => {
            let q = reference::Entity::find()
                .filter(reference::Column::Kind.eq(ConfigKind::Head))
                .filter(reference::Column::Remote.is_null());
            match worktree_id {
                Some(wt) => q.filter(reference::Column::WorktreeId.eq(wt.as_str())),
                None => q.filter(reference::Column::WorktreeId.is_null()),
            }
        }
        RefKey::RemoteHead { remote } => reference::Entity::find()
            .filter(reference::Column::Kind.eq(ConfigKind::Head))
            .filter(reference::Column::Remote.eq(remote.as_str()))
            .filter(reference::Column::WorktreeId.is_null()),
        RefKey::LocalBranch { name } => reference::Entity::find()
            .filter(reference::Column::Kind.eq(ConfigKind::Branch))
            .filter(reference::Column::Name.eq(name.as_str()))
            .filter(reference::Column::Remote.is_null())
            .filter(reference::Column::WorktreeId.is_null()),
        RefKey::RemoteBranch { remote, name } => reference::Entity::find()
            .filter(reference::Column::Kind.eq(ConfigKind::Branch))
            .filter(reference::Column::Name.eq(name.as_str()))
            .filter(reference::Column::Remote.eq(remote.as_str()))
            .filter(reference::Column::WorktreeId.is_null()),
        RefKey::Tag { name } => reference::Entity::find()
            .filter(reference::Column::Kind.eq(ConfigKind::Tag))
            .filter(reference::Column::Name.eq(name.as_str()))
            .filter(reference::Column::Remote.is_null())
            .filter(reference::Column::WorktreeId.is_null()),
    };
    let rows = query
        .all(conn)
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "query reference row", err))?;
    if rows.len() > 1 {
        return Err(StorageBackendError::BackendInternal {
            detail: "ambiguous duplicate reference rows found for single RefKey".to_string(),
        });
    }
    Ok(rows.into_iter().next())
}

fn decode_target_from_model(key: &RefKey, model: &reference::Model) -> StorageResult<RefTarget> {
    match key {
        RefKey::LocalHead { .. } | RefKey::RemoteHead { .. } => {
            if let Some(name) = &model.name {
                if name.is_empty() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "corrupt symbolic HEAD with empty branch name".to_string(),
                    });
                }
                Ok(RefTarget::Symbolic(name.clone()))
            } else if let Some(commit) = &model.commit {
                if commit.is_empty() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "corrupt detached HEAD with empty commit OID".to_string(),
                    });
                }
                Ok(RefTarget::Direct(commit.clone()))
            } else {
                Err(StorageBackendError::BackendInternal {
                    detail: "corrupt HEAD row missing both symbolic branch and commit OID"
                        .to_string(),
                })
            }
        }
        RefKey::LocalBranch { .. } | RefKey::RemoteBranch { .. } | RefKey::Tag { .. } => {
            let commit =
                model
                    .commit
                    .as_ref()
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "corrupt branch/tag row missing commit OID".to_string(),
                    })?;
            if commit.is_empty() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt branch/tag row with empty commit OID".to_string(),
                });
            }
            Ok(RefTarget::Direct(commit.clone()))
        }
    }
}

fn decode_record_from_model(model: reference::Model) -> StorageResult<RefRecord> {
    let key = match model.kind {
        ConfigKind::Head => match &model.remote {
            None => RefKey::LocalHead {
                worktree_id: model.worktree_id.clone(),
            },
            Some(remote) => {
                if model.worktree_id.is_some() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "corrupt remote HEAD row carrying non-NULL worktree_id".to_string(),
                    });
                }
                RefKey::RemoteHead {
                    remote: remote.clone(),
                }
            }
        },
        ConfigKind::Branch => {
            if model.worktree_id.is_some() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt branch row carrying non-NULL worktree_id".to_string(),
                });
            }
            let name = model
                .name
                .clone()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "corrupt branch row missing name".to_string(),
                })?;
            match &model.remote {
                None => RefKey::LocalBranch { name },
                Some(remote) => RefKey::RemoteBranch {
                    remote: remote.clone(),
                    name,
                },
            }
        }
        ConfigKind::Tag => {
            if model.remote.is_some() || model.worktree_id.is_some() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "corrupt tag row carrying remote or worktree_id".to_string(),
                });
            }
            let name = model
                .name
                .clone()
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "corrupt tag row missing name".to_string(),
                })?;
            RefKey::Tag { name }
        }
    };
    let target = decode_target_from_model(&key, &model)?;
    Ok(RefRecord { key, target })
}

fn encode_columns_for_update(key: &RefKey, target: &RefTarget) -> (Option<String>, Option<String>) {
    match key {
        RefKey::LocalHead { .. } | RefKey::RemoteHead { .. } => match target {
            RefTarget::Symbolic(branch) => (Some(branch.clone()), None),
            RefTarget::Direct(oid) => (None, Some(oid.clone())),
        },
        RefKey::LocalBranch { name } | RefKey::RemoteBranch { name, .. } | RefKey::Tag { name } => {
            match target {
                RefTarget::Direct(oid) => (Some(name.clone()), Some(oid.clone())),
                RefTarget::Symbolic(oid) => (Some(name.clone()), Some(oid.clone())),
            }
        }
    }
}

fn build_insert_model(key: &RefKey, target: &RefTarget) -> reference::ActiveModel {
    let (name_col, commit_col) = encode_columns_for_update(key, target);
    let (kind, remote, worktree_id) = match key {
        RefKey::LocalHead { worktree_id } => (ConfigKind::Head, None, worktree_id.clone()),
        RefKey::RemoteHead { remote } => (ConfigKind::Head, Some(remote.clone()), None),
        RefKey::LocalBranch { .. } => (ConfigKind::Branch, None, None),
        RefKey::RemoteBranch { remote, .. } => (ConfigKind::Branch, Some(remote.clone()), None),
        RefKey::Tag { .. } => (ConfigKind::Tag, None, None),
    };
    reference::ActiveModel {
        name: Set(name_col),
        kind: Set(kind),
        commit: Set(commit_col),
        remote: Set(remote),
        worktree_id: Set(worktree_id),
        ..Default::default()
    }
}

async fn query_bridge_publication_in_conn<C: ConnectionTrait>(
    conn: &C,
    operation_id: &str,
) -> StorageResult<Option<BridgePublicationRecord>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT operation_id, commit_oid, phase FROM agent_bridge_publication WHERE operation_id = ?",
            [operation_id.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "query agent_bridge_publication row",
                err,
            )
        })?;
    let Some(row) = row else {
        return Ok(None);
    };
    let op_id: String = row.try_get_by_index(0).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode agent_bridge_publication operation_id",
            err,
        )
    })?;
    let commit_oid: String = row.try_get_by_index(1).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode agent_bridge_publication commit_oid",
            err,
        )
    })?;
    let phase_str: String = row.try_get_by_index(2).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode agent_bridge_publication phase",
            err,
        )
    })?;
    Ok(Some(BridgePublicationRecord {
        operation_id: op_id,
        commit_oid,
        phase: BridgePublicationPhase::parse(&phase_str)?,
    }))
}

fn is_unborn_reserved_branch_model(model: &reference::Model) -> bool {
    model.kind == ConfigKind::Branch
        && model.commit.is_none()
        && model.remote.is_none()
        && model.worktree_id.is_none()
        && matches!(
            model.name.as_deref(),
            Some("intent" | "traces" | "agent-traces")
        )
}

#[async_trait]
impl GitStore for SqliteGitStore {
    fn backend(&self) -> StorageBackendKind {
        StorageBackendKind::Sqlite
    }

    async fn get_ref(&self, key: &RefKey) -> StorageResult<Option<RefTarget>> {
        validate_ref_key(key)?;
        let Some(model) = find_ref_model_in_conn(&self.conn, key).await? else {
            return Ok(None);
        };
        if is_unborn_reserved_branch_model(&model) {
            return Ok(None);
        }
        decode_target_from_model(key, &model).map(Some)
    }

    async fn list_refs(&self, worktree_id: Option<&str>) -> StorageResult<Vec<RefRecord>> {
        validate_worktree_filter(worktree_id)?;
        let rows = reference::Entity::find()
            .order_by_asc(reference::Column::Id)
            .all(&self.conn)
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list reference rows", err)
            })?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            if is_unborn_reserved_branch_model(&row) {
                continue;
            }
            let rec = decode_record_from_model(row)?;
            if rec.key.visible_to_worktree(worktree_id) {
                records.push(rec);
            }
        }
        records.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(records)
    }

    async fn list_all_local_heads(&self) -> StorageResult<Vec<RefRecord>> {
        let rows = reference::Entity::find()
            .filter(reference::Column::Kind.eq(ConfigKind::Head))
            .filter(reference::Column::Remote.is_null())
            .order_by_asc(reference::Column::Id)
            .all(&self.conn)
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list local HEAD rows", err)
            })?;
        let mut records = Vec::with_capacity(rows.len());
        for row in rows {
            records.push(decode_record_from_model(row)?);
        }
        records.sort_by(|a, b| a.key.cmp(&b.key));
        Ok(records)
    }

    async fn update_ref_cas(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
    ) -> StorageResult<RefCasOutcome> {
        self.update_ref_cas_inner(key, expected, new_target, reflogs, None)
            .await
    }

    async fn update_ref_cas_with_bridge_publication(
        &self,
        key: &RefKey,
        expected: Option<&RefTarget>,
        new_target: Option<&RefTarget>,
        reflogs: &[ReflogWrite],
        publication: &BridgePublicationRecord,
    ) -> StorageResult<RefCasOutcome> {
        self.update_ref_cas_inner(key, expected, new_target, reflogs, Some(publication))
            .await
    }

    async fn list_reflog(
        &self,
        ref_name: &str,
        worktree_id: Option<&str>,
        limit: Option<usize>,
    ) -> StorageResult<Vec<ReflogEntry>> {
        if ref_name.is_empty() || ref_name.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "reflog ref_name must be non-empty".to_string(),
            });
        }
        validate_worktree_filter(worktree_id)?;
        if ref_name != REFLOG_HEAD_REF_NAME && worktree_id.is_some() {
            return Err(StorageBackendError::BackendInternal {
                detail: "non-HEAD reflog queries must not specify a worktree_id".to_string(),
            });
        }

        if limit == Some(0) {
            return Ok(Vec::new());
        }

        let mut query = reflog::Entity::find().filter(reflog::Column::RefName.eq(ref_name));
        query = match worktree_id {
            Some(wt) => query.filter(reflog::Column::WorktreeId.eq(wt)),
            None => query.filter(reflog::Column::WorktreeId.is_null()),
        };
        query = query.order_by_desc(reflog::Column::Id);
        if let Some(max) = limit {
            query = query.limit(u64::try_from(max).unwrap_or(u64::MAX));
        }
        let rows = query
            .all(&self.conn)
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "list reflog rows", err))?;

        Ok(rows
            .into_iter()
            .map(|row| ReflogEntry {
                id: row.id,
                ref_name: row.ref_name,
                old_oid: row.old_oid,
                new_oid: row.new_oid,
                timestamp: row.timestamp,
                committer_name: row.committer_name,
                committer_email: row.committer_email,
                action: row.action,
                message: row.message,
                worktree_id: row.worktree_id,
            })
            .collect())
    }

    async fn delete_reflog_entries(
        &self,
        ref_name: &str,
        worktree_id: Option<&str>,
        ids: &[i64],
    ) -> StorageResult<usize> {
        if ref_name.is_empty() || ref_name.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "reflog ref_name must be non-empty".to_string(),
            });
        }
        validate_worktree_filter(worktree_id)?;
        if ref_name != REFLOG_HEAD_REF_NAME && worktree_id.is_some() {
            return Err(StorageBackendError::BackendInternal {
                detail: "non-HEAD reflog mutations must not specify a worktree_id".to_string(),
            });
        }
        if ids.is_empty() {
            return Ok(0);
        }
        let mut del = reflog::Entity::delete_many()
            .filter(reflog::Column::RefName.eq(ref_name))
            .filter(reflog::Column::Id.is_in(ids.iter().copied()));
        del = match worktree_id {
            Some(wt) => del.filter(reflog::Column::WorktreeId.eq(wt)),
            None => del.filter(reflog::Column::WorktreeId.is_null()),
        };
        let res = del.exec(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "delete reflog rows", err)
        })?;
        Ok(usize::try_from(res.rows_affected).unwrap_or(usize::MAX))
    }

    async fn list_all_reflogs(&self) -> StorageResult<Vec<ReflogEntry>> {
        let rows = reflog::Entity::find()
            .order_by_desc(reflog::Column::Id)
            .all(&self.conn)
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list all reflog rows", err)
            })?;
        Ok(rows
            .into_iter()
            .map(|row| ReflogEntry {
                id: row.id,
                ref_name: row.ref_name,
                old_oid: row.old_oid,
                new_oid: row.new_oid,
                timestamp: row.timestamp,
                committer_name: row.committer_name,
                committer_email: row.committer_email,
                action: row.action,
                message: row.message,
                worktree_id: row.worktree_id,
            })
            .collect())
    }

    async fn prepare_bridge_publication(
        &self,
        operation_id: &str,
        commit_oid: &str,
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        if !is_valid_bridge_operation_id(operation_id) {
            return Err(StorageBackendError::BackendInternal {
                detail: "bridge publication operation_id is empty or contains invalid characters"
                    .to_string(),
            });
        }
        if commit_oid.is_empty() || commit_oid.contains('\0') {
            return Err(StorageBackendError::BackendInternal {
                detail: "bridge publication commit_oid must be non-empty".to_string(),
            });
        }
        ensure_bridge_publication_table(&self.conn).await?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin prepare bridge publication transaction",
                err,
            )
        })?;
        let existing = query_bridge_publication_in_conn(&txn, operation_id).await?;
        match existing {
            Some(prev) if prev.commit_oid == commit_oid => {
                // Idempotent: keep `published` if already published, or `prepared`.
                drop(txn);
                return Ok(());
            }
            Some(_) => {
                return Err(StorageBackendError::BackendInternal {
                    detail: "bridge operation is already bound to a different commit".to_string(),
                });
            }
            None => {
                let backend = txn.get_database_backend();
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO agent_bridge_publication (operation_id, commit_oid, phase) VALUES (?, ?, 'prepared')",
                    [operation_id.into(), commit_oid.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "insert prepared agent_bridge_publication row",
                        err,
                    )
                })?;
            }
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit prepare bridge publication transaction",
                err,
            )
        })?;
        Ok(())
    }

    async fn get_bridge_publication(
        &self,
        operation_id: &str,
    ) -> StorageResult<Option<BridgePublicationRecord>> {
        if operation_id.is_empty() || operation_id.contains('\0') {
            return Ok(None);
        }
        if self.read_only
            && !sqlite_schema_contains(&self.conn, "table", "agent_bridge_publication")
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "check agent_bridge_publication table existence",
                        err,
                    )
                })?
        {
            return Ok(None);
        }
        query_bridge_publication_in_conn(&self.conn, operation_id).await
    }

    async fn list_bridge_publications(&self) -> StorageResult<Vec<BridgePublicationRecord>> {
        if self.read_only
            && !sqlite_schema_contains(&self.conn, "table", "agent_bridge_publication")
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "check agent_bridge_publication table existence",
                        err,
                    )
                })?
        {
            return Ok(Vec::new());
        }
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT operation_id, commit_oid, phase FROM agent_bridge_publication ORDER BY operation_id ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list agent_bridge_publication rows",
                    err,
                )
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let op_id: String = row.try_get_by_index(0).map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "decode bridge operation_id", err)
            })?;
            let commit_oid: String = row.try_get_by_index(1).map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "decode bridge commit_oid", err)
            })?;
            let phase_str: String = row.try_get_by_index(2).map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "decode bridge phase", err)
            })?;
            out.push(BridgePublicationRecord {
                operation_id: op_id,
                commit_oid,
                phase: BridgePublicationPhase::parse(&phase_str)?,
            });
        }
        Ok(out)
    }

    async fn upsert_object_index_batch(&self, records: &[ObjectIndexRecord]) -> StorageResult<()> {
        self.ensure_writable()?;
        for rec in records {
            validate_object_index_record(rec)?;
        }
        if records.is_empty() {
            return Ok(());
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin upsert_object_index_batch transaction",
                err,
            )
        })?;

        // Enforce the `object_obliteration` write fence across the entire batch
        // inside the same write transaction before mutating any row.
        let distinct_oids: BTreeSet<&str> = records.iter().map(|r| r.o_id.as_str()).collect();
        check_oids_not_obliterated_for_write_in_conn(&txn, &distinct_oids).await?;

        const UPSERT_CHUNK: usize = 150;
        let backend = txn.get_database_backend();
        for chunk in records.chunks(UPSERT_CHUNK) {
            let mut sql = String::from(
                "INSERT INTO object_index \
                 (o_id, o_type, o_size, repo_id, created_at, is_synced) VALUES ",
            );
            let mut values = Vec::with_capacity(chunk.len() * 6);
            for (idx, rec) in chunk.iter().enumerate() {
                if idx != 0 {
                    sql.push_str(", ");
                }
                sql.push_str("(?, ?, ?, ?, ?, ?)");
                values.extend([
                    Value::from(rec.o_id.clone()),
                    Value::from(rec.o_type.clone()),
                    Value::from(rec.o_size),
                    Value::from(rec.repo_id.clone()),
                    Value::from(rec.created_at),
                    Value::from(i32::from(rec.is_synced)),
                ]);
            }
            sql.push_str(
                " ON CONFLICT(repo_id, o_id) DO UPDATE SET \
                 o_type = CASE \
                   WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
                    AND substr(object_index.o_type, 1, 6) != 'agent_' \
                   THEN excluded.o_type ELSE object_index.o_type END, \
                 o_size = CASE \
                   WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
                    AND substr(object_index.o_type, 1, 6) != 'agent_' \
                   THEN excluded.o_size ELSE object_index.o_size END, \
                 is_synced = CASE \
                   WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
                    AND substr(object_index.o_type, 1, 6) != 'agent_' \
                   THEN 0 ELSE object_index.is_synced END",
            );
            txn.execute_raw(Statement::from_sql_and_values(backend, sql, values))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "upsert object_index batch chunk",
                        err,
                    )
                })?;
        }

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit upsert_object_index_batch transaction",
                err,
            )
        })?;
        Ok(())
    }

    async fn get_object_index(
        &self,
        repo_id: &str,
        o_id: &str,
    ) -> StorageResult<Option<ObjectIndexRecord>> {
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        validate_hex_oid(o_id, "object_index o_id")?;
        let backend = self.conn.get_database_backend();
        // Single SQL statement guarantees atomic MVCC snapshot visibility across
        // both `object_obliteration` and `object_index` on read-only & read-write connections.
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT \
                   (SELECT state FROM object_obliteration WHERE oid = ? LIMIT 1) AS fence_state, \
                   oi.repo_id, oi.o_id, oi.o_type, oi.o_size, oi.created_at, oi.is_synced \
                 FROM (SELECT 1) AS _probe \
                 LEFT JOIN object_index oi ON oi.repo_id = ? AND oi.o_id = ?",
                [o_id.into(), repo_id.into(), o_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "get fenced object_index row", err)
            })?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "fenced object_index probe query returned zero rows".to_string(),
            })?;

        let fence_state: Option<String> = row.try_get_by_index(0).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode object_obliteration fence_state",
                err,
            )
        })?;
        if let Some(state_str) = fence_state {
            let state = ObliterationState::parse(&state_str)?;
            return Err(obliteration_fenced_read_error(state));
        }

        let found_repo: Option<String> = row.try_get_by_index(1).map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "decode object_index repo_id", err)
        })?;
        let Some(found_repo) = found_repo else {
            return Ok(None);
        };
        let found_oid: String = row.try_get_by_index(2).map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "decode object_index o_id", err)
        })?;
        let o_type: String = row.try_get_by_index(3).map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "decode object_index o_type", err)
        })?;
        let o_size: i64 = row.try_get_by_index(4).map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "decode object_index o_size", err)
        })?;
        let created_at: i64 = row.try_get_by_index(5).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode object_index created_at",
                err,
            )
        })?;
        let is_synced: i32 = row.try_get_by_index(6).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode object_index is_synced",
                err,
            )
        })?;
        Ok(Some(ObjectIndexRecord {
            repo_id: found_repo,
            o_id: found_oid,
            o_type,
            o_size,
            created_at,
            is_synced: is_synced != 0,
        }))
    }

    async fn get_object_index_preserved_address(
        &self,
        repo_id: &str,
        o_id: &str,
    ) -> StorageResult<Option<ObjectIndexRecord>> {
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        validate_hex_oid(o_id, "object_index o_id")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT repo_id, o_id, o_type, o_size, created_at, is_synced \
                 FROM object_index WHERE repo_id = ? AND o_id = ?",
                [repo_id.into(), o_id.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "get preserved object_index address",
                    err,
                )
            })?;
        let Some(row) = row else {
            return Ok(None);
        };
        decode_object_index_row(&row).map(Some)
    }

    async fn list_object_index_by_repo(
        &self,
        repo_id: &str,
        synced_filter: Option<bool>,
    ) -> StorageResult<Vec<ObjectIndexRecord>> {
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        let backend = self.conn.get_database_backend();
        let stmt = match synced_filter {
            Some(synced) => Statement::from_sql_and_values(
                backend,
                "SELECT oi.repo_id, oi.o_id, oi.o_type, oi.o_size, oi.created_at, oi.is_synced \
                 FROM object_index oi \
                 WHERE oi.repo_id = ? AND oi.is_synced = ? \
                   AND NOT EXISTS (SELECT 1 FROM object_obliteration ob WHERE ob.oid = oi.o_id) \
                 ORDER BY oi.o_id ASC",
                [repo_id.into(), i32::from(synced).into()],
            ),
            None => Statement::from_sql_and_values(
                backend,
                "SELECT oi.repo_id, oi.o_id, oi.o_type, oi.o_size, oi.created_at, oi.is_synced \
                 FROM object_index oi \
                 WHERE oi.repo_id = ? \
                   AND NOT EXISTS (SELECT 1 FROM object_obliteration ob WHERE ob.oid = oi.o_id) \
                 ORDER BY oi.o_id ASC",
                [repo_id.into()],
            ),
        };
        let rows = self.conn.query_all_raw(stmt).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "list object_index rows", err)
        })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(decode_object_index_row(&row)?);
        }
        Ok(out)
    }

    async fn mark_object_index_synced(
        &self,
        repo_id: &str,
        o_ids: &[String],
    ) -> StorageResult<usize> {
        self.ensure_writable()?;
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        for oid in o_ids {
            validate_hex_oid(oid, "object_index o_id")?;
        }
        if o_ids.is_empty() {
            return Ok(0);
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin mark_object_index_synced transaction",
                err,
            )
        })?;
        let distinct_oids: BTreeSet<&str> = o_ids.iter().map(String::as_str).collect();
        check_oids_not_obliterated_for_write_in_conn(&txn, &distinct_oids).await?;

        let backend = txn.get_database_backend();
        let mut updated = 0usize;
        let ordered: Vec<&str> = distinct_oids.into_iter().collect();
        for chunk in ordered.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql = format!(
                "UPDATE object_index SET is_synced = 1 \
                 WHERE repo_id = ? AND is_synced = 0 AND o_id IN ({placeholders})"
            );
            let mut values = Vec::with_capacity(chunk.len() + 1);
            values.push(Value::from(repo_id.to_string()));
            for oid in chunk {
                values.push(Value::from((*oid).to_string()));
            }
            let res = txn
                .execute_raw(Statement::from_sql_and_values(backend, sql, values))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "mark object_index synced", err)
                })?;
            updated += usize::try_from(res.rows_affected()).unwrap_or(usize::MAX);
        }

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit mark_object_index_synced transaction",
                err,
            )
        })?;
        Ok(updated)
    }

    async fn delete_object_index_rows(
        &self,
        repo_id: &str,
        o_ids: &[String],
    ) -> StorageResult<usize> {
        self.ensure_writable()?;
        validate_non_empty_identifier(repo_id, "object_index repo_id")?;
        if o_ids.is_empty() {
            return Ok(0);
        }
        let backend = self.conn.get_database_backend();
        let mut deleted = 0usize;
        for chunk in o_ids.chunks(400) {
            let placeholders = vec!["?"; chunk.len()].join(", ");
            let sql =
                format!("DELETE FROM object_index WHERE repo_id = ? AND o_id IN ({placeholders})");
            let mut values = Vec::with_capacity(chunk.len() + 1);
            values.push(Value::from(repo_id.to_string()));
            for oid in chunk {
                values.push(Value::from(oid.clone()));
            }
            let res = self
                .conn
                .execute_raw(Statement::from_sql_and_values(backend, sql, values))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "delete object_index rows", err)
                })?;
            deleted += usize::try_from(res.rows_affected()).unwrap_or(usize::MAX);
        }
        Ok(deleted)
    }

    async fn begin_object_obliteration(
        &self,
        req: &ObliterationRequest,
    ) -> StorageResult<ObjectObliterationRecord> {
        self.ensure_writable()?;
        validate_obliteration_request(req)?;
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin object_obliteration transaction",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT OR IGNORE INTO object_obliteration \
             (oid, hash_kind, state, reason, actor, approval_source, requested_at, tombstone_written_at, payload_deleted_at, updated_at) \
             VALUES (?, ?, 'obliterating', ?, ?, ?, ?, ?, NULL, ?)",
            [
                req.oid.clone().into(),
                req.hash_kind.as_str().into(),
                req.reason.clone().into(),
                req.actor.clone().into(),
                req.approval_source.clone().into(),
                req.recorded_at.clone().into(),
                req.recorded_at.clone().into(),
                req.recorded_at.clone().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "insert object_obliteration tombstone",
                err,
            )
        })?;

        let record = query_obliteration_by_oid_and_kind(&txn, &req.oid, req.hash_kind)
            .await?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "object_obliteration tombstone was not visible after insert".to_string(),
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit object_obliteration transaction",
                err,
            )
        })?;
        Ok(record)
    }

    async fn mark_object_obliterated(
        &self,
        oid: &str,
        hash_kind: ObjectHashKind,
        deleted_at: &str,
    ) -> StorageResult<ObjectObliterationRecord> {
        self.ensure_writable()?;
        validate_oid_for_hash_kind(oid, hash_kind)?;
        validate_non_empty_identifier(deleted_at, "obliteration deleted_at")?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin mark_object_obliterated transaction",
                err,
            )
        })?;
        let Some(existing) = query_obliteration_by_oid_and_kind(&txn, oid, hash_kind).await? else {
            return Err(StorageBackendError::NotFound {
                resource: "object_obliteration tombstone".to_string(),
            });
        };
        if existing.state == ObliterationState::Obliterated {
            drop(txn);
            return Ok(existing);
        }
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "UPDATE object_obliteration \
             SET state = 'obliterated', payload_deleted_at = ?, updated_at = ? \
             WHERE oid = ? AND hash_kind = ?",
            [
                deleted_at.into(),
                deleted_at.into(),
                oid.into(),
                hash_kind.as_str().into(),
            ],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "update object_obliteration to obliterated",
                err,
            )
        })?;

        let updated = query_obliteration_by_oid_and_kind(&txn, oid, hash_kind)
            .await?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "object_obliteration tombstone disappeared after finalize".to_string(),
            })?;
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit mark_object_obliterated transaction",
                err,
            )
        })?;
        Ok(updated)
    }

    async fn get_object_obliteration(
        &self,
        oid: &str,
    ) -> StorageResult<Option<ObjectObliterationRecord>> {
        validate_hex_oid(oid, "object_obliteration oid")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT oid, hash_kind, state, reason, actor, approval_source, \
                        requested_at, tombstone_written_at, payload_deleted_at, updated_at \
                 FROM object_obliteration WHERE oid = ? ORDER BY hash_kind ASC LIMIT 1",
                [oid.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "query object_obliteration by oid",
                    err,
                )
            })?;
        match row {
            Some(row) => decode_obliteration_row(&row).map(Some),
            None => Ok(None),
        }
    }

    async fn is_object_obliterated(&self, oid: &str) -> StorageResult<bool> {
        Ok(self.get_object_obliteration(oid).await?.is_some())
    }

    async fn list_incomplete_obliterations(&self) -> StorageResult<Vec<ObjectObliterationRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT oid, hash_kind, state, reason, actor, approval_source, \
                        requested_at, tombstone_written_at, payload_deleted_at, updated_at \
                 FROM object_obliteration WHERE state = 'obliterating' \
                 ORDER BY oid ASC, hash_kind ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list incomplete object_obliteration rows",
                    err,
                )
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(decode_obliteration_row(&row)?);
        }
        Ok(out)
    }

    async fn list_obliterated_oids(&self) -> StorageResult<Vec<String>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT DISTINCT oid FROM object_obliteration ORDER BY oid ASC".to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list obliterated OIDs", err)
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.try_get_by_index(0).map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "decode obliterated oid", err)
            })?);
        }
        Ok(out)
    }

    async fn run_fenced_object_read(
        &self,
        oid: &str,
        read_payload: Box<dyn FnOnce() -> StorageResult<Vec<u8>> + Send>,
    ) -> StorageResult<Vec<u8>> {
        validate_hex_oid(oid, "object OID")?;
        if let Some(rec) = self.get_object_obliteration(oid).await? {
            return Err(obliteration_fenced_read_error(rec.state));
        }
        let payload_result = tokio::task::spawn_blocking(read_payload)
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("fenced object read task failed: {err}"),
            })?;
        // Post-read monotonic verification: if an obliteration tombstone
        // committed while `read_payload` ran, discard the bytes (or replace a
        // post-unlink error) and fail closed with the obliteration fence error.
        if let Some(rec) = self.get_object_obliteration(oid).await? {
            return Err(obliteration_fenced_read_error(rec.state));
        }
        payload_result
    }

    async fn run_fenced_object_write(
        &self,
        record: &ObjectIndexRecord,
        write_payload: Box<dyn FnOnce() -> StorageResult<()> + Send>,
    ) -> StorageResult<()> {
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

        self.ensure_writable()?;
        validate_object_index_record(record)?;

        let state = Arc::new(Mutex::new(CancelState::Pending));
        let mut guard = CancelOnDrop {
            state: Arc::clone(&state),
            armed: true,
        };
        let worker_state = Arc::clone(&state);
        let conn = self.conn.clone();
        let rec = record.clone();

        // Own the SQLite `BEGIN IMMEDIATE` transaction inside a spawned worker
        // task rather than the caller's future. If the caller cancels or drops
        // this future while `write_payload` is running, the worker keeps the
        // SQLite write lock held until `write_payload` returns, and only then
        // rolls back the transaction — preventing any concurrent obliteration
        // from interleaving with an in-flight payload write.
        let worker = tokio::spawn(async move {
            let is_cancelled = || -> bool {
                worker_state
                    .lock()
                    .map(|s| *s == CancelState::Cancelled)
                    .unwrap_or(true)
            };
            if is_cancelled() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "sqlite fenced object write cancelled before starting".to_string(),
                });
            }

            let txn = begin_write_transaction(&conn).await.map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "begin run_fenced_object_write transaction",
                    err,
                )
            })?;

            if is_cancelled() {
                let _ = txn.rollback().await;
                return Err(StorageBackendError::BackendInternal {
                    detail: "sqlite fenced object write cancelled after acquiring write lock"
                        .to_string(),
                });
            }

            let mut distinct = BTreeSet::new();
            distinct.insert(rec.o_id.as_str());
            if let Err(err) = check_oids_not_obliterated_for_write_in_conn(&txn, &distinct).await {
                let _ = txn.rollback().await;
                return Err(err);
            }

            if is_cancelled() {
                let _ = txn.rollback().await;
                return Err(StorageBackendError::BackendInternal {
                    detail: "sqlite fenced object write cancelled before payload write".to_string(),
                });
            }

            // Move `txn` ownership onto the blocking OS thread alongside
            // `write_payload` so SQLite's `BEGIN IMMEDIATE` write lock remains
            // held until `write_payload` exits — even if the caller cancels the
            // future AND the originating Tokio runtime executes
            // `shutdown_background()` or `shutdown_timeout()`.
            let (txn, payload_res) = tokio::task::spawn_blocking(move || {
                let res = write_payload();
                (txn, res)
            })
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("fenced object write task failed: {err}"),
            })?;

            if let Err(err) = payload_res {
                let _ = txn.rollback().await;
                return Err(err);
            }

            if is_cancelled() {
                let _ = txn.rollback().await;
                return Err(StorageBackendError::BackendInternal {
                    detail: "sqlite fenced object write cancelled during payload write".to_string(),
                });
            }

            let backend = txn.get_database_backend();
            if let Err(err) = txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO object_index \
                     (o_id, o_type, o_size, repo_id, created_at, is_synced) VALUES (?, ?, ?, ?, ?, ?) \
                     ON CONFLICT(repo_id, o_id) DO UPDATE SET \
                       o_type = CASE \
                         WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
                          AND substr(object_index.o_type, 1, 6) != 'agent_' \
                         THEN excluded.o_type ELSE object_index.o_type END, \
                       o_size = CASE \
                         WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
                          AND substr(object_index.o_type, 1, 6) != 'agent_' \
                         THEN excluded.o_size ELSE object_index.o_size END, \
                       is_synced = CASE \
                         WHEN substr(excluded.o_type, 1, 6) = 'agent_' \
                          AND substr(object_index.o_type, 1, 6) != 'agent_' \
                         THEN 0 ELSE object_index.is_synced END",
                    [
                        Value::from(rec.o_id.clone()),
                        Value::from(rec.o_type.clone()),
                        Value::from(rec.o_size),
                        Value::from(rec.repo_id.clone()),
                        Value::from(rec.created_at),
                        Value::from(i32::from(rec.is_synced)),
                    ],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "upsert fenced object_index row",
                        err,
                    )
                })
            {
                let _ = txn.rollback().await;
                return Err(err);
            }

            let cancelled_before_commit = {
                let mut st =
                    worker_state
                        .lock()
                        .map_err(|_| StorageBackendError::BackendInternal {
                            detail: "sqlite fenced write cancellation state poisoned".to_string(),
                        })?;
                if *st == CancelState::Cancelled {
                    true
                } else {
                    *st = CancelState::Committing;
                    false
                }
            };
            if cancelled_before_commit {
                let _ = txn.rollback().await;
                return Err(StorageBackendError::BackendInternal {
                    detail: "sqlite fenced object write cancelled before commit".to_string(),
                });
            }

            txn.commit().await.map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "commit run_fenced_object_write transaction",
                    err,
                )
            })?;
            Ok(())
        });

        let result = worker
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!("sqlite fenced object write worker failed: {err}"),
            })?;
        guard.armed = false;
        result
    }

    async fn get_note(&self, notes_ref: &str, object: &str) -> StorageResult<Option<NoteRecord>> {
        validate_notes_ref(notes_ref)?;
        validate_hex_oid(object, "note object OID")?;
        let blob = find_note_blob_in_conn(&self.conn, notes_ref, object).await?;
        Ok(blob.map(|blob| NoteRecord {
            notes_ref: notes_ref.to_string(),
            object: object.to_string(),
            blob,
        }))
    }

    async fn list_notes(&self, notes_ref: &str) -> StorageResult<Vec<NoteRecord>> {
        validate_notes_ref(notes_ref)?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT notes_ref, object, blob FROM notes WHERE notes_ref = ? ORDER BY object ASC",
                [notes_ref.into()],
            ))
            .await
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "list notes rows", err))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(NoteRecord {
                notes_ref: row.try_get_by_index(0).map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "decode note notes_ref", err)
                })?,
                object: row.try_get_by_index(1).map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "decode note object", err)
                })?,
                blob: row.try_get_by_index(2).map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "decode note blob", err)
                })?,
            });
        }
        Ok(out)
    }

    async fn list_all_notes(&self) -> StorageResult<Vec<NoteRecord>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT notes_ref, object, blob FROM notes ORDER BY notes_ref ASC, object ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list all notes rows", err)
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(NoteRecord {
                notes_ref: row.try_get_by_index(0).map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "decode note notes_ref", err)
                })?,
                object: row.try_get_by_index(1).map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "decode note object", err)
                })?,
                blob: row.try_get_by_index(2).map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "decode note blob", err)
                })?,
            });
        }
        Ok(out)
    }

    async fn put_note(
        &self,
        notes_ref: &str,
        object: &str,
        blob: &str,
        overwrite: bool,
    ) -> StorageResult<NoteWriteOutcome> {
        self.ensure_writable()?;
        validate_notes_ref(notes_ref)?;
        validate_hex_oid(object, "note object OID")?;
        validate_hex_oid(blob, "note blob OID")?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "begin put_note transaction", err)
        })?;
        let existing = find_note_blob_in_conn(&txn, notes_ref, object).await?;
        let backend = txn.get_database_backend();
        let outcome = match existing {
            None => {
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO notes (notes_ref, object, blob) VALUES (?, ?, ?)",
                    [notes_ref.into(), object.into(), blob.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "insert note row", err)
                })?;
                NoteWriteOutcome::Inserted
            }
            Some(existing_blob) if !overwrite => {
                drop(txn);
                return Ok(NoteWriteOutcome::AlreadyExists { existing_blob });
            }
            Some(previous_blob) => {
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE notes SET blob = ? WHERE notes_ref = ? AND object = ?",
                    [blob.into(), notes_ref.into(), object.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "update note row", err)
                })?;
                NoteWriteOutcome::Replaced { previous_blob }
            }
        };
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(DatabaseRole::Repository, "commit put_note transaction", err)
        })?;
        Ok(outcome)
    }

    async fn remove_note_cas(
        &self,
        notes_ref: &str,
        object: &str,
        expected_blob: Option<&str>,
    ) -> StorageResult<bool> {
        self.ensure_writable()?;
        validate_notes_ref(notes_ref)?;
        validate_hex_oid(object, "note object OID")?;
        if let Some(exp) = expected_blob {
            validate_hex_oid(exp, "note expected_blob OID")?;
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin remove_note_cas transaction",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let res = match expected_blob {
            Some(exp) => txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM notes WHERE notes_ref = ? AND object = ? AND blob = ?",
                    [notes_ref.into(), object.into(), exp.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "delete note row cas", err)
                })?,
            None => txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM notes WHERE notes_ref = ? AND object = ?",
                    [notes_ref.into(), object.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "delete note row", err)
                })?,
        };
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit remove_note_cas transaction",
                err,
            )
        })?;
        Ok(res.rows_affected() > 0)
    }

    async fn apply_notes_cas_batch(
        &self,
        notes_ref: &str,
        ops: &[NoteCasOp],
    ) -> StorageResult<NoteBatchOutcome> {
        self.ensure_writable()?;
        validate_notes_ref(notes_ref)?;
        for op in ops {
            validate_note_cas_op(op)?;
        }
        if ops.is_empty() {
            return Ok(NoteBatchOutcome::Applied {
                inserted: 0,
                updated: 0,
                deleted: 0,
            });
        }

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin apply_notes_cas_batch transaction",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let mut inserted = 0usize;
        let mut updated = 0usize;
        let mut deleted = 0usize;

        for op in ops {
            match op {
                NoteCasOp::InsertIfAbsent { object, blob } => {
                    if let Some(actual) = find_note_blob_in_conn(&txn, notes_ref, object).await? {
                        drop(txn);
                        return Ok(NoteBatchOutcome::CasConflict {
                            object: object.clone(),
                            actual_blob: Some(actual),
                        });
                    }
                    txn.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "INSERT INTO notes (notes_ref, object, blob) VALUES (?, ?, ?)",
                        [notes_ref.into(), object.clone().into(), blob.clone().into()],
                    ))
                    .await
                    .map_err(|err| {
                        map_sqlite_db_err(DatabaseRole::Repository, "batch insert note row", err)
                    })?;
                    inserted += 1;
                }
                NoteCasOp::UpdateIfMatches {
                    object,
                    expected_blob,
                    new_blob,
                } => {
                    let actual = find_note_blob_in_conn(&txn, notes_ref, object).await?;
                    if actual.as_deref() != Some(expected_blob.as_str()) {
                        drop(txn);
                        return Ok(NoteBatchOutcome::CasConflict {
                            object: object.clone(),
                            actual_blob: actual,
                        });
                    }
                    txn.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "UPDATE notes SET blob = ? WHERE notes_ref = ? AND object = ? AND blob = ?",
                        [
                            new_blob.clone().into(),
                            notes_ref.into(),
                            object.clone().into(),
                            expected_blob.clone().into(),
                        ],
                    ))
                    .await
                    .map_err(|err| {
                        map_sqlite_db_err(DatabaseRole::Repository, "batch update note row", err)
                    })?;
                    updated += 1;
                }
                NoteCasOp::DeleteIfMatches {
                    object,
                    expected_blob,
                } => {
                    let actual = find_note_blob_in_conn(&txn, notes_ref, object).await?;
                    if actual.as_deref() != Some(expected_blob.as_str()) {
                        drop(txn);
                        return Ok(NoteBatchOutcome::CasConflict {
                            object: object.clone(),
                            actual_blob: actual,
                        });
                    }
                    txn.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "DELETE FROM notes WHERE notes_ref = ? AND object = ? AND blob = ?",
                        [
                            notes_ref.into(),
                            object.clone().into(),
                            expected_blob.clone().into(),
                        ],
                    ))
                    .await
                    .map_err(|err| {
                        map_sqlite_db_err(DatabaseRole::Repository, "batch delete note row", err)
                    })?;
                    deleted += 1;
                }
            }
        }

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit apply_notes_cas_batch transaction",
                err,
            )
        })?;
        Ok(NoteBatchOutcome::Applied {
            inserted,
            updated,
            deleted,
        })
    }

    async fn get_revision_ordinal_meta(
        &self,
        ref_name: &str,
    ) -> StorageResult<Option<RevisionOrdinalMeta>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        query_revision_ordinal_meta_in_conn(&self.conn, ref_name).await
    }

    async fn list_revision_ordinal_metas(&self) -> StorageResult<Vec<RevisionOrdinalMeta>> {
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT ref_name, tip_oid, replace_sig, max_ordinal, built_at \
                 FROM revision_ordinal_meta ORDER BY ref_name ASC"
                    .to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list revision_ordinal_meta rows",
                    err,
                )
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(decode_revision_ordinal_meta_row(&row)?);
        }
        Ok(out)
    }

    async fn replace_revision_ordinals(
        &self,
        meta: &RevisionOrdinalMeta,
        entries: &[(i64, String)],
    ) -> StorageResult<()> {
        self.ensure_writable()?;
        validate_revision_ordinal_entries(meta, entries)?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin replace_revision_ordinals transaction",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM revision_ordinal WHERE ref_name = ?",
            [meta.ref_name.clone().into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "clear revision_ordinal rows for rebuild",
                err,
            )
        })?;

        insert_revision_ordinal_rows_in_conn(&txn, &meta.ref_name, entries).await?;
        upsert_revision_ordinal_meta_in_conn(&txn, meta).await?;

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit replace_revision_ordinals transaction",
                err,
            )
        })?;
        Ok(())
    }

    async fn append_revision_ordinals(
        &self,
        ref_name: &str,
        expected_tip_oid: &str,
        expected_replace_sig: &str,
        new_meta: &RevisionOrdinalMeta,
        suffix_oids: &[String],
    ) -> StorageResult<RevisionOrdinalAppendOutcome> {
        self.ensure_writable()?;
        validate_revision_ordinal_append(
            ref_name,
            expected_tip_oid,
            expected_replace_sig,
            new_meta,
            suffix_oids,
        )?;

        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin append_revision_ordinals transaction",
                err,
            )
        })?;
        let actual = query_revision_ordinal_meta_in_conn(&txn, ref_name).await?;
        let Some(existing) = actual else {
            drop(txn);
            return Ok(RevisionOrdinalAppendOutcome::Stale { actual: None });
        };
        if existing.tip_oid != expected_tip_oid || existing.replace_sig != expected_replace_sig {
            drop(txn);
            return Ok(RevisionOrdinalAppendOutcome::Stale {
                actual: Some(existing),
            });
        }
        let suffix_len =
            i64::try_from(suffix_oids.len()).map_err(|_| StorageBackendError::BackendInternal {
                detail: "revision_ordinal suffix count overflowed i64".to_string(),
            })?;
        let expected_new_max = existing
            .max_ordinal
            .checked_add(suffix_len)
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "revision_ordinal max_ordinal overflowed i64".to_string(),
            })?;
        if new_meta.max_ordinal != expected_new_max {
            return Err(StorageBackendError::BackendInternal {
                detail:
                    "revision_ordinal append new_meta.max_ordinal does not match existing + suffix"
                        .to_string(),
            });
        }

        let mut rows = Vec::with_capacity(suffix_oids.len());
        let mut ord = existing.max_ordinal;
        for oid in suffix_oids {
            ord += 1;
            rows.push((ord, oid.clone()));
        }
        insert_revision_ordinal_rows_in_conn(&txn, ref_name, &rows).await?;
        upsert_revision_ordinal_meta_in_conn(&txn, new_meta).await?;

        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit append_revision_ordinals transaction",
                err,
            )
        })?;
        Ok(RevisionOrdinalAppendOutcome::Applied {
            meta: new_meta.clone(),
        })
    }

    async fn find_revision_oid_by_ordinal(
        &self,
        ref_name: &str,
        ordinal: i64,
    ) -> StorageResult<Option<String>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        if ordinal < 1 {
            return Ok(None);
        }
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT oid FROM revision_ordinal WHERE ref_name = ? AND ordinal = ? LIMIT 1",
                [ref_name.into(), ordinal.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "query revision_ordinal by ordinal",
                    err,
                )
            })?;
        match row {
            Some(row) => row.try_get_by_index::<String>(0).map(Some).map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "decode revision_ordinal oid", err)
            }),
            None => Ok(None),
        }
    }

    async fn find_revision_ordinal_by_oid(
        &self,
        ref_name: &str,
        oid: &str,
    ) -> StorageResult<Option<i64>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        validate_hex_oid(oid, "revision_ordinal oid")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT ordinal FROM revision_ordinal WHERE ref_name = ? AND oid = ? LIMIT 1",
                [ref_name.into(), oid.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "query revision_ordinal by oid",
                    err,
                )
            })?;
        match row {
            Some(row) => row.try_get_by_index::<i64>(0).map(Some).map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "decode revision_ordinal ordinal",
                    err,
                )
            }),
            None => Ok(None),
        }
    }

    async fn list_revision_ordinals(&self, ref_name: &str) -> StorageResult<Vec<(i64, String)>> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        let backend = self.conn.get_database_backend();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT ordinal, oid FROM revision_ordinal WHERE ref_name = ? ORDER BY ordinal ASC",
                [ref_name.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "list revision_ordinal rows", err)
            })?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let ordinal: i64 = row.try_get_by_index(0).map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "decode revision_ordinal ordinal",
                    err,
                )
            })?;
            let oid: String = row.try_get_by_index(1).map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "decode revision_ordinal oid", err)
            })?;
            out.push((ordinal, oid));
        }
        Ok(out)
    }

    async fn count_revision_ordinals(&self, ref_name: &str) -> StorageResult<u64> {
        validate_non_empty_identifier(ref_name, "revision_ordinal ref_name")?;
        let backend = self.conn.get_database_backend();
        let row = self
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT COUNT(*) FROM revision_ordinal WHERE ref_name = ?",
                [ref_name.into()],
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(DatabaseRole::Repository, "count revision_ordinal rows", err)
            })?
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "count revision_ordinal query returned zero rows".to_string(),
            })?;
        let count: i64 = row.try_get_by_index(0).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode revision_ordinal count",
                err,
            )
        })?;
        u64::try_from(count).map_err(|_| StorageBackendError::BackendInternal {
            detail: "revision_ordinal count was negative".to_string(),
        })
    }

    async fn prune_revision_ordinals_except(&self, live_refs: &[String]) -> StorageResult<usize> {
        self.ensure_writable()?;
        for r in live_refs {
            validate_non_empty_identifier(r, "revision_ordinal live ref")?;
        }
        let live_set: BTreeSet<&str> = live_refs.iter().map(String::as_str).collect();
        let txn = begin_write_transaction(&self.conn).await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "begin prune_revision_ordinals_except transaction",
                err,
            )
        })?;
        let backend = txn.get_database_backend();
        let rows = txn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT ref_name FROM revision_ordinal_meta ORDER BY ref_name ASC".to_string(),
            ))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "list revision_ordinal_meta refs for prune",
                    err,
                )
            })?;
        let mut pruned = 0usize;
        for row in rows {
            let ref_name: String = row.try_get_by_index(0).map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "decode revision_ordinal_meta ref_name",
                    err,
                )
            })?;
            if !live_set.contains(ref_name.as_str()) {
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM revision_ordinal WHERE ref_name = ?",
                    [ref_name.clone().into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(DatabaseRole::Repository, "prune revision_ordinal rows", err)
                })?;
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM revision_ordinal_meta WHERE ref_name = ?",
                    [ref_name.into()],
                ))
                .await
                .map_err(|err| {
                    map_sqlite_db_err(
                        DatabaseRole::Repository,
                        "prune revision_ordinal_meta row",
                        err,
                    )
                })?;
                pruned += 1;
            }
        }
        txn.commit().await.map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "commit prune_revision_ordinals_except transaction",
                err,
            )
        })?;
        Ok(pruned)
    }
}

async fn check_oids_not_obliterated_for_write_in_conn<C: ConnectionTrait>(
    conn: &C,
    distinct_oids: &BTreeSet<&str>,
) -> StorageResult<()> {
    if distinct_oids.is_empty() {
        return Ok(());
    }
    let backend = conn.get_database_backend();
    let ordered: Vec<&str> = distinct_oids.iter().copied().collect();
    for chunk in ordered.chunks(400) {
        let placeholders = vec!["?"; chunk.len()].join(", ");
        let sql = format!(
            "SELECT state FROM object_obliteration WHERE oid IN ({placeholders}) \
             ORDER BY oid ASC LIMIT 1"
        );
        let values: Vec<Value> = chunk
            .iter()
            .map(|oid| Value::from((*oid).to_string()))
            .collect();
        if let Some(row) = conn
            .query_one_raw(Statement::from_sql_and_values(backend, sql, values))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "check object_obliteration write fence",
                    err,
                )
            })?
        {
            let state_str: String = row.try_get_by_index(0).map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "decode object_obliteration state",
                    err,
                )
            })?;
            let state = ObliterationState::parse(&state_str)?;
            return Err(obliteration_fenced_write_error(state));
        }
    }
    Ok(())
}

fn decode_object_index_row(row: &sea_orm::QueryResult) -> StorageResult<ObjectIndexRecord> {
    let repo_id: String = row.try_get_by_index(0).map_err(|err| {
        map_sqlite_db_err(DatabaseRole::Repository, "decode object_index repo_id", err)
    })?;
    let o_id: String = row.try_get_by_index(1).map_err(|err| {
        map_sqlite_db_err(DatabaseRole::Repository, "decode object_index o_id", err)
    })?;
    let o_type: String = row.try_get_by_index(2).map_err(|err| {
        map_sqlite_db_err(DatabaseRole::Repository, "decode object_index o_type", err)
    })?;
    let o_size: i64 = row.try_get_by_index(3).map_err(|err| {
        map_sqlite_db_err(DatabaseRole::Repository, "decode object_index o_size", err)
    })?;
    let created_at: i64 = row.try_get_by_index(4).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_index created_at",
            err,
        )
    })?;
    let is_synced: i32 = row.try_get_by_index(5).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_index is_synced",
            err,
        )
    })?;
    Ok(ObjectIndexRecord {
        repo_id,
        o_id,
        o_type,
        o_size,
        created_at,
        is_synced: is_synced != 0,
    })
}

async fn query_obliteration_by_oid_and_kind<C: ConnectionTrait>(
    conn: &C,
    oid: &str,
    hash_kind: ObjectHashKind,
) -> StorageResult<Option<ObjectObliterationRecord>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT oid, hash_kind, state, reason, actor, approval_source, \
                    requested_at, tombstone_written_at, payload_deleted_at, updated_at \
             FROM object_obliteration WHERE oid = ? AND hash_kind = ? LIMIT 1",
            [oid.into(), hash_kind.as_str().into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "query object_obliteration row",
                err,
            )
        })?;
    match row {
        Some(row) => decode_obliteration_row(&row).map(Some),
        None => Ok(None),
    }
}

fn decode_obliteration_row(row: &sea_orm::QueryResult) -> StorageResult<ObjectObliterationRecord> {
    let oid: String = row.try_get_by_index(0).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration oid",
            err,
        )
    })?;
    let hash_kind_str: String = row.try_get_by_index(1).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration hash_kind",
            err,
        )
    })?;
    let state_str: String = row.try_get_by_index(2).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration state",
            err,
        )
    })?;
    let reason: Option<String> = row.try_get_by_index(3).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration reason",
            err,
        )
    })?;
    let actor: Option<String> = row.try_get_by_index(4).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration actor",
            err,
        )
    })?;
    let approval_source: Option<String> = row.try_get_by_index(5).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration approval_source",
            err,
        )
    })?;
    let requested_at: String = row.try_get_by_index(6).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration requested_at",
            err,
        )
    })?;
    let tombstone_written_at: String = row.try_get_by_index(7).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration tombstone_written_at",
            err,
        )
    })?;
    let payload_deleted_at: Option<String> = row.try_get_by_index(8).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration payload_deleted_at",
            err,
        )
    })?;
    let updated_at: String = row.try_get_by_index(9).map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "decode object_obliteration updated_at",
            err,
        )
    })?;
    Ok(ObjectObliterationRecord {
        oid,
        hash_kind: ObjectHashKind::parse(&hash_kind_str)?,
        state: ObliterationState::parse(&state_str)?,
        reason,
        actor,
        approval_source,
        requested_at,
        tombstone_written_at,
        payload_deleted_at,
        updated_at,
    })
}

async fn find_note_blob_in_conn<C: ConnectionTrait>(
    conn: &C,
    notes_ref: &str,
    object: &str,
) -> StorageResult<Option<String>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT blob FROM notes WHERE notes_ref = ? AND object = ? LIMIT 1",
            [notes_ref.into(), object.into()],
        ))
        .await
        .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "query note blob", err))?;
    match row {
        Some(row) => row
            .try_get_by_index::<String>(0)
            .map(Some)
            .map_err(|err| map_sqlite_db_err(DatabaseRole::Repository, "decode note blob", err)),
        None => Ok(None),
    }
}

async fn query_revision_ordinal_meta_in_conn<C: ConnectionTrait>(
    conn: &C,
    ref_name: &str,
) -> StorageResult<Option<RevisionOrdinalMeta>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT ref_name, tip_oid, replace_sig, max_ordinal, built_at \
             FROM revision_ordinal_meta WHERE ref_name = ? LIMIT 1",
            [ref_name.into()],
        ))
        .await
        .map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "query revision_ordinal_meta row",
                err,
            )
        })?;
    match row {
        Some(row) => decode_revision_ordinal_meta_row(&row).map(Some),
        None => Ok(None),
    }
}

fn decode_revision_ordinal_meta_row(
    row: &sea_orm::QueryResult,
) -> StorageResult<RevisionOrdinalMeta> {
    Ok(RevisionOrdinalMeta {
        ref_name: row.try_get_by_index(0).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode revision_ordinal_meta ref_name",
                err,
            )
        })?,
        tip_oid: row.try_get_by_index(1).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode revision_ordinal_meta tip_oid",
                err,
            )
        })?,
        replace_sig: row.try_get_by_index(2).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode revision_ordinal_meta replace_sig",
                err,
            )
        })?,
        max_ordinal: row.try_get_by_index(3).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode revision_ordinal_meta max_ordinal",
                err,
            )
        })?,
        built_at: row.try_get_by_index(4).map_err(|err| {
            map_sqlite_db_err(
                DatabaseRole::Repository,
                "decode revision_ordinal_meta built_at",
                err,
            )
        })?,
    })
}

async fn insert_revision_ordinal_rows_in_conn<C: ConnectionTrait>(
    conn: &C,
    ref_name: &str,
    rows: &[(i64, String)],
) -> StorageResult<()> {
    if rows.is_empty() {
        return Ok(());
    }
    const INSERT_CHUNK: usize = 250;
    let backend = conn.get_database_backend();
    for chunk in rows.chunks(INSERT_CHUNK) {
        let mut sql = String::from("INSERT INTO revision_ordinal (ref_name, ordinal, oid) VALUES ");
        let mut values = Vec::with_capacity(chunk.len() * 3);
        for (idx, (ordinal, oid)) in chunk.iter().enumerate() {
            if idx != 0 {
                sql.push_str(", ");
            }
            sql.push_str("(?, ?, ?)");
            values.extend([
                Value::from(ref_name.to_string()),
                Value::from(*ordinal),
                Value::from(oid.clone()),
            ]);
        }
        conn.execute_raw(Statement::from_sql_and_values(backend, sql, values))
            .await
            .map_err(|err| {
                map_sqlite_db_err(
                    DatabaseRole::Repository,
                    "insert revision_ordinal rows",
                    err,
                )
            })?;
    }
    Ok(())
}

async fn upsert_revision_ordinal_meta_in_conn<C: ConnectionTrait>(
    conn: &C,
    meta: &RevisionOrdinalMeta,
) -> StorageResult<()> {
    let backend = conn.get_database_backend();
    conn.execute_raw(Statement::from_sql_and_values(
        backend,
        "INSERT INTO revision_ordinal_meta (ref_name, tip_oid, replace_sig, max_ordinal, built_at) \
         VALUES (?, ?, ?, ?, ?) \
         ON CONFLICT(ref_name) DO UPDATE SET \
           tip_oid = excluded.tip_oid, \
           replace_sig = excluded.replace_sig, \
           max_ordinal = excluded.max_ordinal, \
           built_at = excluded.built_at",
        [
            meta.ref_name.clone().into(),
            meta.tip_oid.clone().into(),
            meta.replace_sig.clone().into(),
            meta.max_ordinal.into(),
            meta.built_at.clone().into(),
        ],
    ))
    .await
    .map_err(|err| {
        map_sqlite_db_err(
            DatabaseRole::Repository,
            "upsert revision_ordinal_meta row",
            err,
        )
    })?;
    Ok(())
}
