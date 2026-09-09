//! Convergence of the independently shipped operation-v2 and #472 branches.

use std::sync::Arc;

use libra::internal::db;
use sea_orm::ConnectionTrait;
use tokio::sync::Barrier;

use super::{
    MigrationRunner, all_builtin_runner, builtin_migrations, column_exists, connect, table_exists,
};

#[path = "branch_convergence/fixtures.rs"]
mod fixtures;
use fixtures::{
    CHANGE_AI_LINK, CONFIG_REPAIR, CONVERGENCE, OPERATION_V2, branch_database, operation_rows,
    receipts, rows, snapshot,
};

#[test]
fn combined_registry_keeps_both_original_migrations_and_adds_a_forward_barrier() {
    let migrations = builtin_migrations();
    assert_eq!(migrations.len(), 61);
    let tail: Vec<_> = migrations
        .iter()
        .filter(|migration| migration.version >= OPERATION_V2)
        .map(|migration| (migration.version, migration.name))
        .collect();
    assert_eq!(
        tail,
        vec![
            (OPERATION_V2, "operation_v2"),
            (CONFIG_REPAIR, "legacy_config_table"),
            (CONVERGENCE, "operation_v2_branch_convergence"),
            (CHANGE_AI_LINK, "change_ai_link"),
        ]
    );
    assert!(migrations.last().unwrap().down.is_none());
}

#[tokio::test]
async fn config_branch_ordinary_open_catches_up_operations_without_rewriting_receipts() {
    // Given the actual #472 branch shape: 58 receipts, max0601, populated v1.
    let (_dir, path, conn) = branch_database(CONFIG_REPAIR).await;
    let before = receipts(&conn).await;
    assert_eq!(before.len(), 58);
    assert_eq!(before.last().unwrap().0, CONFIG_REPAIR);
    assert!(!before.iter().any(|row| row.0 == OPERATION_V2));
    assert!(column_exists(&conn, "operation", "view_id").await);
    let legacy = operation_rows(&conn, "").await;
    let config = rows(&conn, "config").await;
    let modern = rows(&conn, "config_kv").await;
    conn.close().await.unwrap();

    // When an ordinary production open sees the higher compatibility barrier.
    let conn = db::establish_connection(path.to_str().unwrap())
        .await
        .unwrap();

    // Then old rows survive in their legacy namespace and v2 starts empty.
    assert!(column_exists(&conn, "operation", "format_version").await);
    assert!(table_exists(&conn, "operation_head").await);
    assert_eq!(operation_rows(&conn, "legacy_").await, legacy);
    assert!(rows(&conn, "operation").await.is_empty());
    assert_eq!(rows(&conn, "config").await, config);
    assert_eq!(rows(&conn, "config_kv").await, modern);
    let after = receipts(&conn).await;
    assert_eq!(after.len(), 61);
    for (version, name) in [
        (OPERATION_V2, "operation_v2"),
        (CONVERGENCE, "operation_v2_branch_convergence"),
        (CHANGE_AI_LINK, "change_ai_link"),
    ] {
        assert_eq!(after.iter().find(|row| row.0 == version).unwrap().1, name);
    }
    for receipt in &before {
        assert!(
            after.contains(receipt),
            "changed original receipt {receipt:?}"
        );
    }
    assert_eq!(after.last().unwrap().0, CHANGE_AI_LINK);
    let unchanged = snapshot(&conn).await;
    conn.close().await.unwrap();
    let reopened = db::establish_connection(path.to_str().unwrap())
        .await
        .unwrap();
    assert!(
        db::upgrade_database_schema(&path)
            .await
            .unwrap()
            .applied_versions
            .is_empty()
    );
    assert_eq!(snapshot(&reopened).await, unchanged);
}

#[tokio::test]
async fn operation_v2_branch_keeps_modern_and_legacy_rows_without_recopying() {
    // Given the remote branch already copied legacy rows and recorded 0101.
    let (_dir, path, conn) = branch_database(OPERATION_V2).await;
    conn.execute_unprepared(
        "INSERT INTO operation (op_id,repo_id,kind,status,scope_kind,pre_view_oid,post_view_oid,start_ts) \
         VALUES ('v2-op','repo','command','succeeded','main','pre','post',20); \
         INSERT INTO operation_head VALUES ('repo','main','v2-op',7); \
         INSERT INTO operation_journal (journal_id,op_id,phase,owner,updated_at) \
         VALUES ('journal','v2-op','completed','fixture',21); \
         CREATE TRIGGER forbid_legacy_recopy BEFORE INSERT ON legacy_operation \
         BEGIN SELECT RAISE(ABORT,'legacy rows must not be recopied'); END;"
    ).await.unwrap();
    let before = receipts(&conn).await;
    let legacy = operation_rows(&conn, "legacy_").await;
    let modern = rows(&conn, "operation").await;
    let heads = rows(&conn, "operation_head").await;
    let journals = rows(&conn, "operation_journal").await;

    // When config repair and the convergence barrier are applied.
    let report = db::upgrade_database_schema(&path).await.unwrap();

    // Then both histories and the original 0101 claim survive unchanged.
    assert_eq!(
        report.applied_versions,
        vec![CONFIG_REPAIR, CONVERGENCE, CHANGE_AI_LINK]
    );
    assert_eq!(operation_rows(&conn, "legacy_").await, legacy);
    assert_eq!(rows(&conn, "operation").await, modern);
    assert_eq!(rows(&conn, "operation_head").await, heads);
    assert_eq!(rows(&conn, "operation_journal").await, journals);
    let after = receipts(&conn).await;
    for receipt in before {
        assert!(after.contains(&receipt));
    }
}

#[tokio::test]
async fn catch_up_failure_rolls_back_schema_data_and_both_new_receipts() {
    // Given invalid v1 copy keys on the already repaired #472 branch.
    let (_dir, path, conn) = branch_database(CONFIG_REPAIR).await;
    conn.execute_unprepared("UPDATE operation SET repo_id = ''")
        .await
        .unwrap();
    let before = snapshot(&conn).await;

    // When ordinary open attempts the catch-up transaction.
    let error = db::establish_connection(path.to_str().unwrap())
        .await
        .expect_err("invalid v1 copy must refuse ordinary open");

    // Then neither claimed receipt nor staging/copy/drop work leaks out.
    assert!(error.to_string().contains("migration"), "{error}");
    assert_eq!(snapshot(&conn).await, before);
    assert!(
        !receipts(&conn)
            .await
            .iter()
            .any(|row| [OPERATION_V2, CONVERGENCE].contains(&row.0))
    );
}

#[tokio::test]
async fn convergence_barrier_refuses_rollback_below_the_old_binary_tip_atomically() {
    // Given a successfully converged repository with preserved legacy rows.
    let (_dir, path, conn) = branch_database(CONFIG_REPAIR).await;
    db::upgrade_database_schema(&path).await.unwrap();
    let before = snapshot(&conn).await;

    // When a downgrade would advertise compatibility with an old #472 binary.
    let mut runner = MigrationRunner::new();
    runner
        .extend(
            builtin_migrations()
                .into_iter()
                .filter(|migration| migration.version <= CONVERGENCE),
        )
        .unwrap();
    let error = runner
        .rollback_to(&conn, CONFIG_REPAIR)
        .await
        .unwrap_err();

    // Then the forward-only fence refuses before any schema/data/receipt change.
    assert!(matches!(
        error,
        super::MigrationError::IrreversibleMigration {
            version: CONVERGENCE,
            ..
        }
    ));
    assert_eq!(snapshot(&conn).await, before);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn concurrent_config_branch_upgraders_claim_the_copy_and_barrier_once() {
    // Given two callers that both observed max0601 before either claims 0801.
    let (_dir, path, left) = branch_database(CONFIG_REPAIR).await;
    let url = format!("sqlite://{}", path.display());
    let right = connect(&url).await;
    let expected = operation_rows(&left, "").await;
    let rendezvous = Arc::new(Barrier::new(2));
    let other = Arc::clone(&rendezvous);
    let first = all_builtin_runner().unwrap();
    let second = all_builtin_runner().unwrap();

    // When both race through the runner's existing post-read synchronization seam.
    let (a, b) = tokio::join!(
        first.run_pending_with_post_read_gate(&left, || async {
            rendezvous.wait().await;
        }),
        second.run_pending_with_post_read_gate(&right, || async {
            other.wait().await;
        }),
    );

    // Then one barrier owner performs the catch-up; the loser does no copy DDL.
    let mut applied = a.unwrap();
    applied.extend(b.unwrap());
    assert_eq!(applied, vec![CONVERGENCE, CHANGE_AI_LINK]);
    assert_eq!(operation_rows(&left, "legacy_").await, expected);
    assert_eq!(receipts(&left).await.len(), 61);
}
