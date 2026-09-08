use std::{path::Path, str::FromStr};

use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, QueryResult, Statement};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::{
    policy::{AuthenticatedMemoryContext, REPO_EPISODE_POLICY_VERSION, policy_snapshot_digest},
    tree::load_snapshot,
};
use crate::internal::ai::keyed_digest::RepositoryKeyedDigest;

const VIEW_SCHEMA_VERSION: u32 = 1;
const MAX_FULL_REF_BYTES: usize = 512;
const MAX_WORKTREE_ID_BYTES: usize = 512;

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct FrozenCodeAnchorV1 {
    commit_oid: ObjectHash,
    full_branch_ref: String,
    worktree_id: String,
}

impl FrozenCodeAnchorV1 {
    pub(crate) fn new(
        commit_oid: ObjectHash,
        full_branch_ref: impl Into<String>,
        worktree_id: impl Into<String>,
    ) -> Result<Self, ResolvedMemoryViewError> {
        let full_branch_ref = full_branch_ref.into();
        let worktree_id = worktree_id.into();
        if full_branch_ref.len() > MAX_FULL_REF_BYTES
            || !full_branch_ref.starts_with("refs/heads/")
            || !crate::utils::util::is_valid_refname(&full_branch_ref)
            || worktree_id.len() > MAX_WORKTREE_ID_BYTES
            || worktree_id.trim() != worktree_id
            || worktree_id.chars().any(char::is_control)
        {
            return Err(ResolvedMemoryViewError::new(
                ResolvedMemoryViewErrorKind::InvalidCodeAnchor,
            ));
        }
        Ok(Self {
            commit_oid,
            full_branch_ref,
            worktree_id,
        })
    }

    pub(crate) const fn commit_oid(&self) -> ObjectHash {
        self.commit_oid
    }

    pub(crate) fn full_branch_ref(&self) -> &str {
        &self.full_branch_ref
    }

    pub(crate) fn worktree_id(&self) -> &str {
        &self.worktree_id
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedMemoryViewV1 {
    repository_id: String,
    principal_digest: String,
    code_anchor: FrozenCodeAnchorV1,
    memory_ref_oid: Option<ObjectHash>,
    projection_event_seq: Option<u64>,
    policy_hash: String,
    view_hash: String,
}

impl ResolvedMemoryViewV1 {
    pub(crate) async fn freeze<C: ConnectionTrait>(
        database: &C,
        repository_path: &Path,
        digest: &RepositoryKeyedDigest,
        context: &AuthenticatedMemoryContext,
        code_anchor: FrozenCodeAnchorV1,
    ) -> Result<Self, ResolvedMemoryViewError> {
        validate_identity(database, digest, context).await?;
        let state = read_repo_state(database).await?;
        validate_repo_state(repository_path, state.as_ref())?;
        let principal_digest = digest
            .principal_digest(context.actor().principal_id.as_bytes())
            .map_err(|_| {
                ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::DigestUnavailable)
            })?
            .encoded();
        let policy_hash = policy_snapshot_digest(REPO_EPISODE_POLICY_VERSION).map_err(|_| {
            ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::UnknownPolicy)
        })?;
        let (memory_ref_oid, projection_event_seq) = match state {
            None => (None, None),
            Some(state) => (Some(state.head), Some(state.event_seq)),
        };
        let canonical = CanonicalViewV1 {
            schema_version: VIEW_SCHEMA_VERSION,
            repository_id: context.repository_id(),
            principal_digest: &principal_digest,
            code_commit: code_anchor.commit_oid.to_string(),
            full_branch_ref: code_anchor.full_branch_ref(),
            worktree_id: code_anchor.worktree_id(),
            scope_key: "repo",
            namespace: "default",
            memory_ref_oid: memory_ref_oid.map(|oid| oid.to_string()),
            projection_event_seq,
            policy_hash: &policy_hash,
        };
        let canonical = serde_json::to_vec(&canonical).map_err(|_| {
            ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
        })?;
        let view_hash = format!("sha256:{}", hex::encode(Sha256::digest(canonical)));
        Ok(Self {
            repository_id: context.repository_id().to_string(),
            principal_digest,
            code_anchor,
            memory_ref_oid,
            projection_event_seq,
            policy_hash,
            view_hash,
        })
    }

    pub(crate) async fn revalidate<C: ConnectionTrait>(
        &self,
        database: &C,
        repository_path: &Path,
        digest: &RepositoryKeyedDigest,
        context: &AuthenticatedMemoryContext,
    ) -> Result<(), ResolvedMemoryViewError> {
        validate_identity(database, digest, context).await?;
        if context.repository_id() != self.repository_id
            || digest
                .principal_digest(context.actor().principal_id.as_bytes())
                .map_err(|_| {
                    ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::DigestUnavailable)
                })?
                .encoded()
                != self.principal_digest
        {
            return Err(ResolvedMemoryViewError::new(
                ResolvedMemoryViewErrorKind::Unauthorized,
            ));
        }
        let state = read_repo_state(database).await?;
        validate_repo_state(repository_path, state.as_ref())?;
        let current = state.map(|state| (state.head, state.event_seq));
        if current != self.memory_ref_oid.zip(self.projection_event_seq) {
            return Err(ResolvedMemoryViewError::new(
                ResolvedMemoryViewErrorKind::StaleProjection,
            ));
        }
        Ok(())
    }

    pub(crate) fn repository_id(&self) -> &str {
        &self.repository_id
    }

    pub(crate) fn principal_digest(&self) -> &str {
        &self.principal_digest
    }

    pub(crate) fn code_anchor(&self) -> &FrozenCodeAnchorV1 {
        &self.code_anchor
    }

    pub(crate) const fn code_commit(&self) -> ObjectHash {
        self.code_anchor.commit_oid
    }

    pub(crate) fn full_branch_ref(&self) -> &str {
        &self.code_anchor.full_branch_ref
    }

    pub(crate) fn worktree_id(&self) -> &str {
        &self.code_anchor.worktree_id
    }

    pub(crate) const fn memory_ref_oid(&self) -> Option<ObjectHash> {
        self.memory_ref_oid
    }

    pub(crate) const fn projection_event_seq(&self) -> Option<u64> {
        self.projection_event_seq
    }

    pub(crate) fn policy_hash(&self) -> &str {
        &self.policy_hash
    }

    pub(crate) fn view_hash(&self) -> &str {
        &self.view_hash
    }
}

#[derive(Serialize)]
struct CanonicalViewV1<'a> {
    schema_version: u32,
    repository_id: &'a str,
    principal_digest: &'a str,
    code_commit: String,
    full_branch_ref: &'a str,
    worktree_id: &'a str,
    scope_key: &'static str,
    namespace: &'static str,
    memory_ref_oid: Option<String>,
    projection_event_seq: Option<u64>,
    policy_hash: &'a str,
}

#[derive(Clone, Copy)]
struct RepoProjectionState {
    head: ObjectHash,
    event_seq: u64,
}

async fn validate_identity<C: ConnectionTrait>(
    database: &C,
    digest: &RepositoryKeyedDigest,
    context: &AuthenticatedMemoryContext,
) -> Result<(), ResolvedMemoryViewError> {
    if context.repository_id() != digest.repository_id() {
        return Err(ResolvedMemoryViewError::new(
            ResolvedMemoryViewErrorKind::Unauthorized,
        ));
    }
    digest
        .validate_for_connection(database)
        .await
        .map_err(|_| ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::DigestUnavailable))
}

async fn read_repo_state<C: ConnectionTrait>(
    database: &C,
) -> Result<Option<RepoProjectionState>, ResolvedMemoryViewError> {
    let refs = database
        .query_all_raw(Statement::from_sql_and_values(
            database.get_database_backend(),
            "SELECT `commit` FROM reference
             WHERE kind = 'Branch' AND remote IS NULL
               AND name = 'libra/memory/repo' LIMIT 2",
            [],
        ))
        .await
        .map_err(|_| {
            ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::StorageUnavailable)
        })?;
    if refs.len() > 1 {
        return Err(ResolvedMemoryViewError::new(
            ResolvedMemoryViewErrorKind::CorruptProjection,
        ));
    }
    let projection = database
        .query_one_raw(Statement::from_string(
            database.get_database_backend(),
            "SELECT projected_ref_oid, last_event_seq, schema_version, policy_version
             FROM memory_projection_state WHERE scope_key = 'repo'"
                .to_string(),
        ))
        .await
        .map_err(|_| {
            ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::StorageUnavailable)
        })?;
    match (refs.into_iter().next(), projection) {
        (None, None) => Ok(None),
        (Some(reference), Some(projection)) => decode_repo_state(reference, projection).map(Some),
        _ => Err(ResolvedMemoryViewError::new(
            ResolvedMemoryViewErrorKind::StaleProjection,
        )),
    }
}

fn decode_repo_state(
    reference: QueryResult,
    projection: QueryResult,
) -> Result<RepoProjectionState, ResolvedMemoryViewError> {
    let reference_oid: Option<String> = reference.try_get("", "commit").map_err(|_| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    let reference_oid = reference_oid.ok_or_else(|| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    let projected_oid: String = projection.try_get("", "projected_ref_oid").map_err(|_| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    let event_seq: i64 = projection.try_get("", "last_event_seq").map_err(|_| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    let schema_version: i64 = projection.try_get("", "schema_version").map_err(|_| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    let policy_version: String = projection.try_get("", "policy_version").map_err(|_| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    if reference_oid != projected_oid || schema_version != 1 {
        return Err(ResolvedMemoryViewError::new(
            ResolvedMemoryViewErrorKind::StaleProjection,
        ));
    }
    if policy_version != REPO_EPISODE_POLICY_VERSION {
        return Err(ResolvedMemoryViewError::new(
            ResolvedMemoryViewErrorKind::UnknownPolicy,
        ));
    }
    let head = ObjectHash::from_str(&reference_oid).map_err(|_| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    let event_seq = u64::try_from(event_seq).map_err(|_| {
        ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection)
    })?;
    Ok(RepoProjectionState { head, event_seq })
}

fn validate_repo_state(
    repository_path: &Path,
    state: Option<&RepoProjectionState>,
) -> Result<(), ResolvedMemoryViewError> {
    let Some(state) = state else {
        return Ok(());
    };
    let snapshot = load_snapshot(
        repository_path,
        Some(state.head),
        REPO_EPISODE_POLICY_VERSION,
    )
    .map_err(|_| ResolvedMemoryViewError::new(ResolvedMemoryViewErrorKind::CorruptProjection))?;
    if snapshot.manifest.last_event_seq != state.event_seq {
        return Err(ResolvedMemoryViewError::new(
            ResolvedMemoryViewErrorKind::StaleProjection,
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ResolvedMemoryViewErrorKind {
    InvalidCodeAnchor,
    Unauthorized,
    DigestUnavailable,
    StorageUnavailable,
    StaleProjection,
    CorruptProjection,
    UnknownPolicy,
}

#[derive(Debug, Error)]
#[error("cannot freeze or validate Memory view ({kind:?})")]
pub(crate) struct ResolvedMemoryViewError {
    kind: ResolvedMemoryViewErrorKind,
}

impl ResolvedMemoryViewError {
    const fn new(kind: ResolvedMemoryViewErrorKind) -> Self {
        Self { kind }
    }

    pub(crate) const fn kind(&self) -> ResolvedMemoryViewErrorKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::ConnectionTrait;

    use super::*;
    use crate::internal::ai::memory::{
        domain::{ActorKind, ActorRefV1},
        policy::AuthenticatedMemoryContext,
        writer::tests::{fixture, proposal},
    };

    const CODE_OID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    #[tokio::test]
    async fn frozen_view_is_stable_and_rejects_identity_or_watermark_changes() {
        let fixture = fixture().await;
        let anchor = FrozenCodeAnchorV1::new(
            CODE_OID.parse().expect("fixed code OID"),
            "refs/heads/main",
            "",
        )
        .expect("valid code anchor");
        let empty = ResolvedMemoryViewV1::freeze(
            fixture.database.as_ref(),
            fixture._temp.path(),
            fixture.digest.as_ref(),
            &fixture.context,
            anchor.clone(),
        )
        .await
        .expect("freeze empty Memory view");
        let repeated = ResolvedMemoryViewV1::freeze(
            fixture.database.as_ref(),
            fixture._temp.path(),
            fixture.digest.as_ref(),
            &fixture.context,
            anchor.clone(),
        )
        .await
        .expect("repeat identical freeze");
        assert_eq!(empty, repeated);
        let linked_worktree = ResolvedMemoryViewV1::freeze(
            fixture.database.as_ref(),
            fixture._temp.path(),
            fixture.digest.as_ref(),
            &fixture.context,
            FrozenCodeAnchorV1::new(
                CODE_OID.parse().expect("fixed code OID"),
                "refs/heads/main",
                "wt-linked",
            )
            .expect("valid linked-worktree anchor"),
        )
        .await
        .expect("freeze linked-worktree view");
        assert_ne!(empty.view_hash(), linked_worktree.view_hash());

        let other_context = AuthenticatedMemoryContext::new(
            fixture.context.repository_id(),
            ActorRefV1 {
                kind: ActorKind::Agent,
                principal_id: "agent:other-reader".to_string(),
            },
        )
        .expect("construct other principal");
        let other = ResolvedMemoryViewV1::freeze(
            fixture.database.as_ref(),
            fixture._temp.path(),
            fixture.digest.as_ref(),
            &other_context,
            anchor,
        )
        .await
        .expect("freeze other principal view");
        assert_ne!(empty.view_hash(), other.view_hash());
        assert_eq!(
            empty
                .revalidate(
                    fixture.database.as_ref(),
                    fixture._temp.path(),
                    fixture.digest.as_ref(),
                    &other_context,
                )
                .await
                .expect_err("a frozen principal cannot be replaced")
                .kind(),
            ResolvedMemoryViewErrorKind::Unauthorized,
        );

        fixture
            .writer
            .commit(
                &fixture.context,
                &fixture.target,
                &proposal(&fixture.target, fixture.key_id, 1),
                None,
            )
            .await
            .expect("advance Memory history");
        assert_eq!(
            empty
                .revalidate(
                    fixture.database.as_ref(),
                    fixture._temp.path(),
                    fixture.digest.as_ref(),
                    &fixture.context,
                )
                .await
                .expect_err("an advanced projection invalidates the frozen view")
                .kind(),
            ResolvedMemoryViewErrorKind::StaleProjection,
        );

        fixture
            .database
            .execute_unprepared(
                "UPDATE memory_projection_state SET policy_version = 'unknown-policy'",
            )
            .await
            .expect("corrupt projected policy for fail-closed test");
        let error = ResolvedMemoryViewV1::freeze(
            fixture.database.as_ref(),
            fixture._temp.path(),
            fixture.digest.as_ref(),
            &fixture.context,
            FrozenCodeAnchorV1::new(
                CODE_OID.parse().expect("fixed code OID"),
                "refs/heads/main",
                "",
            )
            .expect("valid code anchor"),
        )
        .await
        .expect_err("unknown policy must not use projected rows");
        assert_eq!(error.kind(), ResolvedMemoryViewErrorKind::UnknownPolicy);
    }
}
