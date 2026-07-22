//! Wrapper-layer tests for transaction orchestration, rollback, dedup, and parent resolution.

use std::collections::HashSet;

use libra::internal::{
    operation::{OperationRecord, OperationService, OperationStatus},
    operation_wrapper::{
        OperationError, OperationMeta, OperationParentPolicy, OperationScope, ParentSelectionMode,
        resolve_parent_selection_with_conn, with_operation_log_with_conn,
    },
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, Statement};
use uuid::Uuid;

/// Build valid operation metadata with a unique digest for dedup-sensitive tests.
fn valid_meta() -> OperationMeta {
    valid_meta_with_digest(&format!("sha256:{}", Uuid::now_v7()))
}

/// Build valid operation metadata with a caller-provided digest.
fn valid_meta_with_digest(digest: &str) -> OperationMeta {
    OperationMeta {
        command_name: "commit".to_string(),
        description: "record snapshot".to_string(),
        actor: "alice".to_string(),
        repo_id: "repo_1".to_string(),
        args_digest: Some(digest.to_string()),
    }
}

/// Build a deterministic seed operation record for parent-resolution tests.
fn sample_record(op_id: &str, status: OperationStatus, end_ts: i64) -> OperationRecord {
    OperationRecord {
        op_id: op_id.to_string(),
        repo_id: "repo_1".to_string(),
        view_id: format!("view_{op_id}"),
        command_name: "commit".to_string(),
        description: format!("desc_{op_id}"),
        actor: "alice".to_string(),
        args_digest: Some("sha256:abcd".to_string()),
        start_ts: end_ts - 5,
        end_ts: Some(end_ts),
        status,
        worktree_id: String::new(),
    }
}

/// Create the full operation-layer schema required by wrapper tests.
async fn create_operation_schema(db: &DatabaseConnection) {
    let ddl = [
        "CREATE TABLE operation(op_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,view_id TEXT NOT NULL,command_name TEXT NOT NULL,description TEXT NOT NULL,actor TEXT NOT NULL,args_digest TEXT,start_ts INTEGER NOT NULL,end_ts INTEGER,status TEXT NOT NULL,worktree_id TEXT NOT NULL DEFAULT '');",
        "CREATE TABLE operation_parent(op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id));",
        "CREATE TABLE operation_view(view_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,head_kind TEXT NOT NULL,head_target TEXT NOT NULL,created_at INTEGER NOT NULL);",
        "CREATE TABLE operation_view_ref(view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote));",
        "CREATE TABLE operation_view_workspace(view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind));",
    ];
    for sql in ddl {
        db.execute(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .unwrap();
    }
}

/// Create a schema that is missing `operation_view` so persist failure paths can be exercised.
async fn create_operation_schema_missing_view(db: &DatabaseConnection) {
    let ddl = [
        "CREATE TABLE operation(op_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,view_id TEXT NOT NULL,command_name TEXT NOT NULL,description TEXT NOT NULL,actor TEXT NOT NULL,args_digest TEXT,start_ts INTEGER NOT NULL,end_ts INTEGER,status TEXT NOT NULL,worktree_id TEXT NOT NULL DEFAULT '');",
        "CREATE TABLE operation_parent(op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id));",
        "CREATE TABLE operation_view_ref(view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote));",
        "CREATE TABLE operation_view_workspace(view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind));",
    ];
    for sql in ddl {
        db.execute(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .unwrap();
    }
}

/// Create the reference table with both HEAD and main branch rows.
async fn create_reference_table_with_head(db: &DatabaseConnection) {
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT,kind TEXT NOT NULL,\"commit\" TEXT,remote TEXT,worktree_id TEXT)".to_string(),
    ))
    .await
    .unwrap();
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Head', NULL, NULL)"
            .to_string(),
    ))
    .await
    .unwrap();
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)".to_string(),
    ))
    .await
    .unwrap();
}

/// Create the reference table without a HEAD row to force snapshot failure.
async fn create_reference_table_without_head(db: &DatabaseConnection) {
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT,kind TEXT NOT NULL,\"commit\" TEXT,remote TEXT,worktree_id TEXT)".to_string(),
    ))
    .await
    .unwrap();
}

/// Create a probe table used to assert rollback behavior.
async fn create_tx_probe_table(db: &DatabaseConnection) {
    db.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE tx_probe (id INTEGER PRIMARY KEY)".to_string(),
    ))
    .await
    .unwrap();
}

#[tokio::test]
/// Verifies that parent resolution returns the expected mode and scan counters.
async fn resolve_parent_selection_returns_mode_and_scan_stats() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_old_success", OperationStatus::Succeeded, 10),
    )
    .await
    .unwrap();
    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_new_failed", OperationStatus::Failed, 30),
    )
    .await
    .unwrap();
    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_latest_success", OperationStatus::Succeeded, 40),
    )
    .await
    .unwrap();

    let result =
        resolve_parent_selection_with_conn(&db, "repo_1", ParentSelectionMode::SingleLatestSuccess)
            .await
            .unwrap();

    assert_eq!(result.mode, ParentSelectionMode::SingleLatestSuccess);
    assert_eq!(result.selected, vec!["op_latest_success".to_string()]);
    assert_eq!(result.scanned_pages, 1);
    assert_eq!(result.scanned_items, 3);
    assert_eq!(result.success_candidates, 2);
}

#[tokio::test]
/// Verifies that successful wrapper execution reports parent-selection metrics.
async fn success_path_exposes_parent_selection_metrics() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_seed_failed", OperationStatus::Failed, 9),
    )
    .await
    .unwrap();
    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_seed_success", OperationStatus::Succeeded, 10),
    )
    .await
    .unwrap();

    let result =
        with_operation_log_with_conn(&db, valid_meta(), OperationScope::default(), |_txn| {
            Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) })
        })
        .await
        .unwrap();

    assert_eq!(
        result.parent_metrics.resolver_mode,
        ParentSelectionMode::SingleLatestSuccess
    );
    assert_eq!(result.parent_metrics.scanned_pages, 1);
    assert_eq!(result.parent_metrics.scanned_items, 2);
    assert_eq!(result.parent_metrics.success_candidates, 1);
    assert_eq!(result.parent_metrics.selected_parent_count, 1);
}

#[tokio::test]
/// Verifies that invalid parent-policy combinations are rejected before execution.
async fn invalid_parent_policy_is_rejected() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    let scope = OperationScope {
        parent_policy: OperationParentPolicy {
            allow_multi_parent: false,
            max_parents: 2,
        },
        ..OperationScope::default()
    };

    let error = with_operation_log_with_conn(&db, valid_meta(), scope, |_txn| {
        Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) })
    })
    .await
    .unwrap_err();

    assert!(matches!(error, OperationError::Validation(_)));
}

#[tokio::test]
/// Verifies that multi-parent scope still persists only the supported single parent today.
async fn success_path_still_persists_single_parent_when_multi_parent_reserved() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_seed_success", OperationStatus::Succeeded, 10),
    )
    .await
    .unwrap();

    let scope = OperationScope {
        parent_policy: OperationParentPolicy {
            allow_multi_parent: true,
            max_parents: 2,
        },
        ..OperationScope::default()
    };

    let result = with_operation_log_with_conn(&db, valid_meta(), scope, |_txn| {
        Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) })
    })
    .await
    .unwrap();

    let graph = OperationService::load_restore_view_by_operation_with_conn(&db, &result.op_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(graph.parents.len(), 1);
    assert_eq!(graph.parents[0].parent_op_id, "op_seed_success");
}

#[tokio::test]
/// Verifies that a successful wrapper call persists the captured view and parent edge.
async fn success_path_persists_operation_view_and_parent() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_seed_success", OperationStatus::Succeeded, 10),
    )
    .await
    .unwrap();

    let result =
        with_operation_log_with_conn(&db, valid_meta(), OperationScope::default(), |_txn| {
            Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) })
        })
        .await
        .unwrap();

    let graph = OperationService::load_restore_view_by_operation_with_conn(&db, &result.op_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(graph.view.head_kind, "branch");
    assert_eq!(graph.refs.len(), 1);
    assert_eq!(graph.workspace.len(), 1);
    assert_eq!(graph.parents.len(), 1);
    assert_eq!(graph.parents[0].parent_op_id, "op_seed_success");
}

#[tokio::test]
/// Verifies that business-step failure rolls back both probe writes and operation rows.
async fn business_failure_rolls_back_all_writes() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;
    create_tx_probe_table(&db).await;

    let error = with_operation_log_with_conn(&db, valid_meta(), OperationScope::default(), |txn| {
        Box::pin(async move {
            txn.execute(Statement::from_string(
                DbBackend::Sqlite,
                "INSERT INTO tx_probe(id) VALUES(1)".to_string(),
            ))
            .await?;
            Err::<(), DbErr>(DbErr::Custom("boom".to_string()))
        })
    })
    .await
    .unwrap_err();

    assert!(matches!(error, OperationError::Business(_)));

    let tx_count = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM tx_probe".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    let op_count = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM operation".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    assert_eq!(tx_count, 0);
    assert_eq!(op_count, 0);
}

#[tokio::test]
/// Verifies that snapshot failure leaves no persisted probe rows or operation graph data.
async fn snapshot_failure_rolls_back_and_persists_nothing() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_without_head(&db).await;
    create_tx_probe_table(&db).await;

    let error = with_operation_log_with_conn(&db, valid_meta(), OperationScope::default(), |txn| {
        Box::pin(async move {
            txn.execute(Statement::from_string(
                DbBackend::Sqlite,
                "INSERT INTO tx_probe(id) VALUES(3)".to_string(),
            ))
            .await?;
            Ok::<_, DbErr>(())
        })
    })
    .await
    .unwrap_err();

    assert!(matches!(error, OperationError::Snapshot(_)));

    let tx_count = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM tx_probe WHERE id = 3".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    let op_count = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM operation".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    assert_eq!(tx_count, 0);
    assert_eq!(op_count, 0);
}

#[tokio::test]
/// Verifies that persist failure rolls back business writes and operation rows.
async fn persist_failure_rolls_back_business_writes() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema_missing_view(&db).await;
    create_reference_table_with_head(&db).await;
    create_tx_probe_table(&db).await;

    let error = with_operation_log_with_conn(&db, valid_meta(), OperationScope::default(), |txn| {
        Box::pin(async move {
            txn.execute(Statement::from_string(
                DbBackend::Sqlite,
                "INSERT INTO tx_probe(id) VALUES(2)".to_string(),
            ))
            .await?;
            Ok::<_, DbErr>(())
        })
    })
    .await
    .unwrap_err();

    assert!(matches!(
        error,
        OperationError::Persist(_) | OperationError::Rollback(_)
    ));

    let tx_count = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM tx_probe WHERE id = 2".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    let op_count = db
        .query_one(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM operation".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    assert_eq!(tx_count, 0);
    assert_eq!(op_count, 0);
}

#[tokio::test]
/// Verifies that serial duplicate submissions are rejected inside the dedup window.
async fn serial_duplicate_submission_is_rejected_within_window() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    let meta = valid_meta_with_digest("sha256:dedup-serial");
    let first =
        with_operation_log_with_conn(&db, meta.clone(), OperationScope::default(), |_txn| {
            Box::pin(async move { Ok::<_, DbErr>("first".to_string()) })
        })
        .await
        .unwrap();

    let second = with_operation_log_with_conn(&db, meta, OperationScope::default(), |_txn| {
        Box::pin(async move { Ok::<_, DbErr>("second".to_string()) })
    })
    .await;

    assert!(matches!(second, Err(OperationError::Business(_))));

    let first_graph = OperationService::load_restore_view_by_operation_with_conn(&db, &first.op_id)
        .await
        .unwrap()
        .unwrap();
    assert!(first_graph.operation.end_ts.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
/// Verifies that concurrent duplicate submissions collapse to exactly one success.
async fn concurrent_duplicate_submission_allows_only_one_success() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    let mut handles = Vec::new();
    for _ in 0..6 {
        let db_clone = db.clone();
        handles.push(tokio::spawn(async move {
            with_operation_log_with_conn(
                &db_clone,
                valid_meta_with_digest("sha256:dedup-concurrent"),
                OperationScope::default(),
                |_txn| Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) }),
            )
            .await
        }));
    }

    let mut success_count = 0;
    let mut duplicate_error_count = 0;
    for handle in handles {
        match handle.await.unwrap() {
            Ok(_) => success_count += 1,
            Err(OperationError::Business(_)) => duplicate_error_count += 1,
            Err(err) => panic!("unexpected error: {err}"),
        }
    }

    assert_eq!(success_count, 1);
    assert_eq!(duplicate_error_count, 5);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
/// Verifies that concurrent successful writes never create orphan parent links.
async fn concurrent_writes_keep_parent_links_non_orphaned() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    OperationService::insert_operation_with_conn(
        &db,
        &sample_record("op_seed_success", OperationStatus::Succeeded, 10),
    )
    .await
    .unwrap();

    let mut handles = Vec::new();
    for _ in 0..8 {
        let db_clone = db.clone();
        handles.push(tokio::spawn(async move {
            with_operation_log_with_conn(
                &db_clone,
                valid_meta(),
                OperationScope::default(),
                |_txn| Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) }),
            )
            .await
        }));
    }

    let mut op_ids = Vec::new();
    for handle in handles {
        let result = handle.await.unwrap().unwrap();
        op_ids.push(result.op_id);
    }

    let mut seen = HashSet::new();
    for op_id in &op_ids {
        assert!(seen.insert(op_id.clone()));
        let graph = OperationService::load_restore_view_by_operation_with_conn(&db, op_id)
            .await
            .unwrap()
            .unwrap();
        assert!(graph.parents.len() <= 1);
        if let Some(parent) = graph.parents.first() {
            let parent_exists =
                OperationService::find_operation_by_id_with_conn(&db, &parent.parent_op_id)
                    .await
                    .unwrap()
                    .is_some();
            assert!(parent_exists);
        }
    }
}

#[tokio::test]
/// Verifies that successive wrapper writes build a stable one-parent restore chain.
async fn parent_chain_restore_view_consistency() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    let first =
        with_operation_log_with_conn(&db, valid_meta(), OperationScope::default(), |_txn| {
            Box::pin(async move { Ok::<_, DbErr>("first".to_string()) })
        })
        .await
        .unwrap();

    let second =
        with_operation_log_with_conn(&db, valid_meta(), OperationScope::default(), |_txn| {
            Box::pin(async move { Ok::<_, DbErr>("second".to_string()) })
        })
        .await
        .unwrap();

    let first_graph = OperationService::load_restore_view_by_operation_with_conn(&db, &first.op_id)
        .await
        .unwrap()
        .unwrap();
    let second_graph =
        OperationService::load_restore_view_by_operation_with_conn(&db, &second.op_id)
            .await
            .unwrap()
            .unwrap();

    assert_eq!(first_graph.parents.len(), 0);
    assert_eq!(second_graph.parents.len(), 1);
    assert_eq!(second_graph.parents[0].parent_op_id, first.op_id);
    assert_eq!(second_graph.view.repo_id, "repo_1");
}
