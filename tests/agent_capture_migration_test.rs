//! Integration tests for the `2026050303_agent_capture` migration.
//!
//! See `docs/development/commands/_general.md` (section 4.4) for the acceptance criteria
//! these tests pin: fresh-DB up, legacy-DB compatibility, and `up → down → up`
//! idempotency.

use libra::internal::db::migration::{MigrationRunner, builtin_migrations, run_builtin_migrations};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, ExecResult, Statement,
};
use tempfile::TempDir;

const LEGACY_BOOTSTRAP_SQL: &str = include_str!("../sql/sqlite_20260309_init.sql");

fn fresh_db_url() -> (TempDir, String) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("agent_capture.db");
    std::fs::File::create(&path).expect("touch sqlite file");
    let url = format!("sqlite://{}", path.display());
    (dir, url)
}

async fn connect(url: &str) -> DatabaseConnection {
    let mut opts = ConnectOptions::new(url.to_string());
    opts.sqlx_logging(false);
    Database::connect(opts).await.expect("connect")
}

async fn table_exists(conn: &DatabaseConnection, name: &str) -> bool {
    let backend = conn.get_database_backend();
    conn.query_one_raw(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
        [name.into()],
    ))
    .await
    .expect("query")
    .is_some()
}

async fn index_exists(conn: &DatabaseConnection, name: &str) -> bool {
    let backend = conn.get_database_backend();
    conn.query_one_raw(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ? LIMIT 1",
        [name.into()],
    ))
    .await
    .expect("query")
    .is_some()
}

fn registered_runner() -> MigrationRunner {
    let mut runner = MigrationRunner::new();
    runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|migration| migration.version < 2026090101),
        )
        .expect("historical builtin migrations must register clean");
    runner
}

fn registered_versions_after(target: i64) -> Vec<i64> {
    builtin_migrations()
        .into_iter()
        .map(|migration| migration.version)
        .filter(|version| *version > target && *version < 2026090101)
        .collect()
}

/// Replay the legacy bootstrap SQL the way `establish_connection` does on
/// first-time install. Statements are executed individually because the
/// driver only accepts one DDL per `execute` call.
async fn run_legacy_bootstrap(conn: &DatabaseConnection) {
    let backend = conn.get_database_backend();
    for raw in LEGACY_BOOTSTRAP_SQL.split(';') {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            continue;
        }
        let _: ExecResult = conn
            .execute_raw(Statement::from_string(backend, trimmed.to_string()))
            .await
            .unwrap_or_else(|e| panic!("legacy bootstrap stmt failed: {trimmed}\n{e}"));
    }
}

#[tokio::test]
async fn agent_capture_creates_tables_and_indexes() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = registered_runner();
    let applied = runner.run_pending(&conn).await.expect("run_pending");
    assert!(applied.contains(&2026050303));

    assert!(table_exists(&conn, "agent_session").await);
    assert!(table_exists(&conn, "agent_checkpoint").await);
    assert!(index_exists(&conn, "idx_agent_session_provider").await);
    assert!(index_exists(&conn, "idx_agent_session_active").await);
    assert!(index_exists(&conn, "idx_agent_session_thread").await);
    assert!(index_exists(&conn, "idx_agent_checkpoint_session").await);
    assert!(index_exists(&conn, "idx_agent_checkpoint_scope").await);
}

#[tokio::test]
async fn agent_capture_run_pending_is_idempotent() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = registered_runner();

    let first = runner.run_pending(&conn).await.expect("run_pending #1");
    assert!(first.contains(&2026050303));

    let second = runner.run_pending(&conn).await.expect("run_pending #2");
    assert!(
        second.is_empty(),
        "second run must apply no migrations; got {second:?}"
    );

    // Tables still present after the no-op pass.
    assert!(table_exists(&conn, "agent_session").await);
    assert!(table_exists(&conn, "agent_checkpoint").await);
}

#[tokio::test]
async fn agent_capture_rollback_drops_tables_and_indexes_only() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = registered_runner();
    runner.run_pending(&conn).await.expect("run_pending");

    // Rolling back to before agent_capture also rolls back every migration
    // sitting on top of it (parent_commit nullable, approved_permission,
    // agent_usage_stats agent_name column, source_call_log, notes, agent-traces
    // branch rename). Rollback returns versions in reverse-application order —
    // newest first — so the list reads from the most recent built-in migration
    // down to agent_capture itself.
    let rolled_back = runner
        .rollback_to(&conn, 2026050302)
        .await
        .expect("rollback_to(2026050302)");
    let mut expected_rolled_back = registered_versions_after(2026050302);
    expected_rolled_back.reverse();
    assert_eq!(rolled_back, expected_rolled_back);

    // agent_capture artifacts gone.
    assert!(!table_exists(&conn, "agent_session").await);
    assert!(!table_exists(&conn, "agent_checkpoint").await);
    assert!(!index_exists(&conn, "idx_agent_session_provider").await);
    assert!(!index_exists(&conn, "idx_agent_session_active").await);
    assert!(!index_exists(&conn, "idx_agent_session_thread").await);
    assert!(!index_exists(&conn, "idx_agent_checkpoint_session").await);
    assert!(!index_exists(&conn, "idx_agent_checkpoint_scope").await);

    // Earlier migrations remain intact.
    assert!(table_exists(&conn, "automation_log").await);
    assert!(table_exists(&conn, "agent_usage_stats").await);
    assert_eq!(
        runner.current_version(&conn).await.unwrap(),
        Some(2026050302)
    );
}

#[tokio::test]
async fn agent_capture_up_down_up_round_trip() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = registered_runner();

    runner.run_pending(&conn).await.expect("up #1");
    runner
        .rollback_to(&conn, 2026050302)
        .await
        .expect("rollback");

    let applied_again = runner.run_pending(&conn).await.expect("up #2");
    assert!(applied_again.contains(&2026050303));
    assert!(applied_again.contains(&2026050501));
    assert!(table_exists(&conn, "agent_session").await);
    assert!(table_exists(&conn, "agent_checkpoint").await);
}

/// W4 ownership migration must never guess that historical capture rows came
/// from main. They receive the explicit `legacy_unknown` marker, and rollback
/// is blocked after any operator/runtime has attached a real scope.
#[tokio::test]
async fn capture_workspace_scope_migration_preserves_legacy_unknown_and_fences_down() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = registered_runner();
    runner
        .run_pending(&conn)
        .await
        .expect("run pending migrations");

    // Recreate the exact historical schema, then write the row before W4's
    // additive migration runs. Inserting after the latest migration would
    // only prove the column default, not that a real upgrade preserves an
    // existing capture row without inventing a main-worktree owner.
    let rolled_back_to_pre_w4 = runner
        .rollback_to(&conn, 2026073101)
        .await
        .expect("roll back W4 scope migration on an empty capture catalog");
    // Rollback returns every rolled-back version, newest first. Everything
    // registered above the W4 scope migration (the agent-usage pair, the
    // approved-permission provenance migration, the agent-bridge migrations,
    // …) sits on top of it and rides along without touching agent_session, so
    // derive the expectation from the registry the way the other rollback
    // cases in this file do — a hard-coded list silently rots into a failure
    // the next time any unrelated migration lands.
    let mut expected_rolled_back_to_pre_w4 = registered_versions_after(2026073101);
    expected_rolled_back_to_pre_w4.reverse();
    assert_eq!(rolled_back_to_pre_w4, expected_rolled_back_to_pre_w4);

    // This focused migration fixture intentionally does not install the
    // unrelated `ai_thread` parent table that the historical agent-session
    // schema references. The legacy row has no thread id; disable FK checks
    // only while seeding that pre-W4 shape, as the export-job fixture does.
    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "PRAGMA foreign_keys = OFF".to_string(),
    ))
    .await
    .expect("disable unrelated thread foreign key for legacy seed");

    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at
         ) VALUES ('legacy-scope', 'claude_code', 'legacy-provider', 'stopped', '/tmp',
                   '{}', '{}', 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed pre-W4-shaped session");
    assert_eq!(
        runner
            .run_pending(&conn)
            .await
            .expect("upgrade legacy capture row to W4 scope schema"),
        // Ascending application order: W4 scope first, then everything that
        // was rolled back with it, re-applied on top. Derived from the
        // registry for the same reason as the rollback expectation above.
        registered_versions_after(2026073101)
    );
    let row = conn
        .query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             FROM agent_session WHERE session_id = 'legacy-scope'"
                .to_string(),
        ))
        .await
        .expect("read legacy scope")
        .expect("legacy row");
    assert_eq!(
        row.try_get_by::<Option<String>, _>("repo_id").unwrap(),
        None
    );
    assert_eq!(
        row.try_get_by::<Option<String>, _>("worktree_id").unwrap(),
        None
    );
    assert_eq!(
        row.try_get_by::<Option<String>, _>("workspace_id").unwrap(),
        None
    );
    assert_eq!(
        row.try_get_by::<Option<i64>, _>("workspace_fence").unwrap(),
        None
    );
    assert_eq!(
        row.try_get_by::<String, _>("scope_state").unwrap(),
        "legacy_unknown"
    );
    for index in [
        "idx_agent_session_capture_provider",
        "idx_agent_export_job_capture_provider",
        "idx_agent_import_identity_capture_provider",
    ] {
        assert!(
            index_exists(&conn, index).await,
            "W4 provider-claim validation must stay indexed: {index}"
        );
    }

    let dangling_fence = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE agent_session
             SET repo_id = 'repo-1', worktree_id = '', workspace_fence = 7,
                 scope_state = 'scoped'
             WHERE session_id = 'legacy-scope'"
                .to_string(),
        ))
        .await;
    assert!(
        dangling_fence.is_err(),
        "a scoped capture row must reject a workspace fence without its workspace id"
    );

    conn.execute_raw(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_session
         SET repo_id = 'repo-1', worktree_id = '', scope_state = 'scoped'
         WHERE session_id = 'legacy-scope'"
            .to_string(),
    ))
    .await
    .expect("explicit scoped adoption shape");
    let error = runner
        .rollback_to(&conn, 2026073101)
        .await
        .expect_err("scoped capture rows must prevent dropping ownership columns");
    assert!(
        error.to_string().contains("CHECK constraint failed"),
        "down guard must fail closed: {error:#}"
    );
    assert_eq!(
        runner.current_version(&conn).await.unwrap(),
        Some(2026080401),
        "a refused down migration must leave the W4 schema active"
    );
}

/// AG-20 (plan.md Task A5): the `2026070802_agent_checkpoint_paging`
/// migration survives an up → down → up round-trip. Forward creates the
/// (deliberately non-unique) `traces_commit` probe index plus the two
/// keyset-pagination indexes; the paired down drops exactly those three
/// and nothing else; a second up re-creates them without collision.
#[tokio::test]
async fn agent_checkpoint_paging_up_down_up_round_trip() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = registered_runner();

    let paging_indexes = [
        "idx_agent_checkpoint_traces_commit",
        "idx_agent_session_started_paging",
        "idx_agent_checkpoint_created_paging",
    ];

    // Up: full registry applied → all three indexes exist.
    let applied = runner.run_pending(&conn).await.expect("up #1");
    assert!(applied.contains(&2026070802));
    for index in paging_indexes {
        assert!(index_exists(&conn, index).await, "missing {index} after up");
    }

    // The traces_commit index must be NON-unique (brick-avoidance decision
    // documented in the migration): duplicate traces_commit rows in a
    // legacy DB must not fail the automatic upgrade.
    let backend = conn.get_database_backend();
    let unique_row = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'index' \
             AND name = 'idx_agent_checkpoint_traces_commit' \
             AND sql LIKE '%UNIQUE%'"
                .to_string(),
        ))
        .await
        .expect("query index uniqueness")
        .expect("count row");
    let unique_count: i64 = unique_row.try_get_by("n").expect("decode count");
    assert_eq!(
        unique_count, 0,
        "idx_agent_checkpoint_traces_commit must be a plain (non-UNIQUE) index"
    );

    // Down: roll only this migration off; the indexes disappear while the
    // underlying tables stay.
    let rolled = runner
        .rollback_to(&conn, 2026070801)
        .await
        .expect("rollback_to(2026070801)");
    let mut expected_rolled = registered_versions_after(2026070801);
    expected_rolled.reverse();
    assert_eq!(rolled, expected_rolled);
    for index in paging_indexes {
        assert!(
            !index_exists(&conn, index).await,
            "{index} must be dropped by the down migration"
        );
    }
    assert!(table_exists(&conn, "agent_session").await);
    assert!(table_exists(&conn, "agent_checkpoint").await);
    // Pre-existing agent_capture indexes are untouched by the down.
    assert!(index_exists(&conn, "idx_agent_checkpoint_session").await);

    // Up again: re-creates the indexes with no `IF NOT EXISTS` collision.
    // `run_pending` re-applies oldest-first, so the paging migration
    // (2026070802) precedes the audit-log (2026070803) and coverage-gate
    // (2026071301) migrations, all of which rolled off when we rewound to
    // 2026070801 above.
    let reapplied = runner.run_pending(&conn).await.expect("up #2");
    assert_eq!(reapplied, registered_versions_after(2026070801));
    for index in paging_indexes {
        assert!(
            index_exists(&conn, index).await,
            "{index} must exist after re-applying"
        );
    }
}

/// CEX-EntireIO Phase 2 follow-up: `agent_checkpoint.parent_commit` must be
/// NULLable so the runtime can distinguish "user branch unborn" from
/// "lookup error" without conflating both into an empty string. After the
/// `2026050501` migration applies, an INSERT with NULL parent_commit must
/// succeed, and SELECTing it back must yield None.
#[tokio::test]
async fn agent_capture_parent_commit_is_nullable_after_migration() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    // The FK from `agent_session.thread_id` references `ai_thread`, which is
    // created by the legacy bootstrap. Replay it so the FK declaration is
    // satisfiable when SQLite enforces it on INSERT.
    run_legacy_bootstrap(&conn).await;
    run_builtin_migrations(&conn)
        .await
        .expect("run canonical built-in migrations");

    let backend = conn.get_database_backend();
    // Seed an agent_session that the FK'd checkpoint can hang off of.
    conn.execute_raw(Statement::from_string(
        backend,
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            started_at, last_event_at
         ) VALUES ('s1', 'claude_code', 'p1', 'active', '/tmp', 0, 0)"
            .to_string(),
    ))
    .await
    .expect("seed agent_session");

    // Insert a checkpoint with NULL parent_commit. Pre-migration this would
    // have failed the NOT NULL constraint; post-migration it must succeed.
    let res = conn
        .execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES ('c1', 's1', 'committed', NULL, 't', 'm', 'tc', 0)"
                .to_string(),
        ))
        .await;
    assert!(
        res.is_ok(),
        "NULL parent_commit must be accepted post-migration: {res:?}"
    );

    let row = conn
        .query_one_raw(Statement::from_string(
            backend,
            "SELECT parent_commit FROM agent_checkpoint WHERE checkpoint_id = 'c1'".to_string(),
        ))
        .await
        .expect("query")
        .expect("row");
    let parent: Option<String> = row.try_get_by("parent_commit").unwrap();
    assert!(
        parent.is_none(),
        "parent_commit must round-trip as NULL, got {parent:?}"
    );
}

#[tokio::test]
async fn agent_capture_compatible_with_legacy_bootstrap() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;

    // Simulate a database that was first created by the legacy bootstrap
    // SQL — `run_pending` must apply cleanly on top of it.
    run_legacy_bootstrap(&conn).await;

    let applied = run_builtin_migrations(&conn)
        .await
        .expect("run_pending on legacy bootstrap");
    assert!(applied.contains(&2026050303));

    assert!(table_exists(&conn, "agent_session").await);
    assert!(table_exists(&conn, "agent_checkpoint").await);
}

/// Inserting a row whose `state` is outside the allowed set must fail because
/// the migration declares a CHECK constraint. This pins that the constraint
/// is applied — not silently dropped during DDL execution.
#[tokio::test]
async fn agent_capture_session_state_check_constraint_rejects_invalid() {
    let (_dir, url) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = registered_runner();
    runner.run_pending(&conn).await.expect("run_pending");

    let backend = conn.get_database_backend();
    let res = conn
        .execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_session ( \
                session_id, agent_kind, provider_session_id, state, working_dir, \
                started_at, last_event_at \
             ) VALUES ('s1', 'claude_code', 'p1', 'bogus', '/', 0, 0)"
                .to_string(),
        ))
        .await;
    assert!(
        res.is_err(),
        "CHECK constraint on agent_session.state must reject 'bogus'"
    );
}
