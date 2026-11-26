//! SQLite projection for stable changes and their visible revisions.

use std::fmt;

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, DbErr, Statement};
use thiserror::Error;

use super::ChangeId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RevisionVisibility {
    Visible,
    Hidden,
}

impl fmt::Display for RevisionVisibility {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Visible => "visible",
            Self::Hidden => "hidden",
        })
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ChangeRevision {
    pub change_id: ChangeId,
    pub commit_oid: String,
    pub created_op_id: String,
    pub visibility: RevisionVisibility,
    pub revision_ordinal: i64,
}

#[derive(Debug, Error)]
pub enum ChangeStoreError {
    #[error("database error: {0}")]
    Database(#[from] DbErr),
    #[error("change id collision: {0}")]
    Collision(String),
    #[error("invalid projected change id: {0}")]
    InvalidChangeId(String),
}

#[derive(Clone)]
pub struct ChangeStore {
    db: Option<DatabaseConnection>,
    state_store: Option<std::sync::Arc<dyn crate::storage::git_state_store::GitStateStore>>,
}

impl ChangeStore {
    pub fn new(db: DatabaseConnection) -> Self {
        Self {
            db: Some(db),
            state_store: None,
        }
    }

    pub fn new_with_store(
        state_store: std::sync::Arc<dyn crate::storage::git_state_store::GitStateStore>,
    ) -> Self {
        Self {
            db: None,
            state_store: Some(state_store),
        }
    }

    pub fn backend_kind(&self) -> crate::storage::StorageBackendKind {
        if let Some(store) = &self.state_store {
            store.backend()
        } else {
            crate::storage::StorageBackendKind::Sqlite
        }
    }

    fn require_db(&self) -> Result<&DatabaseConnection, ChangeStoreError> {
        self.db.as_ref().ok_or_else(|| {
            ChangeStoreError::Database(DbErr::Custom(
                "ChangeStore has no SQLite connection".to_string(),
            ))
        })
    }

    fn require_state_store(
        &self,
    ) -> Result<&std::sync::Arc<dyn crate::storage::git_state_store::GitStateStore>, ChangeStoreError>
    {
        self.state_store.as_ref().ok_or_else(|| {
            ChangeStoreError::Database(DbErr::Custom(
                "ChangeStore has no GitStateStore handle".to_string(),
            ))
        })
    }

    pub(crate) async fn insert_identity_with_store(
        store: &dyn crate::storage::git_state_store::GitStateStore,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        let id_str = change_id.to_string();
        if store
            .get_change_identity(&id_str)
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?
            .is_some()
        {
            return Err(ChangeStoreError::Collision(id_str));
        }
        store
            .upsert_change_identity(&crate::storage::git_state_store::ChangeIdentityRecord {
                change_id: id_str,
                repo_id: repo_id.to_string(),
                origin: origin.to_string(),
                created_op_id: created_op_id.to_string(),
                created_at: Utc::now().timestamp_millis(),
            })
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))
    }

    pub(crate) async fn ensure_identity_with_store(
        store: &dyn crate::storage::git_state_store::GitStateStore,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        match Self::insert_identity_with_store(store, repo_id, change_id, origin, created_op_id)
            .await
        {
            Ok(()) => Ok(()),
            Err(ChangeStoreError::Collision(_)) => {
                let existing = store
                    .get_change_identity(&change_id.to_string())
                    .await
                    .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?;
                if existing.as_ref().map(|r| r.repo_id.as_str()) == Some(repo_id) {
                    Ok(())
                } else {
                    Err(ChangeStoreError::Collision(change_id.to_string()))
                }
            }
            Err(err) => Err(err),
        }
    }

    pub(crate) async fn next_revision_ordinal_with_store(
        store: &dyn crate::storage::git_state_store::GitStateStore,
        repo_id: &str,
        change_id: ChangeId,
    ) -> Result<i64, ChangeStoreError> {
        let id_str = change_id.to_string();
        let Some(identity) = store
            .get_change_identity(&id_str)
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?
        else {
            return Ok(0);
        };
        if identity.repo_id != repo_id {
            return Ok(0);
        }
        let rows = store
            .list_change_revisions(&id_str)
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?;
        Ok(rows.len() as i64)
    }

    pub(crate) async fn insert_revision_with_store(
        store: &dyn crate::storage::git_state_store::GitStateStore,
        revision: &ChangeRevision,
    ) -> Result<(), ChangeStoreError> {
        let id_str = revision.change_id.to_string();
        let existing = store
            .list_change_revisions(&id_str)
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?;
        if existing.iter().any(|r| r.commit_oid == revision.commit_oid) {
            return Ok(());
        }
        store
            .upsert_change_revision(&crate::storage::git_state_store::ChangeRevisionRecord {
                change_id: id_str,
                commit_oid: revision.commit_oid.clone(),
                created_op_id: revision.created_op_id.clone(),
                visibility: revision.visibility.to_string(),
                revision_ordinal: revision.revision_ordinal,
            })
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))
    }

    pub(crate) async fn revision_with_store(
        store: &dyn crate::storage::git_state_store::GitStateStore,
        change_id: ChangeId,
        commit_oid: &str,
    ) -> Result<Option<ChangeRevision>, ChangeStoreError> {
        let rows = store
            .list_change_revisions(&change_id.to_string())
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?;
        let Some(rec) = rows.into_iter().find(|r| r.commit_oid == commit_oid) else {
            return Ok(None);
        };
        change_revision_from_record(rec).map(Some)
    }

    pub(crate) async fn change_id_for_commit_with_store(
        store: &dyn crate::storage::git_state_store::GitStateStore,
        repo_id: &str,
        commit_oid: &str,
    ) -> Result<Option<ChangeId>, ChangeStoreError> {
        let mut rows = store
            .find_change_revisions_by_commit(commit_oid)
            .await
            .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?;
        rows.sort_by_key(|r| r.revision_ordinal);
        for rec in rows {
            if let Some(id_rec) = store
                .get_change_identity(&rec.change_id)
                .await
                .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?
                && id_rec.repo_id == repo_id
            {
                let parsed = rec
                    .change_id
                    .parse()
                    .map_err(|_| ChangeStoreError::InvalidChangeId(rec.change_id))?;
                return Ok(Some(parsed));
            }
        }
        Ok(None)
    }

    pub async fn insert_identity(
        &self,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                Self::insert_identity_with_store(
                    &**store,
                    repo_id,
                    change_id,
                    origin,
                    created_op_id,
                )
                .await
            }
            crate::storage::StorageBackendKind::Sqlite => {
                Self::insert_identity_on(
                    self.require_db()?,
                    repo_id,
                    change_id,
                    origin,
                    created_op_id,
                )
                .await
            }
        }
    }

    pub(crate) async fn ensure_identity_on<C: ConnectionTrait>(
        db: &C,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        match Self::insert_identity_on(db, repo_id, change_id, origin, created_op_id).await {
            Ok(()) => Ok(()),
            Err(ChangeStoreError::Collision(_)) => {
                let row = db
                    .query_one_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT repo_id FROM change_identity WHERE change_id = ?",
                        [change_id.to_string().into()],
                    ))
                    .await?;
                if row
                    .and_then(|row| row.try_get::<String>("", "repo_id").ok())
                    .as_deref()
                    == Some(repo_id)
                {
                    Ok(())
                } else {
                    Err(ChangeStoreError::Collision(change_id.to_string()))
                }
            }
            Err(error) => Err(error),
        }
    }

    pub(crate) async fn next_revision_ordinal_on<C: ConnectionTrait>(
        db: &C,
        repo_id: &str,
        change_id: ChangeId,
    ) -> Result<i64, ChangeStoreError> {
        let row = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM change_revision r \
                 JOIN change_identity i ON i.change_id = r.change_id \
                 WHERE i.repo_id = ? AND r.change_id = ?",
                [repo_id.to_string().into(), change_id.to_string().into()],
            ))
            .await?;
        Ok(row
            .map(|row| row.try_get::<i64>("", "count"))
            .transpose()?
            .unwrap_or(0))
    }

    pub(crate) async fn insert_revision_on<C: ConnectionTrait>(
        db: &C,
        revision: &ChangeRevision,
    ) -> Result<(), ChangeStoreError> {
        db.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO change_revision \
             (change_id, commit_oid, created_op_id, visibility, revision_ordinal) \
             VALUES (?, ?, ?, ?, ?) \
             ON CONFLICT(change_id, commit_oid) DO NOTHING",
            [
                revision.change_id.to_string().into(),
                revision.commit_oid.clone().into(),
                revision.created_op_id.clone().into(),
                revision.visibility.to_string().into(),
                revision.revision_ordinal.into(),
            ],
        ))
        .await
        .map(|_| ())
        .map_err(ChangeStoreError::Database)
    }

    pub(crate) async fn revision_on<C: ConnectionTrait>(
        db: &C,
        change_id: ChangeId,
        commit_oid: &str,
    ) -> Result<Option<ChangeRevision>, ChangeStoreError> {
        let row = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT change_id, commit_oid, created_op_id, visibility, revision_ordinal \
                 FROM change_revision WHERE change_id = ? AND commit_oid = ?",
                [change_id.to_string().into(), commit_oid.to_string().into()],
            ))
            .await?;
        let Some(row) = row else {
            return Ok(None);
        };
        let id = row.try_get::<String>("", "change_id")?;
        let visibility = match row.try_get::<String>("", "visibility")?.as_str() {
            "visible" => RevisionVisibility::Visible,
            "hidden" => RevisionVisibility::Hidden,
            value => return Err(ChangeStoreError::InvalidChangeId(value.to_string())),
        };
        Ok(Some(ChangeRevision {
            change_id: id
                .parse()
                .map_err(|_| ChangeStoreError::InvalidChangeId(id))?,
            commit_oid: row.try_get("", "commit_oid")?,
            created_op_id: row.try_get("", "created_op_id")?,
            visibility,
            revision_ordinal: row.try_get("", "revision_ordinal")?,
        }))
    }

    pub(crate) async fn insert_identity_on<C: ConnectionTrait>(
        db: &C,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        let result = db
            .execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO change_identity \
                 (change_id, repo_id, origin, created_op_id, created_at) VALUES (?, ?, ?, ?, ?)",
                [
                    change_id.to_string().into(),
                    repo_id.to_string().into(),
                    origin.to_string().into(),
                    created_op_id.to_string().into(),
                    Utc::now().timestamp_millis().into(),
                ],
            ))
            .await;
        result.map(|_| ()).map_err(|error| {
            if error
                .to_string()
                .to_ascii_lowercase()
                .contains("constraint")
            {
                ChangeStoreError::Collision(change_id.to_string())
            } else {
                ChangeStoreError::Database(error)
            }
        })
    }

    pub async fn ensure_identity(
        &self,
        repo_id: &str,
        change_id: ChangeId,
        origin: &str,
        created_op_id: &str,
    ) -> Result<(), ChangeStoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                Self::ensure_identity_with_store(
                    &**store,
                    repo_id,
                    change_id,
                    origin,
                    created_op_id,
                )
                .await
            }
            crate::storage::StorageBackendKind::Sqlite => {
                Self::ensure_identity_on(
                    self.require_db()?,
                    repo_id,
                    change_id,
                    origin,
                    created_op_id,
                )
                .await
            }
        }
    }

    pub async fn insert_revision(&self, revision: &ChangeRevision) -> Result<(), ChangeStoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                Self::insert_revision_with_store(&**store, revision).await
            }
            crate::storage::StorageBackendKind::Sqlite => {
                Self::insert_revision_on(self.require_db()?, revision).await
            }
        }
    }

    pub async fn revisions_for_change(
        &self,
        repo_id: &str,
        change_id: ChangeId,
    ) -> Result<Vec<ChangeRevision>, ChangeStoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let id_str = change_id.to_string();
                let Some(identity) = store
                    .get_change_identity(&id_str)
                    .await
                    .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?
                else {
                    return Ok(Vec::new());
                };
                if identity.repo_id != repo_id {
                    return Ok(Vec::new());
                }
                let mut rows = store
                    .list_change_revisions(&id_str)
                    .await
                    .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?;
                rows.sort_by_key(|r| r.revision_ordinal);
                rows.into_iter().map(change_revision_from_record).collect()
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT r.change_id, r.commit_oid, r.created_op_id, r.visibility, \
                         r.revision_ordinal FROM change_revision r \
                         JOIN change_identity i ON i.change_id = r.change_id \
                         WHERE i.repo_id = ? AND r.change_id = ? ORDER BY r.revision_ordinal",
                        [repo_id.to_string().into(), change_id.to_string().into()],
                    ))
                    .await?;
                rows.into_iter()
                    .map(|row| {
                        let id = row.try_get::<String>("", "change_id")?;
                        Ok(ChangeRevision {
                            change_id: id
                                .parse()
                                .map_err(|_| ChangeStoreError::InvalidChangeId(id.clone()))?,
                            commit_oid: row.try_get("", "commit_oid")?,
                            created_op_id: row.try_get("", "created_op_id")?,
                            visibility: match row.try_get::<String>("", "visibility")?.as_str() {
                                "visible" => RevisionVisibility::Visible,
                                "hidden" => RevisionVisibility::Hidden,
                                value => {
                                    return Err(ChangeStoreError::InvalidChangeId(
                                        value.to_string(),
                                    ));
                                }
                            },
                            revision_ordinal: row.try_get("", "revision_ordinal")?,
                        })
                    })
                    .collect()
            }
        }
    }

    /// Return a bounded repository-wide revision projection for read-only
    /// consumers such as CLI operation-history views.
    pub async fn revisions_for_repo(
        &self,
        repo_id: &str,
        limit: usize,
    ) -> Result<Vec<ChangeRevision>, ChangeStoreError> {
        let clamped = limit.clamp(1, 200);
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                let identities = store
                    .list_change_identities_by_prefix(repo_id, "")
                    .await
                    .map_err(|err| ChangeStoreError::Database(DbErr::Custom(err.to_string())))?;
                let mut all = Vec::new();
                for id in identities {
                    let revs = store
                        .list_change_revisions(&id.change_id)
                        .await
                        .map_err(|err| {
                            ChangeStoreError::Database(DbErr::Custom(err.to_string()))
                        })?;
                    for rec in revs {
                        all.push(change_revision_from_record(rec)?);
                    }
                }
                all.sort_by(|a, b| {
                    b.revision_ordinal
                        .cmp(&a.revision_ordinal)
                        .then_with(|| b.commit_oid.cmp(&a.commit_oid))
                });
                all.truncate(clamped);
                Ok(all)
            }
            crate::storage::StorageBackendKind::Sqlite => {
                let db = self.require_db()?;
                let rows = db
                    .query_all_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "SELECT r.change_id, r.commit_oid, r.created_op_id, r.visibility, \
                         r.revision_ordinal FROM change_revision r \
                         JOIN change_identity i ON i.change_id = r.change_id \
                         WHERE i.repo_id = ? ORDER BY r.revision_ordinal DESC, r.commit_oid DESC LIMIT ?",
                        [repo_id.to_string().into(), (clamped as i64).into()],
                    ))
                    .await?;
                rows.into_iter()
                    .map(|row| {
                        let id = row.try_get::<String>("", "change_id")?;
                        Ok(ChangeRevision {
                            change_id: id
                                .parse()
                                .map_err(|_| ChangeStoreError::InvalidChangeId(id.clone()))?,
                            commit_oid: row.try_get("", "commit_oid")?,
                            created_op_id: row.try_get("", "created_op_id")?,
                            visibility: match row.try_get::<String>("", "visibility")?.as_str() {
                                "visible" => RevisionVisibility::Visible,
                                "hidden" => RevisionVisibility::Hidden,
                                value => {
                                    return Err(ChangeStoreError::InvalidChangeId(
                                        value.to_string(),
                                    ));
                                }
                            },
                            revision_ordinal: row.try_get("", "revision_ordinal")?,
                        })
                    })
                    .collect()
            }
        }
    }

    pub async fn change_id_for_commit(
        &self,
        repo_id: &str,
        commit_oid: &str,
    ) -> Result<Option<ChangeId>, ChangeStoreError> {
        match self.backend_kind() {
            crate::storage::StorageBackendKind::Redb => {
                let store = self.require_state_store()?;
                Self::change_id_for_commit_with_store(&**store, repo_id, commit_oid).await
            }
            crate::storage::StorageBackendKind::Sqlite => {
                self.change_id_for_commit_on(self.require_db()?, repo_id, commit_oid)
                    .await
            }
        }
    }

    pub(crate) async fn change_id_for_commit_on<C: ConnectionTrait>(
        &self,
        db: &C,
        repo_id: &str,
        commit_oid: &str,
    ) -> Result<Option<ChangeId>, ChangeStoreError> {
        let row = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT r.change_id FROM change_revision r \
                 JOIN change_identity i ON i.change_id = r.change_id \
                 WHERE i.repo_id = ? AND r.commit_oid = ? \
                 ORDER BY r.revision_ordinal LIMIT 1",
                [repo_id.to_string().into(), commit_oid.to_string().into()],
            ))
            .await?;
        row.map(|row| {
            let id = row.try_get::<String>("", "change_id")?;
            id.parse()
                .map_err(|_| ChangeStoreError::InvalidChangeId(id))
        })
        .transpose()
    }
}

fn change_revision_from_record(
    rec: crate::storage::git_state_store::ChangeRevisionRecord,
) -> Result<ChangeRevision, ChangeStoreError> {
    let visibility = match rec.visibility.as_str() {
        "visible" => RevisionVisibility::Visible,
        "hidden" => RevisionVisibility::Hidden,
        value => return Err(ChangeStoreError::InvalidChangeId(value.to_string())),
    };
    Ok(ChangeRevision {
        change_id: rec
            .change_id
            .parse()
            .map_err(|_| ChangeStoreError::InvalidChangeId(rec.change_id))?,
        commit_oid: rec.commit_oid,
        created_op_id: rec.created_op_id,
        visibility,
        revision_ordinal: rec.revision_ordinal,
    })
}

#[cfg(all(test, feature = "storage-redb"))]
mod tests {
    use super::*;
    use crate::internal::change::{
        ChangeIdResolution, ChangeRevisionBuilder, RelationKind,
        genealogy::{
            AiOperationLink, ai_links_for_change_with_store,
            attach_pending_ai_operation_links_with_store, evolution_for_commit_with_store,
            link_ai_operation_with_store, record_pending_ai_operation_link_with_store,
        },
        resolve::resolve_change_id_prefix_with_store,
    };

    #[tokio::test]
    async fn redb_change_genealogy_and_global_migration_compat() {
        let dir = tempfile::tempdir().expect("tempdir");
        let dot_libra = dir.path().join(".libra");
        let _paths = crate::storage::runtime::init_redb_repository_storage(&dot_libra)
            .await
            .expect("init redb repo");
        assert!(!dot_libra.join("libra.db").exists());

        let state_store = crate::storage::runtime::open_git_state_store_read_write(&dot_libra)
            .await
            .expect("open git state store");
        let repo_id = "repo-rb14-change";
        let oid_a = "1111111111111111111111111111111111111111";
        let oid_b = "2222222222222222222222222222222222222222";
        let oid_split = "3333333333333333333333333333333333333333";

        let rev1 = ChangeRevisionBuilder::for_new_change_with_store(
            state_store.clone(),
            repo_id,
            "op-change-1",
        )
        .set_commit_oid(oid_a)
        .build()
        .await
        .expect("build rev1");

        let rev2 = ChangeRevisionBuilder::for_rewrite_with_store(
            state_store.clone(),
            repo_id,
            "op-change-2",
            rev1.change_id,
        )
        .set_commit_oid(oid_b)
        .set_predecessors([(oid_a, RelationKind::Amend)])
        .build()
        .await
        .expect("build rev2");
        assert_eq!(rev2.change_id, rev1.change_id);
        assert_eq!(rev2.revision_ordinal, 1);

        let rev_split = ChangeRevisionBuilder::for_new_change_with_store(
            state_store.clone(),
            repo_id,
            "op-change-3",
        )
        .set_commit_oid(oid_split)
        .set_predecessors([(oid_b, RelationKind::Split)])
        .build()
        .await
        .expect("build split rev");
        assert_ne!(rev_split.change_id, rev1.change_id);

        let store = ChangeStore::new_with_store(state_store.clone());
        assert_eq!(
            store
                .change_id_for_commit(repo_id, oid_b)
                .await
                .expect("lookup commit"),
            Some(rev1.change_id)
        );
        let revs = store
            .revisions_for_change(repo_id, rev1.change_id)
            .await
            .expect("list revisions");
        assert_eq!(revs.len(), 2);

        let prefix = &rev1.change_id.to_string()[..12];
        assert_eq!(
            resolve_change_id_prefix_with_store(&*state_store, repo_id, prefix)
                .await
                .expect("resolve prefix"),
            ChangeIdResolution::Exact(rev1.change_id)
        );

        let edges = evolution_for_commit_with_store(&*state_store, oid_b, 10)
            .await
            .expect("evolution");
        assert_eq!(edges.len(), 1);
        assert_eq!(edges[0].predecessor_oid, oid_a);
        assert_eq!(edges[0].relation_kind, RelationKind::Amend);

        // AI operation links (pending + direct).
        record_pending_ai_operation_link_with_store(
            &*state_store,
            "op-pending-1",
            Some("sess-1"),
            None,
            None,
            Some("intent-1"),
            repo_id,
            "v1",
        )
        .await
        .expect("record pending link");
        attach_pending_ai_operation_links_with_store(
            &*state_store,
            repo_id,
            rev1.change_id,
            &["op-pending-1"],
        )
        .await
        .expect("attach pending");
        link_ai_operation_with_store(
            &*state_store,
            &AiOperationLink {
                operation_id: "op-direct-1".to_string(),
                change_id: rev1.change_id,
                session_id: Some("sess-1".to_string()),
                run_id: None,
                tool_invocation_id: None,
                intent_id: Some("intent-1".to_string()),
                repo_id: repo_id.to_string(),
                worktree_id: None,
                workspace_id: None,
                lease_generation: None,
                config_provenance_digest: None,
                redaction_version: "v1".to_string(),
            },
        )
        .await
        .expect("link ai op");
        let links = ai_links_for_change_with_store(&*state_store, repo_id, rev1.change_id)
            .await
            .expect("list ai links");
        assert_eq!(links.len(), 2);

        // ADR-RB-02 + GC-13 global_migration classification: config.redb vs unmigrated config.db.
        let global_dir = dir.path().join("global-cfg");
        std::fs::create_dir_all(&global_dir).expect("mkdir global-cfg");
        let unmigrated_db = global_dir.join("config.db");
        std::fs::write(&unmigrated_db, b"SQLite format 3\0").expect("seed dummy sqlite");
        assert!(matches!(
            crate::internal::config::global_migration::classify_global_config_dir(&global_dir)
                .expect("classify sqlite"),
            crate::internal::config::global_migration::GlobalConfigBackendState::UnmigratedSqlite { .. }
        ));

        let redb_dir = dir.path().join("global-redb");
        std::fs::create_dir_all(&redb_dir).expect("mkdir global-redb");
        let _guard = crate::storage::open_or_create_locked_config_scope_for_write(
            &redb_dir,
            crate::internal::db::schema::DatabaseRole::GlobalConfig,
            crate::storage::StorageBackendKind::Redb,
            crate::storage::DEFAULT_CONFIG_SCOPE_LOCK_TIMEOUT,
        )
        .await
        .expect("create global config.redb");
        assert!(matches!(
            crate::internal::config::global_migration::classify_global_config_dir(&redb_dir)
                .expect("classify redb"),
            crate::internal::config::global_migration::GlobalConfigBackendState::RedbActive { .. }
        ));
        let outcome = crate::internal::config::global_migration::migrate_legacy_global_config(
            &unmigrated_db,
            &redb_dir.join("config.db"),
        )
        .await;
        assert_eq!(
            outcome,
            crate::internal::config::global_migration::GlobalConfigMigration::AlreadyPresent
        );
        assert!(!dot_libra.join("libra.db").exists());
    }
}
