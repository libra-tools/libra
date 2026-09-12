//! Successful persistent operations must pair each scope key with its own storage.

use sea_orm::{ActiveModelTrait, ActiveValue::Set, ConnectionTrait, DbBackend, Statement};
use tokio::sync::oneshot;

use super::*;
use crate::{
    internal::{
        db::get_db_conn_instance_for_path,
        layer::LayerStore,
        model::reference,
        operation::{OperationResult, OperationStoreV2, WorkspaceStatePointer},
        worktree_scope::request_db,
    },
    utils::{client_storage::ClientStorage, util},
};

async fn linked_fixture() -> ScopeFixture {
    let repository = fixture(true).await;
    let linked = repository.scope.workdir.join("linked");
    let gitdir = linked.join(".libra");
    fs::create_dir_all(&gitdir).expect("linked gitdir");
    let linked = linked.canonicalize().expect("canonical linked root");
    let worktree_id = util::worktree_instance_id(&linked);
    fs::write(gitdir.join("worktree_id"), &worktree_id).expect("linked identity");
    fs::write(
        gitdir.join("commondir"),
        repository.scope.storage.to_string_lossy().as_bytes(),
    )
    .expect("linked common storage");
    fs::write(gitdir.join("HEAD"), b"ref: refs/heads/linked\n").expect("linked HEAD");
    let scope = RequestScope::resolve(linked).expect("resolved linked request");
    assert_eq!(scope.scope, WorktreeScope::Linked(worktree_id));
    assert_eq!(scope.storage, repository.scope.storage);
    assert_eq!(scope.gitdir, gitdir);
    let connection = get_db_conn_instance_for_path(&scope.storage.join("libra.db"))
        .await
        .expect("linked repository connection");
    reference::ActiveModel {
        name: Set(Some("linked".to_string())),
        kind: Set(reference::ConfigKind::Head),
        commit: Set(None),
        remote: Set(None),
        worktree_id: Set(scope.scope.worktree_id().map(str::to_string)),
        ..Default::default()
    }
    .insert(&connection)
    .await
    .expect("linked unborn HEAD");
    ScopeFixture {
        _directory: repository._directory,
        scope,
    }
}

async fn write_request_layer(source: &str) -> Result<Option<RequestScope>, OperationError> {
    // The write derives BOTH its key and database from request consumers, not the fixture.
    LayerStore::add(
        &WorktreeScope::for_request(),
        "scope-probe",
        source,
        0,
        true,
    )
    .await
    .map_err(OperationError::Storage)?;
    let connection = request_db()
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let rows = connection
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "PRAGMA database_list",
        ))
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let database: String = rows[0].try_get_by_index(2).expect("actual SQLite file");
    let scope = WorktreeScope::request_scope().expect("request binding");
    assert_eq!(
        std::path::PathBuf::from(database)
            .canonicalize()
            .expect("actual database path"),
        scope
            .storage
            .join("libra.db")
            .canonicalize()
            .expect("request database path")
    );
    Ok(Some(scope))
}

async fn assert_published(
    scope: &RequestScope,
    source: &str,
    result: &OperationResult<Option<RequestScope>>,
) {
    assert!(result.recorded);
    assert_eq!(result.value, Some(scope.clone()));
    let op_id = result
        .operation_id
        .as_ref()
        .expect("persistent operation id");
    let connection = get_db_conn_instance_for_path(&scope.storage.join("libra.db"))
        .await
        .expect("explicit repository connection");
    let rows = connection
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT worktree_id, name, source FROM layer ORDER BY worktree_id, name",
        ))
        .await
        .expect("stored layers");
    let layers: Vec<(String, String, String)> = rows
        .iter()
        .map(|row| {
            (
                row.try_get_by_index(0).expect("scope key"),
                row.try_get_by_index(1).expect("layer name"),
                row.try_get_by_index(2).expect("layer source"),
            )
        })
        .collect();
    assert_eq!(
        layers,
        [(
            scope.scope.storage_key().into(),
            "scope-probe".into(),
            source.into()
        )]
    );
    let repo_id = RepoIdentity::resolve(&connection)
        .await
        .expect("repository identity");
    let operations = connection
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT op_id, repo_id, status FROM operation",
        ))
        .await
        .expect("stored operations");
    assert_eq!(operations.len(), 1);
    assert_eq!(
        operations[0]
            .try_get_by_index::<String>(0)
            .expect("operation id"),
        *op_id
    );
    assert_eq!(
        operations[0]
            .try_get_by_index::<String>(1)
            .expect("operation repository"),
        repo_id.as_str()
    );
    assert_eq!(
        operations[0]
            .try_get_by_index::<String>(2)
            .expect("operation status"),
        "success"
    );
    let store = OperationStoreV2::new_for_repo(
        repo_id.as_str(),
        connection,
        ClientStorage::init_local(scope.storage.join("objects")),
    );
    assert_eq!(
        store
            .read_heads(repo_id.as_str(), scope.scope.storage_key())
            .await
            .expect("scope heads"),
        std::slice::from_ref(op_id)
    );
    assert_eq!(
        store
            .read_head_generation(repo_id.as_str(), scope.scope.storage_key())
            .await
            .expect("scope generation"),
        1
    );
    let pointer = WorkspaceStatePointer::load(scope)
        .await
        .expect("linked pointer");
    assert_eq!((&pointer.last_op_id, pointer.generation), (op_id, 1));
    assert!(!scope.storage.join("info/operation-pointer.json").exists());
}

#[tokio::test]
#[serial]
async fn operation_scope_successful_persistent_writes_keep_keys_and_storage_isolated() {
    let a = linked_fixture().await;
    let b = linked_fixture().await;
    let fallback = fixture(true).await;
    let _fallback_pin = WorktreeScope::pin_request_scope(fallback.scope.workdir.clone());
    let (a_entered_tx, a_entered_rx) = oneshot::channel();
    let (b_entered_tx, b_entered_rx) = oneshot::channel();
    let (a_finished_tx, a_finished_rx) = oneshot::channel();
    let branch_a = async {
        let result = run_with_operation(
            &a.scope,
            OperationMetaV2::default(),
            MutationClass::LibraStateMutation,
            move |_| async move {
                a_entered_tx
                    .send(())
                    .map_err(|_| OperationError::Mutation("B stopped".into()))?;
                b_entered_rx
                    .await
                    .map_err(|_| OperationError::Mutation("B did not enter".into()))?;
                write_request_layer("source-A").await
            },
        )
        .await;
        let _ = a_finished_tx.send(());
        result
    };
    let branch_b = async {
        a_entered_rx
            .await
            .map_err(|_| OperationError::Mutation("A did not enter".into()))?;
        run_with_operation(
            &b.scope,
            OperationMetaV2::default(),
            MutationClass::LibraStateMutation,
            move |_| async move {
                b_entered_tx
                    .send(())
                    .map_err(|_| OperationError::Mutation("A stopped".into()))?;
                a_finished_rx
                    .await
                    .map_err(|_| OperationError::Mutation("A did not finish".into()))?;
                write_request_layer("source-B").await
            },
        )
        .await
    };
    let (a_result, b_result) =
        tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(branch_a, branch_b) })
            .await
            .expect("successful operation handshakes must finish");
    assert!(
        a_result.is_ok() && b_result.is_ok(),
        "A: {a_result:?}; B: {b_result:?}"
    );
    assert_eq!(WorktreeScope::request_scope(), Some(fallback.scope.clone()));
    assert_published(&a.scope, "source-A", &a_result.expect("A success")).await;
    assert_published(&b.scope, "source-B", &b_result.expect("B success")).await;
    let fallback_db = get_db_conn_instance_for_path(&fallback.scope.storage.join("libra.db"))
        .await
        .expect("explicit fallback connection");
    let counts = fallback_db
        .query_one_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT (SELECT COUNT(*) FROM layer), (SELECT COUNT(*) FROM operation), \
             (SELECT COUNT(*) FROM operation_head)",
        ))
        .await
        .expect("fallback table counts")
        .expect("one count row");
    for column in 0..3 {
        assert_eq!(
            counts.try_get_by_index::<i64>(column).expect("table count"),
            0
        );
    }
    assert!(!WorkspaceStatePointer::path(&fallback.scope).exists());
}
