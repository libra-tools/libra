//! SQLite implementation of [`GitStore`] (`RB-04`).
//!
//! Operates directly against the canonical repository SQLite tables
//! (`reference`, `reflog`, and `agent_bridge_publication`) inside single
//! `BEGIN IMMEDIATE` write transactions (`begin_write_transaction`).

use std::{path::Path, time::Duration};

use async_trait::async_trait;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectOptions, ConnectionTrait, Database,
    DatabaseConnection, EntityTrait, ModelTrait, QueryFilter, QueryOrder, QuerySelect, Statement,
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
            BridgePublicationPhase, BridgePublicationRecord, GitStore, REFLOG_HEAD_REF_NAME,
            RefCasOutcome, RefKey, RefRecord, RefTarget, ReflogEntry, ReflogWrite,
            is_valid_bridge_operation_id, validate_bridge_publication_binding, validate_ref_key,
            validate_ref_target, validate_reflog_writes_for_key, validate_worktree_filter,
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
}
