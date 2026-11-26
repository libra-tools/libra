//! Nested synchronous overrides and cancellation within request futures.

use tokio::sync::oneshot;

use super::*;

#[tokio::test]
#[serial]
async fn operation_scope_nested_override_and_unpinned_restore_the_enclosing_request() {
    let baseline = fixture(false).await;
    let outer = fixture(false).await;
    let inner = fixture(false).await;
    let _baseline_pin = WorktreeScope::pin_request_scope(baseline.scope.workdir.clone());

    // Given an explicit request over a different, still-live synchronous fallback.
    let result = run_with_operation(
        &outer.scope,
        OperationMetaV2::default(),
        MutationClass::ReadOnly,
        |_| async {
            let mut seen = vec![WorktreeScope::request_scope()];
            {
                let _override = WorktreeScope::override_scope(inner.scope.workdir.clone());
                seen.push(WorktreeScope::request_scope());
                {
                    let _unpinned = WorktreeScope::unpinned();
                    seen.push(WorktreeScope::request_scope());
                }
                seen.push(WorktreeScope::request_scope());
            }
            seen.push(WorktreeScope::request_scope());
            Ok::<_, OperationError>(seen)
        },
    )
    .await
    .expect("nested request");

    // Then explicit None masks the fallback, and both override levels restore exactly.
    assert_eq!(WorktreeScope::request_scope(), Some(baseline.scope.clone()));
    assert_eq!(
        result.value,
        vec![
            Some(outer.scope.clone()),
            Some(inner.scope.clone()),
            None,
            Some(inner.scope.clone()),
            Some(outer.scope.clone()),
        ]
    );
}

#[tokio::test]
#[serial]
async fn operation_scope_cancelled_nested_override_restores_parent_and_fallback() {
    let baseline = fixture(false).await;
    let outer = fixture(false).await;
    let inner = fixture(false).await;
    let override_target = fixture(false).await;
    let override_workdir = override_target.scope.workdir.clone();
    let _baseline_pin = WorktreeScope::pin_request_scope(baseline.scope.workdir.clone());

    // Given a nested operation parked while its synchronous override is live.
    let result = tokio::time::timeout(
        TEST_TIMEOUT,
        run_with_operation(
            &outer.scope,
            OperationMetaV2::default(),
            MutationClass::ReadOnly,
            |_| async {
                let (entered_tx, entered_rx) = oneshot::channel();
                let mut nested = Box::pin(run_with_operation(
                    &inner.scope,
                    OperationMetaV2::default(),
                    MutationClass::ReadOnly,
                    |_| async move {
                        let _override = WorktreeScope::override_scope(override_workdir);
                        entered_tx
                            .send(WorktreeScope::request_scope())
                            .expect("announce live override");
                        std::future::pending::<Result<(), OperationError>>().await
                    },
                ));
                let during = tokio::select! {
                    result = nested.as_mut() => panic!("nested operation completed: {result:?}"),
                    observed = entered_rx => observed.expect("nested callback entered"),
                };
                // When the nested future is cancelled, its guard must restore its own slot.
                drop(nested);
                Ok::<_, OperationError>((during, WorktreeScope::request_scope()))
            },
        ),
    )
    .await
    .expect("nested cancellation must finish")
    .expect("outer request");

    // Then the enclosing request and the caller's fallback remain separate.
    assert_eq!(WorktreeScope::request_scope(), Some(baseline.scope.clone()));
    assert_eq!(result.value.0, Some(override_target.scope.clone()));
    assert_eq!(result.value.1, Some(outer.scope.clone()));
}

#[tokio::test]
#[serial]
async fn operation_scope_guard_dropped_inside_another_request_restores_its_original_slot() {
    let outer = fixture(false).await;
    let inner = fixture(false).await;
    let override_target = fixture(false).await;
    let baseline = WorktreeScope::request_scope();
    let result = run_with_operation(
        &outer.scope,
        OperationMetaV2::default(),
        MutationClass::ReadOnly,
        |_| async {
            let guard = WorktreeScope::override_scope(override_target.scope.workdir.clone());
            let nested = run_with_operation(
                &inner.scope,
                OperationMetaV2::default(),
                MutationClass::ReadOnly,
                |_| async move {
                    drop(guard);
                    Ok::<_, OperationError>(WorktreeScope::request_scope())
                },
            )
            .await?;
            Ok::<_, OperationError>((nested.value, WorktreeScope::request_scope()))
        },
    )
    .await
    .expect("nested request");
    assert_eq!(
        result.value,
        (Some(inner.scope.clone()), Some(outer.scope.clone()))
    );
    assert_eq!(WorktreeScope::request_scope(), baseline);
}

#[tokio::test]
#[serial]
async fn operation_scope_cancelled_outer_request_preserves_the_synchronous_fallback() {
    let fallback = fixture(false).await;
    let request = fixture(false).await;
    let override_target = fixture(false).await;
    let _fallback_pin = WorktreeScope::pin_request_scope(fallback.scope.workdir.clone());
    let (entered_tx, entered_rx) = oneshot::channel();
    let mut operation = Box::pin(run_with_operation(
        &request.scope,
        OperationMetaV2::default(),
        MutationClass::ReadOnly,
        |_| async {
            let _guard = WorktreeScope::override_scope(override_target.scope.workdir.clone());
            entered_tx
                .send(WorktreeScope::request_scope())
                .expect("announce override");
            std::future::pending::<Result<(), OperationError>>().await
        },
    ));
    let observed = tokio::time::timeout(TEST_TIMEOUT, async {
        tokio::select! {
            result = operation.as_mut() => panic!("operation completed: {result:?}"),
            observed = entered_rx => observed.expect("callback entered"),
        }
    })
    .await
    .expect("callback must enter");
    drop(operation);
    assert_eq!(observed, Some(override_target.scope.clone()));
    assert_eq!(WorktreeScope::request_scope(), Some(fallback.scope.clone()));
}
