//! The single operation boundary for mutable CLI and Agent work.
//!
//! Classification is intentionally name-based at this layer so the CLI and
//! Agent gateways can share it without making their argument enums public.
//! The CLI's existing exhaustive `command_scope` match remains the compile
//! time census for every concrete command variant.  Unknown names are a hard
//! error: an unclassified mutation must never silently run outside the log.

use std::{
    future::Future,
    pin::Pin,
    time::{SystemTime, UNIX_EPOCH},
};

use git_internal::{hash::ObjectHash, internal::object::types::ObjectType};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
use serde_json::json;
use thiserror::Error;
use uuid::Uuid;

pub(crate) const REPOSITORY_REF_LEASE_HELD_ENV: &str = "LIBRA_INTERNAL_REPOSITORY_REF_LEASE_HELD";

tokio::task_local! {
    static CURRENT_OPERATION_ID: String;
    static CURRENT_REPOSITORY_REF_LEASE: ();
}

/// Return the operation id active for the current async command.
pub(crate) fn current_operation_id() -> Option<String> {
    CURRENT_OPERATION_ID.try_with(Clone::clone).ok()
}

/// Run a future with the operation id of an existing persisted boundary.
pub(crate) async fn with_operation_id<T>(
    operation_id: String,
    future: impl Future<Output = T>,
) -> T {
    CURRENT_OPERATION_ID.scope(operation_id, future).await
}

pub(crate) fn repository_ref_lease_is_held() -> bool {
    CURRENT_REPOSITORY_REF_LEASE.try_with(|_| ()).is_ok()
        || std::env::var_os(REPOSITORY_REF_LEASE_HELD_ENV).is_some_and(|value| value == "1")
}

pub(crate) async fn with_repository_ref_lease<T>(future: impl Future<Output = T>) -> T {
    CURRENT_REPOSITORY_REF_LEASE.scope((), future).await
}

mod lease;
use lease::LeaseFilePermissions;
pub(crate) use lease::ScopeLease;

use super::{
    Completeness, JournalEntry, JournalPhase, OperationKind, OperationMetaV2, OperationStatusV2,
    OperationStoreV2, OperationV2, PinnedRequestScope, RepoViewV2, SnapshotError, Staleness,
    WorkspaceSnapshotter, WorkspaceStatePointer,
};
use crate::{
    internal::{config::ConfigKv, db::get_db_conn_instance_for_path, workspace::RepoIdentity},
    utils::{
        client_storage::ClientStorage,
        error::{CliError, StableErrorCode},
        util::DATABASE,
    },
};

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
        "branch" | "tag" | "commit" | "reset" | "fetch" | "pull" | "config-set" | "metadata"
        | "file" => MutationClass::RepoMutation,
        "merge" | "rebase" | "cherry-pick" | "revert" | "am" | "bisect" => {
            MutationClass::SequencerMutation
        }
        "worktree" | "sparse-view" | "layer" | "dirty" | "auth" | "automation" => {
            MutationClass::LibraStateMutation
        }
        "shell" | "exec" | "external-git" | "hook" => MutationClass::ExternalOrUnknown,
        "internal-worker" | "recovery-worker" | "status-io-worker" => MutationClass::InternalWorker,
        _ => return Err(ClassificationError::Unknown(name)),
    };
    Ok(class)
}

/// Whether a command can change a ref shared by linked worktrees. The first
/// token is used because legacy operation records include control arguments
/// (for example, `rebase --continue`) in `command_name`.
pub(crate) fn command_may_mutate_shared_refs(command_name: &str) -> bool {
    let normalized = command_name.trim().to_ascii_lowercase();
    let mut parts = normalized.split_ascii_whitespace();
    let command = parts.next().unwrap_or_default();
    if command == "op" {
        return matches!(parts.next(), Some("restore" | "undo" | "redo" | "revert"));
    }
    matches!(
        command,
        "branch"
            | "br"
            | "tag"
            | "commit"
            | "ci"
            | "reset"
            | "fetch"
            | "pull"
            | "push"
            | "merge"
            | "rebase"
            | "rb"
            | "cherry-pick"
            | "cp"
            | "revert"
            | "am"
            | "bisect"
            | "checkout"
            | "switch"
            | "sw"
            | "update-ref"
            | "symbolic-ref"
            | "reflog"
            | "notes"
            | "replace"
            | "stash"
            | "remote"
            | "worktree"
            | "shell"
            | "exec"
            | "external-git"
            | "hook"
            | "fast-import"
    )
}

fn operation_needs_repository_lease(meta: &OperationMetaV2, class: MutationClass) -> bool {
    if class == MutationClass::ExternalOrUnknown {
        return true;
    }
    let Some(command_name) = meta.command_name.as_deref() else {
        return matches!(
            class,
            MutationClass::RepoMutation | MutationClass::SequencerMutation
        );
    };
    let mut parts = command_name.split_ascii_whitespace();
    if parts.next() == Some("stash") && parts.next() == Some("pop") {
        // The stash entry is applied before its raw-line CAS. That critical
        // section takes the repository lease after the pop rendezvous, so two
        // worktrees can both apply and exactly one can win the shared-stack
        // delete.
        return false;
    }
    command_may_mutate_shared_refs(command_name)
}

#[derive(Debug, Error)]
pub enum OperationError {
    #[error(transparent)]
    Classification(#[from] ClassificationError),
    #[error("external mutation could not be verified before operation publication")]
    ExternalUnverified,
    #[error("operation mutation failed: {0}")]
    Mutation(String),
    #[error("operation storage failed: {0}")]
    Storage(String),
    #[error(
        "operation storage failed: operation scope lease is already held for {scope_key} at '{path}'; wait for the other operation to finish, then retry"
    )]
    LeaseBusy { scope_key: String, path: String },
    #[error("workspace operation pointer is stale: {0}")]
    Stale(String),
    #[error("operation publication compare-and-swap failed: {0}")]
    Cas(String),
    #[error(transparent)]
    Cli(#[from] CliError),
}

/// Mutable context passed to the business closure.  It carries the stable
/// operation id and provides the explicit proof bit used by external shell
/// and Git calls.  Internal workers never receive a recording context.
pub struct OperationTxn {
    pub op_id: String,
    pub class: MutationClass,
    post_snapshot_complete: bool,
}

impl OperationTxn {
    fn observe_post_snapshot(&mut self, complete: bool) {
        self.post_snapshot_complete = complete;
    }
}

#[derive(Debug)]
pub struct OperationResult<T> {
    pub value: T,
    pub operation_id: Option<String>,
    pub recorded: bool,
}

#[cfg(test)]
pub(crate) mod test_hooks {
    use std::sync::{Mutex, OnceLock};

    use super::{OperationError, OperationMetaV2};

    static PRE_LEASE_BUSY: OnceLock<Mutex<Option<String>>> = OnceLock::new();
    static POST_MUTATION_FAILURE: OnceLock<Mutex<Option<(String, String)>>> = OnceLock::new();

    fn pre_lease_busy() -> &'static Mutex<Option<String>> {
        PRE_LEASE_BUSY.get_or_init(|| Mutex::new(None))
    }

    fn post_mutation_failure() -> &'static Mutex<Option<(String, String)>> {
        POST_MUTATION_FAILURE.get_or_init(|| Mutex::new(None))
    }

    pub(crate) fn fail_next_lease_for_causal_context(causal_context_id: String) {
        if let Ok(mut slot) = pre_lease_busy().lock() {
            *slot = Some(causal_context_id);
        }
    }

    pub(crate) fn clear_pre_lease_busy() {
        if let Ok(mut slot) = pre_lease_busy().lock() {
            *slot = None;
        }
    }

    pub(crate) fn fail_after_mutation_for_causal_context(
        causal_context_id: String,
        reason: String,
    ) {
        if let Ok(mut slot) = post_mutation_failure().lock() {
            *slot = Some((causal_context_id, reason));
        }
    }

    pub(crate) fn clear_post_mutation_failure() {
        if let Ok(mut slot) = post_mutation_failure().lock() {
            *slot = None;
        }
    }

    pub(super) fn take_pre_lease_busy(meta: &OperationMetaV2) -> Option<OperationError> {
        let causal_context_id = meta.causal_context_id.as_deref()?;
        let mut slot = pre_lease_busy().lock().ok()?;
        if slot.as_deref() != Some(causal_context_id) {
            return None;
        }
        slot.take();
        Some(OperationError::LeaseBusy {
            scope_key: "test-scope".to_string(),
            path: "test-operation-v2.lock".to_string(),
        })
    }

    pub(super) fn take_post_mutation_failure(meta: &OperationMetaV2) -> Option<OperationError> {
        let causal_context_id = meta.causal_context_id.as_deref()?;
        let mut slot = post_mutation_failure().lock().ok()?;
        let (expected, _) = slot.as_ref()?;
        if expected != causal_context_id {
            return None;
        }
        let (_, reason) = slot.take()?;
        Some(OperationError::Storage(reason))
    }
}

/// Execute a closure at the unified mutation boundary.
///
/// Repository-backed scopes execute the complete v2 pipeline: scope lease,
/// pointer/head freshness check, pre-snapshot, journal reservation, business
/// mutation, post-snapshot, operation persistence, head CAS, and pointer
/// advancement.  The small in-memory fallback exists only for callers that
/// deliberately operate outside a repository (and keeps the classifier useful
/// to unit tests); a real pinned repository never takes that path.
pub async fn run_with_operation<T, F, Fut>(
    scope: &PinnedRequestScope,
    meta: OperationMetaV2,
    class: MutationClass,
    f: F,
) -> Result<OperationResult<T>, OperationError>
where
    F: FnOnce(&mut OperationTxn) -> Fut,
    Fut: Future<Output = Result<T, OperationError>>,
{
    crate::internal::worktree_scope::with_request_scope(Some(scope.clone()), async {
        if repository_scope_is_ready(scope) {
            return run_with_persistent_operation(scope, meta, class, f).await;
        }
        if class == MutationClass::ExternalOrUnknown {
            // Outside a pinned repository there is no bounded before/after
            // snapshot, so an external mutation must not be executed at all.
            return Err(OperationError::ExternalUnverified);
        }
        run_ephemeral_operation(class, f).await
    })
    .await
}

async fn run_ephemeral_operation<T, F, Fut>(
    class: MutationClass,
    f: F,
) -> Result<OperationResult<T>, OperationError>
where
    F: FnOnce(&mut OperationTxn) -> Fut,
    Fut: Future<Output = Result<T, OperationError>>,
{
    let should_record = !matches!(
        class,
        MutationClass::ReadOnly | MutationClass::InternalWorker
    );
    let mut txn = OperationTxn {
        op_id: Uuid::now_v7().to_string(),
        class,
        post_snapshot_complete: false,
    };
    let value = with_operation_id(txn.op_id.clone(), f(&mut txn)).await?;
    Ok(OperationResult {
        value,
        operation_id: should_record.then_some(txn.op_id),
        recorded: should_record,
    })
}

fn repository_scope_is_ready(scope: &PinnedRequestScope) -> bool {
    // A pinned gitdir is enough to identify a repository-backed mutation.  Do
    // not silently fall back to the ephemeral path when the database is
    // missing or damaged: that would let a real repository mutation bypass
    // the v2 journal and snapshot boundary.
    scope.gitdir.is_dir()
}

async fn run_with_persistent_operation<T, F, Fut>(
    scope: &PinnedRequestScope,
    meta: OperationMetaV2,
    class: MutationClass,
    f: F,
) -> Result<OperationResult<T>, OperationError>
where
    F: FnOnce(&mut OperationTxn) -> Fut,
    Fut: Future<Output = Result<T, OperationError>>,
{
    if matches!(
        class,
        MutationClass::ReadOnly | MutationClass::InternalWorker
    ) {
        return run_ephemeral_operation(class, f).await;
    }

    let db_path = scope.storage.join(DATABASE);
    let db = get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let identity = RepoIdentity::resolve(&db)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let repo_id = identity.as_str().to_string();
    let shared_repository = ConfigKv::get_with_conn(&db, "core.sharedRepository")
        .await
        .map_err(|error| {
            OperationError::Storage(format!(
                "cannot read core.sharedRepository before acquiring the operation scope lease: {error}"
            ))
        })?;
    if shared_repository
        .as_ref()
        .is_some_and(|entry| entry.encrypted)
    {
        return Err(OperationError::Storage(
            "core.sharedRepository must be plaintext; reset it with `libra init --shared=<mode>`"
                .to_string(),
        ));
    }
    let shared_repository_value = shared_repository.as_ref().map(|entry| entry.value.as_str());
    // Repository-wide ref transitions take the common lease before the
    // worktree lease, matching restore's lock order. Worktree-only edits keep
    // their existing concurrency across linked worktrees.
    let _repository_lease =
        if operation_needs_repository_lease(&meta, class) && !repository_ref_lease_is_held() {
            Some(ScopeLease::acquire_repository(scope, &repo_id, shared_repository_value).await?)
        } else {
            None
        };
    let lease_permissions = LeaseFilePermissions::from_shared_repository(
        shared_repository.as_ref().map(|entry| entry.value.as_str()),
    )?;
    #[cfg(test)]
    if let Some(error) = test_hooks::take_pre_lease_busy(&meta) {
        return Err(error);
    }
    let _lease = ScopeLease::acquire_with_permissions(scope, &repo_id, lease_permissions).await?;
    let storage = ClientStorage::init_local(scope.storage.join("objects"));
    let store = OperationStoreV2::new_for_repo(&repo_id, db.clone(), storage.clone());
    let scope_key = scope.scope.storage_key().to_string();
    let mut heads = store
        .read_heads_view(&repo_id, &scope_key)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let mut expected_generation = store
        .read_head_generation(&repo_id, &scope_key)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let pointer = match WorkspaceStatePointer::load(scope).await {
        Ok(pointer) => pointer,
        Err(super::PointerError::Missing(_)) => bootstrap_pointer(&heads)?,
        Err(error) => return Err(OperationError::Stale(error.to_string())),
    };
    if !heads.head_ids().is_empty() {
        match pointer.staleness(&heads) {
            Staleness::Fresh => {}
            state => {
                return Err(OperationError::Stale(format!(
                    "working-copy pointer is {state:?} against current heads; run `libra op doctor --fix` to recover an interrupted operation"
                )));
            }
        }
    }

    let mut pre_snapshotter = WorkspaceSnapshotter::new(scope.clone(), pointer.clone());
    let pre = pre_snapshotter
        .capture()
        .await
        .map_err(snapshot_error_to_operation)?;
    if class == MutationClass::ExternalOrUnknown && pre.snapshot.completeness != Completeness::Full
    {
        return Err(OperationError::Storage(
            "pre-mutation external snapshot is partial; refusing to mutate without a coherent baseline"
                .to_string(),
        ));
    }
    let pre_refs = capture_reference_state_with_repository_lease(
        &db,
        scope,
        &repo_id,
        shared_repository_value,
        _repository_lease.is_some(),
    )
    .await?;

    // The pointer records the last captured workspace, so a changed disk
    // state on entry is an external mutation that must become its own DAG
    // operation before the requested command can run.
    let previous_content_oid = pointer
        .last_content_oid
        .unwrap_or(pointer.last_snapshot_oid);
    if pointer.last_op_id != "bootstrap" && previous_content_oid != pre.content_oid {
        let external_id = Uuid::now_v7().to_string();
        let external_view_oid = write_view(
            &store,
            &repo_id,
            &pre.snapshot,
            pre.snapshot_oid,
            &pre_refs,
            &heads.head_ids(),
        )?;
        let external = OperationV2 {
            op_id: external_id.clone(),
            parent_op_ids: heads.head_ids(),
            pre_view_oid: external_view_oid,
            post_view_oid: external_view_oid,
            kind: OperationKind::ExternalSnapshot,
            status: OperationStatusV2::Running,
            metadata: OperationMetaV2 {
                command_name: Some("external.snapshot".to_string()),
                description: Some("External workspace change detected".to_string()),
                ..Default::default()
            },
            restores_op_id: None,
            reverts_op_id: None,
            predecessor_map_oid: None,
        };
        store
            .write_operation(&external)
            .await
            .map_err(|error| OperationError::Storage(error.to_string()))?;
        append_journal(
            &store,
            &external_id,
            JournalPhase::Publish,
            Some(external_view_oid),
            Some(external_view_oid),
            &format!("pid-{}", std::process::id()),
            now_millis(),
        )
        .await?;
        let generation = match store
            .cas_update_op_heads_at_generation(
                &repo_id,
                &scope_key,
                expected_generation,
                &external.parent_op_ids,
                std::slice::from_ref(&external_id),
            )
            .await
        {
            Ok(generation) => generation,
            Err(error) => {
                // ADR-OL-06: a concurrent publication keeps both candidates as
                // sibling heads instead of overwriting one of them; the head
                // set is converged later by `libra op reconcile`.
                match store
                    .merge_op_heads(
                        &repo_id,
                        &scope_key,
                        &external.parent_op_ids,
                        std::slice::from_ref(&external_id),
                    )
                    .await
                {
                    Ok(generation) => generation,
                    Err(merge_error) => {
                        let _ = store
                            .update_operation_status(&external_id, OperationStatusV2::Failed)
                            .await;
                        return Err(OperationError::Cas(format!(
                            "{error}; sibling head merge also failed: {merge_error}"
                        )));
                    }
                }
            }
        };
        let mut external_pointer =
            WorkspaceStatePointer::new(external_id.clone(), pre.snapshot_oid, generation);
        external_pointer.last_content_oid = Some(pre.content_oid);
        external_pointer
            .save(scope)
            .await
            .map_err(|error| OperationError::Storage(error.to_string()))?;
        store
            .update_operation_status(&external_id, OperationStatusV2::Success)
            .await
            .map_err(|error| OperationError::Storage(error.to_string()))?;
        heads = store
            .read_heads_view(&repo_id, &scope_key)
            .await
            .map_err(|error| OperationError::Storage(error.to_string()))?;
        expected_generation = generation;
    }
    let pre_view_oid = write_view(
        &store,
        &repo_id,
        &pre.snapshot,
        pre.snapshot_oid,
        &pre_refs,
        &heads.head_ids(),
    )?;
    let operation_id = Uuid::now_v7().to_string();
    let owner = format!("pid-{}", std::process::id());
    let now = now_millis();
    let operation = OperationV2 {
        op_id: operation_id.clone(),
        parent_op_ids: heads.head_ids(),
        pre_view_oid,
        post_view_oid: pre_view_oid,
        kind: if class == MutationClass::ExternalOrUnknown {
            OperationKind::ExternalSnapshot
        } else {
            OperationKind::Command
        },
        status: OperationStatusV2::Running,
        metadata: meta.clone(),
        restores_op_id: None,
        reverts_op_id: None,
        predecessor_map_oid: None,
    };
    store
        .write_operation(&operation)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    append_journal(
        &store,
        &operation_id,
        JournalPhase::Reserved,
        Some(pre_view_oid),
        None,
        &owner,
        now,
    )
    .await?;
    append_journal(
        &store,
        &operation_id,
        JournalPhase::PreView,
        Some(pre_view_oid),
        None,
        &owner,
        now,
    )
    .await?;
    let mut txn = OperationTxn {
        op_id: operation_id.clone(),
        class,
        post_snapshot_complete: false,
    };
    append_journal(
        &store,
        &operation_id,
        JournalPhase::Mutation,
        Some(pre_view_oid),
        None,
        &owner,
        now_millis(),
    )
    .await?;
    let operation_future = with_operation_id(txn.op_id.clone(), f(&mut txn));
    let operation_result = if _repository_lease.is_some() {
        with_repository_ref_lease(operation_future).await
    } else {
        operation_future.await
    };
    let value = match operation_result {
        Ok(value) => value,
        Err(error) => {
            persist_failed_operation(
                &store,
                &operation_id,
                &meta,
                &heads.head_ids(),
                pre_view_oid,
                &owner,
            )
            .await?;
            return Err(error);
        }
    };

    #[cfg(test)]
    if let Some(error) = test_hooks::take_post_mutation_failure(&meta) {
        persist_failed_operation(
            &store,
            &operation_id,
            &meta,
            &heads.head_ids(),
            pre_view_oid,
            &owner,
        )
        .await?;
        return Err(error);
    }

    let mut post_snapshotter = WorkspaceSnapshotter::new(scope.clone(), pointer.clone());
    let post = match post_snapshotter.capture().await {
        Ok(post) => post,
        Err(error) => {
            persist_failed_operation(
                &store,
                &operation_id,
                &meta,
                &heads.head_ids(),
                pre_view_oid,
                &owner,
            )
            .await?;
            return Err(OperationError::Storage(error.to_string()));
        }
    };
    txn.observe_post_snapshot(post.snapshot.completeness == Completeness::Full);
    if class == MutationClass::ExternalOrUnknown && !txn.post_snapshot_complete {
        persist_failed_operation(
            &store,
            &operation_id,
            &meta,
            &heads.head_ids(),
            pre_view_oid,
            &owner,
        )
        .await?;
        return Err(OperationError::ExternalUnverified);
    }
    let post_refs = capture_reference_state_with_repository_lease(
        &db,
        scope,
        &repo_id,
        shared_repository_value,
        _repository_lease.is_some(),
    )
    .await?;
    let full_view_is_unchanged = pre.snapshot.completeness == Completeness::Full
        && post.snapshot.completeness == Completeness::Full
        && pre.content_oid == post.content_oid
        && pre_refs == post_refs;
    if full_view_is_unchanged
        && matches!(
            class,
            MutationClass::WorkspaceMutation | MutationClass::ExternalOrUnknown
        )
    {
        store
            .delete_operation(&operation_id)
            .await
            .map_err(|error| OperationError::Storage(error.to_string()))?;
        store
            .delete_journal(&operation_id)
            .await
            .map_err(|error| OperationError::Storage(error.to_string()))?;
        return Ok(OperationResult {
            value,
            operation_id: None,
            recorded: false,
        });
    }
    let post_view_oid = write_view(
        &store,
        &repo_id,
        &post.snapshot,
        post.snapshot_oid,
        &post_refs,
        &heads.head_ids(),
    )?;
    append_journal(
        &store,
        &operation_id,
        JournalPhase::PostView,
        Some(pre_view_oid),
        Some(post_view_oid),
        &owner,
        now_millis(),
    )
    .await?;
    store
        .update_operation_post_view(&operation_id, &post_view_oid)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    append_journal(
        &store,
        &operation_id,
        JournalPhase::Publish,
        Some(pre_view_oid),
        Some(post_view_oid),
        &owner,
        now_millis(),
    )
    .await?;
    let generation = match store
        .cas_update_op_heads_at_generation(
            &repo_id,
            &scope_key,
            expected_generation,
            &operation.parent_op_ids,
            std::slice::from_ref(&operation_id),
        )
        .await
    {
        Ok(generation) => generation,
        Err(error) => {
            // ADR-OL-06: a concurrent publication keeps both candidates as
            // sibling heads instead of overwriting one of them; the head set
            // is converged later by `libra op reconcile`.
            match store
                .merge_op_heads(
                    &repo_id,
                    &scope_key,
                    &operation.parent_op_ids,
                    std::slice::from_ref(&operation_id),
                )
                .await
            {
                Ok(generation) => generation,
                Err(merge_error) => {
                    let _ = store
                        .update_operation_status(&operation_id, OperationStatusV2::Failed)
                        .await;
                    return Err(OperationError::Cas(format!(
                        "{error}; sibling head merge also failed: {merge_error}"
                    )));
                }
            }
        }
    };
    let mut operation_pointer =
        WorkspaceStatePointer::new(operation_id.clone(), post.snapshot_oid, generation);
    operation_pointer.last_content_oid = Some(post.content_oid);
    operation_pointer
        .save(scope)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    store
        .update_operation_status(&operation_id, OperationStatusV2::Success)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    Ok(OperationResult {
        value,
        operation_id: Some(operation_id),
        recorded: true,
    })
}

/// A newly attached/re-created worktree has a fresh gitdir and therefore no
/// pointer file, even though the repository may already have published heads.
/// Treat that identity as a new incarnation rooted at the current head. Its
/// zero content OID deliberately forces the next boundary to capture the
/// current worktree as an external snapshot before publishing the mutation.
fn bootstrap_pointer(heads: &super::OpHeadsView) -> Result<WorkspaceStatePointer, OperationError> {
    let (last_op_id, generation) = heads
        .heads
        .iter()
        .max_by_key(|(_, generation)| *generation)
        .map(|(op_id, generation)| (op_id.clone(), *generation))
        .unwrap_or_else(|| ("bootstrap".to_string(), 0));
    let zero_oid = ObjectHash::from_bytes(&vec![0; git_internal::hash::get_hash_kind().size()])
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    Ok(WorkspaceStatePointer::new(last_op_id, zero_oid, generation))
}

fn now_millis() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

fn snapshot_error_to_operation(error: SnapshotError) -> OperationError {
    let detail = error.to_string();
    let lower = detail.to_ascii_lowercase();
    if lower.contains("failed to load tree object") || lower.contains("existing tree object") {
        return OperationError::Cli(
            CliError::fatal(format!("failed to load tree: {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt),
        );
    }
    if lower.contains("failed to register its cloud object-index repair marker")
        || lower.contains("failed to store object")
    {
        return OperationError::Cli(
            CliError::fatal(format!("failed to store object: {detail}"))
                .with_stable_code(StableErrorCode::IoWriteFailed),
        );
    }
    if lower.contains("index") {
        return OperationError::Cli(
            CliError::fatal(format!("unable to read index: {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt),
        );
    }
    OperationError::Storage(detail)
}

fn write_view(
    store: &OperationStoreV2,
    repo_id: &str,
    snapshot: &super::WorkspaceSnapshotV2,
    snapshot_oid: ObjectHash,
    refs: &serde_json::Value,
    heads: &[String],
) -> Result<ObjectHash, OperationError> {
    let refs_bytes = serde_json::to_vec(&json!({"heads": heads, "references": refs}))
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let refs_oid = ObjectHash::from_type_and_data(ObjectType::Blob, &refs_bytes);
    store
        .write_blob(&refs_oid, &refs_bytes, ObjectType::Blob)
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let workspace_id = snapshot.workspace_id.clone();
    let view = RepoViewV2 {
        schema_version: super::view::REPO_VIEW_SCHEMA_VERSION,
        repo_id: repo_id.to_string(),
        refs_facet_oid: refs_oid,
        workspaces: [(workspace_id, snapshot_oid)].into_iter().collect(),
        change_roots: Vec::new(),
        extension_facets: Default::default(),
    };
    store
        .write_view_manifest(&view)
        .map_err(|error| OperationError::Storage(error.to_string()))
}

async fn capture_reference_state(
    db: &DatabaseConnection,
) -> Result<serde_json::Value, OperationError> {
    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT id, name, kind, \"commit\", remote, worktree_id FROM reference ORDER BY id",
        ))
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    let mut references = Vec::with_capacity(rows.len());
    for row in rows {
        references.push(json!({
            "id": row.try_get_by_index::<i64>(0).map_err(|error| OperationError::Storage(error.to_string()))?,
            "name": row.try_get_by_index::<Option<String>>(1).map_err(|error| OperationError::Storage(error.to_string()))?,
            "kind": row.try_get_by_index::<String>(2).map_err(|error| OperationError::Storage(error.to_string()))?,
            "commit": row.try_get_by_index::<Option<String>>(3).map_err(|error| OperationError::Storage(error.to_string()))?,
            "remote": row.try_get_by_index::<Option<String>>(4).map_err(|error| OperationError::Storage(error.to_string()))?,
            "worktree_id": row.try_get_by_index::<Option<String>>(5).map_err(|error| OperationError::Storage(error.to_string()))?,
        }));
    }
    Ok(serde_json::Value::Array(references))
}

async fn capture_reference_state_with_repository_lease(
    db: &DatabaseConnection,
    scope: &PinnedRequestScope,
    repo_id: &str,
    shared_repository: Option<&str>,
    lease_already_held: bool,
) -> Result<serde_json::Value, OperationError> {
    let _read_lease = if lease_already_held || repository_ref_lease_is_held() {
        None
    } else {
        Some(ScopeLease::acquire_repository_wait(scope, repo_id, shared_repository).await?)
    };
    capture_reference_state(db).await
}

async fn append_journal(
    store: &OperationStoreV2,
    operation_id: &str,
    phase: JournalPhase,
    pre_view_oid: Option<ObjectHash>,
    target_view_oid: Option<ObjectHash>,
    owner: &str,
    updated_at: i64,
) -> Result<(), OperationError> {
    store
        .append_journal(&JournalEntry {
            journal_id: operation_id.to_string(),
            op_id: operation_id.to_string(),
            phase,
            pre_view_oid,
            target_view_oid,
            owner: owner.to_string(),
            updated_at,
            recovery_payload: None,
        })
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))
}

async fn persist_failed_operation(
    store: &OperationStoreV2,
    operation_id: &str,
    _meta: &OperationMetaV2,
    _parents: &[String],
    pre_view_oid: ObjectHash,
    owner: &str,
) -> Result<(), OperationError> {
    store
        .update_operation_status(operation_id, OperationStatusV2::Failed)
        .await
        .map_err(|error| OperationError::Storage(error.to_string()))?;
    append_journal(
        store,
        operation_id,
        JournalPhase::Mutation,
        Some(pre_view_oid),
        None,
        owner,
        now_millis(),
    )
    .await
}

/// Type alias useful to gateways that erase a closure into a boxed future.
pub type OperationFuture<'a, T> =
    Pin<Box<dyn Future<Output = Result<T, OperationError>> + Send + 'a>>;

#[cfg(test)]
mod lease_tests;

#[cfg(test)]
mod scope_context_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_classification_covers_the_public_mutation_families() {
        assert_eq!(classify_command("status"), Ok(MutationClass::ReadOnly));
        assert_eq!(
            classify_command("add"),
            Ok(MutationClass::WorkspaceMutation)
        );
        assert_eq!(classify_command("commit"), Ok(MutationClass::RepoMutation));
        assert_eq!(
            classify_command("rebase"),
            Ok(MutationClass::SequencerMutation)
        );
        assert_eq!(
            classify_command("sparse-view"),
            Ok(MutationClass::LibraStateMutation)
        );
        assert_eq!(
            classify_command("shell"),
            Ok(MutationClass::ExternalOrUnknown)
        );
        assert_eq!(
            classify_command("internal-worker"),
            Ok(MutationClass::InternalWorker)
        );
        assert!(matches!(
            classify_command("future-command"),
            Err(ClassificationError::Unknown(_))
        ));
    }

    #[test]
    fn operation_restore_commands_take_the_repository_ref_fence() {
        for command in ["op restore", "op undo --last", "op redo", "op revert"] {
            assert!(
                command_may_mutate_shared_refs(command),
                "{command} can restore a shared branch tip"
            );
        }
        assert!(!command_may_mutate_shared_refs("op log"));
    }

    #[test]
    fn stash_pop_defers_the_repository_ref_fence_until_its_stack_cas() {
        let pop = OperationMetaV2 {
            command_name: Some("stash pop".to_string()),
            ..OperationMetaV2::default()
        };
        assert!(!operation_needs_repository_lease(
            &pop,
            MutationClass::RepoMutation
        ));

        let push = OperationMetaV2 {
            command_name: Some("stash".to_string()),
            ..OperationMetaV2::default()
        };
        assert!(operation_needs_repository_lease(
            &push,
            MutationClass::RepoMutation
        ));
    }

    #[tokio::test]
    async fn operation_id_context_matches_the_operation_txn() {
        let root = tempfile::tempdir().expect("scope root");
        let scope = crate::internal::worktree_scope::RequestScope {
            scope: crate::internal::worktree_scope::WorktreeScope::Main,
            workdir: root.path().to_path_buf(),
            gitdir: root.path().join(".libra"),
            storage: root.path().to_path_buf(),
            worktree_root: root.path().to_path_buf(),
        };

        let result = run_with_operation(
            &scope,
            OperationMetaV2::default(),
            MutationClass::RepoMutation,
            |txn| {
                let expected = txn.op_id.clone();
                async move {
                    assert_eq!(current_operation_id().as_deref(), Some(expected.as_str()));
                    Ok::<_, OperationError>(expected)
                }
            },
        )
        .await
        .expect("operation");
        assert_eq!(result.operation_id.as_deref(), Some(result.value.as_str()));
        assert!(current_operation_id().is_none());
    }

    #[tokio::test]
    async fn persisted_control_boundary_id_can_scope_a_command() {
        with_operation_id("persisted-control-op".to_string(), async {
            assert_eq!(
                current_operation_id().as_deref(),
                Some("persisted-control-op")
            );
        })
        .await;
        assert!(current_operation_id().is_none());
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
        let result = run_with_operation(
            &scope,
            OperationMetaV2::default(),
            MutationClass::ReadOnly,
            |_txn| async { Ok::<_, OperationError>(7) },
        )
        .await
        .expect("read-only operation");
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
        let result = run_with_operation(
            &scope,
            OperationMetaV2::default(),
            MutationClass::ExternalOrUnknown,
            |_txn| async { Ok::<_, OperationError>(()) },
        )
        .await;
        assert!(matches!(result, Err(OperationError::ExternalUnverified)));
    }
}
