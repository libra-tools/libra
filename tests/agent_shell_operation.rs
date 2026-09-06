//! OL-09 Agent gateway guard: shell is external and must prove verification.

use libra::internal::operation::{run_with_operation, MutationClass, OperationError, OperationMetaV2};
use libra::internal::worktree_scope::{RequestScope, WorktreeScope};

fn scope(root: &std::path::Path) -> RequestScope {
    RequestScope { scope: WorktreeScope::Main, workdir: root.to_path_buf(), gitdir: root.join(".libra"), storage: root.to_path_buf(), worktree_root: root.to_path_buf() }
}

#[tokio::test]
async fn unverified_shell_is_rejected_and_verified_shell_records_id() {
    let root = tempfile::tempdir().unwrap();
    let rejected = run_with_operation(&scope(root.path()), OperationMetaV2::default(), MutationClass::ExternalOrUnknown, |_txn| async { Ok::<_, OperationError>(()) }).await;
    assert!(matches!(rejected, Err(OperationError::ExternalUnverified)));
    let accepted = run_with_operation(&scope(root.path()), OperationMetaV2::default(), MutationClass::ExternalOrUnknown, |txn| { txn.mark_external_verified(); async { Ok::<_, OperationError>(()) } }).await.unwrap();
    assert!(accepted.recorded);
    assert!(accepted.operation_id.is_some());
}
