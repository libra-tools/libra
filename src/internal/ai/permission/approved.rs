//! `ApprovedRuleset` projection over the `approved_permission` table.
//!
//! This module is the second half of OC-Phase 2 P2.5 from
//! `docs/development/commands/_general.md` (the first half is the migration that
//! introduces the table — `sql/migrations/2026050601_approved_permission.sql`).
//! plan-20260715 W4-07 adds Repository ownership: `project_id` is the
//! canonical [`RepoIdentity`] (`libra.repoid`), and provenance columns audit
//! which worktree/session recorded an Always approval without changing the
//! matching key.
//!
//! Lifecycle:
//!
//! 1. The user clicks `Always` on a permission prompt for some
//!    `(permission, pattern)` pair. The runtime calls
//!    [`ApprovedRulesetStore::append`] to persist one row per pattern under
//!    the repository identity, with optional provenance.
//! 2. On the next session start, the runtime calls
//!    [`ApprovedRulesetStore::load`] for the repository and merges the
//!    resulting [`ApprovedRuleset`] into the in-memory
//!    [`PermissionRuleset`] **before** the per-session ruleset, so a
//!    subsequent session-level ask can still escalate or deny.
//! 3. Pattern-level deletion is handled by [`ApprovedRulesetStore::remove`];
//!    project-level wipe by [`ApprovedRulesetStore::clear`].
//! 4. Rows whose `project_id` is not the current `libra.repoid` stay invisible
//!    to [`ApprovedRulesetStore::load`] until an explicit doctor adopt
//!    ([`ApprovedRulesetStore::adopt_legacy_project_id`]) or clear.
//!
//! What this module is **not**:
//! - It does not own the prompt / Reply state machine — that lives in the
//!   sandbox layer (`crate::internal::ai::sandbox::ApprovalCachePolicy`).
//!   This file is the persistent projection consumed by the cache policy.
//! - It does not enforce `Deny` rules; only `Allow` reaches the table.
//!   Deny is a refusal at prompt time and never persists here.
//! - Runtime ApprovalStore cache keys / lease takeover are W4-13
//!   ([`super::runtime_cache`]).

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, Statement};

use super::rule::{PermissionAction, PermissionRule, PermissionRuleset};
use crate::internal::{
    config::ConfigKv,
    workspace::{RepoIdentity, WorkspaceError},
};

/// Audit-only provenance for an Always approval (plan-20260715 W4-07).
///
/// Empty strings mean "not recorded". These columns do not participate in the
/// matching key `(project_id, permission, pattern)`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ApprovalProvenance {
    pub source_worktree_id: String,
    pub source_session_id: String,
    pub source_workspace_id: String,
}

impl ApprovalProvenance {
    /// No worktree/session/workspace attribution.
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.source_worktree_id.is_empty()
            && self.source_session_id.is_empty()
            && self.source_workspace_id.is_empty()
    }
}

/// Errors from Repository-owned approved_permission operations.
#[derive(Debug, thiserror::Error)]
pub enum ApprovedStoreError {
    #[error(transparent)]
    Db(#[from] DbErr),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    NotFound(String),
}

/// Per-project snapshot of every persisted `Always`-reply approval.
///
/// `rules` always uses [`PermissionAction::Allow`] because the table only
/// stores positive approvals — a `Deny` reply does not persist (it just
/// refuses the current call). Order is the chronological insert order
/// produced by the load query's `ORDER BY created_at ASC, permission ASC,
/// pattern ASC`. The `idx_approved_permission_project (project_id,
/// created_at)` index accelerates the lookup; it does not by itself
/// dictate ordering — that comes from the query's `ORDER BY` clause.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedRuleset {
    pub project_id: String,
    pub rules: PermissionRuleset,
}

impl ApprovedRuleset {
    /// Empty ruleset for a project that has never persisted an `Always`
    /// approval. Useful as the initial value before the first DB load.
    pub fn empty(project_id: impl Into<String>) -> Self {
        Self {
            project_id: project_id.into(),
            rules: Vec::new(),
        }
    }

    /// Returns `true` when no approvals are persisted for this project.
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }
}

/// One Always row plus W4-07 provenance (audit only; not a matching key).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovedPermissionRecord {
    pub permission: String,
    pub pattern: String,
    pub provenance: ApprovalProvenance,
}

/// CRUD helpers for the `approved_permission` table. Stateless — every
/// method takes the active connection so the caller can share the same
/// `DatabaseConnection` with the rest of the runtime.
#[derive(Clone, Debug, Default)]
pub struct ApprovedRulesetStore;

/// Newest `libra.repoid` — same ordering as [`RepoIdentity::resolve`] /
/// `ConfigKv::get_with_conn` (newest row wins).
const REPO_IDENTITY_SUBQUERY: &str =
    "(SELECT value FROM config_kv WHERE key = 'libra.repoid' ORDER BY id DESC LIMIT 1)";

impl ApprovedRulesetStore {
    /// Resolve `libra.repoid`, minting it once under a serialized write lock
    /// when absent. Concurrent callers must not each insert a different UUID
    /// into the non-unique `config_kv` index.
    pub async fn ensure_repo_identity(
        conn: &DatabaseConnection,
    ) -> Result<RepoIdentity, ApprovedStoreError> {
        match RepoIdentity::resolve(conn).await {
            Ok(identity) => return Ok(identity),
            Err(WorkspaceError::Corrupt(message))
                if message.contains("repository identity (libra.repoid) is missing") => {}
            Err(error) => return Err(error.into()),
        }

        let txn = crate::internal::db::begin_write_transaction(conn).await?;
        match RepoIdentity::resolve(&txn).await {
            Ok(identity) => {
                txn.commit().await?;
                return Ok(identity);
            }
            Err(WorkspaceError::Corrupt(message))
                if message.contains("repository identity (libra.repoid) is missing") => {}
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error.into());
            }
        }

        let minted = uuid::Uuid::new_v4().to_string();
        if let Err(error) = ConfigKv::set_with_conn(&txn, "libra.repoid", &minted, false).await {
            txn.rollback().await.ok();
            return Err(ApprovedStoreError::Workspace(WorkspaceError::WriteFailed(
                format!("cannot initialize the repository identity (libra.repoid): {error}"),
            )));
        }
        txn.commit().await?;
        RepoIdentity::resolve(conn).await.map_err(Into::into)
    }

    /// Load every persisted approval for this repository's canonical
    /// identity (`libra.repoid`). Legacy `project_id` rows are excluded
    /// until doctor adopt.
    pub async fn load(conn: &DatabaseConnection) -> Result<ApprovedRuleset, ApprovedStoreError> {
        let identity = Self::ensure_repo_identity(conn).await?;
        Self::load_for_project_id(conn, identity.as_str())
            .await
            .map_err(Into::into)
    }

    /// Load every persisted approval for an explicit `project_id` (doctor /
    /// tests). Prefer [`ApprovedRulesetStore::load`] for runtime paths.
    pub async fn load_for_project_id(
        conn: &DatabaseConnection,
        project_id: &str,
    ) -> Result<ApprovedRuleset, DbErr> {
        let backend = conn.get_database_backend();
        let stmt = Statement::from_sql_and_values(
            backend,
            "SELECT permission, pattern FROM approved_permission \
             WHERE project_id = ? \
             ORDER BY created_at ASC, permission ASC, pattern ASC",
            [project_id.into()],
        );
        let rows = ApprovedRow::find_by_statement(stmt).all(conn).await?;
        let rules = rows
            .into_iter()
            .map(|row| PermissionRule::new(row.permission, row.pattern, PermissionAction::Allow))
            .collect();
        Ok(ApprovedRuleset {
            project_id: project_id.to_string(),
            rules,
        })
    }

    /// Load Always rows including W4-07 provenance for the canonical identity.
    pub async fn list_with_provenance(
        conn: &DatabaseConnection,
    ) -> Result<Vec<ApprovedPermissionRecord>, ApprovedStoreError> {
        let identity = Self::ensure_repo_identity(conn).await?;
        Self::list_with_provenance_for_project_id(conn, identity.as_str())
            .await
            .map_err(Into::into)
    }

    /// Load Always rows including provenance for an explicit `project_id`.
    pub async fn list_with_provenance_for_project_id(
        conn: &DatabaseConnection,
        project_id: &str,
    ) -> Result<Vec<ApprovedPermissionRecord>, DbErr> {
        let backend = conn.get_database_backend();
        let stmt = Statement::from_sql_and_values(
            backend,
            "SELECT permission, pattern, source_worktree_id, source_session_id, \
             source_workspace_id FROM approved_permission \
             WHERE project_id = ? \
             ORDER BY created_at ASC, permission ASC, pattern ASC",
            [project_id.into()],
        );
        let rows = ApprovedProvenanceRow::find_by_statement(stmt)
            .all(conn)
            .await?;
        Ok(rows
            .into_iter()
            .map(|row| ApprovedPermissionRecord {
                permission: row.permission,
                pattern: row.pattern,
                provenance: ApprovalProvenance {
                    source_worktree_id: row.source_worktree_id,
                    source_session_id: row.source_session_id,
                    source_workspace_id: row.source_workspace_id,
                },
            })
            .collect())
    }

    /// Persist one `(permission, pattern)` approval under the canonical
    /// repository identity, recording provenance for audit.
    ///
    /// The table's primary key is `(project_id, permission, pattern)`, so
    /// re-appending the same triple is a no-op. Returns the number of rows
    /// actually inserted: `1` for a fresh approval, `0` if the row already
    /// existed (the user replied `Always` again for the same pattern).
    ///
    /// The insert is fenced to the live `libra.repoid`: a concurrent rewrite
    /// of the identity makes the write affect zero rows and returns an
    /// identity-drift error instead of orphaning the approval.
    pub async fn append(
        conn: &DatabaseConnection,
        permission: &str,
        pattern: &str,
        provenance: &ApprovalProvenance,
    ) -> Result<u64, ApprovedStoreError> {
        Self::ensure_repo_identity(conn).await?;
        let txn = crate::internal::db::begin_write_transaction(conn).await?;
        let identity = RepoIdentity::resolve(&txn).await?;
        let backend = txn.get_database_backend();
        let now_micros = Utc::now().timestamp_micros();
        let exec = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "INSERT OR IGNORE INTO approved_permission \
                     (project_id, permission, pattern, created_at, \
                      source_worktree_id, source_session_id, source_workspace_id) \
                     SELECT ?, ?, ?, ?, ?, ?, ? \
                     WHERE ? = {REPO_IDENTITY_SUBQUERY}"
                ),
                [
                    identity.as_str().into(),
                    permission.into(),
                    pattern.into(),
                    now_micros.into(),
                    provenance.source_worktree_id.as_str().into(),
                    provenance.source_session_id.as_str().into(),
                    provenance.source_workspace_id.as_str().into(),
                    identity.as_str().into(),
                ],
            ))
            .await?;
        let inserted = exec.rows_affected();
        if inserted == 0 {
            match RepoIdentity::resolve(&txn).await {
                Ok(current) if current.as_str() != identity.as_str() => {
                    txn.rollback().await.ok();
                    return Err(ApprovedStoreError::Conflict(format!(
                        "the repository identity changed from {} to {} while persisting an \
                         Always approval; re-run the approval against the current identity",
                        identity.as_str(),
                        current.as_str()
                    )));
                }
                Ok(_) => {
                    // Duplicate under the same identity (INSERT OR IGNORE), or the
                    // fence matched but the row already existed.
                }
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error.into());
                }
            }
        }
        txn.commit().await?;
        Ok(inserted)
    }

    /// Persist under an explicit `project_id` (legacy seeding / tests).
    /// Prefer [`ApprovedRulesetStore::append`] for runtime paths.
    pub async fn append_for_project_id(
        conn: &DatabaseConnection,
        project_id: &str,
        permission: &str,
        pattern: &str,
        provenance: &ApprovalProvenance,
    ) -> Result<u64, DbErr> {
        let backend = conn.get_database_backend();
        let now_micros = Utc::now().timestamp_micros();
        let exec = conn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT OR IGNORE INTO approved_permission \
                 (project_id, permission, pattern, created_at, \
                  source_worktree_id, source_session_id, source_workspace_id) \
                 VALUES (?, ?, ?, ?, ?, ?, ?)",
                [
                    project_id.into(),
                    permission.into(),
                    pattern.into(),
                    now_micros.into(),
                    provenance.source_worktree_id.as_str().into(),
                    provenance.source_session_id.as_str().into(),
                    provenance.source_workspace_id.as_str().into(),
                ],
            ))
            .await?;
        Ok(exec.rows_affected())
    }

    /// Remove a single `(permission, pattern)` approval for the canonical
    /// repository identity. Returns the number of rows actually removed
    /// (0 or 1).
    pub async fn remove(
        conn: &DatabaseConnection,
        permission: &str,
        pattern: &str,
    ) -> Result<u64, ApprovedStoreError> {
        let identity = Self::ensure_repo_identity(conn).await?;
        Self::remove_for_project_id(conn, identity.as_str(), permission, pattern)
            .await
            .map_err(Into::into)
    }

    /// Remove under an explicit `project_id`.
    pub async fn remove_for_project_id(
        conn: &DatabaseConnection,
        project_id: &str,
        permission: &str,
        pattern: &str,
    ) -> Result<u64, DbErr> {
        let backend = conn.get_database_backend();
        let exec = conn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM approved_permission \
                 WHERE project_id = ? AND permission = ? AND pattern = ?",
                [project_id.into(), permission.into(), pattern.into()],
            ))
            .await?;
        Ok(exec.rows_affected())
    }

    /// Wipe every persisted approval for the canonical repository identity.
    pub async fn clear(conn: &DatabaseConnection) -> Result<u64, ApprovedStoreError> {
        let identity = Self::ensure_repo_identity(conn).await?;
        Self::clear_for_project_id(conn, identity.as_str())
            .await
            .map_err(Into::into)
    }

    /// Wipe every persisted approval for an explicit `project_id` (doctor
    /// cleanup of a legacy bucket, or `--reset-approvals` style flows).
    pub async fn clear_for_project_id(
        conn: &DatabaseConnection,
        project_id: &str,
    ) -> Result<u64, DbErr> {
        let backend = conn.get_database_backend();
        let exec = conn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM approved_permission WHERE project_id = ?",
                [project_id.into()],
            ))
            .await?;
        Ok(exec.rows_affected())
    }

    /// Distinct `project_id` values that are not the current `libra.repoid`.
    ///
    /// These rows are invisible to [`ApprovedRulesetStore::load`] until
    /// [`ApprovedRulesetStore::adopt_legacy_project_id`] or [`ApprovedRulesetStore::clear_for_project_id`]. When the
    /// repository identity is missing, every stored `project_id` is returned
    /// so doctor can still discover opaque legacy buckets.
    pub async fn list_legacy_project_ids(
        conn: &DatabaseConnection,
    ) -> Result<Vec<String>, ApprovedStoreError> {
        let backend = conn.get_database_backend();
        let (sql, values): (&str, Vec<sea_orm::Value>) = match RepoIdentity::resolve(conn).await {
            Ok(identity) => (
                "SELECT DISTINCT project_id FROM approved_permission \
                 WHERE project_id <> ? ORDER BY project_id ASC",
                vec![identity.as_str().into()],
            ),
            Err(WorkspaceError::Corrupt(message))
                if message.contains("repository identity (libra.repoid) is missing") =>
            {
                (
                    "SELECT DISTINCT project_id FROM approved_permission \
                     ORDER BY project_id ASC",
                    Vec::new(),
                )
            }
            Err(error) => return Err(error.into()),
        };
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(backend, sql, values))
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let id: String = row.try_get_by_index(0).map_err(|error| {
                DbErr::Custom(format!(
                    "cannot read legacy approved_permission project_id: {error}"
                ))
            })?;
            out.push(id);
        }
        Ok(out)
    }

    /// Delete Every approval under `legacy_project_id`, but only while that
    /// id is still non-canonical. Fenced to the live `libra.repoid` so a
    /// concurrent identity rewrite cannot turn this into a wipe of current
    /// repository approvals.
    pub async fn clear_legacy_project_id(
        conn: &DatabaseConnection,
        legacy_project_id: &str,
    ) -> Result<u64, ApprovedStoreError> {
        let identity = RepoIdentity::resolve(conn).await?;
        if legacy_project_id == identity.as_str() {
            return Err(ApprovedStoreError::Conflict(format!(
                "project_id '{legacy_project_id}' is the canonical repository identity; refuse \
                 to clear current-repository approvals via the legacy recovery path"
            )));
        }
        let txn = crate::internal::db::begin_write_transaction(conn).await?;
        let backend = txn.get_database_backend();
        let result = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "DELETE FROM approved_permission \
                     WHERE project_id = ? \
                       AND project_id <> {REPO_IDENTITY_SUBQUERY} \
                       AND ? = {REPO_IDENTITY_SUBQUERY}"
                ),
                [legacy_project_id.into(), identity.as_str().into()],
            ))
            .await?;
        if result.rows_affected() == 0 {
            match RepoIdentity::resolve(&txn).await {
                Ok(current) if current.as_str() != identity.as_str() => {
                    txn.rollback().await.ok();
                    return Err(ApprovedStoreError::Conflict(format!(
                        "the repository identity changed from {} to {} while clearing \
                         approved_permission rows; re-run the clear against the current identity",
                        identity.as_str(),
                        current.as_str()
                    )));
                }
                Ok(current) if current.as_str() == legacy_project_id => {
                    txn.rollback().await.ok();
                    return Err(ApprovedStoreError::Conflict(format!(
                        "project_id '{legacy_project_id}' is now the canonical repository \
                         identity; refuse to clear current-repository approvals via the legacy \
                         recovery path"
                    )));
                }
                Ok(_) => {
                    txn.rollback().await.ok();
                    return Err(ApprovedStoreError::NotFound(format!(
                        "no approved_permission rows use project_id '{legacy_project_id}'"
                    )));
                }
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error.into());
                }
            }
        }
        txn.commit().await?;
        Ok(result.rows_affected())
    }

    /// Re-home every approval under `legacy_project_id` onto the current
    /// `libra.repoid`. Explicit doctor action only — migrations never do this.
    ///
    /// Fails closed if a `(permission, pattern)` already exists under the
    /// canonical identity (unique primary key): clear the conflict first.
    pub async fn adopt_legacy_project_id(
        conn: &DatabaseConnection,
        legacy_project_id: &str,
    ) -> Result<u64, ApprovedStoreError> {
        let identity = RepoIdentity::resolve(conn).await?;
        if legacy_project_id == identity.as_str() {
            return Err(ApprovedStoreError::Conflict(format!(
                "project_id '{legacy_project_id}' is already the canonical repository identity; \
                 nothing to adopt"
            )));
        }
        let count = Self::load_for_project_id(conn, legacy_project_id)
            .await?
            .rules
            .len() as u64;
        if count == 0 {
            return Err(ApprovedStoreError::NotFound(format!(
                "no approved_permission rows use legacy project_id '{legacy_project_id}'"
            )));
        }
        let txn = crate::internal::db::begin_write_transaction(conn).await?;
        let backend = txn.get_database_backend();
        let result = match txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "UPDATE approved_permission SET project_id = ? \
                     WHERE project_id = ? AND ? = {REPO_IDENTITY_SUBQUERY}"
                ),
                [
                    identity.as_str().into(),
                    legacy_project_id.into(),
                    identity.as_str().into(),
                ],
            ))
            .await
        {
            Ok(result) => result,
            Err(error) if is_unique_violation(&error) => {
                txn.rollback().await.ok();
                return Err(ApprovedStoreError::Conflict(format!(
                    "cannot adopt legacy project_id '{legacy_project_id}' onto '{}': one or more \
                 (permission, pattern) pairs already exist under the canonical identity — clear \
                 the conflict (or the legacy bucket) before retrying",
                    identity.as_str()
                )));
            }
            Err(error) => {
                txn.rollback().await.ok();
                return Err(ApprovedStoreError::Db(error));
            }
        };
        if result.rows_affected() == 0 {
            match RepoIdentity::resolve(&txn).await {
                Ok(current) if current.as_str() != identity.as_str() => {
                    txn.rollback().await.ok();
                    return Err(ApprovedStoreError::Conflict(format!(
                        "the repository identity changed from {} to {} while adopting \
                         approved_permission rows; re-run the adoption against the current \
                         identity",
                        identity.as_str(),
                        current.as_str()
                    )));
                }
                Ok(_) => {
                    txn.rollback().await.ok();
                    return Err(ApprovedStoreError::NotFound(format!(
                        "no approved_permission rows use legacy project_id '{legacy_project_id}'"
                    )));
                }
                Err(error) => {
                    txn.rollback().await.ok();
                    return Err(error.into());
                }
            }
        }
        txn.commit().await?;
        Ok(result.rows_affected())
    }
}

fn is_unique_violation(error: &DbErr) -> bool {
    let rendered = error.to_string();
    rendered.contains("UNIQUE constraint failed") || rendered.contains("2067")
}

/// Shape used by [`ApprovedRulesetStore::load_for_project_id`] when projecting
/// raw rows into `(permission, pattern)` pairs. The query selects only these
/// two columns — `created_at` appears in the `ORDER BY` clause only and is not
/// fetched into the struct, since the in-memory [`PermissionRule`] type is
/// timestamp-agnostic.
#[derive(Debug, FromQueryResult)]
struct ApprovedRow {
    permission: String,
    pattern: String,
}

#[derive(Debug, FromQueryResult)]
struct ApprovedProvenanceRow {
    permission: String,
    pattern: String,
    source_worktree_id: String,
    source_session_id: String,
    source_workspace_id: String,
}

#[cfg(test)]
mod tests {
    use sea_orm::{Database, DatabaseConnection, DbBackend, Statement};

    use super::*;
    use crate::internal::db::migration::run_builtin_migrations;

    /// Connect to a fresh in-memory SQLite and apply every built-in
    /// migration. Seeds `libra.repoid` so canonical APIs resolve.
    async fn fresh_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        run_builtin_migrations(&conn)
            .await
            .expect("apply built-in migrations");
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO config_kv (key, value, encrypted) VALUES ('libra.repoid', ?, 0)",
            ["repo-canonical".into()],
        ))
        .await
        .expect("seed libra.repoid");
        conn
    }

    #[tokio::test]
    async fn load_empty_when_no_rows_persisted() {
        let conn = fresh_db().await;
        let ruleset = ApprovedRulesetStore::load(&conn).await.unwrap();
        assert_eq!(ruleset.project_id, "repo-canonical");
        assert!(ruleset.is_empty());
    }

    #[tokio::test]
    async fn append_then_load_round_trips_a_single_allow() {
        let conn = fresh_db().await;
        let provenance = ApprovalProvenance {
            source_worktree_id: "wt-1".into(),
            source_session_id: "sess-1".into(),
            source_workspace_id: String::new(),
        };
        let inserted = ApprovedRulesetStore::append(&conn, "edit", "src/**", &provenance)
            .await
            .unwrap();
        assert_eq!(inserted, 1);

        let ruleset = ApprovedRulesetStore::load(&conn).await.unwrap();
        assert_eq!(ruleset.rules.len(), 1);
        let rule = &ruleset.rules[0];
        assert_eq!(rule.permission, "edit");
        assert_eq!(rule.pattern, "src/**");
        assert_eq!(rule.action, PermissionAction::Allow);

        let backend = conn.get_database_backend();
        let row = conn
            .query_one_raw(Statement::from_string(
                backend,
                "SELECT source_worktree_id, source_session_id FROM approved_permission".to_string(),
            ))
            .await
            .unwrap()
            .expect("row");
        let wt: String = row.try_get_by_index(0).unwrap();
        let sess: String = row.try_get_by_index(1).unwrap();
        assert_eq!(wt, "wt-1");
        assert_eq!(sess, "sess-1");
    }

    #[tokio::test]
    async fn append_is_idempotent_on_duplicate_pattern() {
        let conn = fresh_db().await;
        let empty = ApprovalProvenance::empty();
        ApprovedRulesetStore::append(&conn, "edit", "src/**", &empty)
            .await
            .unwrap();
        let again = ApprovedRulesetStore::append(&conn, "edit", "src/**", &empty)
            .await
            .unwrap();
        assert_eq!(again, 0, "duplicate insert must be a no-op");

        let ruleset = ApprovedRulesetStore::load(&conn).await.unwrap();
        assert_eq!(ruleset.rules.len(), 1);
    }

    #[tokio::test]
    async fn load_returns_rules_in_insertion_order() {
        let conn = fresh_db().await;
        let empty = ApprovalProvenance::empty();
        ApprovedRulesetStore::append(&conn, "edit", "src/**", &empty)
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        ApprovedRulesetStore::append(&conn, "shell", "git status", &empty)
            .await
            .unwrap();

        let ruleset = ApprovedRulesetStore::load(&conn).await.unwrap();
        let names: Vec<&str> = ruleset
            .rules
            .iter()
            .map(|r| r.permission.as_str())
            .collect();
        assert_eq!(names, vec!["edit", "shell"]);
    }

    #[tokio::test]
    async fn load_filters_by_canonical_project_id() {
        let conn = fresh_db().await;
        let empty = ApprovalProvenance::empty();
        ApprovedRulesetStore::append(&conn, "edit", "*", &empty)
            .await
            .unwrap();
        ApprovedRulesetStore::append_for_project_id(&conn, "legacy-other", "shell", "*", &empty)
            .await
            .unwrap();

        let canonical = ApprovedRulesetStore::load(&conn).await.unwrap();
        assert_eq!(canonical.rules.len(), 1);
        assert_eq!(canonical.rules[0].permission, "edit");

        let legacy = ApprovedRulesetStore::list_legacy_project_ids(&conn)
            .await
            .unwrap();
        assert_eq!(legacy, vec!["legacy-other".to_string()]);
    }

    #[tokio::test]
    async fn remove_drops_a_specific_pattern_only() {
        let conn = fresh_db().await;
        let empty = ApprovalProvenance::empty();
        ApprovedRulesetStore::append(&conn, "edit", "src/**", &empty)
            .await
            .unwrap();
        ApprovedRulesetStore::append(&conn, "edit", "tests/**", &empty)
            .await
            .unwrap();

        let removed = ApprovedRulesetStore::remove(&conn, "edit", "src/**")
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let again = ApprovedRulesetStore::remove(&conn, "edit", "src/**")
            .await
            .unwrap();
        assert_eq!(again, 0, "second remove must report no rows affected");

        let remaining = ApprovedRulesetStore::load(&conn).await.unwrap();
        assert_eq!(remaining.rules.len(), 1);
        assert_eq!(remaining.rules[0].pattern, "tests/**");
    }

    #[tokio::test]
    async fn clear_wipes_canonical_only() {
        let conn = fresh_db().await;
        let empty = ApprovalProvenance::empty();
        ApprovedRulesetStore::append(&conn, "edit", "*", &empty)
            .await
            .unwrap();
        ApprovedRulesetStore::append(&conn, "shell", "*", &empty)
            .await
            .unwrap();
        ApprovedRulesetStore::append_for_project_id(&conn, "legacy-beta", "shell", "*", &empty)
            .await
            .unwrap();

        let removed = ApprovedRulesetStore::clear(&conn).await.unwrap();
        assert_eq!(removed, 2);

        assert!(ApprovedRulesetStore::load(&conn).await.unwrap().is_empty());
        let beta = ApprovedRulesetStore::load_for_project_id(&conn, "legacy-beta")
            .await
            .unwrap();
        assert_eq!(beta.rules.len(), 1, "legacy bucket must survive clear");
    }

    #[tokio::test]
    async fn adopt_legacy_rehomes_onto_canonical_identity() {
        let conn = fresh_db().await;
        let empty = ApprovalProvenance::empty();
        ApprovedRulesetStore::append_for_project_id(&conn, "opaque-old", "edit", "src/**", &empty)
            .await
            .unwrap();
        assert!(ApprovedRulesetStore::load(&conn).await.unwrap().is_empty());

        let adopted = ApprovedRulesetStore::adopt_legacy_project_id(&conn, "opaque-old")
            .await
            .unwrap();
        assert_eq!(adopted, 1);
        assert!(
            ApprovedRulesetStore::list_legacy_project_ids(&conn)
                .await
                .unwrap()
                .is_empty()
        );
        let ruleset = ApprovedRulesetStore::load(&conn).await.unwrap();
        assert_eq!(ruleset.rules.len(), 1);
        assert_eq!(ruleset.rules[0].pattern, "src/**");
    }
}
