//! Canonical ownership scope for captured external-agent state.
//!
//! Provider session ids are not repository identifiers.  This type binds a
//! capture/import/export write to the repository's `libra.repoid`, current
//! worktree, and (when one exists) a workspace lease fence.  Callers must
//! reject mismatches instead of silently adopting a row written elsewhere.

use std::path::Path;

use anyhow::{Context, Result, bail};
use sea_orm::{ConnectionTrait, Statement, Value};
use serde::{Deserialize, Serialize};

use crate::{
    internal::workspace::{RepoIdentity, WorkspaceStore},
    utils::util,
};

/// The durable key carried by every new capture/import/export write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureScope {
    pub repo_id: String,
    /// Empty is the canonical main-worktree key.  Linked worktrees use their
    /// stable registry id, never a path-derived caller spelling.
    pub worktree_id: String,
    pub workspace_id: Option<String>,
    pub workspace_fence: Option<i64>,
}

impl CaptureScope {
    /// Resolve the scope from one already-selected repository root.
    ///
    /// The workspace lookup is read-only.  A normal main/human worktree need
    /// not have a WorkspaceRecord, but if one does exist its current fence is
    /// carried into later conditional writes.
    pub async fn resolve<C: ConnectionTrait>(conn: &C, repo_root: &Path) -> Result<Self> {
        let identity = RepoIdentity::resolve(conn).await.map_err(|error| {
            anyhow::anyhow!("cannot resolve capture repository identity: {error}")
        })?;
        // This is an externally selected worktree root, so failure to resolve
        // its local metadata must not silently alias capture to main.  Once
        // the gitdir is resolved, `None` is the unambiguous main-worktree
        // representation; `worktree_id_for_gitdir` synthesizes a stable id
        // for a damaged linked-worktree id file.
        let gitdir =
            util::try_get_worktree_gitdir(Some(repo_root.to_path_buf())).with_context(|| {
                format!(
                    "cannot resolve capture worktree metadata for '{}'",
                    repo_root.display()
                )
            })?;
        let worktree_id = util::worktree_id_for_gitdir(&gitdir).unwrap_or_default();
        let record = if worktree_id.is_empty() {
            WorkspaceStore::find_live_by_path_with_conn(conn, repo_root)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("cannot resolve capture workspace by path: {error}")
                })?
        } else {
            WorkspaceStore::find_live_linked_with_conn(conn, &worktree_id)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("cannot resolve capture workspace by worktree id: {error}")
                })?
        };
        Ok(Self {
            repo_id: identity.as_str().to_string(),
            worktree_id,
            workspace_id: record.as_ref().map(|record| record.workspace_id.clone()),
            workspace_fence: record.as_ref().map(|record| record.lease_fence),
        })
    }

    /// Test-only/in-process compatibility entrypoint.  The public hook path
    /// always calls [`Self::resolve`] with the real worktree root; callers
    /// that only supplied a database connection are deliberately treated as
    /// main scope rather than trusting an envelope-provided cwd.
    pub async fn main_for_connection<C: ConnectionTrait>(conn: &C) -> Result<Self> {
        let identity = RepoIdentity::resolve(conn).await.map_err(|error| {
            anyhow::anyhow!("cannot resolve capture repository identity: {error}")
        })?;
        Ok(Self {
            repo_id: identity.as_str().to_string(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        })
    }

    /// Assert that a provider session claim is already owned by exactly this
    /// scope.  `agent_kind` is intentionally not part of the read: adapters
    /// forwarding the same provider id must not bypass the cross-scope guard.
    pub async fn assert_provider_session_compatible<C: ConnectionTrait>(
        &self,
        conn: &C,
        provider_session_id: &str,
    ) -> Result<()> {
        // The import identity table can retain several source rows for one
        // provider session. Ask SQLite for just one incompatible claim rather
        // than materializing every same-scope source row on this hot path.
        let mismatch = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT scope_state FROM (
                     SELECT scope_state FROM agent_session
                      WHERE provider_session_id = ?
                        AND (scope_state <> 'scoped' OR repo_id IS NOT ?
                             OR worktree_id IS NOT ? OR workspace_id IS NOT ?
                             OR workspace_fence IS NOT ?)
                     UNION ALL
                     SELECT scope_state FROM agent_export_job
                      WHERE provider_session_id = ?
                        AND (scope_state <> 'scoped' OR repo_id IS NOT ?
                             OR worktree_id IS NOT ? OR workspace_id IS NOT ?
                             OR workspace_fence IS NOT ?)
                     UNION ALL
                     SELECT scope_state FROM agent_import_identity
                      WHERE provider_session_id = ?
                        AND (scope_state <> 'scoped' OR repo_id IS NOT ?
                             OR worktree_id IS NOT ? OR workspace_id IS NOT ?
                             OR workspace_fence IS NOT ?)
                 ) LIMIT 1",
                [
                    provider_session_id.into(),
                    self.repo_id.clone().into(),
                    self.worktree_id.clone().into(),
                    self.workspace_id.clone().into(),
                    self.workspace_fence.into(),
                    provider_session_id.into(),
                    self.repo_id.clone().into(),
                    self.worktree_id.clone().into(),
                    self.workspace_id.clone().into(),
                    self.workspace_fence.into(),
                    provider_session_id.into(),
                    self.repo_id.clone().into(),
                    self.worktree_id.clone().into(),
                    self.workspace_id.clone().into(),
                    self.workspace_fence.into(),
                ],
            ))
            .await
            .context("read captured provider-session ownership")?;
        let Some(row) = mismatch else {
            return Ok(());
        };
        let scope_state: String = row.try_get_by("scope_state")?;
        if scope_state != "scoped" {
            bail!(
                "provider session '{provider_session_id}' has legacy unscoped capture state; \
                 inspect it with `libra worktree doctor` and explicitly adopt it before retrying"
            );
        }
        bail!(
            "provider session '{provider_session_id}' is already claimed by another \
             repository/worktree/workspace scope; refusing to overwrite it — inspect \
             with `libra worktree doctor`; only legacy unscoped claims may be explicitly adopted"
        );
    }

    /// Verify the optional workspace lease immediately before a mutable
    /// operation. A non-workspace capture scope needs no lease; a workspace
    /// scope must never continue after its record was released or fenced.
    pub async fn assert_workspace_fence_live<C: ConnectionTrait>(&self, conn: &C) -> Result<()> {
        let Some(workspace_id) = self.workspace_id.as_deref() else {
            return Ok(());
        };
        let live = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM workspace_record
                 WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                   AND state IN ('provisioning', 'active', 'releasing')
                   AND lease_owner IS NOT NULL
                   AND lease_expires_at > (unixepoch('now') * 1000)",
                [
                    workspace_id.into(),
                    self.repo_id.clone().into(),
                    self.workspace_fence.into(),
                ],
            ))
            .await
            .context("verify capture workspace lease fence")?;
        if live.is_none() {
            bail!(
                "capture workspace lease is no longer live at its recorded fence; rerun from the current workspace before retrying"
            );
        }
        Ok(())
    }

    /// SQL values in the order used by scoped inserts.
    pub fn sql_values(&self) -> [Value; 4] {
        [
            self.repo_id.clone().into(),
            self.worktree_id.clone().into(),
            self.workspace_id.clone().into(),
            self.workspace_fence.into(),
        ]
    }

    /// Predicate for updates/UPSERT conflict paths.  `IS` gives SQLite's
    /// NULL-safe equality for an optional workspace and its fence.
    pub fn matches_existing_sql(table: &str) -> String {
        format!(
            "{table}.scope_state = 'scoped' AND {table}.repo_id = ? \
             AND {table}.worktree_id = ? AND {table}.workspace_id IS ? \
             AND {table}.workspace_fence IS ?"
        )
    }
}
