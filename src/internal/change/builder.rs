//! Single entry point for new and rewritten Change revisions.

use sea_orm::{DatabaseConnection, DatabaseTransaction, TransactionTrait};
use thiserror::Error;

use super::{
    AiOperationLink, ChangeId, ChangeRevision, ChangeStore, ChangeStoreError, GenealogyError,
    PredecessorEdge, RelationKind, RevisionVisibility, attach_pending_ai_operation_links,
    link_ai_operation,
};

#[derive(Debug, Error)]
pub enum ChangeRevisionBuildError {
    #[error(transparent)]
    Identity(#[from] super::ChangeIdError),
    #[error(transparent)]
    Store(#[from] ChangeStoreError),
    #[error(transparent)]
    Genealogy(#[from] GenealogyError),
    #[error("change revision commit oid is empty")]
    EmptyCommitOid,
    #[error("repository identity unavailable: {0}")]
    RepositoryIdentity(String),
}

/// Record a revision for a commit produced by a normal or rewrite command.
/// Rewrite callers pass their predecessor OID; when that predecessor already
/// has a projection, its stable Change ID is inherited. Legacy commits without
/// a projection receive a new random ID while retaining the typed edge.
pub async fn record_current_repo_commit_revision(
    op_id: impl Into<String>,
    commit_oid: impl Into<String>,
    predecessor: Option<(String, RelationKind)>,
) -> Result<ChangeRevision, ChangeRevisionBuildError> {
    record_current_repo_commit_revision_with_predecessors(
        op_id,
        commit_oid,
        predecessor.into_iter().collect(),
    )
    .await
}

/// Record one revision with an ordered, possibly multi-edge predecessor set.
/// The first predecessor with a known projection supplies the stable Change
/// ID; callers use this for squash (fold target first, folded commit second),
/// split, and duplicate workflows.
pub async fn record_current_repo_commit_revision_with_predecessors(
    op_id: impl Into<String>,
    commit_oid: impl Into<String>,
    predecessors: Vec<(String, RelationKind)>,
) -> Result<ChangeRevision, ChangeRevisionBuildError> {
    let database = crate::internal::db::get_db_conn_instance().await;
    let repo_id = crate::internal::workspace::RepoIdentity::resolve_or_init(&database)
        .await
        .map_err(|error| ChangeRevisionBuildError::RepositoryIdentity(error.to_string()))?;
    let repo_id = repo_id.to_string();
    let op_id = op_id.into();
    let operation_id = op_id.clone();
    let commit_oid = commit_oid.into();
    let inherited = if let Some((predecessor_oid, _)) = predecessors.first() {
        ChangeStore::new(database.clone())
            .change_id_for_commit(repo_id.as_str(), predecessor_oid)
            .await?
    } else {
        None
    };
    let builder = match inherited {
        Some(change_id) => {
            ChangeRevisionBuilder::for_rewrite(database.clone(), repo_id.as_str(), op_id, change_id)
        }
        None => ChangeRevisionBuilder::for_new_change(database.clone(), repo_id.as_str(), op_id),
    }
    .set_commit_oid(commit_oid);
    let revision = builder.set_predecessors(predecessors).build().await?;
    link_ai_operation(
        &database,
        &AiOperationLink {
            operation_id,
            change_id: revision.change_id,
            session_id: None,
            run_id: None,
            tool_invocation_id: None,
            intent_id: None,
            repo_id: repo_id.clone(),
            worktree_id: None,
            workspace_id: None,
            lease_generation: None,
            config_provenance_digest: None,
            redaction_version: "v1".to_string(),
        },
    )
    .await?;
    let mut operation_ids = std::env::var("LIBRA_AI_PENDING_OPERATION_IDS")
        .ok()
        .into_iter()
        .flat_map(|value| value.split(',').map(str::to_owned).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    if let Ok(operation_id) = std::env::var("LIBRA_AI_OPERATION_ID") {
        operation_ids.push(operation_id);
    }
    let operation_ids = operation_ids.iter().map(String::as_str).collect::<Vec<_>>();
    attach_pending_ai_operation_links(&database, &repo_id, revision.change_id, &operation_ids)
        .await?;
    Ok(revision)
}

/// Record the visible revisions created by a split operation. Each output has
/// its own operation/commit identity and points back to every supplied source
/// commit with the typed `split` relation.
pub async fn record_split_revisions<I>(
    revisions: I,
) -> Result<Vec<ChangeRevision>, ChangeRevisionBuildError>
where
    I: IntoIterator<Item = (String, String, Vec<String>)>,
{
    let mut recorded = Vec::new();
    for (operation_id, commit_oid, predecessors) in revisions {
        let predecessors = predecessors
            .into_iter()
            .map(|oid| (oid, RelationKind::Split))
            .collect();
        recorded.push(
            record_current_repo_commit_revision_with_predecessors(
                operation_id,
                commit_oid,
                predecessors,
            )
            .await?,
        );
    }
    Ok(recorded)
}

/// Record a visible duplicate revision with a typed `duplicate` edge.
pub async fn record_duplicate_revision(
    operation_id: impl Into<String>,
    commit_oid: impl Into<String>,
    predecessor_oid: impl Into<String>,
) -> Result<ChangeRevision, ChangeRevisionBuildError> {
    record_current_repo_commit_revision_with_predecessors(
        operation_id,
        commit_oid,
        vec![(predecessor_oid.into(), RelationKind::Duplicate)],
    )
    .await
}

pub struct ChangeRevisionBuilder {
    db: DatabaseConnection,
    repo_id: String,
    op_id: String,
    change_id: Option<ChangeId>,
    predecessors: Vec<(String, RelationKind)>,
    commit_oid: Option<String>,
    visibility: RevisionVisibility,
    allow_existing_identity: bool,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use super::*;
    use crate::internal::{change::ChangeStore, db};
    use tempfile::tempdir;

    #[tokio::test]
    async fn new_change_duplicate_and_rewrite_identity_contract() {
        let dir = tempdir().expect("tempdir");
        let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
            .await
            .expect("database");
        let first = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-1")
            .set_commit_oid("commit-1")
            .build()
            .await
            .expect("first revision");
        let second = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-2")
            .set_commit_oid("commit-2")
            .build()
            .await
            .expect("second revision");
        assert_ne!(first.change_id, second.change_id);
        assert!(
            ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-duplicate")
                .set_change_id(first.change_id)
                .set_commit_oid("commit-duplicate")
                .build()
                .await
                .is_err()
        );
        let rewritten =
            ChangeRevisionBuilder::for_rewrite(database, "repo", "op-rewrite", first.change_id)
                .set_commit_oid("commit-rewrite")
                .build()
                .await
                .expect("rewrite revision");
        assert_eq!(rewritten.change_id, first.change_id);
    }

    #[tokio::test]
    async fn failed_edge_rolls_back_new_identity_and_revision() {
        let dir = tempdir().expect("tempdir");
        let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
            .await
            .expect("database");
        let change_id = ChangeId::from_str("0123456789abcdef0123456789abcdef").unwrap();
        let result = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "op-fail")
            .set_change_id(change_id)
            .set_commit_oid("commit-fail")
            .set_predecessors([
                (String::from("parent"), RelationKind::Rebase),
                (String::from("parent"), RelationKind::Rebase),
            ])
            .build()
            .await;
        assert!(result.is_err());
        let retry_database = database.clone();
        let store = ChangeStore::new(database);
        assert!(
            store
                .revisions_for_change("repo", change_id)
                .await
                .unwrap()
                .is_empty()
        );
        assert!(
            store
                .change_id_for_commit("repo", "commit-fail")
                .await
                .unwrap()
                .is_none()
        );
        let retry = ChangeRevisionBuilder::for_new_change(retry_database, "repo", "op-retry")
            .set_change_id(change_id)
            .set_commit_oid("commit-retry")
            .build()
            .await
            .expect("identity row must have rolled back");
        assert_eq!(retry.change_id, change_id);
    }
}

impl ChangeRevisionBuilder {
    pub fn for_new_change(
        db: DatabaseConnection,
        repo_id: impl Into<String>,
        op_id: impl Into<String>,
    ) -> Self {
        Self {
            db,
            repo_id: repo_id.into(),
            op_id: op_id.into(),
            change_id: None,
            predecessors: Vec::new(),
            commit_oid: None,
            visibility: RevisionVisibility::Visible,
            allow_existing_identity: false,
        }
    }

    pub fn for_rewrite(
        db: DatabaseConnection,
        repo_id: impl Into<String>,
        op_id: impl Into<String>,
        change_id: ChangeId,
    ) -> Self {
        let mut builder = Self::for_new_change(db, repo_id, op_id);
        builder.change_id = Some(change_id);
        builder.allow_existing_identity = true;
        builder
    }

    pub fn set_change_id(mut self, change_id: ChangeId) -> Self {
        self.change_id = Some(change_id);
        self
    }

    pub fn generate_new_change_id(mut self) -> Result<Self, super::ChangeIdError> {
        self.change_id = Some(ChangeId::generate()?);
        Ok(self)
    }

    pub fn set_commit_oid(mut self, commit_oid: impl Into<String>) -> Self {
        self.commit_oid = Some(commit_oid.into());
        self
    }

    pub fn set_visibility(mut self, visibility: RevisionVisibility) -> Self {
        self.visibility = visibility;
        self
    }

    pub fn set_predecessors<I, S>(mut self, predecessors: I) -> Self
    where
        I: IntoIterator<Item = (S, RelationKind)>,
        S: Into<String>,
    {
        self.predecessors = predecessors
            .into_iter()
            .map(|(oid, relation)| (oid.into(), relation))
            .collect();
        self
    }

    pub async fn build(self) -> Result<ChangeRevision, ChangeRevisionBuildError> {
        let transaction = self.db.begin().await.map_err(ChangeStoreError::Database)?;
        let result = self.build_on(&transaction).await;
        match result {
            Ok(revision) => {
                transaction
                    .commit()
                    .await
                    .map_err(ChangeStoreError::Database)?;
                Ok(revision)
            }
            Err(error) => {
                let _ = transaction.rollback().await;
                Err(error)
            }
        }
    }

    async fn build_on(
        &self,
        database: &DatabaseTransaction,
    ) -> Result<ChangeRevision, ChangeRevisionBuildError> {
        let commit_oid = self
            .commit_oid
            .as_ref()
            .ok_or(ChangeRevisionBuildError::EmptyCommitOid)?
            .clone();
        if commit_oid.trim().is_empty() {
            return Err(ChangeRevisionBuildError::EmptyCommitOid);
        }
        let supplied_change_id = self.change_id;
        let mut change_id = supplied_change_id.unwrap_or(ChangeId::generate()?);
        let revision_ordinal =
            ChangeStore::next_revision_ordinal_on(database, &self.repo_id, change_id).await?;
        // The commit object itself is owned by the caller's Git transaction;
        // this projection is written only after that object is available.
        loop {
            let identity_result = if self.allow_existing_identity {
                ChangeStore::ensure_identity_on(
                    database,
                    &self.repo_id,
                    change_id,
                    "generated",
                    &self.op_id,
                )
                .await
            } else {
                ChangeStore::insert_identity_on(
                    database,
                    &self.repo_id,
                    change_id,
                    "generated",
                    &self.op_id,
                )
                .await
            };
            match identity_result {
                Ok(()) => break,
                Err(ChangeStoreError::Collision(_)) if supplied_change_id.is_none() => {
                    change_id = ChangeId::generate()?;
                }
                Err(error) => return Err(error.into()),
            }
        }
        let revision = ChangeRevision {
            change_id,
            commit_oid: commit_oid.clone(),
            created_op_id: self.op_id.clone(),
            visibility: self.visibility,
            revision_ordinal,
        };
        ChangeStore::insert_revision_on(database, &revision).await?;
        for (ordinal, (predecessor_oid, relation_kind)) in self.predecessors.iter().enumerate() {
            super::genealogy::insert_predecessor_on(
                database,
                &PredecessorEdge {
                    successor_oid: commit_oid.clone(),
                    predecessor_oid: predecessor_oid.clone(),
                    op_id: self.op_id.clone(),
                    relation_kind: *relation_kind,
                    ordinal: ordinal as i32,
                },
            )
            .await?;
        }
        Ok(revision)
    }
}
