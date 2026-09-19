//! Single entry point for new and rewritten Change revisions.

use std::str::FromStr;

use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use thiserror::Error;
use uuid::Uuid;

use super::{
    AiOperationLink, ChangeId, ChangeRevision, ChangeStore, ChangeStoreError, GenealogyError,
    PredecessorEdge, RelationKind, RevisionVisibility, attach_pending_ai_operation_links_on,
    link_ai_operation_on,
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
    #[error("repository mutation has no active operation id")]
    MissingOperationContext,
}

/// Record a revision for a commit produced by a normal or rewrite command.
/// Rewrite callers pass typed predecessor edges; only identity-preserving
/// relations inherit a predecessor's known Change ID. Split and duplicate
/// relations retain genealogy while starting a new Change identity.
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
/// The first identity-preserving predecessor with a known projection supplies
/// the stable Change ID. Split and duplicate edges remain in genealogy but
/// always start a new logical change.
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
    let commit_oid = commit_oid.into();
    let pending_operation_ids = pending_ai_operation_ids();
    build_revision_with_predecessors_with_ai_context(
        database.clone(),
        repo_id.as_str(),
        op_id,
        commit_oid,
        predecessors,
        &pending_operation_ids,
    )
    .await
}

#[cfg(test)]
async fn build_revision_with_predecessors(
    database: DatabaseConnection,
    repo_id: &str,
    op_id: String,
    commit_oid: String,
    predecessors: Vec<(String, RelationKind)>,
) -> Result<ChangeRevision, ChangeRevisionBuildError> {
    let builder =
        revision_builder_for_predecessors(database, repo_id, op_id, commit_oid, predecessors)
            .await?;
    builder.build().await
}

async fn build_revision_with_predecessors_with_ai_context(
    database: DatabaseConnection,
    repo_id: &str,
    op_id: String,
    commit_oid: String,
    predecessors: Vec<(String, RelationKind)>,
    pending_operation_ids: &[String],
) -> Result<ChangeRevision, ChangeRevisionBuildError> {
    let builder =
        revision_builder_for_predecessors(database, repo_id, op_id, commit_oid, predecessors)
            .await?;
    builder.build_with_ai_context(pending_operation_ids).await
}

async fn revision_builder_for_predecessors(
    database: DatabaseConnection,
    repo_id: &str,
    op_id: String,
    commit_oid: String,
    predecessors: Vec<(String, RelationKind)>,
) -> Result<ChangeRevisionBuilder, ChangeRevisionBuildError> {
    revision_builder_for_predecessors_on(
        &database,
        database.clone(),
        repo_id,
        op_id,
        commit_oid,
        predecessors,
    )
    .await
}

async fn revision_builder_for_predecessors_on<C: ConnectionTrait>(
    lookup_database: &C,
    database: DatabaseConnection,
    repo_id: &str,
    op_id: String,
    commit_oid: String,
    predecessors: Vec<(String, RelationKind)>,
) -> Result<ChangeRevisionBuilder, ChangeRevisionBuildError> {
    let inherited_predecessor = predecessors
        .iter()
        .find(|(_, relation_kind)| relation_kind.preserves_change_identity());
    let (inherited, origin) = match inherited_predecessor {
        Some((predecessor_oid, _)) => {
            match ChangeStore::new(database.clone())
                .change_id_for_commit_on(lookup_database, repo_id, predecessor_oid)
                .await?
            {
                Some(change_id) => (Some(change_id), "generated"),
                // ADR-OL-04: a legacy commit without a sidecar projection
                // keeps a stable logical identity across its first rewrite
                // through the deterministic, domain-separated synthetic
                // Change ID derived from its commit OID.
                None => (
                    synthetic_change_id_for_legacy_predecessor(predecessor_oid),
                    "synthetic",
                ),
            }
        }
        None => (None, "generated"),
    };
    let builder = match inherited {
        Some(change_id) => {
            ChangeRevisionBuilder::for_rewrite(database.clone(), repo_id, op_id, change_id)
        }
        None => ChangeRevisionBuilder::for_new_change(database, repo_id, op_id),
    }
    .set_commit_oid(commit_oid)
    .with_identity_origin(origin);
    Ok(builder.set_predecessors(predecessors))
}

fn pending_ai_operation_ids() -> Vec<String> {
    let mut operation_ids = std::env::var("LIBRA_AI_PENDING_OPERATION_IDS")
        .ok()
        .into_iter()
        .flat_map(|value| value.split(',').map(str::to_owned).collect::<Vec<_>>())
        .collect::<Vec<_>>();
    if let Ok(operation_id) = std::env::var("LIBRA_AI_OPERATION_ID") {
        operation_ids.push(operation_id);
    }
    operation_ids
}

async fn link_revision_on<C: ConnectionTrait>(
    database: &C,
    repo_id: &str,
    revision: &ChangeRevision,
    pending_operation_ids: &[String],
) -> Result<(), ChangeRevisionBuildError> {
    link_ai_operation_on(
        database,
        &AiOperationLink {
            operation_id: revision.created_op_id.clone(),
            change_id: revision.change_id,
            session_id: None,
            run_id: None,
            tool_invocation_id: None,
            intent_id: None,
            repo_id: repo_id.to_string(),
            worktree_id: None,
            workspace_id: None,
            lease_generation: None,
            config_provenance_digest: None,
            redaction_version: "v1".to_string(),
        },
    )
    .await?;
    let operation_ids = pending_operation_ids
        .iter()
        .map(String::as_str)
        .collect::<Vec<_>>();
    attach_pending_ai_operation_links_on(database, repo_id, revision.change_id, &operation_ids)
        .await?;
    Ok(())
}

/// Record a revision using the persisted operation boundary active for this command.
///
/// In a pinned repository invocation, missing operation context is an error: a
/// fresh UUID would sever provenance from the operation log. Direct in-process
/// command callers that have no request boundary retain the standalone fallback.
pub async fn record_current_repo_commit_revision_for_active_operation(
    commit_oid: impl Into<String>,
    predecessor: Option<(String, RelationKind)>,
) -> Result<ChangeRevision, ChangeRevisionBuildError> {
    record_current_repo_commit_revision_with_predecessors_for_active_operation(
        commit_oid,
        predecessor.into_iter().collect(),
    )
    .await
}

/// Record an ordered predecessor set using the operation boundary active for this command.
pub async fn record_current_repo_commit_revision_with_predecessors_for_active_operation(
    commit_oid: impl Into<String>,
    predecessors: Vec<(String, RelationKind)>,
) -> Result<ChangeRevision, ChangeRevisionBuildError> {
    let op_id = match crate::internal::operation::current_operation_id() {
        Some(op_id) => op_id,
        None if crate::internal::worktree_scope::WorktreeScope::request_scope().is_some() => {
            return Err(ChangeRevisionBuildError::MissingOperationContext);
        }
        None => {
            // Standalone in-process callers have no operation boundary; a
            // fresh UUID keeps the revision valid but severs provenance from
            // the operation log, so it must stay visible when it happens.
            let generated = Uuid::now_v7().to_string();
            let commit_oid = commit_oid.into();
            tracing::warn!(
                commit = %commit_oid,
                op_id = %generated,
                "change revision recorded without an active operation context; \
                 provenance will not resolve through the operation log"
            );
            return record_current_repo_commit_revision_with_predecessors(
                generated,
                commit_oid,
                predecessors,
            )
            .await;
        }
    };
    record_current_repo_commit_revision_with_predecessors(op_id, commit_oid, predecessors).await
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
    let database = crate::internal::db::get_db_conn_instance().await;
    let repo_id = crate::internal::workspace::RepoIdentity::resolve_or_init(&database)
        .await
        .map_err(|error| ChangeRevisionBuildError::RepositoryIdentity(error.to_string()))?
        .to_string();
    let pending_operation_ids = pending_ai_operation_ids();
    let transaction = crate::internal::db::begin_write_transaction(&database)
        .await
        .map_err(ChangeStoreError::Database)?;
    let result = async {
        let mut recorded = Vec::new();
        for (operation_id, commit_oid, predecessors) in revisions {
            let predecessors = predecessors
                .into_iter()
                .map(|oid| (oid, RelationKind::Split))
                .collect();
            let builder = revision_builder_for_predecessors_on(
                &transaction,
                database.clone(),
                &repo_id,
                operation_id,
                commit_oid,
                predecessors,
            )
            .await?;
            let revision = builder.build_on(&transaction).await?;
            link_revision_on(&transaction, &repo_id, &revision, &pending_operation_ids).await?;
            recorded.push(revision);
        }
        Ok::<_, ChangeRevisionBuildError>(recorded)
    }
    .await;
    match result {
        Ok(recorded) => {
            transaction
                .commit()
                .await
                .map_err(ChangeStoreError::Database)?;
            Ok(recorded)
        }
        Err(error) => {
            let _ = transaction.rollback().await;
            Err(error)
        }
    }
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
    origin: &'static str,
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use tempfile::tempdir;

    use super::*;
    use crate::internal::{change::ChangeStore, db};

    #[tokio::test]
    async fn active_operation_revision_fails_closed_without_a_boundary_id() {
        let root = tempdir().expect("scope root");
        let scope = crate::internal::worktree_scope::RequestScope {
            scope: crate::internal::worktree_scope::WorktreeScope::Main,
            workdir: root.path().to_path_buf(),
            gitdir: root.path().join(".libra"),
            storage: root.path().to_path_buf(),
            worktree_root: root.path().to_path_buf(),
        };

        let error = crate::internal::worktree_scope::with_request_scope(
            Some(scope),
            record_current_repo_commit_revision_for_active_operation(
                "commit-without-boundary",
                None,
            ),
        )
        .await
        .expect_err("a pinned command must not invent operation provenance");
        assert!(matches!(
            error,
            ChangeRevisionBuildError::MissingOperationContext
        ));
    }

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
    async fn split_and_duplicate_edges_start_new_identities_from_registered_sources() {
        let dir = tempdir().expect("tempdir");
        let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
            .await
            .expect("database");
        let source = ChangeRevisionBuilder::for_new_change(database.clone(), "repo", "source-op")
            .set_commit_oid("source-commit")
            .build()
            .await
            .expect("source revision");
        let store = ChangeStore::new(database.clone());
        assert_eq!(
            store
                .change_id_for_commit("repo", "source-commit")
                .await
                .expect("source projection"),
            Some(source.change_id)
        );

        let split = build_revision_with_predecessors(
            database.clone(),
            "repo",
            "split-op".to_string(),
            "split-commit".to_string(),
            vec![("source-commit".to_string(), RelationKind::Split)],
        )
        .await
        .expect("split revision");
        let duplicate = build_revision_with_predecessors(
            database.clone(),
            "repo",
            "duplicate-op".to_string(),
            "duplicate-commit".to_string(),
            vec![("source-commit".to_string(), RelationKind::Duplicate)],
        )
        .await
        .expect("duplicate revision");
        let amended = build_revision_with_predecessors(
            database.clone(),
            "repo",
            "amend-op".to_string(),
            "amended-commit".to_string(),
            vec![("source-commit".to_string(), RelationKind::Amend)],
        )
        .await
        .expect("amended revision");

        assert_ne!(split.change_id, source.change_id);
        assert_ne!(duplicate.change_id, source.change_id);
        assert_eq!(amended.change_id, source.change_id);
        assert_eq!(
            crate::internal::change::evolution_for_commit(&database, "split-commit", 10)
                .await
                .expect("split genealogy")[0]
                .relation_kind,
            RelationKind::Split
        );
        assert_eq!(
            crate::internal::change::evolution_for_commit(&database, "duplicate-commit", 10)
                .await
                .expect("duplicate genealogy")[0]
                .relation_kind,
            RelationKind::Duplicate
        );
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

    #[test]
    fn synthetic_legacy_predecessor_is_deterministic_and_format_separated() {
        let sha1_oid = "0123456789abcdef0123456789abcdef01234567";
        let first = synthetic_change_id_for_legacy_predecessor(sha1_oid).expect("synthetic id");
        let second = synthetic_change_id_for_legacy_predecessor(sha1_oid).expect("synthetic id");
        assert_eq!(first, second);
        // A 64-hex oid is a SHA-256 commit and must not alias the SHA-1
        // synthetic identity for the same bytes.
        let sha256_oid = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let sha256 =
            synthetic_change_id_for_legacy_predecessor(sha256_oid).expect("sha256 synthetic");
        assert_ne!(first, sha256);
        assert!(synthetic_change_id_for_legacy_predecessor("not-hex").is_none());
    }

    #[tokio::test]
    async fn legacy_rewrite_records_the_synthetic_origin() {
        use sea_orm::ConnectionTrait;

        let dir = tempdir().expect("tempdir");
        let database = db::create_database(dir.path().join("repo.db").to_str().unwrap())
            .await
            .expect("database");
        let predecessor =
            ObjectHash::from_str("0123456789abcdef0123456789abcdef01234567").expect("legacy oid");
        let synthetic = synthetic_change_id_for_legacy_predecessor(&predecessor.to_string())
            .expect("synthetic id");
        let rewritten = ChangeRevisionBuilder::for_rewrite(
            database.clone(),
            "repo",
            "op-legacy-rewrite",
            synthetic,
        )
        .with_identity_origin("synthetic")
        .set_commit_oid(predecessor.to_string())
        .build()
        .await
        .expect("legacy rewrite revision");
        assert_eq!(rewritten.change_id, synthetic);

        let row = database
            .query_one_raw(sea_orm::Statement::from_sql_and_values(
                sea_orm::DbBackend::Sqlite,
                "SELECT origin FROM change_identity WHERE change_id = ?",
                [synthetic.to_string().into()],
            ))
            .await
            .expect("identity row");
        assert_eq!(
            row.expect("identity row")
                .try_get::<String>("", "origin")
                .expect("origin column"),
            "synthetic"
        );
    }
}

/// Derive the deterministic synthetic Change ID for a legacy commit that has
/// no sidecar projection (ADR-OL-04). Returns `None` when the OID does not
/// parse as an object hash.
fn synthetic_change_id_for_legacy_predecessor(predecessor_oid: &str) -> Option<ChangeId> {
    ObjectHash::from_str(predecessor_oid)
        .ok()
        .map(|hash| ChangeId::synthetic_for_commit(hash.kind().as_str(), hash.as_ref()))
}

#[allow(clippy::items_after_test_module)]
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
            origin: "generated",
        }
    }

    /// Record the identity as derived from a legacy commit's deterministic
    /// synthetic Change ID (ADR-OL-04) instead of fresh generation.
    pub fn with_identity_origin(mut self, origin: &'static str) -> Self {
        self.origin = origin;
        self
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
        let transaction = crate::internal::db::begin_write_transaction(&self.db)
            .await
            .map_err(ChangeStoreError::Database)?;
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

    async fn build_with_ai_context(
        self,
        pending_operation_ids: &[String],
    ) -> Result<ChangeRevision, ChangeRevisionBuildError> {
        let transaction = crate::internal::db::begin_write_transaction(&self.db)
            .await
            .map_err(ChangeStoreError::Database)?;
        let revision = match self.build_on(&transaction).await {
            Ok(revision) => revision,
            Err(error) => {
                let _ = transaction.rollback().await;
                return Err(error);
            }
        };
        if let Err(error) = link_revision_on(
            &transaction,
            &self.repo_id,
            &revision,
            pending_operation_ids,
        )
        .await
        {
            let _ = transaction.rollback().await;
            return Err(error);
        }
        transaction
            .commit()
            .await
            .map_err(ChangeStoreError::Database)?;
        Ok(revision)
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
                    self.origin,
                    &self.op_id,
                )
                .await
            } else {
                ChangeStore::insert_identity_on(
                    database,
                    &self.repo_id,
                    change_id,
                    self.origin,
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
        Ok(ChangeStore::revision_on(database, change_id, &commit_oid)
            .await?
            .unwrap_or(revision))
    }
}
