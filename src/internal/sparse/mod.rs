//! Read-only sparse VIEW filter (lore.md 2.2) — the non-declined complement of
//! git sparse-checkout (D10 defers the materializing form). A stored allowlist
//! of gitignore-syntax include patterns scopes WHAT the read/query commands
//! (`ls-files`, `diff`) OUTPUT. It NEVER mutates the working tree, never writes
//! skip-worktree bits, and — critically — never filters the changes-to-be-
//! committed set that `commit` records, so `status`'s dirtiness and exit code
//! stay HONEST (a sparse view must not make status lie about what commit will
//! do). `status` only surfaces a one-line advisory that a view is active.
//!
//! State: the ordered pattern list lives in the `sparse_view` table (owned
//! solely by [`SparseViewStore`], §3.6); the enabled toggle lives in the
//! per-worktree `sparse_view_meta` projection (W1 §C.4.1.1 — it replaced the
//! scope-less `sparse.enabled` `config_kv` key via migration `2026072304`).
//! Absence-tolerant: a missing table (pre-migration / old binary) resolves
//! to an empty, disabled view. Every method takes the request's ONE resolved
//! [`WorktreeScope`] — patterns and the toggle are per-worktree facts.

use std::path::Path;

use ignore::{Match, gitignore::Gitignore};
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};

use crate::{
    internal::{db::get_db_conn_instance, worktree_scope::WorktreeScope},
    utils::util,
};

/// Single-owner store over `sparse_view` + the `sparse_view_meta` toggle.
pub struct SparseViewStore;

impl SparseViewStore {
    /// The ordered include patterns of `scope` (empty if the table is
    /// absent — tolerant, for read-only display callers).
    pub async fn list(scope: &WorktreeScope) -> Result<Vec<String>, String> {
        match Self::list_strict(scope).await {
            Err(e) if e.contains("no such table") => Ok(Vec::new()),
            other => other,
        }
    }

    /// The ordered include patterns of `scope`, PROPAGATING every read
    /// error INCLUDING a missing table — the migration runner creates the
    /// table on every connection, so its absence here means a corrupted or
    /// tampered store, and a materialization gate (hydrate) must fail
    /// closed on it rather than see an empty (no-op) view.
    pub async fn list_strict(scope: &WorktreeScope) -> Result<Vec<String>, String> {
        let db = get_db_conn_instance().await;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT pattern FROM sparse_view WHERE worktree_id = ? \
             ORDER BY ordinal ASC, id ASC",
            [scope.storage_key().into()],
        );
        let rows = db
            .query_all(stmt)
            .await
            .map_err(|e| format!("failed to list the sparse view: {e}"))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.try_get_by_index(0).map_err(|e| e.to_string())?);
        }
        Ok(out)
    }

    /// Whether `scope`'s view is enabled, PROPAGATING every read error
    /// INCLUDING a missing `sparse_view_meta` table — the mutation/
    /// materialization gates (hydrate) must fail closed instead of treating
    /// an unreadable or missing toggle store as "disabled" (the migration
    /// runner creates the table on every connection; absence = corruption).
    pub async fn is_enabled_strict(scope: &WorktreeScope) -> Result<bool, String> {
        let db = get_db_conn_instance().await;
        let stmt = Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT enabled FROM sparse_view_meta WHERE worktree_id = ?",
            [scope.storage_key().into()],
        );
        match db.query_one(stmt).await {
            Ok(Some(row)) => Ok(row.try_get_by_index::<i32>(0).map_err(|e| e.to_string())? != 0),
            Ok(None) => Ok(false),
            Err(e) => Err(format!("failed to read the sparse view toggle: {e}")),
        }
    }

    /// Whether `scope`'s view is enabled. Read errors resolve to `false` —
    /// acceptable ONLY for read-only display filtering (a query command must
    /// not fail on an advisory probe); hydrate uses [`Self::is_enabled_strict`].
    pub async fn is_enabled(scope: &WorktreeScope) -> bool {
        Self::is_enabled_strict(scope).await.unwrap_or(false)
    }

    async fn set_enabled(scope: &WorktreeScope, enabled: bool) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO sparse_view_meta (worktree_id, enabled) VALUES (?, ?) \
             ON CONFLICT(worktree_id) DO UPDATE SET enabled = excluded.enabled",
            [
                scope.storage_key().into(),
                (if enabled { 1 } else { 0 }).into(),
            ],
        ))
        .await
        .map_err(|e| format!("failed to set the sparse view toggle: {e}"))?;
        Ok(())
    }

    /// Replace `scope`'s pattern list (transactional) and ENABLE its view.
    pub async fn replace(scope: &WorktreeScope, patterns: &[String]) -> Result<(), String> {
        Self::rewrite(scope, patterns).await?;
        Self::set_enabled(scope, true).await
    }

    /// Append patterns (keeping order) and ENABLE `scope`'s view.
    pub async fn add(scope: &WorktreeScope, patterns: &[String]) -> Result<(), String> {
        let mut all = Self::list(scope).await?;
        all.extend(patterns.iter().cloned());
        Self::rewrite(scope, &all).await?;
        Self::set_enabled(scope, true).await
    }

    /// Drop every pattern and DISABLE `scope`'s view.
    pub async fn clear(scope: &WorktreeScope) -> Result<(), String> {
        Self::rewrite(scope, &[]).await?;
        Self::set_enabled(scope, false).await
    }

    /// Enable / disable `scope`'s view without changing the patterns.
    pub async fn enable(scope: &WorktreeScope) -> Result<(), String> {
        Self::set_enabled(scope, true).await
    }
    pub async fn disable(scope: &WorktreeScope) -> Result<(), String> {
        Self::set_enabled(scope, false).await
    }

    async fn rewrite(scope: &WorktreeScope, patterns: &[String]) -> Result<(), String> {
        let db = get_db_conn_instance().await;
        let txn = db.begin().await.map_err(|e| e.to_string())?;
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM sparse_view WHERE worktree_id = ?",
            [scope.storage_key().into()],
        ))
        .await
        .map_err(|e| format!("failed to clear the sparse view: {e}"))?;
        for (ordinal, pattern) in patterns.iter().enumerate() {
            txn.execute(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO sparse_view (worktree_id, pattern, ordinal) VALUES (?, ?, ?)",
                [
                    scope.storage_key().into(),
                    pattern.as_str().into(),
                    (ordinal as i64).into(),
                ],
            ))
            .await
            .map_err(|e| format!("failed to record a sparse pattern: {e}"))?;
        }
        txn.commit().await.map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// A compiled sparse view ready for per-path verdicts. `None` when the view is
/// disabled or has no patterns — in which case EVERYTHING is in view (a
/// deliberate anti-footgun: an enabled-but-empty view degrades to a no-op
/// rather than hiding the whole tree).
pub struct SparseView {
    matcher: Option<Gitignore>,
    workdir: std::path::PathBuf,
}

impl SparseView {
    /// Load + compile `scope`'s active view. Returns a no-op view
    /// (`is_active()` == false) when disabled/empty or on any load error —
    /// acceptable ONLY for read-only display filtering (`ls-files`/`diff`/
    /// `status` advisory); materialization paths use [`Self::try_load`].
    pub async fn load(scope: &WorktreeScope) -> Self {
        Self::try_load(scope).await.unwrap_or_else(|_| Self {
            matcher: None,
            workdir: util::working_dir(),
        })
    }

    /// Load + compile `scope`'s active view, PROPAGATING store read errors
    /// (W1 §C.4.1.1: a mutation/materialization gate like `hydrate` must not
    /// treat an unreadable view as "everything in view" — that would
    /// materialize past the gate on a probe failure). A disabled or empty
    /// view still resolves Ok to a no-op view: that is its true state.
    pub async fn try_load(scope: &WorktreeScope) -> Result<Self, String> {
        let workdir = util::working_dir();
        if !SparseViewStore::is_enabled_strict(scope).await? {
            return Ok(Self {
                matcher: None,
                workdir,
            });
        }
        let patterns = SparseViewStore::list_strict(scope).await?;
        let matcher = util::build_exclude_matcher(&workdir, &patterns)
            .map_err(|e| format!("failed to compile the sparse view patterns: {e}"))?;
        Ok(Self { matcher, workdir })
    }

    /// Whether the view actually filters anything.
    pub fn is_active(&self) -> bool {
        self.matcher.is_some()
    }

    /// Is `rel_path` (repo-root-relative, either separator) IN the view? Always
    /// true for a no-op view. ALLOWLIST semantics (lore.md 2.2 / Codex MF2):
    /// the last matching pattern wins — an `Ignore` verdict means in-view, a
    /// `Whitelist` (`!pat`) means the path was carved back OUT even under a
    /// broader include, and `None` (no pattern matched) means out-of-view
    /// (a view is an allowlist). NO ancestor-dominance short-circuit (that is
    /// exclude semantics and would defeat `!child` negations).
    pub fn contains(&self, rel_path: &Path) -> bool {
        let Some(matcher) = &self.matcher else {
            return true;
        };
        let abs = self.workdir.join(rel_path);
        match matcher.matched(&abs, false) {
            Match::Ignore(_) => true,
            Match::Whitelist(_) | Match::None => false,
        }
    }

    /// Convenience for string paths.
    pub fn contains_str(&self, rel_path: &str) -> bool {
        self.contains(Path::new(rel_path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::utils::test::{ChangeDirGuard, setup_with_new_libra_in};

    /// Allowlist verdict (Codex MF2): last-match-wins, `!child` re-excludes,
    /// no ancestor-dominance short-circuit, default-exclude.
    #[test]
    fn allowlist_verdict_honors_negation() {
        let dir = tempfile::tempdir().expect("tmp");
        let workdir = dir.path().to_path_buf();
        let matcher = util::build_exclude_matcher(
            &workdir,
            &["src/**".to_string(), "!src/gen/**".to_string()],
        )
        .expect("compile")
        .expect("some");
        let view = SparseView {
            matcher: Some(matcher),
            workdir,
        };
        assert!(view.contains_str("src/a.txt"), "included by src/**");
        assert!(
            !view.contains_str("src/gen/g.txt"),
            "!src/gen/** carves it OUT"
        );
        assert!(
            !view.contains_str("docs/d.txt"),
            "default-exclude (allowlist)"
        );
    }

    /// A no-op view (disabled/empty) includes everything.
    #[test]
    fn noop_view_includes_all() {
        let view = SparseView {
            matcher: None,
            workdir: std::path::PathBuf::from("/x"),
        };
        assert!(!view.is_active());
        assert!(view.contains_str("anything/at/all.txt"));
    }

    /// Store round-trip: set/add/list ordering + enable/disable/clear.
    #[tokio::test]
    #[serial_test::serial]
    async fn store_round_trip() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;
        let scope = WorktreeScope::Main;

        assert!(!SparseViewStore::is_enabled(&scope).await);
        SparseViewStore::replace(&scope, &["a/**".to_string(), "b/**".to_string()])
            .await
            .expect("replace");
        assert!(SparseViewStore::is_enabled(&scope).await, "replace enables");
        assert_eq!(
            SparseViewStore::list(&scope).await.expect("list"),
            vec!["a/**", "b/**"]
        );
        SparseViewStore::add(&scope, &["!a/x/**".to_string()])
            .await
            .expect("add");
        assert_eq!(
            SparseViewStore::list(&scope).await.expect("list"),
            vec!["a/**", "b/**", "!a/x/**"]
        );
        SparseViewStore::disable(&scope).await.expect("disable");
        assert!(!SparseViewStore::is_enabled(&scope).await);
        assert_eq!(
            SparseViewStore::list(&scope).await.expect("list").len(),
            3,
            "patterns kept"
        );
        SparseViewStore::clear(&scope).await.expect("clear");
        assert!(
            SparseViewStore::list(&scope)
                .await
                .expect("list")
                .is_empty()
        );
        assert!(!SparseViewStore::is_enabled(&scope).await);
    }

    /// W1 §C.4.1.1: two scopes hold patterns and enabled state independently
    /// — one scope's replace/clear/disable never leaks into the other's view.
    #[tokio::test]
    #[serial_test::serial]
    async fn scopes_are_isolated() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;
        let main = WorktreeScope::Main;
        let linked = WorktreeScope::Linked("wt-test".to_string());

        SparseViewStore::replace(&main, &["main/**".to_string()])
            .await
            .expect("main replace");
        SparseViewStore::replace(&linked, &["linked/**".to_string()])
            .await
            .expect("linked replace");
        assert_eq!(
            SparseViewStore::list(&main).await.expect("list"),
            vec!["main/**"]
        );
        assert_eq!(
            SparseViewStore::list(&linked).await.expect("list"),
            vec!["linked/**"]
        );

        // Disabling one scope leaves the other enabled.
        SparseViewStore::disable(&linked).await.expect("disable");
        assert!(SparseViewStore::is_enabled(&main).await);
        assert!(!SparseViewStore::is_enabled(&linked).await);

        // Clearing one scope leaves the other's patterns intact.
        SparseViewStore::clear(&main).await.expect("clear");
        assert!(SparseViewStore::list(&main).await.expect("list").is_empty());
        assert_eq!(
            SparseViewStore::list(&linked).await.expect("list"),
            vec!["linked/**"],
            "linked patterns survive main's clear"
        );
    }

    /// W1 §C.4.1.1: the STRICT load path (the hydrate materialization gate)
    /// fails closed when either sparse table is missing — the migration
    /// runner creates both on every connection, so absence means a
    /// corrupted/tampered store, and it must NOT degrade to a no-op
    /// "everything in view" verdict. The tolerant display path (`load`)
    /// still degrades.
    #[tokio::test]
    #[serial_test::serial]
    async fn try_load_fails_closed_on_missing_tables() {
        use sea_orm::{ConnectionTrait, DbBackend, Statement};

        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = ChangeDirGuard::new(tmp.path());
        setup_with_new_libra_in(tmp.path()).await;
        let scope = WorktreeScope::Main;
        SparseViewStore::replace(&scope, &["src/**".to_string()])
            .await
            .expect("replace");

        // Enabled view + missing PATTERN table → strict load refuses.
        let db = crate::internal::db::get_db_conn_instance().await;
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "ALTER TABLE sparse_view RENAME TO sparse_view__hidden".to_string(),
        ))
        .await
        .expect("hide pattern table");
        let err = SparseView::try_load(&scope)
            .await
            .err()
            .expect("missing pattern table must fail closed");
        assert!(err.contains("no such table"), "{err}");
        // The tolerant display path degrades to a no-op view instead.
        assert!(!SparseView::load(&scope).await.is_active());
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "ALTER TABLE sparse_view__hidden RENAME TO sparse_view".to_string(),
        ))
        .await
        .expect("restore pattern table");

        // Missing META table → strict load refuses too (the toggle store
        // is unreadable, not "disabled").
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "ALTER TABLE sparse_view_meta RENAME TO sparse_view_meta__hidden".to_string(),
        ))
        .await
        .expect("hide meta table");
        let err = SparseView::try_load(&scope)
            .await
            .err()
            .expect("missing meta table must fail closed");
        assert!(err.contains("no such table"), "{err}");
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "ALTER TABLE sparse_view_meta__hidden RENAME TO sparse_view_meta".to_string(),
        ))
        .await
        .expect("restore meta table");
        let view = SparseView::try_load(&scope).await.expect("restored");
        assert!(view.is_active(), "restored view is active again");
    }
}
