//! `tests/agent_bridge_migration_test.rs` — migration contract for the bridge
//! durable projection (plan-20260818 LB-02).
//!
//! Covers fresh bootstrap, old→new upgrade, repeated apply, and the
//! forward-only down/freeze semantics: the down path refuses while any bridge
//! row exists (never deleting acked events/evidence) and only drops the tables
//! on an empty database.

use libra::internal::db::migration::{
    MigrationError, MigrationRunner, builtin_migrations, run_builtin_migrations,
};
use sea_orm::{ConnectionTrait, Database, DbBackend, Statement};

const BRIDGE_TABLES: &[&str] = &[
    "agent_bridge_session",
    "agent_bridge_event",
    "agent_bridge_operation",
    "agent_bridge_checkpoint",
    "agent_bridge_link",
];

fn builtin_runner() -> Result<MigrationRunner, MigrationError> {
    let mut runner = MigrationRunner::new();
    runner.extend(
        builtin_migrations()
            .into_iter()
            .filter(|migration| migration.version < 2026090101),
    )?;
    Ok(runner)
}

async fn table_exists(db: &sea_orm::DatabaseConnection, table: &str) -> bool {
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT name FROM sqlite_master WHERE type='table' AND name=?",
            [table.into()],
        ))
        .await
        .expect("query sqlite_master");
    !rows.is_empty()
}

/// Fresh bootstrap: applying every builtin migration creates all five bridge
/// tables.
#[tokio::test]
async fn fresh_bootstrap_creates_bridge_tables() {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&db)
        .await
        .expect("builtin migrations apply");
    for table in BRIDGE_TABLES {
        assert!(table_exists(&db, table).await, "missing table {table}");
    }
}

/// Old→new upgrade: a database at the previous max migration (2026081301)
/// lacks the bridge tables; applying the remainder adds them.
#[tokio::test]
async fn old_db_upgrade_adds_bridge_tables() {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    let runner = builtin_runner().expect("registry builds");
    runner
        .run_pending_up_to(&db, 2026081301)
        .await
        .expect("apply up to previous migration");
    for table in BRIDGE_TABLES {
        assert!(
            !table_exists(&db, table).await,
            "{table} must not exist yet"
        );
    }
    runner
        .run_pending(&db)
        .await
        .expect("apply the remainder (including 2026081801)");
    for table in BRIDGE_TABLES {
        assert!(
            table_exists(&db, table).await,
            "missing table {table} after upgrade"
        );
    }
}

/// Repeated apply is a no-op (idempotent migrations).
#[tokio::test]
async fn repeated_apply_is_idempotent() {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&db).await.expect("first apply");
    run_builtin_migrations(&db)
        .await
        .expect("second apply is a no-op");
    for table in BRIDGE_TABLES {
        assert!(table_exists(&db, table).await, "missing table {table}");
    }
}

/// The down migration REFUSES while any acked bridge row exists (forward-only
/// freeze — never delete acked events/evidence as a rollback shortcut), and
/// succeeds only once the database is empty.
#[tokio::test]
async fn migration_up_down_preserves_acked_events() {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    let runner = builtin_runner().expect("registry builds");
    runner.run_pending(&db).await.expect("apply all");
    assert!(table_exists(&db, "agent_bridge_event").await);

    // Insert an acked event (a durable projection row).
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO agent_bridge_session \
         (bridge_session_id, source, repository_id, session_state, schema_version, \
          last_event_seq, last_acked_seq, created_at, updated_at) \
         VALUES ('s1', 'deepseek-harness', 'repo1', 'closed', 1, 1, 1, 0, 0)",
        [],
    ))
    .await
    .expect("insert session");
    db.execute_raw(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO agent_bridge_event \
         (bridge_session_id, event_seq, event_type, payload_sha256, payload, created_at) \
         VALUES ('s1', 1, 'tool/result', 'abc', '{}', 0)",
        [],
    ))
    .await
    .expect("insert acked event");

    // Down to the previous version must FAIL (the freeze guard blocks it),
    // leaving the acked event intact.
    let down = runner.rollback_to(&db, 2026081301).await;
    assert!(down.is_err(), "down with bridge rows must refuse");
    assert!(
        table_exists(&db, "agent_bridge_event").await,
        "rows preserved"
    );
    assert!(
        table_exists(&db, "agent_bridge_session").await,
        "rows preserved"
    );

    // After the rows are gone, the down succeeds and drops the tables.
    db.execute_raw(Statement::from_string(
        DbBackend::Sqlite,
        "DELETE FROM agent_bridge_session",
    ))
    .await
    .expect("clear bridge rows");
    runner
        .rollback_to(&db, 2026081301)
        .await
        .expect("down succeeds on empty database");
    for table in BRIDGE_TABLES {
        assert!(!table_exists(&db, table).await, "{table} should be dropped");
    }
}

/// The schema enforces the fixed `deepseek-harness` source and the bounded
/// event payload cap via CHECK constraints.
#[tokio::test]
async fn schema_enforces_source_and_payload_caps() {
    let db = Database::connect("sqlite::memory:").await.expect("connect");
    run_builtin_migrations(&db).await.expect("apply");

    // Wrong source is rejected by the CHECK constraint.
    let bad_source = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO agent_bridge_session \
             (bridge_session_id, source, repository_id, session_state, schema_version, \
              last_event_seq, last_acked_seq, created_at, updated_at) \
             VALUES ('s-x', 'claude-code', 'repo1', 'open', 1, 0, 0, 0, 0)",
            [],
        ))
        .await;
    assert!(
        bad_source.is_err(),
        "non-deepseek-harness source must be rejected"
    );

    // Oversized event payload is rejected by the CHECK constraint.
    let big_payload = "x".repeat(256 * 1024 + 1);
    let oversize = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO agent_bridge_session \
             (bridge_session_id, source, repository_id, session_state, schema_version, \
              last_event_seq, last_acked_seq, created_at, updated_at) \
             VALUES ('s2', 'deepseek-harness', 'repo1', 'open', 1, 0, 0, 0, 0)",
            [],
        ))
        .await;
    assert!(oversize.is_ok(), "valid session insert");
    let oversize = db
        .execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO agent_bridge_event \
             (bridge_session_id, event_seq, event_type, payload_sha256, payload, created_at) \
             VALUES ('s2', 1, 'tool/result', 'abc', ?, 0)",
            [big_payload.into()],
        ))
        .await;
    assert!(oversize.is_err(), "payload over 256 KiB must be rejected");
}
