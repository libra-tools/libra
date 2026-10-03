//! Local sensitive catalog associations, distinct from recovery/source wire.
//! Registry MACs authenticate an association; they do not grant source I/O.
//! No Debug/Serialize implementation is exposed for resolved catalog context.

use std::{path::Path, time::Instant};

use anyhow::{Result, ensure};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};
use serde::{Deserialize, Serialize};
use uuid::{Uuid, Variant, Version};

use crate::internal::{
    ai::{
        capture::catalog::{
            CaptureCatalogError, PendingSessionContext, resolve_pending_session_context,
        },
        capture_scope::CaptureScope,
    },
    metadata::{MetadataKv, MetadataScope},
    workspace::{RepoIdentity, WorkspaceError},
};

const VERSION: u8 = 1;
pub(crate) const MAX_ALIAS_BYTES: usize = 8 * 1024;
const MAX_ALIASES: usize = 16;
const REMEDY: &str = "capture session association cannot be trusted; run `libra agent doctor`";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkerIdentityError {
    Retryable,
    Invalid,
}

/// A concurrent writer already published a different alias for the same
/// catalog PK/incarnation. The winner stays authoritative; the loser rolls
/// back without publishing and retries. This is not association corruption.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("capture session association was published concurrently; retry the same native event")]
pub(crate) struct PendingAliasConflict;

pub(crate) fn transient_io_failure(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
            | std::io::ErrorKind::TimedOut
    ) {
        return true;
    }
    #[cfg(unix)]
    return matches!(
        error.raw_os_error(),
        Some(libc::EIO | libc::EBUSY | libc::ENFILE | libc::EMFILE | libc::ENOMEM)
    );
    #[cfg(not(unix))]
    false
}

fn transient_identity_error(error: &anyhow::Error, deadline: Instant) -> bool {
    Instant::now() >= deadline
        || error.chain().any(|cause| {
            cause.is::<sea_orm::DbErr>()
                || cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(transient_io_failure)
                || cause
                    .downcast_ref::<WorkspaceError>()
                    .is_some_and(|error| {
                        matches!(error, WorkspaceError::ReadFailed(_) | WorkspaceError::LeaseLost { .. })
                    })
                || cause
                    .downcast_ref::<CaptureCatalogError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            CaptureCatalogError::Database
                                | CaptureCatalogError::TransactionStart
                                | CaptureCatalogError::CommitFailed
                                | CaptureCatalogError::DeadlineExceeded
                                | CaptureCatalogError::SchemaUnavailable
                                | CaptureCatalogError::WorkspaceLeaseRejected
                        )
                    })
                || cause
                    .downcast_ref::<crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
                                | crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
                        )
                    })
        })
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AliasBody {
    version: u8,
    alias: String,
    session_id: String,
    repo_id: String,
    worktree_id: String,
    workspace_id: Option<String>,
    capture_incarnation: Option<String>,
}

impl AliasBody {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSION && canonical_alias(&self.alias),
            REMEDY
        );
        ensure!(
            !self.session_id.is_empty()
                && self.session_id.len() <= 1024
                && !self.session_id.chars().any(char::is_control),
            REMEDY
        );
        ensure!(
            bounded_identity(&self.repo_id, false)
                && bounded_identity(&self.worktree_id, true)
                && self
                    .workspace_id
                    .as_deref()
                    .is_none_or(|id| bounded_identity(id, false)),
            REMEDY
        );
        ensure!(
            self.capture_incarnation
                .as_deref()
                .is_none_or(|value| value.len() == 32
                    && value
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))),
            REMEDY
        );
        Ok(())
    }

    fn matches(&self, context: &PendingSessionContext) -> bool {
        self.session_id == context.session_id()
            && self.capture_incarnation.as_deref() == context.incarnation()
            && self.repo_id == context.scope().repo_id
            && self.worktree_id == context.scope().worktree_id
            && self.workspace_id == context.scope().workspace_id
    }
}

fn bounded_identity(value: &str, empty_allowed: bool) -> bool {
    (empty_allowed || !value.is_empty())
        && value.len() <= 512
        && !value.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn canonical_alias(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|uuid| {
        uuid.get_version() == Some(Version::Random)
            && uuid.get_variant() == Variant::RFC4122
            && uuid.to_string() == value
    })
}

fn preserve_transient_association_error(error: anyhow::Error) -> anyhow::Error {
    let retryable = error.chain().any(|cause| {
        cause.is::<sea_orm::DbErr>()
                || cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(transient_io_failure)
            || cause.downcast_ref::<WorkspaceError>().is_some_and(|error| {
                matches!(
                    error,
                    WorkspaceError::ReadFailed(_) | WorkspaceError::LeaseLost { .. }
                )
            })
            || cause
                .downcast_ref::<crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
                            | crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
                    )
                })
            || cause
                .downcast_ref::<CaptureCatalogError>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        CaptureCatalogError::Database
                            | CaptureCatalogError::TransactionStart
                            | CaptureCatalogError::CommitFailed
                            | CaptureCatalogError::DeadlineExceeded
                            | CaptureCatalogError::SchemaUnavailable
                            | CaptureCatalogError::WorkspaceLeaseRejected
                    )
                })
    });
    if retryable {
        error
    } else {
        anyhow::anyhow!(REMEDY)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingSessionAlias {
    body: AliasBody,
    mac: String,
}

/// Only successful existing-key authentication constructs this publication
/// capability. A decoded registry row alone cannot be published.
pub(crate) struct PreparedPendingAlias {
    record: PendingSessionAlias,
    context: PendingSessionContext,
}

impl PreparedPendingAlias {
    pub(crate) fn alias(&self) -> &str {
        self.record.alias()
    }

    pub(crate) fn context(&self) -> &PendingSessionContext {
        &self.context
    }

    pub(crate) fn matches_authenticated_record(&self, record: &PendingSessionAlias) -> bool {
        self.record == *record
    }

    /// Artifact first, association second, in the caller's ONE transaction.
    /// The writer lock is acquired before reverse lookup even if the caller
    /// began a deferred transaction. Missing artifacts cannot leave aliases.
    pub(crate) async fn publish_for_artifact(
        &self,
        txn: &DatabaseTransaction,
        checkpoint_id: &str,
    ) -> Result<()> {
        ensure!(
            Uuid::parse_str(checkpoint_id).is_ok_and(|id| id.to_string() == checkpoint_id),
            REMEDY
        );
        txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
            .await
            .map_err(|error| preserve_transient_association_error(error.into()))?;
        let current =
            resolve_pending_session_context(txn, self.context.scope(), self.context.session_id())
                .await
                .map_err(|error| preserve_transient_association_error(error.into()))?;
        ensure!(current == self.context, REMEDY);
        let existing = retained_alias(txn, &current).await?;
        if let Some(existing) = &existing {
            // Losing the INSERT-only mint race is a typed retryable conflict;
            // only a same-alias record mismatch is untrusted association data.
            if existing.alias() != self.alias() {
                return Err(PendingAliasConflict.into());
            }
            ensure!(*existing == self.record, REMEDY);
        }
        let header_rows = txn
            .query_all_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT CASE WHEN length(CAST(value AS BLOB)) <= 8192 THEN value ELSE NULL END
               AS bounded_value FROM metadata_kv
             WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
               AND target = ? AND key = ? LIMIT 2",
                [current.scope().repo_id.clone().into(), checkpoint_id.into()],
            ))
            .await
            .map_err(|error| preserve_transient_association_error(error.into()))?;
        ensure!(header_rows.len() == 1, REMEDY);
        let text: Option<String> = header_rows[0]
            .try_get_by("bounded_value")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let header =
            super::pending::PendingHeader::decode(&text.ok_or_else(|| anyhow::anyhow!(REMEDY))?)
                .map_err(|_| anyhow::anyhow!(REMEDY))?;
        ensure!(
            header.binding.session_id == self.alias()
                && header.binding.checkpoint_id == checkpoint_id
                && header.binding.scope == *current.scope(),
            REMEDY
        );
        if existing.is_none() {
            let count = txn
                .query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = ? AND target = ?",
                    [
                        MetadataScope::AgentCaptureSessionAlias.as_str().into(),
                        current.scope().repo_id.clone().into(),
                    ],
                ))
                .await
                .map_err(|error| preserve_transient_association_error(error.into()))?
                .ok_or_else(|| anyhow::anyhow!(REMEDY))?
                .try_get_by::<i64, _>("n")
                .map_err(|_| anyhow::anyhow!(REMEDY))?;
            ensure!((0..MAX_ALIASES as i64).contains(&count), REMEDY);
            let now = chrono::Utc::now().to_rfc3339();
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO metadata_kv (scope, target, key, value, value_type, created_at, updated_at)
                 VALUES (?, ?, ?, ?, 'text', ?, ?)",
                [MetadataScope::AgentCaptureSessionAlias.as_str().into(),
                 current.scope().repo_id.clone().into(), self.alias().into(),
                 self.record.encode()?.into(), now.clone().into(), now.into()],
            )).await.map_err(|error| preserve_transient_association_error(error.into()))?;
        }
        Ok(())
    }
}

impl PendingSessionAlias {
    pub(crate) fn alias(&self) -> &str {
        &self.body.alias
    }

    fn decode(text: &str, repo_id: &str, alias: &str) -> Result<Self> {
        ensure!(
            text.len() <= MAX_ALIAS_BYTES && canonical_alias(alias),
            REMEDY
        );
        let record: Self = serde_json::from_str(text).map_err(|_| anyhow::anyhow!(REMEDY))?;
        record.body.validate()?;
        ensure!(
            record.body.repo_id == repo_id
                && record.body.alias == alias
                && record
                    .mac
                    .strip_prefix("pending-alias/hmac-v1/")
                    .is_some_and(|tag| tag.len() == 64
                        && tag
                            .bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)))
                && serde_json::to_string(&record).map_err(|_| anyhow::anyhow!(REMEDY))? == text,
            REMEDY
        );
        Ok(record)
    }

    pub(crate) fn encode(&self) -> Result<String> {
        self.body.validate()?;
        let text = serde_json::to_string(self).map_err(|_| anyhow::anyhow!(REMEDY))?;
        ensure!(text.len() <= MAX_ALIAS_BYTES, REMEDY);
        Ok(text)
    }

    /// Used only after the writer-locked catalog has selected its candidate.
    /// Signing occurs outside that transaction; publication revalidates it.
    pub(crate) async fn prepare(
        conn: &DatabaseConnection,
        context: &PendingSessionContext,
        existing: Option<Self>,
        storage: &Path,
        root: &Path,
        deadline: Instant,
    ) -> Result<PreparedPendingAlias> {
        let body = existing.as_ref().map_or_else(
            || AliasBody {
                version: VERSION,
                alias: Uuid::new_v4().to_string(),
                session_id: context.session_id().to_owned(),
                repo_id: context.scope().repo_id.clone(),
                worktree_id: context.scope().worktree_id.clone(),
                workspace_id: context.scope().workspace_id.clone(),
                capture_incarnation: context.incarnation().map(str::to_owned),
            },
            |record| record.body.clone(),
        );
        body.validate()?;
        ensure!(body.matches(context), REMEDY);
        let bytes = serde_json::to_vec(&body).map_err(|_| anyhow::anyhow!(REMEDY))?;
        let mac = super::key::authenticate_pending_alias_in_scope_until(
            conn,
            context.scope(),
            storage,
            root,
            &bytes,
            existing.as_ref().map(|record| record.mac.as_str()),
            deadline,
        )
        .await
        .map_err(preserve_transient_association_error)?;
        let record = Self { body, mac };
        record.encode()?;
        Ok(PreparedPendingAlias {
            record,
            context: context.clone(),
        })
    }

    /// Authenticate before selecting the sensitive catalog PK. A current
    /// lease/scope/incarnation is independently required, not supplied by MAC.
    /// Use a READ transaction here: ACF-12 must repeat the catalog/current
    /// receipt fences in its writer transaction, without doing key I/O there.
    pub(crate) async fn resolve(
        &self,
        txn: &DatabaseTransaction,
        scope: &CaptureScope,
        storage: &Path,
        root: &Path,
        deadline: Instant,
    ) -> Result<PendingSessionContext> {
        ensure!(
            self.body.repo_id == scope.repo_id
                && self.body.worktree_id == scope.worktree_id
                && self.body.workspace_id == scope.workspace_id,
            REMEDY
        );
        let bytes = serde_json::to_vec(&self.body).map_err(|_| anyhow::anyhow!(REMEDY))?;
        super::key::authenticate_pending_alias_in_scope_until(
            txn,
            scope,
            storage,
            root,
            &bytes,
            Some(&self.mac),
            deadline,
        )
        .await
        .map_err(preserve_transient_association_error)?;
        let context = resolve_pending_session_context(txn, scope, &self.body.session_id)
            .await
            .map_err(|error| preserve_transient_association_error(error.into()))?;
        ensure!(self.body.matches(&context), REMEDY);
        Ok(context)
    }

    /// Worker-specific classification keeps storage/database failures in the
    /// pending queue while allowing authenticated association corruption to be
    /// retained in quarantine.
    pub(crate) async fn resolve_for_worker(
        &self,
        txn: &DatabaseTransaction,
        scope: &CaptureScope,
        storage: &Path,
        root: &Path,
        deadline: Instant,
    ) -> Result<PendingSessionContext, WorkerIdentityError> {
        if self.body.repo_id != scope.repo_id
            || self.body.worktree_id != scope.worktree_id
            || self.body.workspace_id != scope.workspace_id
        {
            return Err(WorkerIdentityError::Invalid);
        }
        let bytes = serde_json::to_vec(&self.body).map_err(|_| WorkerIdentityError::Invalid)?;
        super::key::authenticate_pending_alias_in_scope_until(
            txn,
            scope,
            storage,
            root,
            &bytes,
            Some(&self.mac),
            deadline,
        )
        .await
        .map_err(|error| {
            if transient_identity_error(&error, deadline) {
                WorkerIdentityError::Retryable
            } else {
                WorkerIdentityError::Invalid
            }
        })?;
        let context = resolve_pending_session_context(txn, scope, &self.body.session_id)
            .await
            .map_err(|error| match error {
                crate::internal::ai::capture::catalog::CaptureCatalogError::Database
                | crate::internal::ai::capture::catalog::CaptureCatalogError::TransactionStart
                | crate::internal::ai::capture::catalog::CaptureCatalogError::CommitFailed
                | crate::internal::ai::capture::catalog::CaptureCatalogError::DeadlineExceeded
                | crate::internal::ai::capture::catalog::CaptureCatalogError::SchemaUnavailable
                | crate::internal::ai::capture::catalog::CaptureCatalogError::WorkspaceLeaseRejected => {
                    WorkerIdentityError::Retryable
                }
                _ => WorkerIdentityError::Invalid,
            })?;
        if !self.body.matches(&context) {
            return Err(WorkerIdentityError::Invalid);
        }
        Ok(context)
    }

    pub(crate) fn matches_context(&self, context: &PendingSessionContext) -> bool {
        self.body.matches(context)
    }
}

pub(crate) async fn assert_private_repo<C: ConnectionTrait>(conn: &C, repo_id: &str) -> Result<()> {
    let canonical = RepoIdentity::resolve(conn)
        .await
        .map_err(|error| preserve_transient_association_error(error.into()))?;
    ensure!(canonical.as_str() == repo_id, REMEDY);
    let foreign = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM metadata_kv WHERE scope IN (
           'agent_capture_pending', 'agent_capture_quarantine',
           'agent_capture_pending_chunk', 'agent_capture_session_alias'
         ) AND target <> ? LIMIT 1",
            [repo_id.into()],
        ))
        .await
        .map_err(|error| preserve_transient_association_error(error.into()))?;
    ensure!(foreign.is_none(), REMEDY);
    Ok(())
}

/// GC checks only targets, never association bodies or integrity keys. Old
/// databases with no private rows need no capture identity initialization.
pub(crate) async fn assert_private_targets_for_gc<C: ConnectionTrait>(conn: &C) -> Result<()> {
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT CASE WHEN length(CAST(target AS BLOB)) <= 512
           THEN target ELSE NULL END AS bounded_target FROM metadata_kv
         WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine',
           'agent_capture_pending_chunk', 'agent_capture_session_alias') LIMIT 1",
            [],
        ))
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    if let Some(row) = row {
        let target: Option<String> = row
            .try_get_by("bounded_target")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        assert_private_repo(conn, &target.ok_or_else(|| anyhow::anyhow!(REMEDY))?).await?;
    }
    Ok(())
}

/// One indexed association lookup; cap its bytes in SQL before hydration.
pub(crate) async fn lookup<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    alias: &str,
) -> Result<Option<PendingSessionAlias>> {
    ensure!(canonical_alias(alias), REMEDY);
    assert_private_repo(conn, repo_id)
        .await
        .map_err(preserve_transient_association_error)?;
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT value_type, CASE WHEN length(CAST(value AS BLOB)) <= 8192
           THEN value ELSE NULL END AS bounded_value FROM metadata_kv
         WHERE scope = ? AND target = ? AND key = ? LIMIT 1",
            [
                MetadataScope::AgentCaptureSessionAlias.as_str().into(),
                repo_id.into(),
                alias.into(),
            ],
        ))
        .await
        .map_err(|error| preserve_transient_association_error(error.into()))?;
    let Some(row) = row else {
        return Ok(None);
    };
    let value: Option<String> = row
        .try_get_by("bounded_value")
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    let kind: String = row
        .try_get_by("value_type")
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    ensure!(kind == "text", REMEDY);
    Ok(Some(PendingSessionAlias::decode(
        &value.ok_or_else(|| anyhow::anyhow!(REMEDY))?,
        repo_id,
        alias,
    )?))
}

pub(crate) async fn lookup_for_worker<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    alias: &str,
) -> std::result::Result<Option<PendingSessionAlias>, WorkerIdentityError> {
    if !canonical_alias(alias) {
        return Err(WorkerIdentityError::Invalid);
    }
    let canonical = RepoIdentity::resolve(conn).await.map_err(|error| {
        if matches!(error, WorkspaceError::ReadFailed(_)) {
            WorkerIdentityError::Retryable
        } else {
            WorkerIdentityError::Invalid
        }
    })?;
    if canonical.as_str() != repo_id {
        return Err(WorkerIdentityError::Invalid);
    }
    let foreign = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM metadata_kv WHERE scope IN (
           'agent_capture_pending', 'agent_capture_quarantine',
           'agent_capture_pending_chunk', 'agent_capture_session_alias'
         ) AND target <> ? LIMIT 1",
            [repo_id.into()],
        ))
        .await
        .map_err(|_| WorkerIdentityError::Retryable)?;
    if foreign.is_some() {
        return Err(WorkerIdentityError::Invalid);
    }
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT value_type, CASE WHEN length(CAST(value AS BLOB)) <= 8192
           THEN value ELSE NULL END AS bounded_value FROM metadata_kv
         WHERE scope = ? AND target = ? AND key = ? LIMIT 1",
            [
                MetadataScope::AgentCaptureSessionAlias.as_str().into(),
                repo_id.into(),
                alias.into(),
            ],
        ))
        .await
        .map_err(|_| WorkerIdentityError::Retryable)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let value: Option<String> = row
        .try_get_by("bounded_value")
        .map_err(|_| WorkerIdentityError::Invalid)?;
    let kind: String = row
        .try_get_by("value_type")
        .map_err(|_| WorkerIdentityError::Invalid)?;
    if kind != "text" {
        return Err(WorkerIdentityError::Invalid);
    }
    let record =
        PendingSessionAlias::decode(&value.ok_or(WorkerIdentityError::Invalid)?, repo_id, alias)
            .map_err(|_| WorkerIdentityError::Invalid)?;
    Ok(Some(record))
}

/// Call only under the catalog's writer lock, before minting an alias. No
/// session scan or rowid association; at most 17 bounded private rows.
pub(crate) async fn retained_alias(
    txn: &DatabaseTransaction,
    context: &PendingSessionContext,
) -> Result<Option<PendingSessionAlias>> {
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .map_err(|error| preserve_transient_association_error(error.into()))?;
    assert_private_repo(txn, &context.scope().repo_id)
        .await
        .map_err(preserve_transient_association_error)?;
    // Key and value bytes are capped in SQL. A type-confused row hydrates as
    // NULL and fails with the content-free remedy, never a retryable DbErr.
    let rows = txn.query_all_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT CASE WHEN typeof(key) = 'text' AND length(CAST(key AS BLOB)) <= 36 THEN CAST(key AS BLOB) ELSE NULL END AS bounded_key,
           CASE WHEN value_type = 'text' AND typeof(value) = 'text' AND length(CAST(value AS BLOB)) <= 8192 THEN CAST(value AS BLOB) ELSE NULL END AS bounded_value
           FROM metadata_kv WHERE scope = ? AND target = ? ORDER BY key LIMIT 17",
        [MetadataScope::AgentCaptureSessionAlias.as_str().into(), context.scope().repo_id.clone().into()],
    )).await.map_err(|error| preserve_transient_association_error(error.into()))?;
    ensure!(rows.len() <= MAX_ALIASES, REMEDY);
    let mut found = None;
    for row in rows {
        let key: Option<Vec<u8>> = row
            .try_get_by("bounded_key")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let value: Option<Vec<u8>> = row
            .try_get_by("bounded_value")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let (key, value) = key
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .zip(
                value
                    .as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok()),
            )
            .ok_or_else(|| anyhow::anyhow!(REMEDY))?;
        let record = PendingSessionAlias::decode(value, &context.scope().repo_id, key)?;
        if record.matches_context(context) {
            ensure!(found.is_none(), REMEDY);
            found = Some(record);
        }
    }
    Ok(found)
}

pub(crate) struct ErasureAliases {
    aliases: Vec<String>,
    unassigned: bool,
    repo_id: String,
    session_id: String,
    incarnation: Option<String>,
    other_incarnation: bool,
}

impl ErasureAliases {
    pub(crate) fn aliases(&self) -> &[String] {
        &self.aliases
    }
    pub(crate) fn has_unassigned(&self) -> bool {
        self.unassigned
    }
    pub(crate) fn has_other_incarnation(&self) -> bool {
        self.other_incarnation
    }
}

/// Deletion is not replay: no MAC/key/receipt proof is required here. The
/// caller owns the existing tombstone-fenced catalog transaction. Foreign
/// rows are retained rather than blocking another session's explicit erase.
pub(crate) async fn aliases_for_erasure(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
    session_id: &str,
    incarnation: Option<&str>,
) -> Result<ErasureAliases> {
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    let repo = RepoIdentity::resolve(txn)
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    ensure!(repo.as_str() == scope.repo_id, REMEDY);
    let rows = txn.query_all_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT CASE WHEN typeof(key) = 'text' AND length(CAST(key AS BLOB)) <= 36 THEN CAST(key AS BLOB) ELSE NULL END AS bounded_key,
           CASE WHEN value_type = 'text' AND typeof(value) = 'text' AND length(CAST(value AS BLOB)) <= 8192 THEN CAST(value AS BLOB) ELSE NULL END AS bounded_value
           FROM metadata_kv WHERE scope = ? AND target = ? ORDER BY key LIMIT 17",
        [MetadataScope::AgentCaptureSessionAlias.as_str().into(), scope.repo_id.clone().into()],
    )).await.map_err(|_| anyhow::anyhow!(REMEDY))?;
    let mut result = ErasureAliases {
        aliases: Vec::new(),
        unassigned: rows.len() > MAX_ALIASES,
        repo_id: scope.repo_id.clone(),
        session_id: session_id.to_owned(),
        incarnation: incarnation.map(str::to_owned),
        other_incarnation: false,
    };
    for row in rows {
        let key: Option<Vec<u8>> = row
            .try_get_by("bounded_key")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let value: Option<Vec<u8>> = row
            .try_get_by("bounded_value")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let record = key
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .zip(
                value
                    .as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok()),
            )
            .and_then(|(key, value)| PendingSessionAlias::decode(value, &scope.repo_id, key).ok());
        if let Some(record) = record {
            let body = &record.body;
            if body.session_id == session_id {
                if body.capture_incarnation.as_deref() == incarnation {
                    result.aliases.push(body.alias.clone());
                } else {
                    result.other_incarnation = true;
                }
            }
        } else {
            result.unassigned = true;
        }
    }
    Ok(result)
}

/// Explicit erase retires EVERY attributed association with the session row,
/// even if unrelated corrupt headers remain. Unlike completion, erase never
/// leaves a sensitive catalog FK waiting for all repo headers to be repaired.
/// The caller must use this in its tombstone-fenced final catalog transaction.
pub(crate) async fn erase_attributed_aliases(
    txn: &DatabaseTransaction,
    owned: &ErasureAliases,
) -> Result<()> {
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    let repo = RepoIdentity::resolve(txn)
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    ensure!(repo.as_str() == owned.repo_id, REMEDY);
    for alias in &owned.aliases {
        let row = txn.query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT CASE WHEN length(CAST(value AS BLOB)) <= 8192 THEN value ELSE NULL END AS bounded_value,
               value_type FROM metadata_kv WHERE scope = ? AND target = ? AND key = ? LIMIT 1",
            [MetadataScope::AgentCaptureSessionAlias.as_str().into(), owned.repo_id.clone().into(), alias.clone().into()],
        )).await.map_err(|_| anyhow::anyhow!(REMEDY))?;
        let Some(row) = row else {
            continue;
        };
        let value: Option<String> = row
            .try_get_by("bounded_value")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let kind: String = row
            .try_get_by("value_type")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        ensure!(kind == "text", REMEDY);
        let record = PendingSessionAlias::decode(
            &value.ok_or_else(|| anyhow::anyhow!(REMEDY))?,
            &owned.repo_id,
            alias,
        )?;
        ensure!(
            record.body.session_id == owned.session_id
                && record.body.capture_incarnation == owned.incarnation,
            REMEDY
        );
        MetadataKv::unset_with_conn(
            txn,
            MetadataScope::AgentCaptureSessionAlias,
            &owned.repo_id,
            alias,
        )
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    }
    Ok(())
}

/// A bounded independent attribution probe may ignore damaged non-ownership
/// fields, but duplicate keys and ambiguous alias/checkpoint ownership fail
/// closed. This grants only deletion attribution, never replay authority.
pub(crate) fn header_alias_attribution(text: &str, checkpoint_key: &str) -> Option<String> {
    use crate::internal::ai::observed_agents::coverage::CanonValue;
    if text.len() > MAX_ALIAS_BYTES {
        return None;
    }
    let uuid = Uuid::parse_str(checkpoint_key).ok()?;
    if uuid.to_string() != checkpoint_key {
        return None;
    }
    let value = crate::internal::ai::observed_agents::parse_canon_value(text.as_bytes()).ok()?;
    let CanonValue::Object(header) = value else {
        return None;
    };
    let CanonValue::Object(binding) = header.get("binding")? else {
        return None;
    };
    let alias = binding.get("session_id")?.as_str()?;
    if !canonical_alias(alias) || binding.get("checkpoint_id")?.as_str()? != checkpoint_key {
        return None;
    }
    Some(alias.to_owned())
}

/// Remove only after the caller removed the owned artifact in this same
/// transaction. Unattributable headers retain the association conservatively.
pub(crate) async fn remove_alias_if_unreferenced(
    txn: &DatabaseTransaction,
    repo_id: &str,
    alias: &str,
) -> Result<bool> {
    ensure!(canonical_alias(alias), REMEDY);
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    let repo = RepoIdentity::resolve(txn)
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    ensure!(repo.as_str() == repo_id, REMEDY);
    let rows = txn.query_all_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT CASE WHEN typeof(key) = 'text' AND length(CAST(key AS BLOB)) <= 36 THEN CAST(key AS BLOB) ELSE NULL END AS bounded_key,
           CASE WHEN typeof(value) = 'text' AND length(CAST(value AS BLOB)) <= 8192 THEN CAST(value AS BLOB) ELSE NULL END AS bounded_value
         FROM metadata_kv WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
           AND target = ? ORDER BY key LIMIT 17", [repo_id.into()],
    )).await.map_err(|_| anyhow::anyhow!(REMEDY))?;
    if rows.len() > MAX_ALIASES {
        return Ok(false);
    }
    for row in rows {
        let key: Option<Vec<u8>> = row
            .try_get_by("bounded_key")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let text: Option<Vec<u8>> = row
            .try_get_by("bounded_value")
            .map_err(|_| anyhow::anyhow!(REMEDY))?;
        let Some(attribution) = key
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .zip(
                text.as_deref()
                    .and_then(|bytes| std::str::from_utf8(bytes).ok()),
            )
            .and_then(|(key, text)| header_alias_attribution(text, key))
        else {
            return Ok(false);
        };
        if attribution == alias {
            return Ok(false);
        }
    }
    MetadataKv::unset_with_conn(txn, MetadataScope::AgentCaptureSessionAlias, repo_id, alias)
        .await
        .map_err(|_| anyhow::anyhow!(REMEDY))?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::{path::PathBuf, time::Duration};

    #[cfg(unix)]
    use sea_orm::{DatabaseConnection, TransactionTrait};
    #[cfg(unix)]
    use tempfile::TempDir;

    use super::*;
    #[cfg(unix)]
    use crate::internal::{config::ConfigKv, db, metadata::MetadataValueType};

    #[test]
    fn transient_database_errors_are_not_replaced_with_association_remedy() {
        let error =
            preserve_transient_association_error(anyhow::anyhow!(CaptureCatalogError::Database));
        assert!(error.chain().any(|cause| {
            cause.downcast_ref::<CaptureCatalogError>() == Some(&CaptureCatalogError::Database)
        }));
        let permanent = preserve_transient_association_error(anyhow::anyhow!(
            "invalid authenticated association"
        ));
        assert_eq!(permanent.to_string(), REMEDY);

        let key_failure =
            preserve_transient_association_error(anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "key file temporarily unavailable",
            )));
        assert!(
            key_failure
                .chain()
                .any(|cause| cause.is::<std::io::Error>())
        );
        let unreadable_repo = preserve_transient_association_error(anyhow::Error::new(
            WorkspaceError::ReadFailed("database temporarily unavailable".to_owned()),
        ));
        assert!(
            unreadable_repo
                .chain()
                .any(|cause| cause.is::<WorkspaceError>())
        );
        let permanent_key_failure = preserve_transient_association_error(anyhow::anyhow!(
            "repository-private agent capture key is not owned by this user"
        ));
        assert_eq!(permanent_key_failure.to_string(), REMEDY);
    }

    #[test]
    fn worker_identity_retries_only_typed_transient_failures() {
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        let db = anyhow::anyhow!(CaptureCatalogError::Database);
        let schema = anyhow::anyhow!(CaptureCatalogError::SchemaUnavailable);
        let permission =
            anyhow::anyhow!("repository-private agent capture key must have permissions 0600");
        let authentication = anyhow::anyhow!(
            "pending capture artifact authentication failed; run libra agent doctor"
        );
        assert!(transient_identity_error(&db, deadline));
        assert!(transient_identity_error(&schema, deadline));
        assert!(!transient_identity_error(&permission, deadline));
        assert!(!transient_identity_error(&authentication, deadline));
        assert!(transient_identity_error(
            &anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "temporary key read timeout",
            )),
            deadline
        ));
        #[cfg(unix)]
        assert!(!transient_identity_error(
            &anyhow::Error::new(std::io::Error::from_raw_os_error(libc::ELOOP)),
            deadline
        ));
    }

    #[cfg(unix)]
    struct Fixture {
        root: TempDir,
        storage: PathBuf,
        conn: DatabaseConnection,
        scope: CaptureScope,
    }

    #[cfg(unix)]
    impl Fixture {
        async fn new() -> Self {
            let root = tempfile::tempdir().expect("isolated repository fixture");
            let storage = root.path().join(".libra");
            std::fs::create_dir_all(storage.join("objects")).unwrap();
            let path = storage.join("libra.db");
            let conn = db::create_database(path.to_str().unwrap()).await.unwrap();
            ConfigKv::set_with_conn(&conn, "libra.repoid", "private-repo", false)
                .await
                .unwrap();
            super::super::key::load_capture_dedup_secret(&storage).unwrap();
            let scope = CaptureScope::resolve(&conn, root.path()).await.unwrap();
            let fixture = Self {
                root,
                storage,
                conn,
                scope,
            };
            fixture
                .seed_session("claude__native-session", "native-session", None)
                .await;
            fixture
        }

        async fn seed_session(&self, session: &str, native: &str, incarnation: Option<&str>) {
            let metadata = match incarnation {
                Some(value) => serde_json::json!({"capture_incarnation": value}).to_string(),
                None => "{}".to_owned(),
            };
            self.conn
                .execute_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
                   state, working_dir, metadata_json, started_at, last_event_at,
                   sync_revision, repo_id, worktree_id, scope_state)
                 VALUES (?, 'claude_code', ?, 'active', ?, ?, 1, 1, 1, ?, '', 'scoped')",
                    [
                        session.into(),
                        native.into(),
                        self.root.path().to_string_lossy().into_owned().into(),
                        metadata.into(),
                        self.scope.repo_id.clone().into(),
                    ],
                ))
                .await
                .unwrap();
        }

        async fn context(&self) -> PendingSessionContext {
            let txn = db::begin_write_transaction(&self.conn).await.unwrap();
            let context =
                resolve_pending_session_context(&txn, &self.scope, "claude__native-session")
                    .await
                    .unwrap();
            assert!(retained_alias(&txn, &context).await.unwrap().is_none());
            txn.commit().await.unwrap();
            context
        }

        async fn prepare(&self) -> PreparedPendingAlias {
            PendingSessionAlias::prepare(
                &self.conn,
                &self.context().await,
                None,
                &self.storage,
                self.root.path(),
                deadline(),
            )
            .await
            .unwrap()
        }

        async fn write_record(&self, record: &PendingSessionAlias) {
            MetadataKv::set_with_conn(
                &self.conn,
                MetadataScope::AgentCaptureSessionAlias,
                &self.scope.repo_id,
                record.alias(),
                &record.encode().unwrap(),
                MetadataValueType::Text,
            )
            .await
            .unwrap();
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn worker_lookup_treats_repository_mismatch_and_foreign_rows_as_invalid() {
        let fixture = Fixture::new().await;
        let alias = Uuid::new_v4().to_string();
        assert!(matches!(
            lookup_for_worker(&fixture.conn, "another-repository", &alias).await,
            Err(WorkerIdentityError::Invalid)
        ));
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentCapturePending,
            "another-repository",
            "foreign-checkpoint",
            "{}",
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(matches!(
            lookup_for_worker(&fixture.conn, &fixture.scope.repo_id, &alias).await,
            Err(WorkerIdentityError::Invalid)
        ));
    }

    #[cfg(unix)]
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    fn record() -> PendingSessionAlias {
        PendingSessionAlias {
            body: AliasBody {
                version: VERSION,
                alias: Uuid::new_v4().to_string(),
                session_id: "claude__native-session".into(),
                repo_id: "private-repo".into(),
                worktree_id: String::new(),
                workspace_id: None,
                capture_incarnation: None,
            },
            mac: format!("pending-alias/hmac-v1/{}", "a".repeat(64)),
        }
    }

    #[test]
    fn closed_canonical_alias_codec_is_bounded_and_content_free() {
        let record = record();
        let text = record.encode().unwrap();
        assert!(PendingSessionAlias::decode(&text, "private-repo", record.alias()).is_ok());
        for bad in [
            text.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
            text.replacen("{", "{\"path\":\"secret-locator\",", 1),
            format!(" {text}"),
            "x".repeat(MAX_ALIAS_BYTES + 1),
            text.replace("pending-alias/hmac-v1/", "pending-envelope/hmac-v1/"),
        ] {
            let error = PendingSessionAlias::decode(&bad, "private-repo", record.alias())
                .err()
                .expect("reject malformed association")
                .to_string();
            assert_eq!(error, REMEDY);
            assert!(!error.contains("secret-locator"));
        }
        for alias in [
            Uuid::nil().to_string(),
            record.alias().to_uppercase(),
            "not-uuid".into(),
        ] {
            assert!(!canonical_alias(&alias));
        }
        assert!(PendingSessionAlias::decode(&text, "foreign", record.alias()).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn real_catalog_pk_survives_vacuum_and_source_removal() {
        let fixture = Fixture::new().await;
        let prepared = fixture.prepare().await;
        let source = fixture.root.path().join("source.jsonl");
        std::fs::write(&source, b"provider content never read by identity port").unwrap();
        fixture.write_record(&prepared.record).await;
        std::fs::remove_file(&source).unwrap();
        fixture.conn.execute_unprepared("VACUUM").await.unwrap();
        let row = lookup(&fixture.conn, &fixture.scope.repo_id, prepared.alias())
            .await
            .unwrap()
            .unwrap();
        let txn = fixture.conn.begin().await.unwrap();
        let context = row
            .resolve(
                &txn,
                &fixture.scope,
                &fixture.storage,
                fixture.root.path(),
                deadline(),
            )
            .await
            .unwrap();
        assert_eq!(context.session_id(), "claude__native-session");
        assert_eq!(context.provider_session_id(), "native-session");
        assert_eq!(context.working_dir(), fixture.root.path().to_string_lossy());
        txn.rollback().await.unwrap();
        assert!(!source.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn mac_swap_scope_and_recapture_reject_stale_association() {
        let fixture = Fixture::new().await;
        let prepared = fixture.prepare().await;
        fixture
            .seed_session("claude__other-session", "other-session", None)
            .await;
        let mut swapped = prepared.record.clone();
        swapped.body.session_id = "claude__other-session".into();
        let txn = fixture.conn.begin().await.unwrap();
        assert!(
            swapped
                .resolve(
                    &txn,
                    &fixture.scope,
                    &fixture.storage,
                    fixture.root.path(),
                    deadline()
                )
                .await
                .is_err()
        );
        let mut foreign = fixture.scope.clone();
        foreign.worktree_id = "other-worktree".into();
        assert!(
            prepared
                .record
                .resolve(
                    &txn,
                    &foreign,
                    &fixture.storage,
                    fixture.root.path(),
                    deadline()
                )
                .await
                .is_err()
        );
        txn.rollback().await.unwrap();
        fixture
            .conn
            .execute_unprepared(
                "DELETE FROM agent_session WHERE session_id = 'claude__native-session'",
            )
            .await
            .unwrap();
        fixture
            .seed_session(
                "claude__native-session",
                "native-session",
                Some(&"b".repeat(32)),
            )
            .await;
        let txn = fixture.conn.begin().await.unwrap();
        assert!(
            prepared
                .record
                .resolve(
                    &txn,
                    &fixture.scope,
                    &fixture.storage,
                    fixture.root.path(),
                    deadline()
                )
                .await
                .is_err()
        );
        txn.rollback().await.unwrap();
        let new_alias = fixture.prepare().await;
        assert_ne!(new_alias.alias(), prepared.alias());
        assert_eq!(
            new_alias.context.incarnation(),
            Some("b".repeat(32).as_str())
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn alias_lookup_refuses_oversized_and_foreign_rows_before_hydration() {
        let fixture = Fixture::new().await;
        let row = record();
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentCaptureSessionAlias,
            &fixture.scope.repo_id,
            row.alias(),
            &"secret".repeat(MAX_ALIAS_BYTES),
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let error = lookup(&fixture.conn, &fixture.scope.repo_id, row.alias())
            .await
            .err()
            .unwrap()
            .to_string();
        assert_eq!(error, REMEDY);
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentCaptureSessionAlias,
            "foreign",
            row.alias(),
            "secret",
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(
            assert_private_repo(&fixture.conn, &fixture.scope.repo_id)
                .await
                .is_err()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reverse_selection_maps_type_confused_or_oversized_rows_to_remedy() {
        let fixture = Fixture::new().await;
        let context = fixture.context().await;
        let valid = record();
        for (key, value) in [
            // BLOB-typed value under a text value_type.
            ("CAST(? AS TEXT)", "CAST(CAST(? AS TEXT) AS BLOB)"),
            // BLOB-typed key with a well-formed text value.
            ("CAST(CAST(? AS TEXT) AS BLOB)", "CAST(? AS TEXT)"),
        ] {
            let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                format!(
                    "INSERT INTO metadata_kv (scope, target, key, value, value_type, created_at, updated_at)
                     VALUES ('agent_capture_session_alias', ?, {key}, {value}, 'text', 'c', 'u')"
                ),
                [
                    fixture.scope.repo_id.clone().into(),
                    valid.alias().into(),
                    valid.encode().unwrap().into(),
                ],
            ))
            .await
            .unwrap();
            let error = retained_alias(&txn, &context)
                .await
                .err()
                .expect("type-confused registry rows fail closed");
            assert_eq!(error.to_string(), REMEDY);
            assert!(
                !error.chain().any(|cause| cause.is::<sea_orm::DbErr>()),
                "a corrupt registry row must not be classified as a transient database fault"
            );
            txn.rollback().await.unwrap();
        }
        // An out-of-band key above the metadata key limit is never hydrated
        // by either the reverse selection or the shared bounded reader.
        let canary = format!("canary-oversized-key-{}", "k".repeat(300));
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "INSERT INTO metadata_kv (scope, target, key, value, value_type, created_at, updated_at)
                 VALUES ('agent_capture_session_alias', ?, ?, ?, 'text', 'c', 'u')",
                [
                    fixture.scope.repo_id.clone().into(),
                    canary.clone().into(),
                    valid.encode().unwrap().into(),
                ],
            ))
            .await
            .unwrap();
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        let error = retained_alias(&txn, &context).await.err().unwrap();
        assert_eq!(error.to_string(), REMEDY);
        txn.rollback().await.unwrap();
        let error = MetadataKv::list_bounded_with_conn(
            &fixture.conn,
            &[MetadataScope::AgentCaptureSessionAlias],
            Some(&fixture.scope.repo_id),
            None,
            17,
            MAX_ALIAS_BYTES,
        )
        .await
        .expect_err("oversized metadata keys fail closed before hydration");
        assert!(!format!("{error:#}").contains("canary-oversized-key"));
    }

    #[cfg(unix)]
    async fn header_fixture(
        txn: &DatabaseTransaction,
        prepared: &PreparedPendingAlias,
        checkpoint: &str,
    ) {
        let binding = super::super::pending::PendingBinding {
            scope: prepared.context.scope().clone(),
            session_id: prepared.alias().to_owned(),
            checkpoint_id: checkpoint.to_owned(),
            event_id: Uuid::new_v4().to_string(),
            action_key: "action-private-fixture".into(),
            receipt_key: "receipt-private-fixture".into(),
            marker_generation: "marker-private-fixture".into(),
            source_commitment: format!("source/hmac-v2/{}", "a".repeat(64)),
            reserved_revision: 1,
            original_deadline_millis: None,
            deferrable: true,
            first_attempt_millis: 1,
            parent_commit: None,
            parent_unborn: true,
        };
        // This private fixture covers association publication, not envelope
        // replay authenticity or the receipt/coverage integration of ACF-10.
        let text = format!(
            "{{\"version\":1,\"binding\":{},\"mac\":\"pending-envelope/hmac-v1/{}\",\"envelope_bytes\":1,\"chunks\":1,\"manual_attempted\":false}}",
            serde_json::to_string(&binding).unwrap(),
            "c".repeat(64)
        );
        MetadataKv::set_with_conn(
            txn,
            MetadataScope::AgentCapturePending,
            &prepared.context.scope().repo_id,
            checkpoint,
            &text,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn publication_requires_artifact_and_rolls_back_competing_mint() {
        let fixture = Fixture::new().await;
        let first = fixture.prepare().await;
        let competing = fixture.prepare().await;
        let checkpoint = Uuid::new_v4().to_string();
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        assert!(first.publish_for_artifact(&txn, &checkpoint).await.is_err());
        txn.rollback().await.unwrap();
        assert!(
            lookup(&fixture.conn, &fixture.scope.repo_id, first.alias())
                .await
                .unwrap()
                .is_none()
        );
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        header_fixture(&txn, &first, &checkpoint).await;
        first.publish_for_artifact(&txn, &checkpoint).await.unwrap();
        txn.rollback().await.unwrap();
        assert!(
            lookup(&fixture.conn, &fixture.scope.repo_id, first.alias())
                .await
                .unwrap()
                .is_none()
        );
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        header_fixture(&txn, &first, &checkpoint).await;
        first.publish_for_artifact(&txn, &checkpoint).await.unwrap();
        txn.commit().await.unwrap();
        let second_checkpoint = Uuid::new_v4().to_string();
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        header_fixture(&txn, &competing, &second_checkpoint).await;
        let lost = competing
            .publish_for_artifact(&txn, &second_checkpoint)
            .await
            .expect_err("a lost mint race must not publish a second alias");
        assert!(
            lost.chain().any(|cause| cause.is::<PendingAliasConflict>()),
            "a lost mint race is a typed retryable conflict, not the corruption remedy"
        );
        assert_ne!(lost.to_string(), REMEDY);
        txn.rollback().await.unwrap();
        assert!(
            lookup(&fixture.conn, &fixture.scope.repo_id, competing.alias())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            MetadataKv::get_with_conn(
                &fixture.conn,
                MetadataScope::AgentCapturePending,
                &fixture.scope.repo_id,
                &second_checkpoint
            )
            .await
            .unwrap()
            .is_none()
        );
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        let context =
            resolve_pending_session_context(&txn, &fixture.scope, "claude__native-session")
                .await
                .unwrap();
        let existing = retained_alias(&txn, &context).await.unwrap().unwrap();
        txn.commit().await.unwrap();
        let reused = PendingSessionAlias::prepare(
            &fixture.conn,
            &context,
            Some(existing),
            &fixture.storage,
            fixture.root.path(),
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(reused.alias(), first.alias());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn keyless_erasure_ignores_ledger_and_unattributable_foreign_rows() {
        let fixture = Fixture::new().await;
        let prepared = fixture.prepare().await;
        // Erase trusts the local canonical FK, not replay MAC/scope proof:
        // an old worktree/workspace association must not strand this PK.
        let mut old_scope = prepared.record.clone();
        old_scope.body.worktree_id = "retired-worktree".into();
        old_scope.body.workspace_id = Some("retired-workspace".into());
        fixture.write_record(&old_scope).await;
        let key_path = fixture
            .storage
            .join(super::super::key::CAPTURE_DEDUP_SECRET_DIR)
            .join(super::super::key::CAPTURE_DEDUP_SECRET_FILE);
        std::fs::remove_file(&key_path).unwrap();
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentCaptureSessionAlias,
            "foreign",
            &Uuid::new_v4().to_string(),
            "damaged-foreign",
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        MetadataKv::set_with_conn(
            &fixture.conn,
            MetadataScope::AgentCaptureSessionAlias,
            &fixture.scope.repo_id,
            &Uuid::new_v4().to_string(),
            "damaged-own",
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        let owned = aliases_for_erasure(&txn, &fixture.scope, "claude__native-session", None)
            .await
            .unwrap();
        assert_eq!(owned.aliases, [prepared.alias()]);
        assert!(owned.unassigned);
        MetadataKv::set_with_conn(
            &txn,
            MetadataScope::AgentCapturePending,
            &fixture.scope.repo_id,
            &Uuid::new_v4().to_string(),
            "damaged-unrelated-header",
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(
            !remove_alias_if_unreferenced(&txn, &fixture.scope.repo_id, prepared.alias())
                .await
                .unwrap(),
            "normal completion remains conservative"
        );
        erase_attributed_aliases(&txn, &owned).await.unwrap();
        txn.commit().await.unwrap();
        assert!(
            MetadataKv::get_with_conn(
                &fixture.conn,
                MetadataScope::AgentCaptureSessionAlias,
                &fixture.scope.repo_id,
                prepared.alias()
            )
            .await
            .unwrap()
            .is_none()
        );
        assert_eq!(
            MetadataKv::list_bounded_with_conn(
                &fixture.conn,
                &[MetadataScope::AgentCaptureSessionAlias],
                None,
                None,
                17,
                MAX_ALIAS_BYTES
            )
            .await
            .unwrap()
            .len(),
            2
        );
        assert!(!key_path.exists());
    }

    #[test]
    fn damaged_header_attribution_is_independent_but_never_ambiguous() {
        let alias = Uuid::new_v4().to_string();
        let checkpoint = Uuid::new_v4().to_string();
        let header = format!(
            "{{\"binding\":{{\"session_id\":\"{alias}\",\"checkpoint_id\":\"{checkpoint}\"}},\"mac\":null,\"unrelated_damage\":true}}"
        );
        assert_eq!(header_alias_attribution(&header, &checkpoint), Some(alias));
        assert!(
            header_alias_attribution(
                &header.replacen("\"binding\":", "\"binding\":null,\"binding\":", 1),
                &checkpoint
            )
            .is_none()
        );
        assert!(header_alias_attribution(&header, &Uuid::new_v4().to_string()).is_none());
        assert!(header_alias_attribution(&"x".repeat(MAX_ALIAS_BYTES + 1), &checkpoint).is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reverse_capacity_is_bounded_without_overwriting_retained_rows() {
        let fixture = Fixture::new().await;
        let prepared = fixture.prepare().await;
        for index in 0..MAX_ALIASES {
            let mut row = record();
            row.body.alias = format!("{index:08x}-0000-4000-8000-000000000000");
            row.body.session_id = format!("claude__retained-{index}");
            fixture.write_record(&row).await;
        }
        let checkpoint = Uuid::new_v4().to_string();
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        header_fixture(&txn, &prepared, &checkpoint).await;
        assert!(
            prepared
                .publish_for_artifact(&txn, &checkpoint)
                .await
                .is_err()
        );
        txn.rollback().await.unwrap();
        assert!(
            MetadataKv::get_with_conn(
                &fixture.conn,
                MetadataScope::AgentCapturePending,
                &fixture.scope.repo_id,
                &checkpoint
            )
            .await
            .unwrap()
            .is_none()
        );
        assert_eq!(
            MetadataKv::list_bounded_with_conn(
                &fixture.conn,
                &[MetadataScope::AgentCaptureSessionAlias],
                Some(&fixture.scope.repo_id),
                None,
                17,
                MAX_ALIAS_BYTES
            )
            .await
            .unwrap()
            .len(),
            MAX_ALIASES
        );
        let mut excess = record();
        excess.body.alias = "fffffffe-0000-4000-8000-000000000000".into();
        fixture.write_record(&excess).await;
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        assert!(retained_alias(&txn, prepared.context()).await.is_err());
        txn.rollback().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn out_of_band_excess_is_reported_without_claiming_complete_erasure() {
        let fixture = Fixture::new().await;
        let prepared = fixture.prepare().await;
        for index in 0..17 {
            let mut row = record();
            row.body.alias = format!("{index:08x}-0000-4000-8000-000000000000");
            row.body.session_id = format!("claude__other-{index}");
            fixture.write_record(&row).await;
        }
        let mut after_bound = prepared.record.clone();
        after_bound.body.alias = "ffffffff-0000-4000-8000-000000000000".into();
        fixture.write_record(&after_bound).await;
        let txn = db::begin_write_transaction(&fixture.conn).await.unwrap();
        let owners = aliases_for_erasure(&txn, &fixture.scope, "claude__native-session", None)
            .await
            .unwrap();
        assert!(owners.aliases().is_empty());
        assert!(
            owners.has_unassigned(),
            "ACF-10 must emit the content-free incomplete-erasure note"
        );
        erase_attributed_aliases(&txn, &owners).await.unwrap();
        txn.commit().await.unwrap();
        assert!(
            MetadataKv::get_with_conn(
                &fixture.conn,
                MetadataScope::AgentCaptureSessionAlias,
                &fixture.scope.repo_id,
                after_bound.alias()
            )
            .await
            .unwrap()
            .is_some()
        );
    }
}
