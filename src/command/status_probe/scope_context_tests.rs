//! Request-context propagation across both pooled-worker enqueue paths.

use std::{fs, time::Duration};

use serial_test::serial;

use super::{with_io_deadline_bounded, with_io_deadline_detached};
use crate::internal::{
    operation::{
        OperationMetaV2,
        middleware::{MutationClass, OperationError, run_with_operation},
    },
    worktree_scope::{RequestScope, WorktreeScope},
};

const WORKER_TIMEOUT: Duration = Duration::from_secs(2);

fn fixture() -> (tempfile::TempDir, RequestScope) {
    let directory = tempfile::tempdir().expect("request root");
    let workdir = directory.path().canonicalize().expect("canonical root");
    let gitdir = workdir.join(".libra");
    fs::create_dir_all(gitdir.join("objects")).expect("object directory");
    fs::write(gitdir.join("HEAD"), b"ref: refs/heads/main\n").expect("HEAD");
    fs::write(gitdir.join("libra.db"), b"").expect("repository marker");
    let scope = RequestScope::resolve(workdir).expect("resolved request");
    (directory, scope)
}

fn run_worker<T: Send + 'static>(
    detached: bool,
    operation: impl FnOnce() -> T + Send + 'static,
) -> T {
    if detached {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        with_io_deadline_detached(move || {
            let _ = tx.send(operation());
        })
        .expect("submit pooled job");
        rx.recv_timeout(WORKER_TIMEOUT).expect("pooled job result")
    } else {
        with_io_deadline_bounded(WORKER_TIMEOUT, operation).expect("bounded pooled job result")
    }
}

fn observe_worker(detached: bool) -> Option<RequestScope> {
    run_worker(detached, WorktreeScope::request_scope)
}

async fn assert_worker_context(detached: bool) {
    let (_baseline_directory, baseline) = fixture();
    let (_request_directory, request) = fixture();
    let _baseline_pin = WorktreeScope::pin_request_scope(baseline.workdir.clone());

    // Given a caller-local request over a different live synchronous fallback.
    let result = run_with_operation(
        &request,
        OperationMetaV2::default(),
        MutationClass::ReadOnly,
        |_| async {
            let bound = observe_worker(detached);
            let unbound = {
                let _unpinned = WorktreeScope::unpinned();
                observe_worker(detached)
            };
            let restored = observe_worker(detached);
            Ok::<_, OperationError>((bound, unbound, restored))
        },
    )
    .await
    .expect("read-only operation");

    // Then both Some and explicit None cross the thread boundary, without leaking a pin.
    assert_eq!(WorktreeScope::request_scope(), Some(baseline));
    assert_eq!(result.value, (Some(request.clone()), None, Some(request)));
}

#[tokio::test]
#[serial]
async fn operation_scope_bounded_status_jobs_inherit_scope_and_explicit_none() {
    assert_worker_context(false).await;
}

#[tokio::test]
#[serial]
async fn operation_scope_detached_status_jobs_inherit_scope_and_explicit_none() {
    assert_worker_context(true).await;
}

async fn assert_worker_owns_captured_value(detached: bool) {
    let (_baseline_directory, baseline) = fixture();
    let (_request_directory, request) = fixture();
    let (_override_directory, override_target) = fixture();
    let _baseline_pin = WorktreeScope::pin_request_scope(baseline.workdir.clone());
    let result = run_with_operation(
        &request,
        OperationMetaV2::default(),
        MutationClass::ReadOnly,
        |_| async {
            let guard = WorktreeScope::override_scope(override_target.workdir.clone());
            let worker = run_worker(detached, move || {
                // Restore the parent's slot AFTER enqueue, from the worker's own slot.
                drop(guard);
                WorktreeScope::request_scope()
            });
            Ok::<_, OperationError>((worker, WorktreeScope::request_scope()))
        },
    )
    .await
    .expect("read-only operation");
    assert_eq!(result.value, (Some(override_target), Some(request)));
    assert_eq!(WorktreeScope::request_scope(), Some(baseline));
}

#[tokio::test]
#[serial]
async fn operation_scope_bounded_status_worker_uses_a_value_not_parent_slot() {
    assert_worker_owns_captured_value(false).await;
}

#[tokio::test]
#[serial]
async fn operation_scope_detached_status_worker_uses_a_value_not_parent_slot() {
    assert_worker_owns_captured_value(true).await;
}
