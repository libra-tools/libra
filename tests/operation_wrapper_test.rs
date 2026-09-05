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
        scope_provenance: "declared".to_string(),
        restorable: true,
        control_slot: None,
        claim_owner: None,
        scope_kind: "main".to_string(),
    }
}

/// Create the full operation-layer schema required by wrapper tests.
async fn create_operation_schema(db: &DatabaseConnection) {
    let ddl = [
        "CREATE TABLE legacy_operation(op_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,view_id TEXT NOT NULL,command_name TEXT NOT NULL,description TEXT NOT NULL,actor TEXT NOT NULL,args_digest TEXT,start_ts INTEGER NOT NULL,end_ts INTEGER,status TEXT NOT NULL,worktree_id TEXT NOT NULL DEFAULT '',scope_provenance TEXT NOT NULL DEFAULT 'declared',restorable INTEGER NOT NULL DEFAULT 1,control_slot TEXT,claim_owner TEXT,scope_kind TEXT NOT NULL DEFAULT 'main');",
        "CREATE TABLE legacy_operation_parent(op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id));",
        // Present in every real repository (bootstrap schema); the write-lock
        // primitive in `db::begin_write_transaction` writes a no-op row filter
        // against it, and a fixture without it is not a repository database.
        "CREATE TABLE config_kv(id INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL,value TEXT NOT NULL,encrypted INTEGER NOT NULL DEFAULT 0);",
        "CREATE TABLE legacy_operation_view(view_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,head_kind TEXT NOT NULL,head_target TEXT NOT NULL,created_at INTEGER NOT NULL);",
        "CREATE TABLE legacy_operation_view_ref(view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote));",
        "CREATE TABLE legacy_operation_view_workspace(view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind));",
    ];
    for sql in ddl {
        db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .unwrap();
    }
}

/// Create a schema that is missing `legacy_operation_view` so persist failure paths can be exercised.
async fn create_operation_schema_missing_view(db: &DatabaseConnection) {
    let ddl = [
        "CREATE TABLE legacy_operation(op_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,view_id TEXT NOT NULL,command_name TEXT NOT NULL,description TEXT NOT NULL,actor TEXT NOT NULL,args_digest TEXT,start_ts INTEGER NOT NULL,end_ts INTEGER,status TEXT NOT NULL,worktree_id TEXT NOT NULL DEFAULT '',scope_provenance TEXT NOT NULL DEFAULT 'declared',restorable INTEGER NOT NULL DEFAULT 1,control_slot TEXT,claim_owner TEXT,scope_kind TEXT NOT NULL DEFAULT 'main');",
        "CREATE TABLE legacy_operation_parent(op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id));",
        // Present in every real repository (bootstrap schema); the write-lock
        // primitive in `db::begin_write_transaction` writes a no-op row filter
        // against it, and a fixture without it is not a repository database.
        "CREATE TABLE config_kv(id INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL,value TEXT NOT NULL,encrypted INTEGER NOT NULL DEFAULT 0);",
        "CREATE TABLE legacy_operation_view_ref(view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote));",
        "CREATE TABLE legacy_operation_view_workspace(view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind));",
    ];
    for sql in ddl {
        db.execute_raw(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .unwrap();
    }
}

/// Create the reference table with both HEAD and main branch rows.
async fn create_reference_table_with_head(db: &DatabaseConnection) {
    db.execute_raw(Statement::from_string(
         DbBackend::Sqlite,
         "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT,kind TEXT NOT NULL,\"commit\" TEXT,remote TEXT,worktree_id TEXT)".to_string(),
     ))
     .await
     .unwrap();
    // HEAD resolution is scoped to `WorktreeScope::current()` (cwd-derived):
    // seed the row for the scope this process actually runs in, so the suite
    // also passes when invoked from inside a linked worktree (where main's
    // NULL-worktree_id row is invisible to the scoped query).
    let scope_worktree_id = libra::internal::worktree_scope::WorktreeScope::current()
        .worktree_id()
        .map(str::to_string);
    db.execute_raw(Statement::from_sql_and_values(
         DbBackend::Sqlite,
         "INSERT INTO reference(name, kind, \"commit\", remote, worktree_id) VALUES('main', 'Head', NULL, NULL, ?)",
         [scope_worktree_id.into()],
     ))
     .await
     .unwrap();
    db.execute_raw(Statement::from_string(
         DbBackend::Sqlite,
         "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)".to_string(),
     ))
     .await
     .unwrap();
}

/// Create the reference table without a HEAD row to force snapshot failure.
async fn create_reference_table_without_head(db: &DatabaseConnection) {
    db.execute_raw(Statement::from_string(
         DbBackend::Sqlite,
         "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT,kind TEXT NOT NULL,\"commit\" TEXT,remote TEXT,worktree_id TEXT)".to_string(),
     ))
     .await
     .unwrap();
}

/// Create a probe table used to assert rollback behavior.
async fn create_tx_probe_table(db: &DatabaseConnection) {
    db.execute_raw(Statement::from_string(
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
            txn.execute_raw(Statement::from_string(
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
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM tx_probe".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    let op_count = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM legacy_operation".to_string(),
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
            txn.execute_raw(Statement::from_string(
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
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM tx_probe WHERE id = 3".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    let op_count = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM legacy_operation".to_string(),
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
            txn.execute_raw(Statement::from_string(
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
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM tx_probe WHERE id = 2".to_string(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get_by_index::<i64>(0)
        .unwrap_or_default();
    let op_count = db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT COUNT(*) FROM legacy_operation".to_string(),
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

/// Part C W1 (§C.9): the duplicate window is a SCOPE point query, so
/// operations in OTHER worktrees cannot push a same-scope operation out of it.
///
/// The old implementation took the newest 50 rows for the whole repository and
/// then filtered by worktree in memory. Fifty newer operations elsewhere were
/// therefore enough to hide the duplicate — the check silently stopped
/// working on exactly the repositories the scoping was added for.
#[tokio::test]
async fn cross_scope_interference_cannot_hide_a_duplicate() {
    use sea_orm::{ConnectionTrait, Statement};

    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    // One same-scope success, which the next identical submission must be
    // refused against.
    let meta = valid_meta_with_digest("sha256:dedup-interference");
    with_operation_log_with_conn(&db, meta.clone(), OperationScope::default(), |_txn| {
        Box::pin(async move { Ok::<_, DbErr>("first".to_string()) })
    })
    .await
    .unwrap();

    // Fifty NEWER operations in other worktrees — more than the old page.
    let now = chrono::Utc::now().timestamp();
    for i in 0..50 {
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO legacy_operation (op_id, repo_id, view_id, command_name, description, actor, \
              args_digest, start_ts, end_ts, status, worktree_id, scope_provenance) \
              VALUES (?, 'repo_1', ?, 'commit', 'other worktree', 'bob', 'sha256:other', ?, ?, \
              'succeeded', ?, 'declared')",
            [
                format!("op-other-{i}").into(),
                format!("view-other-{i}").into(),
                (now + 1).into(),
                (now + 1).into(),
                format!("wt-other-{i}").into(),
            ],
        ))
        .await
        .expect("seed a cross-scope operation");
    }

    let second = with_operation_log_with_conn(&db, meta, OperationScope::default(), |_txn| {
        Box::pin(async move { Ok::<_, DbErr>("second".to_string()) })
    })
    .await;
    assert!(
        matches!(second, Err(OperationError::Business(_))),
        "the duplicate must still be refused with 50 newer cross-scope rows present"
    );
}

/// The converse: the same command and digest in a DIFFERENT worktree is not a
/// duplicate. Two linked worktrees running the same control action within the
/// window must both proceed (§C.11 W1 acceptance).
#[tokio::test]
async fn the_same_action_in_another_worktree_is_not_a_duplicate() {
    use sea_orm::{ConnectionTrait, Statement};

    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    // A success recorded by ANOTHER worktree, one second ago.
    let now = chrono::Utc::now().timestamp();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO legacy_operation (op_id, repo_id, view_id, command_name, description, actor, \
          args_digest, start_ts, end_ts, status, worktree_id, scope_provenance) \
          VALUES ('op-linked', 'repo_1', 'view-linked', 'commit', 'linked worktree', 'bob', \
          'sha256:same-action', ?, ?, 'succeeded', 'wt-linked-1', 'declared')",
        [(now - 1).into(), (now - 1).into()],
    ))
    .await
    .expect("seed the other worktree's operation");

    // This worktree (main, scope key "") runs the identical action.
    let accepted = with_operation_log_with_conn(
        &db,
        valid_meta_with_digest("sha256:same-action"),
        OperationScope::default(),
        |_txn| Box::pin(async move { Ok::<_, DbErr>("mine".to_string()) }),
    )
    .await;
    assert!(
        accepted.is_ok(),
        "another worktree's identical action must not dedupe mine: {accepted:?}"
    );
}

/// §C.12 named regression `op_restore_dedup_key_is_scope_aware`: `op restore`
/// is a WRAPPED command, so its dedup identity is the one users can actually
/// hit. Restoring the same operation id from two worktrees inside the window
/// must not read as a repeat of itself.
///
/// This is asserted at the wrapper seam rather than through the CLI on
/// purpose: end to end, the cross-scope restore guard (`op.rs`, ADR-0714-08)
/// refuses a linked worktree BEFORE the wrapper is reached, so a command-level
/// test would pass with a repo-wide key and prove nothing about the key.
#[tokio::test]
async fn op_restore_dedup_key_is_scope_aware() {
    use sea_orm::{ConnectionTrait, Statement};

    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    let target_op_id = "0191f0de-0000-7000-8000-00000000cafe";
    let restore_meta = || OperationMeta {
        command_name: "op restore".to_string(),
        description: format!("restore to {}", &target_op_id[..8]),
        actor: "alice".to_string(),
        repo_id: "repo_1".to_string(),
        args_digest: Some(target_op_id.to_string()),
    };

    // Another worktree restored this very operation one second ago.
    let now = chrono::Utc::now().timestamp();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO legacy_operation (op_id, repo_id, view_id, command_name, description, actor, \
          args_digest, start_ts, end_ts, status, worktree_id, scope_provenance) \
          VALUES ('op-linked-restore', 'repo_1', 'view-linked-restore', 'op restore', \
          'restore to 0191f0de', 'bob', ?, ?, ?, 'succeeded', 'wt-linked-1', 'declared')",
        [target_op_id.into(), (now - 1).into(), (now - 1).into()],
    ))
    .await
    .expect("seed the other worktree's restore");

    // This scope's identical restore is a different operation, and proceeds.
    let mine =
        with_operation_log_with_conn(&db, restore_meta(), OperationScope::default(), |_txn| {
            Box::pin(async move { Ok::<_, DbErr>("mine".to_string()) })
        })
        .await;
    assert!(
        mine.is_ok(),
        "another worktree's restore of the same operation must not dedupe mine: {mine:?}"
    );

    // ...and the key still has teeth WITHIN the scope: repeating it here is a
    // duplicate. Without this half the test would pass with dedup disabled.
    let repeat =
        with_operation_log_with_conn(&db, restore_meta(), OperationScope::default(), |_txn| {
            Box::pin(async move { Ok::<_, DbErr>("repeat".to_string()) })
        })
        .await;
    assert!(
        matches!(repeat, Err(OperationError::Business(_))),
        "the same restore repeated in THIS scope is still a duplicate: {repeat:?}"
    );
}

/// A row written BEFORE digests were canonicalized (padded with whitespace)
/// must still be found by the duplicate check.
///
/// The window is a SQL equality now, so a stored `" digest "` is invisible to
/// a `"digest"` submission — and the duplicate it should refuse goes through
/// once. Migration `2026073001` canonicalizes such rows; this pins the
/// behaviour the migration exists for.
#[tokio::test]
async fn a_legacy_padded_digest_row_still_blocks_a_duplicate() {
    use sea_orm::{ConnectionTrait, Statement};

    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    // A pre-canonicalization row: same scope, same command, PADDED digest.
    // The dedup window is a scope point query keyed by `storage_key()` — seed
    // the rows for the scope this process actually runs in ('' for main, the
    // id inside a linked worktree) so the suite passes from either cwd.
    let scope_storage_key = libra::internal::worktree_scope::WorktreeScope::current()
        .storage_key()
        .to_string();
    let now = chrono::Utc::now().timestamp();
    db.execute_raw(Statement::from_sql_and_values(
        db.get_database_backend(),
        "INSERT INTO legacy_operation (op_id, repo_id, view_id, command_name, description, actor, \
          args_digest, start_ts, end_ts, status, worktree_id, scope_provenance) \
          VALUES ('op-legacy', 'repo_1', 'view-legacy', 'commit', 'legacy row', 'alice', \
          '  sha256:legacy-pad  ', ?, ?, 'succeeded', ?, 'declared')",
        [
            (now - 1).into(),
            (now - 1).into(),
            scope_storage_key.clone().into(),
        ],
    ))
    .await
    .expect("seed a legacy padded row");

    // Rows padded with a TAB and a NEWLINE, and one that is whitespace only —
    // SQLite's one-argument TRIM would leave all three untouched.
    for (op_id, digest) in [("op-tab", "\tsha256:legacy-tab\n"), ("op-ws", "   ")] {
        db.execute_raw(Statement::from_sql_and_values(
            db.get_database_backend(),
            "INSERT INTO legacy_operation (op_id, repo_id, view_id, command_name, description, actor, \
              args_digest, start_ts, end_ts, status, worktree_id, scope_provenance) \
              VALUES (?, 'repo_1', ?, 'commit', 'legacy row', 'alice', ?, ?, ?, 'succeeded', ?, \
              'declared')",
            [
                op_id.into(),
                format!("view-{op_id}").into(),
                digest.into(),
                (now - 1).into(),
                (now - 1).into(),
                scope_storage_key.clone().into(),
            ],
        ))
        .await
        .expect("seed a legacy row");
    }

    // Run the shipped migration body against the retained legacy table, not a
    // hand-written approximation: the predicate and trim set must remain the
    // same while the active v1 service is pointed at `legacy_operation`.
    let canonical_sql = include_str!(
        "../sql/migrations/2026073001_operation_args_digest_canonical.sql"
    )
    .replacen("`operation`", "`legacy_operation`", 1);
    db.execute_unprepared(&canonical_sql)
        .await
        .expect("canonicalize");

    // Whitespace-only becomes NULL (no digest), and the tab/newline row is
    // trimmed to its token.
    let rows = db
        .query_all_raw(Statement::from_string(
            db.get_database_backend(),
            "SELECT op_id, args_digest FROM legacy_operation WHERE op_id IN ('op-tab', 'op-ws') \
              ORDER BY op_id"
                .to_string(),
        ))
        .await
        .expect("read back");
    let canonical: Vec<(String, Option<String>)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get_by_index::<String>(0).expect("op_id"),
                row.try_get_by_index::<Option<String>>(1).expect("digest"),
            )
        })
        .collect();
    assert_eq!(
        canonical,
        vec![
            ("op-tab".to_string(), Some("sha256:legacy-tab".to_string())),
            ("op-ws".to_string(), None),
        ],
        "the shipped migration must trim ASCII whitespace and NULL an empty result"
    );

    // And the tab-padded row now blocks its duplicate.
    let tab_blocked = with_operation_log_with_conn(
        &db,
        valid_meta_with_digest("sha256:legacy-tab"),
        OperationScope::default(),
        |_txn| Box::pin(async move { Ok::<_, DbErr>("dup".to_string()) }),
    )
    .await;
    assert!(
        matches!(tab_blocked, Err(OperationError::Business(_))),
        "a tab/newline-padded legacy row must refuse its duplicate too: {tab_blocked:?}"
    );

    let blocked = with_operation_log_with_conn(
        &db,
        valid_meta_with_digest("sha256:legacy-pad"),
        OperationScope::default(),
        |_txn| Box::pin(async move { Ok::<_, DbErr>("dup".to_string()) }),
    )
    .await;
    assert!(
        matches!(blocked, Err(OperationError::Business(_))),
        "a canonicalized legacy row must still refuse the duplicate: {blocked:?}"
    );
}

/// A submission whose digest carries whitespace is stored canonically, so the
/// NEXT identical submission matches it.
#[tokio::test]
async fn a_padded_digest_is_stored_canonically() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;
    create_reference_table_with_head(&db).await;

    with_operation_log_with_conn(
        &db,
        valid_meta_with_digest("  sha256:pad-on-write  "),
        OperationScope::default(),
        |_txn| Box::pin(async move { Ok::<_, DbErr>("first".to_string()) }),
    )
    .await
    .expect("first submission");

    let second = with_operation_log_with_conn(
        &db,
        valid_meta_with_digest("sha256:pad-on-write"),
        OperationScope::default(),
        |_txn| Box::pin(async move { Ok::<_, DbErr>("second".to_string()) }),
    )
    .await;
    assert!(
        matches!(second, Err(OperationError::Business(_))),
        "the trimmed form must match what was stored: {second:?}"
    );
}
