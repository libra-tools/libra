//! Real operation callbacks must respect physical-worktree exclusion.

use std::fs;

use super::{
    MutationClass, OperationError, OperationMetaV2, run_with_operation,
    support::{Fixture, busy_error, journal_count},
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
