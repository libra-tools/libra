//! Integration tests for the schema migration runner.
//!
//! Every test runs against an isolated, on-disk SQLite file inside
//! `tempfile::tempdir()` so the cases neither pollute each other nor
//! depend on the embedded canonical bootstrap path.

use std::path::PathBuf;

use libra::internal::db::migration::{
    Migration, MigrationError, MigrationRunner, builtin_migrations, builtin_runner,
    run_builtin_migrations,
};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, Statement};
use tempfile::TempDir;

/// Path helper. Returns `(tempdir, sqlite-url)`. The TempDir is held by the
/// caller for the lifetime of the test.
fn fresh_db_url() -> (TempDir, String, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("test.db");
    // sqlite needs the file to exist before connecting.
    std::fs::File::create(&path).expect("touch sqlite file");
    let url = format!("sqlite://{}", path.display());
    (dir, url, path)
}

async fn connect(url: &str) -> DatabaseConnection {
    let mut opts = ConnectOptions::new(url.to_string());
    opts.sqlx_logging(false);
    Database::connect(opts).await.expect("connect")
}

// ---------------------------------------------------------------------------
// Builtin runner contract: current runtime migrations are registered
// ---------------------------------------------------------------------------

#[test]
fn builtin_migrations_register_current_schema_migrations() {
    // Keep this explicit so future built-in migrations update this test with
    // the registry shape they introduce.
    let migrations = builtin_migrations();
    let versions: Vec<i64> = migrations
        .iter()
        .map(|migration| migration.version)
        .collect();
    let names: Vec<&str> = migrations.iter().map(|migration| migration.name).collect();
    assert_eq!(
        versions,
        vec![
            2026050301, 2026050302, 2026050303, 2026050501, 2026050601, 2026050801, 2026052301,
            2026053101, 2026060201, 2026060401, 2026060801, 2026061401, 2026062301, 2026070201,
            2026070202, 2026070301, 2026070401, 2026070501, 2026070601, 2026070701, 2026070801,
            2026070802, 2026070803, 2026071301, 2026071401, 2026071402, 2026071403, 2026071404,
            2026071405, 2026071406, 2026071407, 2026071901, 2026072101, 2026072201, 2026072301,
            2026072302, 2026072303, 2026072304, 2026072401, 2026072402, 2026072403
        ]
    );
    assert_eq!(
        names,
        vec![
            "automation_log",
            "agent_usage_stats",
            "agent_capture",
            "agent_checkpoint_parent_nullable",
            "approved_permission",
            "agent_usage_stats_agent_name",
            "source_call_log",
            "ai_final_decision",
            "source_call_log_agent_run_id",
            "cherry_pick_state",
            "revert_sequence",
            "notes",
            "rename_agent_traces_branch",
            "metadata_kv",
            "working_dirty",
            "revision_ordinal",
            "sequence_state",
            "layer",
            "object_obliteration",
            "sparse_view",
            "worktree_isolation",
            "agent_checkpoint_paging",
            "agent_audit_log",
            "agent_coverage_gate",
            "agent_export_job",
            "agent_import_identity",
            "agent_import_tombstone",
            "agent_tombstone_compat_barrier",
            "agent_coverage_conflict",
            "agent_subagent_content",
            "agent_subagent_replication",
            "sequencer_worktree_scope",
            "rebase_state_worktree_scope",
            "operation_worktree_scope",
            "bisect_state_worktree_scope",
            "working_dirty_worktree_scope",
            "layer_worktree_scope",
            "sparse_view_worktree_scope",
            "worktree_registry_v2",
            "worktree_lifecycle_journal",
            "worktree_migrate_intent",
        ]
    );

    let runner = builtin_runner().expect("builtin registry must build clean");
    assert!(!runner.is_empty());
    assert_eq!(runner.len(), 41);
    assert_eq!(runner.max_registered_version(), Some(2026072403));
}

// ---------------------------------------------------------------------------
// run_pending on a fresh database: applies every registered migration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_pending_applies_all_registered_migrations_on_fresh_db() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "create_widgets",
            up: "CREATE TABLE IF NOT EXISTS widgets (id INTEGER PRIMARY KEY, name TEXT NOT NULL)",
            down: Some("DROP TABLE IF EXISTS widgets"),
        })
        .unwrap();
    runner
        .register(Migration {
            version: 2,
            name: "add_widget_index",
            up: "CREATE INDEX IF NOT EXISTS idx_widgets_name ON widgets(name)",
            down: Some("DROP INDEX IF EXISTS idx_widgets_name"),
        })
        .unwrap();

    let applied = runner.run_pending(&conn).await.expect("run_pending");
    assert_eq!(applied, vec![1, 2]);
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(2));

    // Both DDL bodies actually ran.
    assert!(table_exists(&conn, "widgets").await);
    assert!(index_exists(&conn, "idx_widgets_name").await);
    // The runner created its own bookkeeping table.
    assert!(table_exists(&conn, "schema_versions").await);
}

// ---------------------------------------------------------------------------
// run_pending is idempotent: second call applies nothing
// ---------------------------------------------------------------------------

/// Codex r3 P2: idempotency must hold across **reopen**, not just within a
/// single connection. A real upgrade scenario closes the DB, restarts the
/// process, and reopens — that round-trip is what `schema_versions`
/// existence guards against.
#[tokio::test]
async fn run_pending_is_idempotent_across_connection_reopen() {
    let (_dir, url, _path) = fresh_db_url();

    // First run on connection A.
    {
        let conn = connect(&url).await;
        let mut runner = MigrationRunner::new();
        runner
            .register(Migration {
                version: 42,
                name: "create_reopen_target",
                up: "CREATE TABLE IF NOT EXISTS reopen_target (id INTEGER PRIMARY KEY)",
                down: Some("DROP TABLE IF EXISTS reopen_target"),
            })
            .unwrap();
        let applied = runner.run_pending(&conn).await.unwrap();
        assert_eq!(applied, vec![42]);
    }

    // Second run on a brand new connection + brand new runner. Even
    // though the runner instance is fresh, the `schema_versions` row
    // must persist on disk and the runner must see version 42 already
    // applied.
    let conn = connect(&url).await;
    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 42,
            name: "create_reopen_target",
            up: "CREATE TABLE IF NOT EXISTS reopen_target (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS reopen_target"),
        })
        .unwrap();
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(42));
    let applied = runner.run_pending(&conn).await.unwrap();
    assert!(
        applied.is_empty(),
        "reopen run must report no new applies; got {applied:?}"
    );
}

/// Codex r3 P2: a migration whose `up` body executes some DDL statements
/// successfully and then fails on a later statement must leave the
/// database transactionally clean — the partially-created table must NOT
/// remain, and `schema_versions` must be untouched.
#[tokio::test]
async fn failing_partway_through_up_ddl_rolls_back_completed_statements() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "two_statements_one_broken",
            // First statement is valid; second is intentionally invalid.
            // SQLite executes them in the same transaction, so the
            // failure on the second must roll back the first.
            up: "CREATE TABLE IF NOT EXISTS half_baked (id INTEGER PRIMARY KEY); \
                 CREATE TABLE !!! BROKEN DDL",
            down: None,
        })
        .unwrap();

    let err = runner.run_pending(&conn).await.unwrap_err();
    assert!(matches!(err, MigrationError::Database(_)));

    // The transaction-atomicity contract: NEITHER statement should have
    // persisted. The first table must not exist, and schema_versions
    // must remain empty.
    assert!(
        !table_exists(&conn, "half_baked").await,
        "first DDL statement must roll back when the second fails"
    );
    assert_eq!(runner.current_version(&conn).await.unwrap(), None);
}

/// Codex r3 P2: the `name` and `applied_at` columns are not just storage
/// detail — audit / observability code reads them. Pin that they round-trip
/// correctly: `name` must equal what was registered, and `applied_at`
/// must be a parseable RFC3339 timestamp.
#[tokio::test]
async fn run_pending_persists_name_and_parseable_applied_at() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 7,
            name: "create_audit_widgets",
            up: "CREATE TABLE IF NOT EXISTS audit_widgets (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS audit_widgets"),
        })
        .unwrap();
    runner.run_pending(&conn).await.unwrap();

    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT version, name, applied_at FROM schema_versions WHERE version = 7",
        ))
        .await
        .expect("query")
        .expect("row exists");
    let version: i64 = row.try_get_by_index(0).expect("version");
    let name: String = row.try_get_by_index(1).expect("name");
    let applied_at: String = row.try_get_by_index(2).expect("applied_at");

    assert_eq!(version, 7);
    assert_eq!(name, "create_audit_widgets");
    chrono::DateTime::parse_from_rfc3339(&applied_at)
        .expect("applied_at must be a parseable RFC3339 timestamp");
}

#[tokio::test]
async fn run_pending_is_idempotent_when_already_applied() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 7,
            name: "create_things",
            up: "CREATE TABLE IF NOT EXISTS things (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS things"),
        })
        .unwrap();

    let first = runner.run_pending(&conn).await.unwrap();
    assert_eq!(first, vec![7]);

    let second = runner.run_pending(&conn).await.unwrap();
    assert!(
        second.is_empty(),
        "second run must be a no-op; got {second:?}"
    );
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(7));
}

// ---------------------------------------------------------------------------
// run_pending on a legacy DB (pre-CEX-12.5 tables already exist) is safe
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_pending_tolerates_pre_existing_tables_via_idempotent_ddl() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    // Simulate a legacy database that already contains `legacy_widgets`,
    // pre-dating any version tracking.
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TABLE legacy_widgets (id INTEGER PRIMARY KEY, kind TEXT NOT NULL)",
    ))
    .await
    .unwrap();

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "ensure_legacy_widgets",
            up: "CREATE TABLE IF NOT EXISTS legacy_widgets (id INTEGER PRIMARY KEY, kind TEXT NOT NULL)",
            down: Some("DROP TABLE IF EXISTS legacy_widgets"),
        })
        .unwrap();

    let applied = runner.run_pending(&conn).await.expect("run_pending");
    // The migration's up-DDL is a no-op against the existing table, but
    // the runner still records it as applied so future versions chain
    // correctly.
    assert_eq!(applied, vec![1]);
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(1));
    assert!(table_exists(&conn, "legacy_widgets").await);
}

// ---------------------------------------------------------------------------
// register validation: duplicate / non-monotonic / out-of-order
// ---------------------------------------------------------------------------

#[test]
fn register_rejects_duplicate_versions() {
    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "first",
            up: "",
            down: None,
        })
        .unwrap();
    let err = runner
        .register(Migration {
            version: 1,
            name: "second",
            up: "",
            down: None,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        MigrationError::DuplicateVersion {
            version: 1,
            existing: "first",
            new: "second"
        }
    ));
}

#[test]
fn register_rejects_non_monotonic_versions() {
    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 5,
            name: "later",
            up: "",
            down: None,
        })
        .unwrap();
    let err = runner
        .register(Migration {
            version: 3,
            name: "earlier",
            up: "",
            down: None,
        })
        .unwrap_err();
    assert!(matches!(
        err,
        MigrationError::NonMonotonicRegistration {
            prev_version: 5,
            new_version: 3,
            ..
        }
    ));
}

// ---------------------------------------------------------------------------
// Codex r5 P1#4 fix: extend's "stop at first error, retain accepted prefix"
// contract was previously only exercised with a single-element duplicate. A
// real caller passes a longer iterator and trusts that the prefix it accepted
// before the failure is preserved in the runner.
// ---------------------------------------------------------------------------
#[test]
fn extend_preserves_accepted_prefix_when_failing_partway_through() {
    let mut runner = MigrationRunner::new();
    // [v1 ok, v2 ok, v1-again fails non-monotonic (v1 < v2), v3 never tried].
    let err = runner
        .extend(vec![
            Migration {
                version: 1,
                name: "first",
                up: "",
                down: None,
            },
            Migration {
                version: 2,
                name: "second",
                up: "",
                down: None,
            },
            Migration {
                version: 1,
                name: "out_of_order_dup_v1",
                up: "",
                down: None,
            },
            Migration {
                version: 3,
                name: "never_reached",
                up: "",
                down: None,
            },
        ])
        .unwrap_err();
    // The strict-monotonic guard catches the regression as
    // NonMonotonicRegistration (v1 < v2), not DuplicateVersion. Either
    // is a correct rejection of an invalid registration; both still
    // satisfy the "stop at first error" contract.
    assert!(matches!(
        err,
        MigrationError::NonMonotonicRegistration {
            prev_version: 2,
            new_version: 1,
            ..
        }
    ));
    // The accepted prefix (v1, v2) stays in the runner; the failed item
    // and everything after is dropped.
    assert_eq!(runner.len(), 2);
    assert_eq!(runner.max_registered_version(), Some(2));
}

// ---------------------------------------------------------------------------
// Codex r5 P1#5 fix: current_version on a fresh database (table created via
// ensure_schema_versions_table side-effect, no rows) must return Ok(None) —
// not Some(0) or any sentinel. Prior tests only asserted None after a failed
// migration, never on the explicit "table exists, no rows" baseline.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn current_version_returns_none_on_fresh_database_with_empty_schema_versions() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = MigrationRunner::new();
    let version = runner
        .current_version(&conn)
        .await
        .expect("current_version on fresh DB");
    assert_eq!(version, None);
    // current_version's side-effect created the bookkeeping table even
    // though the runner has no migrations registered.
    assert!(table_exists(&conn, "schema_versions").await);
}

// ---------------------------------------------------------------------------
// Codex r5 P1#2 fix: empty-database rollback returns the dedicated variant
// (RollbackOnEmptyDatabase) so callers — and future migrations that may
// legitimately use negative version numbers — can distinguish "nothing to
// roll back" from "rollback target too high" without colliding on a sentinel
// `current = -1`.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn rollback_to_on_empty_database_returns_dedicated_variant() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "never_applied",
            up: "CREATE TABLE IF NOT EXISTS t (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS t"),
        })
        .unwrap();
    // No run_pending — schema_versions remains empty.
    let err = runner.rollback_to(&conn, 0).await.unwrap_err();
    assert!(
        matches!(err, MigrationError::RollbackOnEmptyDatabase { target: 0 }),
        "expected RollbackOnEmptyDatabase, got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// rollback_to: reverse a contiguous range of applied migrations
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rollback_to_reverses_in_descending_version_order() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "a",
            up: "CREATE TABLE IF NOT EXISTS a (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS a"),
        })
        .unwrap();
    runner
        .register(Migration {
            version: 2,
            name: "b",
            up: "CREATE TABLE IF NOT EXISTS b (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS b"),
        })
        .unwrap();
    runner
        .register(Migration {
            version: 3,
            name: "c",
            up: "CREATE TABLE IF NOT EXISTS c (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS c"),
        })
        .unwrap();

    runner.run_pending(&conn).await.unwrap();
    assert!(table_exists(&conn, "a").await);
    assert!(table_exists(&conn, "b").await);
    assert!(table_exists(&conn, "c").await);

    // Roll back to version 1: removes b and c in that order.
    let rolled = runner.rollback_to(&conn, 1).await.expect("rollback");
    assert_eq!(rolled, vec![3, 2]);
    assert!(table_exists(&conn, "a").await);
    assert!(!table_exists(&conn, "b").await);
    assert!(!table_exists(&conn, "c").await);
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(1));
}

#[tokio::test]
async fn rollback_to_errors_when_target_is_not_below_current() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "only",
            up: "CREATE TABLE IF NOT EXISTS only_t (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS only_t"),
        })
        .unwrap();
    runner.run_pending(&conn).await.unwrap();

    let err = runner.rollback_to(&conn, 1).await.unwrap_err();
    assert!(matches!(
        err,
        MigrationError::RollbackTargetNotBelowCurrent {
            target: 1,
            current: 1
        }
    ));
}

#[tokio::test]
async fn rollback_to_refuses_irreversible_migration() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "forward_only",
            up: "CREATE TABLE IF NOT EXISTS forward (id INTEGER PRIMARY KEY)",
            down: None,
        })
        .unwrap();
    runner
        .register(Migration {
            version: 2,
            name: "reversible",
            up: "CREATE TABLE IF NOT EXISTS reversible (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS reversible"),
        })
        .unwrap();
    runner.run_pending(&conn).await.unwrap();

    // Rolling back to 0 must traverse migration 1 (forward-only).
    let err = runner.rollback_to(&conn, 0).await.unwrap_err();
    assert!(matches!(
        err,
        MigrationError::IrreversibleMigration {
            version: 1,
            name: "forward_only",
        }
    ));
}

/// Codex r1 P1#8 fix regression guard: when `rollback_to` finds an
/// irreversible migration in its plan, NO `down` DDL must run. Without
/// the pre-validation phase, the runner would have rolled back v3 → v2
/// (reversible) successfully and then errored on v1 (irreversible),
/// leaving the database in an inconsistent state with v3/v2 dropped but
/// v1 still present and the v2/v3 rows removed from `schema_versions`.
#[tokio::test]
async fn rollback_to_runs_no_down_ddl_when_plan_contains_irreversible_migration() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "forward_only",
            up: "CREATE TABLE IF NOT EXISTS forward_t (id INTEGER PRIMARY KEY)",
            down: None,
        })
        .unwrap();
    runner
        .register(Migration {
            version: 2,
            name: "reversible_a",
            up: "CREATE TABLE IF NOT EXISTS reversible_a (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS reversible_a"),
        })
        .unwrap();
    runner
        .register(Migration {
            version: 3,
            name: "reversible_b",
            up: "CREATE TABLE IF NOT EXISTS reversible_b (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS reversible_b"),
        })
        .unwrap();
    runner.run_pending(&conn).await.unwrap();
    assert!(table_exists(&conn, "forward_t").await);
    assert!(table_exists(&conn, "reversible_a").await);
    assert!(table_exists(&conn, "reversible_b").await);

    // Plan for `rollback_to(0)` is [v3, v2, v1]; v1 is irreversible.
    let err = runner.rollback_to(&conn, 0).await.unwrap_err();
    assert!(matches!(
        err,
        MigrationError::IrreversibleMigration {
            version: 1,
            name: "forward_only"
        }
    ));

    // None of the down DDL ran — every table must still exist and the
    // current version must still be 3.
    assert!(table_exists(&conn, "forward_t").await);
    assert!(table_exists(&conn, "reversible_a").await);
    assert!(table_exists(&conn, "reversible_b").await);
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(3));
}

// ---------------------------------------------------------------------------
// Codex r5 P1#6 fix: rollback_to with a target that lies between registered
// versions (or in a registration gap) must still terminate correctly and
// produce a consistent final state, rather than over-rolling-back or
// erroring. The runner's contract is "no migration with version > target
// remains applied"; the highest remaining version becomes the new current,
// even if it's strictly less than target.
// ---------------------------------------------------------------------------
#[tokio::test]
async fn rollback_to_with_target_in_registration_gap_lands_on_lower_version() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "v1",
            up: "CREATE TABLE IF NOT EXISTS gap_v1 (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS gap_v1"),
        })
        .unwrap();
    // Registration gap: no v2.
    runner
        .register(Migration {
            version: 3,
            name: "v3",
            up: "CREATE TABLE IF NOT EXISTS gap_v3 (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE IF EXISTS gap_v3"),
        })
        .unwrap();
    runner.run_pending(&conn).await.unwrap();
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(3));

    // Target = 2 falls in the gap. Plan should contain only v3 (since
    // 3 > 2 and 1 <= 2 stops the iteration). v1 stays applied.
    let rolled = runner.rollback_to(&conn, 2).await.expect("rollback");
    assert_eq!(rolled, vec![3]);
    assert!(
        !table_exists(&conn, "gap_v3").await,
        "v3 down DDL must have run"
    );
    assert!(
        table_exists(&conn, "gap_v1").await,
        "v1 must still be applied — target=2 only requires versions > 2 to roll back"
    );
    // current is now Some(1), not Some(2): no migration was registered or
    // applied at version 2, so the highest applied version drops to 1.
    assert_eq!(runner.current_version(&conn).await.unwrap(), Some(1));
}

// ---------------------------------------------------------------------------
// Codex r5 P1#3 + P1#7 fix: concurrent rollback_to calls must each report
// only the versions THEY owned (won the DELETE race for), with no version
// owned by both callers and no down DDL ever running twice. The DELETE-first
// reorder in apply_down_migration makes this symmetric to run_pending's
// INSERT OR IGNORE concurrency contract.
// ---------------------------------------------------------------------------
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_rollback_calls_partition_owned_versions_without_double_ddl() {
    let (_dir, url, _path) = fresh_db_url();

    // Setup: apply v1 + v2 against a single connection so the race
    // surface for rollback is `(0, 2]`.
    {
        let conn = connect_with_busy_timeout(&url).await;
        let mut runner = MigrationRunner::new();
        runner
            .register(Migration {
                version: 1,
                name: "rb_v1",
                up: "CREATE TABLE IF NOT EXISTS rb_v1 (id INTEGER PRIMARY KEY)",
                down: Some("DROP TABLE IF EXISTS rb_v1"),
            })
            .unwrap();
        runner
            .register(Migration {
                version: 2,
                name: "rb_v2",
                up: "CREATE TABLE IF NOT EXISTS rb_v2 (id INTEGER PRIMARY KEY)",
                down: Some("DROP TABLE IF EXISTS rb_v2"),
            })
            .unwrap();
        runner.run_pending(&conn).await.unwrap();
    }

    let conn_a = connect_with_busy_timeout(&url).await;
    let conn_b = connect_with_busy_timeout(&url).await;
    let runner_a = build_rollback_runner();
    let runner_b = build_rollback_runner();

    let task_a = tokio::spawn(async move { runner_a.rollback_to(&conn_a, 0).await });
    let task_b = tokio::spawn(async move { runner_b.rollback_to(&conn_b, 0).await });
    let a = task_a.await.expect("task A");
    let b = task_b.await.expect("task B");

    // Tokio may schedule one task only after the other has completed both
    // down migrations. Such a late observer correctly sees an empty registry
    // and preserves rollback_to's dedicated empty-database error contract;
    // normalize that valid serialized outcome to an empty ownership set.
    let completed_or_serialized_empty = |runner: &str, result| match result {
        Ok(versions) => versions,
        Err(MigrationError::RollbackOnEmptyDatabase { target: 0 }) => Vec::new(),
        Err(err) => panic!("{runner} failed unexpectedly: {err}"),
    };
    let a = completed_or_serialized_empty("runner A", a);
    let b = completed_or_serialized_empty("runner B", b);

    // Union must cover {1, 2} exactly; intersection must be empty. A
    // regression that re-ran down DDL would either show duplicates in
    // the union, or surface as an Err on the loser when its DELETE was
    // a no-op but the down DDL hit a non-idempotent SQL state.
    let mut union: Vec<i64> = a.iter().chain(b.iter()).copied().collect();
    union.sort();
    assert_eq!(
        union,
        vec![1, 2],
        "union of owned versions must be exactly {{1,2}}; got A={a:?} B={b:?}"
    );
    let a_set: std::collections::HashSet<i64> = a.iter().copied().collect();
    let b_set: std::collections::HashSet<i64> = b.iter().copied().collect();
    assert!(
        a_set.is_disjoint(&b_set),
        "no version may be owned by both callers; A={a:?} B={b:?}"
    );

    // Both tables are gone (down DDL ran exactly once each) and
    // schema_versions is empty.
    let conn = connect_with_busy_timeout(&url).await;
    assert!(!table_exists(&conn, "rb_v1").await);
    assert!(!table_exists(&conn, "rb_v2").await);
    assert_eq!(count_schema_versions(&conn).await, 0);
}

/// Rollback runner whose down DDL is **intentionally non-idempotent**
/// (Codex r6 P1 fix): `DROP TABLE` without `IF EXISTS` errors with "no
/// such table" if executed twice. Without this, a regression that ran
/// the down DDL on the loser's path would still pass the union /
/// intersection assertions because `DROP TABLE IF EXISTS` is a no-op on
/// missing tables. With a non-idempotent down, the loser's task panics
/// at `.expect("runner B succeeds")` and the test fails — surfacing the
/// exact regression class P1#3 targets.
fn build_rollback_runner() -> MigrationRunner {
    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "rb_v1",
            up: "CREATE TABLE IF NOT EXISTS rb_v1 (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE rb_v1"),
        })
        .unwrap();
    runner
        .register(Migration {
            version: 2,
            name: "rb_v2",
            up: "CREATE TABLE IF NOT EXISTS rb_v2 (id INTEGER PRIMARY KEY)",
            down: Some("DROP TABLE rb_v2"),
        })
        .unwrap();
    runner
}

// ---------------------------------------------------------------------------
// Concurrency: two simultaneous run_pending calls against the same DB file
// must converge to a single applied row, with `INSERT OR IGNORE` letting the
// loser report `applied = []` instead of erroring on a UNIQUE conflict.
// (Codex r2 P1 fix: the prior tests only covered single-connection sequential
// runs; this test pins the actual race that P1#2's fix targets.)
// ---------------------------------------------------------------------------

/// Codex r4 P1#1 fix: the prior round's concurrency test was vacuously
/// passing because either runner's internal `current_version` read could
/// see the winner's commit and short-circuit before reaching the INSERT
/// path. This version pre-populates `schema_versions` with a synthetic
/// baseline (`version = 0`) so both runners' `current_version` returns
/// `Some(0)`. Both then proceed to `apply_one_migration` for `version =
/// 1` — the actual race path. Without `INSERT OR IGNORE`, the loser's
/// `INSERT` would raise a UNIQUE-constraint violation and the runner
/// would error; with the fix, the loser reports `applied = []`. Either
/// regression surface fails this test.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_run_pending_calls_converge_without_unique_violation() {
    let (_dir, url, _path) = fresh_db_url();

    let conn_a = connect_with_busy_timeout(&url).await;
    let conn_b = connect_with_busy_timeout(&url).await;

    // Bootstrap: create `schema_versions` and seed a synthetic baseline
    // row so both runners' internal `current_version` returns `Some(0)`
    // and neither one can short-circuit before the INSERT race for
    // version 1.
    conn_a
        .execute(Statement::from_string(
            conn_a.get_database_backend(),
            "CREATE TABLE IF NOT EXISTS schema_versions (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL)",
        ))
        .await
        .unwrap();
    conn_a
        .execute(Statement::from_sql_and_values(
            conn_a.get_database_backend(),
            "INSERT INTO schema_versions (version, name, applied_at) VALUES (0, 'baseline', '2026-01-01T00:00:00Z')",
            [],
        ))
        .await
        .unwrap();

    let runner_a = build_runner();
    let runner_b = build_runner();

    // Spawn both `run_pending` calls and let the SQLite busy-timeout
    // arbitrate. Both runners' `current_version` returns `Some(0)`,
    // both loop bodies pass the `1 <= 0` short-circuit check, both
    // reach the INSERT path. The loser sees an existing row and
    // `INSERT OR IGNORE` reports `changes() = 0`.
    let task_a = tokio::spawn(async move { runner_a.run_pending(&conn_a).await });
    let task_b = tokio::spawn(async move { runner_b.run_pending(&conn_b).await });
    let a = task_a.await.expect("task A").expect("runner A succeeds");
    let b = task_b.await.expect("task B").expect("runner B succeeds");

    // Exactly one runner reports having applied; the other returns [].
    // A plain-INSERT regression would surface here as
    // `runner B succeeds` panicking on a UNIQUE-violation Result::Err,
    // OR as both runners returning `[1]` (concat = [1, 1] != [1]).
    let totals: Vec<i64> = a.iter().chain(b.iter()).copied().collect();
    assert_eq!(
        totals,
        vec![1],
        "exactly one runner must report version 1 applied; got A={a:?} B={b:?}"
    );

    // schema_versions now has exactly two rows: synthetic baseline + new.
    let conn = connect_with_busy_timeout(&url).await;
    assert!(table_exists(&conn, "race_target").await);
    let row_count = count_schema_versions(&conn).await;
    assert_eq!(
        row_count, 2,
        "schema_versions must have baseline + version-1 = 2 rows; saw {row_count}"
    );
}

fn build_runner() -> MigrationRunner {
    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "create_race_target",
            up: "CREATE TABLE IF NOT EXISTS race_target (id INTEGER PRIMARY KEY, payload TEXT)",
            down: Some("DROP TABLE IF EXISTS race_target"),
        })
        .unwrap();
    runner
}

async fn connect_with_busy_timeout(url: &str) -> DatabaseConnection {
    use std::time::Duration;
    let mut opts = ConnectOptions::new(url.to_string());
    opts.sqlx_logging(false);
    // Match the production busy-timeout path so the test exercises the
    // realistic concurrency model.
    opts.map_sqlx_sqlite_opts(move |sqlx_opts| sqlx_opts.busy_timeout(Duration::from_secs(5)));
    Database::connect(opts).await.expect("connect")
}

async fn count_schema_versions(conn: &DatabaseConnection) -> i64 {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT COUNT(*) FROM schema_versions",
        ))
        .await
        .expect("count")
        .expect("row");
    row.try_get_by_index(0).expect("decode count")
}

// ---------------------------------------------------------------------------
// run_pending atomicity: a failing up-DDL leaves no schema_versions row
// ---------------------------------------------------------------------------

#[tokio::test]
async fn failing_up_migration_leaves_schema_versions_unchanged() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;

    let mut runner = MigrationRunner::new();
    runner
        .register(Migration {
            version: 1,
            name: "broken",
            // Intentionally invalid SQL.
            up: "CREATE TABLE !!! INVALID DDL",
            down: None,
        })
        .unwrap();

    let err = runner.run_pending(&conn).await.unwrap_err();
    assert!(matches!(err, MigrationError::Database(_)));
    // The version row must NOT have been recorded.
    assert_eq!(runner.current_version(&conn).await.unwrap(), None);
}

// ---------------------------------------------------------------------------
// Fresh-init path (`db::create_database`) must create a database that can be
// reopened by `db::establish_connection` without applying implicit migrations.
// This guards both sides of the explicit-upgrade contract: init creates the
// current schema, while ordinary connections only verify compatibility.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn fresh_create_database_runs_migrations_just_like_reopen() {
    use libra::internal::db::{create_database, establish_connection};

    // Fresh path: create_database from scratch.
    let fresh_dir = tempfile::tempdir().unwrap();
    let fresh_path = fresh_dir.path().join("fresh.db");
    let fresh_path_str = fresh_path.to_str().unwrap();
    let fresh_conn = create_database(fresh_path_str).await.unwrap();
    assert!(
        table_exists(&fresh_conn, "schema_versions").await,
        "fresh create_database must run migrations and create schema_versions"
    );

    // Reopen path: connect to a different freshly created file via
    // establish_connection. Schema must already be current before the
    // connection check runs.
    let reopen_dir = tempfile::tempdir().unwrap();
    let reopen_path = reopen_dir.path().join("reopen.db");
    let reopen_path_str = reopen_path.to_str().unwrap();
    // establish_connection requires the file to exist; touch it via
    // create_database first, then close and reopen.
    let _ = create_database(reopen_path_str).await.unwrap();
    let reopen_conn = establish_connection(reopen_path_str).await.unwrap();
    assert!(
        table_exists(&reopen_conn, "schema_versions").await,
        "establish_connection path must see schema_versions from create_database"
    );

    // Both paths produce identical `schema_versions` shape.
    let fresh_cols = describe_schema_versions(&fresh_conn).await;
    let reopen_cols = describe_schema_versions(&reopen_conn).await;
    assert_eq!(
        fresh_cols, reopen_cols,
        "fresh and reopen paths must produce identical schema_versions shape"
    );
}

#[tokio::test]
async fn establish_connection_auto_upgrades_stale_schema() {
    use libra::internal::db::{create_database, establish_connection};

    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("stale.db");
    let path_str = path.to_str().unwrap();
    let conn = create_database(path_str).await.unwrap();

    let runner = builtin_runner().expect("builtin runner builds clean");
    runner
        .rollback_to(&conn, 2026050601)
        .await
        .expect("roll back latest migration");
    conn.close().await.unwrap();

    // Opening the connection now applies any pending migrations automatically,
    // so an out-of-date repository is upgraded in place simply by being opened.
    establish_connection(path_str)
        .await
        .expect("ordinary connect should auto-upgrade a stale schema");

    let raw = connect(&format!("sqlite://{}", path.display())).await;
    let latest = builtin_runner()
        .expect("builtin runner builds clean")
        .max_registered_version();
    let current = builtin_runner()
        .expect("builtin runner builds clean")
        .current_version_readonly(&raw)
        .await
        .expect("read current version");
    assert_eq!(
        current, latest,
        "connecting should migrate the schema up to the latest registered version"
    );
    assert!(
        column_exists(&raw, "agent_usage_stats", "agent_name").await,
        "ordinary connect should apply the pending agent_name migration"
    );
}

async fn describe_schema_versions(conn: &DatabaseConnection) -> Vec<String> {
    let backend = conn.get_database_backend();
    let mut rows = vec![];
    let stream = conn
        .query_all(Statement::from_string(
            backend,
            "PRAGMA table_info(schema_versions)",
        ))
        .await
        .expect("table_info");
    for row in stream {
        let name: String = row.try_get_by_index(1).expect("col name");
        let typ: String = row.try_get_by_index(2).expect("col type");
        rows.push(format!("{name}:{typ}"));
    }
    rows.sort();
    rows
}

// ---------------------------------------------------------------------------
// Builtin wiring: run_builtin_migrations is callable from production code
// ---------------------------------------------------------------------------

#[tokio::test]
async fn run_builtin_migrations_applies_current_builtin_registry() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let applied = run_builtin_migrations(&conn)
        .await
        .expect("run_builtin_migrations");
    assert_eq!(
        applied,
        vec![
            2026050301, 2026050302, 2026050303, 2026050501, 2026050601, 2026050801, 2026052301,
            2026053101, 2026060201, 2026060401, 2026060801, 2026061401, 2026062301, 2026070201,
            2026070202, 2026070301, 2026070401, 2026070501, 2026070601, 2026070701, 2026070801,
            2026070802, 2026070803, 2026071301, 2026071401, 2026071402, 2026071403, 2026071404,
            2026071405, 2026071406, 2026071407, 2026071901, 2026072101, 2026072201, 2026072301,
            2026072302, 2026072303, 2026072304, 2026072401, 2026072402, 2026072403
        ]
    );
    assert!(table_exists(&conn, "schema_versions").await);
    // AG-20 agent_checkpoint_paging: traces_commit probe index (non-unique
    // by design) + keyset pagination indexes.
    assert!(index_exists(&conn, "idx_agent_checkpoint_traces_commit").await);
    assert!(index_exists(&conn, "idx_agent_session_started_paging").await);
    assert!(index_exists(&conn, "idx_agent_checkpoint_created_paging").await);
    assert!(column_exists(&conn, "reference", "worktree_id").await);
    assert!(column_exists(&conn, "reflog", "worktree_id").await);
    assert!(table_exists(&conn, "sparse_view").await);
    assert!(table_exists(&conn, "object_obliteration").await);
    assert!(table_exists(&conn, "layer").await);
    assert!(table_exists(&conn, "layer_path").await);
    assert!(table_exists(&conn, "metadata_kv").await);
    assert!(table_exists(&conn, "working_dirty").await);
    assert!(table_exists(&conn, "working_dirty_meta").await);
    assert!(table_exists(&conn, "revision_ordinal").await);
    assert!(table_exists(&conn, "revision_ordinal_meta").await);
    assert!(table_exists(&conn, "ai_final_decision").await);
    assert!(table_exists(&conn, "automation_log").await);
    assert!(table_exists(&conn, "agent_usage_stats").await);
    assert!(table_exists(&conn, "agent_session").await);
    assert!(table_exists(&conn, "agent_checkpoint").await);
    assert!(table_exists(&conn, "approved_permission").await);
    assert!(column_exists(&conn, "agent_usage_stats", "agent_name").await);
    assert!(index_exists(&conn, "idx_agent_usage_stats_agent_name_provider_model").await);
    assert!(table_exists(&conn, "source_call_log").await);
    assert!(index_exists(&conn, "idx_source_call_log_session").await);
    assert!(column_exists(&conn, "source_call_log", "agent_run_id").await);
    assert!(index_exists(&conn, "idx_source_call_log_agent_run_id").await);
    // lore.md 2.6: the 2026070401 migration folds cherry-pick into the unified
    // `sequence_state` and drops both the cherry_pick_state table and the
    // never-read revert_sequence orphan.
    assert!(!table_exists(&conn, "cherry_pick_state").await);
    assert!(!table_exists(&conn, "revert_sequence").await);
    assert!(table_exists(&conn, "sequence_state").await);
    assert!(table_exists(&conn, "notes").await);
    assert!(index_exists(&conn, "idx_notes_ref").await);
    // plan-20260713 DR-05c-0: per-turn coverage claim/revision gate.
    assert!(table_exists(&conn, "agent_coverage_claim").await);
    assert!(table_exists(&conn, "agent_coverage_revision").await);
    assert!(table_exists(&conn, "agent_coverage_conflict").await);
    assert!(index_exists(&conn, "idx_agent_coverage_claim_logical_key").await);
    assert!(index_exists(&conn, "idx_agent_coverage_claim_session_state").await);
    assert!(index_exists(&conn, "idx_agent_coverage_claim_checkpoint_id").await);
    assert!(index_exists(&conn, "idx_agent_coverage_revision_checkpoint_id").await);
    assert!(index_exists(&conn, "idx_agent_coverage_conflict_observed_at").await);
    // plan-20260713 DR-04b: OpenCode export-bridge job state.
    assert!(table_exists(&conn, "agent_export_job").await);
    assert!(index_exists(&conn, "idx_agent_export_job_session").await);
    assert!(index_exists(&conn, "idx_agent_export_job_ttl").await);
    // plan-20260713 M4: historical import identity + local erase barrier.
    assert!(table_exists(&conn, "agent_import_identity").await);
    assert!(index_exists(&conn, "idx_agent_import_identity_key").await);
    assert!(table_exists(&conn, "agent_import_tombstone").await);
    assert!(index_exists(&conn, "idx_agent_import_tombstone_provider").await);
    assert!(index_exists(&conn, "idx_agent_import_tombstone_erased_session").await);
    // plan-20260713 M5: source-scoped subagent content revisions + links.
    assert!(table_exists(&conn, "agent_subagent_content_claim").await);
    assert!(table_exists(&conn, "agent_subagent_content_revision").await);
    assert!(table_exists(&conn, "agent_subagent_link").await);
    assert!(table_exists(&conn, "agent_capture_incarnation").await);
    assert!(table_exists(&conn, "agent_checkpoint_prune_tombstone").await);
    assert!(column_exists(&conn, "agent_session", "sync_revision").await);
    assert!(column_exists(&conn, "agent_checkpoint", "sync_revision").await);
    assert!(column_exists(&conn, "agent_subagent_content_claim", "revision_cursor").await);
    assert!(column_exists(&conn, "agent_subagent_content_claim", "sync_revision").await);
    assert!(column_exists(&conn, "agent_subagent_link", "sync_revision").await);
    assert!(index_exists(&conn, "idx_agent_subagent_content_claim_current").await);
    assert!(index_exists(&conn, "idx_agent_subagent_link_parent_state").await);
    assert!(index_exists(&conn, "idx_agent_checkpoint_prune_tombstone_session").await);
}

#[tokio::test]
async fn agent_subagent_content_up_down_up_and_nonempty_guard() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = builtin_runner().expect("builtin runner");
    runner.run_pending(&conn).await.expect("M5 up #1");
    assert!(table_exists(&conn, "agent_subagent_content_claim").await);
    assert!(table_exists(&conn, "agent_subagent_content_revision").await);
    assert!(table_exists(&conn, "agent_subagent_link").await);
    assert!(table_exists(&conn, "agent_capture_incarnation").await);
    assert!(table_exists(&conn, "agent_checkpoint_prune_tombstone").await);
    assert!(column_exists(&conn, "agent_checkpoint", "sync_revision").await);

    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
    ))
    .await
    .expect("seed minimal ai_thread FK target");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, schema_version
         ) VALUES ('m5-down-session', 'claude_code', 'm5-down-provider',
                   'active', '/tmp', '{}', '{}', 1, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed M5 parent session");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_subagent_content_claim (
            parent_session_id, provider_kind, source_key, content_schema_version,
            state, attempt_digest, attempt_checkpoint_id, owner, lease_expires_at,
            fence_token, created_at, updated_at
         ) VALUES ('m5-down-session', 'claude_code', 'project/session/subagents/a.jsonl',
                   1, 'reserved', 'digest', 'attempt', 'owner', 999999, 1, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed M5 reservation");
    let error = runner
        .rollback_to(&conn, 2026071405)
        .await
        .expect_err("M5 down must preserve active recovery state");
    assert!(
        format!("{error:#}").contains("cannot roll back subagent replication"),
        "unexpected M5 rollback error: {error:#}"
    );
    assert_eq!(
        runner
            .current_version(&conn)
            .await
            .expect("current M5 version"),
        Some(2026071407)
    );
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM agent_subagent_content_claim".to_string(),
    ))
    .await
    .expect("clear M5 recovery state");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid,
            traces_commit, created_at
         ) VALUES ('m5-link-only-checkpoint', 'm5-down-session', 'subagent',
                   '1111111111111111111111111111111111111111',
                   '2222222222222222222222222222222222222222',
                   '3333333333333333333333333333333333333333', 1)"
            .to_string(),
    ))
    .await
    .expect("seed link-only M5 checkpoint");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_subagent_link (
            content_checkpoint_id, parent_session_id, link_state,
            boundary_checkpoint_id, stable_subagent_id, created_at, updated_at
         ) VALUES ('m5-link-only-checkpoint', 'm5-down-session', 'unresolved',
                   NULL, NULL, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed link-only M5 attribution");
    let error = runner
        .rollback_to(&conn, 2026071405)
        .await
        .expect_err("M5 down must preserve link-only attribution");
    assert!(
        format!("{error:#}").contains("cannot roll back subagent content attribution"),
        "unexpected link-only rollback error: {error:#}"
    );
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM agent_subagent_link".to_string(),
    ))
    .await
    .expect("clear link-only M5 link");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM agent_checkpoint WHERE checkpoint_id = 'm5-link-only-checkpoint'".to_string(),
    ))
    .await
    .expect("clear link-only M5 checkpoint");
    assert_eq!(
        runner
            .run_pending(&conn)
            .await
            .expect("restore 1407 after the 1406 link-only rollback guard"),
        vec![
            2026071407, 2026071901, 2026072101, 2026072201, 2026072301, 2026072302, 2026072303,
            2026072304, 2026072401, 2026072402, 2026072403
        ]
    );
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint_prune_tombstone (checkpoint_id, session_id, pruned_at)
         VALUES ('m5-pruned', 'm5-down-session', 1)"
            .to_string(),
    ))
    .await
    .expect("seed ordinary prune tombstone");
    let error = runner
        .rollback_to(&conn, 2026071405)
        .await
        .expect_err("M5 down must preserve cloud prune fences");
    assert!(
        format!("{error:#}").contains("cannot roll back subagent replication"),
        "unexpected prune-fence rollback error: {error:#}"
    );
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "DELETE FROM agent_checkpoint_prune_tombstone".to_string(),
    ))
    .await
    .expect("clear ordinary prune tombstone");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_session SET sync_revision = 2 WHERE session_id = 'm5-down-session'"
            .to_string(),
    ))
    .await
    .expect("advance session cloud generation");
    let error = runner
        .rollback_to(&conn, 2026071405)
        .await
        .expect_err("M5 down must preserve advanced session generations");
    assert!(
        format!("{error:#}").contains("cannot roll back subagent replication"),
        "unexpected session-generation rollback error: {error:#}"
    );
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "UPDATE agent_session SET sync_revision = 1 WHERE session_id = 'm5-down-session'"
            .to_string(),
    ))
    .await
    .expect("reset session generation for clean down/up exercise");
    assert_eq!(
        runner
            .rollback_to(&conn, 2026071405)
            .await
            .expect("M5 down after clearing state"),
        vec![2026071407, 2026071406]
    );
    assert!(!table_exists(&conn, "agent_subagent_content_claim").await);
    assert!(!table_exists(&conn, "agent_capture_incarnation").await);
    assert!(!table_exists(&conn, "agent_checkpoint_prune_tombstone").await);
    assert!(!column_exists(&conn, "agent_session", "sync_revision").await);
    assert!(!column_exists(&conn, "agent_checkpoint", "sync_revision").await);
    assert_eq!(
        runner.run_pending(&conn).await.expect("M5 up #2"),
        vec![
            2026071406, 2026071407, 2026071901, 2026072101, 2026072201, 2026072301, 2026072302,
            2026072303, 2026072304, 2026072401, 2026072402, 2026072403
        ]
    );
    assert!(table_exists(&conn, "agent_subagent_content_claim").await);
    assert!(table_exists(&conn, "agent_capture_incarnation").await);
    assert!(table_exists(&conn, "agent_checkpoint_prune_tombstone").await);
    assert!(column_exists(&conn, "agent_session", "sync_revision").await);
    assert!(column_exists(&conn, "agent_checkpoint", "sync_revision").await);
}

/// Early M5 builds applied the immutable 1406 claim/revision/link schema
/// before revision allocation and cloud generations moved to their own
/// migration.  Those repositories must receive 1407 instead of being left
/// with runtime queries against missing columns.
#[tokio::test]
async fn existing_agent_subagent_1406_schema_upgrades_to_replication() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let mut base_runner = MigrationRunner::new();
    base_runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|migration| migration.version <= 2026071406),
        )
        .expect("build immutable 1406 registry");
    base_runner
        .run_pending(&conn)
        .await
        .expect("construct immutable 1406 schema");

    assert!(!column_exists(&conn, "agent_session", "sync_revision").await);
    assert!(!column_exists(&conn, "agent_checkpoint", "sync_revision").await);
    assert!(!column_exists(&conn, "agent_subagent_content_claim", "revision_cursor").await);
    assert!(!column_exists(&conn, "agent_subagent_content_claim", "sync_revision").await);
    assert!(!column_exists(&conn, "agent_subagent_link", "sync_revision").await);
    assert!(!table_exists(&conn, "agent_capture_incarnation").await);
    assert!(!table_exists(&conn, "agent_capture_cloud_base").await);
    assert!(!table_exists(&conn, "agent_checkpoint_prune_tombstone").await);

    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
    ))
    .await
    .expect("seed immutable 1406 ai_thread FK target");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, schema_version
         ) VALUES ('m5-compat-session', 'claude_code', 'm5-compat-provider',
                   'active', '/tmp', '{}', '{}', 1, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed immutable 1406 parent session");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid,
            traces_commit, created_at
         ) VALUES ('m5-compat-checkpoint', 'm5-compat-session', 'subagent',
                   '1111111111111111111111111111111111111111',
                   '2222222222222222222222222222222222222222',
                   '3333333333333333333333333333333333333333', 1)"
            .to_string(),
    ))
    .await
    .expect("seed immutable 1406 checkpoint");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_subagent_content_claim (
            parent_session_id, provider_kind, source_key, content_schema_version,
            current_revision, current_checkpoint_id, current_digest, state,
            fence_token, created_at, updated_at
         ) VALUES ('m5-compat-session', 'claude_code', 'source/sha256/compat', 1,
                   3, 'm5-compat-checkpoint', 'digest-3', 'idle', 3, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed immutable 1406 current claim");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_subagent_content_revision (
            parent_session_id, provider_kind, source_key, content_schema_version,
            revision, checkpoint_id, content_digest, source_channel, partial, created_at
         ) VALUES ('m5-compat-session', 'claude_code', 'source/sha256/compat', 1,
                   3, 'm5-compat-checkpoint', 'digest-3', 'import', 0, 1)"
            .to_string(),
    ))
    .await
    .expect("seed immutable 1406 revision");

    let runner = builtin_runner().expect("current builtin runner");
    assert_eq!(
        runner
            .run_pending(&conn)
            .await
            .expect("upgrade immutable 1406 schema"),
        vec![
            2026071407, 2026071901, 2026072101, 2026072201, 2026072301, 2026072302, 2026072303,
            2026072304, 2026072401, 2026072402, 2026072403
        ]
    );
    let claim = conn
        .query_one(Statement::from_string(
            conn.get_database_backend(),
            "SELECT revision_cursor, sync_revision
             FROM agent_subagent_content_claim
             WHERE parent_session_id = 'm5-compat-session'"
                .to_string(),
        ))
        .await
        .expect("read upgraded M5 claim")
        .expect("upgraded M5 claim exists");
    assert_eq!(
        claim
            .try_get_by::<i64, _>("revision_cursor")
            .expect("revision cursor"),
        3
    );
    assert_eq!(
        claim
            .try_get_by::<i64, _>("sync_revision")
            .expect("claim sync revision"),
        1
    );
    assert!(column_exists(&conn, "agent_session", "sync_revision").await);
    assert!(column_exists(&conn, "agent_checkpoint", "sync_revision").await);
    assert!(column_exists(&conn, "agent_subagent_link", "sync_revision").await);
    assert!(table_exists(&conn, "agent_capture_incarnation").await);
    assert!(table_exists(&conn, "agent_capture_cloud_base").await);
    assert!(table_exists(&conn, "agent_checkpoint_prune_tombstone").await);
    assert!(trigger_exists(&conn, "trg_agent_subagent_boundary_delete").await);
}

/// Later M5 development builds had already folded the replication columns
/// into 1406.  The 1407 compatibility migration must probe them instead of
/// issuing duplicate ALTER statements (the shape used by long-lived global
/// config databases during the live gate).
#[tokio::test]
async fn evolved_agent_subagent_1406_columns_upgrade_idempotently() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let mut base_runner = MigrationRunner::new();
    base_runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|migration| migration.version <= 2026071406),
        )
        .expect("build immutable 1406 registry");
    base_runner
        .run_pending(&conn)
        .await
        .expect("construct immutable 1406 schema");
    for alter in [
        "ALTER TABLE agent_session ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE agent_checkpoint ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE agent_subagent_content_claim ADD COLUMN revision_cursor INTEGER NOT NULL DEFAULT 0",
        "ALTER TABLE agent_subagent_content_claim ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 1",
        "ALTER TABLE agent_subagent_link ADD COLUMN sync_revision INTEGER NOT NULL DEFAULT 1",
    ] {
        conn.execute(Statement::from_string(
            conn.get_database_backend(),
            alter.to_string(),
        ))
        .await
        .expect("construct evolved 1406 column shape");
    }
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
    ))
    .await
    .expect("seed evolved 1406 ai_thread FK target");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at,
            schema_version, sync_revision
         ) VALUES ('m5-evolved-session', 'claude_code', 'm5-evolved-provider',
                   'active', '/tmp', '{}', '{}', 1, 1, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed evolved 1406 parent session");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_checkpoint (
            checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid,
            traces_commit, created_at, sync_revision
         ) VALUES ('m5-evolved-checkpoint', 'm5-evolved-session', 'subagent',
                   '1111111111111111111111111111111111111111',
                   '2222222222222222222222222222222222222222',
                   '3333333333333333333333333333333333333333', 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed evolved 1406 checkpoint");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_subagent_content_claim (
            parent_session_id, provider_kind, source_key, content_schema_version,
            current_revision, current_checkpoint_id, current_digest, state,
            fence_token, created_at, updated_at, revision_cursor, sync_revision
         ) VALUES ('m5-evolved-session', 'claude_code', 'source/sha256/evolved', 1,
                   3, 'm5-evolved-checkpoint', 'digest-3', 'idle', 3, 1, 1, 0, 1)"
            .to_string(),
    ))
    .await
    .expect("seed evolved 1406 stale cursor");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_subagent_content_revision (
            parent_session_id, provider_kind, source_key, content_schema_version,
            revision, checkpoint_id, content_digest, source_channel, partial, created_at
         ) VALUES ('m5-evolved-session', 'claude_code', 'source/sha256/evolved', 1,
                   3, 'm5-evolved-checkpoint', 'digest-3', 'import', 0, 1)"
            .to_string(),
    ))
    .await
    .expect("seed evolved 1406 revision");

    let runner = builtin_runner().expect("current builtin runner");
    assert_eq!(
        runner
            .run_pending(&conn)
            .await
            .expect("upgrade evolved 1406 schema"),
        vec![
            2026071407, 2026071901, 2026072101, 2026072201, 2026072301, 2026072302, 2026072303,
            2026072304, 2026072401, 2026072402, 2026072403
        ]
    );
    let cursor = conn
        .query_one(Statement::from_string(
            conn.get_database_backend(),
            "SELECT revision_cursor FROM agent_subagent_content_claim
             WHERE parent_session_id = 'm5-evolved-session'"
                .to_string(),
        ))
        .await
        .expect("read evolved 1406 upgraded cursor")
        .expect("evolved 1406 claim exists")
        .try_get_by::<i64, _>("revision_cursor")
        .expect("decode evolved 1406 upgraded cursor");
    assert_eq!(cursor, 3);
    assert!(table_exists(&conn, "agent_capture_incarnation").await);
    assert!(table_exists(&conn, "agent_capture_cloud_base").await);
    assert!(table_exists(&conn, "agent_checkpoint_prune_tombstone").await);
    assert!(trigger_exists(&conn, "trg_agent_subagent_boundary_delete").await);
}

/// M4 migration gate: both import tables roll back in reverse order and
/// reapply cleanly, while the older export-job schema remains intact.
#[tokio::test]
async fn agent_import_identity_tombstone_up_down_up_round_trip() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = builtin_runner().expect("builtin runner");

    runner.run_pending(&conn).await.expect("M4 up #1");
    assert!(table_exists(&conn, "agent_import_identity").await);
    assert!(table_exists(&conn, "agent_import_tombstone").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_session_insert").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_session_update").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_checkpoint_insert").await);

    let rolled = runner
        .rollback_to(&conn, 2026071401)
        .await
        .expect("rollback M4 import migrations");
    assert_eq!(
        rolled,
        vec![
            2026072403, 2026072402, 2026072401, 2026072304, 2026072303, 2026072302, 2026072301,
            2026072201, 2026072101, 2026071901, 2026071407, 2026071406, 2026071405, 2026071404,
            2026071403, 2026071402
        ]
    );
    assert!(!table_exists(&conn, "agent_import_identity").await);
    assert!(!table_exists(&conn, "agent_import_tombstone").await);
    assert!(!table_exists(&conn, "agent_coverage_conflict").await);
    assert!(!trigger_exists(&conn, "agent_tombstone_block_session_insert").await);
    assert!(!trigger_exists(&conn, "agent_tombstone_block_session_update").await);
    assert!(!trigger_exists(&conn, "agent_tombstone_block_checkpoint_insert").await);
    assert!(table_exists(&conn, "agent_export_job").await);

    let reapplied = runner.run_pending(&conn).await.expect("M4 up #2");
    assert_eq!(
        reapplied,
        vec![
            2026071402, 2026071403, 2026071404, 2026071405, 2026071406, 2026071407, 2026071901,
            2026072101, 2026072201, 2026072301, 2026072302, 2026072303, 2026072304, 2026072401,
            2026072402, 2026072403
        ]
    );
    assert!(table_exists(&conn, "agent_import_identity").await);
    assert!(table_exists(&conn, "agent_import_tombstone").await);
    assert!(table_exists(&conn, "agent_coverage_conflict").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_session_insert").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_session_update").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_checkpoint_insert").await);
}

/// Repositories that shipped at 2026071403 already have the tombstone table
/// but not the old-writer compatibility triggers. The monotonic follow-up
/// migration must install those triggers instead of relying on edited 1403
/// DDL that MigrationRunner will correctly skip.
#[tokio::test]
async fn existing_agent_tombstone_1403_schema_upgrades_to_compat_barrier() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = builtin_runner().expect("builtin runner");
    runner
        .run_pending(&conn)
        .await
        .expect("apply current migration registry");
    let rolled = runner
        .rollback_to(&conn, 2026071403)
        .await
        .expect("construct released 1403 schema shape");
    assert_eq!(
        rolled,
        vec![
            2026072403, 2026072402, 2026072401, 2026072304, 2026072303, 2026072302, 2026072301,
            2026072201, 2026072101, 2026071901, 2026071407, 2026071406, 2026071405, 2026071404
        ]
    );
    assert!(table_exists(&conn, "agent_import_tombstone").await);
    assert!(!trigger_exists(&conn, "agent_tombstone_block_session_insert").await);
    assert!(!trigger_exists(&conn, "agent_tombstone_block_session_update").await);
    assert!(!trigger_exists(&conn, "agent_tombstone_block_checkpoint_insert").await);

    let applied = runner
        .run_pending(&conn)
        .await
        .expect("upgrade existing 1403 schema");
    assert_eq!(
        applied,
        vec![
            2026071404, 2026071405, 2026071406, 2026071407, 2026071901, 2026072101, 2026072201,
            2026072301, 2026072302, 2026072303, 2026072304, 2026072401, 2026072402, 2026072403
        ]
    );
    assert!(trigger_exists(&conn, "agent_tombstone_block_session_insert").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_session_update").await);
    assert!(trigger_exists(&conn, "agent_tombstone_block_checkpoint_insert").await);
}

#[tokio::test]
async fn agent_import_tombstone_rollback_refuses_nonempty_barrier() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = builtin_runner().expect("builtin runner");
    runner
        .run_pending(&conn)
        .await
        .expect("apply M4 migrations");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_import_tombstone (
            tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at
         ) VALUES ('t1', 'claude_code', 'provider-erased', 'claude__provider-erased', 1)"
            .to_string(),
    ))
    .await
    .expect("seed anti-resurrection barrier");

    let error = runner
        .rollback_to(&conn, 2026071402)
        .await
        .expect_err("non-empty tombstone rollback must be irreversible in practice");
    assert!(
        format!("{error:#}").contains("cannot roll back agent tombstones"),
        "unexpected rollback error: {error:#}"
    );
    assert!(table_exists(&conn, "agent_import_tombstone").await);
    let count: i64 = conn
        .query_one(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM agent_import_tombstone".to_string(),
        ))
        .await
        .expect("query tombstone")
        .expect("count row")
        .try_get_by("n")
        .expect("decode count");
    assert_eq!(count, 1, "rollback failure lost tombstone data");
    assert_eq!(
        runner
            .current_version(&conn)
            .await
            .expect("current version"),
        Some(2026071404),
        "failed down migration removed its schema registry row"
    );
}

#[tokio::test]
async fn agent_coverage_conflict_rollback_refuses_nonempty_evidence() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = builtin_runner().expect("builtin runner");
    runner
        .run_pending(&conn)
        .await
        .expect("apply M4 migrations");
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_string(
        backend,
        "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
    ))
    .await
    .expect("seed minimal ai_thread FK target");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, schema_version
         ) VALUES ('conflict-down-session', 'claude_code', 'conflict-down-provider',
                   'active', '/tmp', '{}', '{}', 1, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed conflict session");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO agent_coverage_claim (
            session_id, logical_turn_key, coverage_schema_version,
            coverage_digest, completeness, revision, state, source_channel,
            created_at, updated_at
         ) VALUES ('conflict-down-session', 'turn-1', 1, 'incumbent',
                   'complete', 1, 'conflicted', 'live', 1, 2)"
            .to_string(),
    ))
    .await
    .expect("seed conflicted claim");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO agent_coverage_conflict (
            session_id, logical_turn_key, coverage_schema_version,
            incumbent_revision, incumbent_digest, incumbent_checkpoint_id,
            incoming_digest, incoming_source_channel, incoming_observed_at,
            incoming_canonical_json, incoming_redaction_report_json
         ) VALUES ('conflict-down-session', 'turn-1', 1, 1, 'incumbent', NULL,
                   'challenger', 'live', 2, '[]',
                   '{\"matches\":[],\"bytes_scanned\":0,\"bytes_redacted\":0}')"
            .to_string(),
    ))
    .await
    .expect("seed durable challenger evidence");

    let error = runner
        .rollback_to(&conn, 2026071404)
        .await
        .expect_err("non-empty conflict rollback must refuse evidence loss");
    assert!(
        format!("{error:#}").contains("cannot roll back agent coverage conflicts"),
        "unexpected rollback error: {error:#}"
    );
    assert!(table_exists(&conn, "agent_coverage_conflict").await);
    let count: i64 = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT COUNT(*) AS n FROM agent_coverage_conflict".to_string(),
        ))
        .await
        .expect("query conflict evidence")
        .expect("count row")
        .try_get_by("n")
        .expect("decode count");
    assert_eq!(count, 1, "rollback failure lost challenger evidence");
    assert_eq!(
        runner
            .current_version(&conn)
            .await
            .expect("current version"),
        Some(2026071405),
        "failed conflict down migration removed its schema registry row"
    );
}

#[tokio::test]
async fn agent_import_identity_rollback_refuses_nonempty_recovery_state() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = builtin_runner().expect("builtin runner");
    runner
        .run_pending(&conn)
        .await
        .expect("apply M4 migrations");
    conn.execute(Statement::from_string(
        conn.get_database_backend(),
        "INSERT INTO agent_import_identity (
            identity_id, agent_kind, provider_session_id, source_kind, source_id,
            schema_version, next_ordinal, state, created_at, updated_at
         ) VALUES ('identity-down-guard', 'claude_code', 'provider-down-guard',
                   'file', 'relative/source', 1, 0, 'failed', 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed import recovery identity");

    let error = runner
        .rollback_to(&conn, 2026071401)
        .await
        .expect_err("non-empty import identity rollback must refuse data loss");
    assert!(
        format!("{error:#}").contains("cannot roll back agent import identities"),
        "unexpected rollback error: {error:#}"
    );
    assert!(table_exists(&conn, "agent_import_identity").await);
    assert_eq!(
        runner
            .current_version(&conn)
            .await
            .expect("current version"),
        Some(2026071402),
        "failed identity down migration removed its schema registry row"
    );
}

/// M4 schema failure matrix: stable identity, state machine, provider
/// tombstone key, and erased-session lookup key all fail closed.
#[tokio::test]
async fn agent_import_schema_constraints_fail_closed() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    run_builtin_migrations(&conn)
        .await
        .expect("apply builtin migrations");
    let backend = conn.get_database_backend();
    // This registry-only fixture intentionally does not replay the bootstrap
    // schema, while `agent_session.thread_id` carries a soft FK to ai_thread.
    // Supply the minimal FK target so the rollback-barrier assertions exercise
    // normal foreign-key enforcement instead of disabling it.
    conn.execute(Statement::from_string(
        backend,
        "CREATE TABLE IF NOT EXISTS ai_thread (thread_id TEXT PRIMARY KEY)".to_string(),
    ))
    .await
    .expect("seed minimal ai_thread FK target");

    let identity_sql = "INSERT INTO agent_import_identity (
        identity_id, agent_kind, provider_session_id, source_kind, source_id,
        schema_version, next_ordinal, state, created_at, updated_at
     ) VALUES (?, 'claude_code', 'provider-1', 'file', 'relative/source', 1, 0, ?, 1, 1)";
    conn.execute(Statement::from_sql_and_values(
        backend,
        identity_sql,
        ["identity-1".into(), "discovered".into()],
    ))
    .await
    .expect("seed valid identity");
    assert!(
        conn.execute(Statement::from_sql_and_values(
            backend,
            identity_sql,
            ["identity-2".into(), "discovered".into()],
        ))
        .await
        .is_err(),
        "stable import identity key must be unique"
    );
    assert!(
        conn.execute(Statement::from_sql_and_values(
            backend,
            "INSERT INTO agent_import_identity (
                identity_id, agent_kind, provider_session_id, source_kind, source_id,
                schema_version, next_ordinal, state, created_at, updated_at
             ) VALUES ('identity-bad', 'codex', 'provider-2', 'file', 'x', 1, 0,
                       'resurrecting', 1, 1)",
            Vec::<sea_orm::Value>::new(),
        ))
        .await
        .is_err(),
        "unknown import state must violate CHECK"
    );

    let tombstone_sql = "INSERT INTO agent_import_tombstone (
        tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at
     ) VALUES (?, 'claude_code', ?, ?, 1)";
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO agent_session (
            session_id, agent_kind, provider_session_id, state, working_dir,
            metadata_json, redaction_report, started_at, last_event_at, schema_version
         ) VALUES ('session-a', 'claude_code', 'provider-a', 'active', '/tmp',
                   '{}', '{}', 1, 1, 1)"
            .to_string(),
    ))
    .await
    .expect("seed session that will be tombstoned");
    conn.execute(Statement::from_sql_and_values(
        backend,
        tombstone_sql,
        ["tomb-1".into(), "provider-a".into(), "session-a".into()],
    ))
    .await
    .expect("seed tombstone");
    assert!(
        conn.execute(Statement::from_sql_and_values(
            backend,
            tombstone_sql,
            ["tomb-2".into(), "provider-a".into(), "session-b".into()],
        ))
        .await
        .is_err(),
        "provider tombstone key must be unique"
    );
    assert!(
        conn.execute(Statement::from_sql_and_values(
            backend,
            tombstone_sql,
            ["tomb-3".into(), "provider-b".into(), "session-a".into()],
        ))
        .await
        .is_err(),
        "erased session id must remain uniquely classifiable"
    );
    assert!(
        conn.execute(Statement::from_string(
            backend,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, schema_version
             ) VALUES ('session-recreated', 'claude_code', 'provider-a', 'active', '/tmp',
                       '{}', '{}', 2, 2, 1)"
                .to_string(),
        ))
        .await
        .is_err(),
        "an older writer must not recreate a tombstoned provider session"
    );
    assert!(
        conn.execute(Statement::from_string(
            backend,
            "UPDATE agent_session SET last_event_at = 2 WHERE session_id = 'session-a'".to_string(),
        ))
        .await
        .is_err(),
        "an older writer must not advance a session after its tombstone"
    );
    assert!(
        conn.execute(Statement::from_string(
            backend,
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, tree_oid, metadata_blob_oid,
                traces_commit, created_at
             ) VALUES ('checkpoint-after-erase', 'session-a', 'committed',
                       'tree', 'metadata', 'commit', 2)"
                .to_string(),
        ))
        .await
        .is_err(),
        "an older writer must not publish a checkpoint after erasure"
    );
}

/// OC-Phase 2 P2.5 regression guard: `approved_permission` survives an
/// up → down → up round-trip cleanly. The down migration drops the table
/// and the index destructively, so a subsequent up must re-create both
/// without colliding on a stale `IF NOT EXISTS`.
#[tokio::test]
async fn approved_permission_up_down_up_round_trip() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let runner = builtin_runner().expect("builtin runner builds clean");

    // Up: full registry applied.
    runner
        .run_pending(&conn)
        .await
        .expect("first up applies cleanly");
    assert!(table_exists(&conn, "approved_permission").await);
    assert!(index_exists(&conn, "idx_approved_permission_project").await);

    // Down: roll approved_permission off again. Newer migrations stacked above
    // it must roll back first, while the older migrations stay applied.
    let rolled = runner
        .rollback_to(&conn, 2026050501)
        .await
        .expect("rollback past approved_permission");
    assert_eq!(
        rolled,
        vec![
            2026072403, 2026072402, 2026072401, 2026072304, 2026072303, 2026072302, 2026072301,
            2026072201, 2026072101, 2026071901, 2026071407, 2026071406, 2026071405, 2026071404,
            2026071403, 2026071402, 2026071401, 2026071301, 2026070803, 2026070802, 2026070801,
            2026070701, 2026070601, 2026070501, 2026070401, 2026070301, 2026070202, 2026070201,
            2026062301, 2026061401, 2026060801, 2026060401, 2026060201, 2026053101, 2026052301,
            2026050801, 2026050601
        ]
    );
    assert!(
        !table_exists(&conn, "approved_permission").await,
        "down migration must drop the table"
    );
    assert!(
        !index_exists(&conn, "idx_approved_permission_project").await,
        "down migration must drop the index"
    );
    assert!(
        !column_exists(&conn, "agent_usage_stats", "agent_name").await,
        "newer migration down must remove the agent_name column"
    );

    // Up again: re-create the table + indexes with no `IF NOT EXISTS` collision.
    let reapplied = runner
        .run_pending(&conn)
        .await
        .expect("second up reapplies cleanly");
    assert_eq!(
        reapplied,
        vec![
            2026050601, 2026050801, 2026052301, 2026053101, 2026060201, 2026060401, 2026060801,
            2026061401, 2026062301, 2026070201, 2026070202, 2026070301, 2026070401, 2026070501,
            2026070601, 2026070701, 2026070801, 2026070802, 2026070803, 2026071301, 2026071401,
            2026071402, 2026071403, 2026071404, 2026071405, 2026071406, 2026071407, 2026071901,
            2026072101, 2026072201, 2026072301, 2026072302, 2026072303, 2026072304, 2026072401,
            2026072402, 2026072403
        ]
    );
    assert!(table_exists(&conn, "approved_permission").await);
    assert!(index_exists(&conn, "idx_approved_permission_project").await);
    assert!(column_exists(&conn, "agent_usage_stats", "agent_name").await);
    assert!(index_exists(&conn, "idx_agent_usage_stats_agent_name_provider_model").await);
}

// ---------------------------------------------------------------------------
// 2026072101 rebase_state worktree scope (plan-20260714 Part C W1 §C.4.2)
// ---------------------------------------------------------------------------

/// A database whose `rebase_state` still has a lazy-DDL-era column subset
/// (here the 8-column bootstrap shape: no autosquash / todo_actions /
/// empty_mode) upgrades through `run_builtin_migrations` — the normalize hook
/// fills the missing columns, then the 2026072101 static rebuild re-keys the
/// table by `worktree_id` — with the in-progress row preserved at the MAIN
/// scope (`worktree_id = ''`) and lazy-column defaults applied.
#[tokio::test]
async fn rebase_state_migration_preserves_active_row_from_lazy_shape() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `rebase_state` (
            `id`           INTEGER PRIMARY KEY AUTOINCREMENT,
            `head_name`    TEXT NOT NULL,
            `onto`         TEXT NOT NULL,
            `orig_head`    TEXT NOT NULL,
            `current_head` TEXT NOT NULL,
            `todo`         TEXT NOT NULL,
            `done`         TEXT NOT NULL,
            `stopped_sha`  TEXT
        );"#
        .to_string(),
    ))
    .await
    .expect("legacy 8-column table");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO rebase_state \
         (head_name, onto, orig_head, current_head, todo, done, stopped_sha) \
         VALUES ('refs/heads/main', 'aa11', 'bb22', 'cc33', 'dd44', '', NULL);"
            .to_string(),
    ))
    .await
    .expect("legacy in-progress row");

    run_builtin_migrations(&conn).await.expect("migrations");

    assert!(column_exists(&conn, "rebase_state", "worktree_id").await);
    assert!(
        !column_exists(&conn, "rebase_state", "id").await,
        "the AUTOINCREMENT id is retired by the worktree_id re-key"
    );
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT worktree_id, head_name, todo_actions, autosquash, empty_mode \
             FROM rebase_state"
                .to_string(),
        ))
        .await
        .expect("query migrated row")
        .expect("row survives the rebuild");
    let worktree_id: String = row.try_get_by_index(0).expect("worktree_id");
    let head_name: String = row.try_get_by_index(1).expect("head_name");
    let todo_actions: String = row.try_get_by_index(2).expect("todo_actions");
    let autosquash: i64 = row.try_get_by_index(3).expect("autosquash");
    let empty_mode: String = row.try_get_by_index(4).expect("empty_mode");
    assert_eq!(
        worktree_id, "",
        "pre-existing row belongs to the main scope"
    );
    assert_eq!(head_name, "refs/heads/main");
    assert_eq!(todo_actions, "", "lazy-column default filled in");
    assert_eq!(autosquash, 0, "lazy-column default filled in");
    assert_eq!(empty_mode, "keep", "lazy-column default filled in");
}

/// The 2026072101 down migration FAILS CLOSED while a linked worktree's
/// rebase row exists (the legacy single-row schema cannot represent it, so
/// rolling back would silently discard that worktree's rebase). After the
/// linked row is gone, the same rollback succeeds and restores the legacy
/// shape with the main row intact.
#[tokio::test]
async fn rebase_down_migration_rejects_linked_rows() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO rebase_state \
         (worktree_id, head_name, onto, orig_head, current_head, todo, done) \
         VALUES ('', 'refs/heads/main', 'aa', 'bb', 'cc', 'dd', ''), \
                ('wt1234', 'refs/heads/feature', 'ee', 'ff', 'aa', 'bb', '');"
            .to_string(),
    ))
    .await
    .expect("main + linked rows");

    let runner = builtin_runner().expect("builtin runner");
    let err = runner
        .rollback_to(&conn, 2026071901)
        .await
        .expect_err("rollback with a linked rebase row must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the down-guard CHECK: {rendered}"
    );
    // The scoped table survives the refused rollback (txn rolled back).
    assert!(column_exists(&conn, "rebase_state", "worktree_id").await);

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM rebase_state WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("finish the linked rebase");
    let rolled = runner
        .rollback_to(&conn, 2026071901)
        .await
        .expect("rollback succeeds once only the main row remains");
    assert_eq!(
        rolled,
        vec![
            2026072402, 2026072401, 2026072304, 2026072303, 2026072302, 2026072301, 2026072201,
            2026072101
        ]
    );
    assert!(column_exists(&conn, "rebase_state", "id").await);
    assert!(!column_exists(&conn, "rebase_state", "worktree_id").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT head_name FROM rebase_state".to_string(),
        ))
        .await
        .expect("query restored row")
        .expect("main row survives the rollback");
    let head_name: String = row.try_get_by_index(0).expect("head_name");
    assert_eq!(head_name, "refs/heads/main");
}

/// The 2026071901 down migration FAILS CLOSED while a linked worktree's
/// sequence row exists (the legacy `CHECK(id = 1)` single-row schema cannot
/// represent it, so rolling back would silently discard that worktree's
/// cherry-pick/am/revert sequence). After the linked row is gone, the same
/// rollback succeeds and restores the legacy shape with the main row intact.
#[tokio::test]
async fn sequence_down_migration_rejects_linked_rows() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO sequence_state \
         (worktree_id, kind, head_name, head_orig, current_oid, todo) \
         VALUES ('', 'cherry_pick', 'refs/heads/main', 'aa', 'bb', '[]'), \
                ('wt1234', 'revert', 'refs/heads/feature', 'cc', 'dd', '[]');"
            .to_string(),
    ))
    .await
    .expect("main + linked sequence rows");

    let runner = builtin_runner().expect("builtin runner");
    let err = runner
        .rollback_to(&conn, 2026071407)
        .await
        .expect_err("rollback with a linked sequence row must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the down-guard CHECK: {rendered}"
    );
    // The scoped table survives the refused rollback (txn rolled back).
    assert!(column_exists(&conn, "sequence_state", "worktree_id").await);

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM sequence_state WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("finish the linked sequence");
    // The runner commits each down migration separately, so the refused
    // attempt already rolled back 2026072301/2026072201/2026072101 before
    // failing closed at 2026071901 — only the guarded one remains.
    let rolled = runner
        .rollback_to(&conn, 2026071407)
        .await
        .expect("rollback succeeds once only the main row remains");
    assert_eq!(
        rolled,
        vec![
            2026072402, 2026072401, 2026072304, 2026072303, 2026072302, 2026072301, 2026072201,
            2026072101, 2026071901
        ]
    );
    assert!(!column_exists(&conn, "sequence_state", "worktree_id").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT kind, head_name FROM sequence_state".to_string(),
        ))
        .await
        .expect("query restored row")
        .expect("main row survives the rollback");
    let kind: String = row.try_get_by_index(0).expect("kind");
    let head_name: String = row.try_get_by_index(1).expect("head_name");
    assert_eq!(kind, "cherry_pick");
    assert_eq!(head_name, "refs/heads/main");
}

/// A database whose `bisect_state` still carries the OLDEST lazy shape
/// (before `completed`/`first_parent`/`worktree_id` were ADD COLUMNed) is
/// normalized on connection open and re-keyed by 2026072301: the newest row
/// wins, lands in the main scope, and the lazy defaults are filled in.
#[tokio::test]
async fn bisect_state_migration_preserves_active_row_from_lazy_shape() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `bisect_state` (
            `id`             INTEGER PRIMARY KEY AUTOINCREMENT,
            `orig_head`      TEXT NOT NULL,
            `orig_head_name` TEXT,
            `bad`            TEXT,
            `good`           TEXT NOT NULL,
            `current`        TEXT,
            `skipped`        TEXT,
            `steps`          INTEGER
        );"#
        .to_string(),
    ))
    .await
    .expect("oldest lazy 8-column table");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO bisect_state \
         (orig_head, orig_head_name, bad, good, current, skipped, steps) \
         VALUES ('aa11', 'refs/heads/main', 'bb22', '[\"cc33\"]', 'dd44', '[]', 3), \
                ('ee55', NULL, 'ff66', '[\"aa77\"]', 'bb88', '[]', 2);"
            .to_string(),
    ))
    .await
    .expect("stale + newest lazy rows");

    run_builtin_migrations(&conn).await.expect("migrations");

    assert!(column_exists(&conn, "bisect_state", "worktree_id").await);
    assert!(
        !column_exists(&conn, "bisect_state", "id").await,
        "the AUTOINCREMENT id is retired by the worktree_id re-key"
    );
    let rows = conn
        .query_all(Statement::from_string(
            backend,
            "SELECT worktree_id, orig_head, completed, first_parent FROM bisect_state".to_string(),
        ))
        .await
        .expect("query migrated rows");
    assert_eq!(rows.len(), 1, "newest id wins per scope");
    let worktree_id: String = rows[0].try_get_by_index(0).expect("worktree_id");
    let orig_head: String = rows[0].try_get_by_index(1).expect("orig_head");
    let completed: i64 = rows[0].try_get_by_index(2).expect("completed");
    let first_parent: i64 = rows[0].try_get_by_index(3).expect("first_parent");
    assert_eq!(
        worktree_id, "",
        "pre-existing rows belong to the main scope"
    );
    assert_eq!(orig_head, "ee55", "newest lazy row survives");
    assert_eq!(completed, 0, "lazy-column default filled in");
    assert_eq!(first_parent, 0, "lazy-column default filled in");
}

/// A v0.19.34-era lazy shape (worktree_id already ADD COLUMNed, AUTOINCREMENT
/// id, stale + newest rows in SEVERAL scopes) is re-keyed to exactly one row
/// per scope — the newest id in each scope wins, linked rows included.
#[tokio::test]
async fn bisect_state_migration_keeps_newest_row_per_scope() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `bisect_state` (
            `id`             INTEGER PRIMARY KEY AUTOINCREMENT,
            `orig_head`      TEXT NOT NULL,
            `orig_head_name` TEXT,
            `bad`            TEXT,
            `good`           TEXT NOT NULL,
            `current`        TEXT,
            `skipped`        TEXT,
            `steps`          INTEGER,
            `completed`      INTEGER NOT NULL DEFAULT 0,
            `first_parent`   INTEGER NOT NULL DEFAULT 0,
            `worktree_id`    TEXT NOT NULL DEFAULT ''
        );"#
        .to_string(),
    ))
    .await
    .expect("v0.19.34-era lazy full shape");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO bisect_state \
         (orig_head, good, completed, first_parent, worktree_id) \
         VALUES ('main-old', '[]', 1, 0, ''), \
                ('main-new', '[]', 0, 0, ''), \
                ('wt1-old', '[]', 0, 1, 'wt1'), \
                ('wt1-new', '[]', 0, 1, 'wt1'), \
                ('wt2-only', '[]', 0, 0, 'wt2');"
            .to_string(),
    ))
    .await
    .expect("stale + newest rows across three scopes");

    run_builtin_migrations(&conn).await.expect("migrations");

    let rows = conn
        .query_all(Statement::from_string(
            backend,
            "SELECT worktree_id, orig_head FROM bisect_state ORDER BY worktree_id".to_string(),
        ))
        .await
        .expect("query migrated rows");
    let survivors: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get_by_index(0).expect("worktree_id"),
                row.try_get_by_index(1).expect("orig_head"),
            )
        })
        .collect();
    assert_eq!(
        survivors,
        vec![
            ("".to_string(), "main-new".to_string()),
            ("wt1".to_string(), "wt1-new".to_string()),
            ("wt2".to_string(), "wt2-only".to_string()),
        ],
        "newest id per scope wins; no scope is dropped"
    );
}

/// Two runners racing `run_pending` on the same fresh database: the version
/// claim happens before the up DDL inside each migration's transaction, so
/// every migration is applied exactly once (RENAME-based rebuilds like
/// 2026072101/2026072301 are not idempotent and must never run twice) and
/// both callers finish without error.
///
/// The race is FORCED deterministically: both racers rendezvous in the
/// runner's post-read gate (`run_pending_with_post_read_gate`), i.e. AFTER
/// each has read the same empty current-version and computed the full
/// pending list, but BEFORE either has claimed anything. Every subsequent
/// version claim is therefore contended by construction — under the old
/// DDL-before-claim order the claim loser re-ran the RENAME rebuilds
/// against already-rebuilt tables and errored deterministically. The
/// path intentionally skips the normalize hooks (raw `builtin_runner` on a
/// fresh database — the rebuild migrations self-provision their input
/// shape), so nothing outside the gate can serialize the racers.
/// Three rounds vary the interleaving of the claims themselves.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_run_pending_applies_each_migration_exactly_once() {
    for round in 0..3 {
        let (_dir, url, _path) = fresh_db_url();
        let conn_a = connect(&url).await;
        let conn_b = connect(&url).await;
        let runner_a = builtin_runner().expect("builtin runner A");
        let runner_b = builtin_runner().expect("builtin runner B");

        let barrier = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let barrier_a = barrier.clone();
        let barrier_b = barrier;
        let race_a = runner_a.run_pending_with_post_read_gate(&conn_a, || async move {
            barrier_a.wait().await;
        });
        let race_b = runner_b.run_pending_with_post_read_gate(&conn_b, || async move {
            barrier_b.wait().await;
        });
        let (applied_a, applied_b) = tokio::join!(race_a, race_b);
        let applied_a = applied_a.expect("racer A succeeds");
        let applied_b = applied_b.expect("racer B succeeds");

        // Each version is owned by exactly one racer.
        let mut all: Vec<i64> = applied_a.iter().chain(applied_b.iter()).copied().collect();
        all.sort_unstable();
        let mut deduped = all.clone();
        deduped.dedup();
        assert_eq!(all, deduped, "round {round}: no version applied twice");

        // The union covers every registered migration, and the rebuilt
        // table is in the re-keyed shape exactly once.
        let runner = builtin_runner().expect("builtin runner");
        assert_eq!(
            all.len(),
            runner.len(),
            "round {round}: union covers the full registry"
        );
        assert!(column_exists(&conn_a, "bisect_state", "worktree_id").await);
        assert!(!column_exists(&conn_a, "bisect_state", "id").await);
        assert!(column_exists(&conn_a, "rebase_state", "worktree_id").await);
    }
}

/// The 2026072301 down migration FAILS CLOSED while a linked worktree's
/// bisect row exists; after the linked session is reset, the rollback
/// succeeds and restores the lazy shape with the main row intact.
#[tokio::test]
async fn bisect_down_migration_rejects_linked_rows() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO bisect_state \
         (worktree_id, orig_head, good, completed, first_parent) \
         VALUES ('', 'aa11', '[\"bb22\"]', 0, 0), \
                ('wt1234', 'cc33', '[\"dd44\"]', 0, 1);"
            .to_string(),
    ))
    .await
    .expect("main + linked bisect rows");

    let runner = builtin_runner().expect("builtin runner");
    let err = runner
        .rollback_to(&conn, 2026072201)
        .await
        .expect_err("rollback with a linked bisect row must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the down-guard CHECK: {rendered}"
    );
    // The re-keyed table survives the refused rollback (txn rolled back).
    assert!(!column_exists(&conn, "bisect_state", "id").await);

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM bisect_state WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("reset the linked bisect");
    let rolled = runner
        .rollback_to(&conn, 2026072201)
        .await
        .expect("rollback succeeds once only the main row remains");
    assert_eq!(
        rolled,
        vec![
            2026072402, 2026072401, 2026072304, 2026072303, 2026072302, 2026072301
        ]
    );
    assert!(column_exists(&conn, "bisect_state", "id").await);
    assert!(column_exists(&conn, "bisect_state", "worktree_id").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT orig_head, worktree_id FROM bisect_state".to_string(),
        ))
        .await
        .expect("query restored row")
        .expect("main row survives the rollback");
    let orig_head: String = row.try_get_by_index(0).expect("orig_head");
    let worktree_id: String = row.try_get_by_index(1).expect("worktree_id");
    assert_eq!(orig_head, "aa11");
    assert_eq!(worktree_id, "");
}

/// 2026072302 clears legacy dirty-cache rows (rebuildable advisory state —
/// §C.4.1.1 "clear and rescan, never guess the owner") and re-keys the meta
/// singleton to worktree_id.
#[tokio::test]
async fn dirty_migration_clears_advisory_rows_and_rekeys_meta() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `working_dirty` (
            `id`          INTEGER PRIMARY KEY AUTOINCREMENT,
            `path`        TEXT NOT NULL,
            `kind`        TEXT NOT NULL DEFAULT 'unknown',
            `source`      TEXT NOT NULL,
            `marked_at`   TEXT NOT NULL,
            `verified_at` TEXT,
            UNIQUE(`path`, `kind`)
        );"#
        .to_string(),
    ))
    .await
    .expect("legacy rows table");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO working_dirty (path, kind, source, marked_at) \
         VALUES ('f.txt', 'modified', 'scan', '2026-07-01T00:00:00Z');"
            .to_string(),
    ))
    .await
    .expect("legacy row");

    run_builtin_migrations(&conn).await.expect("migrations");

    assert!(column_exists(&conn, "working_dirty", "worktree_id").await);
    assert!(column_exists(&conn, "working_dirty_meta", "worktree_id").await);
    assert!(!column_exists(&conn, "working_dirty_meta", "id").await);
    let rows = conn
        .query_all(Statement::from_string(
            backend,
            "SELECT path FROM working_dirty".to_string(),
        ))
        .await
        .expect("query");
    assert!(rows.is_empty(), "advisory rows are cleared, not adopted");
}

/// The 2026072302 down migration FAILS CLOSED while linked-scope dirty rows
/// or meta exist; after clearing them the rollback restores the legacy
/// single-row shapes with the main rows intact.
#[tokio::test]
async fn dirty_down_migration_rejects_linked_rows() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO working_dirty (worktree_id, path, kind, source, marked_at) \
         VALUES ('', 'main.txt', 'modified', 'scan', '2026-07-01T00:00:00Z'), \
                ('wt1', 'linked.txt', 'modified', 'scan', '2026-07-01T00:00:00Z');"
            .to_string(),
    ))
    .await
    .expect("main + linked rows");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO working_dirty_meta (worktree_id, state) VALUES ('', 'fresh');".to_string(),
    ))
    .await
    .expect("main meta");

    let runner = builtin_runner().expect("builtin runner");
    let err = runner
        .rollback_to(&conn, 2026072301)
        .await
        .expect_err("rollback with a linked dirty row must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the down-guard CHECK: {rendered}"
    );
    assert!(column_exists(&conn, "working_dirty", "worktree_id").await);

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM working_dirty WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("clear linked rows");

    // Second guard branch: a linked META row alone must also fail closed.
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO working_dirty_meta (worktree_id, state) VALUES ('wt1', 'fresh');".to_string(),
    ))
    .await
    .expect("linked meta");
    let err = runner
        .rollback_to(&conn, 2026072301)
        .await
        .expect_err("rollback with a linked META row must fail closed");
    assert!(
        format!("{err:?}").contains("CHECK")
            || format!("{err:?}").to_lowercase().contains("constraint"),
        "meta guard CHECK fires"
    );
    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM working_dirty_meta WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("clear linked meta");
    let rolled = runner
        .rollback_to(&conn, 2026072301)
        .await
        .expect("rollback succeeds once only main rows remain");
    assert_eq!(rolled, vec![2026072302]);
    assert!(!column_exists(&conn, "working_dirty", "worktree_id").await);
    assert!(column_exists(&conn, "working_dirty_meta", "id").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT path FROM working_dirty".to_string(),
        ))
        .await
        .expect("query")
        .expect("main row survives");
    let path: String = row.try_get_by_index(0).expect("path");
    assert_eq!(path, "main.txt");
}

/// 2026072303 re-keys `layer`/`layer_path` per worktree. Layer ownership is
/// NOT rebuildable (§C.4.1.1), so legacy global rows are ADOPTED to the main
/// scope ('') — permitted because the guard proved no linked worktree exists.
#[tokio::test]
async fn layer_migration_adopts_main_rows_without_linked_evidence() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    // Pre-create the legacy shapes with rows: 2026070501's CREATE IF NOT
    // EXISTS skips them, and 2026072303 must rebuild + adopt.
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `layer` (
            `id`         INTEGER PRIMARY KEY AUTOINCREMENT,
            `name`       TEXT NOT NULL UNIQUE,
            `source`     TEXT NOT NULL,
            `priority`   INTEGER NOT NULL DEFAULT 0,
            `enabled`    INTEGER NOT NULL DEFAULT 1,
            `created_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            `updated_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );"#
        .to_string(),
    ))
    .await
    .expect("legacy layer table");
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `layer_path` (
            `id`              INTEGER PRIMARY KEY AUTOINCREMENT,
            `layer_name`      TEXT NOT NULL,
            `path`            TEXT NOT NULL UNIQUE,
            `content_hash`    TEXT NOT NULL,
            `materialized_at` TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );"#
        .to_string(),
    ))
    .await
    .expect("legacy layer_path table");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO layer (name, source, priority, enabled) VALUES ('ov', '/src/ov', 3, 1);"
            .to_string(),
    ))
    .await
    .expect("legacy layer row");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO layer_path (layer_name, path, content_hash) \
         VALUES ('ov', 'dir/f.txt', 'hash1');"
            .to_string(),
    ))
    .await
    .expect("legacy path row");

    run_builtin_migrations(&conn).await.expect("migrations");

    assert!(column_exists(&conn, "layer", "worktree_id").await);
    assert!(column_exists(&conn, "layer_path", "worktree_id").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT worktree_id, name, source, priority FROM layer".to_string(),
        ))
        .await
        .expect("query")
        .expect("adopted layer row");
    let scope: String = row.try_get_by_index(0).expect("worktree_id");
    let name: String = row.try_get_by_index(1).expect("name");
    let source: String = row.try_get_by_index(2).expect("source");
    let priority: i64 = row.try_get_by_index(3).expect("priority");
    assert_eq!((scope.as_str(), name.as_str()), ("", "ov"));
    assert_eq!((source.as_str(), priority), ("/src/ov", 3));
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT worktree_id, path, content_hash FROM layer_path".to_string(),
        ))
        .await
        .expect("query")
        .expect("adopted path row");
    let scope: String = row.try_get_by_index(0).expect("worktree_id");
    let path: String = row.try_get_by_index(1).expect("path");
    assert_eq!((scope.as_str(), path.as_str()), ("", "dir/f.txt"));
}

/// 2026072303 FAILS CLOSED when legacy global layer rows coexist with linked
/// worktree evidence (a linked HEAD row): ownership must not be guessed —
/// the user unapplies/removes from the owning worktree first (§C.4.1.1).
#[tokio::test]
async fn layer_migration_fails_closed_with_linked_evidence() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    let runner = builtin_runner().expect("builtin runner");
    run_builtin_migrations(&conn).await.expect("migrations");
    // Re-open the legacy window: roll back ONLY 2026072303 (its empty tables
    // pass the down guard), then plant legacy rows + linked HEAD evidence.
    assert_eq!(
        runner
            .rollback_to(&conn, 2026072302)
            .await
            .expect("rollback layer scope"),
        vec![2026072403, 2026072402, 2026072401, 2026072304, 2026072303]
    );
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO layer (name, source) VALUES ('ov', '/src/ov');".to_string(),
    ))
    .await
    .expect("legacy layer row");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO reference (name, kind, `commit`, worktree_id) \
         VALUES (NULL, 'Head', 'aa11', 'wt1');"
            .to_string(),
    ))
    .await
    .expect("linked HEAD evidence");

    let err = runner
        .run_pending(&conn)
        .await
        .expect_err("legacy rows + linked evidence must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the adopt guard CHECK: {rendered}"
    );
    // Nothing was claimed or rebuilt: the legacy shape and row survive.
    assert!(!column_exists(&conn, "layer", "worktree_id").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT name FROM layer".to_string(),
        ))
        .await
        .expect("query")
        .expect("legacy row untouched");
    let name: String = row.try_get_by_index(0).expect("name");
    assert_eq!(name, "ov");

    // After the user clears the ambiguity (here: the linked evidence goes
    // away), the migration applies and adopts to main.
    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM reference WHERE worktree_id = 'wt1';".to_string(),
    ))
    .await
    .expect("clear linked evidence");
    assert_eq!(
        runner.run_pending(&conn).await.expect("retry succeeds"),
        vec![2026072303, 2026072304, 2026072401, 2026072402, 2026072403]
    );
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT worktree_id FROM layer WHERE name = 'ov'".to_string(),
        ))
        .await
        .expect("query")
        .expect("adopted row");
    let scope: String = row.try_get_by_index(0).expect("worktree_id");
    assert_eq!(scope, "");
}

/// The 2026072303 down migration FAILS CLOSED while linked-scope layer rows
/// exist (dropping them would make overlay files committable); after the
/// linked scopes are explicitly cleared it restores the legacy shapes with
/// the main rows intact.
#[tokio::test]
async fn layer_down_migration_rejects_linked_rows() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO layer (worktree_id, name, source) \
         VALUES ('', 'ov', '/src/main'), ('wt1', 'ov', '/src/linked');"
            .to_string(),
    ))
    .await
    .expect("main + linked rows");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO layer_path (worktree_id, layer_name, path, content_hash) \
         VALUES ('', 'ov', 'main.txt', 'h1');"
            .to_string(),
    ))
    .await
    .expect("main path row");

    let runner = builtin_runner().expect("builtin runner");
    let err = runner
        .rollback_to(&conn, 2026072302)
        .await
        .expect_err("rollback with a linked layer row must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the down-guard CHECK: {rendered}"
    );
    assert!(column_exists(&conn, "layer", "worktree_id").await);

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM layer WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("clear linked layer row");

    // Second guard branch: a linked PATH row alone must also fail closed.
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO layer_path (worktree_id, layer_name, path, content_hash) \
         VALUES ('wt1', 'ov', 'linked.txt', 'h2');"
            .to_string(),
    ))
    .await
    .expect("linked path row");
    let err = runner
        .rollback_to(&conn, 2026072302)
        .await
        .expect_err("rollback with a linked layer_path row must fail closed");
    assert!(
        format!("{err:?}").contains("CHECK")
            || format!("{err:?}").to_lowercase().contains("constraint"),
        "path guard CHECK fires"
    );
    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM layer_path WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("clear linked path row");

    let rolled = runner
        .rollback_to(&conn, 2026072302)
        .await
        .expect("rollback succeeds once only main rows remain");
    assert_eq!(rolled, vec![2026072303]);
    assert!(!column_exists(&conn, "layer", "worktree_id").await);
    assert!(!column_exists(&conn, "layer_path", "worktree_id").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT name, source FROM layer".to_string(),
        ))
        .await
        .expect("query")
        .expect("main row survives");
    let name: String = row.try_get_by_index(0).expect("name");
    let source: String = row.try_get_by_index(1).expect("source");
    assert_eq!((name.as_str(), source.as_str()), ("ov", "/src/main"));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

async fn table_exists(conn: &DatabaseConnection, name: &str) -> bool {
    let backend = conn.get_database_backend();
    conn.query_one(Statement::from_sql_and_values(
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
    conn.query_one(Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'index' AND name = ? LIMIT 1",
        [name.into()],
    ))
    .await
    .expect("query")
    .is_some()
}

async fn trigger_exists(conn: &DatabaseConnection, name: &str) -> bool {
    conn.query_one(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT 1 AS one FROM sqlite_master WHERE type = 'trigger' AND name = ?",
        [name.into()],
    ))
    .await
    .expect("query sqlite_master trigger")
    .is_some()
}

async fn column_exists(conn: &DatabaseConnection, table: &str, column: &str) -> bool {
    let backend = conn.get_database_backend();
    let escaped_table = table.replace('`', "``");
    let rows = conn
        .query_all(Statement::from_string(
            backend,
            format!("PRAGMA table_info(`{escaped_table}`)"),
        ))
        .await
        .expect("table_info");
    rows.iter().any(|row| {
        let name: String = row.try_get_by_index(1).expect("column name");
        name == column
    })
}

/// 2026072304 re-keys `sparse_view` per worktree and projects the legacy
/// `sparse.enabled` `config_kv` key into `sparse_view_meta` — legacy
/// patterns/toggle adopt to the main scope ('') when no linked worktree
/// exists, and the retired config key is removed.
#[tokio::test]
async fn sparse_migration_adopts_main_state_without_linked_evidence() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    // Pre-create the legacy shape with rows + the legacy config toggle:
    // 2026070701's CREATE IF NOT EXISTS skips the table, and 2026072304
    // must rebuild + adopt + project.
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `sparse_view` (
            `id`      INTEGER PRIMARY KEY AUTOINCREMENT,
            `pattern` TEXT NOT NULL,
            `ordinal` INTEGER NOT NULL
        );"#
        .to_string(),
    ))
    .await
    .expect("legacy sparse table");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO sparse_view (pattern, ordinal) VALUES ('src/**', 0), ('!src/gen/**', 1);"
            .to_string(),
    ))
    .await
    .expect("legacy patterns");
    conn.execute(Statement::from_string(
        backend,
        r#"CREATE TABLE `config_kv` (
            `id` INTEGER PRIMARY KEY AUTOINCREMENT,
            `key` TEXT NOT NULL,
            `value` TEXT NOT NULL,
            `encrypted` INTEGER NOT NULL DEFAULT 0
        );"#
        .to_string(),
    ))
    .await
    .expect("config_kv table");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO config_kv (key, value) VALUES ('sparse.enabled', 'true');".to_string(),
    ))
    .await
    .expect("legacy toggle");

    run_builtin_migrations(&conn).await.expect("migrations");

    assert!(column_exists(&conn, "sparse_view", "worktree_id").await);
    assert!(table_exists(&conn, "sparse_view_meta").await);
    let rows = conn
        .query_all(Statement::from_string(
            backend,
            "SELECT worktree_id, pattern FROM sparse_view ORDER BY ordinal".to_string(),
        ))
        .await
        .expect("query");
    let adopted: Vec<(String, String)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get_by_index(0).expect("scope"),
                row.try_get_by_index(1).expect("pattern"),
            )
        })
        .collect();
    assert_eq!(
        adopted,
        vec![
            ("".to_string(), "src/**".to_string()),
            ("".to_string(), "!src/gen/**".to_string())
        ]
    );
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT enabled FROM sparse_view_meta WHERE worktree_id = ''".to_string(),
        ))
        .await
        .expect("query")
        .expect("projected toggle");
    let enabled: i32 = row.try_get_by_index(0).expect("enabled");
    assert_eq!(enabled, 1, "truthy legacy toggle projected as enabled");
    let leftover = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT COUNT(*) FROM config_kv WHERE key = 'sparse.enabled'".to_string(),
        ))
        .await
        .expect("query")
        .expect("row");
    let count: i64 = leftover.try_get_by_index(0).expect("count");
    assert_eq!(count, 0, "the legacy config key is retired");
}

/// 2026072304 follows `ConfigKv::get` last-wins semantics for the legacy
/// toggle: with duplicate `sparse.enabled` rows, only the HIGHEST-id value
/// counts — a stale earlier `true` under a later `false` projects as
/// DISABLED and does not count as legacy-enabled state for the guard, so
/// the migration proceeds even alongside linked worktree evidence.
#[tokio::test]
async fn sparse_migration_projects_last_wins_toggle() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    let runner = builtin_runner().expect("builtin runner");
    run_builtin_migrations(&conn).await.expect("migrations");
    assert_eq!(
        runner
            .rollback_to(&conn, 2026072303)
            .await
            .expect("rollback sparse scope"),
        vec![2026072403, 2026072402, 2026072401, 2026072304]
    );
    // Duplicate legacy rows: stale `true` (lower id) then effective `false`
    // (higher id) — `ConfigKv::get` reads the LAST one. Plus linked HEAD
    // evidence: since the effective toggle is falsy and there are no
    // patterns, there is NO legacy-enabled state and the guard must pass.
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO config_kv (key, value) VALUES ('sparse.enabled', 'true');".to_string(),
    ))
    .await
    .expect("stale truthy row");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO config_kv (key, value) VALUES ('sparse.enabled', 'false');".to_string(),
    ))
    .await
    .expect("effective falsy row");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO reference (name, kind, `commit`, worktree_id) \
         VALUES (NULL, 'Head', 'aa11', 'wt1');"
            .to_string(),
    ))
    .await
    .expect("linked HEAD evidence");

    assert_eq!(
        runner
            .run_pending(&conn)
            .await
            .expect("falsy effective toggle does not trip the guard"),
        vec![2026072304, 2026072401, 2026072402, 2026072403]
    );
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT enabled FROM sparse_view_meta WHERE worktree_id = ''".to_string(),
        ))
        .await
        .expect("query")
        .expect("projected toggle");
    let enabled: i32 = row.try_get_by_index(0).expect("enabled");
    assert_eq!(enabled, 0, "last-wins falsy value projects as disabled");
}

/// 2026072304 FAILS CLOSED when legacy sparse state (patterns OR a truthy
/// toggle) coexists with linked worktree evidence — ownership must not be
/// guessed, and patterns are never copied to every worktree (§C.4.1.1).
#[tokio::test]
async fn sparse_migration_fails_closed_with_linked_evidence() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    let runner = builtin_runner().expect("builtin runner");
    run_builtin_migrations(&conn).await.expect("migrations");
    // Re-open the legacy window: roll back ONLY 2026072304 (its empty
    // tables pass the down guard), then plant a truthy legacy toggle plus
    // linked HEAD evidence — the toggle ALONE (no patterns) must trip the
    // guard too.
    assert_eq!(
        runner
            .rollback_to(&conn, 2026072303)
            .await
            .expect("rollback sparse scope"),
        vec![2026072403, 2026072402, 2026072401, 2026072304]
    );
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO config_kv (key, value) VALUES ('sparse.enabled', 'true');".to_string(),
    ))
    .await
    .expect("legacy toggle");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO reference (name, kind, `commit`, worktree_id) \
         VALUES (NULL, 'Head', 'aa11', 'wt1');"
            .to_string(),
    ))
    .await
    .expect("linked HEAD evidence");

    let err = runner
        .run_pending(&conn)
        .await
        .expect_err("legacy toggle + linked evidence must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the adopt guard CHECK: {rendered}"
    );
    assert!(!column_exists(&conn, "sparse_view", "worktree_id").await);

    // Clearing the ambiguity (dropping the legacy toggle) lets the retry
    // apply; no meta row is projected for a state-less repo... except the
    // config row existed, so the main row records disabled-after-clear.
    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM config_kv WHERE key = 'sparse.enabled';".to_string(),
    ))
    .await
    .expect("clear legacy toggle");
    assert_eq!(
        runner.run_pending(&conn).await.expect("retry succeeds"),
        vec![2026072304, 2026072401, 2026072402, 2026072403]
    );
    assert!(column_exists(&conn, "sparse_view", "worktree_id").await);
}

/// The 2026072304 down migration FAILS CLOSED while linked-scope rows exist
/// (patterns or a meta row); after clearing them it restores the legacy
/// shape and re-projects the main toggle back into `config_kv`.
#[tokio::test]
async fn sparse_down_migration_rejects_linked_rows() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    run_builtin_migrations(&conn).await.expect("migrations");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO sparse_view (worktree_id, pattern, ordinal) \
         VALUES ('', 'main/**', 0), ('wt1', 'linked/**', 0);"
            .to_string(),
    ))
    .await
    .expect("main + linked patterns");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO sparse_view_meta (worktree_id, enabled) VALUES ('', 1);".to_string(),
    ))
    .await
    .expect("main toggle");

    let runner = builtin_runner().expect("builtin runner");
    let err = runner
        .rollback_to(&conn, 2026072303)
        .await
        .expect_err("rollback with a linked pattern row must fail closed");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "failure comes from the down-guard CHECK: {rendered}"
    );
    assert!(column_exists(&conn, "sparse_view", "worktree_id").await);

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM sparse_view WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("clear linked pattern");

    // Second guard branch: a linked META row alone must also fail closed.
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO sparse_view_meta (worktree_id, enabled) VALUES ('wt1', 0);".to_string(),
    ))
    .await
    .expect("linked meta row");
    let err = runner
        .rollback_to(&conn, 2026072303)
        .await
        .expect_err("rollback with a linked meta row must fail closed");
    assert!(
        format!("{err:?}").contains("CHECK")
            || format!("{err:?}").to_lowercase().contains("constraint"),
        "meta guard CHECK fires"
    );
    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM sparse_view_meta WHERE worktree_id <> '';".to_string(),
    ))
    .await
    .expect("clear linked meta");

    let rolled = runner
        .rollback_to(&conn, 2026072303)
        .await
        .expect("rollback succeeds once only main rows remain");
    assert_eq!(rolled, vec![2026072304]);
    assert!(!column_exists(&conn, "sparse_view", "worktree_id").await);
    assert!(!table_exists(&conn, "sparse_view_meta").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT value FROM config_kv WHERE key = 'sparse.enabled'".to_string(),
        ))
        .await
        .expect("query")
        .expect("re-projected toggle");
    let value: String = row.try_get_by_index(0).expect("value");
    assert_eq!(value, "true", "main toggle re-projected into config_kv");
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT pattern FROM sparse_view".to_string(),
        ))
        .await
        .expect("query")
        .expect("main pattern survives");
    let pattern: String = row.try_get_by_index(0).expect("pattern");
    assert_eq!(pattern, "main/**");
}

/// W2 §C.4.3 inventory guard: every OID-shaped column in the LIVE schema
/// must be accounted for in `GC_OBJECT_SOURCE_INVENTORY` — either as a
/// traced reachability root or as a documented non-root. A new store that
/// adds an OID column without updating the inventory (and, when a root, the
/// collector) fails here instead of silently shipping un-traced.
#[tokio::test]
async fn gc_object_source_inventory_covers_every_oid_column() {
    use libra::command::maintenance::GC_OBJECT_SOURCE_INVENTORY;

    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    run_builtin_migrations(&conn).await.expect("migrations");

    let oid_keywords = ["oid", "commit", "blob", "tree", "sha", "hash"];
    // Table-level exemptions: stores whose OID-shaped columns are not
    // object-store OIDs at all (AI capture bookkeeping uses provider ids and
    // sync hashes; `agent_checkpoint` itself IS inventoried).
    let exempt_tables: [&str; 0] = [];

    let tables = conn
        .query_all(Statement::from_string(
            backend,
            "SELECT name FROM sqlite_master WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%'"
                .to_string(),
        ))
        .await
        .expect("list tables");
    let mut uncovered: Vec<String> = Vec::new();
    for table_row in tables {
        let table: String = table_row.try_get_by_index(0).expect("table name");
        if exempt_tables.contains(&table.as_str()) {
            continue;
        }
        let columns = conn
            .query_all(Statement::from_string(
                backend,
                format!("PRAGMA table_info({table})"),
            ))
            .await
            .expect("table info");
        for column_row in columns {
            let column: String = column_row.try_get_by_index(1).expect("column name");
            let is_oid_shaped = column
                .split('_')
                .any(|segment| oid_keywords.contains(&segment));
            if !is_oid_shaped {
                continue;
            }
            let inventoried = GC_OBJECT_SOURCE_INVENTORY
                .iter()
                .any(|(t, c, _, _)| *t == table && *c == column);
            if !inventoried {
                uncovered.push(format!("{table}.{column}"));
            }
        }
    }
    // Semantic OID columns the name heuristic cannot flag — pinned by hand;
    // each must be inventoried too.
    for (table, column) in [
        ("operation_view", "head_target"),
        ("operation_view_workspace", "pointer_value"),
        ("object_index", "o_id"),
        ("metadata_kv", "value"),
    ] {
        let inventoried = GC_OBJECT_SOURCE_INVENTORY
            .iter()
            .any(|(t, c, _, _)| *t == table && *c == column);
        if !inventoried {
            uncovered.push(format!("{table}.{column} (semantic)"));
        }
    }
    assert!(
        uncovered.is_empty(),
        "OID-shaped columns missing from GC_OBJECT_SOURCE_INVENTORY (add each as a traced \
         root in collect_reachable_objects or a documented non-root): {uncovered:?}"
    );
}

/// 2026072401 (plan-20260714 §C.7, registry v2): the capability marker makes
/// OLD binaries refuse the repository at connect time (future-schema
/// fail-closed) before they can parse or recreate `worktrees.json`. Its down
/// drops only the marker — the v2 JSON layout itself still fails a v1 parser
/// closed — and re-applying is idempotent.
#[tokio::test]
async fn worktree_registry_v2_capability_marker_round_trip() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    let runner = builtin_runner().expect("builtin runner");
    run_builtin_migrations(&conn).await.expect("migrations");

    assert!(table_exists(&conn, "worktree_registry_capability").await);
    let row = conn
        .query_one(Statement::from_string(
            backend,
            "SELECT version FROM worktree_registry_capability".to_string(),
        ))
        .await
        .expect("query")
        .expect("capability row");
    let version: i32 = row.try_get_by_index(0).expect("version");
    assert_eq!(version, 2, "marker records registry schema v2");

    // Down drops only the marker.
    assert_eq!(
        runner
            .rollback_to(&conn, 2026072304)
            .await
            .expect("rollback capability marker"),
        vec![2026072403, 2026072402, 2026072401]
    );
    assert!(!table_exists(&conn, "worktree_registry_capability").await);

    // Re-apply, then a second full pass is a no-op (idempotent DDL).
    assert_eq!(
        runner.run_pending(&conn).await.expect("re-apply"),
        vec![2026072401, 2026072402, 2026072403]
    );
    assert!(table_exists(&conn, "worktree_registry_capability").await);
    assert_eq!(
        runner.run_pending(&conn).await.expect("idempotent"),
        Vec::<i64>::new()
    );
}

/// 2026072402 (§C.7 W3-s1b): the down migration REFUSES while any lifecycle
/// (detached_from_registry / tombstone) or in-flight journal row exists —
/// v2 lifecycle must never be folded into a v1 active entry or dropped.
/// After the pending state is resolved, the rollback proceeds and re-apply
/// is idempotent.
#[tokio::test]
async fn registry_v2_down_migration_rejects_nonterminal_state() {
    let (_dir, url, _path) = fresh_db_url();
    let conn = connect(&url).await;
    let backend = conn.get_database_backend();
    let runner = builtin_runner().expect("builtin runner");
    run_builtin_migrations(&conn).await.expect("migrations");

    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO worktree_lifecycle (worktree_id, state, path, created_at, updated_at) \
         VALUES ('wt1', 'detached_from_registry', '/wt1', 0, 0);"
            .to_string(),
    ))
    .await
    .expect("lifecycle row");
    let err = runner
        .rollback_to(&conn, 2026072401)
        .await
        .expect_err("down must refuse while a detached row exists");
    let rendered = format!("{err:?}");
    assert!(
        rendered.contains("CHECK") || rendered.to_lowercase().contains("constraint"),
        "refusal comes from the down-guard CHECK: {rendered}"
    );
    assert!(table_exists(&conn, "worktree_lifecycle").await);

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM worktree_lifecycle;".to_string(),
    ))
    .await
    .expect("clear lifecycle");

    // A pending journal row alone must refuse too.
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO worktree_intent_journal (op, worktree_id, payload, created_at) \
         VALUES ('remove', 'wt1', '{}', 0);"
            .to_string(),
    ))
    .await
    .expect("journal row");
    let err = runner
        .rollback_to(&conn, 2026072401)
        .await
        .expect_err("down must refuse while a journal row exists");
    assert!(
        format!("{err:?}").contains("CHECK")
            || format!("{err:?}").to_lowercase().contains("constraint"),
        "journal guard CHECK fires"
    );

    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM worktree_intent_journal;".to_string(),
    ))
    .await
    .expect("clear journal");

    // Linked-scope sequencer state alone must refuse too (§C.7 line 1261);
    // main-scope rows (empty worktree_id) do not block.
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO rebase_state (worktree_id, head_name, onto, orig_head, current_head, \
         todo, done) VALUES ('wt1', 'refs/heads/f', 'aa', 'bb', 'cc', '', '');"
            .to_string(),
    ))
    .await
    .expect("linked rebase row");
    conn.execute(Statement::from_string(
        backend,
        "INSERT INTO sequence_state (worktree_id, kind, head_name, head_orig, current_oid, \
         todo) VALUES ('', 'CherryPick', 'refs/heads/m', 'dd', 'ee', '');"
            .to_string(),
    ))
    .await
    .expect("main sequencer row (must not block)");
    let err = runner
        .rollback_to(&conn, 2026072401)
        .await
        .expect_err("down must refuse while linked sequencer state exists");
    assert!(
        format!("{err:?}").contains("CHECK")
            || format!("{err:?}").to_lowercase().contains("constraint"),
        "sequencer guard CHECK fires"
    );
    conn.execute(Statement::from_string(
        backend,
        "DELETE FROM rebase_state WHERE worktree_id = 'wt1';".to_string(),
    ))
    .await
    .expect("clear linked rebase row");

    assert_eq!(
        runner
            .rollback_to(&conn, 2026072401)
            .await
            .expect("rollback proceeds once terminal"),
        vec![2026072402]
    );
    assert!(!table_exists(&conn, "worktree_lifecycle").await);
    assert!(!table_exists(&conn, "worktree_intent_journal").await);
    assert_eq!(
        runner.run_pending(&conn).await.expect("re-apply"),
        vec![2026072402, 2026072403]
    );
}
