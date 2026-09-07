//! OL-09 Agent gateway guard: shell is external and requires a repository
//! snapshot boundary.

use libra::internal::{
    operation::{MutationClass, OperationError, OperationMetaV2, run_with_operation},
    worktree_scope::{RequestScope, WorktreeScope},
};

fn scope(root: &std::path::Path) -> RequestScope {
    RequestScope {
        scope: WorktreeScope::Main,
        workdir: root.to_path_buf(),
        gitdir: root.join(".libra"),
        storage: root.to_path_buf(),
        worktree_root: root.to_path_buf(),
    }
}

#[tokio::test]
async fn external_shell_outside_repository_is_rejected_before_execution() {
    let root = tempfile::tempdir().unwrap();
    let rejected = run_with_operation(
        &scope(root.path()),
        OperationMetaV2::default(),
        MutationClass::ExternalOrUnknown,
        |_txn| async { Ok::<_, OperationError>(()) },
    )
    .await;
    assert!(matches!(rejected, Err(OperationError::ExternalUnverified)));
}
