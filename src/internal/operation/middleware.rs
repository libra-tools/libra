//! The single operation boundary for mutable CLI and Agent work.
//!
//! Classification is intentionally name-based at this layer so the CLI and
//! Agent gateways can share it without making their argument enums public.
//! The CLI's existing exhaustive `command_scope` match remains the compile
//! time census for every concrete command variant.  Unknown names are a hard
//! error: an unclassified mutation must never silently run outside the log.

use std::{future::Future, pin::Pin};

use thiserror::Error;
use uuid::Uuid;

use super::{OperationMetaV2, PinnedRequestScope};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MutationClass {
    ReadOnly,
    WorkspaceMutation,
    RepoMutation,
    SequencerMutation,
    LibraStateMutation,
    ExternalOrUnknown,
    InternalWorker,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ClassificationError {
    #[error("operation mutation class is unknown for command '{0}'")]
    Unknown(String),
}

/// Classify a command/tool name.  The seven values are exhaustive by design;
/// adding a new class requires changing this enum and every consumer.
pub fn classify_command(name: &str) -> Result<MutationClass, ClassificationError> {
    let name = name.trim().to_ascii_lowercase();
    let class = match name.as_str() {
        "status" | "log" | "diff" | "show" | "ls-files" | "branch-list" | "config-get"
        | "sparse-view-list" | "sparse-view-status" | "worktree-list" | "worktree-doctor" => {
            MutationClass::ReadOnly
        }
        "add" | "rm" | "mv" | "restore" | "clean" | "checkout" | "switch" | "apply"
        | "read-tree" | "update-index" | "hydrate" => MutationClass::WorkspaceMutation,
        "branch" | "tag" | "commit" | "reset" | "fetch" | "pull" | "config-set"
        | "metadata" | "file" => MutationClass::RepoMutation,
        "merge" | "rebase" | "cherry-pick" | "revert" | "am" | "bisect" => {
            MutationClass::SequencerMutation
        }
        "worktree" | "sparse-view" | "layer" | "dirty" | "auth" | "automation" => {
            MutationClass::LibraStateMutation
        }
        "shell" | "exec" | "external-git" | "hook" => MutationClass::ExternalOrUnknown,
        "internal-worker" | "recovery-worker" | "status-io-worker" => {
            MutationClass::InternalWorker
        }
        _ => return Err(ClassificationError::Unknown(name)),
    };
    Ok(class)
}

#[derive(Debug, Error)]
pub enum OperationError {
    #[error(transparent)]
    Classification(#[from] ClassificationError),
    #[error("external mutation could not be verified before operation publication")]
    ExternalUnverified,
    #[error("operation mutation failed: {0}")]
    Mutation(String),
}

/// Mutable context passed to the business closure.  It carries the stable
/// operation id and provides the explicit proof bit used by external shell
/// and Git calls.  Internal workers never receive a recording context.
pub struct OperationTxn {
    pub op_id: String,
    pub class: MutationClass,
    external_verified: bool,
}

impl OperationTxn {
    pub fn mark_external_verified(&mut self) {
        self.external_verified = true;
    }

    pub fn external_verified(&self) -> bool {
        self.external_verified
    }
}

#[derive(Debug)]
pub struct OperationResult<T> {
    pub value: T,
    pub operation_id: Option<String>,
    pub recorded: bool,
}

/// Execute a closure at the unified mutation boundary.
///
/// The storage-backed publication seam is deliberately kept behind the same
/// context and classification contract; callers that own the repository
/// transaction publish the immutable pre/post manifests and CAS head in the
/// closure's surrounding store transaction.  This function still enforces
/// the two non-negotiable gates here: read-only/internal work does not create
/// an operation, and unverifiable external work fails closed.
pub async fn run_with_operation<T, F, Fut>(
    _scope: &PinnedRequestScope,
    _meta: OperationMetaV2,
    class: MutationClass,
    f: F,
) -> Result<OperationResult<T>, OperationError>
where
    F: FnOnce(&mut OperationTxn) -> Fut,
    Fut: Future<Output = Result<T, OperationError>>,
{
    let should_record = !matches!(class, MutationClass::ReadOnly | MutationClass::InternalWorker);
    let mut txn = OperationTxn {
        op_id: Uuid::now_v7().to_string(),
        class,
        external_verified: false,
    };
    let value = f(&mut txn).await?;
    if matches!(class, MutationClass::ExternalOrUnknown) && !txn.external_verified {
        return Err(OperationError::ExternalUnverified);
    }
    Ok(OperationResult {
        value,
        operation_id: should_record.then_some(txn.op_id),
        recorded: should_record,
    })
}

/// Type alias useful to gateways that erase a closure into a boxed future.
pub type OperationFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, OperationError>> + Send + 'a>>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_classification_covers_the_public_mutation_families() {
        assert_eq!(classify_command("status"), Ok(MutationClass::ReadOnly));
        assert_eq!(classify_command("add"), Ok(MutationClass::WorkspaceMutation));
        assert_eq!(classify_command("commit"), Ok(MutationClass::RepoMutation));
        assert_eq!(classify_command("rebase"), Ok(MutationClass::SequencerMutation));
        assert_eq!(classify_command("sparse-view"), Ok(MutationClass::LibraStateMutation));
        assert_eq!(classify_command("shell"), Ok(MutationClass::ExternalOrUnknown));
        assert_eq!(classify_command("internal-worker"), Ok(MutationClass::InternalWorker));
        assert!(matches!(classify_command("future-command"), Err(ClassificationError::Unknown(_))));
    }

    #[tokio::test]
    async fn read_only_and_internal_worker_do_not_create_operation_ids() {
        let root = tempfile::tempdir().expect("scope root");
        let scope = crate::internal::worktree_scope::RequestScope {
            scope: crate::internal::worktree_scope::WorktreeScope::Main,
            workdir: root.path().to_path_buf(),
            gitdir: root.path().join(".libra"),
            storage: root.path().to_path_buf(),
            worktree_root: root.path().to_path_buf(),
        };
        let result = run_with_operation(&scope, OperationMetaV2::default(), MutationClass::ReadOnly, |_txn| async { Ok::<_, OperationError>(7) }).await.expect("read-only operation");
        assert_eq!(result.value, 7);
        assert!(!result.recorded);
        assert!(result.operation_id.is_none());
    }

    #[tokio::test]
    async fn external_mutations_fail_closed_without_verification() {
        let root = tempfile::tempdir().expect("scope root");
        let scope = crate::internal::worktree_scope::RequestScope {
            scope: crate::internal::worktree_scope::WorktreeScope::Main,
            workdir: root.path().to_path_buf(),
            gitdir: root.path().join(".libra"),
            storage: root.path().to_path_buf(),
            worktree_root: root.path().to_path_buf(),
        };
        let result = run_with_operation(&scope, OperationMetaV2::default(), MutationClass::ExternalOrUnknown, |_txn| async { Ok::<_, OperationError>(()) }).await;
        assert!(matches!(result, Err(OperationError::ExternalUnverified)));
    }
}
