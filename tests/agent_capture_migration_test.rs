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

const HERMES_KIND_VERSION: i64 = 2026103001;
const SESSION_COMMIT_VERSION: i64 = 2026102201;

fn hermes_runner() -> MigrationRunner {
    let mut runner = MigrationRunner::new();
    runner
        .extend(builtin_migrations())
        .expect("builtin migrations register");
    runner
}

async fn hermes_exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .unwrap_or_else(|error| panic!("exec {sql}: {error}"));
}

async fn hermes_count(conn: &DatabaseConnection, sql: &str) -> i64 {
    let row = conn
        .query_one_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("count")
        .expect("count row");
    row.try_get_by_index(0).expect("count value")
}

async fn hermes_text_column(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    let rows = conn
        .query_all_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .expect("text query");
    rows.into_iter()
        .map(|row| row.try_get_by_index::<String>(0).expect("text"))
        .collect()
}

async fn hermes_insert_session(conn: &DatabaseConnection, session_id: &str, kind: &str) {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES (?, ?, ?, 'stopped', '/work', '{}', '{}', 1, 1)",
        [
            session_id.into(),
            kind.into(),
            format!("provider-{session_id}").into(),
        ],
    ))
    .await
    .unwrap_or_else(|error| panic!("insert {kind}: {error}"));
}

fn hermes_seed_side_db(path: &std::path::Path, marker: &str) {
    std::fs::File::create(path).expect("create side db");
    // The repository migration must not open these files. A marker row makes
    // a later byte compare detect any write, including a receipt.
    let raw = format!("marker-{marker}-{}", marker.len());
    std::fs::write(path, raw.as_bytes()).expect("seed side bytes");
}

/// HM-01: repository CHECK gains hermes, keeps the previous kinds, and does
/// not write Global, System, or Derived databases.
#[tokio::test]
async fn hermes_role_repository_check_preserves_children_and_blocks_down() {
    let root = tempfile::tempdir().expect("temp root");
    let global = root.path().join("global.db");
    let system = root.path().join("system.db");
    let derived = root.path().join("derived.db");
    hermes_seed_side_db(&global, "global");
    hermes_seed_side_db(&system, "system");
    hermes_seed_side_db(&derived, "derived");
    let global_bytes = std::fs::read(&global).expect("read global");
    let system_bytes = std::fs::read(&system).expect("read system");
    let derived_bytes = std::fs::read(&derived).expect("read derived");

    let repo = root.path().join("repo.db");
    std::fs::File::create(&repo).expect("create repo db");
    let conn = connect(&format!("sqlite://{}", repo.display())).await;
    let runner = hermes_runner();
    runner
        .run_pending_up_to(&conn, SESSION_COMMIT_VERSION)
        .await
        .expect("migrate to the pre-hermes tip");
    hermes_exec(
        &conn,
        "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)",
    )
    .await;
    for kind in [
        "claude_code",
        "cursor",
        "codex",
        "gemini",
        "opencode",
        "copilot",
        "factory_ai",
        "grok",
    ] {
        hermes_insert_session(&conn, &format!("sess-{kind}"), kind).await;
    }
    hermes_exec(
        &conn,
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES ('cp-1', 'sess-claude_code', 'temporary', 'parent', 'tree', 'meta', 'traces', 1)",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) VALUES ('sess-claude_code', 'sha1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_coverage_claim (session_id, logical_turn_key, coverage_schema_version, coverage_digest, completeness, revision, state, source_channel, created_at, updated_at) VALUES ('sess-claude_code', 'turn-1', 1, 'digest', 'incomplete', 0, 'reserved_live', 'live', 1, 1)",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_coverage_revision (session_id, logical_turn_key, coverage_schema_version, revision, checkpoint_id, coverage_digest, completeness, source_channel, created_at) VALUES ('sess-claude_code', 'turn-1', 1, 1, 'cp-1', 'digest', 'incomplete', 'live', 1)",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_coverage_conflict (session_id, logical_turn_key, coverage_schema_version, incumbent_revision, incumbent_digest, incumbent_checkpoint_id, incoming_digest, incoming_source_channel, incoming_observed_at, incoming_canonical_json, incoming_redaction_report_json) VALUES ('sess-claude_code', 'turn-1', 1, 0, 'digest', 'cp-1', 'incoming', 'live', 1, '{}', '{}')",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_subagent_content_claim (parent_session_id, provider_kind, source_key, content_schema_version, current_revision, state, fence_token, created_at, updated_at) VALUES ('sess-claude_code', 'claude_code', 'key', 1, 0, 'idle', 0, 1, 1)",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_subagent_content_revision (parent_session_id, provider_kind, source_key, content_schema_version, revision, checkpoint_id, content_digest, source_channel, partial, created_at) VALUES ('sess-claude_code', 'claude_code', 'key', 1, 1, 'cp-1', 'digest', 'live', 0, 1)",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_subagent_link (content_checkpoint_id, parent_session_id, link_state, boundary_checkpoint_id, stable_subagent_id, created_at, updated_at) VALUES ('cp-1', 'sess-claude_code', 'resolved', 'cp-1', 'sub-1', 1, 1)",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO agent_import_tombstone (tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at) VALUES ('tomb-1', 'codex', 'erased-provider', 'codex__erased', 1)",
    )
    .await;

    let child_tables = hermes_text_column(
        &conn,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name != 'agent_session' AND LOWER(sql) LIKE '%references%agent_session%' ORDER BY name",
    )
    .await;
    let mut child_tables = child_tables;
    if !child_tables
        .iter()
        .any(|name| name == "agent_import_tombstone")
    {
        child_tables.push("agent_import_tombstone".to_string());
    }
    assert!(
        child_tables.len() >= 8,
        "expected the cascaded agent_session children, got {child_tables:?}"
    );
    let mut child_counts = Vec::new();
    for name in &child_tables {
        let count = hermes_count(&conn, &format!("SELECT COUNT(*) FROM `{name}`")).await;
        assert!(count >= 1, "child {name} was not pre-seeded");
        child_counts.push((name.clone(), count));
    }
    let dependents_before = hermes_text_column(
        &conn,
        "SELECT type || '|' || name || '|' || COALESCE(sql, '') FROM sqlite_master WHERE tbl_name = 'agent_session' AND type IN ('index', 'trigger') ORDER BY type, name",
    )
    .await;
    assert!(
        !dependents_before.is_empty(),
        "agent_session indexes/triggers"
    );
    let fk_before = hermes_count(&conn, "PRAGMA foreign_keys").await;
    let sessions_before = hermes_count(&conn, "SELECT COUNT(*) FROM agent_session").await;
    assert_eq!(sessions_before, 8);

    let raw = builtin_migrations()
        .into_iter()
        .find(|migration| migration.version == HERMES_KIND_VERSION)
        .expect("hermes migration");
    assert!(raw.up.contains("RAISE(ABORT"));
    assert!(raw.down.expect("down sql").contains("RAISE(ABORT"));
    let raw_failure = conn
        .execute_raw(Statement::from_string(
            conn.get_database_backend(),
            raw.up.to_string(),
        ))
        .await;
    assert!(
        raw_failure.is_err(),
        "executing the SQL body directly must abort"
    );
    assert_eq!(
        hermes_count(&conn, "SELECT COUNT(*) FROM agent_session").await,
        sessions_before,
        "a failed raw expansion must keep the preimage"
    );
    assert_eq!(
        hermes_count(
            &conn,
            "SELECT COUNT(*) FROM schema_versions WHERE version = 2026103001"
        )
        .await,
        0
    );

    let applied = runner
        .run_pending_up_to(&conn, HERMES_KIND_VERSION)
        .await
        .expect("apply hermes CHECK through the dedicated connection");
    assert!(
        applied.contains(&HERMES_KIND_VERSION),
        "hermes migration did not apply: {applied:?}"
    );
    assert_eq!(hermes_count(&conn, "PRAGMA foreign_keys").await, fk_before);
    assert_eq!(
        hermes_count(&conn, "SELECT COUNT(*) FROM agent_session").await,
        sessions_before
    );
    for (name, before) in &child_counts {
        assert_eq!(
            hermes_count(&conn, &format!("SELECT COUNT(*) FROM `{name}`")).await,
            *before,
            "child {name} changed"
        );
    }
    assert_eq!(
        hermes_text_column(
            &conn,
            "SELECT type || '|' || name || '|' || COALESCE(sql, '') FROM sqlite_master WHERE tbl_name = 'agent_session' AND type IN ('index', 'trigger') ORDER BY type, name",
        )
        .await,
        dependents_before
    );
    assert_eq!(
        hermes_count(&conn, "SELECT COUNT(*) FROM pragma_foreign_key_check").await,
        0
    );
    let integrity = hermes_text_column(&conn, "PRAGMA integrity_check").await;
    assert_eq!(integrity, vec!["ok".to_string()]);
    let check_sql = hermes_text_column(
        &conn,
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'agent_session'",
    )
    .await
    .join(" ");
    for kind in [
        "claude_code",
        "cursor",
        "codex",
        "gemini",
        "opencode",
        "copilot",
        "factory_ai",
        "grok",
        "hermes",
    ] {
        assert!(
            check_sql.contains(&format!("'{kind}'")),
            "CHECK lost {kind}: {check_sql}"
        );
    }
    assert!(
        !check_sql.contains("'pi'"),
        "pi is not part of the then-complete set; PI-01 adds it later"
    );
    assert_eq!(std::fs::read(&global).expect("reread global"), global_bytes);
    assert_eq!(std::fs::read(&system).expect("reread system"), system_bytes);
    assert_eq!(
        std::fs::read(&derived).expect("reread derived"),
        derived_bytes
    );

    let again = runner
        .run_pending_up_to(&conn, HERMES_KIND_VERSION)
        .await
        .expect("repeat up");
    assert!(again.is_empty(), "repeat up must be a no-op: {again:?}");
    assert_eq!(
        hermes_count(
            &conn,
            "SELECT COUNT(*) FROM schema_versions WHERE version = 2026103001 AND name = 'agent_kind_hermes'"
        )
        .await,
        1
    );

    hermes_insert_session(&conn, "sess-hermes", "hermes").await;
    let rejected_pi = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES ('sess-pi', 'pi', 'p-pi', 'stopped', '/work', '{}', '{}', 1, 1)",
            [],
        ))
        .await;
    assert!(
        rejected_pi.is_err(),
        "pi stays rejected at the hermes tip, before PI-01"
    );
    let refused = runner.rollback_to(&conn, SESSION_COMMIT_VERSION).await;
    let message = refused.expect_err("down must refuse while a hermes row exists");
    assert!(
        message.to_string().contains("were kept"),
        "down error should keep the hermes rows: {message}"
    );
    assert_eq!(
        hermes_count(
            &conn,
            "SELECT COUNT(*) FROM agent_session WHERE agent_kind = 'hermes'"
        )
        .await,
        1
    );
    assert_eq!(
        runner.current_version(&conn).await.expect("version"),
        Some(HERMES_KIND_VERSION)
    );
    for (name, before) in &child_counts {
        assert_eq!(
            hermes_count(&conn, &format!("SELECT COUNT(*) FROM `{name}`")).await,
            *before,
            "refused down changed child {name}"
        );
    }

    hermes_exec(
        &conn,
        "DELETE FROM agent_session WHERE agent_kind = 'hermes'",
    )
    .await;
    let rolled = runner
        .rollback_to(&conn, SESSION_COMMIT_VERSION)
        .await
        .expect("down with zero hermes rows");
    assert_eq!(rolled, vec![HERMES_KIND_VERSION]);
    let rejected_again = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES ('sess-hermes-again', 'hermes', 'p-hermes', 'stopped', '/work', '{}', '{}', 1, 1)",
            [],
        ))
        .await;
    assert!(
        rejected_again.is_err(),
        "old CHECK must reject hermes after rollback"
    );
    hermes_insert_session(&conn, "sess-grok-kept", "grok").await;
    assert_eq!(
        hermes_count(
            &conn,
            "SELECT COUNT(*) FROM agent_session WHERE session_id = 'sess-claude_code'"
        )
        .await,
        1
    );

    runner
        .run_pending_up_to(&conn, HERMES_KIND_VERSION)
        .await
        .expect("re-apply hermes migration");
    hermes_insert_session(&conn, "sess-hermes", "hermes").await;
    let sessions_digest = hermes_text_column(
        &conn,
        "SELECT session_id || '|' || agent_kind FROM agent_session ORDER BY session_id",
    )
    .await;
    hermes_exec(
        &conn,
        "INSERT INTO schema_versions (version, name, applied_at) VALUES (2099010101, 'future-reader', '2000-01-01T00:00:00Z')",
    )
    .await;
    drop(conn);
    let refused_open = libra::internal::db::schema::upgrade_database_schema_for_role(
        &repo,
        libra::internal::db::DatabaseRole::Repository,
    )
    .await;
    let open_error = refused_open.expect_err("a reader older than the file must refuse");
    assert!(open_error.to_string().contains("newer"), "{open_error}");
    let conn = connect(&format!("sqlite://{}", repo.display())).await;
    assert_eq!(
        hermes_text_column(
            &conn,
            "SELECT session_id || '|' || agent_kind FROM agent_session ORDER BY session_id",
        )
        .await,
        sessions_digest
    );
    assert_eq!(
        hermes_count(
            &conn,
            "SELECT COUNT(*) FROM schema_versions WHERE version = 2099010101 AND name = 'future-reader'"
        )
        .await,
        1,
        "the refused open must leave the future receipt untouched"
    );
    assert_eq!(
        std::fs::read(&global).expect("global still untouched"),
        global_bytes
    );
    assert_eq!(
        std::fs::read(&system).expect("system still untouched"),
        system_bytes
    );
    assert_eq!(
        std::fs::read(&derived).expect("derived still untouched"),
        derived_bytes
    );

    for role in [
        libra::internal::db::DatabaseRole::GlobalConfig,
        libra::internal::db::DatabaseRole::SystemConfig,
        libra::internal::db::DatabaseRole::Derived,
    ] {
        assert!(
            libra::internal::db::schema::migrations_for_role(role)
                .iter()
                .all(|migration| migration.version != HERMES_KIND_VERSION),
            "{role:?} must not register the hermes receipt"
        );
    }
    let derived_error = libra::internal::db::schema::upgrade_database_schema_for_role(
        &derived,
        libra::internal::db::DatabaseRole::Derived,
    )
    .await;
    assert!(
        derived_error.is_err(),
        "derived role must not take a migration"
    );
    assert_eq!(
        std::fs::read(&derived).expect("derived after refusal"),
        derived_bytes
    );
}

/// HM-06: hermes is supported for local capture. Review, investigate, and
/// cloud sync stay closed.
#[test]
fn hermes_role_stub_disables_capture_install_and_launch() {
    use libra::internal::ai::{
        capture::deadline_provider_extraction_summary,
        hooks::providers::hermes::settings::{HermesEnableGate, hermes_enable_gate},
        observed_agents::{
            AgentKind, AgentSessionCtx, SlugLookup, agent_for, extract::skill_registry_for,
            is_preview, live_capture_matches_hook_installable, lookup_cli_slug, registration_for,
            supported_slugs,
        },
    };

    assert_eq!(AgentKind::Hermes.as_db_str(), "hermes");
    assert_eq!(AgentKind::Hermes.as_cli_slug(), "hermes");
    assert_eq!(AgentKind::from_db_str("hermes"), Some(AgentKind::Hermes));
    assert_eq!(AgentKind::from_cli_slug("hermes"), Some(AgentKind::Hermes));
    assert!(matches!(
        lookup_cli_slug("vogon"),
        SlugLookup::UnknownQuarantined
    ));
    assert!(!is_preview(AgentKind::Hermes));
    let row = registration_for(AgentKind::Hermes);
    assert!(row.supported);
    assert!(row.hook_installable);
    assert!(!row.launchable_review);
    assert!(!row.launchable_investigate);
    assert!(row.transcript_readable);
    assert_eq!(row.support_wave, Some("hermes"));
    assert!(supported_slugs().contains(&"hermes"));
    assert!(live_capture_matches_hook_installable(AgentKind::Hermes));
    assert!(skill_registry_for(AgentKind::Hermes).is_empty());
    let agent = agent_for(AgentKind::Hermes);
    assert!(agent.as_hooks().is_some());
    assert_eq!(agent.protected_dirs(), &[".hermes"]);
    let transcript_error = agent
        .read_transcript(&AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            transcript_path: None,
        })
        .expect_err("capture stays disabled");
    assert!(
        transcript_error.to_string().contains("not available"),
        "{transcript_error}"
    );
    let summary = deadline_provider_extraction_summary(AgentKind::Hermes, b"thought-canary");
    assert!(summary.partial);
    assert!(summary.prompts.is_empty());
    assert!(
        summary
            .warnings
            .iter()
            .all(|warning| !warning.contains("thought-canary"))
    );

    let cloud = include_str!("../src/command/cloud/sync.rs");
    let allowlist_at = cloud
        .find("CLOUD_CAPTURE_KIND_ALLOWLIST")
        .expect("cloud allowlist");
    let window = &cloud[allowlist_at..allowlist_at + 400];
    assert!(
        !window.contains("\"hermes\""),
        "HM-01 must not add hermes to the cloud allowlist: {window}"
    );

    assert_eq!(hermes_enable_gate(true, false), HermesEnableGate::SkipBare);
    assert_eq!(
        hermes_enable_gate(false, false),
        HermesEnableGate::RefuseExplicit
    );
}

// ---- PI-01: Repository Pi identity and the `pi` agent_kind CHECK ----

const PI_KIND_VERSION: i64 = 2026103101;
/// The complete set the `pi` CHECK starts from: every kind at the hermes tip.
const KINDS_BEFORE_PI: [&str; 9] = [
    "claude_code",
    "cursor",
    "codex",
    "gemini",
    "opencode",
    "copilot",
    "factory_ai",
    "grok",
    "hermes",
];
/// The fixed old binary (DEP-PI-05): the signed v0.30.118 release, the last
/// one without the `pi` receipt. Its absolute path comes from this variable.
const PI_ROLE_ORACLE_ENV: &str = "LIBRA_TEST_PI_ORACLE_BINARY";
const PI_ROLE_ORACLE_VERSION: &str = "libra 0.30.118";
/// SHA-256 of each published v0.30.118 release binary, as listed in
/// `https://download.libra.tools/libra/releases/v0.30.118/<binary>.sha256`.
const PI_ROLE_ORACLE_SHA256: [(&str, &str, &str); 4] = [
    (
        "macos",
        "aarch64",
        "fc47a4ef22e86bd3983f9f6c8963d3e1b882bc88578ea49d9e49bd5caf78f326",
    ),
    (
        "linux",
        "x86_64",
        "28e95edd9596537dedcc858ba6cd95a45c7b5baa72a3a7467c0bceb7d6b6569b",
    ),
    (
        "linux",
        "aarch64",
        "03db130e9e76beea6b22a80829f13ae3ac68c4894e21264a6cb55e824307d699",
    ),
    (
        "windows",
        "x86_64",
        "02475bca63cc47f5c5f9128160517b4967e7a4bfbd3e0d23ab00f0247e5e1030",
    ),
];

/// A real repository (made by `libra init`) taken back to the hermes tip,
/// with a session of every earlier kind, every table that references
/// `agent_session` populated, and views and triggers that depend on it.
struct PiRoleRepo {
    root: TempDir,
    home: TempDir,
}

impl PiRoleRepo {
    fn db(&self) -> std::path::PathBuf {
        self.root.path().join(".libra").join("libra.db")
    }

    fn url(&self) -> String {
        format!("sqlite://{}", self.db().display())
    }

    /// `program status` in the repository: opening it runs the pending
    /// migrations through the production path.
    fn status_with(
        &self,
        program: &std::path::Path,
        envs: &[(&str, &str)],
    ) -> std::process::Output {
        let mut command = pi_role_libra(program, self.home.path());
        command.arg("status").current_dir(self.root.path());
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().expect("run libra status")
    }

    fn status(&self, envs: &[(&str, &str)]) -> std::process::Output {
        self.status_with(std::path::Path::new(env!("CARGO_BIN_EXE_libra")), envs)
    }

    fn run(&self, args: &[&str]) -> std::process::Output {
        pi_role_libra(
            std::path::Path::new(env!("CARGO_BIN_EXE_libra")),
            self.home.path(),
        )
        .args(args)
        .current_dir(self.root.path())
        .output()
        .expect("run libra")
    }

    fn run_with_env(&self, args: &[&str], envs: &[(&str, &str)]) -> std::process::Output {
        let mut command = pi_role_libra(
            std::path::Path::new(env!("CARGO_BIN_EXE_libra")),
            self.home.path(),
        );
        command.args(args).current_dir(self.root.path());
        for (key, value) in envs {
            command.env(key, value);
        }
        command.output().expect("run libra")
    }

    /// Every file under `.libra`, the rest of the worktree, and the isolated
    /// home (where the global and system configuration databases live), by
    /// relative path and content.
    fn files(&self) -> std::collections::BTreeMap<String, Vec<u8>> {
        let mut files = std::collections::BTreeMap::new();
        let libra_dir = self.root.path().join(".libra");
        for (label, base) in [
            ("repo", libra_dir.clone()),
            ("worktree", self.root.path().to_path_buf()),
            ("home", self.home.path().to_path_buf()),
        ] {
            let mut pending = vec![base.clone()];
            while let Some(dir) = pending.pop() {
                for entry in std::fs::read_dir(&dir).expect("read dir") {
                    let path = entry.expect("dir entry").path();
                    if label == "worktree" && path == libra_dir {
                        continue;
                    }
                    if path.is_dir() {
                        pending.push(path);
                    } else {
                        let relative = path.strip_prefix(&base).expect("under base");
                        files.insert(
                            format!("{label}/{}", relative.display()),
                            std::fs::read(&path).expect("read file"),
                        );
                    }
                }
            }
        }
        files
    }
}

/// `program` with an environment of its own under `home`: nothing the child
/// reads is inherited, and the global and system configuration databases
/// are files under `home`.
fn pi_role_libra(program: &std::path::Path, home: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("LIBRA_TEST", "1")
        .env("LIBRA_TEST_HOME", home)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("LIBRA_CONFIG_GLOBAL_DB", home.join("global-config.db"))
        .env("LIBRA_CONFIG_SYSTEM_DB", home.join("system-config.db"))
        .env("LIBRA_NO_HOOKS", "1");
    if let Some(root) = std::env::var_os("SYSTEMROOT") {
        command.env("SYSTEMROOT", root);
    }
    command
}

/// The paths added, removed or rewritten between two [`PiRoleRepo::files`]
/// snapshots.
fn pi_role_changed_files(
    before: &std::collections::BTreeMap<String, Vec<u8>>,
    after: &std::collections::BTreeMap<String, Vec<u8>>,
) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .filter(|path| before.get(*path) != after.get(*path))
        .cloned()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

async fn pi_role_exec(conn: &DatabaseConnection, sql: &str) {
    conn.execute_unprepared(sql)
        .await
        .unwrap_or_else(|error| panic!("exec {sql}: {error}"));
}

async fn pi_role_texts(conn: &DatabaseConnection, sql: &str) -> Vec<String> {
    conn.query_all_raw(Statement::from_string(conn.get_database_backend(), sql))
        .await
        .unwrap_or_else(|error| panic!("{sql}: {error}"))
        .into_iter()
        .map(|row| row.try_get_by_index::<String>(0).expect("text column"))
        .collect()
}

async fn pi_role_insert(conn: &DatabaseConnection, session_id: &str, kind: &str) -> ExecResult {
    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES (?, ?, ?, 'stopped', '/work', '{}', '{}', 1, 1)",
        [
            session_id.into(),
            kind.into(),
            format!("provider-{session_id}").into(),
        ],
    ))
    .await
    .unwrap_or_else(|error| panic!("insert {kind:?}: {error}"))
}

impl PiRoleRepo {
    /// A repository made by `libra init` at the current tip, in an isolated
    /// home.
    fn init() -> Self {
        let repo = PiRoleRepo {
            root: tempfile::tempdir().expect("repo dir"),
            home: tempfile::tempdir().expect("home dir"),
        };
        let init = repo.run(&["init"]);
        assert!(
            init.status.success(),
            "libra init: {}",
            String::from_utf8_lossy(&init.stderr)
        );
        repo
    }
}

async fn pi_role_repository() -> PiRoleRepo {
    let repo = PiRoleRepo::init();
    let conn = connect(&repo.url()).await;
    let rolled_back = hermes_runner()
        .rollback_to(&conn, HERMES_KIND_VERSION)
        .await
        .expect("take the repository back to the hermes tip");
    assert_eq!(rolled_back, vec![PI_KIND_VERSION]);
    for kind in KINDS_BEFORE_PI {
        pi_role_insert(&conn, &format!("sess-{kind}"), kind).await;
    }
    for sql in [
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES ('cp-1', 'sess-claude_code', 'temporary', 'parent', 'tree', 'meta', 'traces', 1)",
        "INSERT INTO agent_session_commit (session_id, hash_kind, commit_oid) VALUES ('sess-claude_code', 'sha1', 'aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa')",
        "INSERT INTO agent_coverage_claim (session_id, logical_turn_key, coverage_schema_version, coverage_digest, completeness, revision, state, source_channel, created_at, updated_at) VALUES ('sess-claude_code', 'turn-1', 1, 'digest', 'incomplete', 0, 'reserved_live', 'live', 1, 1)",
        "INSERT INTO agent_coverage_revision (session_id, logical_turn_key, coverage_schema_version, revision, checkpoint_id, coverage_digest, completeness, source_channel, created_at) VALUES ('sess-claude_code', 'turn-1', 1, 1, 'cp-1', 'digest', 'incomplete', 'live', 1)",
        "INSERT INTO agent_coverage_conflict (session_id, logical_turn_key, coverage_schema_version, incumbent_revision, incumbent_digest, incumbent_checkpoint_id, incoming_digest, incoming_source_channel, incoming_observed_at, incoming_canonical_json, incoming_redaction_report_json) VALUES ('sess-claude_code', 'turn-1', 1, 0, 'digest', 'cp-1', 'incoming', 'live', 1, '{}', '{}')",
        "INSERT INTO agent_subagent_content_claim (parent_session_id, provider_kind, source_key, content_schema_version, current_revision, state, fence_token, created_at, updated_at) VALUES ('sess-claude_code', 'claude_code', 'key', 1, 0, 'idle', 0, 1, 1)",
        "INSERT INTO agent_subagent_content_revision (parent_session_id, provider_kind, source_key, content_schema_version, revision, checkpoint_id, content_digest, source_channel, partial, created_at) VALUES ('sess-claude_code', 'claude_code', 'key', 1, 1, 'cp-1', 'digest', 'live', 0, 1)",
        "INSERT INTO agent_subagent_link (content_checkpoint_id, parent_session_id, link_state, boundary_checkpoint_id, stable_subagent_id, created_at, updated_at) VALUES ('cp-1', 'sess-claude_code', 'resolved', 'cp-1', 'sub-1', 1, 1)",
        "INSERT INTO agent_import_tombstone (tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at) VALUES ('tomb-1', 'codex', 'erased-provider', 'codex__erased', 1)",
        // Arms the rebuild's test failpoints for this database only.
        "CREATE TABLE libra_test_agent_kind_check_failpoints (armed INTEGER)",
        "CREATE VIEW pi01_session_kinds AS SELECT session_id, agent_kind FROM agent_session",
        "CREATE VIEW \"pi01 quoted \"\"view\"\"\" AS SELECT session_id FROM agent_session",
        "CREATE VIEW pi01_session_kinds_named AS SELECT session_id FROM pi01_session_kinds",
        "CREATE TABLE pi01_touch (session_id TEXT NOT NULL)",
        "CREATE TABLE \"pi01 child records\" (session_id TEXT NOT NULL REFERENCES agent_session(session_id) ON DELETE CASCADE)",
        "INSERT INTO \"pi01 child records\" (session_id) VALUES ('sess-hermes')",
        "CREATE TRIGGER pi01_touch_session AFTER INSERT ON pi01_touch BEGIN UPDATE agent_session SET last_event_at = last_event_at + 1 WHERE session_id = NEW.session_id; END",
    ] {
        pi_role_exec(&conn, sql).await;
    }
    // Every table that references agent_session holds a row the rebuild
    // must keep.
    let children = pi_role_texts(
        &conn,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name != 'agent_session' AND LOWER(sql) LIKE '%references%agent_session%' ORDER BY name",
    )
    .await;
    assert!(
        children.len() >= 8,
        "expected the cascaded agent_session children, got {children:?}"
    );
    for table in children {
        let rows = pi_role_texts(&conn, &format!("SELECT COUNT(*) || '' FROM `{table}`")).await;
        assert_ne!(rows, vec!["0".to_string()], "{table} is not populated");
    }
    conn.close().await.expect("close");
    repo
}

/// Every table's rows (by value), and the schema, of the repository.
#[derive(Debug, PartialEq, Eq)]
struct PiRoleDigest {
    rows: std::collections::BTreeMap<String, Vec<String>>,
    schema: Vec<String>,
}

impl PiRoleDigest {
    /// The rows of every table except the schema_versions ledger.
    fn data(&self) -> std::collections::BTreeMap<String, Vec<String>> {
        let mut rows = self.rows.clone();
        rows.remove("schema_versions");
        rows
    }

    /// Indexes, triggers and views: everything but the tables themselves.
    fn dependents(&self) -> Vec<String> {
        self.schema
            .iter()
            .filter(|entry| !entry.starts_with("table|"))
            .cloned()
            .collect()
    }
}

async fn pi_role_digest(repo: &PiRoleRepo) -> PiRoleDigest {
    let conn = connect(&repo.url()).await;
    let mut rows = std::collections::BTreeMap::new();
    for table in pi_role_texts(
        &conn,
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
    )
    .await
    {
        let columns = pi_role_texts(
            &conn,
            &format!(
                "SELECT name FROM pragma_table_info('{}')",
                table.replace('\'', "''")
            ),
        )
        .await;
        let row = columns
            .iter()
            .map(|column| format!("quote(`{column}`)"))
            .collect::<Vec<_>>()
            .join(" || char(31) || ");
        let values = pi_role_texts(
            &conn,
            &format!("SELECT {row} AS r FROM `{table}` ORDER BY r"),
        )
        .await;
        rows.insert(table, values);
    }
    let schema = pi_role_texts(
        &conn,
        "SELECT type || '|' || name || '|' || COALESCE(sql, '') FROM sqlite_master ORDER BY type, name",
    )
    .await;
    conn.close().await.expect("close");
    PiRoleDigest { rows, schema }
}

/// The quoted values of the `agent_kind IN (...)` list in the
/// agent_session DDL, in declaration order.
async fn pi_role_check_values(repo: &PiRoleRepo) -> Vec<String> {
    let conn = connect(&repo.url()).await;
    let ddl = pi_role_texts(
        &conn,
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'agent_session'",
    )
    .await
    .concat();
    conn.close().await.expect("close");
    let start = ddl
        .find("`agent_kind` IN (")
        .unwrap_or_else(|| panic!("no agent_kind IN list: {ddl}"))
        + "`agent_kind` IN (".len();
    let end = start + ddl[start..].find(')').expect("closing parenthesis");
    ddl[start..end]
        .split(',')
        .map(|value| {
            let value = value.trim();
            assert!(
                value.len() >= 2 && value.starts_with('\'') && value.ends_with('\''),
                "unquoted CHECK value {value:?} in {ddl}"
            );
            value[1..value.len() - 1].to_string()
        })
        .collect()
}

async fn pi_role_receipts(repo: &PiRoleRepo) -> Vec<String> {
    let conn = connect(&repo.url()).await;
    let receipts = pi_role_texts(
        &conn,
        &format!(
            "SELECT version || '|' || name FROM schema_versions WHERE version = {PI_KIND_VERSION}"
        ),
    )
    .await;
    conn.close().await.expect("close");
    receipts
}

/// After a complete rebuild: one receipt, the CHECK lists every earlier kind
/// and `pi`, every row, index, trigger and view as before, and a clean
/// foreign-key and integrity check with foreign_keys ON.
async fn assert_pi_rebuilt(repo: &PiRoleRepo, before: &PiRoleDigest) {
    assert_eq!(
        pi_role_receipts(repo).await,
        vec![format!("{PI_KIND_VERSION}|agent_kind_pi")],
        "the pi receipt is recorded once"
    );
    let mut expected: Vec<String> = KINDS_BEFORE_PI
        .iter()
        .map(|kind| kind.to_string())
        .collect();
    expected.push("pi".to_string());
    assert_eq!(pi_role_check_values(repo).await, expected);
    let after = pi_role_digest(repo).await;
    assert_eq!(
        after.data(),
        before.data(),
        "every row is kept, value for value"
    );
    assert_eq!(
        after.dependents(),
        before.dependents(),
        "indexes, triggers and views"
    );
    let conn = connect(&repo.url()).await;
    pi_role_exec(&conn, "PRAGMA foreign_keys = ON").await;
    assert!(
        pi_role_texts(&conn, "SELECT 'x' FROM pragma_foreign_key_check")
            .await
            .is_empty()
    );
    assert_eq!(
        pi_role_texts(&conn, "PRAGMA integrity_check").await,
        vec!["ok".to_string()]
    );
    conn.close().await.expect("close");
}

/// PI-01 AC1/AC2: the new migration takes a unique ID above the hermes tip,
/// and its CHECK accepts exactly the nine kinds it started from plus `pi`.
#[tokio::test]
async fn pi_role_check_adds_only_pi_above_the_tip() {
    let migrations = builtin_migrations();
    let versions: Vec<i64> = migrations
        .iter()
        .map(|migration| migration.version)
        .collect();
    assert!(
        versions.windows(2).all(|pair| pair[0] < pair[1]),
        "migration IDs are unique and increasing: {versions:?}"
    );
    let pi = migrations.last().expect("the registry has a tip");
    assert_eq!((pi.version, pi.name), (PI_KIND_VERSION, "agent_kind_pi"));
    assert!(pi.version > HERMES_KIND_VERSION);
    assert!(pi.up.contains("RAISE(ABORT"));
    assert!(pi.down.expect("down sql").contains("RAISE(ABORT"));

    let repo = pi_role_repository().await;
    let before = pi_role_digest(&repo).await;
    assert_eq!(pi_role_check_values(&repo).await, KINDS_BEFORE_PI);
    // The SQL body alone aborts instead of rebuilding the table.
    let conn = connect(&repo.url()).await;
    assert!(conn.execute_unprepared(pi.up).await.is_err());
    conn.close().await.expect("close");
    assert_eq!(pi_role_digest(&repo).await, before);

    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_pi_rebuilt(&repo, &before).await;

    let conn = connect(&repo.url()).await;
    for kind in KINDS_BEFORE_PI.iter().chain(&["pi"]) {
        pi_role_insert(&conn, &format!("new-{kind}"), kind).await;
    }
    for unknown in [
        "vogon",
        "PI",
        "Pi",
        "pi ",
        " pi",
        "",
        "claude-code",
        "pi_agent",
    ] {
        let rejected = conn
            .execute_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES ('sess-unknown', ?, 'p', 'stopped', '/work', '{}', '{}', 1, 1)",
                [unknown.into()],
            ))
            .await;
        let error = rejected.expect_err("an unknown kind is rejected");
        assert!(
            error.to_string().contains("CHECK constraint failed"),
            "{unknown:?}: {error}"
        );
    }
    conn.close().await.expect("close");
}

/// PI-01 AC3: the enum, its db and CLI round trips, and the CHECK agree.
#[tokio::test]
async fn pi_role_enum_and_parsers_match_the_check() {
    use libra::internal::ai::observed_agents::{AgentKind, registration_for};

    assert_eq!(AgentKind::Pi.as_db_str(), "pi");
    assert_eq!(AgentKind::Pi.as_cli_slug(), "pi");
    for kind in AgentKind::all() {
        assert_eq!(AgentKind::from_db_str(kind.as_db_str()), Some(*kind));
        assert_eq!(AgentKind::from_cli_slug(kind.as_cli_slug()), Some(*kind));
        assert_eq!(registration_for(*kind).db_value, kind.as_db_str());
    }

    let repo = pi_role_repository().await;
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let check = pi_role_check_values(&repo).await;
    let enum_values: Vec<String> = AgentKind::all()
        .iter()
        .map(|kind| kind.as_db_str().to_string())
        .collect();
    assert_eq!(check, enum_values, "CHECK and AgentKind::all() agree");
    for value in &check {
        assert!(
            AgentKind::from_db_str(value).is_some(),
            "{value} parses back"
        );
    }
}

/// PI-01 AC4: every read and install path of the `pi` stub refuses with an
/// unsupported result, and no exhaustive dispatch panics on the new kind.
#[tokio::test]
async fn pi_role_stub_refuses_every_read_and_install() {
    use libra::internal::ai::{
        capture::deadline_provider_extraction_summary,
        observed_agents::{
            AgentKind, AgentSessionCtx, SlugLookup, agent_for, extract::skill_registry_for,
            is_preview, live_capture_matches_hook_installable, lookup_cli_slug, registration_for,
            supported_slugs, truncator_for,
        },
    };

    let row = registration_for(AgentKind::Pi);
    assert!(row.registered);
    assert!(!row.supported);
    assert_eq!(row.support_wave, None);
    assert!(!row.transcript_readable);
    assert!(!row.hook_installable);
    assert!(!row.installed);
    assert!(!row.launchable_review);
    assert!(!row.launchable_investigate);
    assert!(!row.external_binary);
    assert!(!row.capabilities.hooks);
    assert!(row.config_paths.is_empty());
    assert!(!supported_slugs().contains(&"pi"));
    assert!(matches!(lookup_cli_slug("pi"), SlugLookup::Known(found) if !found.supported));
    assert!(!is_preview(AgentKind::Pi));
    assert!(live_capture_matches_hook_installable(AgentKind::Pi));
    assert!(skill_registry_for(AgentKind::Pi).is_empty());
    assert!(truncator_for(AgentKind::Pi).is_none());
    let agent = agent_for(AgentKind::Pi);
    assert_eq!(agent.provider_kind(), AgentKind::Pi);
    assert!(agent.as_hooks().is_none());
    assert_eq!(agent.protected_dirs(), &[".pi"]);
    let read = agent
        .read_transcript(&AgentSessionCtx {
            session_id: "s".to_string(),
            provider_session_id: "p".to_string(),
            working_dir: std::path::PathBuf::from("/work"),
            transcript_path: None,
        })
        .expect_err("pi capture stays disabled");
    assert!(read.to_string().contains("not available"), "{read}");
    let summary = deadline_provider_extraction_summary(AgentKind::Pi, b"pi-thought-canary");
    assert!(summary.partial);
    assert!(summary.prompts.is_empty());
    assert!(
        summary
            .warnings
            .iter()
            .all(|warning| !warning.contains("pi-thought-canary"))
    );

    let repo = pi_role_repository().await;
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    // On the migrated schema, every refusal (and the read-only listing)
    // leaves every file of the worktree, `.libra` and the isolated home as
    // it was. The only exception is the holder line of the object-index
    // repair preflight's generation lock, which every repository command
    // rewrites before dispatch, whatever the command.
    let changed = |before: &std::collections::BTreeMap<String, Vec<u8>>| {
        pi_role_changed_files(before, &repo.files())
            .into_iter()
            .filter(|path| {
                path != "repo/object-index-repair-locks/object-index-repair-generation.lock"
            })
            .collect::<Vec<_>>()
    };
    let refuses_without_writes = |args: &[&str], expected: &[&str]| {
        let before = repo.files();
        let refused = repo.run(args);
        assert!(!refused.status.success(), "{args:?} must fail");
        let stderr = String::from_utf8_lossy(&refused.stderr);
        for text in expected {
            assert!(stderr.contains(text), "{args:?}: {stderr}");
        }
        assert_eq!(
            changed(&before),
            Vec::<String>::new(),
            "{args:?} changed a file"
        );
    };
    let unsupported = [
        "agent 'pi' is not in the supported roster",
        "cannot be enabled or install hooks",
    ];
    refuses_without_writes(&["agent", "enable", "--agent", "pi"], &unsupported);
    refuses_without_writes(&["agent", "add", "pi"], &unsupported);
    let no_hooks = [
        "agent 'pi' is not in the supported roster",
        "has no hooks to remove",
    ];
    refuses_without_writes(&["agent", "disable", "--agent", "pi"], &no_hooks);
    refuses_without_writes(&["agent", "remove", "pi"], &no_hooks);
    refuses_without_writes(
        &["agent", "import", "--agent", "pi", "--all"],
        &["Pi import requires --path to one <timestamp>_<session-id>.jsonl session file"],
    );
    // `pi` is a built-in slug: an external libra-agent-pi binary is never
    // trusted or invoked in its place. The opt-in itself is the one intended
    // write.
    let opt_in = repo.run(&["config", "set", "agent.external_agents.enabled", "true"]);
    assert!(
        opt_in.status.success(),
        "{}",
        String::from_utf8_lossy(&opt_in.stderr)
    );
    let impersonation = ["LBR-AGENT-006", "'pi' is a built-in agent slug"];
    refuses_without_writes(&["agent", "rpc", "trust", "pi"], &impersonation);
    refuses_without_writes(&["agent", "rpc", "invoke", "pi", "info"], &impersonation);
    let before = repo.files();
    let listed = repo.run(&["--json", "agent", "list"]);
    assert!(
        listed.status.success(),
        "{}",
        String::from_utf8_lossy(&listed.stderr)
    );
    let listing: serde_json::Value =
        serde_json::from_slice(&listed.stdout).expect("agent list --json");
    let agents = listing["data"]["agents"].as_array().expect("agents array");
    assert!(!agents.iter().any(|agent| agent["slug"] == "pi"));
    assert_eq!(
        changed(&before),
        Vec::<String>::new(),
        "agent list changed a file"
    );
    let conn = connect(&repo.url()).await;
    assert_eq!(
        pi_role_texts(
            &conn,
            "SELECT COUNT(*) || '' FROM agent_session WHERE agent_kind = 'pi'"
        )
        .await,
        vec!["0".to_string()]
    );
    conn.close().await.expect("close");
    assert!(!repo.home.path().join(".pi").exists());
    assert!(!repo.root.path().join(".pi").exists());
}

/// PI-01 AC4, as PI-10 changed it: the public prepared-import API refused a
/// `pi` request until PI-10 made `pi` importable. On the migrated schema it
/// now stores the `pi` session the CLI also stores; the CHECK accepts the row.
#[tokio::test]
async fn pi_role_prepared_import_stores_a_pi_session() {
    use std::collections::BTreeMap;

    use libra::internal::ai::{
        agent_import::{ImportRequest, TurnBoundary, import_prepared_with_coverage_versions},
        capture::snapshot::{
            CaptureSnapshotCompleteness, CaptureSnapshotProjection, CaptureSnapshotSource,
            CaptureSnapshotSourceKind,
        },
        capture_scope::CaptureScope,
        observed_agents::{AgentKind, Completeness, NormalizedTurn, SemanticRecord},
    };

    let repo = pi_role_repository().await;
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let before = pi_role_digest(&repo).await;
    let files = repo.files();
    let conn = connect(&repo.url()).await;
    let scope = CaptureScope::main_for_connection(&conn)
        .await
        .expect("capture scope");
    let turn = NormalizedTurn {
        logical_turn_key: "pi-turn".to_owned(),
        ordinal: 0,
        completeness: Completeness::Complete,
        started_at: None,
        ended_at: None,
        records: vec![SemanticRecord::User {
            text: "fixed pi prompt".to_owned(),
        }],
    };
    let commitment = format!("source/hmac-v2/{}", "c".repeat(64));
    let request = ImportRequest {
        agent_kind: AgentKind::Pi,
        provider_session_id: "pi-stored".to_owned(),
        session_id: "pi__pi-stored".to_owned(),
        source_kind: "file".to_owned(),
        source_id: commitment.clone(),
        identity_schema_version: 2,
        content_digest: "caller digest".to_owned(),
        started_at: 1,
        ended_at: 2,
        session_state: "stopped".to_owned(),
        stopped_at: Some(2),
        working_dir: repo.root.path().to_path_buf(),
        repository_identity: "not_retained:v1".to_owned(),
        capture_scope: Some(scope),
        source_fingerprint: commitment.clone(),
        existing_session_fingerprint: None,
        redaction_report: serde_json::json!({"pipeline":"typed_allowlist","raw_persisted":false,"matches":[],"bytes_scanned":15,"bytes_redacted":0}),
        transcript_snapshot: Some(CaptureSnapshotProjection {
            completeness: CaptureSnapshotCompleteness::Complete,
            partial_reason: None,
            source: Some(CaptureSnapshotSource {
                kind: CaptureSnapshotSourceKind::ProviderFile,
                identity: "not_retained:v1".to_owned(),
                digest_sha256: Some(commitment),
                byte_len: Some(15),
            }),
            transcript_redacted_bytes: 15,
            redaction_match_count: 0,
            redaction_bytes_scanned: 15,
            redaction_bytes_redacted: 0,
        }),
        turn_boundaries: BTreeMap::from([(
            "pi-turn".to_owned(),
            TurnBoundary {
                started_at: 1,
                ended_at: 2,
            },
        )]),
        turns: vec![turn],
    };
    let storage = repo
        .root
        .path()
        .canonicalize()
        .expect("canonical repository root")
        .join(".libra");
    let imported = import_prepared_with_coverage_versions(
        &conn,
        &storage,
        request,
        &[2],
        std::time::Duration::from_secs(30),
        Some(std::path::Path::new(env!("CARGO_BIN_EXE_libra"))),
    )
    .await
    .expect("a pi import is stored");
    assert_eq!(imported.checkpoints_written, 1);
    assert_eq!(
        pi_role_texts(
            &conn,
            "SELECT agent_kind FROM agent_session WHERE agent_kind = 'pi'"
        )
        .await,
        vec!["pi".to_owned()]
    );
    conn.close().await.expect("close");
    assert_ne!(
        pi_role_digest(&repo).await,
        before,
        "the pi session was written"
    );
    assert_ne!(repo.files(), files, "the checkpoint objects were written");
}

/// PI-01 AC6/AC7: the rebuild runs on the PI-07 dedicated connection (the
/// SQL body alone aborts, and the dedicated path's own failpoint signals),
/// and keeps every row and dependent.
#[tokio::test]
async fn pi_role_rebuild_keeps_every_row_and_dependent() {
    let repo = pi_role_repository().await;
    let before = pi_role_digest(&repo).await;
    let signal = repo.home.path().join("signal");
    let signal_text = signal.display().to_string();
    let opened = repo.status(&[
        ("LIBRA_TEST_AGENT_KIND_CHECK_SIGNAL_AT", "foreign_keys_off"),
        ("LIBRA_TEST_AGENT_KIND_CHECK_SIGNAL_FILE", &signal_text),
    ]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_eq!(
        std::fs::read_to_string(&signal).expect("the dedicated path signalled"),
        "foreign_keys_off",
        "the pi rebuild turned foreign_keys off on its own connection"
    );
    assert_pi_rebuilt(&repo, &before).await;
}

/// How a failure injected at one stage of the rebuild must end.
enum PiRoleOutcome {
    /// Nothing changed: the whole database equals the fixture.
    Unchanged(&'static str),
    /// The rebuild committed before the failure: it is complete.
    Committed(&'static str),
}

/// PI-01 AC7/AC8: a failure injected at any stage of the rebuild, with only the
/// `pi` CHECK pending, leaves either the whole database exactly as it was or
/// the complete rebuild, and says which; the next open finishes it.
#[tokio::test]
async fn pi_role_rebuild_fails_cleanly_at_every_stage() {
    let rolled_back = "agent_kind pi CHECK migration was rolled back and changed nothing (the CHECK and its schema_versions receipt are as before)";
    let cases: [(&str, PiRoleOutcome); 16] = [
        (
            "lookup",
            PiRoleOutcome::Unchanged(
                "could not find the repository database file for the agent_kind CHECK migration (injected agent_kind CHECK failure at lookup); nothing was changed",
            ),
        ),
        (
            "foreign_keys",
            PiRoleOutcome::Unchanged(
                "agent_kind pi CHECK migration could not read or turn off foreign_keys on its connection (injected agent_kind CHECK failure at foreign_keys); nothing was changed and the connection was closed",
            ),
        ),
        (
            "foreign_keys_off",
            PiRoleOutcome::Unchanged(
                "could not turn foreign_keys off before rebuilding agent_session; the CHECK was not changed and the connection was closed",
            ),
        ),
        (
            "begin",
            PiRoleOutcome::Unchanged(
                "agent_kind pi CHECK migration could not start its transaction (injected agent_kind CHECK failure at begin); nothing was changed and the connection was closed",
            ),
        ),
        ("claiming", PiRoleOutcome::Unchanged(rolled_back)),
        ("claimed", PiRoleOutcome::Unchanged(rolled_back)),
        ("created", PiRoleOutcome::Unchanged(rolled_back)),
        ("copied", PiRoleOutcome::Unchanged(rolled_back)),
        ("dropped", PiRoleOutcome::Unchanged(rolled_back)),
        ("renamed", PiRoleOutcome::Unchanged(rolled_back)),
        ("restored", PiRoleOutcome::Unchanged(rolled_back)),
        ("verified", PiRoleOutcome::Unchanged(rolled_back)),
        (
            "commit",
            PiRoleOutcome::Unchanged(
                "agent_kind pi CHECK migration ended with an unknown outcome (its commit failed: injected agent_kind CHECK failure at commit)",
            ),
        ),
        (
            "copied,rollback",
            PiRoleOutcome::Unchanged(
                "agent_kind pi CHECK migration ended with an unknown outcome (injected agent_kind CHECK failure at copied, and then its rollback failed: injected agent_kind CHECK failure at rollback)",
            ),
        ),
        (
            "committed",
            PiRoleOutcome::Committed(
                "agent_kind pi CHECK migration committed and was not rolled back, but the check with foreign_keys=ON afterwards failed: injected agent_kind CHECK failure at committed",
            ),
        ),
        (
            "restore",
            PiRoleOutcome::Committed(
                "agent_kind pi CHECK migration committed (it was not rolled back), but foreign_keys could not be restored on its connection, which was closed instead of being returned",
            ),
        ),
    ];
    // Failures that change nothing share one fixture; each commit needs its own.
    let unchanged_repo = pi_role_repository().await;
    let fixture = pi_role_digest(&unchanged_repo).await;
    for (stages, outcome) in cases {
        let committed_repo;
        let repo = match outcome {
            PiRoleOutcome::Unchanged(_) => &unchanged_repo,
            PiRoleOutcome::Committed(_) => {
                committed_repo = pi_role_repository().await;
                &committed_repo
            }
        };
        let before = pi_role_digest(repo).await;
        let failed = repo.status(&[("LIBRA_TEST_AGENT_KIND_CHECK_FAIL_AT", stages)]);
        assert!(!failed.status.success(), "{stages}: the open fails");
        let stderr = String::from_utf8_lossy(&failed.stderr);
        match outcome {
            PiRoleOutcome::Unchanged(text) => {
                assert!(stderr.contains(text), "{stages}: {stderr}");
                assert_eq!(
                    pi_role_digest(repo).await,
                    before,
                    "{stages}: the whole repository database is as before"
                );
            }
            PiRoleOutcome::Committed(text) => {
                assert!(stderr.contains(text), "{stages}: {stderr}");
                assert!(
                    !stderr.contains("rolled back and changed nothing"),
                    "{stages}: {stderr}"
                );
                assert_pi_rebuilt(repo, &before).await;
                let opened = repo.status(&[]);
                assert!(
                    opened.status.success(),
                    "{stages}: {}",
                    String::from_utf8_lossy(&opened.stderr)
                );
                assert_pi_rebuilt(repo, &before).await;
            }
        }
    }
    let opened = unchanged_repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_pi_rebuilt(&unchanged_repo, &fixture).await;
}

/// The child was killed by `abort()` at the stage it signalled.
fn assert_pi_role_aborted_at(output: &std::process::Output, signal: &std::path::Path, stage: &str) {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(
            output.status.signal(),
            Some(6),
            "{stage}: the process aborts (SIGABRT): {:?} {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(not(unix))]
    assert!(
        !output.status.success() && output.status.code() != Some(128),
        "{stage}: the process aborts rather than failing: {:?}",
        output.status
    );
    assert_eq!(
        std::fs::read_to_string(signal).expect("the stage signalled before the abort"),
        stage,
        "{stage}: the abort happened at the injected stage"
    );
    std::fs::remove_file(signal).expect("clear the signal");
}

/// PI-01 AC7: a crash at any stage of the rebuild keeps either the old
/// database or the complete rebuild; the next open finishes once.
#[tokio::test]
async fn pi_role_rebuild_survives_a_crash_at_every_stage() {
    let repo = pi_role_repository().await;
    let before = pi_role_digest(&repo).await;
    let signal = repo.home.path().join("signal");
    let signal_text = signal.display().to_string();
    // Every stage before the commit, and a rollback after a failed step.
    for (abort_at, fail_at) in [
        ("lookup", ""),
        ("foreign_keys", ""),
        ("foreign_keys_off", ""),
        ("begin", ""),
        ("claiming", ""),
        ("claimed", ""),
        ("created", ""),
        ("copied", ""),
        ("dropped", ""),
        ("renamed", ""),
        ("restored", ""),
        ("verified", ""),
        ("commit", ""),
        ("rollback", "copied"),
    ] {
        let crashed = repo.status(&[
            ("LIBRA_TEST_AGENT_KIND_CHECK_ABORT_AT", abort_at),
            ("LIBRA_TEST_AGENT_KIND_CHECK_SIGNAL_AT", abort_at),
            ("LIBRA_TEST_AGENT_KIND_CHECK_SIGNAL_FILE", &signal_text),
            ("LIBRA_TEST_AGENT_KIND_CHECK_FAIL_AT", fail_at),
        ]);
        assert_pi_role_aborted_at(&crashed, &signal, abort_at);
        // Opening the database rolls the interrupted transaction back.
        assert_eq!(
            pi_role_digest(&repo).await,
            before,
            "{abort_at}: a crash before the commit leaves the database as before"
        );
    }
    // After the commit: right after it, and while restoring foreign_keys.
    for abort_at in ["committed", "restore"] {
        let repo = pi_role_repository().await;
        let before = pi_role_digest(&repo).await;
        let signal = repo.home.path().join("signal");
        let signal_text = signal.display().to_string();
        let crashed = repo.status(&[
            ("LIBRA_TEST_AGENT_KIND_CHECK_ABORT_AT", abort_at),
            ("LIBRA_TEST_AGENT_KIND_CHECK_SIGNAL_AT", abort_at),
            ("LIBRA_TEST_AGENT_KIND_CHECK_SIGNAL_FILE", &signal_text),
        ]);
        assert_pi_role_aborted_at(&crashed, &signal, abort_at);
        assert_pi_rebuilt(&repo, &before).await;
        let committed = pi_role_digest(&repo).await;
        let opened = repo.status(&[]);
        assert!(
            opened.status.success(),
            "{abort_at}: {}",
            String::from_utf8_lossy(&opened.stderr)
        );
        assert_eq!(
            pi_role_digest(&repo).await,
            committed,
            "{abort_at}: the open after the crash finds the rebuild done"
        );
    }
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_pi_rebuilt(&repo, &before).await;
}

/// PI-01 AC9: the `pi` receipt is Repository-only. Upgrading a repository
/// writes neither configuration database, and no other role registers or
/// takes the migration.
#[tokio::test]
async fn pi_role_writes_no_configuration_or_derived_database() {
    use libra::internal::db::{DatabaseRole, schema};

    for role in [
        DatabaseRole::GlobalConfig,
        DatabaseRole::SystemConfig,
        DatabaseRole::Derived,
    ] {
        assert!(
            schema::migrations_for_role(role)
                .iter()
                .all(|migration| migration.version != PI_KIND_VERSION),
            "{role:?} must not register the pi receipt"
        );
    }
    assert!(
        schema::migrations_for_role(DatabaseRole::Repository)
            .iter()
            .any(|migration| migration.version == PI_KIND_VERSION)
    );

    let repo = pi_role_repository().await;
    // Configuration databases with content, so a write of any kind shows.
    let mut configs = Vec::new();
    for (name, role) in [
        ("global-config.db", DatabaseRole::GlobalConfig),
        ("system-config.db", DatabaseRole::SystemConfig),
    ] {
        let path = repo.home.path().join(name);
        std::fs::File::create(&path).expect("create configuration database");
        schema::upgrade_database_schema_for_role(&path, role)
            .await
            .unwrap_or_else(|error| panic!("{role:?} configuration database: {error}"));
        configs.push(path);
    }
    let set = repo.run(&["config", "--global", "user.name", "Pi Role"]);
    assert!(
        set.status.success(),
        "{}",
        String::from_utf8_lossy(&set.stderr)
    );
    let home_before: Vec<(String, Vec<u8>)> = repo
        .files()
        .into_iter()
        .filter(|(path, _)| path.starts_with("home/"))
        .collect();
    assert!(
        home_before
            .iter()
            .any(|(path, _)| path == "home/global-config.db"),
        "{:?}",
        home_before.iter().map(|(path, _)| path).collect::<Vec<_>>()
    );

    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_eq!(pi_role_receipts(&repo).await.len(), 1);
    let home_after: Vec<(String, Vec<u8>)> = repo
        .files()
        .into_iter()
        .filter(|(path, _)| path.starts_with("home/"))
        .collect();
    assert_eq!(
        home_after, home_before,
        "the upgrade wrote nothing under the isolated home"
    );
    for path in &configs {
        let conn = connect(&format!("sqlite://{}", path.display())).await;
        let tables = pi_role_texts(
            &conn,
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name IN ('agent_session', 'schema_versions')",
        )
        .await;
        assert!(
            !tables.contains(&"agent_session".to_string()),
            "{}: {tables:?}",
            path.display()
        );
        // A configuration ledger, where there is one, never carries the
        // repository receipt.
        if tables.contains(&"schema_versions".to_string()) {
            let receipts = pi_role_texts(
                &conn,
                &format!(
                    "SELECT version || '' FROM schema_versions WHERE version = {PI_KIND_VERSION}"
                ),
            )
            .await;
            assert!(receipts.is_empty(), "{}: {receipts:?}", path.display());
        }
        conn.close().await.expect("close");
    }

    let derived = repo.home.path().join("derived.db");
    std::fs::write(&derived, b"derived-marker").expect("seed derived");
    let refused = schema::upgrade_database_schema_for_role(&derived, DatabaseRole::Derived).await;
    assert!(refused.is_err(), "derived role must not take a migration");
    assert_eq!(
        std::fs::read(&derived).expect("derived after refusal"),
        b"derived-marker"
    );
}

/// The fixed pre-migration oracle (DEP-PI-05), checked before any use: a
/// missing variable, another binary, or another version fails the gate.
fn pi_role_oracle(home: &std::path::Path) -> std::path::PathBuf {
    use sha2::{Digest, Sha256};

    let oracle = std::env::var_os(PI_ROLE_ORACLE_ENV).unwrap_or_else(|| {
        panic!(
            "set {PI_ROLE_ORACLE_ENV} to the absolute path of the signed {PI_ROLE_ORACLE_VERSION} release binary"
        )
    });
    let oracle = std::path::PathBuf::from(oracle);
    assert!(
        oracle.is_absolute(),
        "{PI_ROLE_ORACLE_ENV} must be absolute"
    );
    let (_, _, expected) = PI_ROLE_ORACLE_SHA256
        .iter()
        .find(|(os, arch, _)| *os == std::env::consts::OS && *arch == std::env::consts::ARCH)
        .unwrap_or_else(|| {
            panic!(
                "no published {PI_ROLE_ORACLE_VERSION} binary for {}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        });
    let bytes = std::fs::read(&oracle).expect("read the oracle binary");
    assert_eq!(
        hex::encode(Sha256::digest(&bytes)),
        *expected,
        "{} is not the published {PI_ROLE_ORACLE_VERSION} release binary",
        oracle.display()
    );
    let version = pi_role_libra(&oracle, home)
        .arg("--version")
        .output()
        .expect("run the oracle");
    assert_eq!(
        String::from_utf8_lossy(&version.stdout).trim(),
        PI_ROLE_ORACLE_VERSION,
        "the oracle is the fixed release without the pi receipt"
    );
    oracle
}

/// The oracle refuses every repository command with the fixed message and
/// changes no file under `.libra` or the isolated home.
fn assert_pi_role_oracle_refuses(repo: &PiRoleRepo, oracle: &std::path::Path) {
    let before = repo.files();
    for args in [
        &["status"][..],
        &["log"][..],
        &["agent", "session", "list"][..],
        &["config", "--list"][..],
    ] {
        let refused = pi_role_libra(oracle, repo.home.path())
            .args(args)
            .current_dir(repo.root.path())
            .output()
            .expect("run the oracle");
        assert!(
            !refused.status.success(),
            "{args:?}: the old binary refuses"
        );
        let stderr = String::from_utf8_lossy(&refused.stderr);
        assert!(
            stderr.contains(&format!(
                "repository database schema version {PI_KIND_VERSION} is newer than this Libra binary supports (latest supported: {HERMES_KIND_VERSION}); install a newer Libra binary"
            )),
            "{args:?}: {stderr}"
        );
        assert_eq!(
            repo.files(),
            before,
            "{args:?}: no file under .libra or the home changed"
        );
    }
}

/// PI-01 AC10 (DEP-PI-05, run first): the fixed pre-migration binary refuses
/// a repository whose ledger carries the `2026103101` receipt written by the
/// fixture rather than by the migration, and writes nothing. It needs the
/// released binary, so it runs only on request:
/// `LIBRA_TEST_PI_ORACLE_BINARY=<path> cargo test --test agent_capture_migration_test
/// pi_role_oracle_refuses_a_fixture_receipt_without_writes -- --ignored --exact`.
#[tokio::test]
#[ignore = "needs the signed v0.30.118 release binary in LIBRA_TEST_PI_ORACLE_BINARY"]
async fn pi_role_oracle_refuses_a_fixture_receipt_without_writes() {
    let repo = PiRoleRepo::init();
    let oracle = pi_role_oracle(repo.home.path());
    let conn = connect(&repo.url()).await;
    let runner = hermes_runner();
    // Without the real migration: back to the hermes tip when this build
    // registers the pi migration, then a bare receipt row.
    if runner
        .max_registered_version()
        .is_some_and(|tip| tip > HERMES_KIND_VERSION)
    {
        runner
            .rollback_to(&conn, HERMES_KIND_VERSION)
            .await
            .expect("take the repository back to the hermes tip");
    }
    pi_role_exec(
        &conn,
        &format!(
            "INSERT INTO schema_versions (version, name, applied_at) VALUES ({PI_KIND_VERSION}, 'agent_kind_pi', '2026-10-31T00:00:00Z')"
        ),
    )
    .await;
    conn.close().await.expect("close");
    assert_pi_role_oracle_refuses(&repo, &oracle);
}

/// PI-01 AC11 (DEP-PI-05): after the real `2026103101` migration, with a `pi`
/// session, the same fixed binary refuses the repository and writes nothing.
#[tokio::test]
#[ignore = "needs the signed v0.30.118 release binary in LIBRA_TEST_PI_ORACLE_BINARY"]
async fn pi_role_oracle_refuses_the_migrated_repository_without_writes() {
    let repo = pi_role_repository().await;
    let oracle = pi_role_oracle(repo.home.path());
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let conn = connect(&repo.url()).await;
    pi_role_insert(&conn, "sess-pi", "pi").await;
    conn.close().await.expect("close");
    assert_eq!(pi_role_receipts(&repo).await.len(), 1);
    assert_pi_role_oracle_refuses(&repo, &oracle);
}

/// PI-01 AC7: once the rebuild is complete, opening the repository again and
/// a repeated in-process up change nothing; the receipt stays single.
#[tokio::test]
async fn pi_role_repeat_up_changes_nothing() {
    let repo = pi_role_repository().await;
    let before = pi_role_digest(&repo).await;
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_pi_rebuilt(&repo, &before).await;
    let rebuilt = pi_role_digest(&repo).await;
    let again = repo.status(&[]);
    assert!(
        again.status.success(),
        "{}",
        String::from_utf8_lossy(&again.stderr)
    );
    assert_eq!(pi_role_digest(&repo).await, rebuilt, "a repeated open");
    let conn = connect(&repo.url()).await;
    let applied = hermes_runner().run_pending(&conn).await.expect("repeat up");
    assert!(applied.is_empty(), "repeat up must be a no-op: {applied:?}");
    conn.close().await.expect("close");
    assert_eq!(pi_role_digest(&repo).await, rebuilt, "a repeated up");
}

/// Count TCP connections to a loopback listener that closes each one at once.
struct PiRoleConnectionCounter {
    url: String,
    count: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl PiRoleConnectionCounter {
    fn start() -> Self {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        };

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind counter");
        listener.set_nonblocking(true).expect("nonblocking counter");
        let url = format!("http://{}", listener.local_addr().expect("counter addr"));
        let count = Arc::new(AtomicUsize::new(0));
        let stop = Arc::new(AtomicBool::new(false));
        let worker = {
            let count = count.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    match listener.accept() {
                        Ok((stream, _)) => {
                            count.fetch_add(1, Ordering::SeqCst);
                            drop(stream);
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            std::thread::sleep(std::time::Duration::from_millis(5));
                        }
                        Err(_) => break,
                    }
                }
            })
        };
        Self {
            url,
            count,
            stop,
            worker: Some(worker),
        }
    }

    fn connections(&self) -> usize {
        self.count.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl Drop for PiRoleConnectionCounter {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::SeqCst);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// PI-01 AC5 (ADR-PI-WRITE): on the real migrated Repository schema, a
/// repository whose only capture session is `pi` is refused by `cloud sync`
/// in every mode before any remote request. Every HTTP(S) request goes to a
/// local counter; the control run with an allowed kind reaches it, so the
/// zero is meaningful.
#[tokio::test]
async fn pi_role_cloud_sync_refuses_a_migrated_pi_session() {
    let counter = PiRoleConnectionCounter::start();
    let env = [
        ("LIBRA_D1_ACCOUNT_ID", "pi01-account"),
        ("LIBRA_D1_API_TOKEN", "pi01-token"),
        ("LIBRA_D1_DATABASE_ID", "pi01-database"),
        ("LIBRA_STORAGE_ENDPOINT", counter.url.as_str()),
        ("LIBRA_STORAGE_ALLOW_HTTP", "true"),
        ("LIBRA_STORAGE_BUCKET", "pi01-bucket"),
        ("LIBRA_STORAGE_ACCESS_KEY", "pi01-access"),
        ("LIBRA_STORAGE_SECRET_KEY", "pi01-secret"),
        ("HTTPS_PROXY", counter.url.as_str()),
        ("HTTP_PROXY", counter.url.as_str()),
        ("ALL_PROXY", counter.url.as_str()),
    ];
    let repo = PiRoleRepo::init();
    assert_eq!(pi_role_receipts(&repo).await.len(), 1, "migrated by init");
    let conn = connect(&repo.url()).await;
    pi_role_insert(&conn, "sess-pi", "pi").await;
    conn.close().await.expect("close");
    for args in [
        &["cloud", "sync"][..],
        &["cloud", "sync", "--force"][..],
        &["--json", "cloud", "sync"][..],
        &["cloud", "sync", "--progress=json"][..],
    ] {
        let output = repo.run_with_env(args, &env);
        assert_eq!(output.status.code(), Some(128), "{args:?}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("LBR-CONFLICT-002")
                && stderr.contains(
                    "agent_session.agent_kind pi is outside the cloud capture allowlist and was not mirrored"
                ),
            "{args:?}: {stderr}"
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            !stdout.contains("\"ok\":true") && !stdout.contains("Sync complete"),
            "{args:?}: {stdout}"
        );
    }
    assert_eq!(
        counter.connections(),
        0,
        "the refusal must precede every remote request"
    );

    let conn = connect(&repo.url()).await;
    pi_role_exec(&conn, "DELETE FROM agent_session WHERE agent_kind = 'pi'").await;
    pi_role_insert(&conn, "sess-claude", "claude_code").await;
    conn.close().await.expect("close");
    let control = repo.run_with_env(&["cloud", "sync"], &env);
    assert!(
        !control.status.success(),
        "the control run cannot succeed against the counter"
    );
    assert!(
        counter.connections() > 0,
        "control run made no request through the counter: stderr={}",
        String::from_utf8_lossy(&control.stderr)
    );
}

/// PI-01 AC12: down refuses while a `pi` row exists, keeps it with its child,
/// the receipt and the CHECK, and points at a forward fix rather than at
/// deleting captured sessions.
#[tokio::test]
async fn pi_role_down_refuses_while_pi_rows_exist() {
    let repo = pi_role_repository().await;
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    let conn = connect(&repo.url()).await;
    pi_role_insert(&conn, "sess-pi", "pi").await;
    pi_role_exec(
        &conn,
        "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit, tree_oid, metadata_blob_oid, traces_commit, created_at) VALUES ('cp-pi', 'sess-pi', 'temporary', 'parent', 'tree', 'meta', 'traces', 2)",
    )
    .await;
    conn.close().await.expect("close");
    let with_pi = pi_role_digest(&repo).await;

    let conn = connect(&repo.url()).await;
    let refused = hermes_runner()
        .rollback_to(&conn, HERMES_KIND_VERSION)
        .await
        .expect_err("down must refuse while a pi row exists");
    conn.close().await.expect("close");
    let message = refused.to_string();
    assert!(
        message.contains(
            "cannot roll back agent_kind pi: 1 agent_session row(s) use agent_kind 'pi' and were kept; nothing was changed. The repository stays at this schema: keep using a Libra binary that supports it (or a newer one) and fix forward; do not delete captured sessions to roll back"
        ),
        "{message}"
    );
    assert!(!message.contains("Delete"), "{message}");
    assert_eq!(
        pi_role_digest(&repo).await,
        with_pi,
        "the refused down kept every row, the receipt, and the CHECK"
    );
}

/// PI-01 AC13: with no `pi` row, down restores the nine-kind CHECK (which
/// rejects `pi` again) and keeps every other row and dependent.
#[tokio::test]
async fn pi_role_down_restores_the_nine_kind_check() {
    let repo = pi_role_repository().await;
    let fixture = pi_role_digest(&repo).await;
    let opened = repo.status(&[]);
    assert!(
        opened.status.success(),
        "{}",
        String::from_utf8_lossy(&opened.stderr)
    );
    assert_pi_rebuilt(&repo, &fixture).await;
    let conn = connect(&repo.url()).await;
    let rolled = hermes_runner()
        .rollback_to(&conn, HERMES_KIND_VERSION)
        .await
        .expect("down with zero pi rows");
    assert_eq!(rolled, vec![PI_KIND_VERSION]);
    let rejected = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir, metadata_json, redaction_report, started_at, last_event_at) VALUES ('sess-pi-again', 'pi', 'p', 'stopped', '/work', '{}', '{}', 1, 1)",
            [],
        ))
        .await;
    assert!(rejected.is_err(), "the restored CHECK rejects pi");
    conn.close().await.expect("close");
    assert_eq!(pi_role_check_values(&repo).await, KINDS_BEFORE_PI);
    assert!(pi_role_receipts(&repo).await.is_empty());
    let restored = pi_role_digest(&repo).await;
    assert_eq!(restored.data(), fixture.data(), "every earlier row is kept");
    assert_eq!(restored.dependents(), fixture.dependents());
}
