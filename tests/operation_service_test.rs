//! Service-layer tests for operation graph persistence, lookup, and pagination.

use libra::internal::operation::{
    OperationGraphRecord, OperationParentRecord, OperationQueryPage, OperationRecord,
    OperationService, OperationServiceError, OperationStatus, OperationViewRecord,
    OperationViewRefRecord, OperationViewWorkspaceRecord,
};
use sea_orm::{ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement};

/// Create the operation-layer SQLite schema used by the service tests.
async fn create_operation_schema(db: &DatabaseConnection) {
    let ddl = [
        "CREATE TABLE IF NOT EXISTS operation(\
            op_id TEXT PRIMARY KEY,\
            repo_id TEXT NOT NULL,\
            view_id TEXT NOT NULL,\
            command_name TEXT NOT NULL,\
            description TEXT NOT NULL,\
            actor TEXT NOT NULL,\
            args_digest TEXT,\
            start_ts INTEGER NOT NULL,\
            end_ts INTEGER,\
            status TEXT NOT NULL,\
            worktree_id TEXT NOT NULL DEFAULT ''\
        );",
        "CREATE TABLE IF NOT EXISTS operation_parent(\
            op_id TEXT NOT NULL,\
            parent_op_id TEXT NOT NULL,\
            PRIMARY KEY (op_id, parent_op_id)\
        );",
        "CREATE TABLE IF NOT EXISTS operation_view(\
            view_id TEXT PRIMARY KEY,\
            repo_id TEXT NOT NULL,\
            head_kind TEXT NOT NULL,\
            head_target TEXT NOT NULL,\
            created_at INTEGER NOT NULL\
        );",
        "CREATE TABLE IF NOT EXISTS operation_view_ref(\
            view_id TEXT NOT NULL,\
            ref_kind TEXT NOT NULL,\
            ref_name TEXT NOT NULL,\
            ref_remote TEXT NOT NULL,\
            target_oid TEXT NOT NULL,\
            PRIMARY KEY (view_id, ref_kind, ref_name, ref_remote)\
        );",
        "CREATE TABLE IF NOT EXISTS operation_view_workspace(\
            view_id TEXT NOT NULL,\
            pointer_kind TEXT NOT NULL,\
            pointer_value TEXT NOT NULL,\
            PRIMARY KEY (view_id, pointer_kind)\
        );",
        "CREATE INDEX IF NOT EXISTS idx_operation_repo_end_ts ON operation(repo_id, end_ts DESC);",
        "CREATE INDEX IF NOT EXISTS idx_operation_parent_parent ON operation_parent(parent_op_id, op_id);",
        "CREATE INDEX IF NOT EXISTS idx_operation_view_repo_created ON operation_view(repo_id, created_at DESC);",
    ];

    for sql in ddl {
        db.execute(Statement::from_string(DbBackend::Sqlite, sql.to_string()))
            .await
            .unwrap();
    }
}

/// Build a minimal operation record for deterministic service-layer tests.
fn sample_operation(op_id: &str, repo_id: &str, view_id: &str, end_ts: i64) -> OperationRecord {
    OperationRecord {
        op_id: op_id.to_string(),
        repo_id: repo_id.to_string(),
        view_id: view_id.to_string(),
        command_name: "commit".to_string(),
        description: format!("desc_{op_id}"),
        actor: "alice".to_string(),
        args_digest: None,
        start_ts: end_ts - 10,
        end_ts: Some(end_ts),
        status: OperationStatus::Succeeded,
        worktree_id: String::new(),
    }
}

#[tokio::test]
/// Verifies that invalid service arguments are rejected before storage access.
async fn invalid_arguments_are_rejected() {
    let db = Database::connect("sqlite::memory:").await.unwrap();

    let error = OperationService::insert_operation_with_conn(
        &db,
        &OperationRecord {
            op_id: "op_invalid".to_string(),
            repo_id: " ".to_string(),
            view_id: "view_invalid".to_string(),
            command_name: "commit".to_string(),
            description: "desc".to_string(),
            actor: "alice".to_string(),
            args_digest: None,
            start_ts: 10,
            end_ts: Some(20),
            status: OperationStatus::Succeeded,
            worktree_id: String::new(),
        },
    )
    .await
    .unwrap_err();
    assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

    let error = OperationService::find_operation_by_id_with_conn(&db, " ")
        .await
        .unwrap_err();
    assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

    let error = OperationService::list_operations_by_repo_with_conn(&db, " ", 1)
        .await
        .unwrap_err();
    assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

    let error = OperationService::replace_view_refs_with_conn(&db, " ", &[])
        .await
        .unwrap_err();
    assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

    let error = OperationService::list_workspace_snapshots_with_conn(&db, " ")
        .await
        .unwrap_err();
    assert!(matches!(error, OperationServiceError::InvalidArgument(_)));
}

#[tokio::test]
/// Verifies that unique constraints are enforced for duplicated refs and workspace pointers.
async fn duplicate_constraints_are_enforced_for_view_refs_and_workspace() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let ref_insert = Statement::from_string(
        DbBackend::Sqlite,
        "INSERT INTO operation_view_ref(view_id, ref_kind, ref_name, ref_remote, target_oid) VALUES ('view_dup', 'branch', 'main', '', 'oid-1');",
    );
    db.execute(ref_insert).await.unwrap();
    let duplicate_ref = Statement::from_string(
        DbBackend::Sqlite,
        "INSERT INTO operation_view_ref(view_id, ref_kind, ref_name, ref_remote, target_oid) VALUES ('view_dup', 'branch', 'main', '', 'oid-2');",
    );
    assert!(db.execute(duplicate_ref).await.is_err());

    let workspace_insert = Statement::from_string(
        DbBackend::Sqlite,
        "INSERT INTO operation_view_workspace(view_id, pointer_kind, pointer_value) VALUES ('view_dup', 'index', 'oid-1');",
    );
    db.execute(workspace_insert).await.unwrap();
    let duplicate_workspace = Statement::from_string(
        DbBackend::Sqlite,
        "INSERT INTO operation_view_workspace(view_id, pointer_kind, pointer_value) VALUES ('view_dup', 'index', 'oid-2');",
    );
    assert!(db.execute(duplicate_workspace).await.is_err());
}

#[tokio::test]
/// Verifies the load path for both a missing operation and a missing snapshot view.
async fn graph_load_handles_missing_operation_and_missing_view() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let missing = OperationService::load_restore_view_by_operation_with_conn(&db, "op_missing")
        .await
        .unwrap();
    assert!(missing.is_none());

    let op_only = sample_operation("op_only", "repo_only", "view_only", 100);
    OperationService::insert_operation_with_conn(&db, &op_only)
        .await
        .unwrap();

    let error = OperationService::load_restore_view_by_operation_with_conn(&db, "op_only")
        .await
        .unwrap_err();
    assert!(matches!(error, OperationServiceError::Storage(_)));
}

#[tokio::test]
/// Verifies round-trip persistence for main operation records and ordered listing.
async fn operation_main_record_write_read_roundtrip() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let op1 = sample_operation("op_1", "repo_main", "view_1", 120);
    let op2 = sample_operation("op_2", "repo_main", "view_2", 200);

    OperationService::insert_operation_with_conn(&db, &op1)
        .await
        .unwrap();
    OperationService::insert_operation_with_conn(&db, &op2)
        .await
        .unwrap();

    let found = OperationService::find_operation_by_id_with_conn(&db, "op_1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found.repo_id, "repo_main");
    assert_eq!(found.view_id, "view_1");
    assert_eq!(found.end_ts, Some(120));

    let listed = OperationService::list_operations_by_repo_with_conn(&db, "repo_main", 2)
        .await
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].op_id, "op_2");
    assert_eq!(listed[1].op_id, "op_1");
}

#[tokio::test]
/// Verifies parent-edge validation, insertion, readback, and duplicate failure handling.
async fn parent_relation_write_read_and_failure_cases() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let illegal = OperationParentRecord {
        op_id: "op_same".to_string(),
        parent_op_id: "op_same".to_string(),
    };
    let error = OperationService::insert_parent_with_conn(&db, &illegal)
        .await
        .unwrap_err();
    assert!(matches!(error, OperationServiceError::InvalidArgument(_)));

    let parent1 = OperationParentRecord {
        op_id: "op_2".to_string(),
        parent_op_id: "op_0".to_string(),
    };
    let parent2 = OperationParentRecord {
        op_id: "op_2".to_string(),
        parent_op_id: "op_1".to_string(),
    };

    OperationService::insert_parent_with_conn(&db, &parent1)
        .await
        .unwrap();
    OperationService::insert_parent_with_conn(&db, &parent2)
        .await
        .unwrap();

    let parents = OperationService::list_parents_with_conn(&db, "op_2")
        .await
        .unwrap();
    assert_eq!(parents.len(), 2);
    assert_eq!(parents[0].parent_op_id, "op_0");
    assert_eq!(parents[1].parent_op_id, "op_1");

    let duplicate_error = OperationService::insert_parent_with_conn(&db, &parent1)
        .await
        .unwrap_err();
    assert!(matches!(duplicate_error, OperationServiceError::Storage(_)));
}

#[tokio::test]
/// Verifies round-trip persistence for snapshot views, refs, and workspace pointers.
async fn view_refs_workspace_snapshot_write_read_roundtrip() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let op = sample_operation("op_view", "repo_view", "view_roundtrip", 320);
    let view = OperationViewRecord {
        view_id: "view_roundtrip".to_string(),
        repo_id: "repo_view".to_string(),
        head_kind: "branch".to_string(),
        head_target: "main".to_string(),
        created_at: 320,
    };

    OperationService::insert_operation_with_conn(&db, &op)
        .await
        .unwrap();
    OperationService::insert_view_with_conn(&db, &view)
        .await
        .unwrap();

    let found_view = OperationService::find_view_by_operation_with_conn(&db, "op_view")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(found_view.view_id, "view_roundtrip");

    let refs = vec![
        OperationViewRefRecord {
            view_id: "view_roundtrip".to_string(),
            ref_kind: "remote".to_string(),
            ref_name: "main".to_string(),
            ref_remote: Some("origin".to_string()),
            target_oid: "oid-remote-main".to_string(),
        },
        OperationViewRefRecord {
            view_id: "view_roundtrip".to_string(),
            ref_kind: "branch".to_string(),
            ref_name: "main".to_string(),
            ref_remote: None,
            target_oid: "oid-local-main".to_string(),
        },
    ];

    OperationService::replace_view_refs_with_conn(&db, "view_roundtrip", &refs)
        .await
        .unwrap();
    let listed_refs = OperationService::list_view_refs_with_conn(&db, "view_roundtrip")
        .await
        .unwrap();
    assert_eq!(listed_refs.len(), 2);
    assert_eq!(listed_refs[0].ref_kind, "branch");
    assert_eq!(listed_refs[1].ref_kind, "remote");

    let index = OperationViewWorkspaceRecord {
        view_id: "view_roundtrip".to_string(),
        pointer_kind: "index".to_string(),
        pointer_value: "oid-index-v1".to_string(),
    };
    let worktree = OperationViewWorkspaceRecord {
        view_id: "view_roundtrip".to_string(),
        pointer_kind: "worktree".to_string(),
        pointer_value: "oid-worktree-v1".to_string(),
    };

    OperationService::upsert_workspace_snapshot_with_conn(&db, &index)
        .await
        .unwrap();
    OperationService::upsert_workspace_snapshot_with_conn(&db, &worktree)
        .await
        .unwrap();

    let updated_index = OperationViewWorkspaceRecord {
        pointer_value: "oid-index-v2".to_string(),
        ..index
    };
    OperationService::upsert_workspace_snapshot_with_conn(&db, &updated_index)
        .await
        .unwrap();

    let workspace = OperationService::list_workspace_snapshots_with_conn(&db, "view_roundtrip")
        .await
        .unwrap();
    assert_eq!(workspace.len(), 2);
    assert_eq!(workspace[0].pointer_kind, "index");
    assert_eq!(workspace[0].pointer_value, "oid-index-v2");
    assert_eq!(workspace[1].pointer_kind, "worktree");
    assert_eq!(workspace[1].pointer_value, "oid-worktree-v1");
}

#[tokio::test]
/// Verifies ordered pagination, out-of-range pages, and normalization behavior.
async fn paginated_log_query_order_and_boundary() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let records = [
        sample_operation("op_a", "repo_page", "view_a", 100),
        sample_operation("op_b", "repo_page", "view_b", 300),
        sample_operation("op_c", "repo_page", "view_c", 200),
        sample_operation("op_d", "repo_page", "view_d", 400),
        sample_operation("op_e", "repo_page", "view_e", 50),
        sample_operation("op_other", "repo_other", "view_other", 999),
    ];
    for record in records {
        OperationService::insert_operation_with_conn(&db, &record)
            .await
            .unwrap();
    }

    let page1 = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        "repo_page",
        OperationQueryPage {
            page: 1,
            per_page: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(page1.total, 5);
    assert_eq!(page1.items.len(), 2);
    assert_eq!(page1.items[0].op_id, "op_d");
    assert_eq!(page1.items[1].op_id, "op_b");

    let page2 = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        "repo_page",
        OperationQueryPage {
            page: 2,
            per_page: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(page2.total, 5);
    assert_eq!(page2.items.len(), 2);
    assert_eq!(page2.items[0].op_id, "op_c");
    assert_eq!(page2.items[1].op_id, "op_a");

    let page3 = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        "repo_page",
        OperationQueryPage {
            page: 3,
            per_page: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(page3.total, 5);
    assert_eq!(page3.items.len(), 1);
    assert_eq!(page3.items[0].op_id, "op_e");

    let out_of_range = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        "repo_page",
        OperationQueryPage {
            page: 4,
            per_page: 2,
        },
    )
    .await
    .unwrap();
    assert_eq!(out_of_range.total, 5);
    assert!(out_of_range.items.is_empty());

    let normalized = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        "repo_page",
        OperationQueryPage {
            page: 0,
            per_page: 0,
        },
    )
    .await
    .unwrap();
    assert_eq!(normalized.page, 1);
    assert_eq!(normalized.per_page, 50);
    assert_eq!(normalized.total, 5);
    assert_eq!(normalized.items.len(), 5);
}

#[tokio::test]
/// Verifies command-filtered pagination is applied by the storage query.
async fn paginated_log_query_filters_command_before_paging() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let mut branch_old = sample_operation("op_branch_old", "repo_filter", "view_branch_old", 100);
    branch_old.command_name = "branch".to_string();
    let mut commit_new = sample_operation("op_commit_new", "repo_filter", "view_commit_new", 500);
    commit_new.command_name = "commit".to_string();
    let mut branch_mid = sample_operation("op_branch_mid", "repo_filter", "view_branch_mid", 300);
    branch_mid.command_name = "branch".to_string();
    let mut branch_new = sample_operation("op_branch_new", "repo_filter", "view_branch_new", 700);
    branch_new.command_name = "branch".to_string();

    for record in [branch_old, commit_new, branch_mid, branch_new] {
        OperationService::insert_operation_with_conn(&db, &record)
            .await
            .unwrap();
    }

    let page = OperationService::list_operations_by_repo_and_command_paginated_with_conn(
        &db,
        "repo_filter",
        Some("branch"),
        OperationQueryPage {
            page: 2,
            per_page: 1,
        },
    )
    .await
    .unwrap();

    assert_eq!(page.total, 3);
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].op_id, "op_branch_mid");

    let unfiltered = OperationService::list_operations_by_repo_and_command_paginated_with_conn(
        &db,
        "repo_filter",
        Some(" "),
        OperationQueryPage {
            page: 1,
            per_page: 10,
        },
    )
    .await
    .unwrap();
    assert_eq!(unfiltered.total, 4);
}

#[tokio::test]
/// Verifies stable ordering when multiple operations share identical timestamps.
async fn paginated_log_query_is_deterministic_when_timestamps_tie() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let same_end = 500;
    let mut op_a = sample_operation("op_a", "repo_tie", "view_a", same_end);
    op_a.start_ts = 450;
    let mut op_b = sample_operation("op_b", "repo_tie", "view_b", same_end);
    op_b.start_ts = 450;
    let mut op_c = sample_operation("op_c", "repo_tie", "view_c", same_end);
    op_c.start_ts = 450;

    for record in [op_a, op_b, op_c] {
        OperationService::insert_operation_with_conn(&db, &record)
            .await
            .unwrap();
    }

    let page = OperationService::list_operations_by_repo_paginated_with_conn(
        &db,
        "repo_tie",
        OperationQueryPage {
            page: 1,
            per_page: 10,
        },
    )
    .await
    .unwrap();

    let ordered: Vec<String> = page.items.into_iter().map(|item| item.op_id).collect();
    assert_eq!(ordered, vec!["op_c", "op_b", "op_a"]);
}

#[tokio::test]
/// Verifies full graph round-trip persistence and duplicate graph rejection.
async fn graph_roundtrip_and_duplicate_constraint_failure() {
    let db = Database::connect("sqlite::memory:").await.unwrap();
    create_operation_schema(&db).await;

    let graph = OperationGraphRecord {
        operation: OperationRecord {
            op_id: "op_graph".to_string(),
            repo_id: "repo_graph".to_string(),
            view_id: "view_graph".to_string(),
            command_name: "merge".to_string(),
            description: "merge feature into main".to_string(),
            actor: "alice".to_string(),
            args_digest: Some("sha256:graph".to_string()),
            start_ts: 500,
            end_ts: Some(510),
            status: OperationStatus::Succeeded,
            worktree_id: String::new(),
        },
        parents: vec![OperationParentRecord {
            op_id: "op_graph".to_string(),
            parent_op_id: "op_prev".to_string(),
        }],
        view: OperationViewRecord {
            view_id: "view_graph".to_string(),
            repo_id: "repo_graph".to_string(),
            head_kind: "branch".to_string(),
            head_target: "main".to_string(),
            created_at: 510,
        },
        refs: vec![OperationViewRefRecord {
            view_id: "view_graph".to_string(),
            ref_kind: "branch".to_string(),
            ref_name: "main".to_string(),
            ref_remote: None,
            target_oid: "oid-main".to_string(),
        }],
        workspace: vec![OperationViewWorkspaceRecord {
            view_id: "view_graph".to_string(),
            pointer_kind: "index".to_string(),
            pointer_value: "oid-index".to_string(),
        }],
    };

    let persisted = OperationService::persist_operation_graph_with_conn(&db, &graph)
        .await
        .unwrap();
    let loaded = OperationService::load_restore_view_by_operation_with_conn(&db, "op_graph")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(persisted.operation.op_id, "op_graph");
    assert_eq!(loaded.operation.op_id, "op_graph");
    assert_eq!(loaded.parents.len(), 1);
    assert_eq!(loaded.refs.len(), 1);
    assert_eq!(loaded.workspace.len(), 1);

    let duplicate_op_error = OperationService::insert_operation_with_conn(&db, &graph.operation)
        .await
        .unwrap_err();
    assert!(matches!(
        duplicate_op_error,
        OperationServiceError::Storage(_)
    ));

    let duplicate_view = OperationViewRecord {
        view_id: "view_graph".to_string(),
        repo_id: "repo_graph".to_string(),
        head_kind: "branch".to_string(),
        head_target: "main".to_string(),
        created_at: 520,
    };
    let duplicate_view_error = OperationService::insert_view_with_conn(&db, &duplicate_view)
        .await
        .unwrap_err();
    assert!(matches!(
        duplicate_view_error,
        OperationServiceError::Storage(_)
    ));
}
