//! Deterministic non-LIFO operation overlap, without scheduler timing assumptions.

use std::sync::{Arc, Mutex};

use tokio::sync::oneshot;

use super::*;

#[tokio::test]
#[serial]
async fn operation_scope_overlapping_mutations_keep_live_and_final_bindings_isolated() {
    // Given two real repositories and an otherwise unchanged ambient scope.
    let a = fixture(true).await;
    let b = fixture(true).await;
    let fallback = fixture(false).await;
    let _fallback_pin = WorktreeScope::pin_request_scope(fallback.scope.workdir.clone());
    let baseline = WorktreeScope::request_scope();
    let observations = Arc::new(Mutex::new(Vec::new()));
    let a_observations = Arc::clone(&observations);
    let b_observations = Arc::clone(&observations);
    let (a_entered_tx, a_entered_rx) = oneshot::channel();
    let (b_entered_tx, b_entered_rx) = oneshot::channel();
    let (a_finished_tx, a_finished_rx) = oneshot::channel();

    // When A binds, B binds, A finishes first, and only then B finishes.
    let branch_a = async {
        let result = run_with_operation(
            &a.scope,
            OperationMetaV2::default(),
            MutationClass::WorkspaceMutation,
            move |_| async move {
                a_entered_tx.send(()).map_err(|_| {
                    OperationError::Mutation("scope handshake: B stopped before A entered".into())
                })?;
                b_entered_rx.await.map_err(|_| {
                    OperationError::Mutation("scope handshake: B never reached its callback".into())
                })?;
                a_observations
                    .lock()
                    .expect("observations")
                    .push(("A while B is live", WorktreeScope::request_scope()));
                Err::<(), _>(OperationError::Mutation("scope-test-A".into()))
            },
        )
        .await;
        let _ = a_finished_tx.send(());
        result
    };
    let branch_b = async {
        a_entered_rx.await.map_err(|_| {
            OperationError::Mutation("scope handshake: A never reached its callback".into())
        })?;
        run_with_operation(
            &b.scope,
            OperationMetaV2::default(),
            MutationClass::WorkspaceMutation,
            move |_| async move {
                b_entered_tx.send(()).map_err(|_| {
                    OperationError::Mutation("scope handshake: A stopped before B entered".into())
                })?;
                a_finished_rx.await.map_err(|_| {
                    OperationError::Mutation("scope handshake: A did not return its result".into())
                })?;
                b_observations
                    .lock()
                    .expect("observations")
                    .push(("B after A finished", WorktreeScope::request_scope()));
                Err::<(), _>(OperationError::Mutation("scope-test-B".into()))
            },
        )
        .await
    };
    let (a_result, b_result) =
        tokio::time::timeout(TEST_TIMEOUT, async { tokio::join!(branch_a, branch_b) })
            .await
            .expect("operation handshakes must finish");
    let final_scope = WorktreeScope::request_scope();
    let observed = observations.lock().expect("observations").clone();

    // Then neither an active callback nor the next caller sees another request's scope.
    assert!(
        matches!(&a_result, Err(OperationError::Mutation(message)) if message == "scope-test-A")
            && matches!(&b_result, Err(OperationError::Mutation(message)) if message == "scope-test-B"),
        "both callbacks must reach the observation barrier; A returned {a_result:?}; B returned {b_result:?}"
    );
    assert_eq!(
        (observed, final_scope),
        (
            vec![
                ("A while B is live", Some(a.scope.clone())),
                ("B after A finished", Some(b.scope.clone())),
            ],
            baseline,
        )
    );
}
