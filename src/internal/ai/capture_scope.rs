//! Canonical ownership scope for captured external-agent state.
//!
//! Provider session ids are not repository identifiers.  This type binds a
//! capture/import/export write to the repository's `libra.repoid`, current
//! worktree, and (when one exists) a workspace lease fence.  Callers must
//! reject mismatches instead of silently adopting a row written elsewhere.

use std::{
    path::Path,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, bail};
use sea_orm::{ConnectionTrait, DbErr, Statement, Value};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{
    internal::workspace::{RepoIdentity, WorkspaceStore},
    utils::util,
};

/// Paired capture deadline for work which must be authorized by SQLite at its
/// final durable-write boundary.
///
/// `monotonic` remains the process-side budget used to stop asynchronous work.
/// `sqlite_not_after_millis` is the matching, immutable Unix-epoch deadline
/// established at ingress and evaluated by SQLite.  Deliberately do not derive
/// the latter from `Instant` at commit time: that would re-anchor an expired
/// operation to a fresh wall-clock deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaptureCommitDeadline {
    monotonic: Instant,
    sqlite_not_after_millis: i64,
}

impl CaptureCommitDeadline {
    /// Establish both clock domains together at dispatch time for callers
    /// without an existing host-provided capture deadline.
    ///
    /// The SQLite value is deliberately rounded down to milliseconds, making
    /// the durable authorization gate conservatively no later than the
    /// monotonic budget. Callers must retain the returned pair unchanged.
    pub(crate) fn from_budget(budget: Duration) -> Result<Self> {
        // Capture the wall clock first. This makes the persisted millisecond
        // boundary no later than the monotonic side established immediately
        // afterwards (and flooring the duration retains that conservatism).
        // Reversing this order could accidentally grant a few extra wall
        // clock milliseconds between the two clock reads.
        let budget_millis = i64::try_from(budget.as_millis())
            .context("capture commit deadline exceeds the persistent SQLite range")?;
        let now_millis = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context(
                "system clock precedes the Unix epoch while establishing capture commit deadline",
            )?
            .as_millis();
        let now_millis = i64::try_from(now_millis)
            .context("system clock exceeds the persistent SQLite range")?;
        let sqlite_not_after_millis = now_millis
            .checked_add(budget_millis)
            .context("capture commit deadline exceeds the persistent SQLite range")?;
        let monotonic = Instant::now()
            .checked_add(budget)
            .context("capture commit deadline exceeds this platform's monotonic clock range")?;
        Ok(Self {
            monotonic,
            sqlite_not_after_millis,
        })
    }

    /// Construct the pair only from deadline values already established by
    /// the ingress/runtime boundary.
    pub(crate) fn from_established_pair(monotonic: Instant, sqlite_not_after_millis: i64) -> Self {
        Self {
            monotonic,
            sqlite_not_after_millis,
        }
    }

    /// The process-side deadline. This is never used to derive a new SQLite
    /// wall-clock deadline during finalization.
    pub(crate) fn monotonic(self) -> Instant {
        self.monotonic
    }

    /// Immutable SQLite wall-clock authorization deadline in Unix milliseconds.
    pub(crate) fn sqlite_not_after_millis(self) -> i64 {
        self.sqlite_not_after_millis
    }

    #[cfg(test)]
    pub(crate) fn from_test_pair(monotonic: Instant, sqlite_not_after_millis: i64) -> Self {
        Self::from_established_pair(monotonic, sqlite_not_after_millis)
    }
}

/// Why a final database authorization was rejected.
///
/// Callers must roll the transaction back for either rejection.  Keeping the
/// deadline case distinct lets deadline-aware capture paths report a bounded
/// operation without treating an expired workspace lease as a timeout.
#[derive(Debug, Error)]
pub(crate) enum CaptureFinalCommitAuthorizationError {
    #[error("capture commit deadline elapsed before final database authorization")]
    DeadlineElapsed,
    #[error(
        "capture workspace lease is no longer live at its recorded fence; rerun from the current workspace before retrying"
    )]
    WorkspaceFenceRejected,
    #[error("cannot authorize final capture database commit")]
    Database(#[source] DbErr),
}

// SQLite's `unixepoch('now')` is second-granularity. Keep the millisecond
// component in the same SQLite statement as the final authorization fence.
// `julianday` is evaluated once per SQL statement and avoids concatenating
// independently formatted clock fragments at the linearization boundary.
const SQLITE_NOW_MILLIS_SQL: &str = "CAST((julianday('now') - 2440587.5) * 86400000 AS INTEGER)";

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
    /// Only the sealed-payload writer calls this signing façade. Key creation
    /// belongs to verified ingress/source acquisition, never artifact replay.
    pub(crate) async fn sign_pending_envelope_until<C: ConnectionTrait>(
        &self,
        conn: &C,
        storage_path: &Path,
        authorized_root: &Path,
        envelope: &[u8],
        deadline: Instant,
    ) -> Result<String> {
        crate::internal::ai::capture::key::authenticate_pending_envelope_in_scope_until(
            conn,
            self,
            storage_path,
            authorized_root,
            envelope,
            None,
            deadline,
        )
        .await
    }

    /// Check the authenticated byte envelope before constructing RedactedBytes.
    /// A missing key never creates a replacement, including in doctor paths.
    pub(crate) async fn verify_pending_envelope_until<C: ConnectionTrait>(
        &self,
        conn: &C,
        storage_path: &Path,
        authorized_root: &Path,
        envelope: &[u8],
        mac: &str,
        deadline: Instant,
    ) -> Result<()> {
        crate::internal::ai::capture::key::authenticate_pending_envelope_in_scope_until(
            conn,
            self,
            storage_path,
            authorized_root,
            envelope,
            Some(mac),
            deadline,
        )
        .await
        .map(|_| ())
    }

    /// Validate the repository and lease before accessing its private capture
    /// key. This guard is shared by source commitments and envelope MACs; it
    /// does not initialize a key or mutate its namespace.
    pub(crate) async fn assert_capture_key_storage_binding_until<C: ConnectionTrait>(
        &self,
        conn: &C,
        storage_path: &Path,
        authorized_root: &Path,
        deadline: Instant,
    ) -> Result<std::path::PathBuf> {
        tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), async {
            let check_deadline = || -> Result<()> {
                if Instant::now() >= deadline {
                    return Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed.into());
                }
                Ok(())
            };
            check_deadline()?;
            let resolved_scope = Self::resolve(conn, authorized_root)
                .await
                .context("resolve capture commitment scope")?;
            check_deadline()?;
            if &resolved_scope != self {
                bail!("capture commitment root does not belong to the authorized scope");
            }
            self.assert_workspace_fence_live(conn)
                .await
                .context("verify capture commitment workspace lease")?;
            check_deadline()?;
            let authorized_storage =
                util::try_get_storage_path(Some(authorized_root.to_path_buf()))
                    .context("resolve storage for authorized capture root")?
                    .canonicalize()
                    .context("canonicalize storage for authorized capture root")?;
            let storage_path = storage_path
                .canonicalize()
                .context("canonicalize capture commitment storage")?;
            if authorized_storage != storage_path {
                bail!(
                    "capture commitment storage does not belong to the authorized repository root"
                );
            }
            check_deadline()?;
            Ok(storage_path)
        })
        .await
        .map_err(|_| anyhow::anyhow!(CaptureFinalCommitAuthorizationError::DeadlineElapsed))?
    }

    /// Resolve the scope from one already-selected repository root.
    ///
    /// The workspace lookup is read-only.  A normal main/human worktree need
    /// not have a WorkspaceRecord, but if one does exist its current fence is
    /// carried into later conditional writes.
    pub async fn resolve<C: ConnectionTrait>(conn: &C, repo_root: &Path) -> Result<Self> {
        let identity = RepoIdentity::resolve(conn).await.map_err(|error| {
            anyhow::Error::new(error).context("cannot resolve capture repository identity")
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
                    anyhow::Error::new(error).context("cannot resolve capture workspace by path")
                })?
        } else {
            WorkspaceStore::find_live_linked_with_conn(conn, &worktree_id)
                .await
                .map_err(|error| {
                    anyhow::Error::new(error)
                        .context("cannot resolve capture workspace by worktree id")
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
            anyhow::Error::new(error).context("cannot resolve capture repository identity")
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
                "provider session has legacy unscoped capture state; \
                 inspect it with `libra worktree doctor` and explicitly adopt it before retrying"
            );
        }
        bail!(
            "provider session is already claimed by another \
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
            return Err(CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected.into());
        }
        Ok(())
    }

    /// Authorize a final durable commit at one SQLite statement boundary.
    ///
    /// `sqlite_not_after_millis` is an immutable Unix-millisecond deadline
    /// supplied by the ingress/runtime boundary. It is intentionally not a
    /// `std::time::Instant`: converting an `Instant` here would create a new
    /// wall-clock deadline after the operation has already spent its budget.
    ///
    /// A workspace scope uses its existing conditional no-op update as the
    /// final DML. A main/non-workspace scope has no lease row, so it executes
    /// a one-row `SELECT` with the identical SQLite clock predicate. Both
    /// paths are meant to be the transaction's final SQL operation; callers
    /// must then await the non-cancellable commit acknowledgement rather than
    /// wrap a dispatched commit in a deadline timeout.
    pub(crate) async fn authorize_final_commit<C: ConnectionTrait>(
        &self,
        conn: &C,
        sqlite_not_after_millis: Option<i64>,
    ) -> std::result::Result<(), CaptureFinalCommitAuthorizationError> {
        let Some(workspace_id) = self.workspace_id.as_deref() else {
            return authorize_unscoped_final_commit(conn, sqlite_not_after_millis).await;
        };

        let result = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!(
                    "UPDATE workspace_record
                     SET lease_fence = lease_fence
                     WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                       AND state IN ('provisioning', 'active', 'releasing')
                       AND lease_owner IS NOT NULL
                       AND lease_expires_at > {SQLITE_NOW_MILLIS_SQL}
                       AND (? IS NULL OR {SQLITE_NOW_MILLIS_SQL} < ?)"
                ),
                [
                    workspace_id.into(),
                    self.repo_id.clone().into(),
                    self.workspace_fence.into(),
                    sqlite_not_after_millis.into(),
                    sqlite_not_after_millis.into(),
                ],
            ))
            .await
            .map_err(CaptureFinalCommitAuthorizationError::Database)?;
        if result.rows_affected() == 1 {
            return Ok(());
        }

        // The final DML already refused authorization. This follow-up only
        // classifies a failure for callers; it never authorizes a commit.
        // Once this transaction owns SQLite's writer lock, no concurrent
        // workspace mutation can make the classification race with the prior
        // final DML.
        let workspace_still_live = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                format!(
                    "SELECT 1 FROM workspace_record
                     WHERE workspace_id = ? AND repo_id = ? AND lease_fence = ?
                       AND state IN ('provisioning', 'active', 'releasing')
                       AND lease_owner IS NOT NULL
                       AND lease_expires_at > {SQLITE_NOW_MILLIS_SQL}"
                ),
                [
                    workspace_id.into(),
                    self.repo_id.clone().into(),
                    self.workspace_fence.into(),
                ],
            ))
            .await
            .map_err(CaptureFinalCommitAuthorizationError::Database)?;
        if workspace_still_live.is_some() && sqlite_not_after_millis.is_some() {
            Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed)
        } else {
            Err(CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected)
        }
    }

    /// Typed counterpart to [`Self::authorize_final_commit`] for capture
    /// paths that carry both the monotonic and SQLite deadline established at
    /// ingress. The helper reads only the immutable SQLite half.
    pub(crate) async fn authorize_final_commit_until<C: ConnectionTrait>(
        &self,
        conn: &C,
        deadline: Option<CaptureCommitDeadline>,
    ) -> std::result::Result<(), CaptureFinalCommitAuthorizationError> {
        self.authorize_final_commit(
            conn,
            deadline.map(CaptureCommitDeadline::sqlite_not_after_millis),
        )
        .await
    }

    /// Compatibility wrapper for existing unbounded callers. New
    /// deadline-aware writes must call [`Self::authorize_final_commit_until`]
    /// as their final SQL operation instead.
    pub async fn assert_workspace_fence_live_for_commit<C: ConnectionTrait>(
        &self,
        conn: &C,
    ) -> Result<()> {
        self.authorize_final_commit(conn, None)
            .await
            .map_err(anyhow::Error::new)
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

/// Final database authorization for both scoped and legacy/non-capture paths.
///
/// `None` still executes the singleton SQLite statement, so an unscoped
/// deadline-aware write cannot bypass the final database linearization point.
/// The caller must invoke this after every durable mutation and immediately
/// before an awaited, non-cancellable commit acknowledgement.
pub(crate) async fn authorize_final_capture_commit<C: ConnectionTrait>(
    scope: Option<&CaptureScope>,
    conn: &C,
    deadline: Option<CaptureCommitDeadline>,
) -> std::result::Result<(), CaptureFinalCommitAuthorizationError> {
    match scope {
        Some(scope) => scope.authorize_final_commit_until(conn, deadline).await,
        None => {
            authorize_unscoped_final_commit(
                conn,
                deadline.map(CaptureCommitDeadline::sqlite_not_after_millis),
            )
            .await
        }
    }
}

async fn authorize_unscoped_final_commit<C: ConnectionTrait>(
    conn: &C,
    sqlite_not_after_millis: Option<i64>,
) -> std::result::Result<(), CaptureFinalCommitAuthorizationError> {
    let authorized = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            format!(
                "SELECT 1 AS final_commit_authorized
                 WHERE ? IS NULL OR {SQLITE_NOW_MILLIS_SQL} < ?"
            ),
            [
                sqlite_not_after_millis.into(),
                sqlite_not_after_millis.into(),
            ],
        ))
        .await
        .map_err(CaptureFinalCommitAuthorizationError::Database)?;
    authorized
        .is_some()
        .then_some(())
        .ok_or(CaptureFinalCommitAuthorizationError::DeadlineElapsed)
}

#[cfg(test)]
mod tests {
    use sea_orm::{Database, DatabaseConnection};

    use super::*;

    async fn authorization_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open final-authorization test database");
        conn.execute_unprepared(
            "CREATE TABLE config_kv (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 key TEXT NOT NULL,
                 value TEXT NOT NULL,
                 encrypted INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE workspace_record (
                 workspace_id TEXT PRIMARY KEY,
                 repo_id TEXT NOT NULL,
                 lease_fence INTEGER NOT NULL,
                 state TEXT NOT NULL,
                 lease_owner TEXT,
                 lease_expires_at INTEGER NOT NULL
             );
             CREATE TABLE final_authorization_probe (value TEXT NOT NULL);",
        )
        .await
        .expect("create final-authorization test schema");
        conn
    }

    #[tokio::test]
    async fn unscoped_final_authorization_rejects_an_expired_sqlite_deadline() {
        let conn = authorization_db().await;
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin final-authorization transaction");
        let deadline =
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(5), 0);

        let error = authorize_final_capture_commit(None, &txn, Some(deadline))
            .await
            .expect_err("expired SQLite deadline must reject unscoped final authorization");
        assert!(matches!(
            error,
            CaptureFinalCommitAuthorizationError::DeadlineElapsed
        ));
        txn.rollback()
            .await
            .expect("roll back rejected final authorization");
    }

    #[tokio::test]
    async fn workspace_final_authorization_deadline_rolls_back_prior_writes() {
        let conn = authorization_db().await;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                 workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
             ) VALUES (?, ?, ?, 'active', 'test-owner', ?)",
            [
                "workspace".into(),
                "repo".into(),
                7_i64.into(),
                9_999_999_999_999_i64.into(),
            ],
        ))
        .await
        .expect("seed live workspace");
        let scope = CaptureScope {
            repo_id: "repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("workspace".to_string()),
            workspace_fence: Some(7),
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin scoped final-authorization transaction");
        txn.execute_unprepared("INSERT INTO final_authorization_probe (value) VALUES ('pending')")
            .await
            .expect("write state before final authorization");

        let error = authorize_final_capture_commit(
            Some(&scope),
            &txn,
            Some(CaptureCommitDeadline::from_test_pair(
                Instant::now() + Duration::from_secs(5),
                0,
            )),
        )
        .await
        .expect_err("expired SQLite deadline must reject workspace final authorization");
        assert!(matches!(
            error,
            CaptureFinalCommitAuthorizationError::DeadlineElapsed
        ));
        txn.rollback()
            .await
            .expect("roll back rejected scoped final authorization");

        let row = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT 1 FROM final_authorization_probe".to_string(),
            ))
            .await
            .expect("read final-authorization probe");
        assert!(
            row.is_none(),
            "rejected final authorization committed a write"
        );
    }

    #[tokio::test]
    async fn workspace_final_authorization_rejects_a_stale_fence() {
        let conn = authorization_db().await;
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO workspace_record (
                 workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
             ) VALUES (?, ?, ?, 'active', 'test-owner', ?)",
            [
                "workspace".into(),
                "repo".into(),
                7_i64.into(),
                9_999_999_999_999_i64.into(),
            ],
        ))
        .await
        .expect("seed live workspace");
        let stale_scope = CaptureScope {
            repo_id: "repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("workspace".to_string()),
            workspace_fence: Some(8),
        };
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin stale-fence final-authorization transaction");

        let error = authorize_final_capture_commit(Some(&stale_scope), &txn, None)
            .await
            .expect_err("stale workspace fence must reject final authorization");
        assert!(matches!(
            error,
            CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
        ));
        txn.rollback()
            .await
            .expect("roll back stale-fence final authorization");
    }

    #[tokio::test]
    async fn final_authorization_accepts_a_live_pair_without_reanchoring_it() {
        let conn = authorization_db().await;
        let monotonic = Instant::now();
        let deadline = CaptureCommitDeadline::from_test_pair(monotonic, i64::MAX);
        assert_eq!(deadline.monotonic(), monotonic);
        assert_eq!(deadline.sqlite_not_after_millis(), i64::MAX);
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin live final-authorization transaction");

        authorize_final_capture_commit(None, &txn, Some(deadline))
            .await
            .expect("future SQLite deadline must authorize final transaction");
        txn.commit()
            .await
            .expect("commit after authorized final transaction");
    }

    #[test]
    fn production_budget_constructor_establishes_both_deadline_clocks() {
        let before = Instant::now();
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_millis(500))
            .expect("construct paired production deadline");
        assert!(deadline.monotonic() >= before);
        assert!(deadline.sqlite_not_after_millis() > 0);
    }
}
