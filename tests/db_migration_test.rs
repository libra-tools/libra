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
            2026070802, 2026070803, 2026071301, 2026071401
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
        ]
    );

    let runner = builtin_runner().expect("builtin registry must build clean");
    assert!(!runner.is_empty());
    assert_eq!(runner.len(), 25);
    assert_eq!(runner.max_registered_version(), Some(2026071401));
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
            2026070802, 2026070803, 2026071301, 2026071401
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
    assert!(index_exists(&conn, "idx_agent_coverage_claim_logical_key").await);
    assert!(index_exists(&conn, "idx_agent_coverage_claim_session_state").await);
    assert!(index_exists(&conn, "idx_agent_coverage_claim_checkpoint_id").await);
    assert!(index_exists(&conn, "idx_agent_coverage_revision_checkpoint_id").await);
    // plan-20260713 DR-04b: OpenCode export-bridge job state.
    assert!(table_exists(&conn, "agent_export_job").await);
    assert!(index_exists(&conn, "idx_agent_export_job_session").await);
    assert!(index_exists(&conn, "idx_agent_export_job_ttl").await);
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
            2026071401, 2026071301, 2026070803, 2026070802, 2026070801, 2026070701, 2026070601,
            2026070501, 2026070401, 2026070301, 2026070202, 2026070201, 2026062301, 2026061401,
            2026060801, 2026060401, 2026060201, 2026053101, 2026052301, 2026050801, 2026050601
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
            2026070601, 2026070701, 2026070801, 2026070802, 2026070803, 2026071301, 2026071401
        ]
    );
    assert!(table_exists(&conn, "approved_permission").await);
    assert!(index_exists(&conn, "idx_approved_permission_project").await);
    assert!(column_exists(&conn, "agent_usage_stats", "agent_name").await);
    assert!(index_exists(&conn, "idx_agent_usage_stats_agent_name_provider_model").await);
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
