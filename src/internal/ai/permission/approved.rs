//! `ApprovedRuleset` projection over the `approved_permission` table.
//!
//! This module is the second half of OC-Phase 2 P2.5 from
//! `docs/development/commands/_general.md` (the first half is the migration that
//! introduces the table — `sql/migrations/2026050601_approved_permission.sql`).
//!
//! Lifecycle:
//!
//! 1. The user clicks `Always` on a permission prompt for some
//!    `(permission, pattern)` pair. The runtime calls
//!    [`ApprovedRulesetStore::append`] to persist one row per pattern.
//! 2. On the next session start, the runtime calls
//!    [`ApprovedRulesetStore::load`] for the active project and merges the
//!    resulting [`ApprovedRuleset`] into the in-memory
//!    [`PermissionRuleset`] **before** the per-session ruleset, so a
//!    subsequent session-level ask can still escalate or deny.
//! 3. Pattern-level deletion is handled by [`ApprovedRulesetStore::remove`];
//!    project-level wipe by [`ApprovedRulesetStore::clear`].
//!
//! What this module is **not**:
//! - It does not own the prompt / Reply state machine — that lives in the
//!   sandbox layer (`crate::internal::ai::sandbox::ApprovalCachePolicy`).
//!   This file is the persistent projection consumed by the cache policy.
//! - It does not enforce `Deny` rules; only `Allow` reaches the table.
//!   Deny is a refusal at prompt time and never persists here.

use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, FromQueryResult, Statement};

use super::rule::{PermissionAction, PermissionRule, PermissionRuleset};

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

/// CRUD helpers for the `approved_permission` table. Stateless — every
/// method takes the active connection so the caller can share the same
/// `DatabaseConnection` with the rest of the runtime.
#[derive(Clone, Debug, Default)]
pub struct ApprovedRulesetStore;

impl ApprovedRulesetStore {
    /// Load every persisted approval for `project_id`.
    ///
    /// Rows are returned in chronological insertion order (`created_at` ASC,
    /// then `permission`, then `pattern` for deterministic tie-breaking).
    /// An empty result yields [`ApprovedRuleset::empty`].
    pub async fn load(
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

    /// Persist one `(permission, pattern)` approval for `project_id`.
    ///
    /// The table's primary key is `(project_id, permission, pattern)`, so
    /// re-appending the same triple is a no-op. Returns the number of rows
    /// actually inserted: `1` for a fresh approval, `0` if the row already
    /// existed (the user replied `Always` again for the same pattern).
    pub async fn append(
        conn: &DatabaseConnection,
        project_id: &str,
        permission: &str,
        pattern: &str,
    ) -> Result<u64, DbErr> {
        let backend = conn.get_database_backend();
        let now_micros = Utc::now().timestamp_micros();
        let exec = conn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT OR IGNORE INTO approved_permission \
                 (project_id, permission, pattern, created_at) \
                 VALUES (?, ?, ?, ?)",
                [
                    project_id.into(),
                    permission.into(),
                    pattern.into(),
                    now_micros.into(),
                ],
            ))
            .await?;
        Ok(exec.rows_affected())
    }

    /// Remove a single `(permission, pattern)` approval for `project_id`.
    /// Returns the number of rows actually removed (0 or 1).
    pub async fn remove(
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

    /// Wipe every persisted approval for `project_id`. Used by
    /// `--reset-approvals` style CLI flows.
    pub async fn clear(conn: &DatabaseConnection, project_id: &str) -> Result<u64, DbErr> {
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
}

/// Shape used by [`ApprovedRulesetStore::load`] when projecting raw rows
/// into `(permission, pattern)` pairs. The query selects only these two
/// columns — `created_at` appears in the `ORDER BY` clause only and is not
/// fetched into the struct, since the in-memory [`PermissionRule`] type is
/// timestamp-agnostic.
#[derive(Debug, FromQueryResult)]
struct ApprovedRow {
    permission: String,
    pattern: String,
}

#[cfg(test)]
mod tests {
    use sea_orm::{Database, DatabaseConnection};

    use super::*;
    use crate::internal::db::migration::run_builtin_migrations;

    /// Connect to a fresh in-memory SQLite and apply every built-in
    /// migration. Returns the live connection ready for `approved_permission`
    /// inserts.
    async fn fresh_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("connect in-memory sqlite");
        run_builtin_migrations(&conn)
            .await
            .expect("apply built-in migrations");
        conn
    }

    /// Scenario: a project with no persisted approvals returns an empty
    /// `ApprovedRuleset`. This is the cold-start baseline; the runtime
    /// must not error when the table is present but empty.
    #[tokio::test]
    async fn load_empty_when_no_rows_persisted() {
        let conn = fresh_db().await;
        let ruleset = ApprovedRulesetStore::load(&conn, "proj").await.unwrap();
        assert_eq!(ruleset.project_id, "proj");
        assert!(ruleset.is_empty());
    }

    /// Scenario: append → load round-trips a single approval as a
    /// `PermissionAction::Allow` rule.
    #[tokio::test]
    async fn append_then_load_round_trips_a_single_allow() {
        let conn = fresh_db().await;
        let inserted = ApprovedRulesetStore::append(&conn, "proj", "edit", "src/**")
            .await
            .unwrap();
        assert_eq!(inserted, 1);

        let ruleset = ApprovedRulesetStore::load(&conn, "proj").await.unwrap();
        assert_eq!(ruleset.rules.len(), 1);
        let rule = &ruleset.rules[0];
        assert_eq!(rule.permission, "edit");
        assert_eq!(rule.pattern, "src/**");
        assert_eq!(rule.action, PermissionAction::Allow);
    }

    /// Scenario: appending the same `(permission, pattern)` twice is a
    /// no-op. The primary key keeps a single row, and the second append
    /// reports `rows_affected = 0` so the caller can detect a
    /// "already approved" replay.
    #[tokio::test]
    async fn append_is_idempotent_on_duplicate_pattern() {
        let conn = fresh_db().await;
        ApprovedRulesetStore::append(&conn, "proj", "edit", "src/**")
            .await
            .unwrap();
        let again = ApprovedRulesetStore::append(&conn, "proj", "edit", "src/**")
            .await
            .unwrap();
        assert_eq!(again, 0, "duplicate insert must be a no-op");

        let ruleset = ApprovedRulesetStore::load(&conn, "proj").await.unwrap();
        assert_eq!(ruleset.rules.len(), 1);
    }

    /// Scenario: rows are loaded in chronological insertion order so the
    /// runtime's `findLast` semantics see the latest approval as the
    /// override. Two appends with distinct patterns must come back in
    /// insert order.
    #[tokio::test]
    async fn load_returns_rules_in_insertion_order() {
        let conn = fresh_db().await;
        ApprovedRulesetStore::append(&conn, "proj", "edit", "src/**")
            .await
            .unwrap();
        // Wait one microsecond so `created_at` ordering is unambiguous on
        // hosts whose monotonic clock resolution is coarser than 1µs.
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        ApprovedRulesetStore::append(&conn, "proj", "shell", "git status")
            .await
            .unwrap();

        let ruleset = ApprovedRulesetStore::load(&conn, "proj").await.unwrap();
        let names: Vec<&str> = ruleset
            .rules
            .iter()
            .map(|r| r.permission.as_str())
            .collect();
        assert_eq!(names, vec!["edit", "shell"]);
    }

    /// Scenario: rows for a different project are not visible when
    /// loading. The table's PK includes `project_id` but the load filter
    /// must also use it; a regression that dropped the WHERE clause
    /// would leak across projects.
    #[tokio::test]
    async fn load_filters_by_project_id() {
        let conn = fresh_db().await;
        ApprovedRulesetStore::append(&conn, "alpha", "edit", "*")
            .await
            .unwrap();
        ApprovedRulesetStore::append(&conn, "beta", "shell", "*")
            .await
            .unwrap();

        let alpha = ApprovedRulesetStore::load(&conn, "alpha").await.unwrap();
        assert_eq!(alpha.rules.len(), 1);
        assert_eq!(alpha.rules[0].permission, "edit");

        let beta = ApprovedRulesetStore::load(&conn, "beta").await.unwrap();
        assert_eq!(beta.rules.len(), 1);
        assert_eq!(beta.rules[0].permission, "shell");
    }

    /// Scenario: `remove` deletes a specific `(permission, pattern)` row
    /// and reports the deletion count. A second remove of the same row
    /// returns 0.
    #[tokio::test]
    async fn remove_drops_a_specific_pattern_only() {
        let conn = fresh_db().await;
        ApprovedRulesetStore::append(&conn, "proj", "edit", "src/**")
            .await
            .unwrap();
        ApprovedRulesetStore::append(&conn, "proj", "edit", "tests/**")
            .await
            .unwrap();

        let removed = ApprovedRulesetStore::remove(&conn, "proj", "edit", "src/**")
            .await
            .unwrap();
        assert_eq!(removed, 1);

        let again = ApprovedRulesetStore::remove(&conn, "proj", "edit", "src/**")
            .await
            .unwrap();
        assert_eq!(again, 0, "second remove must report no rows affected");

        let remaining = ApprovedRulesetStore::load(&conn, "proj").await.unwrap();
        assert_eq!(remaining.rules.len(), 1);
        assert_eq!(remaining.rules[0].pattern, "tests/**");
    }

    /// Scenario: `clear` wipes every row for a project, leaving siblings
    /// untouched.
    #[tokio::test]
    async fn clear_wipes_one_project_only() {
        let conn = fresh_db().await;
        ApprovedRulesetStore::append(&conn, "alpha", "edit", "*")
            .await
            .unwrap();
        ApprovedRulesetStore::append(&conn, "alpha", "shell", "*")
            .await
            .unwrap();
        ApprovedRulesetStore::append(&conn, "beta", "shell", "*")
            .await
            .unwrap();

        let removed = ApprovedRulesetStore::clear(&conn, "alpha").await.unwrap();
        assert_eq!(removed, 2);

        assert!(
            ApprovedRulesetStore::load(&conn, "alpha")
                .await
                .unwrap()
                .is_empty()
        );
        let beta = ApprovedRulesetStore::load(&conn, "beta").await.unwrap();
        assert_eq!(beta.rules.len(), 1, "sibling project must survive clear");
    }
}
