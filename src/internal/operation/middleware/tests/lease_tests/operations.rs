//! Real operation callbacks must respect physical-worktree exclusion.

use std::fs;

use super::{
    MutationClass, OperationError, OperationMetaV2, ScopeLease, run_with_operation,
    support::{Fixture, REPO_ID, busy_error, journal_count},
};

pub(super) async fn same_scope(fixture: &Fixture) {
    // Given a valid first operation that has reached its business callback.
    let marker = fixture.root.join("second-callback");
    let scope = &fixture.main;
    let outer = run_with_operation(
        scope,
        OperationMetaV2::default(),
        MutationClass::WorkspaceMutation,
        |_| async {
            let before = journal_count(scope).await;
            fixture.ready();
            // When a second operation tries the very same physical scope.
            let second = run_with_operation(
                scope,
                OperationMetaV2::default(),
                MutationClass::WorkspaceMutation,
                |_| async {
                    fs::write(&marker, b"entered").expect("second callback marker");
                    Ok::<_, OperationError>(())
                },
            )
            .await;
            // Then it refuses before callback or journal reservation.
            assert!(
                !marker.exists(),
                "second callback ran while first lease was held"
            );
            let error = second.expect_err("contended operation must refuse");
            busy_error(&error, scope);
            assert_eq!(
                journal_count(scope).await,
                before,
                "refused operation reserved a journal"
            );
            Err::<(), _>(OperationError::Mutation("outer callback completed".into()))
        },
    )
    .await;
    assert!(
        matches!(outer, Err(OperationError::Mutation(ref text)) if text == "outer callback completed"),
        "first operation did not reach the real callback: {outer:?}"
    );
}

pub(super) async fn different_scope(fixture: &Fixture) {
    // Given main and linked physical scopes sharing one repository database.
    let linked = fixture.linked_scope().await;
    assert_eq!(fixture.main.storage, linked.storage);
    assert_ne!(fixture.main.gitdir, linked.gitdir);
    let marker = linked.workdir.join("linked-callback");
    let outer = run_with_operation(
        &fixture.main,
        OperationMetaV2::default(),
        MutationClass::WorkspaceMutation,
        |_| async {
            fixture.ready();
            // When linked work starts while main's real operation still owns its lease.
            let second = run_with_operation(
                &linked,
                OperationMetaV2::default(),
                MutationClass::WorkspaceMutation,
                |_| async {
                    fs::write(&marker, b"linked entered").expect("linked callback marker");
                    Err::<(), _>(OperationError::Mutation("linked callback completed".into()))
                },
            )
            .await;
            // Then the second callback completes independently; a repository-wide lock is wrong.
            assert!(
                matches!(second, Err(OperationError::Mutation(ref text)) if text == "linked callback completed"),
                "independent linked scope never reached callback: {second:?}"
            );
            assert_eq!(fs::read(&marker).expect("linked callback evidence"), b"linked entered");
            Err::<(), _>(OperationError::Mutation("main callback completed".into()))
        },
    )
    .await;
    assert!(
        matches!(outer, Err(OperationError::Mutation(ref text)) if text == "main callback completed"),
        "main operation did not reach the real callback: {outer:?}"
    );
}

pub(super) async fn cancelled_operation(fixture: &Fixture) {
    // Given a real operation that acknowledges entry while holding its lease.
    let (entered, observed) = tokio::sync::oneshot::channel();
    let mut operation = Box::pin(run_with_operation(
        &fixture.main,
        OperationMetaV2::default(),
        MutationClass::WorkspaceMutation,
        |_| async move {
            entered.send(()).expect("callback acknowledgement");
            std::future::pending::<Result<(), OperationError>>().await
        },
    ));
    tokio::select! {
        result = observed => result.expect("operation entered callback"),
        result = &mut operation => panic!("operation returned before cancellation: {result:?}"),
    }
    fixture.ready();
    // When the owning future is dropped, not merely its Pin reference.
    drop(operation);
    // Then another independent handle can acquire the same scope.
    let lease = super::ScopeLease::acquire(&fixture.main, super::support::REPO_ID)
        .await
        .expect("cancelled operation released scope");
    drop(lease);
}

/// A repository-wide restore lease fences ref writers in every linked
/// worktree. The central command boundary, legacy ref transaction wrapper,
/// and long-lived sequencer boundary all refuse before publishing work.
#[tokio::test]
async fn repository_fence_blocks_linked_worktree_ref_writers() {
    use crate::internal::{
        config::ConfigKv,
        db::{create_database, get_db_conn_instance_for_path},
        operation::PinnedRequestScope,
        operation_wrapper::{
            OperationMeta, OperationScope, begin_operation_with_conn, with_operation_log_with_conn,
        },
        worktree_scope::{WorktreeScope, with_request_scope},
    };

    let temp = tempfile::tempdir().expect("repository sandbox");
    let main_root = temp.path().join("main");
    let storage = main_root.join(".libra");
    let linked_root = temp.path().join("linked");
    let linked_gitdir = linked_root.join(".libra");
    fs::create_dir_all(&storage).expect("main common storage");
    fs::create_dir_all(&linked_gitdir).expect("linked gitdir");

    let main = PinnedRequestScope {
        scope: WorktreeScope::Main,
        workdir: main_root.clone(),
        gitdir: storage.clone(),
        storage: storage.clone(),
        worktree_root: main_root,
    };
    let linked = PinnedRequestScope {
        scope: WorktreeScope::Linked("wt-fence-test".to_string()),
        workdir: linked_root.clone(),
        gitdir: linked_gitdir,
        storage: storage.clone(),
        worktree_root: linked_root.clone(),
    };

    let setup = create_database(storage.join("libra.db").to_str().expect("database path"))
        .await
        .expect("repository schema");
    ConfigKv::set_with_conn(&setup, "libra.repoid", REPO_ID, false)
        .await
        .expect("repository identity");
    setup.close().await.expect("close setup connection");

    // Main's restore holds the common fence while a linked worktree tries to
    // commit. The callback must not run, so the branch update cannot be lost.
    let _restore_fence = ScopeLease::acquire_repository(&main, REPO_ID, None)
        .await
        .expect("restore repository fence");
    let commit_marker = linked_root.join("commit-callback-ran");
    let commit_result = crate::internal::operation::run_with_operation(
        &linked,
        OperationMetaV2 {
            command_name: Some("commit".to_string()),
            ..OperationMetaV2::default()
        },
        MutationClass::RepoMutation,
        {
            let commit_marker = commit_marker.clone();
            move |_| async move {
                fs::write(&commit_marker, b"commit entered").expect("commit marker");
                Ok::<_, OperationError>(())
            }
        },
    )
    .await;
    assert!(
        matches!(commit_result, Err(OperationError::LeaseBusy { .. })),
        "linked commit must refuse at the repository fence: {commit_result:?}"
    );
    assert!(
        !commit_marker.exists(),
        "commit callback ran under the fence"
    );

    // Branch ref updates use the legacy SQL wrapper and must take the same
    // fence before opening their write transaction.
    let legacy_marker = linked_root.join("legacy-callback-ran");
    let legacy_db = get_db_conn_instance_for_path(&storage.join("libra.db"))
        .await
        .expect("legacy repository connection");
    let legacy_result = with_request_scope(Some(linked.clone()), async {
        with_operation_log_with_conn(
            &legacy_db,
            OperationMeta {
                command_name: "branch".to_string(),
                description: "update a branch ref".to_string(),
                actor: "test".to_string(),
                repo_id: REPO_ID.to_string(),
                args_digest: None,
            },
            OperationScope::default(),
            {
                let legacy_marker = legacy_marker.clone();
                move |_| {
                    Box::pin(async move {
                        fs::write(&legacy_marker, b"legacy entered").expect("legacy marker");
                        Ok::<_, sea_orm::DbErr>(())
                    })
                }
            },
        )
        .await
    })
    .await;
    assert!(
        matches!(legacy_result, Err(crate::internal::operation_wrapper::OperationError::Begin(ref message)) if message.contains("already held")),
        "legacy ref writer must refuse at the repository fence: {legacy_result:?}"
    );
    assert!(
        !legacy_marker.exists(),
        "legacy callback ran under the fence"
    );

    // Sequencer controls omit refs from their audit snapshot but still move
    // branch/HEAD refs; their boundary must retain the same repository fence.
    let sequencer_result = with_request_scope(Some(linked), async {
        begin_operation_with_conn(
            &legacy_db,
            OperationMeta {
                command_name: "rebase --continue".to_string(),
                description: "continue rebase".to_string(),
                actor: "test".to_string(),
                repo_id: REPO_ID.to_string(),
                args_digest: None,
            },
            OperationScope {
                include_refs: false,
                include_workspace: false,
                duplicate_window: false,
                ..OperationScope::default()
            },
        )
        .await
    })
    .await;
    assert!(
        matches!(sequencer_result, Err(crate::internal::operation_wrapper::OperationError::Begin(message)) if message.contains("already held")),
        "sequencer boundary must refuse at the repository fence"
    );
}
