//! AI workflow history persistence backed by an orphan Git branch.
//!
//! Libra records every AI process artefact (Intent, Task, Run, Plan,
//! PatchSet, Evidence, ToolInvocation, Provenance, Decision, ContextFrame,
//! ...) on a parallel branch named [`AI_REF`] (`libra/intent`). The branch
//! is *orphan*: it shares no history with the user's code branches but
//! lives inside the same object database, which means:
//!
//! * The same `git gc` policy keeps both AI history and code history
//!   reachable.
//! * AI artefacts are content-addressed under standard Git rules and can be
//!   transferred via the same protocol as the rest of the repository.
//!
//! Each commit on this ref points to a tree that is partitioned by object
//! type (`intent/`, `task/`, `plan/`, ...), with one blob per object id
//! beneath the type subtree. The flow for `append` is:
//!
//! 1. Read the current head (with retry on a busy SQLite) — see
//!    [`HistoryManager::resolve_history_head`].
//! 2. Load that head's root tree, splice the new entry in beneath its type
//!    subtree, write a fresh root tree, and create a child commit — see
//!    [`HistoryManager::create_append_commit`].
//! 3. Compare-and-swap the ref forward, retrying on a stale head — see
//!    [`HistoryManager::update_ref_if_matches`].
//!
//! Concurrency is handled via two retry loops: a SQLite-busy retry that
//! covers transient lock contention, and a head-conflict retry that re-reads
//! the head and retries the splice when another process advanced the ref.
//! Both loops have bounded iteration counts so misuse cannot deadlock the
//! caller.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, OnceLock, mpsc},
    time::{Duration, Instant},
};

use anyhow::{Context, Result, anyhow, bail};
#[cfg(test)]
use git_internal::internal::object::types::ObjectType;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        ObjectTrait,
        commit::Commit,
        signature::{Signature, SignatureType},
        tree::{Tree, TreeItem, TreeItemMode},
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbErr,
    EntityTrait, QueryFilter, QueryResult, Set, SqlErr, Statement, TransactionTrait, Value,
    sea_query::Expr,
};
use serde::{Deserialize, Serialize};
use tokio::time::sleep;

#[cfg(unix)]
use crate::internal::ai::authorized_read::{
    RegisteredHelperOutput, registered_helper_command, run_registered_bounded_helper_until,
};
#[cfg(test)]
use crate::internal::ai::observed_agents::RedactedBytes;
#[cfg(test)]
use crate::utils::storage::tiered::verify_fetched_object;
use crate::{
    internal::{
        ai::{
            authorized_read::{StrictBoundedRead, read_strictly_bounded},
            capture_scope::{
                CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
                authorize_final_capture_commit,
            },
        },
        model::reference::{self, ConfigKind},
    },
    utils::{
        object::{
            git_object_hash, read_git_object, read_git_object_bounded_validated, write_git_object,
            write_git_object_with_status,
        },
        storage::Storage,
    },
};

/// Default Git reference for the AI history orphan branch.
///
/// All AI process objects (Intent, Task, Run, Plan, PatchSet, Evidence,
/// ToolInvocation, Provenance, Decision) live on this single branch,
/// running in parallel with the normal code branch (`refs/heads/*`).
///
/// By keeping AI objects reachable from this ref, they are protected
/// from `git gc` — the branch acts as a GC root.
///
/// In the database, this is stored with kind='Branch' and name='libra/intent'.
pub const AI_REF: &str = "libra/intent";
/// Maximum attempts to retry a SQLite operation that returns a transient
/// "database is locked" error before propagating the failure.
const SQLITE_BUSY_MAX_RETRIES: usize = 15;
/// Base delay (ms) for the linear backoff applied between SQLite-busy retries.
/// The actual delay is `BASE * attempt`, so the worst-case wait is roughly
/// `BASE * SUM(1..=MAX_RETRIES)` which keeps total time bounded.
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 100;
/// Maximum attempts to re-read the history head and retry a splice when a
/// concurrent writer advances the ref between read and CAS. The bound is
/// generous because each retry is purely local (no network I/O).
const HISTORY_HEAD_CONFLICT_MAX_RETRIES: usize = 32;
const REJECTED_CLEANUP_MAX_VISITED_OBJECTS: usize = 250_000;
const REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION: Duration = Duration::from_secs(30);
/// A rejected append already has an in-flight marker.  Registering its
/// cleanup state is recovery work, so it receives only this fixed grace to
/// acquire SQLite rather than inheriting a foreground capture's long busy
/// timeout.
const REJECTED_CLEANUP_REGISTRATION_GRACE: Duration = Duration::from_millis(250);
const OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET: Duration = Duration::from_millis(500);
const OBJECT_INDEX_CLEANUP_DRAIN_BUDGET: Duration = Duration::from_secs(5);
const REJECTED_CLEANUP_MAX_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const REJECTED_CLEANUP_MAX_TOTAL_INDEX_BYTES: u64 = 64 * 1024 * 1024;
const REJECTED_CLEANUP_MAX_INDEX_FILES: usize = 256;
const REJECTED_CLEANUP_INDEX_HELPER_FRAME_CAP: u64 = 64 * 1024 * 1024;
pub const REJECTED_CLEANUP_INDEX_HELPER_ARG: &str =
    "--libra-internal-rejected-cleanup-index-helper";
pub const CHECKPOINT_OBJECT_IO_HELPER_ARG: &str = "--libra-internal-checkpoint-object-io-helper";
pub const CHECKPOINT_OBJECT_IO_HELPER_INPUT_CAP: u64 = 32 * 1024 * 1024;
pub const CHECKPOINT_OBJECT_IO_HELPER_OUTPUT_CAP: u64 = 32 * 1024 * 1024;

// Library unit tests do not enter Libra's `main`, so they deliberately have
// no registered private-helper program.  Cleanup behavior tests opt into this
// local-only seam instead of treating the libtest executable as a Libra CLI.
// Production callers never take this branch and fail closed when no main-owned
// helper is registered.
#[cfg(test)]
tokio::task_local! {
    static TEST_DIRECT_REJECTED_CLEANUP_INDEX_SNAPSHOT: ();
}

/// Stop cancellable checkpoint preparation once its paired deadline has
/// elapsed. The SQLite half is deliberately handled only by the final
/// authorization statement below; do not derive it from this `Instant`.
fn ensure_checkpoint_append_before_deadline(deadline: Option<CaptureCommitDeadline>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        bail!("checkpoint append exceeded the historical import execution deadline");
    }
    Ok(())
}

/// Keep the historical-import deadline surface stable when its marker helper
/// uses the shared traces deadline type internally.
fn normalize_checkpoint_marker_deadline(error: anyhow::Error) -> anyhow::Error {
    if error
        .chain()
        .any(|cause| cause.is::<crate::internal::ai::traces::TracesMarkerDeadlineExceeded>())
    {
        anyhow!("checkpoint append exceeded the historical import execution deadline")
    } else {
        error
    }
}

/// Acquire the SQLite writer only while the capture still has budget.
///
/// This deliberately covers acquisition, not a transaction's mutations or
/// COMMIT acknowledgement.  Once a transaction has issued a mutation, the
/// final SQLite authorization is its deadline decision point and its COMMIT
/// must be awaited without cancellation (see
/// [`commit_checkpoint_txn_after_final_authorization`]).
async fn begin_checkpoint_write_transaction_until(
    conn: &DatabaseConnection,
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_checkpoint_append_before_deadline(deadline)?;
    let result = match deadline {
        Some(deadline) => tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic()),
            crate::internal::db::begin_write_transaction(conn),
        )
        .await
        .map_err(|_| {
            anyhow!("checkpoint append exceeded the historical import execution deadline")
                .context(operation)
        })?,
        None => crate::internal::db::begin_write_transaction(conn).await,
    };
    result.context(operation)
}

#[cfg(test)]
tokio::task_local! {
    static TEST_CHECKPOINT_PRECOMMIT_READ_DELAY: Option<Duration>;
}

#[cfg(test)]
async fn with_checkpoint_precommit_read_delay<F>(delay: Duration, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_CHECKPOINT_PRECOMMIT_READ_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
async fn checkpoint_test_delay_before_precommit_read() {
    if let Ok(Some(delay)) = TEST_CHECKPOINT_PRECOMMIT_READ_DELAY.try_with(|configured| *configured)
    {
        tokio::time::sleep(delay).await;
    }
}

/// Await a read-only, pre-commit checkpoint operation while budget remains.
///
/// Callers must never pass a mutation, rollback, or COMMIT acknowledgement to
/// this helper: cancellation after a database write can leave the durable
/// outcome ambiguous.  The checkpoint paths use this solely for fence/marker
/// and ref reads before their final authorization transaction.
async fn await_checkpoint_precommit_read_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_checkpoint_append_before_deadline(deadline)?;
    #[cfg(test)]
    let future = async {
        checkpoint_test_delay_before_precommit_read().await;
        future.await
    };
    let result = match deadline {
        Some(deadline) => {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.monotonic()), future)
                .await
                .map_err(|_| {
                    anyhow!("checkpoint append exceeded the historical import execution deadline")
                        .context(operation)
                })?
        }
        None => future.await,
    };
    // A read can complete ready in the same poll in which its timer becomes
    // due. Do not let that scheduler edge fall through to a marker mutation.
    ensure_checkpoint_append_before_deadline(deadline)?;
    result.context(operation)
}

/// Sleep between transient SQLite retries without extending a capture's
/// deadline.  This is pre-commit retry bookkeeping, so it is safe to stop at
/// the deadline unlike a dispatched COMMIT acknowledgement.
async fn wait_for_checkpoint_sqlite_retry_until(
    deadline: Option<CaptureCommitDeadline>,
    retry_delay: Duration,
) -> Result<()> {
    ensure_checkpoint_append_before_deadline(deadline)?;
    match deadline {
        Some(deadline) => {
            let wake_at = Instant::now()
                .checked_add(retry_delay)
                .map(|wake_at| wake_at.min(deadline.monotonic()))
                .unwrap_or_else(|| deadline.monotonic());
            tokio::time::sleep_until(tokio::time::Instant::from_std(wake_at)).await;
        }
        None => sleep(retry_delay).await,
    }
    ensure_checkpoint_append_before_deadline(deadline)
}

fn rejected_cleanup_registration_deferred(reason: impl Into<String>) -> anyhow::Error {
    RejectedCheckpointCleanupDeferred {
        reason: reason.into(),
    }
    .into()
}

fn rejected_cleanup_registration_deadline(
    capture_deadline: Option<CaptureCommitDeadline>,
) -> Result<CaptureCommitDeadline> {
    // Rejected-append cleanup is a distinct bounded recovery operation once
    // the foreground invocation has elapsed. Establish both clock halves once
    // here, rather than reconstructing a SQLite deadline from an Instant at
    // the final authorization boundary.
    let recovery_deadline = CaptureCommitDeadline::from_budget(REJECTED_CLEANUP_REGISTRATION_GRACE)
        .map_err(|error| {
            rejected_cleanup_registration_deferred(format!(
                "could not establish the 250ms recovery grace deadline: {error}"
            ))
        })?;
    // A still-live foreground deadline remains a ceiling in *both* clock
    // domains. Do not let a long monotonic half re-anchor an already-expired
    // immutable SQLite authorization deadline to the fresh recovery grace.
    // Once the foreground monotonic half elapsed, this deliberately becomes
    // recovery-only work and gets one fixed grace to retain durable evidence.
    let now = Instant::now();
    Ok(
        match capture_deadline.filter(|deadline| deadline.monotonic() > now) {
            Some(capture_deadline) => CaptureCommitDeadline::from_established_pair(
                capture_deadline
                    .monotonic()
                    .min(recovery_deadline.monotonic()),
                capture_deadline
                    .sqlite_not_after_millis()
                    .min(recovery_deadline.sqlite_not_after_millis()),
            ),
            None => recovery_deadline,
        },
    )
}

fn ensure_before_rejected_cleanup_registration_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(rejected_cleanup_registration_deferred(
            "the 250ms SQLite recovery grace elapsed before cleanup registration finished",
        ));
    }
    Ok(())
}

/// Linearize a checkpoint transaction at the final SQLite authorization
/// statement, then wait for COMMIT without a timeout.
///
/// A timeout around `txn.commit()` is unsound for SQLite: SQLx can dispatch
/// COMMIT before the future is cancelled, making a dropped transaction unable
/// to roll it back. The authorization statement is therefore the deadline
/// decision point; after it succeeds we must await the acknowledgement.
async fn commit_checkpoint_txn_after_final_authorization(
    txn: DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<()> {
    if let Err(error) = ensure_checkpoint_append_before_deadline(deadline) {
        txn.rollback().await.ok();
        return Err(error).context(operation);
    }

    let authorization = match authorize_final_capture_commit(capture_scope, &txn, deadline).await {
        Ok(()) => Ok(()),
        Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed) => Err(anyhow!(
            "checkpoint append exceeded the historical import execution deadline"
        )),
        Err(error) => Err(anyhow::Error::new(error).context(
            "verify capture workspace lease before final checkpoint transaction authorization",
        )),
    };
    if let Err(error) = authorization {
        txn.rollback().await.ok();
        return Err(error).context(operation);
    }

    txn.commit().await.context(operation)
}
const CHECKPOINT_OBJECT_READ_MAX_INFLATED_BYTES: u64 = 16 * 1024 * 1024;

#[cfg(test)]
tokio::task_local! {
    static TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT: std::cell::Cell<usize>;
}

#[cfg(test)]
pub(crate) async fn count_checkpoint_snapshot_verifications<F: std::future::Future>(
    future: F,
) -> (F::Output, usize) {
    TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT
        .scope(std::cell::Cell::new(0), async move {
            let output = future.await;
            let count = TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT.with(std::cell::Cell::get);
            (output, count)
        })
        .await
}

fn rejected_cleanup_traversal_duration() -> Duration {
    REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RejectedCleanupDbSnapshot {
    markers: Vec<TracesInflightMarker>,
    candidates: HashSet<String>,
    graph_roots: Vec<String>,
    active_operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct RejectedCleanupRootSnapshot {
    db: RejectedCleanupDbSnapshot,
    index_roots: HashSet<String>,
    index_fingerprints: Vec<(String, String)>,
    active_operations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct RejectedCleanupIndexSnapshot {
    roots: HashSet<String>,
    fingerprints: Vec<(String, String)>,
    active_operations: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct RejectedCleanupIndexHelperRequest {
    repo_path: PathBuf,
    hash_bytes: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct RejectedCleanupIndexHelperResponse {
    snapshot: Option<RejectedCleanupIndexSnapshot>,
    error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointObjectIoHelperRequest {
    /// Standard-base64 encoding of the native path bytes (UTF-8 off Unix).
    repo_path_base64: String,
    operation: CheckpointObjectIoOperation,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum CheckpointObjectIoOperation {
    Read {
        oid: String,
        expected_type: String,
    },
    Write {
        object_type: String,
        data_base64: String,
    },
    VerifySnapshot {
        head: String,
        cataloged_commits: Vec<String>,
        checkpoints: Vec<CheckpointDurabilityHelperSpec>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CheckpointDurabilityHelperSpec {
    checkpoint_id: String,
    traces_commit: String,
    tree_oid: String,
    metadata_blob_oid: String,
}

/// Fixed, content-free private-helper failures.  The helper never serializes
/// filesystem, object, parser, or payload error text across this boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum CheckpointObjectIoHelperError {
    InvalidRequest,
    InvalidPath,
    UnsupportedReadType,
    InvalidObjectId,
    ObjectTypeMismatch,
    ReadFailed,
    UnsupportedObjectType,
    InvalidPayload,
    WriteFailed,
    SnapshotNotDurable,
}

impl CheckpointObjectIoHelperError {
    fn user_message(self) -> &'static str {
        match self {
            Self::InvalidRequest => "checkpoint object-I/O helper rejected an invalid request",
            Self::InvalidPath => "checkpoint object-I/O helper rejected its object store path",
            Self::UnsupportedReadType => {
                "checkpoint object-I/O helper rejected an unsupported read type"
            }
            Self::InvalidObjectId => "checkpoint object-I/O helper rejected an object id",
            Self::ObjectTypeMismatch => {
                "checkpoint object-I/O helper found an unexpected object type"
            }
            Self::ReadFailed => "checkpoint object-I/O helper could not read the object",
            Self::UnsupportedObjectType => {
                "checkpoint object-I/O helper rejected an unsupported object type"
            }
            Self::InvalidPayload => "checkpoint object-I/O helper rejected an object payload",
            Self::WriteFailed => "checkpoint object-I/O helper could not write the object",
            Self::SnapshotNotDurable => {
                "checkpoint object-I/O helper could not verify the checkpoint snapshot"
            }
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
enum CheckpointObjectIoHelperResponse {
    Read {
        oid: String,
        object_type: String,
        data_base64: String,
    },
    Written {
        oid: String,
        was_created: bool,
    },
    Verified {
        oids: Vec<String>,
    },
    Error {
        code: CheckpointObjectIoHelperError,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CheckpointObjectIndexIntent {
    oid: String,
    object_type: String,
    size: i64,
}

/// Test-only observation of one checkpoint CAS attempt after its objects have
/// been written but before the ref transaction decides whether they become
/// reachable. This lets retry regressions assert exact rejected OIDs rather
/// than relying on aggregate row counts.
#[cfg(test)]
#[derive(Debug, Clone)]
pub(crate) struct CheckpointAttemptIndexSnapshot {
    pub(crate) commit_hash: ObjectHash,
    pub(crate) tree_oid: ObjectHash,
    pub(crate) object_index_oids: Vec<String>,
}

/// Composes the capture-specific catalog/claim extra with object-index rows
/// that must become cloud-visible only if the same final traces-ref CAS wins.
struct CheckpointCommitTxnExtra<'a> {
    extra: Option<&'a dyn TracesTxnExtra>,
    capture_scope: Option<&'a CaptureScope>,
    object_index_intents: &'a [CheckpointObjectIndexIntent],
}

#[async_trait::async_trait]
impl TracesTxnExtra for CheckpointCommitTxnExtra<'_> {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
        if let Some(extra) = self.extra {
            extra.apply(txn, ctx).await?;
        }
        // `update_ref_if_matches_with_extra` checks scope before it begins
        // the CAS. Check again after any companion writes and immediately
        // before the cloud-visible index upsert so lease expiry rolls the ref,
        // catalog, claims, and index rows back as one transaction.
        if let Some(scope) = self.capture_scope {
            scope.assert_workspace_fence_live(txn).await.context(
                "verify capture workspace lease before checkpoint object-index transaction",
            )?;
        }
        let updates = self
            .object_index_intents
            .iter()
            .map(|intent| (intent.oid.clone(), intent.object_type.clone(), intent.size))
            .collect::<Vec<_>>();
        crate::utils::client_storage::upsert_agent_object_index_rows_with_conn(txn, &updates)
            .await
            .context("upsert checkpoint object-index rows in final ref transaction")
    }
}

struct CleanupHelperChild {
    child: Option<Child>,
    reaper: mpsc::Sender<Child>,
}

impl CleanupHelperChild {
    fn new(child: Child, reaper: mpsc::Sender<Child>) -> Self {
        Self {
            child: Some(child),
            reaper,
        }
    }

    fn child_mut(&mut self) -> &mut Child {
        // INVARIANT: the guard owns its child until Drop; no method removes it.
        self.child
            .as_mut()
            .expect("cleanup helper child remains owned by its reap guard")
    }
}

impl Drop for CleanupHelperChild {
    fn drop(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        match child.try_wait() {
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => {
                let _ = child.kill();
                // Waiting here would let a helper stuck in uninterruptible
                // filesystem I/O extend the foreground command past its
                // absolute deadline. The process-wide nonblocking reaper
                // owns the Child until try_wait observes and reaps its exit.
                if let Err(error) = self.reaper.send(child) {
                    let mut child = error.0;
                    let _ = child.try_wait();
                }
            }
        }
    }
}

static CLEANUP_HELPER_REAPER: OnceLock<Result<mpsc::Sender<Child>, String>> = OnceLock::new();

#[cfg(test)]
static CLEANUP_HELPER_REAPED_CHILDREN: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

fn cleanup_helper_reaper_sender() -> Result<mpsc::Sender<Child>> {
    let result = CLEANUP_HELPER_REAPER.get_or_init(|| {
        let (sender, receiver) = mpsc::channel::<Child>();
        std::thread::Builder::new()
            .name("libra-cleanup-helper-reaper".to_string())
            .spawn(move || {
                let mut children = Vec::<Child>::new();
                loop {
                    match receiver.recv_timeout(Duration::from_millis(25)) {
                        Ok(child) => children.push(child),
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) if children.is_empty() => break,
                        Err(mpsc::RecvTimeoutError::Disconnected) => {}
                    }
                    while let Ok(child) = receiver.try_recv() {
                        children.push(child);
                    }
                    let mut index = 0;
                    while index < children.len() {
                        match children[index].try_wait() {
                            Ok(Some(_)) => {
                                let mut reaped = children.swap_remove(index);
                                // `try_wait` above observed and reaped this
                                // process. Calling `wait` returns the cached
                                // status immediately and makes that lifecycle
                                // explicit to Clippy's zombie-process audit.
                                let _ = reaped.wait();
                                #[cfg(test)]
                                CLEANUP_HELPER_REAPED_CHILDREN
                                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            }
                            Ok(None) | Err(_) => index += 1,
                        }
                    }
                }
            })
            .map(|_| sender)
            .map_err(|error| format!("start cleanup helper reaper thread: {error}"))
    });
    result
        .as_ref()
        .cloned()
        .map_err(|message| anyhow!(message.clone()))
}

/// Exact ownership identity for one traces writer marker. Checkpoint IDs are
/// intentionally stable across retries, so the random generation is what
/// prevents an expired writer from adopting a takeover writer's replacement
/// marker under the same metadata key.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TracesWriterFence {
    session_id: String,
    attempt_id: String,
    generation: String,
}

/// Typed reason a checkpoint append left the traces ref unchanged because
/// another writer or recovery owned the ref/marker. The checkpoint store
/// classifies these through the error chain, never through display text, so
/// a reworded message cannot silently demote a conflict to a store failure.
/// Every message is a fixed, content-free string.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub(crate) enum CheckpointAppendConflict {
    /// Every bounded ref-CAS attempt lost to a concurrent traces head move.
    #[error("history head changed repeatedly while appending a checkpoint; retry the operation")]
    RefCasExhausted,
    /// The writer marker generation that sealed this attempt was fenced,
    /// replaced, or retired by recovery before the ref update.
    #[error("{0}")]
    MarkerFenced(&'static str),
}

/// The `TracesTxnExtra` companion failed inside the ref-CAS transaction, so
/// the ref, catalog, claim, and object-index rows rolled back together.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("transactional companion writes failed; ref update rolled back")]
pub(crate) struct CheckpointCompanionTransactionFailed;

fn checkpoint_marker_fenced(message: &'static str) -> anyhow::Error {
    anyhow::Error::new(CheckpointAppendConflict::MarkerFenced(message))
}

/// Outcome of a compare-and-swap reference update.
///
/// Used by [`HistoryManager::update_ref_if_matches`] to communicate whether
/// the ref moved successfully (`Updated`) or whether the expected head was
/// stale and the caller must restart the splice (`HeadChanged`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RefUpdateOutcome {
    /// The ref was atomically advanced to the new commit.
    Updated,
    /// Another writer advanced the ref before our CAS — caller should
    /// re-read the head and rebuild the commit on top of it.
    HeadChanged,
}

/// Detect transient SQLite contention that should trigger a retry.
///
/// Functional scope:
/// - Inspects the error message for the well-known "database is locked" or
///   "database schema is locked" substrings emitted by SQLite under busy
///   contention.
///
/// Boundary conditions:
/// - This is intentionally a string match: the SeaORM error wraps the
///   underlying SQLite text, and there is no stable error-code variant for
///   busy/lock conditions in the wrapping layer.
fn is_sqlite_busy(err: &DbErr) -> bool {
    let message = err.to_string();
    message.contains("database is locked") || message.contains("database schema is locked")
}

fn anyhow_is_sqlite_busy(err: &anyhow::Error) -> bool {
    err.chain()
        .filter_map(|cause| cause.downcast_ref::<DbErr>())
        .any(is_sqlite_busy)
}

/// Detect unique-constraint violations on the `reference` table.
///
/// Functional scope:
/// - Used by the optimistic CAS path: when two writers race to insert the
///   same ref name, one will see a unique-constraint violation; we treat
///   that as a `HeadChanged` outcome rather than a hard error.
fn is_sqlite_unique_violation(err: &DbErr) -> bool {
    matches!(err.sql_err(), Some(SqlErr::UniqueConstraintViolation(_)))
}

fn read_cleanup_regular_file(
    path: &Path,
    per_file_limit: u64,
    aggregate_remaining: u64,
    what: &str,
) -> Result<Option<Vec<u8>>> {
    read_cleanup_regular_file_inner(path, per_file_limit, aggregate_remaining, what, || {})
}

fn read_cleanup_regular_file_inner<F: FnOnce()>(
    path: &Path,
    per_file_limit: u64,
    aggregate_remaining: u64,
    what: &str,
    after_metadata: F,
) -> Result<Option<Vec<u8>>> {
    #[cfg(unix)]
    let opened = {
        use std::os::unix::fs::OpenOptionsExt;

        fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NONBLOCK | libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path)
    };
    #[cfg(not(unix))]
    let opened = fs::File::open(path);
    let mut file = match opened {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("open {what} without following symlinks"));
        }
    };
    let metadata = file
        .metadata()
        .with_context(|| format!("inspect held {what} descriptor"))?;
    if !metadata.file_type().is_file() {
        bail!("{what} is not a regular file");
    }
    let limit = per_file_limit.min(aggregate_remaining);
    if metadata.len() > limit {
        bail!("{what} exceeds the {limit} byte cleanup read limit");
    }
    after_metadata();
    let bytes = match read_strictly_bounded(&mut file, limit) {
        StrictBoundedRead::Complete(bytes) => bytes,
        StrictBoundedRead::Oversize { .. } => {
            bail!("{what} grew beyond the {limit} byte cleanup read limit");
        }
        StrictBoundedRead::Failed { error, .. } => {
            return Err(error).with_context(|| format!("read held {what} descriptor"));
        }
    };
    Ok(Some(bytes))
}

fn parse_cleanup_index_roots(
    bytes: &[u8],
    hash_bytes: usize,
    what: &str,
) -> Result<HashSet<String>> {
    if !matches!(hash_bytes, 20 | 32) {
        bail!("{what} uses unsupported {hash_bytes}-byte object ids");
    }
    if bytes.len() < 12 + hash_bytes || &bytes[..4] != b"DIRC" {
        bail!("{what} has an invalid index header");
    }
    let read_u32 = |offset: usize| -> Result<u32> {
        let raw = bytes
            .get(offset..offset + 4)
            .ok_or_else(|| anyhow!("{what} is truncated"))?;
        let raw: [u8; 4] = raw
            .try_into()
            .map_err(|_| anyhow!("{what} has an invalid integer field"))?;
        Ok(u32::from_be_bytes(raw))
    };
    // Index v2 and v3 are supported; v3 only adds the per-entry extended
    // flags word (CE_EXTENDED), decoded through the git-internal helpers so
    // Libra never carries a second extended-bit state machine (issues/490
    // SW-01, ADR-SW-02).
    let index_version = read_u32(4)?;
    if index_version != 2 && index_version != 3 {
        bail!("{what} is not a supported index version ({index_version})");
    }
    let entry_count = usize::try_from(read_u32(8)?)
        .with_context(|| format!("{what} entry count exceeds this platform"))?;
    let checksum_start = bytes.len() - hash_bytes;
    let expected_checksum = &bytes[checksum_start..];
    let kind = match hash_bytes {
        20 => git_internal::hash::HashKind::Sha1,
        32 => {
            // 32-byte tails are shared by sha256 and blake3; prefer the process
            // hash kind when it matches the width, else sha256 (legacy AI cleanup).
            match git_internal::hash::get_hash_kind() {
                git_internal::hash::HashKind::Blake3 => git_internal::hash::HashKind::Blake3,
                _ => git_internal::hash::HashKind::Sha256,
            }
        }
        _ => bail!("{what} uses unsupported {hash_bytes}-byte object ids"),
    };
    let computed = crate::internal::object_format::digest(kind, &bytes[..checksum_start]);
    let checksum_matches = computed.as_ref() == expected_checksum;
    if !checksum_matches {
        bail!("{what} checksum does not match the held file bytes");
    }

    let minimum_entry_bytes = 40_usize
        .checked_add(hash_bytes)
        .and_then(|size| size.checked_add(3))
        .ok_or_else(|| anyhow!("{what} entry size overflow"))?;
    if entry_count > checksum_start.saturating_sub(12) / minimum_entry_bytes {
        bail!("{what} entry count exceeds its bounded file size");
    }
    let mut roots = HashSet::new();
    let mut cursor = 12_usize;
    for _ in 0..entry_count {
        let entry_start = cursor;
        let hash_start = cursor
            .checked_add(40)
            .ok_or_else(|| anyhow!("{what} entry offset overflow"))?;
        let hash_end = hash_start
            .checked_add(hash_bytes)
            .ok_or_else(|| anyhow!("{what} object id offset overflow"))?;
        let flags_end = hash_end
            .checked_add(2)
            .ok_or_else(|| anyhow!("{what} flags offset overflow"))?;
        if flags_end > checksum_start {
            bail!("{what} entry is truncated");
        }
        roots.insert(hex::encode(&bytes[hash_start..hash_end]));
        let flags = u16::from_be_bytes([bytes[hash_end], bytes[hash_end + 1]]);
        let declared_name_len = usize::from(flags & 0x0fff);
        let mut name_start = flags_end;
        if flags & 0x4000 != 0 {
            // CE_EXTENDED: the extended flags word sits between the main flags
            // and the name; unknown bits fail closed (git-internal validates).
            let word_end = flags_end
                .checked_add(2)
                .ok_or_else(|| anyhow!("{what} extended flags offset overflow"))?;
            if word_end > checksum_start {
                bail!("{what} extended flags are truncated");
            }
            let word = u16::from_be_bytes([bytes[flags_end], bytes[flags_end + 1]]);
            git_internal::internal::index::Flags::from_extended_word(word)
                .map_err(|error| anyhow!("{what} has invalid extended flags: {error}"))?;
            name_start = word_end;
        }
        let name_end = if declared_name_len == 0x0fff {
            bytes[name_start..checksum_start]
                .iter()
                .position(|byte| *byte == 0)
                .map(|offset| name_start + offset)
                .ok_or_else(|| anyhow!("{what} long path has no terminator"))?
        } else {
            let end = name_start
                .checked_add(declared_name_len)
                .ok_or_else(|| anyhow!("{what} path length overflow"))?;
            if end >= checksum_start || bytes[end] != 0 {
                bail!("{what} path is truncated or not NUL-terminated");
            }
            end
        };
        cursor = name_end + 1;
        while !(cursor - entry_start).is_multiple_of(8) {
            if cursor >= checksum_start || bytes[cursor] != 0 {
                bail!("{what} entry padding is invalid");
            }
            cursor += 1;
        }
    }

    while cursor < checksum_start {
        let header_end = cursor
            .checked_add(8)
            .ok_or_else(|| anyhow!("{what} extension offset overflow"))?;
        if header_end > checksum_start {
            bail!("{what} extension header is truncated");
        }
        if !bytes[cursor].is_ascii_uppercase() {
            bail!("{what} contains an unsupported required index extension");
        }
        let size = usize::try_from(read_u32(cursor + 4)?)
            .with_context(|| format!("{what} extension size exceeds this platform"))?;
        cursor = header_end
            .checked_add(size)
            .ok_or_else(|| anyhow!("{what} extension size overflow"))?;
        if cursor > checksum_start {
            bail!("{what} extension payload is truncated");
        }
    }
    Ok(roots)
}

fn collect_rejected_cleanup_index_snapshot(
    repo_path: &Path,
    hash_bytes: usize,
) -> Result<RejectedCleanupIndexSnapshot> {
    use sha2::{Digest, Sha256};

    let mut index_paths = vec![repo_path.join("index")];
    let registry_path = repo_path.join("worktrees.json");
    let mut fingerprints = Vec::new();
    let mut total_bytes = 0_u64;
    let registry_what = "worktree registry before rejected object cleanup";
    if let Some(bytes) = read_cleanup_regular_file(
        &registry_path,
        REJECTED_CLEANUP_MAX_INDEX_BYTES,
        REJECTED_CLEANUP_MAX_TOTAL_INDEX_BYTES,
        registry_what,
    )? {
        total_bytes = bytes.len() as u64;
        fingerprints.push((
            registry_path.to_string_lossy().into_owned(),
            hex::encode(Sha256::digest(&bytes)),
        ));
        // Route through the DISCRIMINATING registry parser (§C.7): it
        // accepts exactly one of the v2/v1 shapes and refuses hybrid or
        // malformed documents — an ad-hoc key probe here could pick an
        // empty `entries` over a populated legacy array and silently omit
        // linked-worktree index roots from the cleanup snapshot.
        let registry = crate::command::worktree::WorktreeState::parse(&bytes)
            .map_err(|error| anyhow!("worktree registry rejected: {error}"))
            .context("parse worktree registry before rejected object cleanup")?;
        let worktree_paths = registry.entry_paths();
        if worktree_paths.len() > REJECTED_CLEANUP_MAX_INDEX_FILES {
            bail!(
                "worktree registry exceeds the {} index-file cleanup limit",
                REJECTED_CLEANUP_MAX_INDEX_FILES
            );
        }
        for path in worktree_paths {
            index_paths.push(Path::new(&path).join(".libra/index"));
        }
    }
    index_paths.sort();
    index_paths.dedup();

    let mut index_roots = HashSet::new();
    for index_path in index_paths {
        let remaining = REJECTED_CLEANUP_MAX_TOTAL_INDEX_BYTES.saturating_sub(total_bytes);
        let what = format!("worktree index '{}'", index_path.display());
        let Some(bytes) = read_cleanup_regular_file(
            &index_path,
            REJECTED_CLEANUP_MAX_INDEX_BYTES,
            remaining,
            &what,
        )?
        else {
            continue;
        };
        total_bytes = total_bytes
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| anyhow!("worktree index cleanup input size overflow"))?;
        fingerprints.push((
            index_path.to_string_lossy().into_owned(),
            hex::encode(Sha256::digest(&bytes)),
        ));
        index_roots.extend(parse_cleanup_index_roots(&bytes, hash_bytes, &what)?);
    }
    fingerprints.sort();

    let mut active_operations = Vec::new();
    for name in [
        "rebase-merge",
        "rebase-apply",
        "merge-state.json",
        "merge-autostash.json",
        "revert-state.json",
    ] {
        match fs::symlink_metadata(repo_path.join(name)) {
            Ok(_) => active_operations.push(name.to_string()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("inspect repository operation state '{name}'"));
            }
        }
    }

    Ok(RejectedCleanupIndexSnapshot {
        roots: index_roots,
        fingerprints,
        active_operations,
    })
}

pub fn run_rejected_cleanup_index_helper(input: &[u8]) -> Result<Vec<u8>> {
    let request: RejectedCleanupIndexHelperRequest =
        serde_json::from_slice(input).context("decode rejected-cleanup index helper request")?;
    let response =
        match collect_rejected_cleanup_index_snapshot(&request.repo_path, request.hash_bytes) {
            Ok(snapshot) => RejectedCleanupIndexHelperResponse {
                snapshot: Some(snapshot),
                error: None,
            },
            Err(error) => RejectedCleanupIndexHelperResponse {
                snapshot: None,
                error: Some(format!("{error:#}")),
            },
        };
    serde_json::to_vec(&response).context("encode rejected-cleanup index helper response")
}

fn encode_checkpoint_object_path(path: &Path) -> Result<String> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;

        Ok(STANDARD.encode(path.as_os_str().as_bytes()))
    }
    #[cfg(not(unix))]
    {
        let text = path
            .to_str()
            .ok_or_else(|| anyhow!("checkpoint object store path is not valid platform text"))?;
        Ok(STANDARD.encode(text.as_bytes()))
    }
}

fn decode_checkpoint_object_path(encoded: &str) -> Result<PathBuf> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let bytes = STANDARD
        .decode(encoded)
        .context("decode checkpoint object store path")?;
    #[cfg(unix)]
    {
        use std::{ffi::OsString, os::unix::ffi::OsStringExt};

        Ok(PathBuf::from(OsString::from_vec(bytes)))
    }
    #[cfg(not(unix))]
    {
        let text =
            String::from_utf8(bytes).context("checkpoint object store path is not valid UTF-8")?;
        Ok(PathBuf::from(text))
    }
}

/// Execute one checkpoint object read/write in the private helper process.
///
/// Every recoverable failure is returned as a fixed enum code.  In particular,
/// never serialize an object-store path, object payload, parser detail, or
/// operating-system error: the parent can safely surface the stable category
/// without making the private helper protocol a raw-content side channel.
pub fn run_checkpoint_object_io_helper(input: &[u8]) -> Result<Vec<u8>> {
    use base64::{Engine as _, engine::general_purpose::STANDARD};

    let response = match serde_json::from_slice::<CheckpointObjectIoHelperRequest>(input) {
        Err(_) => CheckpointObjectIoHelperResponse::Error {
            code: CheckpointObjectIoHelperError::InvalidRequest,
        },
        Ok(request) => match decode_checkpoint_object_path(&request.repo_path_base64) {
            Err(_) => CheckpointObjectIoHelperResponse::Error {
                code: CheckpointObjectIoHelperError::InvalidPath,
            },
            Ok(repo_path) => match request.operation {
                CheckpointObjectIoOperation::Read { oid, expected_type } => {
                    if !matches!(expected_type.as_str(), "tree" | "commit") {
                        CheckpointObjectIoHelperResponse::Error {
                            code: CheckpointObjectIoHelperError::UnsupportedReadType,
                        }
                    } else {
                        match crate::internal::ai::util::parse_repo_object_id(&oid) {
                            Err(_) => CheckpointObjectIoHelperResponse::Error {
                                code: CheckpointObjectIoHelperError::InvalidObjectId,
                            },
                            Ok(parsed_oid) => match read_git_object_bounded_validated(
                                &repo_path,
                                &parsed_oid,
                                CHECKPOINT_OBJECT_READ_MAX_INFLATED_BYTES,
                            ) {
                                Ok((object_type, data)) if object_type == expected_type => {
                                    CheckpointObjectIoHelperResponse::Read {
                                        oid,
                                        object_type,
                                        data_base64: STANDARD.encode(data),
                                    }
                                }
                                Ok((_, _)) => CheckpointObjectIoHelperResponse::Error {
                                    code: CheckpointObjectIoHelperError::ObjectTypeMismatch,
                                },
                                Err(_) => CheckpointObjectIoHelperResponse::Error {
                                    code: CheckpointObjectIoHelperError::ReadFailed,
                                },
                            },
                        }
                    }
                }
                CheckpointObjectIoOperation::Write {
                    object_type,
                    data_base64,
                } => {
                    if !matches!(object_type.as_str(), "blob" | "tree" | "commit") {
                        CheckpointObjectIoHelperResponse::Error {
                            code: CheckpointObjectIoHelperError::UnsupportedObjectType,
                        }
                    } else {
                        match STANDARD.decode(data_base64) {
                            Err(_) => CheckpointObjectIoHelperResponse::Error {
                                code: CheckpointObjectIoHelperError::InvalidPayload,
                            },
                            Ok(data) => {
                                match write_git_object_with_status(&repo_path, &object_type, &data)
                                {
                                    Ok((oid, was_created)) => {
                                        CheckpointObjectIoHelperResponse::Written {
                                            oid: oid.to_string(),
                                            was_created,
                                        }
                                    }
                                    Err(_) => CheckpointObjectIoHelperResponse::Error {
                                        code: CheckpointObjectIoHelperError::WriteFailed,
                                    },
                                }
                            }
                        }
                    }
                }
                CheckpointObjectIoOperation::VerifySnapshot {
                    head,
                    cataloged_commits,
                    checkpoints,
                } => {
                    let specs = checkpoints
                        .iter()
                        .map(|checkpoint| CheckpointDurabilitySpec {
                            checkpoint_id: &checkpoint.checkpoint_id,
                            traces_commit: &checkpoint.traces_commit,
                            tree_oid: &checkpoint.tree_oid,
                            metadata_blob_oid: &checkpoint.metadata_blob_oid,
                        })
                        .collect::<Vec<_>>();
                    match parse_cataloged_traces_commits(&cataloged_commits).and_then(
                        |cataloged_commits| {
                            let head = crate::internal::ai::util::parse_repo_object_id(&head)
                                .map_err(|_| anyhow!("invalid traces snapshot head"))?;
                            checkpoint_snapshot_durable_oids_from_head(
                                &repo_path,
                                head,
                                &cataloged_commits,
                                &specs,
                                None,
                            )
                        },
                    ) {
                        Ok(oids) => {
                            let mut oids = oids.into_iter().collect::<Vec<_>>();
                            oids.sort();
                            CheckpointObjectIoHelperResponse::Verified { oids }
                        }
                        Err(_) => CheckpointObjectIoHelperResponse::Error {
                            code: CheckpointObjectIoHelperError::SnapshotNotDurable,
                        },
                    }
                }
            },
        },
    };
    serde_json::to_vec(&response).context("encode checkpoint object-I/O helper response")
}

async fn invoke_checkpoint_object_helper(
    repo_path: &Path,
    operation: CheckpointObjectIoOperation,
    deadline: Instant,
) -> Result<CheckpointObjectIoHelperResponse> {
    #[cfg(not(unix))]
    {
        let _ = (repo_path, operation, deadline);
        // The central private-helper runner intentionally has no raw-pipe
        // containment guarantee off Unix.  Do not spawn a host executable in
        // an embedded process or rely on `kill_on_drop` alone.
        bail!("checkpoint object-I/O helper is unavailable on this platform");
    }

    #[cfg(unix)]
    {
        let request = CheckpointObjectIoHelperRequest {
            repo_path_base64: encode_checkpoint_object_path(repo_path)?,
            operation,
        };
        let frame = serde_json::to_vec(&request).context("encode checkpoint object-I/O request")?;
        if frame.len() as u64 > CHECKPOINT_OBJECT_IO_HELPER_INPUT_CAP {
            bail!(
                "checkpoint object-I/O request exceeds the {}-byte helper limit",
                CHECKPOINT_OBJECT_IO_HELPER_INPUT_CAP
            );
        }
        if Instant::now() >= deadline {
            bail!("checkpoint object I/O exceeded its command deadline");
        }

        let Some(command) = registered_helper_command(CHECKPOINT_OBJECT_IO_HELPER_ARG) else {
            // `main` alone registers the executable which owns the private argv.
            // An embedded host must never be treated as an interchangeable Libra
            // binary merely because `current_exe` happens to exist.
            bail!("checkpoint object-I/O helper is unavailable in this host");
        };
        let response_bytes = match run_registered_bounded_helper_until(
            command,
            &[&frame],
            CHECKPOINT_OBJECT_IO_HELPER_OUTPUT_CAP,
            deadline,
        )
        .await
        {
            RegisteredHelperOutput::Output(response) => response,
            RegisteredHelperOutput::DeadlineExceeded => {
                bail!("checkpoint object I/O exceeded its command deadline");
            }
            RegisteredHelperOutput::Failed => {
                bail!("checkpoint object-I/O helper failed");
            }
        };
        serde_json::from_slice(&response_bytes)
            .map_err(|_| anyhow!("checkpoint object-I/O helper returned an invalid response"))
    }
}

#[cfg(test)]
type TestBeforeCheckpointRefCas = Arc<
    dyn Fn(CheckpointAttemptIndexSnapshot) -> futures::future::BoxFuture<'static, Result<()>>
        + Send
        + Sync,
>;

/// Manages object history using an orphan branch and Git Tree structure.
///
/// The default branch (`libra/intent`) stores **all** AI workflow objects,
/// running in parallel with the normal code history (`refs/heads/*`).
/// This is initialised during `libra init` so both branches exist from the start.
///
/// Structure (Commit -> Tree):
///   ├── intent/
///   │   └── <intent_id>
///   ├── task/
///   │   └── <task_id>
///   ├── run/
///   │   └── <run_id>
///   ├── plan/
///   │   └── <plan_id>
///   └── …
///
/// The manager is cheap to clone (all state lives behind `Arc` or owned
/// `String`/`PathBuf`) and is safe to share across async tasks. Concurrent
/// `append` calls on the same manager are serialised via the SQLite-side
/// CAS in [`Self::update_ref_if_matches`].
pub struct HistoryManager {
    #[cfg_attr(not(test), allow(dead_code))] // read by cfg(test) reachability/get_storage paths
    storage: Arc<dyn Storage + Send + Sync>,
    repo_path: PathBuf,
    db_conn: Arc<DatabaseConnection>,
    /// The reference name this manager writes to (e.g. "libra/intent").
    ref_name: String,
    /// Test-only injection point: runs right after the checkpoint CAS loop
    /// reads the head, BEFORE objects are spliced/committed against it —
    /// the deterministic window for `ref_cas_head_changed_rebuilds_commit_
    /// before_retry` to move the head under a competing writer.
    #[cfg(test)]
    pub(crate) test_after_head_read:
        Option<Arc<dyn Fn() -> futures::future::BoxFuture<'static, Result<()>> + Send + Sync>>,
    /// Test-only injection point immediately after all checkpoint objects have
    /// been constructed and before the final ref/companion CAS transaction.
    /// It lets the regression suite expire a workspace lease in the exact
    /// window that must be fenced by the transactional write.
    #[cfg(test)]
    pub(crate) test_before_checkpoint_ref_cas: Option<TestBeforeCheckpointRefCas>,
}

impl HistoryManager {
    /// Build a manager bound to the canonical [`AI_REF`].
    ///
    /// Functional scope:
    /// - Convenience constructor that delegates to [`Self::new_with_ref`]
    ///   with the standard `libra/intent` branch.
    pub fn new(
        storage: Arc<dyn Storage + Send + Sync>,
        repo_path: PathBuf,
        db_conn: Arc<DatabaseConnection>,
    ) -> Self {
        Self::new_with_ref(storage, repo_path, db_conn, AI_REF)
    }

    /// Build a manager bound to an arbitrary ref name.
    ///
    /// Functional scope:
    /// - Used by tests and tooling that need to write a parallel AI history
    ///   under a custom ref (e.g. for staging, comparison, or namespace
    ///   isolation).
    ///
    /// Boundary conditions:
    /// - The ref name is not validated here; callers must ensure it is a
    ///   legal Git ref. The CAS path will fail loudly if the database
    ///   constraint rejects it.
    pub fn new_with_ref(
        storage: Arc<dyn Storage + Send + Sync>,
        repo_path: PathBuf,
        db_conn: Arc<DatabaseConnection>,
        ref_name: impl Into<String>,
    ) -> Self {
        Self {
            storage,
            repo_path,
            db_conn,
            ref_name: ref_name.into(),
            #[cfg(test)]
            test_after_head_read: None,
            #[cfg(test)]
            test_before_checkpoint_ref_cas: None,
        }
    }

    /// Build a manager bound to [`crate::internal::branch::TRACES_BRANCH`].
    ///
    /// KEEP capture writers must use this instead of [`Self::new`], which
    /// binds the intent ref [`AI_REF`].
    pub fn for_traces(
        storage: Arc<dyn Storage + Send + Sync>,
        repo_path: PathBuf,
        db_conn: Arc<DatabaseConnection>,
    ) -> Self {
        Self::new_with_ref(
            storage,
            repo_path,
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        )
    }

    /// Hand back a clone of the underlying SeaORM connection.
    ///
    /// Functional scope:
    /// - Convenience accessor for callers that need to issue auxiliary
    ///   queries against the same database (e.g. listing references for the
    ///   agent clients) without having to thread a separate `Arc` around.
    pub fn database_connection(&self) -> DatabaseConnection {
        self.db_conn.as_ref().clone()
    }

    /// Initialise the AI orphan branch with an empty tree commit.
    ///
    /// This should be called once during `libra init` so that the AI ref
    /// exists from the start (parallel to `refs/heads/<branch>`).
    /// If the ref already exists this is a no-op.
    ///
    /// Functional scope:
    /// - Writes a single empty-tree commit and points the ref at it. The
    ///   commit has no parents (it is the root of the orphan branch) and
    ///   uses the canonical `Libra <ai@libra>` signatures so authorship is
    ///   traceable.
    ///
    /// Boundary conditions:
    /// - Returns early if the ref already exists; this makes the call
    ///   idempotent and safe to invoke from `libra init` regardless of
    ///   whether previous initialisations completed.
    /// - Surfaces errors from object serialisation, blob writing, or the
    ///   ref CAS so the caller can present an actionable message.
    pub async fn init_branch(&self) -> Result<()> {
        // Already initialised — nothing to do.
        if self.resolve_history_head().await?.is_some() {
            return Ok(());
        }

        // Write an empty tree.
        let empty_tree_hash = self.write_tree(&[])?;

        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "ai@libra".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "ai@libra".to_string(),
        );

        let commit = Commit::new(
            author,
            committer,
            empty_tree_hash,
            vec![],
            "Initialize AI history branch",
        );

        let commit_data = commit
            .to_data()
            .context("Failed to serialize AI history init commit")?;
        let commit_hash = write_git_object(&self.repo_path, "commit", &commit_data)?;
        self.update_ref(&self.ref_name, commit_hash).await?;

        Ok(())
    }

    /// Return the ref name this manager writes to.
    ///
    /// Functional scope:
    /// - Useful for diagnostics, log messages, and agent-client labels that need to
    ///   present the active AI history branch to the user.
    pub fn ref_name(&self) -> &str {
        &self.ref_name
    }

    /// Persist a typed AI object blob onto this manager's ref if one is not
    /// already present.
    pub async fn persist_typed_object(
        &self,
        object_type: &str,
        object_id: &str,
        blob_hash: ObjectHash,
    ) -> Result<(ObjectHash, bool)> {
        if let Some(existing) = self.get_object_hash(object_type, object_id).await? {
            return Ok((existing, true));
        }
        self.append(object_type, object_id, blob_hash).await?;
        Ok((blob_hash, false))
    }

    /// Append an object to the history log.
    /// This operation is synchronous (commits immediately) for the MVP.
    ///
    /// Functional scope:
    /// - Implements the read-merge-CAS loop:
    ///   1. Read the current head.
    ///   2. Write a new commit that adds `<object_type>/<object_id>`
    ///      (replacing any prior entry under that path).
    ///   3. CAS the ref forward.
    /// - Reuses [`Self::create_append_commit`] for splice logic and
    ///   [`Self::update_ref_if_matches`] for the optimistic ref update.
    ///
    /// Boundary conditions:
    /// - Retries up to [`HISTORY_HEAD_CONFLICT_MAX_RETRIES`] times when a
    ///   concurrent writer advances the ref between read and CAS. After the
    ///   bound is exhausted the call fails with a contextual error so the
    ///   caller can decide whether to back off and retry.
    /// - The intermediate commit objects from failed CAS attempts remain in
    ///   the object database as garbage; they are unreachable and will be
    ///   collected by the next `libra gc` cycle.
    ///
    /// See: `tests::test_history_append_simple` and
    /// `tests::test_update_ref_if_matches_rejects_stale_history_head`.
    pub async fn append(
        &self,
        object_type: &str,
        object_id: &str,
        blob_hash: ObjectHash,
    ) -> Result<()> {
        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            // Phase 1: snapshot the head we are racing against.
            let parent_commit_id = self.resolve_history_head().await?;
            // Phase 2: build the new commit on top of the snapshot.
            let commit_hash =
                self.create_append_commit(parent_commit_id, object_type, object_id, blob_hash)?;

            // Phase 3: atomically advance the ref iff its current value still
            // equals the snapshot. On `HeadChanged`, restart from phase 1.
            match self
                .update_ref_if_matches(&self.ref_name, parent_commit_id, commit_hash)
                .await?
            {
                RefUpdateOutcome::Updated => return Ok(()),
                RefUpdateOutcome::HeadChanged if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES => {
                    continue;
                }
                RefUpdateOutcome::HeadChanged => {
                    return Err(anyhow!(
                        "history head changed repeatedly while appending {}/{}",
                        object_type,
                        object_id
                    ));
                }
            }
        }

        unreachable!("head conflict retry loop must return on success or terminal error")
    }

    /// Retrieve the object hash for a given type and ID from the current history.
    ///
    /// Functional scope:
    /// - Resolves the head commit, walks `<root_tree>/<object_type>/<object_id>`,
    ///   and returns the leaf blob hash if it exists.
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when the ref is not initialised, when no
    ///   subtree exists for `object_type`, or when the `object_id` entry is
    ///   missing under that subtree.
    /// - Surfaces `Err` only for object-store / parse failures.
    pub async fn get_object_hash(
        &self,
        object_type: &str,
        object_id: &str,
    ) -> Result<Option<ObjectHash>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            if let Some(type_entry) = root_items.iter().find(|item| item.name == object_type) {
                let type_items = self.load_tree(&type_entry.id)?;
                if let Some(item) = type_items.iter().find(|item| item.name == object_id) {
                    return Ok(Some(item.id));
                }
            }
        }
        Ok(None)
    }

    /// Find an object by ID across all types in the history.
    /// Returns (hash, type).
    ///
    /// Functional scope:
    /// - Convenience wrapper around [`Self::find_object_hashes`] that
    ///   returns only the first match.
    ///
    /// Boundary conditions:
    /// - When the same object id exists under multiple type subtrees the
    ///   caller has no control over which is chosen; use
    ///   [`Self::find_object_hashes`] when a deterministic tie-break is
    ///   required.
    pub async fn find_object_hash(&self, object_id: &str) -> Result<Option<(ObjectHash, String)>> {
        Ok(self.find_object_hashes(object_id).await?.into_iter().next())
    }

    /// Find all objects that share the same object ID across history types.
    ///
    /// Functional scope:
    /// - Walks every type subtree under the head root tree and collects
    ///   `(blob_hash, type_name)` tuples for every subtree containing
    ///   `object_id`.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised or the id
    ///   does not appear under any type.
    ///
    /// See: `tests::test_find_object_hashes_returns_all_matching_types`.
    pub async fn find_object_hashes(&self, object_id: &str) -> Result<Vec<(ObjectHash, String)>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            let mut matches = Vec::new();
            for type_entry in root_items {
                let type_items = self.load_tree(&type_entry.id)?;
                if let Some(item) = type_items.iter().find(|item| item.name == object_id) {
                    matches.push((item.id, type_entry.name.clone()));
                }
            }
            return Ok(matches);
        }
        Ok(Vec::new())
    }

    /// List all objects of a specific type from the current history.
    /// Returns a list of (object_id, object_hash).
    ///
    /// Functional scope:
    /// - Loads the head commit's `<object_type>` subtree and yields its
    ///   contents as `(name, blob_hash)` pairs in tree-order.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised or no
    ///   subtree exists for `object_type`.
    pub async fn list_objects(&self, object_type: &str) -> Result<Vec<(String, ObjectHash)>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let root_items = self.load_commit_tree(&parent_id)?;
            if let Some(type_entry) = root_items.iter().find(|item| item.name == object_type) {
                let type_items = self.load_tree(&type_entry.id)?;
                return Ok(type_items
                    .into_iter()
                    .map(|item| (item.name, item.id))
                    .collect());
            }
        }
        Ok(Vec::new())
    }

    /// List all object types present at the current history head.
    ///
    /// Functional scope:
    /// - Returns the names of every top-level subtree under the head root,
    ///   sorted lexicographically for stable output.
    ///
    /// Boundary conditions:
    /// - Returns an empty vector when the ref is not initialised. The empty
    ///   tree case (initialised ref with no objects) likewise yields an
    ///   empty vector.
    ///
    /// See: `tests::test_list_object_types_returns_sorted_types`.
    pub async fn list_object_types(&self) -> Result<Vec<String>> {
        let parent_commit_id = self.resolve_history_head().await?;
        if let Some(parent_id) = parent_commit_id {
            let mut root_items = self.load_commit_tree(&parent_id)?;
            root_items.sort_by(|a, b| a.name.cmp(&b.name));
            return Ok(root_items.into_iter().map(|item| item.name).collect());
        }
        Ok(Vec::new())
    }

    /// Resolve the current head commit of the AI history ref.
    ///
    /// Functional scope:
    /// - Queries the `reference` table for the row that matches
    ///   `(name=ref_name, kind=Branch)` and parses its `commit` column into
    ///   an [`ObjectHash`].
    /// - Tolerates transient SQLite-busy errors with a bounded linear
    ///   backoff governed by [`SQLITE_BUSY_MAX_RETRIES`] /
    ///   [`SQLITE_BUSY_RETRY_BASE_MS`].
    ///
    /// Boundary conditions:
    /// - Returns `Ok(None)` when the ref row is missing or its `commit`
    ///   column is `NULL` (the ref exists but points nowhere yet).
    /// - Returns `Err` if the stored commit string is not a valid object
    ///   hash — this indicates database corruption and the caller should
    ///   surface it rather than silently treating it as missing.
    pub async fn resolve_history_head(&self) -> Result<Option<ObjectHash>> {
        self.resolve_history_head_until(None).await
    }

    /// Deadline-aware variant used only by checkpoint append.  Ordinary
    /// history readers retain the public unbounded API above; an import must
    /// not wait through the connection's SQLite busy timeout before it can
    /// decide that its capture budget expired.
    async fn resolve_history_head_until(
        &self,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<Option<ObjectHash>> {
        let mut attempt = 0;
        let ref_model = loop {
            let query = async {
                reference::Entity::find()
                    .filter(reference::Column::Name.eq(&self.ref_name))
                    .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                    .one(&*self.db_conn)
                    .await
                    .context("Failed to query history head")
            };
            match await_checkpoint_precommit_read_until(
                deadline,
                "query checkpoint history head",
                query,
            )
            .await
            {
                Ok(found) => break found,
                Err(err) if anyhow_is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    attempt += 1;
                    // Linear backoff (BASE * attempt) — see SQLITE_BUSY_* constants.
                    wait_for_checkpoint_sqlite_retry_until(
                        deadline,
                        Duration::from_millis(SQLITE_BUSY_RETRY_BASE_MS * attempt as u64),
                    )
                    .await?;
                }
                Err(err) => return Err(err),
            }
        };

        match ref_model {
            Some(model) => match model.commit {
                Some(commit_hash) => crate::internal::ai::util::parse_repo_object_id(&commit_hash)
                    .map(Some)
                    .map_err(|e| anyhow!("Invalid commit hash in DB: {}", e)),
                None => Ok(None),
            },
            None => Ok(None),
        }
    }

    /// Load the root tree of a commit by parsing its `tree <hash>` header
    /// line.
    ///
    /// Functional scope:
    /// - Reads the commit blob, scans its text lines for the leading
    ///   `tree ` header, parses the referenced tree, and returns its items.
    ///
    /// Boundary conditions:
    /// - Returns an error when the commit blob is missing the `tree`
    ///   header. That should never happen for objects we wrote ourselves
    ///   but we guard against repository corruption.
    fn load_commit_tree(&self, commit_id: &ObjectHash) -> Result<Vec<TreeItem>> {
        let data = read_git_object(&self.repo_path, commit_id)?;
        // Commit format: tree <hash>\nparent...
        let content = String::from_utf8_lossy(&data);
        for line in content.lines() {
            if let Some(hash_str) = line.strip_prefix("tree ") {
                let tree_hash = crate::internal::ai::util::parse_repo_object_id(hash_str)
                    .map_err(|e| anyhow!("Invalid tree hash in commit: {}", e))?;
                return self.load_tree(&tree_hash);
            }
        }
        Err(anyhow!("Commit has no tree"))
    }

    /// Load and parse a tree object's items.
    ///
    /// Functional scope:
    /// - Thin wrapper around `Tree::from_bytes` for the AI-history call
    ///   sites; centralised so all tree reads go through the same error
    ///   path.
    fn load_tree(&self, tree_id: &ObjectHash) -> Result<Vec<TreeItem>> {
        let data = read_git_object(&self.repo_path, tree_id)?;

        let tree = Tree::from_bytes(&data, *tree_id)?;
        Ok(tree.tree_items)
    }

    /// Serialise tree items into Git's binary tree format and persist as
    /// an object.
    ///
    /// Functional scope:
    /// - Encodes each item as `<mode> <name>\0<binary_hash>` per the Git
    ///   tree spec, concatenates them in caller-provided order, and writes
    ///   the bytes to the object database under type `tree`.
    ///
    /// Boundary conditions:
    /// - Items must already be sorted by the caller (`append`/the splice
    ///   helpers do this). Unsorted items would still parse but would
    ///   produce a different tree hash than canonical Git.
    /// - Rejects hashes whose binary length is not 20 (SHA-1) or 32
    ///   (SHA-256) — protection against malformed inputs that would
    ///   otherwise corrupt the object store.
    fn write_tree(&self, tree_items: &[TreeItem]) -> Result<ObjectHash> {
        Ok(self.write_tree_with_size(tree_items)?.0)
    }

    /// Encode `tree_items` as a Git tree, write the object, and return
    /// `(hash, encoded_size)`. The size is the *content* length (no Git
    /// header) — same convention as `object_index.o_size`.
    ///
    /// Used by legacy/unscoped capture rewrites which need the byte count to
    /// pair with [`crate::utils::client_storage::enqueue_agent_blob_object_index_update`],
    /// and by prune rebuilds which retain an in-memory index intent for their
    /// final ref/catalog transaction. Scoped checkpoint appends use
    /// `write_tree_indexed_for_attempt` and likewise defer their index rows.
    fn write_tree_with_size(&self, tree_items: &[TreeItem]) -> Result<(ObjectHash, usize)> {
        let mut ignored = HashSet::new();
        self.write_tree_with_size_tracked(tree_items, &mut ignored)
    }

    fn write_tree_with_size_tracked(
        &self,
        tree_items: &[TreeItem],
        newly_written: &mut HashSet<String>,
    ) -> Result<(ObjectHash, usize)> {
        let data = Self::encode_tree_data(tree_items)?;
        let size = data.len();
        let (hash, was_created) = write_git_object_with_status(&self.repo_path, "tree", &data)?;
        if was_created {
            newly_written.insert(hash.to_string());
        }
        Ok((hash, size))
    }

    fn encode_tree_data(tree_items: &[TreeItem]) -> Result<Vec<u8>> {
        let mut data = Vec::new();
        for item in tree_items {
            let mode_str = match item.mode {
                TreeItemMode::Tree => "40000",
                TreeItemMode::Blob => "100644",
                TreeItemMode::BlobExecutable => "100755",
                TreeItemMode::Link => "120000",
                TreeItemMode::Commit => "160000",
            };
            data.extend_from_slice(mode_str.as_bytes());
            data.push(b' ');
            data.extend_from_slice(item.name.as_bytes());
            data.push(0);
            let hash_hex = item.id.to_string();
            let hash_bytes =
                hex::decode(&hash_hex).map_err(|e| anyhow!("Invalid hash hex: {}", e))?;
            // 20 bytes for SHA-1, 32 for SHA-256. Anything else is a
            // signal that we are about to corrupt the object database.
            if hash_bytes.len() != 20 && hash_bytes.len() != 32 {
                return Err(anyhow!("Invalid object hash length: {}", hash_bytes.len()));
            }
            data.extend_from_slice(&hash_bytes);
        }
        Ok(data)
    }

    /// Write one replacement tree while rebuilding a checkpoint history.
    ///
    /// Unlike ordinary unscoped writes, prune rewrites do not enqueue a
    /// durable repair marker here: the same prune soon acquires the deletion
    /// fence, and its own marker would be indistinguishable from concurrent
    /// work.  The caller carries this intent into the ref/catalog prune
    /// transaction, where it becomes visible atomically with the rewrite.
    fn write_tree_indexed_for_prune_rewrite(
        &self,
        tree_items: &[TreeItem],
        object_index_intents: &mut Vec<CheckpointObjectIndexIntent>,
    ) -> Result<ObjectHash> {
        let (hash, size) = self.write_tree_with_size(tree_items)?;
        object_index_intents.push(CheckpointObjectIndexIntent {
            oid: hash.to_string(),
            object_type: "tree".to_string(),
            size: i64::try_from(size)
                .context("rewritten checkpoint tree exceeds object-index size range")?,
        });
        Ok(hash)
    }

    async fn load_traces_writer_fence(
        &self,
        session_id: &str,
        attempt_id: &str,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<TracesWriterFence> {
        let entry = await_checkpoint_precommit_read_until(
            deadline,
            "load checkpoint writer marker generation",
            crate::internal::metadata::MetadataKv::get_with_conn(
                self.db_conn.as_ref(),
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                session_id,
                attempt_id,
            ),
        )
        .await?
        .ok_or_else(|| {
            checkpoint_marker_fenced("checkpoint writer marker is missing before append")
        })?;
        let marker =
            decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)?;
        if marker.cleanup_pending {
            return Err(checkpoint_marker_fenced(
                "checkpoint writer marker entered cleanup before append; retry the operation",
            ));
        }
        let generation = marker.generation.ok_or_else(|| {
            anyhow!(
                "checkpoint writer marker predates generation fencing; wait for it to expire and retry"
            )
        })?;
        Ok(TracesWriterFence {
            session_id: session_id.to_string(),
            attempt_id: attempt_id.to_string(),
            generation,
        })
    }

    fn ensure_marker_matches_fence(
        marker: &TracesInflightMarker,
        fence: &TracesWriterFence,
    ) -> Result<()> {
        if marker.session_id != fence.session_id
            || marker.attempt_id != fence.attempt_id
            || marker.generation.as_deref() != Some(fence.generation.as_str())
            || marker.cleanup_pending
        {
            return Err(checkpoint_marker_fenced(
                "checkpoint writer marker generation was fenced or replaced; retry the operation",
            ));
        }
        Ok(())
    }

    async fn persist_attempt_oid_before_write(
        &self,
        fence: &TracesWriterFence,
        capture_scope: Option<&CaptureScope>,
        oid: &ObjectHash,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<()> {
        // Object-index updates from the preceding object use a background
        // SQLite writer. Optimistically take the marker transaction; if the
        // queue won the lock race, drain it before the bounded retry. Waiting
        // unconditionally here would serialize every object-index update and
        // make multi-turn historical imports miss their total deadline.
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            ensure_checkpoint_append_before_deadline(deadline)?;
            let result: Result<()> = async {
                let txn = begin_checkpoint_write_transaction_until(
                    self.db_conn.as_ref(),
                    deadline,
                    "begin checkpoint object ownership update",
                )
                .await?;
                if let Some(scope) = capture_scope {
                    await_checkpoint_precommit_read_until(
                        deadline,
                        "verify capture workspace lease before checkpoint object ownership update",
                        scope.assert_workspace_fence_live(&txn),
                    )
                    .await?;
                }
                let entry = await_checkpoint_precommit_read_until(
                    deadline,
                    "load checkpoint writer marker before object write",
                    crate::internal::metadata::MetadataKv::get_with_conn(
                        &txn,
                        crate::internal::metadata::MetadataScope::AgentTracesInflight,
                        &fence.session_id,
                        &fence.attempt_id,
                    ),
                )
                .await?
                .ok_or_else(|| {
                    checkpoint_marker_fenced(
                        "checkpoint writer marker disappeared before object write; refusing to create loose objects",
                    )
                })?;
                let mut marker = decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                )?;
                Self::ensure_marker_matches_fence(&marker, fence)?;
                marker.schema_version = marker.schema_version.max(3);
                let oid = oid.to_string();
                if !marker.oids.contains(&oid) {
                    marker.oids.push(oid);
                    marker.oids.sort();
                    ensure_checkpoint_append_before_deadline(deadline)?;
                    let updated = match deadline {
                        Some(deadline) => crate::internal::ai::traces::update_traces_inflight_marker_if_generation_with_capture_scope_until(
                            &txn,
                            capture_scope,
                            &marker,
                            &fence.generation,
                            deadline,
                        )
                        .await,
                        None => update_traces_inflight_marker_if_generation_with_capture_scope(
                            &txn,
                            capture_scope,
                            &marker,
                            &fence.generation,
                        )
                        .await,
                    }
                    .map_err(normalize_checkpoint_marker_deadline)
                    .context("persist checkpoint object ownership before write")?;
                    if !updated {
                        txn.rollback().await.ok();
                        return Err(checkpoint_marker_fenced(
                            "checkpoint writer marker generation changed before object write",
                        ));
                    }
                }
                commit_checkpoint_txn_after_final_authorization(
                    txn,
                    capture_scope,
                    deadline,
                    "commit checkpoint object ownership before write",
                )
                .await?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error)
                    if anyhow_is_sqlite_busy(&error) && attempt < SQLITE_BUSY_MAX_RETRIES =>
                {
                    let now = Instant::now();
                    let drain_deadline = deadline
                        .map(CaptureCommitDeadline::monotonic)
                        .unwrap_or(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET)
                        .min(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET);
                    let _ = crate::utils::client_storage::ClientStorage::wait_for_background_tasks_until(
                        drain_deadline,
                    )
                    .await;
                    wait_for_checkpoint_sqlite_retry_until(
                        deadline,
                        Duration::from_millis(SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1)),
                    )
                    .await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(anyhow!(
            "checkpoint object ownership update exhausted its bounded SQLite retry loop"
        ))
    }

    async fn finalize_attempt_oid_after_write(
        &self,
        fence: &TracesWriterFence,
        capture_scope: Option<&CaptureScope>,
        oid: &ObjectHash,
        was_created: bool,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<()> {
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            ensure_checkpoint_append_before_deadline(deadline)?;
            let result: Result<()> = async {
                let txn = begin_checkpoint_write_transaction_until(
                    self.db_conn.as_ref(),
                    deadline,
                    "begin checkpoint object ownership finalization",
                )
                .await?;
                if let Some(scope) = capture_scope {
                    await_checkpoint_precommit_read_until(
                        deadline,
                        "verify capture workspace lease before checkpoint object ownership finalization",
                        scope.assert_workspace_fence_live(&txn),
                    )
                    .await?;
                }
                let entry = await_checkpoint_precommit_read_until(
                    deadline,
                    "load checkpoint writer marker after object write",
                    crate::internal::metadata::MetadataKv::get_with_conn(
                        &txn,
                        crate::internal::metadata::MetadataScope::AgentTracesInflight,
                        &fence.session_id,
                        &fence.attempt_id,
                    ),
                )
                .await?
                .ok_or_else(|| {
                    checkpoint_marker_fenced(
                        "checkpoint writer marker disappeared after object write; refusing to continue",
                    )
                })?;
                let mut marker = decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                )?;
                Self::ensure_marker_matches_fence(&marker, fence)?;
                marker.schema_version = marker.schema_version.max(3);
                let oid = oid.to_string();
                marker.oids.retain(|candidate| candidate != &oid);
                if was_created && !marker.created_oids.contains(&oid) {
                    marker.created_oids.push(oid);
                    marker.created_oids.sort();
                }
                ensure_checkpoint_append_before_deadline(deadline)?;
                let updated = match deadline {
                    Some(deadline) => crate::internal::ai::traces::update_traces_inflight_marker_if_generation_with_capture_scope_until(
                        &txn,
                        capture_scope,
                        &marker,
                        &fence.generation,
                        deadline,
                    )
                    .await,
                    None => update_traces_inflight_marker_if_generation_with_capture_scope(
                        &txn,
                        capture_scope,
                        &marker,
                        &fence.generation,
                    )
                    .await,
                }
                .map_err(normalize_checkpoint_marker_deadline)
                .context("finalize checkpoint object ownership after write")?;
                if !updated {
                    txn.rollback().await.ok();
                    return Err(checkpoint_marker_fenced(
                        "checkpoint writer marker generation changed after object write",
                    ));
                }
                commit_checkpoint_txn_after_final_authorization(
                    txn,
                    capture_scope,
                    deadline,
                    "commit checkpoint object ownership finalization",
                )
                .await?;
                Ok(())
            }
            .await;
            match result {
                Ok(()) => return Ok(()),
                Err(error)
                    if anyhow_is_sqlite_busy(&error) && attempt < SQLITE_BUSY_MAX_RETRIES =>
                {
                    let now = Instant::now();
                    let drain_deadline = deadline
                        .map(CaptureCommitDeadline::monotonic)
                        .unwrap_or(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET)
                        .min(now + OBJECT_INDEX_FOREGROUND_DRAIN_BUDGET);
                    let _ = crate::utils::client_storage::ClientStorage::wait_for_background_tasks_until(
                        drain_deadline,
                    )
                    .await;
                    wait_for_checkpoint_sqlite_retry_until(
                        deadline,
                        Duration::from_millis(SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1)),
                    )
                    .await?;
                }
                Err(error) => return Err(error),
            }
        }
        Err(anyhow!(
            "checkpoint object ownership finalization exhausted its bounded SQLite retry loop"
        ))
    }

    #[allow(clippy::too_many_arguments)]
    async fn write_indexed_object_for_attempt(
        &self,
        object_type: &str,
        data: &[u8],
        index_type: &str,
        what: &str,
        fence: &TracesWriterFence,
        capture_scope: Option<&CaptureScope>,
        deadline: Option<CaptureCommitDeadline>,
        newly_written: &mut HashSet<String>,
        object_index_intents: &mut Vec<CheckpointObjectIndexIntent>,
    ) -> Result<ObjectHash> {
        let expected_oid = git_object_hash(object_type, data);
        let oid_string = expected_oid.to_string();
        let needs_preclaim = if deadline.is_some() {
            // A foreground existence probe can itself block on FUSE/NFS.
            // Preclaiming an already-existing object is harmless: successful
            // helper completion removes that transient ownership row.
            true
        } else {
            !self
                .repo_path
                .join("objects")
                .join(&oid_string[..2])
                .join(&oid_string[2..])
                .exists()
        };
        if needs_preclaim {
            self.persist_attempt_oid_before_write(fence, capture_scope, &expected_oid, deadline)
                .await?;
        }
        if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
            bail!("checkpoint append exceeded the historical import execution deadline");
        }
        let (oid, was_created) = if let Some(deadline) = deadline {
            use base64::{Engine as _, engine::general_purpose::STANDARD};

            let response = invoke_checkpoint_object_helper(
                &self.repo_path,
                CheckpointObjectIoOperation::Write {
                    object_type: object_type.to_string(),
                    data_base64: STANDARD.encode(data),
                },
                deadline.monotonic(),
            )
            .await
            .with_context(|| format!("failed to write checkpoint {what} {object_type}"))?;
            match response {
                CheckpointObjectIoHelperResponse::Written { oid, was_created } => (
                    crate::internal::ai::util::parse_repo_object_id(&oid).map_err(|error| {
                        anyhow!("helper returned invalid checkpoint oid '{oid}': {error}")
                    })?,
                    was_created,
                ),
                CheckpointObjectIoHelperResponse::Error { code } => {
                    bail!(
                        "failed to write checkpoint {what} {object_type}: {}",
                        code.user_message()
                    )
                }
                CheckpointObjectIoHelperResponse::Read { .. } => {
                    bail!("checkpoint object-I/O helper returned a read response for a write")
                }
                CheckpointObjectIoHelperResponse::Verified { .. } => {
                    bail!("checkpoint object-I/O helper returned a verify response for a write")
                }
            }
        } else {
            write_git_object_with_status(&self.repo_path, object_type, data)
                .with_context(|| format!("failed to write checkpoint {what} {object_type}"))?
        };
        if oid != expected_oid {
            bail!("checkpoint object hash changed between ownership registration and write");
        }
        if was_created {
            newly_written.insert(oid.to_string());
        }
        self.finalize_attempt_oid_after_write(fence, capture_scope, &oid, was_created, deadline)
            .await?;
        let size = i64::try_from(data.len())
            .context("checkpoint object exceeds object-index size range")?;
        if capture_scope.is_some() {
            // Scoped capture publishes these only from the final ref/catalog
            // CAS transaction. A lost CAS or workspace fence must leave
            // loose objects as GC-only residue, never cloud-visible rows.
            object_index_intents.push(CheckpointObjectIndexIntent {
                oid: oid.to_string(),
                object_type: index_type.to_string(),
                size,
            });
        } else {
            // Preserve the legacy/unscoped writer's durable repair-marker
            // behavior; only scoped captures can bind this index work to a
            // final capture transaction.
            crate::utils::client_storage::enqueue_agent_blob_object_index_update(
                &self.repo_path,
                &oid.to_string(),
                index_type,
                size,
            )
            .with_context(|| format!("register durable object-index repair for {what} {oid}"))?;
        }
        Ok(oid)
    }

    async fn read_checkpoint_object_for_attempt(
        &self,
        oid: &ObjectHash,
        expected_type: &str,
        deadline: Option<Instant>,
    ) -> Result<Vec<u8>> {
        let Some(deadline) = deadline else {
            return read_git_object(&self.repo_path, oid).map_err(Into::into);
        };
        let response = invoke_checkpoint_object_helper(
            &self.repo_path,
            CheckpointObjectIoOperation::Read {
                oid: oid.to_string(),
                expected_type: expected_type.to_string(),
            },
            deadline,
        )
        .await?;
        match response {
            CheckpointObjectIoHelperResponse::Read {
                oid: returned_oid,
                object_type,
                data_base64,
            } => {
                use base64::{Engine as _, engine::general_purpose::STANDARD};

                if returned_oid != oid.to_string() {
                    bail!("checkpoint object-I/O helper returned the wrong object id");
                }
                if object_type != expected_type {
                    bail!(
                        "checkpoint object-I/O helper returned type '{object_type}', expected '{expected_type}'"
                    );
                }
                STANDARD
                    .decode(data_base64)
                    .context("decode checkpoint object-I/O read payload")
            }
            CheckpointObjectIoHelperResponse::Error { code } => {
                bail!(
                    "failed to read checkpoint object {oid}: {}",
                    code.user_message()
                )
            }
            CheckpointObjectIoHelperResponse::Written { .. } => {
                bail!("checkpoint object-I/O helper returned a write response for a read")
            }
            CheckpointObjectIoHelperResponse::Verified { .. } => {
                bail!("checkpoint object-I/O helper returned a verify response for a read")
            }
        }
    }

    async fn load_commit_tree_for_attempt(
        &self,
        commit_id: &ObjectHash,
        deadline: Option<Instant>,
    ) -> Result<Vec<TreeItem>> {
        let data = self
            .read_checkpoint_object_for_attempt(commit_id, "commit", deadline)
            .await?;
        let content = String::from_utf8_lossy(&data);
        for line in content.lines() {
            if let Some(hash_str) = line.strip_prefix("tree ") {
                let tree_hash = crate::internal::ai::util::parse_repo_object_id(hash_str)
                    .map_err(|error| anyhow!("Invalid tree hash in commit: {error}"))?;
                return self.load_tree_for_attempt(&tree_hash, deadline).await;
            }
        }
        bail!("Commit has no tree")
    }

    async fn load_tree_for_attempt(
        &self,
        tree_id: &ObjectHash,
        deadline: Option<Instant>,
    ) -> Result<Vec<TreeItem>> {
        let data = self
            .read_checkpoint_object_for_attempt(tree_id, "tree", deadline)
            .await?;
        Ok(Tree::from_bytes(&data, *tree_id)?.tree_items)
    }

    async fn write_tree_indexed_for_attempt(
        &self,
        tree_items: &[TreeItem],
        fence: &TracesWriterFence,
        capture_scope: Option<&CaptureScope>,
        deadline: Option<CaptureCommitDeadline>,
        newly_written: &mut HashSet<String>,
        object_index_intents: &mut Vec<CheckpointObjectIndexIntent>,
    ) -> Result<ObjectHash> {
        let data = Self::encode_tree_data(tree_items)?;
        self.write_indexed_object_for_attempt(
            "tree",
            &data,
            "tree",
            "tree",
            fence,
            capture_scope,
            deadline,
            newly_written,
            object_index_intents,
        )
        .await
    }

    fn create_append_commit(
        &self,
        parent_commit_id: Option<ObjectHash>,
        object_type: &str,
        object_id: &str,
        blob_hash: ObjectHash,
    ) -> Result<ObjectHash> {
        let mut root_items = if let Some(parent_id) = parent_commit_id {
            self.load_commit_tree(&parent_id)?
        } else {
            Vec::new()
        };

        let type_tree_entry = root_items
            .iter()
            .find(|item| item.name == object_type)
            .cloned();

        let mut type_items = if let Some(entry) = type_tree_entry {
            self.load_tree(&entry.id)?
        } else {
            Vec::new()
        };

        let new_item = TreeItem::new(TreeItemMode::Blob, blob_hash, object_id.to_string());
        type_items.retain(|item| item.name != object_id);
        type_items.push(new_item);
        type_items.sort_by(|a, b| a.name.cmp(&b.name));

        let type_tree_hash = self.write_tree(&type_items)?;

        let new_root_item =
            TreeItem::new(TreeItemMode::Tree, type_tree_hash, object_type.to_string());
        root_items.retain(|item| item.name != object_type);
        root_items.push(new_root_item);
        root_items.sort_by(|a, b| a.name.cmp(&b.name));

        let root_tree_hash = self.write_tree(&root_items)?;

        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "history@libra".to_string(),
        );

        let signature = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "history@libra".to_string(),
        );

        let message = format!("Update {}/{}", object_type, object_id);
        let parents = parent_commit_id.into_iter().collect::<Vec<_>>();
        let commit = Commit::new(author, signature, root_tree_hash, parents, &message);
        let commit_data = commit
            .to_data()
            .context("Failed to serialize AI history commit")?;
        write_git_object(&self.repo_path, "commit", &commit_data)
            .context("Failed to write AI history commit")
    }

    async fn update_ref(&self, ref_name: &str, hash: ObjectHash) -> Result<()> {
        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let txn: DatabaseTransaction =
                match crate::internal::db::begin_write_transaction(self.db_conn.as_ref()).await {
                    Ok(txn) => txn,
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue;
                    }
                    Err(err) => return Err(err).context("Failed to begin transaction"),
                };

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query reference"),
            };

            let had_existing = existing.is_some();
            let write_result = if let Some(model) = existing {
                let mut active: reference::ActiveModel = model.into();
                active.commit = Set(Some(hash.to_string()));
                active.update(&txn).await.map(|_| ())
            } else {
                let new_ref = reference::ActiveModel {
                    name: Set(Some(ref_name.to_string())),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some(hash.to_string())),
                    remote: Set(None),
                    ..Default::default()
                };
                new_ref.insert(&txn).await.map(|_| ())
            };

            match write_result {
                Ok(()) => {}
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => {
                    let context = if had_existing {
                        "Failed to update reference"
                    } else {
                        "Failed to insert reference"
                    };
                    return Err(err).context(context);
                }
            }

            match txn.commit().await {
                Ok(()) => return Ok(()),
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => return Err(err).context("Failed to commit transaction"),
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    async fn update_ref_if_matches(
        &self,
        ref_name: &str,
        expected_head: Option<ObjectHash>,
        new_hash: ObjectHash,
    ) -> Result<RefUpdateOutcome> {
        self.update_ref_if_matches_with_extra(
            ref_name,
            expected_head,
            new_hash,
            None,
            None,
            None,
            None,
        )
        .await
    }

    /// Conditional ref update with optional transactional companion writes
    /// (plan-20260713 ADR-DR-10). When `extra` is provided, its SQL runs in
    /// the SAME transaction as the ref write — after the CAS row update
    /// succeeds, before COMMIT — so catalog/claim/revision state can never
    /// diverge from the ref. An `extra` error rolls the whole transaction
    /// back (the ref does not move) and propagates as a hard error, not a
    /// `HeadChanged` retry.
    #[allow(clippy::too_many_arguments)]
    async fn update_ref_if_matches_with_extra(
        &self,
        ref_name: &str,
        expected_head: Option<ObjectHash>,
        new_hash: ObjectHash,
        extra: Option<(&dyn TracesTxnExtra, &TracesCommitCtx)>,
        deadline: Option<CaptureCommitDeadline>,
        marker_fence: Option<&TracesWriterFence>,
        capture_scope: Option<&CaptureScope>,
    ) -> Result<RefUpdateOutcome> {
        let expected_commit = expected_head.map(|hash| hash.to_string());
        let new_commit = new_hash.to_string();

        for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            ensure_checkpoint_append_before_deadline(deadline)?;
            let txn: DatabaseTransaction = match begin_checkpoint_write_transaction_until(
                self.db_conn.as_ref(),
                deadline,
                "begin checkpoint ref update transaction",
            )
            .await
            {
                Ok(txn) => txn,
                Err(err) if anyhow_is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    wait_for_checkpoint_sqlite_retry_until(
                        deadline,
                        Duration::from_millis(SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1)),
                    )
                    .await?;
                    continue;
                }
                Err(err) => return Err(err),
            };

            if let Some(scope) = capture_scope
                && let Err(error) = await_checkpoint_precommit_read_until(
                    deadline,
                    "verify capture workspace lease before ref update",
                    scope.assert_workspace_fence_live(&txn),
                )
                .await
            {
                txn.rollback().await.ok();
                return Err(error);
            }

            // An expired ordinary marker may have been fenced and retired by
            // crash recovery while this writer was stalled. The marker check
            // rides the same SQLite writer transaction as the ref/catalog CAS:
            // cleanup wins first => this writer cannot publish; this writer
            // wins first => cleanup observes the committed root/catalog.
            if let Some(marker_fence) = marker_fence {
                let entry = match await_checkpoint_precommit_read_until(
                    deadline,
                    "revalidate checkpoint writer marker before ref update",
                    crate::internal::metadata::MetadataKv::get_with_conn(
                        &txn,
                        crate::internal::metadata::MetadataScope::AgentTracesInflight,
                        &marker_fence.session_id,
                        &marker_fence.attempt_id,
                    ),
                )
                .await
                {
                    Ok(entry) => entry,
                    Err(error) => {
                        txn.rollback().await.ok();
                        return Err(error);
                    }
                };
                let Some(entry) = entry else {
                    txn.rollback().await.ok();
                    return Err(checkpoint_marker_fenced(
                        "checkpoint writer marker was fenced before ref update; retry the operation",
                    ));
                };
                let marker = decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                )?;
                if let Err(error) = Self::ensure_marker_matches_fence(&marker, marker_fence) {
                    txn.rollback().await.ok();
                    return Err(error.context("revalidate marker generation before ref update"));
                }
            }

            let query = async {
                reference::Entity::find()
                    .filter(reference::Column::Name.eq(ref_name))
                    .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                    .one(&txn)
                    .await
                    .context("Failed to query reference")
            };
            let existing = match await_checkpoint_precommit_read_until(
                deadline,
                "query checkpoint reference before compare-and-swap",
                query,
            )
            .await
            {
                Ok(existing) => existing,
                Err(err) if anyhow_is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    wait_for_checkpoint_sqlite_retry_until(
                        deadline,
                        Duration::from_millis(SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1)),
                    )
                    .await?;
                    continue;
                }
                Err(err) => {
                    txn.rollback().await.ok();
                    return Err(err);
                }
            };

            // The following SQL mutates the ref.  Do not place it under an
            // external timeout: the final authorization below will roll it
            // back if the deadline crossed while it ran.
            ensure_checkpoint_append_before_deadline(deadline)?;
            let write_result = match existing {
                Some(model) if model.commit != expected_commit => {
                    let _ = txn.rollback().await;
                    return Ok(RefUpdateOutcome::HeadChanged);
                }
                Some(model) => {
                    let mut update = reference::Entity::update_many()
                        .filter(reference::Column::Id.eq(model.id))
                        .filter(reference::Column::Name.eq(ref_name))
                        .filter(reference::Column::Kind.eq(ConfigKind::Branch));
                    update = match expected_commit.as_ref() {
                        Some(commit) => update.filter(reference::Column::Commit.eq(commit.clone())),
                        None => update.filter(reference::Column::Commit.is_null()),
                    };

                    update
                        .col_expr(
                            reference::Column::Commit,
                            Expr::value(Some(new_commit.clone())),
                        )
                        .exec(&txn)
                        .await
                        .map(Some)
                }
                None if expected_commit.is_some() => {
                    let _ = txn.rollback().await;
                    return Ok(RefUpdateOutcome::HeadChanged);
                }
                None => {
                    let new_ref = reference::ActiveModel {
                        name: Set(Some(ref_name.to_string())),
                        kind: Set(ConfigKind::Branch),
                        commit: Set(Some(new_commit.clone())),
                        remote: Set(None),
                        ..Default::default()
                    };
                    match new_ref.insert(&txn).await {
                        Ok(_) => Ok(None),
                        Err(err) if is_sqlite_unique_violation(&err) => {
                            let _ = txn.rollback().await;
                            return Ok(RefUpdateOutcome::HeadChanged);
                        }
                        Err(err) => Err(err),
                    }
                }
            };

            let rows_affected = match write_result {
                Ok(rows_affected) => rows_affected,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    wait_for_checkpoint_sqlite_retry_until(
                        deadline,
                        Duration::from_millis(SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1)),
                    )
                    .await?;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to compare-and-swap history head"),
            };

            if rows_affected.is_some_and(|result| result.rows_affected != 1) {
                let _ = txn.rollback().await;
                return Ok(RefUpdateOutcome::HeadChanged);
            }

            // ADR-DR-10: companion writes ride the ref transaction. A
            // failure here must NOT move the ref — roll back and fail
            // closed (no HeadChanged retry: the failure is a gate/fence
            // violation or DB fault, not a CAS race).
            ensure_checkpoint_append_before_deadline(deadline)?;
            if let Some((extra, ctx)) = extra
                && let Err(err) = extra.apply(&txn, ctx).await
            {
                let _ = txn.rollback().await;
                return Err(err.context(CheckpointCompanionTransactionFailed));
            }

            // This is the last SQL before COMMIT. It atomically tests the
            // immutable SQLite deadline and, where present, the workspace
            // lease fence; its success is followed only by an unbounded COMMIT
            // acknowledgement so SQLx cannot commit after a cancelled timeout.
            match commit_checkpoint_txn_after_final_authorization(
                txn,
                capture_scope,
                deadline,
                "Failed to commit transaction",
            )
            .await
            {
                Ok(()) => return Ok(RefUpdateOutcome::Updated),
                Err(err) if anyhow_is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    // `commit_checkpoint_txn_after_final_authorization`
                    // waited for the COMMIT acknowledgement before returning;
                    // only the subsequent retry backoff is cancellable.
                    wait_for_checkpoint_sqlite_retry_until(
                        deadline,
                        Duration::from_millis(SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1)),
                    )
                    .await?;
                }
                Err(err) => return Err(err),
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    /// Append a checkpoint commit to this manager's ref.
    ///
    /// AG-20 (E4-libra layout). Builds the layered tree
    ///
    /// ```text
    /// checkpoint/<id[:2]>/<id[2:]>/
    ///   metadata.json
    ///   manifest.json
    ///   events/lifecycle.jsonl
    ///   transcript/<agent_kind>.jsonl        (or `.jsonl.001…` chunks, E5)
    ///   redaction_report.json
    ///   content_hash.txt
    /// ```
    ///
    /// and merges it into the parent commit's tree so successive checkpoints
    /// accumulate (rather than overwrite). The resulting commit message
    /// carries `Libra-*` trailers per the design spec (see
    /// `docs/development/commands/_general.md` §3.3). Pre-AG-20 checkpoints
    /// (metadata.json + `transcript/<provider>` only) remain readable as
    /// legacy-v1; this writer never emits that layout again.
    ///
    /// Returns the freshly-written commit hash plus the OIDs callers need to
    /// stamp onto `agent_checkpoint` (root tree OID and metadata blob OID),
    /// along with span bookkeeping (`cas_retries`, `object_count`).
    pub async fn append_checkpoint_commit(
        &self,
        params: CheckpointCommitParams<'_>,
    ) -> Result<CheckpointCommit> {
        // Durable rejected-object markers are diagnostic/GC work. Appends do
        // only the O(1) exact writer-fence check below; a repository-wide
        // reachability scan here can otherwise impose a permanent 30s+ stall
        // on every checkpoint while making no foreground deletion decision.
        let capture_deadline = params.deadline;
        let writer_fence = self
            .load_traces_writer_fence(params.session_id, params.checkpoint_id, capture_deadline)
            .await?;
        if writer_fence.generation != params.marker_generation {
            return Err(checkpoint_marker_fenced(
                "checkpoint writer marker generation was fenced or replaced before append; retry the operation",
            ));
        }
        let capture_scope = params.capture_scope;
        let mut newly_written = HashSet::new();
        let result = self
            .append_checkpoint_commit_inner(params, &writer_fence, &mut newly_written)
            .await;
        match result {
            Ok(commit) => Ok(commit),
            Err(error) => {
                if let Err(cleanup_error) = self
                    .cleanup_rejected_checkpoint_objects_until(
                        &writer_fence,
                        capture_scope,
                        &newly_written,
                        capture_deadline,
                    )
                    .await
                {
                    // The pre-existing in-flight marker remains the only
                    // durable ownership evidence when this best-effort
                    // recovery registration cannot run. Preserve a typed
                    // deferred-cleanup cause so checkpoint finalization will
                    // not later erase that marker as an ordinary failure.
                    return Err(error.context(RejectedCheckpointCleanupDeferred {
                        reason: format!(
                            "foreground cleanup registration failed: {cleanup_error:#}"
                        ),
                    }));
                }
                Err(error)
            }
        }
    }

    async fn append_checkpoint_commit_inner(
        &self,
        params: CheckpointCommitParams<'_>,
        writer_fence: &TracesWriterFence,
        newly_written: &mut HashSet<String>,
    ) -> Result<CheckpointCommit> {
        // Phase 1: write content blobs once. They are content-addressed, so
        // re-running a CAS retry loop never duplicates them.
        //
        // CEX-EntireIO §14.3 phase-3 item 3: every agent blob is tagged in
        // `object_index` so `libra cloud sync` uploads it to R2. Only the
        // transcript blob(s) carry the distinguished o_type
        // ("agent_transcript"); the JSON sidecars use the standard "blob"
        // tag because cloud sync doesn't filter by o_type — the custom tag
        // exists for downstream tooling that enumerates captured
        // transcripts.
        let deadline = params.deadline;
        let ensure_deadline = || -> Result<()> {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
                bail!("checkpoint append exceeded the historical import execution deadline");
            }
            Ok(())
        };
        ensure_deadline()?;
        let mut object_count: u64 = 0;
        let mut object_index_intents = Vec::new();
        let metadata_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                params.metadata_json.bytes(),
                "blob",
                "metadata.json",
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;
        let events_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                params.lifecycle_events_jsonl.bytes(),
                "blob",
                "events/lifecycle.jsonl",
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;
        let report_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                params.redaction_report_json.bytes(),
                "blob",
                "redaction_report.json",
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;

        // Transcript: E5 line-boundary-safe chunking above the threshold.
        // Small transcripts stay a single `transcript/<agent_kind>.jsonl`
        // file; larger ones split into `.jsonl.001`, `.jsonl.002`, … parts
        // declared (in order) by the manifest's `transcript` role.
        let transcript_bytes = params.transcript_redacted.bytes();
        let threshold = transcript_chunk_threshold();
        let transcript_file_name = format!("{}.jsonl", params.agent_kind);
        let chunks: Vec<&[u8]> = if transcript_bytes.len() > threshold {
            chunk_transcript_line_safe(transcript_bytes, threshold)?
        } else {
            vec![transcript_bytes]
        };
        let chunked = chunks.len() > 1;
        let mut transcript_parts: Vec<TranscriptPartRef> = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            let name = if chunked {
                format!("{}.{:03}", transcript_file_name, index + 1)
            } else {
                transcript_file_name.clone()
            };
            let oid = self
                .write_indexed_object_for_attempt(
                    "blob",
                    chunk,
                    "agent_transcript",
                    "transcript",
                    writer_fence,
                    params.capture_scope,
                    params.deadline,
                    newly_written,
                    &mut object_index_intents,
                )
                .await?;
            object_count += 1;
            ensure_deadline()?;
            transcript_parts.push(TranscriptPartRef {
                name,
                oid,
                byte_len: chunk.len(),
            });
        }

        // content_hash.txt: `sha256:<64-lowercase-hex>` (no trailing
        // newline) over the concatenated bytes of the coverage roles in
        // [`CHECKPOINT_CONTENT_HASH_COVERAGE`] order. The transcript
        // contributes its logical (pre-chunking) byte stream, so the hash
        // is invariant under re-chunking. See the E4-libra section of
        // `docs/development/tracing/agent.md`.
        let content_hash = checkpoint_content_hash(&[
            params.metadata_json.bytes(),
            params.lifecycle_events_jsonl.bytes(),
            transcript_bytes,
            params.redaction_report_json.bytes(),
        ]);
        let content_hash_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                content_hash.as_bytes(),
                "blob",
                "content_hash.txt",
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;

        // manifest.json is written LAST among the blobs: it declares every
        // other entry's OID/length (including content_hash.txt), so nothing
        // can hash or list the manifest itself without circularity.
        let manifest_bytes = build_checkpoint_manifest_json(
            params.checkpoint_id,
            &transcript_file_name,
            ManifestBlobRef::new(metadata_blob_oid, params.metadata_json.len()),
            ManifestBlobRef::new(events_blob_oid, params.lifecycle_events_jsonl.len()),
            &transcript_parts,
            transcript_bytes.len(),
            ManifestBlobRef::new(report_blob_oid, params.redaction_report_json.len()),
            ManifestBlobRef::new(content_hash_blob_oid, content_hash.len()),
        )?;
        let manifest_blob_oid = self
            .write_indexed_object_for_attempt(
                "blob",
                &manifest_bytes,
                "blob",
                "manifest.json",
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        object_count += 1;
        ensure_deadline()?;

        // Phase 2: build the leaf trees (transcript/, events/).
        // All trees written under the agent capture path go through
        // `write_tree_indexed` so they reach `object_index` and the
        // standard cloud sync path; otherwise the orphan ref's commits
        // would dereference to missing trees on a fresh `cloud restore`.
        let mut transcript_items: Vec<TreeItem> = transcript_parts
            .iter()
            .map(|part| TreeItem::new(TreeItemMode::Blob, part.oid, part.name.clone()))
            .collect();
        transcript_items.sort_by(|a, b| a.name.cmp(&b.name));
        let transcript_subtree = self
            .write_tree_indexed_for_attempt(
                &transcript_items,
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        let events_subtree = self
            .write_tree_indexed_for_attempt(
                &[TreeItem::new(
                    TreeItemMode::Blob,
                    events_blob_oid,
                    CHECKPOINT_LIFECYCLE_EVENTS_FILE.to_string(),
                )],
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        object_count += 2;

        let mut inner_items = vec![
            TreeItem::new(
                TreeItemMode::Blob,
                metadata_blob_oid,
                "metadata.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                manifest_blob_oid,
                "manifest.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                report_blob_oid,
                "redaction_report.json".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Blob,
                content_hash_blob_oid,
                "content_hash.txt".to_string(),
            ),
            TreeItem::new(
                TreeItemMode::Tree,
                transcript_subtree,
                "transcript".to_string(),
            ),
            TreeItem::new(TreeItemMode::Tree, events_subtree, "events".to_string()),
        ];
        inner_items.sort_by(|a, b| a.name.cmp(&b.name));
        let inner_tree = self
            .write_tree_indexed_for_attempt(
                &inner_items,
                writer_fence,
                params.capture_scope,
                params.deadline,
                newly_written,
                &mut object_index_intents,
            )
            .await?;
        object_count += 1;

        // Phase 3: CAS loop. Read parent, splice
        // `checkpoint/<prefix>/<rest>` into its tree, write the new commit,
        // and update the ref atomically. Retries on head conflict, mirroring
        // the existing `append` flow.
        let prefix = params
            .checkpoint_id
            .get(..2)
            .ok_or_else(|| anyhow!("checkpoint_id must be at least 2 characters"))?
            .to_string();
        let rest = params.checkpoint_id[2..].to_string();
        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            // Phase 1/2 objects are shared across retries. The spliced
            // trees and commit below are attempt-specific; if the CAS loses,
            // discard only those intents so a later winner never advertises
            // unreachable retry residue to cloud sync.
            let attempt_index_intents_start = object_index_intents.len();
            ensure_deadline()?;
            let parent = self.resolve_history_head_until(params.deadline).await?;
            ensure_deadline()?;
            // Test-only: deterministic head-moved-between-read-and-CAS
            // injection (see the struct field's doc).
            #[cfg(test)]
            if let Some(hook) = &self.test_after_head_read {
                hook().await?;
            }
            let new_root = self
                .splice_checkpoint_tree_for_attempt(
                    parent,
                    &prefix,
                    &rest,
                    inner_tree,
                    writer_fence,
                    params.capture_scope,
                    params.deadline,
                    newly_written,
                    &mut object_index_intents,
                )
                .await?;
            // splice_checkpoint_tree writes exactly three trees
            // (rest→prefix→checkpoint→root splice) per attempt; +1 commit.
            object_count += 4;

            let trailer = format_libra_trailers(&params);
            let message = format!(
                "traces: {} checkpoint {}\n\n{trailer}",
                params.scope.as_str(),
                params.checkpoint_id,
            );
            let author = Signature::new(
                SignatureType::Author,
                "Libra".to_string(),
                "traces@libra".to_string(),
            );
            let committer = Signature::new(
                SignatureType::Committer,
                "Libra".to_string(),
                "traces@libra".to_string(),
            );
            let parents = parent.into_iter().collect::<Vec<_>>();
            let commit = Commit::new(author, committer, new_root, parents, &message);
            let commit_data = commit
                .to_data()
                .context("failed to serialize checkpoint commit")?;
            let commit_hash = self
                .write_indexed_object_for_attempt(
                    "commit",
                    &commit_data,
                    "commit",
                    "commit",
                    writer_fence,
                    params.capture_scope,
                    params.deadline,
                    newly_written,
                    &mut object_index_intents,
                )
                .await?;
            ensure_deadline()?;

            // Per-attempt ctx: commit hash and root tree change on every CAS
            // rebuild, so the companion writes get the values of THIS attempt.
            let commit_ctx = TracesCommitCtx {
                commit_hash: commit_hash.to_string(),
                tree_oid: new_root.to_string(),
                metadata_blob_oid: metadata_blob_oid.to_string(),
            };
            #[cfg(test)]
            if let Some(hook) = &self.test_before_checkpoint_ref_cas {
                hook(CheckpointAttemptIndexSnapshot {
                    commit_hash,
                    tree_oid: new_root,
                    object_index_oids: object_index_intents[attempt_index_intents_start..]
                        .iter()
                        .map(|intent| intent.oid.clone())
                        .collect(),
                })
                .await?;
            }
            let checkpoint_txn_extra = CheckpointCommitTxnExtra {
                extra: params.txn_extra,
                capture_scope: params.capture_scope,
                object_index_intents: &object_index_intents,
            };
            let transactional_extra: Option<(&dyn TracesTxnExtra, &TracesCommitCtx)> =
                if params.capture_scope.is_some() {
                    Some((&checkpoint_txn_extra, &commit_ctx))
                } else {
                    params.txn_extra.map(|extra| (extra, &commit_ctx))
                };
            match self
                .update_ref_if_matches_with_extra(
                    &self.ref_name,
                    parent,
                    commit_hash,
                    transactional_extra,
                    params.deadline,
                    Some(writer_fence),
                    params.capture_scope,
                )
                .await?
            {
                RefUpdateOutcome::Updated => {
                    return Ok(CheckpointCommit {
                        commit_hash,
                        tree_oid: new_root,
                        metadata_blob_oid,
                        marker_generation: writer_fence.generation.clone(),
                        cas_retries: attempt as u64,
                        object_count,
                    });
                }
                RefUpdateOutcome::HeadChanged if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES => {
                    object_index_intents.truncate(attempt_index_intents_start);
                    continue;
                }
                RefUpdateOutcome::HeadChanged => {
                    return Err(anyhow::Error::new(
                        CheckpointAppendConflict::RefCasExhausted,
                    ));
                }
            }
        }
        // The final loop iteration always returns; keep the same typed
        // conflict should the bound ever change shape.
        Err(anyhow::Error::new(
            CheckpointAppendConflict::RefCasExhausted,
        ))
    }

    async fn rejected_cleanup_db_snapshot<C: ConnectionTrait>(
        &self,
        conn: &C,
    ) -> Result<RejectedCleanupDbSnapshot> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let mut markers = list_all_traces_inflight_markers(conn).await?;
        markers.sort_by(|left, right| {
            (&left.session_id, &left.attempt_id).cmp(&(&right.session_id, &right.attempt_id))
        });

        let mut candidates = HashSet::new();
        for marker in markers.iter().filter(|marker| {
            marker.cleanup_pending
                || !marker.is_live(now_ms)
                || !marker.time_fields_trustworthy(now_ms)
        }) {
            let cataloged = conn
                .query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT 1 FROM agent_checkpoint WHERE checkpoint_id = ?",
                    [marker.attempt_id.clone().into()],
                ))
                .await
                .context("verify rejected checkpoint cleanup candidate")?;
            if cataloged.is_none() {
                candidates.extend(marker.created_oids.iter().cloned());
            }
        }

        let mut root_rows = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT `commit` AS oid FROM reference WHERE `commit` IS NOT NULL LIMIT 250001"
                    .to_string(),
            ))
            .await
            .context("list reference roots for rejected object cleanup")?;
        let reflog_exists = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                ["reflog".into()],
            ))
            .await
            .context("check reflog table before rejected object cleanup")?
            .is_some();
        if reflog_exists && root_rows.len() <= REJECTED_CLEANUP_MAX_VISITED_OBJECTS {
            root_rows.extend(
                conn.query_all_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT old_oid AS oid FROM reflog
                     UNION ALL SELECT new_oid AS oid FROM reflog
                     LIMIT 250001"
                        .to_string(),
                ))
                .await
                .context("list reflog roots for rejected object cleanup")?,
            );
        }
        if root_rows.len() > REJECTED_CLEANUP_MAX_VISITED_OBJECTS {
            bail!(
                "repository has more than {} reference/reflog cleanup roots",
                REJECTED_CLEANUP_MAX_VISITED_OBJECTS
            );
        }
        let mut graph_roots = Vec::with_capacity(root_rows.len());
        for row in root_rows {
            let value: String = row.try_get_by("oid")?;
            if !value.is_empty() && !value.bytes().all(|byte| byte == b'0') {
                crate::internal::ai::util::parse_repo_object_id(&value).map_err(|error| {
                    anyhow!("repository cleanup root {value} is invalid: {error}")
                })?;
                graph_roots.push(value);
            }
        }
        graph_roots.sort();
        graph_roots.dedup();

        let mut active_operations = Vec::new();
        for table in ["rebase_state", "sequence_state"] {
            let table_exists = conn
                .query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?",
                    [table.into()],
                ))
                .await
                .with_context(|| format!("check {table} table before object cleanup"))?
                .is_some();
            if table_exists
                && conn
                    .query_one_raw(Statement::from_string(
                        conn.get_database_backend(),
                        format!("SELECT 1 FROM {table} LIMIT 1"),
                    ))
                    .await
                    .with_context(|| format!("check active {table} before object cleanup"))?
                    .is_some()
            {
                active_operations.push(table.to_string());
            }
        }

        Ok(RejectedCleanupDbSnapshot {
            markers,
            candidates,
            graph_roots,
            active_operations,
        })
    }

    fn rejected_cleanup_index_snapshot(
        &self,
        deadline: Instant,
    ) -> Result<RejectedCleanupIndexSnapshot> {
        if Instant::now() >= deadline {
            bail!("repository cleanup deadline expired before index snapshot");
        }

        #[cfg(test)]
        if TEST_DIRECT_REJECTED_CLEANUP_INDEX_SNAPSHOT
            .try_with(|_| ())
            .is_ok()
        {
            let snapshot = collect_rejected_cleanup_index_snapshot(
                &self.repo_path,
                git_internal::hash::get_hash_kind().size(),
            )?;
            if Instant::now() >= deadline {
                bail!("repository cleanup index snapshot exceeded its traversal deadline");
            }
            return Ok(snapshot);
        }

        let request = serde_json::to_vec(&RejectedCleanupIndexHelperRequest {
            repo_path: self.repo_path.clone(),
            hash_bytes: git_internal::hash::get_hash_kind().size(),
        })
        .context("encode rejected-cleanup index helper request")?;
        let Some(program) = crate::internal::ai::authorized_read::helper_program() else {
            // A library embedded in another process must not execute that
            // process with Libra's private helper argument just because it is
            // available as `current_exe`.
            bail!("rejected-cleanup index helper is unavailable in this host");
        };
        let mut output =
            tempfile::tempfile().context("create rejected-cleanup index helper output file")?;
        // Initialize the nonblocking reap service before creating a child, so
        // thread-allocation failure cannot strand an already-started helper.
        let reaper = cleanup_helper_reaper_sender()?;
        let child_output = output
            .try_clone()
            .context("clone rejected-cleanup index helper output file")?;
        let child = Command::new(&program)
            .arg(REJECTED_CLEANUP_INDEX_HELPER_ARG)
            .stdin(Stdio::piped())
            .stdout(Stdio::from(child_output))
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| {
                format!(
                    "start rejected-cleanup index helper '{}'",
                    program.display()
                )
            })?;
        let mut child = CleanupHelperChild::new(child, reaper);
        let mut stdin = child
            .child_mut()
            .stdin
            .take()
            .ok_or_else(|| anyhow!("rejected-cleanup index helper has no stdin pipe"))?;
        stdin
            .write_all(&request)
            .context("write rejected-cleanup index helper request")?;
        drop(stdin);

        let status = loop {
            if let Some(status) = child
                .child_mut()
                .try_wait()
                .context("poll rejected-cleanup index helper")?
            {
                break status;
            }
            if Instant::now() >= deadline {
                bail!("repository cleanup index snapshot exceeded its traversal deadline");
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        if !status.success() {
            bail!("rejected-cleanup index helper exited unsuccessfully");
        }
        output
            .seek(SeekFrom::Start(0))
            .context("rewind rejected-cleanup index helper output")?;
        let response =
            match read_strictly_bounded(&mut output, REJECTED_CLEANUP_INDEX_HELPER_FRAME_CAP) {
                StrictBoundedRead::Complete(response) => response,
                StrictBoundedRead::Oversize { .. } => {
                    bail!("rejected-cleanup index helper response exceeds its frame limit");
                }
                StrictBoundedRead::Failed { error, .. } => {
                    return Err(error).context("read rejected-cleanup index helper output");
                }
            };
        let response: RejectedCleanupIndexHelperResponse = serde_json::from_slice(&response)
            .context("decode rejected-cleanup index helper response")?;
        match (response.snapshot, response.error) {
            (Some(snapshot), None) => Ok(snapshot),
            (None, Some(error)) => bail!("rejected-cleanup index snapshot failed: {error}"),
            _ => bail!("rejected-cleanup index helper returned an invalid response"),
        }
    }

    async fn rejected_cleanup_root_snapshot<C: ConnectionTrait>(
        &self,
        conn: &C,
        deadline: Instant,
    ) -> Result<RejectedCleanupRootSnapshot> {
        let db = self.rejected_cleanup_db_snapshot(conn).await?;
        let RejectedCleanupIndexSnapshot {
            roots,
            fingerprints,
            mut active_operations,
        } = self.rejected_cleanup_index_snapshot(deadline)?;
        active_operations.extend(db.active_operations.iter().cloned());
        active_operations.sort();
        active_operations.dedup();
        Ok(RejectedCleanupRootSnapshot {
            db,
            index_roots: roots,
            index_fingerprints: fingerprints,
            active_operations,
        })
    }

    #[cfg(test)]
    async fn reachable_rejected_objects_with_limit(
        &self,
        ref_heads: Vec<ObjectHash>,
        candidates: &HashSet<String>,
        max_inflated_object_bytes: u64,
    ) -> Result<HashSet<String>> {
        self.reachable_rejected_objects_with_limits(
            ref_heads,
            candidates,
            max_inflated_object_bytes,
            REJECTED_CLEANUP_MAX_VISITED_OBJECTS,
            Instant::now() + REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION,
        )
        .await
    }

    #[cfg(test)]
    async fn reachable_rejected_objects_with_limits(
        &self,
        ref_heads: Vec<ObjectHash>,
        candidates: &HashSet<String>,
        max_inflated_object_bytes: u64,
        max_visited_objects: usize,
        deadline: Instant,
    ) -> Result<HashSet<String>> {
        let mut reachable = HashSet::new();
        let mut seen = HashSet::new();
        let mut stack = ref_heads;
        while let Some(oid) = stack.pop() {
            if Instant::now() >= deadline {
                bail!(
                    "ref-reachability cleanup exceeded its {} second traversal deadline after visiting {} objects",
                    REJECTED_CLEANUP_MAX_TRAVERSAL_DURATION.as_secs(),
                    seen.len()
                );
            }
            if !seen.contains(&oid) && seen.len() >= max_visited_objects {
                bail!(
                    "ref-reachability cleanup exceeded its {max_visited_objects} object traversal limit"
                );
            }
            if !seen.insert(oid) {
                continue;
            }
            let oid_string = oid.to_string();
            if candidates.contains(&oid_string) {
                reachable.insert(oid_string);
            }
            // Diagnostic reachability must understand every ref root, including
            // objects held in local packs or alternates. The storage-level
            // bounded read enforces a conservative load-cost cap before
            // materializing the payload; the explicit OID verification keeps
            // a corrupt loose/packed object from making deletion unsafe.
            let (data, object_type) = tokio::time::timeout_at(
                tokio::time::Instant::from_std(deadline),
                self.storage.get_with_limit(&oid, max_inflated_object_bytes),
            )
            .await
            .map_err(|_| {
                anyhow!(
                    "ref-reachability cleanup exceeded its traversal deadline while reading {oid}"
                )
            })?
            .with_context(|| format!("read ref-reachable {oid} during cleanup"))?;
            if Instant::now() >= deadline {
                bail!(
                    "ref-reachability cleanup exceeded its traversal deadline after reading {oid}"
                );
            }
            verify_fetched_object(&oid, object_type, &data)
                .with_context(|| format!("verify ref-reachable {oid} during cleanup"))?;
            match object_type {
                ObjectType::Commit => {
                    let commit = Commit::from_bytes(&data, oid)
                        .map_err(|error| anyhow!("parse ref-reachable commit {oid}: {error}"))?;
                    stack.push(commit.tree_id);
                    stack.extend(commit.parent_commit_ids);
                }
                ObjectType::Tree => {
                    let tree = Tree::from_bytes(&data, oid)
                        .map_err(|error| anyhow!("parse ref-reachable tree {oid}: {error}"))?;
                    for item in tree.tree_items {
                        let item_oid = item.id.to_string();
                        if candidates.contains(&item_oid) {
                            reachable.insert(item_oid);
                        }
                        if item.mode == TreeItemMode::Tree {
                            stack.push(item.id);
                        }
                    }
                }
                ObjectType::Tag => {
                    let body = std::str::from_utf8(&data).with_context(|| {
                        format!("parse ref-reachable annotated tag {oid} as UTF-8")
                    })?;
                    let target = body
                        .lines()
                        .next()
                        .and_then(|line| line.strip_prefix("object "))
                        .ok_or_else(|| {
                            anyhow!("ref-reachable annotated tag {oid} has no object target")
                        })?;
                    stack.push(
                        crate::internal::ai::util::parse_repo_object_id(target).map_err(
                            |error| anyhow!("parse annotated tag {oid} target {target}: {error}"),
                        )?,
                    );
                }
                ObjectType::Blob => {}
                other => {
                    bail!(
                        "ref-reachable object {oid} has unsupported type '{other}' during rejected checkpoint cleanup"
                    )
                }
            }
        }
        Ok(reachable)
    }

    /// Convert objects created by a rejected append into a durable ownership
    /// record. The foreground failure path only registers the exact writer's
    /// cleanup job; doctor/GC owns object-index draining and repository-wide
    /// reachability so an append deadline cannot be extended by maintenance.
    #[cfg(test)]
    async fn cleanup_rejected_checkpoint_objects(
        &self,
        writer_fence: &TracesWriterFence,
        capture_scope: Option<&CaptureScope>,
        newly_written: &HashSet<String>,
    ) -> Result<()> {
        self.cleanup_rejected_checkpoint_objects_until(
            writer_fence,
            capture_scope,
            newly_written,
            None,
        )
        .await
    }

    async fn cleanup_rejected_checkpoint_objects_until(
        &self,
        writer_fence: &TracesWriterFence,
        capture_scope: Option<&CaptureScope>,
        newly_written: &HashSet<String>,
        capture_deadline: Option<CaptureCommitDeadline>,
    ) -> Result<()> {
        // The ordinary marker already records every preclaimed/created
        // object before the append can fail. This best-effort upgrade to a
        // cleanup-pending marker must not turn an expired capture deadline
        // into the connection's default 30-second SQLite wait. If the fixed
        // recovery grace is exhausted, leave the original marker untouched
        // for doctor/GC instead.
        let recovery_deadline = rejected_cleanup_registration_deadline(capture_deadline)?;
        let txn = tokio::time::timeout_at(
            tokio::time::Instant::from_std(recovery_deadline.monotonic()),
            crate::internal::db::begin_write_transaction(self.db_conn.as_ref()),
        )
        .await
        .map_err(|_| {
            rejected_cleanup_registration_deferred(
                "SQLite writer remained contended after the 250ms recovery grace",
            )
        })?
        .map_err(|error| {
            rejected_cleanup_registration_deferred(format!(
                "could not acquire the SQLite writer for cleanup registration: {error}"
            ))
        })?;
        if let Err(error) =
            ensure_before_rejected_cleanup_registration_deadline(recovery_deadline.monotonic())
        {
            txn.rollback().await.ok();
            return Err(error);
        }
        if let Some(scope) = capture_scope {
            let fence_check = match tokio::time::timeout_at(
                tokio::time::Instant::from_std(recovery_deadline.monotonic()),
                scope.assert_workspace_fence_live(&txn),
            )
            .await
            {
                Ok(fence_check) => fence_check,
                Err(_) => {
                    txn.rollback().await.ok();
                    return Err(rejected_cleanup_registration_deferred(
                        "the 250ms recovery grace elapsed while verifying the capture workspace lease",
                    ));
                }
            };
            if let Err(error) = fence_check {
                txn.rollback().await.ok();
                return Err(error).context(
                    "verify capture workspace lease before rejected checkpoint cleanup registration",
                );
            }
            if let Err(error) =
                ensure_before_rejected_cleanup_registration_deadline(recovery_deadline.monotonic())
            {
                txn.rollback().await.ok();
                return Err(error);
            }
        }
        if let Err(error) =
            ensure_before_rejected_cleanup_registration_deadline(recovery_deadline.monotonic())
        {
            txn.rollback().await.ok();
            return Err(error);
        }
        let existing = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(recovery_deadline.monotonic()),
            crate::internal::metadata::MetadataKv::get_with_conn(
                &txn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                &writer_fence.session_id,
                &writer_fence.attempt_id,
            ),
        )
        .await
        {
            Ok(Ok(existing)) => existing,
            Ok(Err(error)) => {
                txn.rollback().await.ok();
                return Err(error).context("load rejected checkpoint writer marker");
            }
            Err(_) => {
                txn.rollback().await.ok();
                return Err(rejected_cleanup_registration_deferred(
                    "the 250ms recovery grace elapsed while loading the rejected checkpoint marker",
                ));
            }
        };
        if let Err(error) =
            ensure_before_rejected_cleanup_registration_deadline(recovery_deadline.monotonic())
        {
            txn.rollback().await.ok();
            return Err(error);
        }
        let mut marker = match existing {
            Some(entry) => {
                let marker = match decode_and_validate_traces_inflight_marker_for_rejected_cleanup(
                    &entry.value,
                    &entry.target,
                    &entry.key,
                ) {
                    Ok(marker) => marker,
                    Err(error) => {
                        let recovery_bound_exceeded = error
                            .downcast_ref::<TracesInflightRejectedCleanupMarkerBoundExceeded>()
                            .is_some();
                        txn.rollback().await.ok();
                        if recovery_bound_exceeded {
                            return Err(rejected_cleanup_registration_deferred(
                                "the rejected checkpoint marker exceeds the bounded 250ms recovery budget",
                            ));
                        }
                        return Err(error).context("validate rejected checkpoint writer marker");
                    }
                };
                if Self::ensure_marker_matches_fence(&marker, writer_fence).is_err() {
                    txn.rollback().await.ok();
                    tracing::debug!(
                        checkpoint_id = %writer_fence.attempt_id,
                        "leaving rejected objects to repository GC after marker generation changed"
                    );
                    return Ok(());
                }
                marker
            }
            None => {
                txn.rollback().await.ok();
                return Ok(());
            }
        };
        if let Err(error) =
            ensure_before_rejected_cleanup_registration_deadline(recovery_deadline.monotonic())
        {
            txn.rollback().await.ok();
            return Err(error);
        }
        // Do not materialize an unbounded newly-written set inside this
        // fixed-grace transaction. Treat possible overlap conservatively: a
        // deferred marker is recoverable, whereas sorting a huge vector while
        // holding SQLite's writer is not.
        let existing_oid_entries = marker.oids.len().saturating_add(marker.created_oids.len());
        if newly_written.len()
            > TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES
                .saturating_sub(existing_oid_entries)
        {
            txn.rollback().await.ok();
            return Err(rejected_cleanup_registration_deferred(
                "the rejected checkpoint ownership set exceeds the bounded 250ms recovery budget",
            ));
        }
        marker.schema_version = marker.schema_version.max(3);
        marker.created_oids.extend(newly_written.iter().cloned());
        marker.created_oids.sort();
        marker.created_oids.dedup();
        if marker.created_oids.is_empty() {
            if let Err(error) =
                ensure_before_rejected_cleanup_registration_deadline(recovery_deadline.monotonic())
            {
                txn.rollback().await.ok();
                return Err(error);
            }
            let cleared = match crate::internal::ai::traces::clear_traces_inflight_marker_if_generation_with_capture_scope_until(
                &txn,
                capture_scope,
                &writer_fence.session_id,
                &writer_fence.attempt_id,
                &writer_fence.generation,
                recovery_deadline,
            )
            .await {
                Ok(cleared) => cleared,
                Err(error) => {
                    txn.rollback().await.ok();
                    if error.chain().any(|cause| cause.is::<crate::internal::ai::traces::TracesMarkerDeadlineExceeded>()) {
                        return Err(rejected_cleanup_registration_deferred(
                            "the 250ms recovery grace elapsed while clearing the rejected checkpoint marker",
                        ));
                    }
                    return Err(error).context("clear rejected checkpoint writer marker");
                }
            };
            if !cleared {
                txn.rollback().await.ok();
                return Ok(());
            }
            // The deadline-aware marker helper performed the transaction's
            // final authorization. Await the dispatched commit acknowledgement
            // without a timeout.
            txn.commit()
                .await
                .context("commit empty rejected checkpoint cleanup")?;
            return Ok(());
        }
        marker.cleanup_pending = true;
        if let Err(error) =
            ensure_before_rejected_cleanup_registration_deadline(recovery_deadline.monotonic())
        {
            txn.rollback().await.ok();
            return Err(error);
        }
        let updated = match crate::internal::ai::traces::update_traces_inflight_marker_if_generation_with_capture_scope_until(
            &txn,
            capture_scope,
            &marker,
            &writer_fence.generation,
            recovery_deadline,
        )
        .await {
            Ok(updated) => updated,
            Err(error) => {
                txn.rollback().await.ok();
                if error.chain().any(|cause| cause.is::<crate::internal::ai::traces::TracesMarkerDeadlineExceeded>()) {
                    return Err(rejected_cleanup_registration_deferred(
                        "the 250ms recovery grace elapsed while registering the rejected checkpoint cleanup job",
                    ));
                }
                return Err(error).context("persist rejected checkpoint cleanup job");
            }
        };
        if !updated {
            txn.rollback().await.ok();
            return Ok(());
        }
        // The deadline-aware marker helper performed the transaction's final
        // authorization. Await the dispatched commit acknowledgement without
        // a timeout.
        txn.commit()
            .await
            .context("commit rejected checkpoint cleanup registration")?;
        Ok(())
    }

    /// Drain all durable rejected-append cleanup jobs in one serialized
    /// root-fenced ownership retirement. A non-cleanup live writer defers retirement; pending
    /// jobs themselves may be expired and are still never forgotten.
    async fn drain_rejected_checkpoint_cleanup_jobs(&self) -> Result<()> {
        self.drain_rejected_checkpoint_cleanup_jobs_ignoring(None)
            .await
    }

    /// Doctor repair entry point for one valid expired marker. The shared
    /// serialized drain revalidates repository roots and writer state before
    /// retiring ownership. Physical payload reachability and reclamation remain
    /// the repository GC's responsibility. Returns whether the named marker
    /// was fully retired.
    pub async fn repair_expired_traces_inflight_marker(
        &self,
        session_id: &str,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        let entry = crate::internal::metadata::MetadataKv::get_with_conn(
            self.db_conn.as_ref(),
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            session_id,
            attempt_id,
        )
        .await
        .context("load expired traces marker for doctor repair")?;
        let Some(entry) = entry else {
            return Ok(true);
        };
        let marker =
            decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)?;
        // A LIVE marker refuses retirement — but only when its time fields
        // are trustworthy: a future-dated row would otherwise read as "live"
        // forever and be unrepairable (W2 §C.4.3).
        if !marker.cleanup_pending
            && marker.time_fields_trustworthy(now_ms)
            && marker.is_live(now_ms)
        {
            return Ok(false);
        }
        self.drain_rejected_checkpoint_cleanup_jobs().await?;
        Ok(crate::internal::metadata::MetadataKv::get_with_conn(
            self.db_conn.as_ref(),
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            session_id,
            attempt_id,
        )
        .await
        .context("verify expired traces marker doctor repair")?
        .is_none())
    }

    async fn drain_rejected_checkpoint_cleanup_jobs_ignoring(
        &self,
        ignored_attempt: Option<(&str, &str)>,
    ) -> Result<()> {
        if !crate::utils::client_storage::ClientStorage::wait_for_background_tasks_until(
            Instant::now() + OBJECT_INDEX_CLEANUP_DRAIN_BUDGET,
        )
        .await
        {
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "object-index queue did not drain within 5 seconds".to_string(),
            }
            .into());
        }
        let cleanup_deadline = Instant::now() + rejected_cleanup_traversal_duration();

        // Snapshot all DB and filesystem roots without a SQLite writer
        // transaction. Index helpers remain bounded and therefore do not hold
        // the repository writer lock while inspecting filesystem state.
        let initial = self
            .rejected_cleanup_root_snapshot(self.db_conn.as_ref(), cleanup_deadline)
            .await
            .map_err(|error| RejectedCheckpointCleanupDeferred {
                reason: format!("repository cleanup roots could not be snapshotted: {error:#}"),
            })?;
        let now_ms = chrono::Utc::now().timestamp_millis();
        let pending = initial
            .db
            .markers
            .iter()
            .filter(|marker| {
                marker.cleanup_pending
                    || !marker.is_live(now_ms)
                    // W2 §C.4.3: a future-dated (untrustworthy) row reads as
                    // "live" under the absolute deadline forever — it must be
                    // RETIRABLE here, or doctor can never unblock the
                    // fail-closed listing.
                    || !marker.time_fields_trustworthy(now_ms)
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            return Ok(());
        }
        if let Some(other) = initial.db.markers.iter().find(|marker| {
            !marker.cleanup_pending
                && marker.time_fields_trustworthy(now_ms)
                && marker.is_live(now_ms)
                && ignored_attempt.is_none_or(|(session_id, attempt_id)| {
                    marker.session_id != session_id || marker.attempt_id != attempt_id
                })
        }) {
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: other.session_id.clone(),
                attempt_id: other.attempt_id.clone(),
                ttl_ms: other.ttl_ms,
            }
            .into());
        }
        if !initial.active_operations.is_empty() {
            return Err(RejectedCheckpointCleanupDeferred {
                reason: format!(
                    "repository operation state is active ({})",
                    initial.active_operations.join(", ")
                ),
            }
            .into());
        }

        // Rejected-checkpoint cleanup is deliberately non-destructive: it
        // retires durable writer ownership but leaves both payloads and
        // object-index rows for repository GC. Walking every ref graph here
        // therefore cannot make a deletion safer, while a large unrelated
        // history can make the marker impossible to retire. The stable root
        // snapshots below still fence concurrent writers and repository
        // operations; GC performs the eventual reachability proof when it
        // actually reclaims objects.

        // Re-read every root before retirement. Any concurrent ref, reflog,
        // worktree-index, operation-state, marker, or catalog change makes
        // the fence stale and leaves the durable cleanup job for a retry.
        let revalidated = self
            .rejected_cleanup_root_snapshot(self.db_conn.as_ref(), cleanup_deadline)
            .await
            .map_err(|error| RejectedCheckpointCleanupDeferred {
                reason: format!("repository cleanup roots could not be revalidated: {error:#}"),
            })?;
        if revalidated != initial {
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "repository cleanup roots changed before ownership retirement".to_string(),
            }
            .into());
        }

        // Take the writer lock only for the final compare/retire phase.
        // Ref/reflog/marker/catalog writers cannot move after the comparison
        // until this transaction commits.
        let txn = self
            .db_conn
            .begin()
            .await
            .context("begin rejected checkpoint object cleanup")?;
        txn.execute_raw(Statement::from_string(
            txn.get_database_backend(),
            "UPDATE metadata_kv SET updated_at = updated_at
             WHERE scope = 'agent_traces_inflight'"
                .to_string(),
        ))
        .await
        .context("lock traces marker registry for rejected object cleanup")?;
        let locked_db = self.rejected_cleanup_db_snapshot(&txn).await?;
        if locked_db != revalidated.db {
            txn.rollback().await.ok();
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "database cleanup roots changed before the retirement lock was acquired"
                    .to_string(),
            }
            .into());
        }
        let RejectedCleanupIndexSnapshot {
            roots: locked_index_roots,
            fingerprints: locked_index_fingerprints,
            active_operations: mut locked_operations,
        } = match self.rejected_cleanup_index_snapshot(cleanup_deadline) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                txn.rollback().await.ok();
                return Err(RejectedCheckpointCleanupDeferred {
                    reason: format!(
                        "filesystem cleanup roots could not be locked before the deadline: {error:#}"
                    ),
                }
                .into());
            }
        };
        locked_operations.extend(locked_db.active_operations.iter().cloned());
        locked_operations.sort();
        locked_operations.dedup();
        if locked_index_roots != revalidated.index_roots
            || locked_index_fingerprints != revalidated.index_fingerprints
            || locked_operations != revalidated.active_operations
        {
            txn.rollback().await.ok();
            return Err(RejectedCheckpointCleanupDeferred {
                reason: "filesystem cleanup roots changed before ownership retirement".to_string(),
            }
            .into());
        }
        let locked_now_ms = chrono::Utc::now().timestamp_millis();
        let pending = locked_db
            .markers
            .iter()
            .filter(|marker| {
                marker.cleanup_pending
                    || !marker.is_live(locked_now_ms)
                    || !marker.time_fields_trustworthy(locked_now_ms)
            })
            .collect::<Vec<_>>();
        if pending.is_empty() {
            txn.commit()
                .await
                .context("commit empty rejected checkpoint cleanup")?;
            return Ok(());
        }
        if let Some(other) = locked_db.markers.iter().find(|marker| {
            !marker.cleanup_pending
                && marker.time_fields_trustworthy(locked_now_ms)
                && marker.is_live(locked_now_ms)
                && ignored_attempt.is_none_or(|(session_id, attempt_id)| {
                    marker.session_id != session_id || marker.attempt_id != attempt_id
                })
        }) {
            txn.rollback().await.ok();
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: other.session_id.clone(),
                attempt_id: other.attempt_id.clone(),
                ttl_ms: other.ttl_ms,
            }
            .into());
        }
        // Do not unlink shared content-addressed payloads or object-index rows
        // here. Repository GC owns physical reclamation and its reachability
        // proof; this transaction only retires exact marker generations.
        for marker in pending {
            clear_traces_inflight_marker(&txn, &marker.session_id, &marker.attempt_id).await?;
        }
        txn.commit()
            .await
            .context("commit rejected checkpoint object cleanup")?;
        Ok(())
    }

    /// Remove checkpoint commits from this manager's ref and delete their
    /// `agent_checkpoint` rows.
    ///
    /// This is the `libra agent clean` counterpart to
    /// [`Self::append_checkpoint_commit`]. It rewrites the orphan
    /// `refs/libra/traces` chain from the checkpoint catalog, omitting
    /// the supplied checkpoint IDs. Rewriting is necessary because later
    /// committed checkpoints may descend from temporary checkpoints; simply
    /// moving the ref to an ancestor would either keep those temporary commits
    /// reachable or discard later retained checkpoints.
    ///
    /// Repositories that only have catalog rows and an empty traces ref
    /// (older fixtures, partial migrations, or pre-Phase-2 data) still get the
    /// catalog deletion without a ref rewrite.
    pub async fn prune_checkpoint_commits(
        &self,
        checkpoint_ids_to_remove: &[String],
    ) -> Result<CheckpointPruneOutcome> {
        self.prune_checkpoint_commits_inner(checkpoint_ids_to_remove, true)
            .await
    }

    async fn prune_checkpoint_commits_inner(
        &self,
        checkpoint_ids_to_remove: &[String],
        record_cloud_tombstones: bool,
    ) -> Result<CheckpointPruneOutcome> {
        // AG-20 observability (`agent.md` §6): one `agent.clean.prune` span
        // per prune. Required fields: deleted_objects, deleted_sessions,
        // window_guard, duration_ms. No raw filesystem path is ever
        // recorded (forbidden: raw path outside repo).
        let prune_span = tracing::info_span!(
            "agent.clean.prune",
            deleted_objects = tracing::field::Empty,
            deleted_sessions = tracing::field::Empty,
            window_guard = tracing::field::Empty,
            duration_ms = tracing::field::Empty,
        );
        let started = std::time::Instant::now();
        let finish_span = |guard: &'static str, deleted_objects: u64| {
            prune_span.record("deleted_objects", deleted_objects);
            // The prune never deletes `agent_session` rows (sessions are
            // retained for history; only checkpoint rows are dropped).
            prune_span.record("deleted_sessions", 0_u64);
            prune_span.record("window_guard", guard);
            prune_span.record("duration_ms", started.elapsed().as_millis() as u64);
        };

        let remove_set: HashSet<&str> = checkpoint_ids_to_remove
            .iter()
            .map(String::as_str)
            .collect();
        if remove_set.is_empty() {
            finish_span("noop", 0);
            return Ok(CheckpointPruneOutcome {
                removed_checkpoints: 0,
                rewritten_checkpoints: 0,
                ref_rewritten: false,
                window_guard: "noop",
                deleted_object_index_rows: 0,
                deleted_import_identities: 0,
            });
        }

        for attempt in 0..=HISTORY_HEAD_CONFLICT_MAX_RETRIES {
            let expected_head = self.resolve_history_head().await?;
            let rows = self.load_checkpoint_history_rows().await?;
            let existing_remove_ids = rows
                .iter()
                .filter(|row| remove_set.contains(row.checkpoint_id.as_str()))
                .map(|row| row.checkpoint_id.clone())
                .collect::<HashSet<_>>();

            if existing_remove_ids.is_empty() {
                finish_span("noop", 0);
                return Ok(CheckpointPruneOutcome {
                    removed_checkpoints: 0,
                    rewritten_checkpoints: 0,
                    ref_rewritten: false,
                    window_guard: "noop",
                    deleted_object_index_rows: 0,
                    deleted_import_identities: 0,
                });
            }

            // AG-20 window A/B guards — both must pass before any rewrite.
            if let Err(guard_err) = self.enforce_prune_window_guards(expected_head, &rows).await {
                let guard_label = if guard_err
                    .downcast_ref::<SubagentContentReservationPruneGuard>()
                    .is_some()
                {
                    "subagent_reservation_blocked"
                } else {
                    match guard_err.downcast_ref::<CheckpointPruneGuardError>() {
                        Some(CheckpointPruneGuardError::LiveWriterMarker { .. }) => {
                            "live_marker_blocked"
                        }
                        Some(CheckpointPruneGuardError::RefCatalogOrphans { .. }) => {
                            "catalog_orphans_blocked"
                        }
                        // A guard that cannot complete (unreadable chain,
                        // marker-listing failure) still fails the prune closed.
                        None => "guard_check_failed",
                    }
                };
                finish_span(guard_label, 0);
                return Err(guard_err);
            }

            let (retained_rows, removed_rows): (Vec<_>, Vec<_>) = rows
                .into_iter()
                .partition(|row| !existing_remove_ids.contains(&row.checkpoint_id));

            let (new_head, rewritten, rewritten_object_index_intents) = match expected_head {
                Some(head) => self.rebuild_checkpoint_history(head, &retained_rows)?,
                None => (None, Vec::new(), Vec::new()),
            };

            let unreachable_oids =
                collect_exclusive_unreachable_oids(&removed_rows, &retained_rows, &rewritten);

            match self
                .commit_checkpoint_prune(
                    expected_head,
                    new_head,
                    &rewritten,
                    &rewritten_object_index_intents,
                    &existing_remove_ids,
                    &unreachable_oids,
                    record_cloud_tombstones,
                )
                .await?
            {
                (
                    RefUpdateOutcome::Updated,
                    removed_checkpoints,
                    deleted_object_index_rows,
                    deleted_import_identities,
                ) => {
                    finish_span("markers_and_catalog_verified", deleted_object_index_rows);
                    return Ok(CheckpointPruneOutcome {
                        removed_checkpoints,
                        rewritten_checkpoints: rewritten.len(),
                        ref_rewritten: expected_head != new_head,
                        window_guard: "markers_and_catalog_verified",
                        deleted_object_index_rows,
                        deleted_import_identities,
                    });
                }
                (RefUpdateOutcome::HeadChanged, _, _, _)
                    if attempt < HISTORY_HEAD_CONFLICT_MAX_RETRIES =>
                {
                    continue;
                }
                (RefUpdateOutcome::HeadChanged, _, _, _) => {
                    return Err(anyhow!(
                        "traces head changed repeatedly while pruning checkpoints"
                    ));
                }
            }
        }

        unreachable!("checkpoint prune retry loop must return on success or terminal error")
    }

    /// AG-24a local erasure for one session (plan.md Task A8.5): make the
    /// three local faces consistent — rewrite `refs/libra/traces` to drop
    /// the session's checkpoints, delete its `agent_checkpoint` and
    /// `agent_session` rows, and clean the now-unreachable `object_index`
    /// rows. The append-only `agent_audit_log` is a separate table and is
    /// never touched.
    ///
    /// Order matters: checkpoints are pruned FIRST (while the catalog rows
    /// still exist, so the ref rewrite can enumerate what to keep), then
    /// the `agent_session` row is deleted. Deleting the session first would
    /// cascade its checkpoint rows away (FK `ON DELETE CASCADE`) and leave
    /// `refs/libra/traces` pointing at orphan commits.
    ///
    /// Cloud propagation (PD-03): the `agent_import_tombstone` written
    /// here is published to the D1 mirror by `libra cloud sync`, which
    /// also drops the erased session's mirror rows, and `libra cloud
    /// restore` is tombstone-first. Only R2 physical payload deletion
    /// remains a documented deferral.
    pub async fn erase_session_local(&self, session_id: &str) -> Result<SessionEraseOutcome> {
        use sea_orm::{Statement, Value};
        let backend = self.db_conn.get_database_backend();

        // ADR-DR-19 (M4): establish the anti-resurrection barrier BEFORE
        // pruning/deleting anything. The same transaction fences every
        // import/export/coverage holder for this provider identity. If the
        // later ref prune is interrupted, the session remains tombstoned and
        // a retry can safely finish deletion; no in-flight writer can revive
        // it in the meantime.
        let identity = self
            .db_conn
            .query_one_raw(Statement::from_sql_and_values(
                backend,
                "SELECT agent_kind, provider_session_id
                 FROM agent_session WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("read provider identity for session erasure")?;
        let provider_identity = if let Some(row) = identity {
            let agent_kind: String = row.try_get_by("agent_kind")?;
            let provider_session_id: String = row.try_get_by("provider_session_id")?;
            let txn = self
                .db_conn
                .begin()
                .await
                .context("begin agent erasure tombstone transaction")?;
            let incarnation_namespace = uuid::Uuid::new_v4().simple().to_string();
            let incarnation = txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "INSERT INTO agent_capture_incarnation (
                        agent_kind, provider_session_id, next_session_sync_revision,
                        source_namespace, updated_at
                     )
                     SELECT agent_kind, provider_session_id,
                            MAX(sync_revision + 1, 2), ?, ?
                     FROM agent_session WHERE session_id = ?
                     ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
                        next_session_sync_revision = MAX(
                            agent_capture_incarnation.next_session_sync_revision,
                            excluded.next_session_sync_revision
                        ),
                        source_namespace = excluded.source_namespace,
                        updated_at = excluded.updated_at",
                    [
                        incarnation_namespace.into(),
                        chrono::Utc::now().timestamp_millis().into(),
                        session_id.into(),
                    ],
                ))
                .await
                .context("preserve agent capture replication incarnation before erasure")?;
            if incarnation.rows_affected() != 1 {
                if let Err(rollback) = txn.rollback().await {
                    bail!(
                        "agent session disappeared while preserving its cloud replication \
                         incarnation, and rolling back that failed transaction also failed: \
                         {rollback}; retry the erase after checking the local database"
                    );
                }
                bail!(
                    "agent session disappeared while preserving its cloud replication incarnation; retry the erase"
                );
            }
            // V1 sessions can still carry a raw/unkeyed source fingerprint in
            // metadata. A tombstone is durable and may be mirrored, so retain
            // only the provider anti-resurrection key and actively clear any
            // legacy value on both insert and conflict update.
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_import_tombstone (
                    tombstone_id, agent_kind, provider_session_id,
                    erased_session_id, source_fingerprint, erased_at
                 ) VALUES (?, ?, ?, ?, NULL, ?)
                 ON CONFLICT(agent_kind, provider_session_id) DO UPDATE SET
                    erased_session_id = excluded.erased_session_id,
                    source_fingerprint = NULL,
                    erased_at = excluded.erased_at",
                [
                    uuid::Uuid::new_v4().to_string().into(),
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                    session_id.into(),
                    chrono::Utc::now().timestamp_millis().into(),
                ],
            ))
            .await
            .context("write agent import anti-resurrection tombstone")?;
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_import_identity
                 SET state = 'failed', owner = NULL, lease_expires_at = NULL,
                     fence_token = COALESCE(fence_token, 0) + 1,
                     last_error_code = 'LBR-AGENT-019', updated_at = ?
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    chrono::Utc::now().timestamp_millis().into(),
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                ],
            ))
            .await
            .context("fence import identity holders during erasure")?;
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_coverage_claim
                 SET state = 'abandoned', owner = NULL, lease_expires_at = NULL,
                     fence_token = COALESCE(fence_token, 0) + 1, updated_at = ?
                 WHERE session_id = ? AND state IN ('reserved_live','reserved_import')",
                [
                    chrono::Utc::now().timestamp_millis().into(),
                    session_id.into(),
                ],
            ))
            .await
            .context("fence coverage claim holders during erasure")?;
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "UPDATE agent_export_job
                 SET state = 'failed', owner = NULL, lease_expires_at = NULL,
                     fence_token = fence_token + 1,
                     last_error_code = 'LBR-AGENT-019', updated_at = ?
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    chrono::Utc::now().timestamp_millis().into(),
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                ],
            ))
            .await
            .context("fence export job holders during erasure")?;
            txn.commit()
                .await
                .context("commit agent erasure tombstone transaction")?;
            Some((agent_kind, provider_session_id))
        } else {
            None
        };

        // A session can be between reservation and its first catalog row, so
        // an empty checkpoint list does not prove that erasure has no writer
        // to race. Marker creation is serialized with the tombstone
        // transaction above; once the tombstone wins, no new marker for this
        // provider identity can be created. Refuse this attempt until the
        // already-marked writer finishes or its rejected objects are cleaned.
        let live_markers = list_live_traces_inflight_markers(
            self.db_conn.as_ref(),
            chrono::Utc::now().timestamp_millis(),
        )
        .await
        .context("verify in-flight writers before agent session erasure")?;
        if let Some(marker) = live_markers
            .into_iter()
            .find(|marker| marker.session_id == session_id)
        {
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: marker.session_id,
                attempt_id: marker.attempt_id,
                ttl_ms: marker.ttl_ms,
            }
            .into());
        }

        // Enumerate the session's checkpoints from the catalog.
        let rows = self
            .db_conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT checkpoint_id FROM agent_checkpoint WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("list checkpoints for session erasure")?;
        let checkpoint_ids: Vec<String> = rows
            .into_iter()
            .map(|row| row.try_get_by::<String, _>("checkpoint_id"))
            .collect::<std::result::Result<_, _>>()
            .context("decode checkpoint_id for session erasure")?;

        // Prune the checkpoints (ref rewrite + row + object_index) BEFORE
        // deleting the session row.
        // Do not create ordinary checkpoint-retention tombstones here:
        // the session tombstone above is the propagated anti-resurrection
        // authority. R2 physical payload deletion remains deferred.
        let prune = self
            .prune_checkpoint_commits_inner(&checkpoint_ids, false)
            .await?;

        // Delete the session row (cascades claims/revisions/checkpoints) and
        // application-owned import/export job rows together. The tombstone is
        // deliberately retained outside ordinary retention.
        let txn = self
            .db_conn
            .begin()
            .await
            .context("begin agent session catalog erasure")?;
        let capture_capacity_lost =
            crate::internal::ai::capture::pending::erase_session_artifacts(&txn, session_id)
                .await
                .context("remove private capture recovery artifacts during session erasure")?;
        let deleted = txn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_session WHERE session_id = ?",
                [Value::from(session_id.to_string())],
            ))
            .await
            .context("delete agent_session row for erasure")?;
        txn.execute_raw(Statement::from_sql_and_values(
            backend,
            "DELETE FROM metadata_kv WHERE scope = ? AND target = ?",
            [
                crate::internal::metadata::MetadataScope::AgentImportIndexRepair
                    .as_str()
                    .into(),
                session_id.into(),
            ],
        ))
        .await
        .context("delete import object-index repair marker for erased session")?;
        if let Some((agent_kind, provider_session_id)) = provider_identity {
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_import_identity
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [
                    agent_kind.clone().into(),
                    provider_session_id.clone().into(),
                ],
            ))
            .await
            .context("delete import identity rows for erased session")?;
            txn.execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_export_job
                 WHERE agent_kind = ? AND provider_session_id = ?",
                [agent_kind.into(), provider_session_id.into()],
            ))
            .await
            .context("delete export job rows for erased session")?;
        }
        txn.commit()
            .await
            .context("commit agent session catalog erasure")?;
        if capture_capacity_lost {
            tracing::warn!(
                "unattributable private capture recovery evidence was retained and still consumes capacity; run `libra agent doctor` and inspect repository backups"
            );
        }

        Ok(SessionEraseOutcome {
            session_deleted: deleted.rows_affected() > 0,
            removed_checkpoints: prune.removed_checkpoints,
            ref_rewritten: prune.ref_rewritten,
            deleted_object_index_rows: prune.deleted_object_index_rows,
        })
    }

    /// AG-20 window A/B prune guards (`agent.md` write-sequence matrix,
    /// rows 727-732). Both refusals are deterministic and fail-closed:
    ///
    /// - **Window A/B (live writer)**: any live in-flight marker — for ANY
    ///   session, not just the ones being pruned — blocks the prune. The
    ///   prune is a whole-chain rewrite of the shared `refs/libra/traces`
    ///   ref plus the catalog, so a concurrent writer between stages
    ///   (a)–(d) could otherwise lose loose objects (window A) or a
    ///   ref-reachable-but-uncataloged commit (window B). Markers carry a
    ///   TTL ([`AGENT_TRACES_INFLIGHT_TTL_MS`]), so a crashed writer only
    ///   defers pruning temporarily.
    /// - **Window B residue (ref-vs-catalog)**: walks the first-parent
    ///   chain of the current traces head and refuses when any reachable
    ///   commit has no `agent_checkpoint.traces_commit` row. The rebuild is
    ///   catalog-driven and would silently drop such commits; backfilling
    ///   the catalog is `libra agent doctor --repair`'s job.
    async fn enforce_prune_window_guards(
        &self,
        expected_head: Option<ObjectHash>,
        rows: &[CheckpointHistoryRow],
    ) -> Result<()> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        // A marker-listing failure cannot prove the absence of a live
        // writer — propagate it, which aborts (fails closed) the prune.
        let live_markers = list_live_traces_inflight_markers(self.db_conn.as_ref(), now_ms)
            .await
            .context("failed to verify traces in-flight markers (prune fails closed)")?;
        if let Some(marker) = live_markers.first() {
            return Err(CheckpointPruneGuardError::LiveWriterMarker {
                session_id: marker.session_id.clone(),
                attempt_id: marker.attempt_id.clone(),
                ttl_ms: marker.ttl_ms,
            }
            .into());
        }

        // DR-06 closes the short reservation→marker window: a subagent
        // source claim is durable before its marker can be registered. A
        // whole-chain prune in that interval could delete the claim's current
        // leaf and invalidate the writer's source revision base, so fail
        // closed on every unexpired reservation just as we do for markers.
        let reserved = self
            .db_conn
            .query_one_raw(Statement::from_sql_and_values(
                self.db_conn.get_database_backend(),
                "SELECT parent_session_id, attempt_checkpoint_id, lease_expires_at
                 FROM agent_subagent_content_claim
                 WHERE state = 'reserved' AND lease_expires_at > ?
                 ORDER BY lease_expires_at LIMIT 1",
                [now_ms.into()],
            ))
            .await
            .context("failed to verify subagent content reservations (prune fails closed)")?;
        if let Some(row) = reserved {
            return Err(SubagentContentReservationPruneGuard {
                session_id: row.try_get_by("parent_session_id")?,
                attempt_id: row
                    .try_get_by::<Option<String>, _>("attempt_checkpoint_id")?
                    .unwrap_or_else(|| "reservation-before-checkpoint-bind".to_string()),
                lease_expires_at: row.try_get_by("lease_expires_at")?,
            }
            .into());
        }

        let Some(head) = expected_head else {
            return Ok(());
        };
        let cataloged: HashSet<&str> = rows
            .iter()
            .filter_map(|row| row.traces_commit.as_deref())
            .collect();
        // An unreadable chain means the catalog cannot be verified —
        // propagate the walk error (fail closed) rather than pruning blind.
        let mut orphans: Vec<String> = Vec::new();
        for commit_hash in self.first_parent_commit_hashes(head)? {
            if !cataloged.contains(commit_hash.as_str()) {
                orphans.push(commit_hash);
            }
        }
        if let Some(first_commit) = orphans.first().cloned() {
            return Err(CheckpointPruneGuardError::RefCatalogOrphans {
                orphan_count: orphans.len(),
                first_commit,
            }
            .into());
        }
        Ok(())
    }

    /// First-parent commit hashes reachable from `head` (head first),
    /// with a visited-set cycle guard.
    fn first_parent_commit_hashes(&self, head: ObjectHash) -> Result<Vec<String>> {
        let mut hashes = Vec::new();
        let mut visited: HashSet<ObjectHash> = HashSet::new();
        let mut next = Some(head);
        while let Some(oid) = next {
            if !visited.insert(oid) {
                break;
            }
            let data = read_git_object(&self.repo_path, &oid).with_context(|| {
                format!("failed to read traces commit {oid} while walking refs/libra/traces")
            })?;
            let commit = Commit::from_bytes(&data, oid)
                .map_err(|err| anyhow!("failed to parse traces commit {oid}: {err}"))?;
            hashes.push(oid.to_string());
            next = commit.parent_commit_ids.first().copied();
        }
        Ok(hashes)
    }

    async fn load_checkpoint_history_rows(&self) -> Result<Vec<CheckpointHistoryRow>> {
        let backend = self.db_conn.get_database_backend();
        let rows = self
            .db_conn
            .query_all_raw(Statement::from_string(
                backend,
                "SELECT cp.checkpoint_id, cp.session_id, cp.scope, cp.parent_commit, \
                        cp.traces_commit, cp.tree_oid, cp.metadata_blob_oid, cp.created_at, \
                        COALESCE(s.agent_kind, 'unknown') AS agent_kind \
                 FROM agent_checkpoint cp \
                 LEFT JOIN agent_session s ON s.session_id = cp.session_id \
                 ORDER BY cp.created_at ASC, cp.checkpoint_id ASC"
                    .to_string(),
            ))
            .await
            .context("failed to load agent_checkpoint rows for traces rewrite")?;

        rows.into_iter()
            .map(CheckpointHistoryRow::from_query_result)
            .collect()
    }

    fn rebuild_checkpoint_history(
        &self,
        current_head: ObjectHash,
        retained_rows: &[CheckpointHistoryRow],
    ) -> Result<(
        Option<ObjectHash>,
        Vec<RewrittenCheckpoint>,
        Vec<CheckpointObjectIndexIntent>,
    )> {
        if retained_rows.is_empty() {
            return Ok((None, Vec::new(), Vec::new()));
        }

        let current_root = self.load_commit_tree(&current_head)?;
        let mut parent = None;
        let mut rewritten = Vec::with_capacity(retained_rows.len());
        // A prune rebuild must not enqueue repair markers and then have its
        // own deletion fence treat those markers as concurrent work. Keep
        // every replacement tree/commit intent in memory and publish it in
        // the same transaction that moves the ref and rewrites catalog rows.
        let mut object_index_intents = Vec::with_capacity(retained_rows.len() * 4);

        for row in retained_rows {
            let inner_tree = self
                .checkpoint_inner_tree_from_root(&current_root, &row.checkpoint_id)?
                .ok_or_else(|| {
                    anyhow!(
                        "traces tree is missing retained checkpoint {}",
                        row.checkpoint_id
                    )
                })?;
            let (prefix, rest) = checkpoint_tree_path(&row.checkpoint_id)?;
            let root_tree = self.splice_checkpoint_tree(
                parent,
                &prefix,
                &rest,
                inner_tree,
                &mut object_index_intents,
            )?;
            let commit_hash = self.write_rewritten_checkpoint_commit(
                parent,
                root_tree,
                row,
                &mut object_index_intents,
            )?;
            rewritten.push(RewrittenCheckpoint {
                checkpoint_id: row.checkpoint_id.clone(),
                traces_commit: commit_hash,
                tree_oid: root_tree,
            });
            parent = Some(commit_hash);
        }

        Ok((parent, rewritten, object_index_intents))
    }

    fn checkpoint_inner_tree_from_root(
        &self,
        root_items: &[TreeItem],
        checkpoint_id: &str,
    ) -> Result<Option<ObjectHash>> {
        let (prefix, rest) = checkpoint_tree_path(checkpoint_id)?;
        let Some(checkpoint_entry) = root_items.iter().find(|item| item.name == "checkpoint")
        else {
            return Ok(None);
        };
        if checkpoint_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint' entry expected to be a tree, got mode {:?}",
                checkpoint_entry.mode
            ));
        }

        let checkpoint_items = self.load_tree(&checkpoint_entry.id)?;
        let Some(prefix_entry) = checkpoint_items.iter().find(|item| item.name == prefix) else {
            return Ok(None);
        };
        if prefix_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a tree, got mode {:?}",
                prefix_entry.mode
            ));
        }

        let prefix_items = self.load_tree(&prefix_entry.id)?;
        let Some(rest_entry) = prefix_items.iter().find(|item| item.name == rest) else {
            return Ok(None);
        };
        if rest_entry.mode != TreeItemMode::Tree {
            return Err(anyhow!(
                "traces tree corruption: 'checkpoint/{prefix}/{rest}' entry expected to be a tree, got mode {:?}",
                rest_entry.mode
            ));
        }
        Ok(Some(rest_entry.id))
    }

    fn write_rewritten_checkpoint_commit(
        &self,
        parent: Option<ObjectHash>,
        root_tree: ObjectHash,
        row: &CheckpointHistoryRow,
        object_index_intents: &mut Vec<CheckpointObjectIndexIntent>,
    ) -> Result<ObjectHash> {
        let message = format!(
            "traces: {} checkpoint {}\n\n{}",
            row.scope,
            row.checkpoint_id,
            format_rewritten_checkpoint_trailers(row)
        );
        let author = Signature::new(
            SignatureType::Author,
            "Libra".to_string(),
            "traces@libra".to_string(),
        );
        let committer = Signature::new(
            SignatureType::Committer,
            "Libra".to_string(),
            "traces@libra".to_string(),
        );
        let parents = parent.into_iter().collect::<Vec<_>>();
        let commit = Commit::new(author, committer, root_tree, parents, &message);
        let commit_data = commit
            .to_data()
            .context("failed to serialize rewritten checkpoint commit")?;
        let commit_hash = write_git_object(&self.repo_path, "commit", &commit_data)?;
        object_index_intents.push(CheckpointObjectIndexIntent {
            oid: commit_hash.to_string(),
            object_type: "commit".to_string(),
            size: i64::try_from(commit_data.len())
                .context("rewritten checkpoint commit exceeds object-index size range")?,
        });
        Ok(commit_hash)
    }

    /// Transactionally CAS the traces ref, update rewritten rows, delete
    /// pruned rows, and drop `object_index` rows for
    /// `unreachable_oids` (the conservative exclusively-removed set from
    /// [`collect_exclusive_unreachable_oids`]). The `object_index` deletion
    /// is idempotent — re-running deletes nothing — and rides in the same
    /// transaction so a crash cannot leave the catalog and the index
    /// disagreeing about the pruned checkpoints.
    ///
    /// Returns `(outcome, removed_rows, deleted_object_index_rows,
    /// deleted_import_identities)`.
    // This transaction boundary deliberately keeps each independently
    // validated prune input explicit; bundling them would obscure which
    // durable sets participate in the single ref/catalog/index CAS.
    #[allow(clippy::too_many_arguments)]
    async fn commit_checkpoint_prune(
        &self,
        expected_head: Option<ObjectHash>,
        new_head: Option<ObjectHash>,
        rewritten: &[RewrittenCheckpoint],
        rewritten_object_index_intents: &[CheckpointObjectIndexIntent],
        remove_ids: &HashSet<String>,
        unreachable_oids: &[String],
        record_cloud_tombstones: bool,
    ) -> Result<(RefUpdateOutcome, u64, u64, u64)> {
        let expected_commit = expected_head.map(|hash| hash.to_string());
        let new_commit = new_head.map(|hash| hash.to_string());
        let _object_index_deletion_fence =
            crate::utils::client_storage::acquire_object_index_deletion_fence(
                &self.repo_path.join(crate::utils::util::DATABASE),
                unreachable_oids,
            )
            .await
            .with_context(|| {
                "refusing checkpoint prune because concurrent object-index repair work could recreate a deleted catalog row; retry after the repair marker drains"
            })?;

        'retry_sqlite: for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
            let txn: DatabaseTransaction =
                match crate::internal::db::begin_write_transaction(self.db_conn.as_ref()).await {
                    Ok(txn) => txn,
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue;
                    }
                    Err(err) => {
                        return Err(err).context("Failed to begin checkpoint prune transaction");
                    }
                };

            let existing = match reference::Entity::find()
                .filter(reference::Column::Name.eq(&self.ref_name))
                .filter(reference::Column::Kind.eq(ConfigKind::Branch))
                .one(&txn)
                .await
            {
                Ok(existing) => existing,
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                Err(err) => return Err(err).context("Failed to query checkpoint prune ref"),
            };

            let write_ref = match existing {
                Some(model) if model.commit != expected_commit => {
                    let _ = txn.rollback().await;
                    return Ok((RefUpdateOutcome::HeadChanged, 0, 0, 0));
                }
                Some(model) => {
                    let mut active: reference::ActiveModel = model.into();
                    active.commit = Set(new_commit.clone());
                    active.update(&txn).await.map(|_| ())
                }
                None if expected_commit.is_some() => {
                    let _ = txn.rollback().await;
                    return Ok((RefUpdateOutcome::HeadChanged, 0, 0, 0));
                }
                None => {
                    let new_ref = reference::ActiveModel {
                        name: Set(Some(self.ref_name.clone())),
                        kind: Set(ConfigKind::Branch),
                        commit: Set(new_commit.clone()),
                        remote: Set(None),
                        ..Default::default()
                    };
                    new_ref.insert(&txn).await.map(|_| ())
                }
            };

            if let Err(err) = write_ref {
                if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue;
                }
                return Err(err).context("Failed to update checkpoint prune ref");
            }

            let backend = txn.get_database_backend();
            for item in rewritten {
                if let Err(err) = txn
                    .execute_raw(Statement::from_sql_and_values(
                        backend,
                        "UPDATE agent_checkpoint SET traces_commit = ?, tree_oid = ?, \
                            sync_revision = sync_revision + 1 \
                         WHERE checkpoint_id = ?",
                        vec![
                            Value::from(item.traces_commit.to_string()),
                            Value::from(item.tree_oid.to_string()),
                            Value::from(item.checkpoint_id.clone()),
                        ],
                    ))
                    .await
                {
                    if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    return Err(err).context("Failed to update rewritten checkpoint row");
                }
            }

            let mut removed = 0;
            for id in remove_ids {
                if record_cloud_tombstones {
                    txn.execute_raw(Statement::from_sql_and_values(
                        backend,
                        "INSERT INTO agent_checkpoint_prune_tombstone (
                            checkpoint_id, session_id, pruned_at
                         )
                         SELECT checkpoint_id, session_id, ?
                         FROM agent_checkpoint WHERE checkpoint_id = ?
                         ON CONFLICT(checkpoint_id) DO UPDATE SET
                            session_id = excluded.session_id,
                            pruned_at = MAX(
                                agent_checkpoint_prune_tombstone.pruned_at,
                                excluded.pruned_at
                            )",
                        [
                            Value::from(chrono::Utc::now().timestamp_millis()),
                            Value::from(id.clone()),
                        ],
                    ))
                    .await
                    .context("record cloud fence for pruned checkpoint")?;
                }
                // DR-06: subagent content revisions are source-scoped, not
                // checkpoint-parent scoped. Repoint the current source leaf
                // to its newest surviving revision before the checkpoint FK
                // cascades its revision/link.  Keep an empty claim as the
                // durable revision high-water mark so a later capture cannot
                // reuse an audited revision number for different content.
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_subagent_content_revision
                     WHERE checkpoint_id = ?",
                    [Value::from(id.clone())],
                ))
                .await
                .context("delete subagent content revision for pruned checkpoint")?;
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE agent_subagent_content_claim
                     SET sync_revision = sync_revision + 1,
                         current_revision = COALESCE((
                           SELECT r.revision FROM agent_subagent_content_revision r
                           WHERE r.parent_session_id = agent_subagent_content_claim.parent_session_id
                             AND r.provider_kind = agent_subagent_content_claim.provider_kind
                             AND r.source_key = agent_subagent_content_claim.source_key
                             AND r.content_schema_version = agent_subagent_content_claim.content_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ), 0),
                         current_checkpoint_id = (
                           SELECT r.checkpoint_id FROM agent_subagent_content_revision r
                           WHERE r.parent_session_id = agent_subagent_content_claim.parent_session_id
                             AND r.provider_kind = agent_subagent_content_claim.provider_kind
                             AND r.source_key = agent_subagent_content_claim.source_key
                             AND r.content_schema_version = agent_subagent_content_claim.content_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         current_digest = (
                           SELECT r.content_digest FROM agent_subagent_content_revision r
                           WHERE r.parent_session_id = agent_subagent_content_claim.parent_session_id
                             AND r.provider_kind = agent_subagent_content_claim.provider_kind
                             AND r.source_key = agent_subagent_content_claim.source_key
                             AND r.content_schema_version = agent_subagent_content_claim.content_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         state = 'idle', attempt_digest = NULL,
                         attempt_checkpoint_id = NULL, owner = NULL,
                         lease_expires_at = NULL, updated_at = ?
                     WHERE current_checkpoint_id = ?",
                    [
                        Value::from(chrono::Utc::now().timestamp_millis()),
                        Value::from(id.clone()),
                    ],
                ))
                .await
                .context("repoint subagent content claim after checkpoint prune")?;
                // GC-DR-11: coverage revisions/current pointers and import
                // attempt cursors are part of the same catalog fact as the
                // checkpoint. Reconcile them before deleting the row so no
                // committed claim can point at a pruned checkpoint.
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_coverage_conflict
                     WHERE incumbent_checkpoint_id = ?
                        OR EXISTS (
                          SELECT 1 FROM agent_coverage_claim c
                          WHERE c.session_id = agent_coverage_conflict.session_id
                            AND c.logical_turn_key = agent_coverage_conflict.logical_turn_key
                            AND c.coverage_schema_version = agent_coverage_conflict.coverage_schema_version
                            AND c.checkpoint_id = ?
                        )",
                    [Value::from(id.clone()), Value::from(id.clone())],
                ))
                .await
                .context("delete conflict evidence whose incumbent checkpoint is pruned")?;
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_coverage_revision WHERE checkpoint_id = ?",
                    [Value::from(id.clone())],
                ))
                .await
                .context("delete coverage revisions for pruned checkpoint")?;
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_coverage_claim
                     WHERE checkpoint_id = ?
                       AND NOT EXISTS (
                         SELECT 1 FROM agent_coverage_revision r
                         WHERE r.session_id = agent_coverage_claim.session_id
                           AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                           AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                       )",
                    [Value::from(id.clone())],
                ))
                .await
                .context("delete coverage claims emptied by checkpoint prune")?;
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE agent_coverage_claim
                     SET revision = (
                           SELECT r.revision FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         coverage_digest = (
                           SELECT r.coverage_digest FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         completeness = (
                           SELECT r.completeness FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         source_channel = (
                           SELECT r.source_channel FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         checkpoint_id = (
                           SELECT r.checkpoint_id FROM agent_coverage_revision r
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         traces_commit = (
                           SELECT c.traces_commit
                           FROM agent_coverage_revision r
                           JOIN agent_checkpoint c ON c.checkpoint_id = r.checkpoint_id
                           WHERE r.session_id = agent_coverage_claim.session_id
                             AND r.logical_turn_key = agent_coverage_claim.logical_turn_key
                             AND r.coverage_schema_version = agent_coverage_claim.coverage_schema_version
                           ORDER BY r.revision DESC LIMIT 1
                         ),
                         state = 'catalog_committed', owner = NULL,
                         lease_expires_at = NULL, updated_at = ?
                     WHERE checkpoint_id = ?",
                    [
                        Value::from(chrono::Utc::now().timestamp_millis()),
                        Value::from(id.clone()),
                    ],
                ))
                .await
                .context("repoint coverage claim after checkpoint prune")?;
                txn.execute_raw(Statement::from_sql_and_values(
                    backend,
                    "UPDATE agent_import_identity SET attempt_checkpoint_id = NULL
                     WHERE attempt_checkpoint_id = ?",
                    [Value::from(id.clone())],
                ))
                .await
                .context("clear pruned import attempt checkpoint pointer")?;
                match txn
                    .execute_raw(Statement::from_sql_and_values(
                        backend,
                        "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
                        [Value::from(id.clone())],
                    ))
                    .await
                {
                    Ok(result) => removed += result.rows_affected(),
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    Err(err) => return Err(err).context("Failed to delete pruned checkpoint row"),
                }
            }

            let deleted_import_identities = txn
                .execute_raw(Statement::from_sql_and_values(
                    backend,
                    "DELETE FROM agent_import_identity
                 WHERE state IN ('discovered','partial','committed','failed')
                   AND owner IS NULL
                   AND EXISTS (
                     SELECT 1 FROM agent_session s
                     WHERE s.agent_kind = agent_import_identity.agent_kind
                       AND s.provider_session_id = agent_import_identity.provider_session_id
                       AND NOT EXISTS (
                         SELECT 1 FROM agent_coverage_claim c
                         WHERE c.session_id = s.session_id
                       )
                 )",
                    Vec::<Value>::new(),
                ))
                .await
                .context("delete import identity after pruning its final coverage claim")?
                .rows_affected();

            // AG-20: drop `object_index` rows for OIDs this prune made
            // unreachable so cloud sync stops advertising them. Rides in
            // the same transaction; idempotent (missing rows delete 0).
            let deleted_object_index_rows =
                match crate::utils::client_storage::remove_object_index_rows_with_conn(
                    &txn,
                    unreachable_oids,
                )
                .await
                {
                    Ok(count) => count,
                    Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                        let _ = txn.rollback().await;
                        sleep(Duration::from_millis(
                            SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                        ))
                        .await;
                        continue 'retry_sqlite;
                    }
                    Err(err) => {
                        return Err(err).context("Failed to delete pruned object_index rows");
                    }
                };

            // Publish all replacement trees/commits only after the prune has
            // deleted unreachable rows, and in the very same transaction as
            // the ref/catalog rewrite.  This ordering keeps an overlapping
            // OID reachable if a conservative reachability calculation ever
            // includes it in both sets, and avoids the self-marker race at
            // the deletion fence entirely.
            // Content-addressing can make two reconstructed paths share an
            // identical tree. Emit one UPSERT per object ID; Git object IDs
            // include their type, so equal IDs cannot disagree on the
            // object-index type or size.
            let mut seen_rewritten_oids =
                HashSet::with_capacity(rewritten_object_index_intents.len());
            let rewritten_object_index_updates = rewritten_object_index_intents
                .iter()
                .filter(|intent| seen_rewritten_oids.insert(intent.oid.clone()))
                .map(|intent| (intent.oid.clone(), intent.object_type.clone(), intent.size))
                .collect::<Vec<_>>();
            if let Err(err) =
                crate::utils::client_storage::upsert_agent_object_index_rows_with_conn(
                    &txn,
                    &rewritten_object_index_updates,
                )
                .await
            {
                if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES {
                    let _ = txn.rollback().await;
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                    continue 'retry_sqlite;
                }
                return Err(err)
                    .context("Failed to publish rewritten checkpoint object_index rows");
            }

            match txn.commit().await {
                Ok(()) => {
                    return Ok((
                        RefUpdateOutcome::Updated,
                        removed,
                        deleted_object_index_rows,
                        deleted_import_identities,
                    ));
                }
                Err(err) if is_sqlite_busy(&err) && attempt < SQLITE_BUSY_MAX_RETRIES => {
                    sleep(Duration::from_millis(
                        SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                    ))
                    .await;
                }
                Err(err) => {
                    return Err(err).context("Failed to commit checkpoint prune transaction");
                }
            }
        }

        unreachable!("sqlite busy retry loop must return on success or terminal error")
    }

    /// Splice `inner_tree` into `parent`'s tree at the path
    /// `checkpoint/<prefix>/<rest>`, preserving any existing entries in the
    /// surrounding subtrees. Phase 2.1 helper for [`append_checkpoint_commit`].
    fn splice_checkpoint_tree(
        &self,
        parent: Option<ObjectHash>,
        prefix: &str,
        rest: &str,
        inner_tree: ObjectHash,
        object_index_intents: &mut Vec<CheckpointObjectIndexIntent>,
    ) -> Result<ObjectHash> {
        let mut root_items = match parent {
            Some(parent_id) => self.load_commit_tree(&parent_id)?,
            None => Vec::new(),
        };
        let checkpoint_entry = root_items
            .iter()
            .find(|item| item.name == "checkpoint")
            .cloned();
        let mut checkpoint_items = match checkpoint_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => self.load_tree(&entry.id)?,
            Some(entry) => {
                return Err(anyhow!(
                    "traces tree corruption: 'checkpoint' entry expected to be a tree, \
                     got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                ));
            }
            None => Vec::new(),
        };

        let prefix_entry = checkpoint_items
            .iter()
            .find(|item| item.name == prefix)
            .cloned();
        let mut prefix_items = match prefix_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => self.load_tree(&entry.id)?,
            Some(entry) => {
                return Err(anyhow!(
                    "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a \
                     tree, got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                ));
            }
            None => Vec::new(),
        };

        prefix_items.retain(|item| item.name != rest);
        prefix_items.push(TreeItem::new(
            TreeItemMode::Tree,
            inner_tree,
            rest.to_string(),
        ));
        prefix_items.sort_by(|a, b| a.name.cmp(&b.name));
        let prefix_tree =
            self.write_tree_indexed_for_prune_rewrite(&prefix_items, object_index_intents)?;

        checkpoint_items.retain(|item| item.name != prefix);
        checkpoint_items.push(TreeItem::new(
            TreeItemMode::Tree,
            prefix_tree,
            prefix.to_string(),
        ));
        checkpoint_items.sort_by(|a, b| a.name.cmp(&b.name));
        let checkpoint_tree =
            self.write_tree_indexed_for_prune_rewrite(&checkpoint_items, object_index_intents)?;

        root_items.retain(|item| item.name != "checkpoint");
        root_items.push(TreeItem::new(
            TreeItemMode::Tree,
            checkpoint_tree,
            "checkpoint".to_string(),
        ));
        root_items.sort_by(|a, b| a.name.cmp(&b.name));
        self.write_tree_indexed_for_prune_rewrite(&root_items, object_index_intents)
    }

    #[allow(clippy::too_many_arguments)]
    async fn splice_checkpoint_tree_for_attempt(
        &self,
        parent: Option<ObjectHash>,
        prefix: &str,
        rest: &str,
        inner_tree: ObjectHash,
        writer_fence: &TracesWriterFence,
        capture_scope: Option<&CaptureScope>,
        deadline: Option<CaptureCommitDeadline>,
        newly_written: &mut HashSet<String>,
        object_index_intents: &mut Vec<CheckpointObjectIndexIntent>,
    ) -> Result<ObjectHash> {
        let mut root_items = match parent {
            Some(parent_id) => {
                self.load_commit_tree_for_attempt(
                    &parent_id,
                    deadline.map(CaptureCommitDeadline::monotonic),
                )
                .await?
            }
            None => Vec::new(),
        };
        let checkpoint_entry = root_items
            .iter()
            .find(|item| item.name == "checkpoint")
            .cloned();
        let mut checkpoint_items = match checkpoint_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => {
                self.load_tree_for_attempt(
                    &entry.id,
                    deadline.map(CaptureCommitDeadline::monotonic),
                )
                .await?
            }
            Some(entry) => {
                bail!(
                    "traces tree corruption: 'checkpoint' entry expected to be a tree, got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                )
            }
            None => Vec::new(),
        };
        let prefix_entry = checkpoint_items
            .iter()
            .find(|item| item.name == prefix)
            .cloned();
        let mut prefix_items = match prefix_entry {
            Some(entry) if entry.mode == TreeItemMode::Tree => {
                self.load_tree_for_attempt(
                    &entry.id,
                    deadline.map(CaptureCommitDeadline::monotonic),
                )
                .await?
            }
            Some(entry) => {
                bail!(
                    "traces tree corruption: 'checkpoint/{prefix}' entry expected to be a tree, got mode {:?} (oid {})",
                    entry.mode,
                    entry.id
                )
            }
            None => Vec::new(),
        };

        prefix_items.retain(|item| item.name != rest);
        prefix_items.push(TreeItem::new(
            TreeItemMode::Tree,
            inner_tree,
            rest.to_string(),
        ));
        prefix_items.sort_by(|a, b| a.name.cmp(&b.name));
        let prefix_tree = self
            .write_tree_indexed_for_attempt(
                &prefix_items,
                writer_fence,
                capture_scope,
                deadline,
                newly_written,
                object_index_intents,
            )
            .await?;

        checkpoint_items.retain(|item| item.name != prefix);
        checkpoint_items.push(TreeItem::new(
            TreeItemMode::Tree,
            prefix_tree,
            prefix.to_string(),
        ));
        checkpoint_items.sort_by(|a, b| a.name.cmp(&b.name));
        let checkpoint_tree = self
            .write_tree_indexed_for_attempt(
                &checkpoint_items,
                writer_fence,
                capture_scope,
                deadline,
                newly_written,
                object_index_intents,
            )
            .await?;

        root_items.retain(|item| item.name != "checkpoint");
        root_items.push(TreeItem::new(
            TreeItemMode::Tree,
            checkpoint_tree,
            "checkpoint".to_string(),
        ));
        root_items.sort_by(|a, b| a.name.cmp(&b.name));
        self.write_tree_indexed_for_attempt(
            &root_items,
            writer_fence,
            capture_scope,
            deadline,
            newly_written,
            object_index_intents,
        )
        .await
    }

    #[cfg(test)]
    pub fn get_storage(&self) -> Arc<dyn Storage + Send + Sync> {
        self.storage.clone()
    }
}

/// Persist an `ai_session` blob onto [`AI_REF`]. KEEP hook code calls this
/// instead of constructing [`HistoryManager::new`] or naming [`AI_REF`].
pub async fn persist_ai_session(
    storage: Arc<dyn Storage + Send + Sync>,
    repo_path: PathBuf,
    db_conn: Arc<DatabaseConnection>,
    object_type: &str,
    object_id: &str,
    blob_hash: ObjectHash,
) -> Result<(ObjectHash, bool)> {
    HistoryManager::new(storage, repo_path, db_conn)
        .persist_typed_object(object_type, object_id, blob_hash)
        .await
}

/// Canonical intent-ref name. KEEP capture paths must not import [`AI_REF`].
pub fn ai_ref_name() -> &'static str {
    AI_REF
}

pub use crate::internal::ai::traces::{
    AGENT_TRACES_INFLIGHT_TTL_MS, CHECKPOINT_CONTENT_HASH_COVERAGE,
    CHECKPOINT_LIFECYCLE_EVENTS_FILE, CHECKPOINT_MANIFEST_SCHEMA_VERSION,
    CHECKPOINT_METADATA_SCHEMA_VERSION, CheckpointCommit, CheckpointCommitParams,
    CheckpointPruneGuardError, CheckpointPruneOutcome, CheckpointScope, RebuildCatalogRowInputs,
    RebuiltCatalogRow, SessionEraseOutcome, TRACES_INFLIGHT_FUTURE_SKEW_MS,
    TRACES_INFLIGHT_MAX_LIVE_MS, TRANSCRIPT_CHUNK_THRESHOLD_BYTES, TracesCommitCtx,
    TracesCoverageFence, TracesInflightMarker, TracesTxnExtra,
    agent_checkpoint_id_for_traces_commit, checkpoint_content_hash, chunk_transcript_line_safe,
    clear_non_cleanup_traces_inflight_marker, clear_traces_inflight_marker,
    clear_traces_inflight_marker_if_generation,
    clear_traces_inflight_marker_if_generation_with_capture_scope,
    list_live_traces_inflight_markers, parse_content_hash, reassemble_transcript_chunks,
    rebuild_catalog_row_from_traces_ref, register_traces_write_attempt,
    retire_stale_traces_inflight_marker, transcript_chunk_threshold,
    update_traces_inflight_marker_if_generation,
    update_traces_inflight_marker_if_generation_with_capture_scope, write_traces_inflight_marker,
};
pub(crate) use crate::internal::ai::traces::{
    ManifestBlobRef, RejectedCheckpointCleanupDeferred, SubagentContentReservationPruneGuard,
    TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES,
    TracesInflightRejectedCleanupMarkerBoundExceeded, TranscriptPartRef,
    build_checkpoint_manifest_json, decode_and_validate_traces_inflight_marker,
    decode_and_validate_traces_inflight_marker_for_rejected_cleanup,
    list_all_traces_inflight_markers,
};

#[cfg(test)]
pub(crate) async fn checkpoint_leaf_durable_oids<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoint_id: &str,
    traces_commit: &str,
    tree_oid: &str,
    metadata_blob_oid: &str,
) -> Result<HashSet<String>> {
    checkpoint_snapshot_durable_oids(
        conn,
        repo_path,
        &[CheckpointDurabilitySpec {
            checkpoint_id,
            traces_commit,
            tree_oid,
            metadata_blob_oid,
        }],
        None,
    )
    .await
}

#[derive(Clone, Copy)]
pub(crate) struct CheckpointDurabilitySpec<'a> {
    pub checkpoint_id: &'a str,
    pub traces_commit: &'a str,
    pub tree_oid: &'a str,
    pub metadata_blob_oid: &'a str,
}

const CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS: usize = 100_000;
const CHECKPOINT_DURABILITY_MAX_OBJECTS: usize = 100_000;

fn insert_durable_oid_bounded(
    durable_oids: &mut HashSet<String>,
    oid: String,
    max_objects: usize,
) -> Result<()> {
    if !durable_oids.contains(&oid) && durable_oids.len() >= max_objects {
        bail!("checkpoint durability verification exceeded its aggregate object limit");
    }
    durable_oids.insert(oid);
    Ok(())
}

/// Verify a whole capture snapshot with one first-parent traversal. The older
/// per-leaf probe is intentionally retained as a one-item wrapper above for
/// unchanged-replay callers, while cloud sync uses this bounded batch form to
/// avoid quadratic commit reads.
pub(crate) async fn checkpoint_snapshot_durable_oids<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    #[cfg(test)]
    TEST_CHECKPOINT_SNAPSHOT_VERIFY_COUNT
        .try_with(|count| count.set(count.get().saturating_add(1)))
        .ok();
    if checkpoints.is_empty() {
        return Ok(HashSet::new());
    }
    let catalog_rows = conn
        .query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT traces_commit FROM agent_checkpoint ORDER BY traces_commit".to_string(),
        ))
        .await
        .context("load checkpoint catalog while verifying traces durability")?;
    if catalog_rows.len() > CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS {
        bail!("checkpoint catalog exceeds its durability verification limit");
    }
    let cataloged_commits = catalog_rows
        .into_iter()
        .map(|row| row.try_get_by::<String, _>("traces_commit"))
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("decode checkpoint catalog traces commits")?;
    checkpoint_snapshot_durable_oids_with_catalog(
        conn,
        repo_path,
        checkpoints,
        cataloged_commits,
        deadline,
    )
    .await
}

#[cfg(test)]
pub(crate) async fn checkpoint_rows_snapshot_durable_oids<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    if checkpoints.is_empty() {
        return Ok(HashSet::new());
    }
    let cataloged_commits = checkpoints
        .iter()
        .map(|checkpoint| checkpoint.traces_commit.to_string())
        .collect::<Vec<_>>();
    checkpoint_snapshot_durable_oids_with_catalog(
        conn,
        repo_path,
        checkpoints,
        cataloged_commits,
        deadline,
    )
    .await
}

async fn checkpoint_snapshot_durable_oids_with_catalog<C: ConnectionTrait>(
    conn: &C,
    repo_path: &Path,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    cataloged_commits: Vec<String>,
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    let head_row = conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT `commit` FROM reference
             WHERE name = ? AND kind = 'Branch' AND remote IS NULL LIMIT 1",
            [crate::internal::branch::TRACES_BRANCH.into()],
        ))
        .await
        .context("resolve traces ref while verifying unchanged checkpoint")?;
    let head = head_row
        .map(|row| {
            row.try_get_by::<Option<String>, _>("commit")
                .context("decode refs/libra/traces head")
        })
        .transpose()?
        .flatten()
        .context("refs/libra/traces is missing while verifying unchanged checkpoint")?;
    checkpoint_rows_snapshot_durable_oids_from_head(
        repo_path,
        &head,
        &cataloged_commits,
        checkpoints,
        deadline,
    )
    .await
}

/// Verify a supplied checkpoint catalog against an explicitly fenced traces
/// head. Cloud restore uses the head stored in the completed capture manifest
/// instead of trusting independently uploaded generic reference metadata.
pub(crate) async fn checkpoint_rows_snapshot_durable_oids_from_head(
    repo_path: &Path,
    head: &str,
    cataloged_commits: &[String],
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    let parsed_head = crate::internal::ai::util::parse_commit_anchor_for_kind(
        git_internal::hash::get_hash_kind(),
        head,
    )
    .and_then(|r| r.to_object_hash())
    .map_err(|error| anyhow!("invalid fenced refs/libra/traces head: {error}"))?;

    #[cfg(not(test))]
    if let Some(deadline) = deadline {
        let checkpoints = checkpoints
            .iter()
            .map(|checkpoint| CheckpointDurabilityHelperSpec {
                checkpoint_id: checkpoint.checkpoint_id.to_string(),
                traces_commit: checkpoint.traces_commit.to_string(),
                tree_oid: checkpoint.tree_oid.to_string(),
                metadata_blob_oid: checkpoint.metadata_blob_oid.to_string(),
            })
            .collect();
        return match invoke_checkpoint_object_helper(
            repo_path,
            CheckpointObjectIoOperation::VerifySnapshot {
                head: head.to_string(),
                cataloged_commits: cataloged_commits.to_vec(),
                checkpoints,
            },
            deadline,
        )
        .await?
        {
            CheckpointObjectIoHelperResponse::Verified { oids } => Ok(oids.into_iter().collect()),
            CheckpointObjectIoHelperResponse::Error { code } => bail!("{}", code.user_message()),
            CheckpointObjectIoHelperResponse::Read { .. }
            | CheckpointObjectIoHelperResponse::Written { .. } => {
                bail!("checkpoint object-I/O helper returned a non-verify response")
            }
        };
    }

    let cataloged_commits = parse_cataloged_traces_commits(cataloged_commits)?;
    checkpoint_snapshot_durable_oids_from_head(
        repo_path,
        parsed_head,
        &cataloged_commits,
        checkpoints,
        deadline,
    )
}

fn parse_cataloged_traces_commits(commits: &[String]) -> Result<HashSet<String>> {
    if commits.len() > CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS {
        bail!("checkpoint catalog exceeds its durability verification limit");
    }
    Ok(commits.iter().cloned().collect())
}

fn checkpoint_snapshot_durable_oids_from_head(
    repo_path: &Path,
    head: ObjectHash,
    cataloged_commits: &HashSet<String>,
    checkpoints: &[CheckpointDurabilitySpec<'_>],
    deadline: Option<Instant>,
) -> Result<HashSet<String>> {
    const MAX_CHECKPOINT_OBJECTS: usize = 16_384;
    const MAX_COMMIT_OR_TREE_BYTES: u64 = 4 * 1024 * 1024;
    const MAX_CHECKPOINT_BLOB_BYTES: u64 = 32 * 1024 * 1024;

    let mut expected = HashMap::new();
    for checkpoint in checkpoints {
        let commit = crate::internal::ai::util::parse_commit_anchor_for_kind(
            git_internal::hash::get_hash_kind(),
            checkpoint.traces_commit,
        )
        .and_then(|r| r.to_object_hash())
        .map_err(|error| anyhow!("invalid checkpoint traces commit: {error}"))?;
        let tree = crate::internal::ai::util::parse_repo_object_id(checkpoint.tree_oid)
            .map_err(|error| anyhow!("invalid checkpoint root tree: {error}"))?;
        if expected.insert(commit, tree).is_some() {
            bail!("multiple checkpoints share one traces commit");
        }
    }

    let mut next = Some(head);
    let mut visited = HashSet::new();
    let mut durable_oids = HashSet::new();
    let mut found_trees = HashMap::new();
    while let Some(oid) = next {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("checkpoint snapshot durability verification exceeded its deadline");
        }
        if !visited.insert(oid) {
            bail!("refs/libra/traces contains a first-parent cycle");
        }
        let oid_text = oid.to_string();
        insert_durable_oid_bounded(
            &mut durable_oids,
            oid_text.clone(),
            CHECKPOINT_DURABILITY_MAX_OBJECTS,
        )?;
        if visited.len() > CHECKPOINT_DURABILITY_MAX_REACHABLE_COMMITS {
            bail!("refs/libra/traces reachability probe exceeded its commit limit");
        }
        if !cataloged_commits.contains(&oid_text) {
            bail!(
                "refs/libra/traces reaches uncataloged commit {oid}; run `libra agent doctor --repair` before cloud sync or replay"
            );
        }
        let (object_type, data) =
            read_git_object_bounded_validated(repo_path, &oid, MAX_COMMIT_OR_TREE_BYTES)
                .with_context(|| format!("read traces commit {oid}"))?;
        if object_type != "commit" {
            bail!("traces ref points through non-commit object {oid}");
        }
        let commit = Commit::from_bytes(&data, oid)
            .map_err(|error| anyhow!("parse traces commit {oid}: {error}"))?;
        if let Some(expected_tree) = expected.get(&oid) {
            if commit.tree_id != *expected_tree {
                bail!("checkpoint commit root tree no longer matches its catalog row");
            }
            found_trees.insert(oid, commit.tree_id);
        }
        next = commit.parent_commit_ids.first().copied();
    }
    if found_trees.len() != expected.len() {
        bail!("one or more checkpoint commits are no longer reachable from refs/libra/traces");
    }

    for checkpoint in checkpoints {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("checkpoint snapshot durability verification exceeded its deadline");
        }
        let expected_tree = crate::internal::ai::util::parse_repo_object_id(checkpoint.tree_oid)
            .map_err(|error| anyhow!("invalid checkpoint root tree: {error}"))?;
        let expected_metadata =
            crate::internal::ai::util::parse_repo_object_id(checkpoint.metadata_blob_oid)
                .map_err(|error| anyhow!("invalid checkpoint metadata blob: {error}"))?;
        let mut leaf_oids = checkpoint_leaf_tree_durable_oids(
            repo_path,
            checkpoint.checkpoint_id,
            expected_tree,
            expected_metadata,
            deadline,
            MAX_CHECKPOINT_OBJECTS,
            MAX_COMMIT_OR_TREE_BYTES,
            MAX_CHECKPOINT_BLOB_BYTES,
        )?;
        for oid in leaf_oids.drain() {
            insert_durable_oid_bounded(&mut durable_oids, oid, CHECKPOINT_DURABILITY_MAX_OBJECTS)?;
        }
    }
    Ok(durable_oids)
}

#[allow(clippy::too_many_arguments)]
fn checkpoint_leaf_tree_durable_oids(
    repo_path: &Path,
    checkpoint_id: &str,
    expected_tree: ObjectHash,
    expected_metadata: ObjectHash,
    deadline: Option<Instant>,
    max_checkpoint_objects: usize,
    max_commit_or_tree_bytes: u64,
    max_checkpoint_blob_bytes: u64,
) -> Result<HashSet<String>> {
    let mut durable_oids = HashSet::new();
    let (root_type, root_bytes) =
        read_git_object_bounded_validated(repo_path, &expected_tree, max_commit_or_tree_bytes)
            .context("read checkpoint root tree")?;
    if root_type != "tree" {
        bail!("checkpoint root object is not a tree");
    }
    durable_oids.insert(expected_tree.to_string());
    let root = Tree::from_bytes(&root_bytes, expected_tree)
        .map_err(|error| anyhow!("parse checkpoint root tree: {error}"))?;
    let (prefix, rest) = checkpoint_tree_path(checkpoint_id)?;
    let (checkpoint_root_oid, checkpoint_root) =
        tree_child(repo_path, &root.tree_items, "checkpoint")?
            .context("checkpoint root tree has no checkpoint directory")?;
    durable_oids.insert(checkpoint_root_oid.to_string());
    let (prefix_root_oid, prefix_root) = tree_child(repo_path, &checkpoint_root, &prefix)?
        .context("checkpoint root tree has no checkpoint prefix directory")?;
    durable_oids.insert(prefix_root_oid.to_string());
    let leaf_oid = prefix_root
        .iter()
        .find(|item| item.name == rest && item.mode == TreeItemMode::Tree)
        .map(|item| item.id)
        .context("checkpoint root tree has no durable leaf for the checkpoint id")?;

    let mut stack = vec![leaf_oid];
    let mut object_count = 0_usize;
    let mut metadata_matches = false;
    let mut seen = HashSet::new();
    while let Some(oid) = stack.pop() {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            bail!("checkpoint snapshot durability verification exceeded its deadline");
        }
        if !seen.insert(oid) {
            continue;
        }
        durable_oids.insert(oid.to_string());
        object_count = object_count.saturating_add(1);
        if object_count > max_checkpoint_objects {
            bail!("checkpoint object verification exceeded its object limit");
        }
        let (object_type, data) =
            read_git_object_bounded_validated(repo_path, &oid, max_checkpoint_blob_bytes)
                .with_context(|| format!("read checkpoint object {oid}"))?;
        if object_type != "tree" {
            bail!("checkpoint directory object {oid} is not a tree");
        }
        let tree = Tree::from_bytes(&data, oid)
            .map_err(|error| anyhow!("parse checkpoint tree {oid}: {error}"))?;
        for item in tree.tree_items {
            if oid == leaf_oid && item.name == "metadata.json" {
                if item.mode == TreeItemMode::Tree || item.id != expected_metadata {
                    bail!("checkpoint metadata blob no longer matches its catalog row");
                }
                metadata_matches = true;
            }
            if item.mode == TreeItemMode::Tree {
                stack.push(item.id);
                continue;
            }
            let (object_type, _) =
                read_git_object_bounded_validated(repo_path, &item.id, max_checkpoint_blob_bytes)
                    .with_context(|| format!("read checkpoint blob {}", item.id))?;
            if object_type != "blob" {
                bail!("checkpoint leaf {} is not a blob", item.id);
            }
            durable_oids.insert(item.id.to_string());
            object_count = object_count.saturating_add(1);
            if object_count > max_checkpoint_objects {
                bail!("checkpoint object verification exceeded its object limit");
            }
        }
    }
    if !metadata_matches {
        bail!("checkpoint leaf has no matching metadata.json blob");
    }
    Ok(durable_oids)
}

fn tree_child(
    repo_path: &Path,
    items: &[TreeItem],
    name: &str,
) -> Result<Option<(ObjectHash, Vec<TreeItem>)>> {
    let Some(entry) = items
        .iter()
        .find(|item| item.name == name && item.mode == TreeItemMode::Tree)
    else {
        return Ok(None);
    };
    let (object_type, data) =
        read_git_object_bounded_validated(repo_path, &entry.id, 4 * 1024 * 1024)
            .with_context(|| format!("read checkpoint tree component {name}"))?;
    if object_type != "tree" {
        bail!("checkpoint tree component {name} is not a tree");
    }
    let tree = Tree::from_bytes(&data, entry.id)
        .map_err(|error| anyhow!("parse checkpoint tree component {name}: {error}"))?;
    Ok(Some((entry.id, tree.tree_items)))
}

#[derive(Debug, Clone)]
struct CheckpointHistoryRow {
    checkpoint_id: String,
    session_id: String,
    agent_kind: String,
    scope: String,
    parent_commit: Option<String>,
    /// `agent_checkpoint.traces_commit` — the commit this row currently
    /// points at on `refs/libra/traces`. Consumed by the prune-side
    /// ref-vs-catalog window-B guard and the `object_index` cleanup.
    traces_commit: Option<String>,
    /// `agent_checkpoint.tree_oid` (root tree of `traces_commit`).
    tree_oid: Option<String>,
    /// `agent_checkpoint.metadata_blob_oid` (the checkpoint's
    /// `metadata.json` blob).
    metadata_blob_oid: Option<String>,
}

impl CheckpointHistoryRow {
    fn from_query_result(row: QueryResult) -> Result<Self> {
        Ok(Self {
            checkpoint_id: row
                .try_get_by("checkpoint_id")
                .context("decode agent_checkpoint.checkpoint_id")?,
            session_id: row
                .try_get_by("session_id")
                .context("decode agent_checkpoint.session_id")?,
            agent_kind: row
                .try_get_by("agent_kind")
                .context("decode agent_session.agent_kind")?,
            scope: row
                .try_get_by("scope")
                .context("decode agent_checkpoint.scope")?,
            parent_commit: row.try_get_by("parent_commit").ok().flatten(),
            traces_commit: row.try_get_by("traces_commit").ok().flatten(),
            tree_oid: row.try_get_by("tree_oid").ok().flatten(),
            metadata_blob_oid: row.try_get_by("metadata_blob_oid").ok().flatten(),
        })
    }
}

#[derive(Debug, Clone)]
struct RewrittenCheckpoint {
    checkpoint_id: String,
    traces_commit: ObjectHash,
    tree_oid: ObjectHash,
}

/// OIDs that a prune provably makes unreachable and that are exclusively
/// referenced by the removed checkpoints — the conservative candidate set
/// for `object_index` cleanup (AG-20; the pre-fix behaviour leaked every
/// row forever).
///
/// Included per removed catalog row: its `traces_commit` (the commit
/// object), `tree_oid` (the commit's root tree), and `metadata_blob_oid`
/// (its `metadata.json` blob). Each is referenced only by that
/// checkpoint's chain entry by construction, and anything still referenced
/// is excluded below.
///
/// Deliberately **excluded** (exclusivity is not cheaply provable from the
/// catalog, so we skip rather than risk deleting a shared OID):
/// - inner checkpoint subtrees and transcript/events/manifest blobs of the
///   removed checkpoints (their OIDs are not recorded in the catalog);
/// - the pre-rewrite commits/root trees of RETAINED checkpoints (they may
///   be byte-identical to their rewritten successors, and the leak is
///   bounded by the retained-row count).
///
/// The exclusion set covers every OID the catalog still references after
/// the prune: retained rows' current OIDs plus the freshly rewritten
/// commits/trees and the new head.
fn collect_exclusive_unreachable_oids(
    removed_rows: &[CheckpointHistoryRow],
    retained_rows: &[CheckpointHistoryRow],
    rewritten: &[RewrittenCheckpoint],
) -> Vec<String> {
    let mut still_referenced: HashSet<String> = HashSet::new();
    for row in retained_rows {
        still_referenced.extend(
            [&row.traces_commit, &row.tree_oid, &row.metadata_blob_oid]
                .into_iter()
                .filter_map(|oid| oid.clone()),
        );
    }
    for item in rewritten {
        still_referenced.insert(item.traces_commit.to_string());
        still_referenced.insert(item.tree_oid.to_string());
    }

    let mut seen: HashSet<String> = HashSet::new();
    let mut unreachable = Vec::new();
    for row in removed_rows {
        for oid in [&row.traces_commit, &row.tree_oid, &row.metadata_blob_oid]
            .into_iter()
            .filter_map(|oid| oid.clone())
        {
            // Legacy rows may spell "no traces commit" as an EMPTY string
            // rather than NULL — an empty id is not an object to deindex,
            // and passing it to the deletion fence aborts the whole prune.
            if oid.is_empty() {
                continue;
            }
            if !still_referenced.contains(&oid) && seen.insert(oid.clone()) {
                unreachable.push(oid);
            }
        }
    }
    unreachable
}

fn checkpoint_tree_path(checkpoint_id: &str) -> Result<(String, String)> {
    let prefix = checkpoint_id
        .get(..2)
        .ok_or_else(|| anyhow!("checkpoint_id must be at least 2 characters"))?
        .to_string();
    let rest = checkpoint_id
        .get(2..)
        .ok_or_else(|| anyhow!("checkpoint_id must be valid UTF-8 at byte 2"))?
        .to_string();
    Ok((prefix, rest))
}

fn format_libra_trailers(params: &CheckpointCommitParams<'_>) -> String {
    let mut buf = String::new();
    buf.push_str(&format!("Libra-Session: {}\n", params.session_id));
    buf.push_str(&format!("Libra-Agent: {}\n", params.agent_kind));
    if let Some(commit) = params.parent_commit {
        buf.push_str(&format!("Libra-Parent-Commit: {commit}\n"));
    }
    buf.push_str(&format!("Libra-Checkpoint-ID: {}\n", params.checkpoint_id));
    buf.push_str(&format!("Libra-Scope: {}\n", params.scope.as_str()));
    if let Some(tool) = params.tool_use_id {
        buf.push_str(&format!("Libra-Tool-Use-ID: {tool}\n"));
    }
    buf
}

fn format_rewritten_checkpoint_trailers(row: &CheckpointHistoryRow) -> String {
    let mut buf = String::new();
    buf.push_str(&format!("Libra-Session: {}\n", row.session_id));
    buf.push_str(&format!("Libra-Agent: {}\n", row.agent_kind));
    if let Some(commit) = &row.parent_commit {
        buf.push_str(&format!("Libra-Parent-Commit: {commit}\n"));
    }
    buf.push_str(&format!("Libra-Checkpoint-ID: {}\n", row.checkpoint_id));
    buf.push_str(&format!("Libra-Scope: {}\n", row.scope));
    buf
}

/// Crate-visible fault seams for checkpoint-store replay tests. They act on
/// the manager the store constructs for one write, so production append code
/// keeps a single path and no environment knob can reach it.
#[cfg(test)]
impl HistoryManager {
    /// Fail the next checkpoint append after every attempt object has been
    /// written and recorded in the writer marker, immediately before the
    /// final ref/companion CAS transaction.
    pub(crate) fn fail_once_before_checkpoint_ref_cas(&mut self) {
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        self.test_before_checkpoint_ref_cas = Some(Arc::new(move |_snapshot| {
            let fired = fired.clone();
            Box::pin(async move {
                if fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    return Ok(());
                }
                bail!("injected checkpoint object-store failure before ref CAS")
            })
        }));
    }

    /// Make every bounded ref-CAS attempt lose to a concurrent traces writer:
    /// after each head read, a valid competing commit moves the same ref, so
    /// the append exhausts its real retry loop rather than receiving a
    /// synthetic typed error.
    pub(crate) fn lose_every_checkpoint_ref_cas(&mut self) {
        let interloper = Arc::new(Self::new_with_ref(
            self.storage.clone(),
            self.repo_path.clone(),
            self.db_conn.clone(),
            self.ref_name.clone(),
        ));
        let moves = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        self.test_after_head_read = Some(Arc::new(move || {
            let interloper = interloper.clone();
            let moves = moves.clone();
            Box::pin(async move {
                let index = moves.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let parent = interloper.resolve_history_head().await?;
                let blob = write_git_object(
                    &interloper.repo_path,
                    "blob",
                    format!("competing traces writer {index}").as_bytes(),
                )
                .context("write competing traces blob")?;
                let tree = interloper
                    .write_tree(&[TreeItem::new(
                        TreeItemMode::Blob,
                        blob,
                        "competing".to_string(),
                    )])
                    .context("write competing traces tree")?;
                let signature =
                    |kind| Signature::new(kind, "Libra".to_string(), "traces@libra".to_string());
                let commit = Commit::new(
                    signature(SignatureType::Author),
                    signature(SignatureType::Committer),
                    tree,
                    parent.into_iter().collect(),
                    "test competing traces head",
                );
                let head = write_git_object(
                    &interloper.repo_path,
                    "commit",
                    &commit
                        .to_data()
                        .context("serialize competing traces commit")?,
                )
                .context("write competing traces commit")?;
                match interloper
                    .update_ref_if_matches(&interloper.ref_name, parent, head)
                    .await?
                {
                    RefUpdateOutcome::Updated => Ok(()),
                    RefUpdateOutcome::HeadChanged => {
                        bail!("test competing traces writer lost its own CAS")
                    }
                }
            })
        }));
    }

    /// Run doctor's marker-repair entry point under the in-process index
    /// snapshot seam that library tests use instead of the private helper
    /// program registered by Libra's `main`.
    pub(crate) async fn repair_expired_traces_inflight_marker_for_test(
        &self,
        session_id: &str,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        TEST_DIRECT_REJECTED_CLEANUP_INDEX_SNAPSHOT
            .scope((), async {
                crate::utils::client_storage::ClientStorage::with_background_index_failure_scope(
                    self.repair_expired_traces_inflight_marker(session_id, attempt_id, now_ms),
                )
                .await
            })
            .await
    }
}

#[cfg(test)]
mod tests {
    use sea_orm::{ConnectionTrait, Database, Schema, Statement};
    use tempfile::tempdir;
    use tokio::time::sleep;

    /// Ownership-only erasure fixture: intentionally no replay authority.
    /// The full payload/MAC round trip is tested by capture::pending; deletion
    /// must also work when the key and evictable receipt ledger are gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn pending_artifact_is_removed_by_session_erase() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use uuid::Uuid;

        use crate::internal::{
            ai::{
                capture::{
                    catalog::{
                        CaptureCatalogAction, CaptureCatalogApplyRequest, CaptureCatalogError,
                        CaptureCatalogMutation, CaptureCatalogPort, CaptureCatalogSession,
                        CaptureCatalogStore, resolve_pending_session_context,
                    },
                    key,
                    pending::{PendingBinding, PendingHeader},
                    pending_identity::{self, PendingSessionAlias},
                },
                capture_scope::CaptureScope,
            },
            config::ConfigKv,
            metadata::{MetadataKv, MetadataScope, MetadataValueType},
        };

        let root = tempdir().unwrap();
        let repo = root.path().join(".libra");
        std::fs::create_dir_all(repo.join("objects")).unwrap();
        let conn = crate::internal::db::create_database(repo.join("libra.db").to_str().unwrap())
            .await
            .unwrap();
        ConfigKv::set_with_conn(&conn, "libra.repoid", "history-private-erase", false)
            .await
            .unwrap();
        key::load_capture_dedup_secret(&repo).unwrap();
        let pk = "claude__history-erasure-native";
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
             state, working_dir, metadata_json, started_at, last_event_at, sync_revision,
             repo_id, worktree_id, scope_state)
             VALUES (?, 'claude_code', 'history-erasure-native', 'active', ?, '{}',
             1, 1, 1, 'history-private-erase', '', 'scoped')",
            [pk.into(), root.path().to_string_lossy().into_owned().into()],
        ))
        .await
        .unwrap();
        let scope = CaptureScope::resolve(&conn, root.path()).await.unwrap();
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let context = resolve_pending_session_context(&txn, &scope, pk)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let identity =
            PendingSessionAlias::prepare(&conn, &context, None, &repo, root.path(), deadline)
                .await
                .unwrap();
        let alias = identity.alias().to_string();
        let checkpoint = Uuid::new_v4().to_string();
        let binding = PendingBinding {
            scope: scope.clone(),
            session_id: alias.clone(),
            checkpoint_id: checkpoint.clone(),
            event_id: Uuid::new_v4().to_string(),
            action_key: "ownership-only-erasure-fixture".into(),
            receipt_key: "evicted-receipt".into(),
            marker_generation: "expired-marker".into(),
            source_commitment: format!("source/hmac-v2/{}", "a".repeat(64)),
            reserved_revision: 1,
            original_deadline_millis: None,
            deferrable: true,
            first_attempt_millis: 1,
            parent_commit: None,
            parent_unborn: true,
        };
        let body = br#"{"ownership_only_redacted_evidence":"safe erasure fixture"}"#;
        let mac = scope
            .sign_pending_envelope_until(&conn, &repo, root.path(), body, deadline)
            .await
            .unwrap();
        let header = format!(
            "{{\"version\":1,\"binding\":{},\"mac\":\"{}\",\"envelope_bytes\":{},\"chunks\":1,\"manual_attempted\":false}}",
            serde_json::to_string(&binding).unwrap(),
            mac,
            body.len(),
        );
        assert!(PendingHeader::decode(&header).is_ok());
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        MetadataKv::set_with_conn(
            &txn,
            MetadataScope::AgentCapturePending,
            &scope.repo_id,
            &checkpoint,
            &header,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        MetadataKv::set_with_conn(
            &txn,
            MetadataScope::AgentCapturePendingChunk,
            &scope.repo_id,
            &format!("{checkpoint}:000"),
            &STANDARD.encode(body),
            MetadataValueType::Binary,
        )
        .await
        .unwrap();
        identity
            .publish_for_artifact(&txn, &checkpoint)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let corrupt_key = Uuid::new_v4().to_string();
        let corrupt = "canary-unassigned-header";
        MetadataKv::set_with_conn(
            &conn,
            MetadataScope::AgentCaptureQuarantine,
            &scope.repo_id,
            &corrupt_key,
            corrupt,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let key_path = repo
            .join(key::CAPTURE_DEDUP_SECRET_DIR)
            .join(key::CAPTURE_DEDUP_SECRET_FILE);
        std::fs::remove_file(&key_path).unwrap();

        let storage = Arc::new(crate::utils::storage::local::LocalStorage::new(
            repo.join("objects"),
        ));
        let history = HistoryManager::for_traces(storage, repo.clone(), Arc::new(conn.clone()));
        use tracing::instrument::WithSubscriber;
        #[derive(Clone, Default)]
        struct WarnSink(Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for WarnSink {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(bytes);
                Ok(bytes.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for WarnSink {
            type Writer = Self;
            fn make_writer(&'a self) -> Self::Writer {
                self.clone()
            }
        }
        let warnings = WarnSink::default();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .with_writer(warnings.clone())
            .finish();
        let outcome = history
            .erase_session_local(pk)
            .with_subscriber(subscriber)
            .await
            .unwrap();
        let warning = String::from_utf8(warnings.0.lock().unwrap().clone()).unwrap();
        assert!(warning.contains("unattributable private capture recovery evidence was retained and still consumes capacity"));
        for private in [corrupt, pk, alias.as_str(), root.path().to_str().unwrap()] {
            assert!(
                !warning.contains(private),
                "lost-capacity warning must be content-free"
            );
        }
        assert!(outcome.session_deleted);
        assert_eq!(outcome.removed_checkpoints, 0);
        assert!(
            !key_path.exists(),
            "keyless erasure must not recreate a key"
        );
        assert!(
            pending_identity::lookup(&conn, &scope.repo_id, &alias)
                .await
                .unwrap()
                .is_none()
        );
        for (scope_kind, key) in [
            (MetadataScope::AgentCapturePending, checkpoint.clone()),
            (
                MetadataScope::AgentCapturePendingChunk,
                format!("{checkpoint}:000"),
            ),
        ] {
            assert!(
                MetadataKv::get_with_conn(&conn, scope_kind, &scope.repo_id, &key)
                    .await
                    .unwrap()
                    .is_none()
            );
        }
        assert_eq!(
            MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentCaptureQuarantine,
                &scope.repo_id,
                &corrupt_key
            )
            .await
            .unwrap()
            .unwrap()
            .value,
            corrupt
        );
        assert!(
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_import_tombstone WHERE erased_session_id = ?",
                [pk.into()],
            ))
            .await
            .unwrap()
            .is_some(),
            "the first-phase anti-resurrection tombstone must remain"
        );

        use crate::internal::ai::{
            agent_import::restore_tombstone,
            capture::state::{LifecycleReducerInput, reduce_lifecycle},
            hooks::LifecycleEventKind,
            observed_agents::AgentKind,
        };
        let catalog = CaptureCatalogStore::new(conn.clone());
        let recapture_event = Uuid::new_v4();
        let reduction = reduce_lifecycle(LifecycleReducerInput {
            current: None,
            event_kind: LifecycleEventKind::SessionStart,
            event_id: recapture_event,
            occurred_at: 2,
            deadline: None,
        })
        .unwrap();
        let recapture = CaptureCatalogApplyRequest::new(
            scope.clone(),
            CaptureCatalogSession::new(
                pk,
                "claude_code",
                "history-erasure-native",
                root.path().to_string_lossy(),
            )
            .unwrap(),
            CaptureCatalogAction::lifecycle(recapture_event, None),
            CaptureCatalogMutation::from_reducer(None, &reduction, 2).unwrap(),
        )
        .unwrap();
        assert_eq!(
            catalog.apply(&recapture).await.unwrap_err(),
            CaptureCatalogError::Tombstoned
        );
        assert!(
            !key_path.exists(),
            "rejected stale capture must not regenerate the key"
        );

        // Use the existing audited restore protocol, never a raw tombstone
        // deletion. Only this explicit restore permits a fresh incarnation.
        assert!(
            restore_tombstone(&conn, AgentKind::ClaudeCode, "history-erasure-native")
                .await
                .unwrap()
        );
        catalog.apply(&recapture).await.unwrap();
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let fresh_context = resolve_pending_session_context(&txn, &scope, pk)
            .await
            .unwrap();
        assert!(fresh_context.incarnation().is_some());
        assert!(
            pending_identity::retained_alias(&txn, &fresh_context)
                .await
                .unwrap()
                .is_none()
        );
        let alias_count = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT COUNT(*) AS count FROM metadata_kv
             WHERE scope = 'agent_capture_session_alias' AND target = ?",
                [scope.repo_id.clone().into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by::<i64, _>("count")
            .unwrap();
        assert_eq!(
            alias_count, 0,
            "there must be no retained association to reuse after erasure"
        );
        let revision = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT sync_revision FROM agent_session WHERE session_id = ?",
                [pk.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by::<i64, _>("sync_revision")
            .unwrap();
        assert!(revision > 1);
        txn.commit().await.unwrap();
        // Key creation is authorized for fresh capture, not for replay or
        // erasure. The deleted association must never be reused.
        key::load_capture_dedup_secret(&repo).unwrap();
        let fresh =
            PendingSessionAlias::prepare(&conn, &fresh_context, None, &repo, root.path(), deadline)
                .await
                .unwrap();
        assert_ne!(fresh.alias(), alias);
        assert!(
            pending_identity::lookup(&conn, &scope.repo_id, &alias)
                .await
                .unwrap()
                .is_none()
        );
        let audit = conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS count FROM agent_audit_log WHERE action = 'restore_erased_import'".to_owned(),
        )).await.unwrap().unwrap().try_get_by::<i64, _>("count").unwrap();
        assert_eq!(audit, 1, "recapture must retain the existing restore audit");
    }

    /// Shape-valid local association is deletion ownership only, not replay
    /// authority. Keyless erasure must work on every platform.
    #[tokio::test]
    async fn pending_artifact_keyless_erase_is_cross_platform() {
        use base64::{Engine, engine::general_purpose::STANDARD};
        use uuid::Uuid;

        use crate::internal::{
            ai::{
                capture::{
                    pending::{PendingBinding, PendingHeader},
                    pending_identity,
                },
                capture_scope::CaptureScope,
            },
            config::ConfigKv,
            metadata::{MetadataKv, MetadataScope, MetadataValueType},
        };
        let root = tempdir().unwrap();
        let repo = root.path().join(".libra");
        std::fs::create_dir_all(repo.join("objects")).unwrap();
        let conn = crate::internal::db::create_database(repo.join("libra.db").to_str().unwrap())
            .await
            .unwrap();
        ConfigKv::set_with_conn(&conn, "libra.repoid", "cross-platform-erase", false)
            .await
            .unwrap();
        let pk = "claude__keyless-platform-session";
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
             state, working_dir, metadata_json, started_at, last_event_at, sync_revision,
             repo_id, worktree_id, scope_state)
             VALUES (?, 'claude_code', 'keyless-platform-session', 'active', ?, '{}',
             1, 1, 1, 'cross-platform-erase', '', 'scoped')",
            [pk.into(), root.path().to_string_lossy().into_owned().into()],
        ))
        .await
        .unwrap();
        let scope = CaptureScope::resolve(&conn, root.path()).await.unwrap();
        let alias = Uuid::new_v4().to_string();
        let checkpoint = Uuid::new_v4().to_string();
        let association = format!(
            "{{\"body\":{{\"version\":1,\"alias\":{},\"session_id\":{},\"repo_id\":{},\"worktree_id\":\"\",\"workspace_id\":null,\"capture_incarnation\":null}},\"mac\":\"pending-alias/hmac-v1/{}\"}}",
            serde_json::to_string(&alias).unwrap(),
            serde_json::to_string(pk).unwrap(),
            serde_json::to_string(&scope.repo_id).unwrap(),
            "a".repeat(64),
        );
        MetadataKv::set_with_conn(
            &conn,
            MetadataScope::AgentCaptureSessionAlias,
            &scope.repo_id,
            &alias,
            &association,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(
            pending_identity::lookup(&conn, &scope.repo_id, &alias)
                .await
                .unwrap()
                .is_some()
        );
        let binding = PendingBinding {
            scope: scope.clone(),
            session_id: alias.clone(),
            checkpoint_id: checkpoint.clone(),
            event_id: Uuid::new_v4().to_string(),
            action_key: "keyless-erase-action".into(),
            receipt_key: "evicted-receipt".into(),
            marker_generation: "expired-marker".into(),
            source_commitment: format!("source/hmac-v2/{}", "b".repeat(64)),
            reserved_revision: 1,
            original_deadline_millis: None,
            deferrable: true,
            first_attempt_millis: 1,
            parent_commit: None,
            parent_unborn: true,
        };
        let header = format!(
            "{{\"version\":1,\"binding\":{},\"mac\":\"pending-envelope/hmac-v1/{}\",\"envelope_bytes\":1,\"chunks\":1,\"manual_attempted\":false}}",
            serde_json::to_string(&binding).unwrap(),
            "c".repeat(64),
        );
        assert!(PendingHeader::decode(&header).is_ok());
        MetadataKv::set_with_conn(
            &conn,
            MetadataScope::AgentCapturePending,
            &scope.repo_id,
            &checkpoint,
            &header,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        MetadataKv::set_with_conn(
            &conn,
            MetadataScope::AgentCapturePendingChunk,
            &scope.repo_id,
            &format!("{checkpoint}:000"),
            &STANDARD.encode(b"a"),
            MetadataValueType::Binary,
        )
        .await
        .unwrap();
        let storage: Arc<dyn Storage + Send + Sync> =
            Arc::new(LocalStorage::new(repo.join("objects")));
        let history = HistoryManager::for_traces(storage, repo.clone(), Arc::new(conn.clone()));
        let repository_entries = || {
            std::fs::read_dir(&repo)
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .collect::<HashSet<_>>()
        };
        let keyless_layout = repository_entries();
        assert!(
            history
                .erase_session_local(pk)
                .await
                .unwrap()
                .session_deleted
        );
        let private_count = conn.query_one_raw(Statement::from_sql_and_values(conn.get_database_backend(),
            "SELECT COUNT(*) AS count FROM metadata_kv WHERE target = ? AND scope IN
             ('agent_capture_pending', 'agent_capture_quarantine', 'agent_capture_pending_chunk', 'agent_capture_session_alias')",
            [scope.repo_id.clone().into()],
        )).await.unwrap().unwrap().try_get_by::<i64, _>("count").unwrap();
        assert_eq!(private_count, 0);
        assert_eq!(
            repository_entries(),
            keyless_layout,
            "keyless erasure must not create new top-level private key storage"
        );
    }

    #[test]
    #[serial_test::serial(cwd, env)]
    fn checkpoint_durability_aggregate_object_bound_is_fail_closed() {
        let mut durable = HashSet::new();
        insert_durable_oid_bounded(&mut durable, "one".to_string(), 2).expect("first object");
        insert_durable_oid_bounded(&mut durable, "two".to_string(), 2).expect("second object");
        insert_durable_oid_bounded(&mut durable, "two".to_string(), 2)
            .expect("duplicate does not consume the bound");
        let error = insert_durable_oid_bounded(&mut durable, "three".to_string(), 2)
            .expect_err("a distinct object beyond the aggregate bound must fail");
        assert!(error.to_string().contains("aggregate object limit"));
        assert_eq!(durable.len(), 2);
    }

    #[test]
    fn subagent_content_reservation_error_display_is_stable_and_actionable() {
        let error = SubagentContentReservationPruneGuard {
            session_id: "session-7".to_string(),
            attempt_id: "checkpoint-9".to_string(),
            lease_expires_at: 1_700_000_123_456,
        };
        assert_eq!(
            error.to_string(),
            "refusing to prune traces checkpoints: a subagent content write is reserved \
             (session 'session-7', attempt 'checkpoint-9', lease expires at \
             1700000123456); retry once the writer finishes or the lease expires"
        );
    }

    #[test]
    fn checkpoint_object_helper_rejects_compression_bomb_before_unbounded_inflate() {
        let repo = tempdir().unwrap();
        let oversized = vec![0_u8; CHECKPOINT_OBJECT_READ_MAX_INFLATED_BYTES as usize + 1];
        let (oid, _) = write_git_object_with_status(repo.path(), "tree", &oversized).unwrap();
        let request = CheckpointObjectIoHelperRequest {
            repo_path_base64: encode_checkpoint_object_path(repo.path()).unwrap(),
            operation: CheckpointObjectIoOperation::Read {
                oid: oid.to_string(),
                expected_type: "tree".to_string(),
            },
        };
        let frame = serde_json::to_vec(&request).unwrap();
        let response: CheckpointObjectIoHelperResponse =
            serde_json::from_slice(&run_checkpoint_object_io_helper(&frame).unwrap()).unwrap();
        let CheckpointObjectIoHelperResponse::Error { code } = response else {
            panic!("oversized compressed object was accepted")
        };
        assert_eq!(
            code,
            CheckpointObjectIoHelperError::ReadFailed,
            "the private helper must return a fixed, typed read failure"
        );
        assert_eq!(
            code.user_message(),
            "checkpoint object-I/O helper could not read the object",
            "the parent-visible helper failure must remain content-free and stable"
        );
    }

    #[test]
    fn checkpoint_object_helper_rejects_unknown_input_without_echoing_raw_content() {
        let raw_marker = "raw-checkpoint-helper-secret-must-not-cross";
        let request = serde_json::json!({
            "repo_path_base64": "L3RtcA==",
            "operation": {
                "kind": "write",
                "object_type": "blob",
                "data_base64": "",
                "unexpected_raw_payload": raw_marker,
            },
        });
        let wire = run_checkpoint_object_io_helper(
            serde_json::to_string(&request)
                .expect("serialize malformed helper request")
                .as_bytes(),
        )
        .expect("helper must encode a fixed invalid-request response");
        let response: CheckpointObjectIoHelperResponse =
            serde_json::from_slice(&wire).expect("decode fixed helper response");
        assert!(matches!(
            response,
            CheckpointObjectIoHelperResponse::Error {
                code: CheckpointObjectIoHelperError::InvalidRequest,
            }
        ));
        assert!(
            !String::from_utf8(wire)
                .expect("helper response is JSON")
                .contains(raw_marker),
            "unknown request fields must be rejected and never echoed into the response"
        );
    }

    #[tokio::test]
    async fn checkpoint_object_helper_embedded_host_fails_closed_without_spawn() {
        let result = crate::internal::ai::authorized_read::with_no_test_helper_program(async {
            invoke_checkpoint_object_helper(
                Path::new("/not-a-real-checkpoint-store"),
                CheckpointObjectIoOperation::Write {
                    object_type: "blob".to_string(),
                    data_base64: String::new(),
                },
                Instant::now() + Duration::from_secs(1),
            )
            .await
        })
        .await;
        let error = result.expect_err("an embedded host must not spawn its current executable");
        #[cfg(unix)]
        assert_eq!(
            error.to_string(),
            "checkpoint object-I/O helper is unavailable in this host"
        );
        #[cfg(not(unix))]
        assert_eq!(
            error.to_string(),
            "checkpoint object-I/O helper is unavailable on this platform"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_index_helper_embedded_host_fails_closed_without_current_exe_fallback()
    {
        let dir = tempdir().expect("create embedded cleanup-helper fixture");
        let db_conn = Arc::new(setup_test_db().await);
        let manager = traces_manager(&dir, db_conn);

        let result = crate::internal::ai::authorized_read::with_no_test_helper_program(async {
            manager.rejected_cleanup_index_snapshot(Instant::now() + Duration::from_secs(1))
        })
        .await;

        let error = result.expect_err("embedded hosts must not run current_exe as a helper");
        assert_eq!(
            error.to_string(),
            "rejected-cleanup index helper is unavailable in this host"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_object_helper_drains_stdout_before_writing_stdin() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("create checkpoint helper tempdir");
        let program = dir.path().join("checkpoint-helper-output-first");
        std::fs::write(
            &program,
            "#!/bin/sh\ndd if=/dev/zero bs=65536 count=4 2>/dev/null\ncat >/dev/null\n",
        )
        .expect("write output-first checkpoint helper");
        let mut permissions = std::fs::metadata(&program)
            .expect("inspect output-first checkpoint helper")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions)
            .expect("make output-first checkpoint helper executable");

        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let oversized_request = STANDARD.encode(vec![b'x'; 256 * 1024]);
        let result = crate::internal::ai::authorized_read::with_test_helper_program(
            program,
            tokio::time::timeout(
                Duration::from_secs(2),
                invoke_checkpoint_object_helper(
                    dir.path(),
                    CheckpointObjectIoOperation::Write {
                        object_type: "blob".to_string(),
                        data_base64: oversized_request,
                    },
                    Instant::now() + Duration::from_secs(10),
                ),
            ),
        )
        .await;
        assert!(
            result.is_ok(),
            "the response drain must start before a large request write to avoid pipe deadlock"
        );
        assert!(
            result
                .expect("helper must finish without a pipe deadlock")
                .is_err(),
            "the output-first fixture intentionally has no valid helper response"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_object_helper_outer_cancellation_kills_descendant_holding_stdout() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("create checkpoint helper tempdir");
        let program = dir.path().join("checkpoint-helper-forks-stdout-holder");
        let descendant_file = dir.path().join("checkpoint-helper-descendant.pid");
        let descendant_path = descendant_file.to_string_lossy().replace('\'', "'\"'\"'");
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncat >/dev/null\n/bin/sleep 30 &\nprintf '%s\\n' \"$!\" > '{descendant_path}'\nexit 0\n"
            ),
        )
        .expect("write forking checkpoint helper");
        let mut permissions = std::fs::metadata(&program)
            .expect("inspect forking checkpoint helper")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions)
            .expect("make forking checkpoint helper executable");

        // Cancel only after the descendant has published its pid: a fixed
        // outer timeout could fire before the helper forks under full-suite
        // load, which would make the reaping assertion vacuous or flaky.
        let mut invocation = Box::pin(
            crate::internal::ai::authorized_read::with_test_helper_program(
                program,
                invoke_checkpoint_object_helper(
                    dir.path(),
                    CheckpointObjectIoOperation::Write {
                        object_type: "blob".to_string(),
                        data_base64: String::new(),
                    },
                    Instant::now() + Duration::from_secs(30),
                ),
            ),
        );
        let publish_deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        let descendant = loop {
            tokio::select! {
                result = &mut invocation => panic!(
                    "descendant-held stdout must keep the helper in flight until outer cancellation (ok={})",
                    result.is_ok()
                ),
                _ = sleep(Duration::from_millis(10)) => {}
            }
            if let Some(pid) = std::fs::read_to_string(&descendant_file)
                .ok()
                .and_then(|value| value.trim().parse::<libc::pid_t>().ok())
            {
                break pid;
            }
            assert!(
                tokio::time::Instant::now() < publish_deadline,
                "forked helper descendant did not report its pid"
            );
        };
        // Outer cancellation: drop the in-flight invocation future.
        drop(invocation);
        let reap_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            // SAFETY: `descendant` was reported by the helper process started by this test.
            let alive = unsafe { libc::kill(descendant, 0) } == 0
                || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
            if !alive {
                break;
            }
            assert!(
                tokio::time::Instant::now() < reap_deadline,
                "outer cancellation left checkpoint helper descendant {descendant} alive"
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checkpoint_object_helper_invalid_response_does_not_leak_raw_content() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempdir().expect("create checkpoint helper tempdir");
        let program = dir.path().join("checkpoint-helper-invalid-response");
        let raw_marker = "raw-checkpoint-helper-response-secret";
        std::fs::write(
            &program,
            format!(
                "#!/bin/sh\ncat >/dev/null\nprintf '%s' '{{\"status\":\"error\",\"code\":\"read_failed\",\"unexpected\":\"{raw_marker}\"}}'\n"
            ),
        )
        .expect("write invalid-response checkpoint helper");
        let mut permissions = std::fs::metadata(&program)
            .expect("inspect invalid-response checkpoint helper")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&program, permissions)
            .expect("make invalid-response checkpoint helper executable");

        let error = crate::internal::ai::authorized_read::with_test_helper_program(
            program,
            invoke_checkpoint_object_helper(
                dir.path(),
                CheckpointObjectIoOperation::Write {
                    object_type: "blob".to_string(),
                    data_base64: String::new(),
                },
                Instant::now() + Duration::from_secs(2),
            ),
        )
        .await
        .expect_err("unknown response fields must be rejected");
        assert_eq!(
            error.to_string(),
            "checkpoint object-I/O helper returned an invalid response"
        );
        assert!(
            !format!("{error:#}").contains(raw_marker),
            "raw helper output must not be included in parent errors"
        );
    }

    /// plan-20260713 DR-05c-0: the shared rebuild boundary classifies both
    /// auto-rebuildable scopes and fails closed on anything else.
    #[test]
    fn rebuilt_catalog_row_committed_and_subagent() {
        let base = RebuildCatalogRowInputs {
            scope: "committed".to_string(),
            checkpoint_id: "cp1".to_string(),
            session_id: "s1".to_string(),
            parent_commit: Some("p".to_string()),
            tree_oid: "t".to_string(),
            metadata_blob_oid: "m".to_string(),
            traces_commit: "c".to_string(),
            created_at: 7,
            ..Default::default()
        };
        match rebuild_catalog_row_from_traces_ref(base.clone()).expect("committed rebuilds") {
            RebuiltCatalogRow::Committed {
                checkpoint_id,
                session_id,
                created_at,
                ..
            } => {
                assert_eq!(checkpoint_id, "cp1");
                assert_eq!(session_id, "s1");
                assert_eq!(created_at, 7);
            }
            other => panic!("expected Committed, got {other:?}"),
        }

        let sub = RebuildCatalogRowInputs {
            scope: "subagent".to_string(),
            parent_checkpoint_id: Some("parent-cp".to_string()),
            tool_use_id: Some("tool-1".to_string()),
            ..base.clone()
        };
        match rebuild_catalog_row_from_traces_ref(sub).expect("subagent rebuilds") {
            RebuiltCatalogRow::Subagent {
                parent_checkpoint_id,
                tool_use_id,
                ..
            } => {
                assert_eq!(parent_checkpoint_id.as_deref(), Some("parent-cp"));
                assert_eq!(tool_use_id.as_deref(), Some("tool-1"));
            }
            other => panic!("expected Subagent, got {other:?}"),
        }

        // Fail-closed: unknown scopes are an error, never a guessed shape.
        let weird = RebuildCatalogRowInputs {
            scope: "temporary".to_string(),
            ..base
        };
        assert!(rebuild_catalog_row_from_traces_ref(weird).is_err());
    }

    use super::*;

    async fn drain_rejected_cleanup_in_invocation_scope(manager: &HistoryManager) -> Result<()> {
        // Production cleanup runs under the CLI invocation-local object-index
        // scope. Direct unit-test calls must model that boundary and opt into
        // the in-process index snapshot seam: the libtest executable has no
        // Libra `main` helper entrypoint.
        TEST_DIRECT_REJECTED_CLEANUP_INDEX_SNAPSHOT
            .scope((), async {
                crate::utils::client_storage::ClientStorage::with_background_index_failure_scope(
                    manager.drain_rejected_checkpoint_cleanup_jobs(),
                )
                .await
            })
            .await
    }

    async fn repair_expired_marker_in_invocation_scope(
        manager: &HistoryManager,
        session_id: &str,
        attempt_id: &str,
        now_ms: i64,
    ) -> Result<bool> {
        TEST_DIRECT_REJECTED_CLEANUP_INDEX_SNAPSHOT
            .scope((), async {
                crate::utils::client_storage::ClientStorage::with_background_index_failure_scope(
                    manager.repair_expired_traces_inflight_marker(session_id, attempt_id, now_ms),
                )
                .await
            })
            .await
    }

    /// W2 §C.4.3 end-to-end unblock: an `i64::MAX`-dated marker row blocks
    /// the listing fail-closed, and `agent doctor --repair`'s retirement
    /// entry point RETIRES it (the drain classifies untrustworthy rows as
    /// retirable), after which the listing succeeds again.
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn doctor_repair_retires_future_dated_marker_and_unblocks_listing() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let db = crate::internal::db::get_db_conn_instance().await;
        let now = chrono::Utc::now().timestamp_millis();
        let mut marker = TracesInflightMarker::new("sess-max", "attempt-max", now);
        marker.started_at_ms = i64::MAX;
        crate::internal::metadata::MetadataKv::set_with_conn(
            &db,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            "sess-max",
            "attempt-max",
            &serde_json::to_string(&marker).expect("encode"),
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("seed i64::MAX marker");

        list_live_traces_inflight_markers(&db, now)
            .await
            .expect_err("the corrupt row blocks the listing");

        let storage: Arc<dyn crate::utils::storage::Storage + Send + Sync> =
            Arc::new(crate::utils::storage::local::LocalStorage::new(
                crate::utils::util::storage_path().join("objects"),
            ));
        let history = HistoryManager::new_with_ref(
            storage,
            crate::utils::util::storage_path(),
            Arc::new(db.clone()),
            "refs/libra/traces",
        );
        let retired =
            repair_expired_marker_in_invocation_scope(&history, "sess-max", "attempt-max", now)
                .await
                .expect("doctor repair retires the untrustworthy marker");
        assert!(retired, "the marker row was fully retired");

        let live = list_live_traces_inflight_markers(&db, now)
            .await
            .expect("the listing is unblocked after retirement");
        assert!(live.is_empty(), "no marker remains: {live:?}");
    }

    /// W2 §C.4.3: the LISTING fails closed on a future-dated marker row —
    /// every destructive consumer (gc defer/roots, prune, erasure) stops
    /// instead of silently losing or trusting the row.
    #[tokio::test]
    #[serial_test::serial(cwd, env)]
    async fn listing_fails_closed_on_future_dated_marker_row() {
        let tmp = tempfile::tempdir().expect("tmp");
        let _guard = crate::utils::test::ChangeDirGuard::new(tmp.path());
        crate::utils::test::setup_with_new_libra_in(tmp.path()).await;
        let db = crate::internal::db::get_db_conn_instance().await;
        let now = chrono::Utc::now().timestamp_millis();
        let marker = TracesInflightMarker::new("sess-f", "attempt-f", now + 48 * 60 * 60 * 1000);
        crate::internal::metadata::MetadataKv::set_with_conn(
            &db,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            "sess-f",
            "attempt-f",
            &serde_json::to_string(&marker).expect("encode"),
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("seed future-dated marker");

        let err = list_live_traces_inflight_markers(&db, now)
            .await
            .expect_err("future-dated row must fail the listing closed");
        assert!(
            format!("{err:#}").contains("implausible timestamp"),
            "actionable corruption error: {err:#}"
        );
    }

    /// W2 §C.4.3: marker liveness is a DETERMINISTIC absolute deadline from
    /// the persisted fields (TTL capped at 24h, no per-read re-anchoring),
    /// and a future-dated start beyond skew tolerance marks the ROW ITSELF
    /// untrustworthy — the listing fails closed on it instead of silently
    /// filtering or trusting it.
    #[test]
    fn inflight_marker_liveness_is_bounded_and_deterministic() {
        let now = 1_700_000_000_000_i64;
        let mut marker = TracesInflightMarker::new("s", "a", now - 60_000);
        // Normal recent marker with its ordinary TTL: live and trustworthy.
        assert!(marker.time_fields_trustworthy(now));
        assert!(marker.is_live(now));
        // Absurd TTL is capped: dead 24h past start no matter the claim.
        marker.ttl_ms = i64::MAX;
        assert!(marker.is_live(now));
        assert!(!marker.is_live(marker.started_at_ms + TRACES_INFLIGHT_MAX_LIVE_MS + 1));
        // Future-dated start beyond skew: UNTRUSTWORTHY at any read time
        // before the claimed start — the listing bails rather than judging
        // liveness; once real time catches up the row is trustworthy again
        // and the ordinary capped deadline applies.
        marker.started_at_ms = now + 48 * 60 * 60 * 1000;
        marker.ttl_ms = 600_000;
        assert!(!marker.time_fields_trustworthy(now));
        assert!(!marker.time_fields_trustworthy(now + 60 * 60 * 1000));
        assert!(marker.time_fields_trustworthy(marker.started_at_ms));
        assert!(marker.is_live(marker.started_at_ms + 1));
        assert!(!marker.is_live(marker.started_at_ms + 600_001));
        // Small clock skew is tolerated deterministically.
        marker.started_at_ms = now + 60_000;
        assert!(marker.time_fields_trustworthy(now));
        assert!(marker.is_live(now));
        assert!(!marker.is_live(now + TRACES_INFLIGHT_MAX_LIVE_MS + 120_000));
    }
    use crate::{internal::db, utils::storage::local::LocalStorage};

    #[cfg(unix)]
    #[test]
    fn cleanup_index_snapshot_rejects_fifo_and_symlink_inputs_without_blocking() {
        use std::{ffi::CString, os::unix::ffi::OsStrExt as _};

        let dir = tempdir().unwrap();
        let fifo = dir.path().join("index-fifo");
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).unwrap();
        // SAFETY: fifo_name is NUL-terminated inside this test's tempdir.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);
        let started = Instant::now();
        let error = read_cleanup_regular_file(&fifo, 1024, 1024, "test cleanup index")
            .expect_err("FIFO index must fail closed");
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(format!("{error:#}").contains("not a regular file"));

        let target = dir.path().join("real-index");
        std::fs::write(&target, b"index").unwrap();
        let symlink = dir.path().join("index-symlink");
        std::os::unix::fs::symlink(&target, &symlink).unwrap();
        let error = read_cleanup_regular_file(&symlink, 1024, 1024, "test cleanup index")
            .expect_err("symlink index must fail closed");
        assert!(format!("{error:#}").contains("without following symlinks"));
    }

    #[test]
    fn cleanup_index_snapshot_rejects_growth_after_held_descriptor_metadata() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("growing-index");
        std::fs::write(&path, b"1234").unwrap();
        let path_for_hook = path.clone();
        let error =
            read_cleanup_regular_file_inner(&path, 4, 4, "test growing cleanup index", move || {
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(path_for_hook)
                    .unwrap()
                    .write_all(b"5")
                    .unwrap();
            })
            .expect_err("post-metadata growth must fail closed");
        assert!(format!("{error:#}").contains("grew beyond"));
    }

    #[test]
    #[ignore = "helper process invoked only by cleanup_helper_guard_returns_promptly_and_reaps_repeated_timeouts"]
    fn cleanup_helper_child_sleeper_process() {
        std::thread::sleep(Duration::from_secs(10));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cleanup_helper_guard_returns_promptly_and_reaps_repeated_timeouts() {
        use std::sync::atomic::Ordering;

        let executable = std::env::current_exe().expect("resolve test executable");
        let reaper = cleanup_helper_reaper_sender().expect("start cleanup child reaper");
        let reaped_before = CLEANUP_HELPER_REAPED_CHILDREN.load(Ordering::SeqCst);
        let mut pids = Vec::new();
        for _ in 0..3 {
            let child = Command::new(&executable)
                .arg("--ignored")
                .arg("--exact")
                // libtest's exact test name omits the crate name.
                .arg("internal::ai::history::tests::cleanup_helper_child_sleeper_process")
                .arg("--nocapture")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("start sleeper child");
            pids.push(child.id());
            let guard = CleanupHelperChild::new(child, reaper.clone());
            std::thread::sleep(Duration::from_millis(75));
            let started = Instant::now();
            drop(guard);
            assert!(
                started.elapsed() < Duration::from_millis(250),
                "timeout cleanup blocked while waiting for a killed helper"
            );
        }

        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            let all_reaped = pids
                .iter()
                .all(|pid| !Path::new(&format!("/proc/{pid}")).exists());
            let observed = CLEANUP_HELPER_REAPED_CHILDREN.load(Ordering::SeqCst);
            if all_reaped && observed >= reaped_before + pids.len() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "killed cleanup helpers were not reaped before the test deadline"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    async fn setup_test_db() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        let builder = db.get_database_backend();
        let schema = Schema::new(builder);
        let stmt = schema.create_table_from_entity(reference::Entity);
        db.execute_raw(builder.build(&stmt)).await.unwrap();
        // Present in every real repository (bootstrap schema). The write-lock
        // primitive in `db::begin_write_transaction` issues a no-op write
        // against it, so a fixture without it is not a repository database.
        db.execute_raw(Statement::from_string(
            builder,
            "CREATE TABLE config_kv(id INTEGER PRIMARY KEY AUTOINCREMENT,key TEXT NOT NULL,\
             value TEXT NOT NULL,encrypted INTEGER NOT NULL DEFAULT 0)"
                .to_string(),
        ))
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn test_history_append_simple() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        // 1. Append first object
        let blob_hash = crate::internal::ai::util::parse_repo_object_id(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .unwrap();
        manager.append("task", "task-1", blob_hash).await.unwrap();

        // Verify ref exists in DB
        let ref_model = reference::Entity::find()
            .filter(reference::Column::Name.eq(AI_REF))
            .filter(reference::Column::Kind.eq(ConfigKind::Branch))
            .one(&*db_conn)
            .await
            .unwrap()
            .expect("Reference should exist");

        let commit_hash_str = ref_model.commit.expect("Commit hash should exist");
        let commit_hash =
            crate::internal::ai::util::parse_repo_object_id(&commit_hash_str).unwrap();

        // Verify we can load commit
        let data = read_git_object(&repo_path, &commit_hash).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(content.contains("tree "));
        assert!(content.contains("Update task/task-1"));

        // 2. Append second object (same type)
        let blob_hash_2 = crate::internal::ai::util::parse_repo_object_id(
            "f4e6d0434b8b29ae775ad8c2e48c5391e69de29b",
        )
        .unwrap();
        manager.append("task", "task-2", blob_hash_2).await.unwrap();

        // 3. Append third object (different type)
        manager.append("run", "run-1", blob_hash).await.unwrap();

        // Load Head Commit from DB
        let head = manager.resolve_history_head().await.unwrap().unwrap();

        // Verify we can load commit
        let data = read_git_object(&repo_path, &head).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(content.contains("tree "));
        assert!(content.contains("Update run/run-1"));
    }

    #[tokio::test]
    async fn test_find_object_hashes_returns_all_matching_types() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        let blob_hash = crate::internal::ai::util::parse_repo_object_id(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .unwrap();
        let other_hash = crate::internal::ai::util::parse_repo_object_id(
            "f4e6d0434b8b29ae775ad8c2e48c5391e69de29b",
        )
        .unwrap();

        manager
            .append("patchset", "shared-id", blob_hash)
            .await
            .unwrap();
        manager
            .append("event", "shared-id", other_hash)
            .await
            .unwrap();

        let matches = manager.find_object_hashes("shared-id").await.unwrap();
        assert_eq!(matches.len(), 2);
        assert!(matches.iter().any(|(_, kind)| kind == "patchset"));
        assert!(matches.iter().any(|(_, kind)| kind == "event"));
    }

    #[tokio::test]
    async fn test_list_object_types_returns_sorted_types() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage.clone(), repo_path.clone(), db_conn.clone());

        let blob_hash = crate::internal::ai::util::parse_repo_object_id(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .unwrap();
        manager
            .append("run_event", "run-event-1", blob_hash)
            .await
            .unwrap();
        manager
            .append("patchset", "patchset-1", blob_hash)
            .await
            .unwrap();

        let types = manager.list_object_types().await.unwrap();
        assert_eq!(types, vec!["patchset".to_string(), "run_event".to_string()]);
    }

    #[tokio::test]
    async fn test_update_ref_retries_when_sqlite_is_locked() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");
        std::fs::create_dir(&objects_dir).unwrap();
        let db_path = repo_path.join("libra.db");

        let db_conn = Arc::new(
            db::create_database(db_path.to_str().unwrap())
                .await
                .expect("failed to create sqlite database"),
        );
        let storage = Arc::new(LocalStorage::new(objects_dir));
        let manager = HistoryManager::new(storage, repo_path.clone(), db_conn.clone());

        let locker = db::establish_connection_with_busy_timeout(
            db_path.to_str().unwrap(),
            Duration::from_millis(50),
        )
        .await
        .expect("failed to open lock holder connection");
        let backend = locker.get_database_backend();
        locker
            .execute_raw(Statement::from_string(backend, "BEGIN EXCLUSIVE"))
            .await
            .expect("failed to acquire sqlite exclusive lock");

        let release = {
            let locker = locker.clone();
            tokio::spawn(async move {
                sleep(Duration::from_millis(250)).await;
                let backend = locker.get_database_backend();
                locker
                    .execute_raw(Statement::from_string(backend, "COMMIT"))
                    .await
                    .expect("failed to release sqlite exclusive lock");
            })
        };

        let hash = crate::internal::ai::util::parse_repo_object_id(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .unwrap();
        manager
            .update_ref(AI_REF, hash)
            .await
            .expect("update_ref should retry through a transient sqlite lock");
        release.await.unwrap();

        let resolved = manager
            .resolve_history_head()
            .await
            .expect("history head should be readable after retry")
            .expect("history head should exist");
        assert_eq!(resolved, hash);
    }

    // -- plan-20260713 DR-05c-0 required M1 tests ---------------------------

    fn traces_manager(dir: &tempfile::TempDir, db_conn: Arc<DatabaseConnection>) -> HistoryManager {
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir_all(repo_path.join("objects")).unwrap();
        let storage = Arc::new(LocalStorage::new(repo_path.join("objects")));
        HistoryManager::new_with_ref(
            storage,
            repo_path,
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        )
    }

    fn checkpoint_params<'a>(
        checkpoint_id: &'a str,
        marker_generation: &'a str,
        blobs: &'a RedactedBytes,
        txn_extra: Option<&'a dyn TracesTxnExtra>,
    ) -> CheckpointCommitParams<'a> {
        CheckpointCommitParams {
            checkpoint_id,
            session_id: "claude_code__s1",
            marker_generation,
            capture_scope: None,
            agent_kind: "claude_code",
            parent_commit: None,
            scope: CheckpointScope::Committed,
            tool_use_id: None,
            metadata_json: blobs,
            transcript_redacted: blobs,
            lifecycle_events_jsonl: blobs,
            redaction_report_json: blobs,
            txn_extra,
            deadline: None,
        }
    }

    async fn prepare_checkpoint_test_schema(conn: &DatabaseConnection) {
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
        ))
        .await
        .expect("create checkpoint marker registry");
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "CREATE TABLE IF NOT EXISTS agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)"
                .to_string(),
        ))
        .await
        .expect("create checkpoint cleanup catalog probe");
    }

    async fn seed_test_writer_fence(
        conn: &DatabaseConnection,
        session_id: &str,
        attempt_id: &str,
    ) -> TracesWriterFence {
        let marker = TracesInflightMarker::new(
            session_id,
            attempt_id,
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(conn, &marker)
            .await
            .expect("seed writer marker generation");
        TracesWriterFence {
            session_id: session_id.to_string(),
            attempt_id: attempt_id.to_string(),
            generation: marker.generation.expect("new marker generation"),
        }
    }

    async fn deadline_contention_fixture() -> (
        tempfile::TempDir,
        PathBuf,
        Arc<DatabaseConnection>,
        HistoryManager,
        TracesWriterFence,
    ) {
        let dir = tempdir().expect("create deadline contention fixture");
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).expect("create deadline contention repository");
        let objects_dir = repo_path.join("objects");
        std::fs::create_dir(&objects_dir).expect("create deadline contention objects directory");
        let database_path = repo_path.join("libra.db");
        let db_conn = Arc::new(
            db::create_database(
                database_path
                    .to_str()
                    .expect("deadline contention database path is utf-8"),
            )
            .await
            .expect("create deadline contention database"),
        );
        let manager = HistoryManager::new_with_ref(
            Arc::new(LocalStorage::new(objects_dir)),
            repo_path,
            db_conn.clone(),
            crate::internal::branch::TRACES_BRANCH,
        );
        let fence = seed_test_writer_fence(
            &db_conn,
            "deadline-contention-session",
            "deadline-contention-attempt",
        )
        .await;
        (dir, database_path, db_conn, manager, fence)
    }

    fn deadline_for_contention_test() -> CaptureCommitDeadline {
        CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_millis(250),
            chrono::Utc::now().timestamp_millis() + 5_000,
        )
    }

    async fn append_test_checkpoint(
        manager: &HistoryManager,
        checkpoint_id: &str,
        blobs: &RedactedBytes,
        txn_extra: Option<&dyn TracesTxnExtra>,
    ) -> Result<CheckpointCommit> {
        let marker = TracesInflightMarker::new(
            "claude_code__s1",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(manager.db_conn.as_ref(), &marker).await?;
        let marker_generation = marker
            .generation
            .as_deref()
            .context("new test marker has no writer generation")?;
        let result = manager
            .append_checkpoint_commit(checkpoint_params(
                checkpoint_id,
                marker_generation,
                blobs,
                txn_extra,
            ))
            .await;
        if let Ok(written) = &result {
            clear_traces_inflight_marker_if_generation(
                manager.db_conn.as_ref(),
                "claude_code__s1",
                checkpoint_id,
                &written.marker_generation,
            )
            .await?;
        }
        result
    }

    async fn test_table_row_count(conn: &DatabaseConnection, table: &str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            format!("SELECT COUNT(*) AS n FROM {table}"),
        ))
        .await
        .expect("count test table rows")
        .expect("test table count row")
        .try_get_by("n")
        .expect("decode test table count")
    }

    /// ref_cas_head_changed_rebuilds_commit_before_retry: a competing commit
    /// lands BETWEEN the loop's head read and its CAS (deterministically, via
    /// the test-only injection hook) — the CAS must reject the stale attempt,
    /// the loop must RETRY (cas_retries > 0) and REBUILD the commit parented
    /// on the freshly-read head, keeping the chain linear.
    #[tokio::test]
    async fn ref_cas_head_changed_rebuilds_commit_before_retry() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let mut manager = traces_manager(&dir, db_conn.clone());
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());

        // Seed head H0.
        let seeded = append_test_checkpoint(
            &manager,
            "aaaa0000-0000-0000-0000-000000000001",
            &blobs,
            None,
        )
        .await
        .expect("seed checkpoint");
        let h0 = seeded.commit_hash;

        // Competing writer, fired from INSIDE the tested append's
        // read→CAS window (first attempt only) via the injection hook.
        let interloper = Arc::new(traces_manager(&dir, db_conn.clone()));
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let interloper_commit: Arc<std::sync::Mutex<Option<ObjectHash>>> =
            Arc::new(std::sync::Mutex::new(None));
        {
            let interloper = interloper.clone();
            let fired = fired.clone();
            let interloper_commit = interloper_commit.clone();
            manager.test_after_head_read = Some(Arc::new(move || {
                let interloper = interloper.clone();
                let fired = fired.clone();
                let interloper_commit = interloper_commit.clone();
                Box::pin(async move {
                    if fired.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        return Ok(()); // only the first attempt races
                    }
                    let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
                    let won = append_test_checkpoint(
                        &interloper,
                        "bbbb0000-0000-0000-0000-000000000002",
                        &blobs,
                        None,
                    )
                    .await?;
                    *interloper_commit.lock().unwrap() = Some(won.commit_hash);
                    Ok(())
                })
            }));
        }

        let rebuilt = append_test_checkpoint(
            &manager,
            "cccc0000-0000-0000-0000-000000000003",
            &blobs,
            None,
        )
        .await
        .expect("append survives the mid-window head move");
        let h1 = interloper_commit
            .lock()
            .unwrap()
            .expect("interloper committed");

        // A real retry happened …
        assert!(
            rebuilt.cas_retries > 0,
            "the first attempt must lose the CAS and retry, got cas_retries = {}",
            rebuilt.cas_retries
        );
        // … and the rebuilt commit parents the interloper's head, not H0.
        let data = read_git_object(&manager.repo_path, &rebuilt.commit_hash).unwrap();
        let content = String::from_utf8_lossy(&data);
        assert!(
            content.contains(&format!("parent {h1}")),
            "rebuilt commit must parent the NEW head {h1}, got:\n{content}"
        );
        assert!(
            !content.contains(&format!("parent {h0}")),
            "rebuilt commit must not still parent the stale head {h0}"
        );
        let head = manager.resolve_history_head().await.unwrap().unwrap();
        assert_eq!(head, rebuilt.commit_hash, "chain stays linear");
    }

    /// A scoped checkpoint must defer all cloud-visible index rows until its
    /// winning final CAS. When a competing writer moves the head, the first
    /// attempt's three spliced trees and commit are unreachable residue and
    /// must never be indexed or published through a repair marker.
    #[tokio::test]
    async fn scoped_checkpoint_retry_indexes_only_winning_attempt_objects() {
        let dir = tempdir().expect("create scoped retry fixture");
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir_all(&repo_path).expect("create scoped retry repository");
        let db_path = repo_path.join(crate::utils::util::DATABASE);
        let db_conn = Arc::new(
            crate::internal::db::create_database(&db_path.to_string_lossy())
                .await
                .expect("create scoped retry database"),
        );
        prepare_checkpoint_test_schema(&db_conn).await;

        let scope = CaptureScope {
            repo_id: "history-retry-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-retry-scope-workspace".to_string()),
            workspace_fence: Some(29),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                           'active', 'history-retry-test', ?, 9999999999999, 1, 1)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    repo_path.to_string_lossy().into_owned().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live scoped retry workspace");

        let checkpoint_id = "cccc0000-0000-0000-0000-000000000003";
        let marker = TracesInflightMarker::new(
            "history-retry-scope-session",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        let marker_generation = marker
            .generation
            .clone()
            .expect("new writer marker has a generation");
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed scoped retry writer marker");

        let mut manager = traces_manager(&dir, db_conn.clone());
        let interloper = Arc::new(traces_manager(&dir, db_conn.clone()));
        let head_moved = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let interloper = interloper.clone();
            let head_moved = head_moved.clone();
            manager.test_after_head_read = Some(Arc::new(move || {
                let interloper = interloper.clone();
                let head_moved = head_moved.clone();
                Box::pin(async move {
                    if head_moved.swap(true, std::sync::atomic::Ordering::SeqCst) {
                        return Ok(());
                    }

                    // Build a valid competing checkpoint/<cc>/... tree with
                    // raw object writes. It deliberately shares the target
                    // prefix but not its rest path, making every first-attempt
                    // splice tree differ from the retry without publishing
                    // unrelated object-index repair markers.
                    let blob = write_git_object(&interloper.repo_path, "blob", b"head move")
                        .context("write competing checkpoint blob")?;
                    let leaf = interloper
                        .write_tree(&[TreeItem::new(TreeItemMode::Blob, blob, "note".to_string())])
                        .context("write competing checkpoint leaf tree")?;
                    let prefix_tree = interloper
                        .write_tree(&[TreeItem::new(
                            TreeItemMode::Tree,
                            leaf,
                            "different".to_string(),
                        )])
                        .context("write competing checkpoint prefix tree")?;
                    let checkpoint_tree = interloper
                        .write_tree(&[TreeItem::new(
                            TreeItemMode::Tree,
                            prefix_tree,
                            "cc".to_string(),
                        )])
                        .context("write competing checkpoint tree")?;
                    let root_tree = interloper
                        .write_tree(&[TreeItem::new(
                            TreeItemMode::Tree,
                            checkpoint_tree,
                            "checkpoint".to_string(),
                        )])
                        .context("write competing traces root tree")?;
                    let commit = Commit::new(
                        Signature::new(
                            SignatureType::Author,
                            "Libra".to_string(),
                            "traces@libra".to_string(),
                        ),
                        Signature::new(
                            SignatureType::Committer,
                            "Libra".to_string(),
                            "traces@libra".to_string(),
                        ),
                        root_tree,
                        Vec::new(),
                        "test competing traces head",
                    );
                    let commit_data = commit
                        .to_data()
                        .context("serialize competing traces commit")?;
                    let competing_head =
                        write_git_object(&interloper.repo_path, "commit", &commit_data)
                            .context("write competing traces commit")?;
                    match interloper
                        .update_ref_if_matches(
                            crate::internal::branch::TRACES_BRANCH,
                            None,
                            competing_head,
                        )
                        .await?
                    {
                        RefUpdateOutcome::Updated => Ok(()),
                        RefUpdateOutcome::HeadChanged => {
                            bail!("test competing writer unexpectedly lost an empty-head CAS")
                        }
                    }
                })
            }));
        }

        let attempts = Arc::new(std::sync::Mutex::new(
            Vec::<CheckpointAttemptIndexSnapshot>::new(),
        ));
        {
            let attempts = attempts.clone();
            manager.test_before_checkpoint_ref_cas = Some(Arc::new(move |snapshot| {
                let attempts = attempts.clone();
                Box::pin(async move {
                    attempts
                        .lock()
                        .expect("attempt snapshot lock")
                        .push(snapshot);
                    Ok(())
                })
            }));
        }

        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let committed = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id,
                session_id: "history-retry-scope-session",
                marker_generation: &marker_generation,
                capture_scope: Some(&scope),
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Committed,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: None,
                deadline: None,
            })
            .await
            .expect("scoped checkpoint retries after a competing head move");
        assert_eq!(committed.cas_retries, 1, "exactly one attempt must lose");

        let attempts = attempts.lock().expect("attempt snapshot lock").clone();
        assert_eq!(
            attempts.len(),
            2,
            "must observe rejected and winning attempts"
        );
        let rejected = &attempts[0];
        let winning = &attempts[1];
        assert_ne!(
            rejected.commit_hash, committed.commit_hash,
            "a retry must rebuild a distinct commit"
        );
        assert_ne!(
            rejected.tree_oid, committed.tree_oid,
            "the competing checkpoint prefix must rebuild a distinct root tree"
        );
        assert_eq!(
            rejected.object_index_oids.len(),
            4,
            "a checkpoint attempt contributes three splice trees plus its commit"
        );
        assert_eq!(
            rejected
                .object_index_oids
                .iter()
                .collect::<HashSet<_>>()
                .len(),
            4,
            "the test topology must make every rejected-attempt object distinct"
        );
        assert_eq!(winning.commit_hash, committed.commit_hash);
        assert_eq!(winning.tree_oid, committed.tree_oid);
        assert_eq!(winning.object_index_oids.len(), 4);

        for oid in &rejected.object_index_oids {
            let rows: i64 = db_conn
                .query_one_raw(Statement::from_sql_and_values(
                    db_conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM object_index WHERE o_id = ?",
                    [oid.clone().into()],
                ))
                .await
                .expect("count rejected-attempt object-index row")
                .expect("rejected-attempt object-index count row")
                .try_get_by("n")
                .expect("decode rejected-attempt object-index count");
            assert_eq!(
                rows, 0,
                "rejected checkpoint object {oid} must never reach object_index"
            );
        }
        for oid in &winning.object_index_oids {
            let rows: i64 = db_conn
                .query_one_raw(Statement::from_sql_and_values(
                    db_conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM object_index WHERE o_id = ?",
                    [oid.clone().into()],
                ))
                .await
                .expect("count winning-attempt object-index row")
                .expect("winning-attempt object-index count row")
                .try_get_by("n")
                .expect("decode winning-attempt object-index count");
            assert_eq!(
                rows, 1,
                "winning checkpoint object {oid} must be indexed with the final ref"
            );
        }
        assert!(
            !manager.repo_path.join("object-index-repair").exists(),
            "scoped retries must not leave durable object-index repair markers"
        );
    }

    /// A lease can expire while a checkpoint's objects are already durable
    /// but before the final ref/catalog transaction starts. That worker must
    /// neither move the traces ref nor rewrite/retire its marker during error
    /// cleanup, because both are now owned by the current workspace lease.
    #[tokio::test]
    async fn expired_workspace_scope_between_objects_and_ref_cas_preserves_marker_and_ref() {
        struct SentinelExtra;

        #[async_trait::async_trait]
        impl TracesTxnExtra for SentinelExtra {
            async fn apply(&self, txn: &DatabaseTransaction, _ctx: &TracesCommitCtx) -> Result<()> {
                let marker = reference::ActiveModel {
                    name: Set(Some("scope-final-cas-sentinel".to_string())),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some("must-not-persist".to_string())),
                    remote: Set(None),
                    ..Default::default()
                };
                marker.insert(txn).await?;
                Ok(())
            }
        }

        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE workspace_record (
                    workspace_id TEXT PRIMARY KEY,
                    repo_id TEXT NOT NULL,
                    lease_fence INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    lease_owner TEXT,
                    lease_expires_at INTEGER
                )"
                .to_string(),
            ))
            .await
            .expect("create workspace lease fixture");

        let scope = CaptureScope {
            repo_id: "history-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-scope-workspace".to_string()),
            workspace_fence: Some(17),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
                 ) VALUES (?, ?, ?, 'active', 'history-test', 9999999999999)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live workspace lease");

        let checkpoint_id = "ddee0000-0000-0000-0000-000000000001";
        let marker = TracesInflightMarker::new(
            "history-scope-session",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        let marker_generation = marker
            .generation
            .clone()
            .expect("new writer marker has a generation");
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed scoped writer marker");

        let mut manager = traces_manager(&dir, db_conn.clone());
        let marker_before_expiry = Arc::new(std::sync::Mutex::new(None::<String>));
        {
            let db_conn = db_conn.clone();
            let marker_before_expiry = marker_before_expiry.clone();
            manager.test_before_checkpoint_ref_cas = Some(Arc::new(move |_| {
                let db_conn = db_conn.clone();
                let marker_before_expiry = marker_before_expiry.clone();
                Box::pin(async move {
                    let row = db_conn
                        .query_one_raw(Statement::from_sql_and_values(
                            db_conn.get_database_backend(),
                            "SELECT value FROM metadata_kv
                             WHERE scope = 'agent_traces_inflight'
                               AND target = 'history-scope-session' AND key = ?",
                            [checkpoint_id.into()],
                        ))
                        .await?
                        .ok_or_else(|| {
                            anyhow!("scoped checkpoint marker disappeared before CAS")
                        })?;
                    *marker_before_expiry.lock().expect("marker snapshot lock") =
                        Some(row.try_get_by("value")?);
                    db_conn
                        .execute_raw(Statement::from_string(
                            db_conn.get_database_backend(),
                            "UPDATE workspace_record SET lease_expires_at = 0
                             WHERE workspace_id = 'history-scope-workspace'"
                                .to_string(),
                        ))
                        .await?;
                    Ok(())
                })
            }));
        }

        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let sentinel = SentinelExtra;
        let error = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id,
                session_id: "history-scope-session",
                marker_generation: &marker_generation,
                capture_scope: Some(&scope),
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Committed,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: Some(&sentinel),
                deadline: None,
            })
            .await
            .expect_err("expired scope must reject final checkpoint CAS");
        assert!(
            format!("{error:#}").contains("workspace lease"),
            "unexpected scope error: {error:#}"
        );

        let marker_after: String = db_conn
            .query_one_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "SELECT value FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'
                   AND target = 'history-scope-session' AND key = ?",
                [checkpoint_id.into()],
            ))
            .await
            .expect("read scoped marker after rejected CAS")
            .expect("marker must remain recovery evidence")
            .try_get_by("value")
            .expect("marker value");
        assert_eq!(
            marker_after,
            marker_before_expiry
                .lock()
                .expect("marker snapshot lock")
                .clone()
                .expect("snapshot taken after object construction"),
            "expired cleanup must not alter the marker after object construction"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces ref after rejected CAS")
                .is_none(),
            "expired scope must not move the traces ref"
        );
        let sentinel_rows: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM reference
                 WHERE name = 'scope-final-cas-sentinel'"
                    .to_string(),
            ))
            .await
            .expect("count final-CAS companion sentinel")
            .expect("sentinel count row")
            .try_get_by("n")
            .expect("sentinel count value");
        assert_eq!(
            sentinel_rows, 0,
            "expired scope must not apply companion writes"
        );
    }

    /// Scoped checkpoint writers retain object-index work in memory until the
    /// final ref/catalog CAS. If the workspace lease expires after every
    /// object intent exists but before that transaction, neither a durable
    /// repair marker nor an `object_index` row may advertise the stale
    /// payload to cloud sync.
    #[tokio::test]
    async fn expired_workspace_scope_after_index_intents_before_ref_cas_leaves_no_publication() {
        let dir = tempdir().expect("create scoped index publication fixture");
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir_all(&repo_path).expect("create scoped index publication repository");
        let db_path = repo_path.join(crate::utils::util::DATABASE);
        let db_conn = Arc::new(
            crate::internal::db::create_database(&db_path.to_string_lossy())
                .await
                .expect("create scoped index publication database"),
        );
        prepare_checkpoint_test_schema(&db_conn).await;

        let scope = CaptureScope {
            repo_id: "history-index-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-index-scope-workspace".to_string()),
            workspace_fence: Some(23),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                           'active', 'history-index-test', ?, 9999999999999, 1, 1)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    repo_path.to_string_lossy().into_owned().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live scoped index publication workspace");

        let checkpoint_id = "eeff0000-0000-0000-0000-000000000001";
        let marker = TracesInflightMarker::new(
            "history-index-scope-session",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        let marker_generation = marker
            .generation
            .clone()
            .expect("new writer marker has a generation");
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed scoped index publication writer marker");

        let mut manager = traces_manager(&dir, db_conn.clone());
        {
            let db_conn = db_conn.clone();
            manager.test_before_checkpoint_ref_cas = Some(Arc::new(move |_| {
                let db_conn = db_conn.clone();
                Box::pin(async move {
                    db_conn
                        .execute_raw(Statement::from_string(
                            db_conn.get_database_backend(),
                            "UPDATE workspace_record SET lease_expires_at = 0
                             WHERE workspace_id = 'history-index-scope-workspace'"
                                .to_string(),
                        ))
                        .await
                        .context("expire workspace after checkpoint index intents")?;
                    Ok(())
                })
            }));
        }

        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let error = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id,
                session_id: "history-index-scope-session",
                marker_generation: &marker_generation,
                capture_scope: Some(&scope),
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Committed,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: None,
                deadline: None,
            })
            .await
            .expect_err("expired scope must reject final checkpoint index transaction");
        assert!(
            format!("{error:#}").contains("workspace lease"),
            "unexpected scoped-index error: {error:#}"
        );

        assert!(
            !manager.repo_path.join("object-index-repair").exists(),
            "expired scope must not publish an object-index repair marker"
        );
        let object_index_rows: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM object_index".to_string(),
            ))
            .await
            .expect("count object-index rows after rejected publication")
            .expect("object-index count row")
            .try_get_by("n")
            .expect("decode object-index count");
        assert_eq!(
            object_index_rows, 0,
            "expired scope must not publish a scoped object-index mutation"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces ref after rejected scoped index transaction")
                .is_none(),
            "expired scope must not move the traces ref"
        );
    }

    /// The scoped final transaction owns the ref, catalog companion, and
    /// object-index intent rows together. A companion failure after its own
    /// write must roll every one of them back rather than leaving a
    /// cloud-visible orphan from the already-written loose objects.
    #[tokio::test]
    async fn scoped_checkpoint_failing_extra_rolls_back_ref_catalog_and_object_index() {
        struct FailingScopedExtra;

        #[async_trait::async_trait]
        impl TracesTxnExtra for FailingScopedExtra {
            async fn apply(&self, txn: &DatabaseTransaction, _ctx: &TracesCommitCtx) -> Result<()> {
                let sentinel = reference::ActiveModel {
                    name: Set(Some("scoped-index-rollback-sentinel".to_string())),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some("must-not-persist".to_string())),
                    remote: Set(None),
                    ..Default::default()
                };
                sentinel.insert(txn).await?;
                bail!("simulated scoped checkpoint companion failure")
            }
        }

        let dir = tempdir().expect("create scoped rollback fixture");
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir_all(&repo_path).expect("create scoped rollback repository");
        let db_path = repo_path.join(crate::utils::util::DATABASE);
        let db_conn = Arc::new(
            crate::internal::db::create_database(&db_path.to_string_lossy())
                .await
                .expect("create scoped rollback database"),
        );
        prepare_checkpoint_test_schema(&db_conn).await;

        let scope = CaptureScope {
            repo_id: "history-extra-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-extra-scope-workspace".to_string()),
            workspace_fence: Some(31),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                           'active', 'history-extra-test', ?, 9999999999999, 1, 1)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    repo_path.to_string_lossy().into_owned().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live scoped rollback workspace");

        let checkpoint_id = "aabb0000-0000-0000-0000-000000000004";
        let marker = TracesInflightMarker::new(
            "history-extra-scope-session",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        let marker_generation = marker
            .generation
            .clone()
            .expect("new writer marker has a generation");
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed scoped rollback writer marker");

        let manager = traces_manager(&dir, db_conn.clone());
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let extra = FailingScopedExtra;
        let error = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id,
                session_id: "history-extra-scope-session",
                marker_generation: &marker_generation,
                capture_scope: Some(&scope),
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Committed,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: Some(&extra),
                deadline: None,
            })
            .await
            .expect_err("a failing scoped companion must roll back the final transaction");
        assert!(
            format!("{error:#}").contains("simulated scoped checkpoint companion failure"),
            "unexpected scoped companion error: {error:#}"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces ref after failed scoped companion")
                .is_none(),
            "failing scoped companion must roll back the traces ref"
        );

        let sentinel_rows: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM reference
                 WHERE name = 'scoped-index-rollback-sentinel'"
                    .to_string(),
            ))
            .await
            .expect("count failed scoped companion sentinel")
            .expect("failed scoped companion sentinel count row")
            .try_get_by("n")
            .expect("decode failed scoped companion sentinel count");
        assert_eq!(
            sentinel_rows, 0,
            "failing scoped companion must roll back catalog-side writes"
        );
        let object_index_rows: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM object_index".to_string(),
            ))
            .await
            .expect("count object-index rows after failed scoped companion")
            .expect("failed scoped companion object-index count row")
            .try_get_by("n")
            .expect("decode failed scoped companion object-index count");
        assert_eq!(
            object_index_rows, 0,
            "failing scoped companion must not publish object-index rows"
        );
        assert!(
            !manager.repo_path.join("object-index-repair").exists(),
            "failing scoped companion must not publish object-index repair markers"
        );
    }

    /// The initial scope check happens before the ref CAS, but time can pass
    /// while a catalog companion is running. The composite transaction extra
    /// must check it again immediately before object-index insertion, rolling
    /// back the already-written ref and companion row when the lease expires.
    #[tokio::test]
    async fn scoped_checkpoint_rechecks_lease_after_companion_before_index_upsert() {
        struct ExpiringCompanion {
            entered: Arc<std::sync::atomic::AtomicBool>,
        }

        #[async_trait::async_trait]
        impl TracesTxnExtra for ExpiringCompanion {
            async fn apply(&self, txn: &DatabaseTransaction, _ctx: &TracesCommitCtx) -> Result<()> {
                let sentinel = reference::ActiveModel {
                    name: Set(Some("scoped-index-expiry-sentinel".to_string())),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some("must-not-persist".to_string())),
                    remote: Set(None),
                    ..Default::default()
                };
                sentinel.insert(txn).await?;
                self.entered
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                // The writer lock prevents a competing workspace mutation;
                // expiry is clock based, so this deterministically exercises
                // the recheck after the initial transaction fence passed.
                sleep(Duration::from_millis(1_200)).await;
                Ok(())
            }
        }

        let dir = tempdir().expect("create scoped expiry recheck fixture");
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE workspace_record (
                    workspace_id TEXT PRIMARY KEY,
                    repo_id TEXT NOT NULL,
                    lease_fence INTEGER NOT NULL,
                    state TEXT NOT NULL,
                    lease_owner TEXT,
                    lease_expires_at INTEGER
                )"
                .to_string(),
            ))
            .await
            .expect("create scoped expiry recheck workspace table");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE object_index (
                    o_id TEXT NOT NULL,
                    o_type TEXT NOT NULL,
                    o_size INTEGER NOT NULL,
                    repo_id TEXT NOT NULL,
                    created_at INTEGER NOT NULL,
                    is_synced INTEGER NOT NULL,
                    UNIQUE(repo_id, o_id)
                )"
                .to_string(),
            ))
            .await
            .expect("create scoped expiry recheck object-index table");

        let scope = CaptureScope {
            repo_id: "history-recheck-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-recheck-scope-workspace".to_string()),
            workspace_fence: Some(37),
        };
        // The fence predicate uses SQLite's whole-second `unixepoch('now')`.
        // Pick the next exact second boundary so the initial check is live
        // and the companion delay deterministically crosses its rejection
        // boundary without relying on scheduler timing.
        let lease_expires_at = (chrono::Utc::now().timestamp() + 1) * 1_000;
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
                 ) VALUES (?, ?, ?, 'active', 'history-recheck-test', ?)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    scope.workspace_fence.into(),
                    lease_expires_at.into(),
                ],
            ))
            .await
            .expect("seed scoped expiry recheck workspace");

        let checkpoint_id = "bbcc0000-0000-0000-0000-000000000005";
        let marker = TracesInflightMarker::new(
            "history-recheck-scope-session",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        let marker_generation = marker
            .generation
            .clone()
            .expect("new writer marker has a generation");
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed scoped expiry recheck writer marker");

        let manager = traces_manager(&dir, db_conn.clone());
        let entered = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let extra = ExpiringCompanion {
            entered: entered.clone(),
        };
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let error = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id,
                session_id: "history-recheck-scope-session",
                marker_generation: &marker_generation,
                capture_scope: Some(&scope),
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Committed,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: Some(&extra),
                deadline: None,
            })
            .await
            .expect_err("expired lease after companion must roll back final transaction");
        assert!(
            entered.load(std::sync::atomic::Ordering::SeqCst),
            "the initial scope fence must pass before the companion delay"
        );
        assert!(
            format!("{error:#}").contains("workspace lease"),
            "unexpected post-companion expiry error: {error:#}"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces ref after post-companion expiry")
                .is_none(),
            "post-companion lease expiry must roll back the traces ref"
        );

        let sentinel_rows: i64 = db_conn
            .query_one_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM reference WHERE name = ?",
                ["scoped-index-expiry-sentinel".into()],
            ))
            .await
            .expect("count post-companion rollback sentinel")
            .expect("post-companion rollback sentinel count row")
            .try_get_by("n")
            .expect("decode post-companion rollback sentinel count");
        assert_eq!(
            sentinel_rows, 0,
            "post-companion lease expiry must roll back catalog-side writes"
        );
        let object_index_rows: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM object_index".to_string(),
            ))
            .await
            .expect("count post-companion rollback object-index rows")
            .expect("post-companion rollback object-index count row")
            .try_get_by("n")
            .expect("decode post-companion rollback object-index count");
        assert_eq!(
            object_index_rows, 0,
            "post-companion lease expiry must roll back object-index writes"
        );
        assert!(
            !manager.repo_path.join("object-index-repair").exists(),
            "post-companion lease expiry must not publish object-index repair markers"
        );
    }

    /// The final workspace fence must also cover expiry caused after the
    /// object-index upsert itself. The trigger changes the lease in the same
    /// transaction after index insertion, deterministically exercising the
    /// final-DML fence without a timing-dependent pause.
    #[tokio::test]
    async fn scoped_checkpoint_expiry_after_index_upsert_rolls_back_final_transaction() {
        struct CatalogExtra;

        #[async_trait::async_trait]
        impl TracesTxnExtra for CatalogExtra {
            async fn apply(&self, txn: &DatabaseTransaction, _ctx: &TracesCommitCtx) -> Result<()> {
                let sentinel = reference::ActiveModel {
                    name: Set(Some("scoped-post-index-expiry-sentinel".to_string())),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some("must-not-persist".to_string())),
                    remote: Set(None),
                    ..Default::default()
                };
                sentinel.insert(txn).await?;
                Ok(())
            }
        }

        let dir = tempdir().expect("create post-index expiry fixture");
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir_all(&repo_path).expect("create post-index expiry repository");
        let db_path = repo_path.join(crate::utils::util::DATABASE);
        let db_conn = Arc::new(
            crate::internal::db::create_database(&db_path.to_string_lossy())
                .await
                .expect("create post-index expiry database"),
        );
        prepare_checkpoint_test_schema(&db_conn).await;

        let scope = CaptureScope {
            repo_id: "history-post-index-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-post-index-scope-workspace".to_string()),
            workspace_fence: Some(41),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                           'active', 'history-post-index-test', ?, 9999999999999, 1, 1)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    repo_path.to_string_lossy().into_owned().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live post-index expiry workspace");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TRIGGER expire_workspace_after_checkpoint_index
                 AFTER INSERT ON object_index
                 BEGIN
                     UPDATE workspace_record SET lease_expires_at = 0
                     WHERE workspace_id = 'history-post-index-scope-workspace';
                 END"
                .to_string(),
            ))
            .await
            .expect("install post-index expiry trigger");

        let checkpoint_id = "bbcc0000-0000-0000-0000-000000000006";
        let marker = TracesInflightMarker::new(
            "history-post-index-scope-session",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        let marker_generation = marker
            .generation
            .clone()
            .expect("new writer marker has a generation");
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed post-index expiry writer marker");

        let manager = traces_manager(&dir, db_conn.clone());
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let error = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id,
                session_id: "history-post-index-scope-session",
                marker_generation: &marker_generation,
                capture_scope: Some(&scope),
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Committed,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: Some(&CatalogExtra),
                deadline: None,
            })
            .await
            .expect_err("expiry after index upsert must roll back the final transaction");
        assert!(
            format!("{error:#}").contains("workspace lease"),
            "unexpected post-index expiry error: {error:#}"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces ref after post-index expiry")
                .is_none(),
            "post-index lease expiry must roll back the traces ref"
        );

        let sentinel_rows: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM reference
                 WHERE name = 'scoped-post-index-expiry-sentinel'"
                    .to_string(),
            ))
            .await
            .expect("count post-index rollback sentinel")
            .expect("post-index rollback sentinel count row")
            .try_get_by("n")
            .expect("decode post-index rollback sentinel count");
        assert_eq!(
            sentinel_rows, 0,
            "post-index lease expiry must roll back catalog-side writes"
        );
        let object_index_rows: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM object_index".to_string(),
            ))
            .await
            .expect("count post-index rollback object-index rows")
            .expect("post-index rollback object-index count row")
            .try_get_by("n")
            .expect("decode post-index rollback object-index count");
        assert_eq!(
            object_index_rows, 0,
            "post-index lease expiry must roll back object-index writes"
        );
        let lease_expires_at: i64 = db_conn
            .query_one_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "SELECT lease_expires_at FROM workspace_record
                 WHERE workspace_id = 'history-post-index-scope-workspace'"
                    .to_string(),
            ))
            .await
            .expect("read workspace after post-index rollback")
            .expect("post-index workspace remains")
            .try_get_by("lease_expires_at")
            .expect("decode post-index workspace lease");
        assert_eq!(
            lease_expires_at, 9_999_999_999_999,
            "post-index expiry trigger must roll back with the final transaction"
        );
        assert!(
            !manager.repo_path.join("object-index-repair").exists(),
            "post-index lease expiry must not publish object-index repair markers"
        );
    }

    /// The final SQLite deadline authorization must roll every V2 durable
    /// sink back together: the writer marker, traces ref,
    /// checkpoint/catalog companions, and object-index row must all remain
    /// untouched. The deliberately-expired SQLite half reaches the exact
    /// final SQL boundary without relying on cancellation of COMMIT.
    #[tokio::test]
    async fn scoped_checkpoint_sqlite_deadline_authorization_rolls_back_all_durable_sinks() {
        struct DurableCompanionRows;

        #[async_trait::async_trait]
        impl TracesTxnExtra for DurableCompanionRows {
            async fn apply(&self, txn: &DatabaseTransaction, _ctx: &TracesCommitCtx) -> Result<()> {
                let backend = txn.get_database_backend();
                txn.execute_raw(Statement::from_string(
                    backend,
                    "INSERT INTO agent_checkpoint (checkpoint_id) VALUES ('deadline-checkpoint')"
                        .to_string(),
                ))
                .await
                .context("insert deadline checkpoint companion")?;
                txn.execute_raw(Statement::from_string(
                    backend,
                    "INSERT INTO agent_subagent_content_claim (id) VALUES ('deadline-claim')"
                        .to_string(),
                ))
                .await
                .context("insert deadline content claim companion")?;
                txn.execute_raw(Statement::from_string(
                    backend,
                    "INSERT INTO agent_subagent_link (id) VALUES ('deadline-link')".to_string(),
                ))
                .await
                .context("insert deadline content link companion")?;
                Ok(())
            }
        }

        let dir = tempdir().expect("create deadline final-fence fixture");
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        for ddl in [
            "CREATE TABLE agent_subagent_content_claim (id TEXT PRIMARY KEY)",
            "CREATE TABLE agent_subagent_link (id TEXT PRIMARY KEY)",
            "CREATE TABLE object_index (
                o_id TEXT NOT NULL,
                o_type TEXT NOT NULL,
                o_size INTEGER NOT NULL,
                repo_id TEXT NOT NULL,
                created_at INTEGER NOT NULL,
                is_synced INTEGER NOT NULL,
                UNIQUE(repo_id, o_id)
            )",
            "CREATE TABLE workspace_record (
                workspace_id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                lease_fence INTEGER NOT NULL,
                state TEXT NOT NULL,
                lease_owner TEXT,
                lease_expires_at INTEGER
            )",
        ] {
            db_conn
                .execute_raw(Statement::from_string(
                    db_conn.get_database_backend(),
                    ddl.to_string(),
                ))
                .await
                .expect("create deadline final-fence fixture table");
        }

        let scope = CaptureScope {
            repo_id: "history-deadline-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-deadline-scope-workspace".to_string()),
            workspace_fence: Some(47),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
                 ) VALUES (?, ?, ?, 'active', 'history-deadline-test', 9999999999999)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live deadline workspace");

        let fence = seed_test_writer_fence(
            &db_conn,
            "history-deadline-scope-session",
            "history-deadline-scope-attempt",
        )
        .await;
        let marker_before = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("read marker before deadline final-fence attempt")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("seeded deadline marker");

        let manager = traces_manager(&dir, db_conn.clone());

        let new_head = crate::internal::object_format::parse_repo_oid(
            "1234567890abcdef1234567890abcdef12345678",
        )
        .expect("parse deadline final-fence ref target");
        let object_index_intents = vec![CheckpointObjectIndexIntent {
            oid: "abcdefabcdefabcdefabcdefabcdefabcdefabcd".to_string(),
            object_type: "commit".to_string(),
            size: 1,
        }];
        let companion = DurableCompanionRows;
        let txn_extra = CheckpointCommitTxnExtra {
            extra: Some(&companion),
            capture_scope: Some(&scope),
            object_index_intents: &object_index_intents,
        };
        let ctx = TracesCommitCtx {
            commit_hash: new_head.to_string(),
            tree_oid: "deadline-tree".to_string(),
            metadata_blob_oid: "deadline-metadata".to_string(),
        };
        let deadline =
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(5), 0);
        let error = manager
            .update_ref_if_matches_with_extra(
                crate::internal::branch::TRACES_BRANCH,
                None,
                new_head,
                Some((&txn_extra, &ctx)),
                Some(deadline),
                Some(&fence),
                Some(&scope),
            )
            .await
            .expect_err("expired SQLite final authorization must roll back the final transaction");
        assert!(
            format!("{error:#}").contains("historical import execution deadline"),
            "unexpected final-authorization deadline error: {error:#}"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces ref after post-fence deadline")
                .is_none(),
            "post-fence deadline expiry must not move the traces ref"
        );
        let marker_after = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("read marker after deadline rollback")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("deadline rollback must retain writer marker");
        assert_eq!(
            marker_after, marker_before,
            "deadline rollback must not mutate the durable writer marker"
        );
        for table in [
            "agent_checkpoint",
            "agent_subagent_content_claim",
            "agent_subagent_link",
            "object_index",
        ] {
            assert_eq!(
                test_table_row_count(&db_conn, table).await,
                0,
                "final SQLite deadline authorization must roll back {table}"
            );
        }
    }

    /// Object ownership and rejected-append cleanup each update the same
    /// durable writer marker in their own transaction.  Expire the workspace
    /// from a trigger after each marker DML so the final commit fence proves
    /// that no stale writer can publish (or retire) that recovery evidence.
    #[tokio::test]
    async fn scoped_history_marker_mutations_expiring_after_dml_roll_back() {
        let dir = tempdir().expect("create scoped marker final-fence fixture");
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir_all(&repo_path).expect("create scoped marker repository");
        let db_path = repo_path.join(crate::utils::util::DATABASE);
        let db_conn = Arc::new(
            crate::internal::db::create_database(&db_path.to_string_lossy())
                .await
                .expect("create scoped marker database"),
        );
        prepare_checkpoint_test_schema(&db_conn).await;

        let scope = CaptureScope {
            repo_id: "history-marker-scope-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("history-marker-scope-workspace".to_string()),
            workspace_fence: Some(43),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                           'active', 'history-marker-test', ?, 9999999999999, 1, 1)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    repo_path.to_string_lossy().into_owned().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live scoped marker workspace");

        let manager = traces_manager(&dir, db_conn.clone());
        let fence = seed_test_writer_fence(
            &db_conn,
            "history-marker-scope-session",
            "history-marker-scope-attempt",
        )
        .await;
        let oid = crate::internal::object_format::parse_repo_oid(
            "0123456789abcdef0123456789abcdef01234567",
        )
        .expect("parse test ownership object id");
        let marker_before = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker before post-DML expiry")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("seeded marker exists");

        // A future monotonic half reaches the final DB boundary, while the
        // deliberately-expired immutable SQLite half rejects the marker DML
        // and rolls it back before COMMIT can be dispatched.
        let expired_sqlite_deadline =
            CaptureCommitDeadline::from_test_pair(Instant::now() + Duration::from_secs(5), 0);
        let error = manager
            .persist_attempt_oid_before_write(
                &fence,
                Some(&scope),
                &oid,
                Some(expired_sqlite_deadline),
            )
            .await
            .expect_err("expired SQLite authorization must reject marker preclaim");
        assert!(format!("{error:#}").contains("historical import execution deadline"));
        let marker_after_sqlite_deadline = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after SQLite deadline preclaim rollback")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker remains after SQLite deadline preclaim rollback");
        assert_eq!(
            marker_after_sqlite_deadline, marker_before,
            "expired SQLite authorization must not alter durable writer ownership"
        );

        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TRIGGER expire_scope_after_history_marker_update
                 AFTER UPDATE OF value ON metadata_kv
                 WHEN NEW.scope = 'agent_traces_inflight'
                   AND NEW.target = 'history-marker-scope-session'
                   AND NEW.key = 'history-marker-scope-attempt'
                 BEGIN
                     UPDATE workspace_record SET lease_expires_at = 0
                     WHERE workspace_id = 'history-marker-scope-workspace';
                 END"
                .to_string(),
            ))
            .await
            .expect("install post-marker-update expiry trigger");

        let error = manager
            .persist_attempt_oid_before_write(&fence, Some(&scope), &oid, None)
            .await
            .expect_err("expiry after ownership preclaim must roll back marker mutation");
        assert!(format!("{error:#}").contains("workspace lease"));
        let marker_after_preclaim_expiry = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after preclaim rollback")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker remains after preclaim rollback");
        assert_eq!(
            marker_after_preclaim_expiry, marker_before,
            "post-preclaim expiry must not alter durable writer ownership"
        );
        scope
            .assert_workspace_fence_live(&*db_conn)
            .await
            .expect("post-preclaim expiry trigger must roll back with the marker");

        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "DROP TRIGGER expire_scope_after_history_marker_update".to_string(),
            ))
            .await
            .expect("remove post-marker-update expiry trigger");
        manager
            .persist_attempt_oid_before_write(&fence, Some(&scope), &oid, None)
            .await
            .expect("preclaim ownership while workspace lease is live");
        let marker_after_preclaim = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after successful preclaim")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker remains after successful preclaim");
        assert_eq!(marker_after_preclaim.oids, vec![oid.to_string()]);
        assert!(marker_after_preclaim.created_oids.is_empty());
        assert!(!marker_after_preclaim.cleanup_pending);

        let error = manager
            .finalize_attempt_oid_after_write(
                &fence,
                Some(&scope),
                &oid,
                true,
                Some(CaptureCommitDeadline::from_test_pair(
                    Instant::now() + Duration::from_secs(5),
                    0,
                )),
            )
            .await
            .expect_err("expired SQLite authorization must reject marker finalization");
        assert!(format!("{error:#}").contains("historical import execution deadline"));
        let marker_after_sqlite_deadline_finalization = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after SQLite deadline finalization rollback")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker remains after SQLite deadline finalization rollback");
        assert_eq!(
            marker_after_sqlite_deadline_finalization, marker_after_preclaim,
            "expired SQLite authorization must preserve the preclaim recovery state"
        );

        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TRIGGER expire_scope_after_history_marker_finalization
                 AFTER UPDATE OF value ON metadata_kv
                 WHEN NEW.scope = 'agent_traces_inflight'
                   AND NEW.target = 'history-marker-scope-session'
                   AND NEW.key = 'history-marker-scope-attempt'
                 BEGIN
                     UPDATE workspace_record SET lease_expires_at = 0
                     WHERE workspace_id = 'history-marker-scope-workspace';
                 END"
                .to_string(),
            ))
            .await
            .expect("install post-finalization expiry trigger");
        let error = manager
            .finalize_attempt_oid_after_write(&fence, Some(&scope), &oid, true, None)
            .await
            .expect_err("expiry after ownership finalization must roll back marker mutation");
        assert!(format!("{error:#}").contains("workspace lease"));
        let marker_after_finalization_expiry = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after finalization rollback")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker remains after finalization rollback");
        assert_eq!(
            marker_after_finalization_expiry, marker_after_preclaim,
            "post-finalization expiry must preserve the preclaim recovery state"
        );
        scope
            .assert_workspace_fence_live(&*db_conn)
            .await
            .expect("post-finalization expiry trigger must roll back with the marker");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "DROP TRIGGER expire_scope_after_history_marker_finalization".to_string(),
            ))
            .await
            .expect("remove post-finalization expiry trigger");

        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TRIGGER expire_scope_after_history_cleanup_marker
                 AFTER UPDATE OF value ON metadata_kv
                 WHEN NEW.scope = 'agent_traces_inflight'
                   AND NEW.target = 'history-marker-scope-session'
                   AND NEW.key = 'history-marker-scope-attempt'
                 BEGIN
                     UPDATE workspace_record SET lease_expires_at = 0
                     WHERE workspace_id = 'history-marker-scope-workspace';
                 END"
                .to_string(),
            ))
            .await
            .expect("install post-cleanup-marker expiry trigger");
        let error = manager
            .cleanup_rejected_checkpoint_objects(
                &fence,
                Some(&scope),
                &HashSet::from([oid.to_string()]),
            )
            .await
            .expect_err("expiry after cleanup registration must roll back marker mutation");
        assert!(format!("{error:#}").contains("workspace lease"));
        let marker_after_cleanup_expiry = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after cleanup rollback")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker remains after cleanup rollback");
        assert_eq!(
            marker_after_cleanup_expiry, marker_after_preclaim,
            "post-cleanup expiry must not publish a cleanup-pending marker"
        );
        scope
            .assert_workspace_fence_live(&*db_conn)
            .await
            .expect("post-cleanup expiry trigger must roll back with the marker");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "DROP TRIGGER expire_scope_after_history_cleanup_marker".to_string(),
            ))
            .await
            .expect("remove post-cleanup-marker expiry trigger");

        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TRIGGER expire_scope_after_history_cleanup_clear
                 AFTER DELETE ON metadata_kv
                 WHEN OLD.scope = 'agent_traces_inflight'
                   AND OLD.target = 'history-marker-scope-session'
                   AND OLD.key = 'history-marker-scope-attempt'
                 BEGIN
                     UPDATE workspace_record SET lease_expires_at = 0
                     WHERE workspace_id = 'history-marker-scope-workspace';
                 END"
                .to_string(),
            ))
            .await
            .expect("install post-cleanup-clear expiry trigger");
        let no_new_objects = HashSet::new();
        let error = manager
            .cleanup_rejected_checkpoint_objects(&fence, Some(&scope), &no_new_objects)
            .await
            .expect_err("expiry after cleanup marker removal must roll back the deletion");
        assert!(format!("{error:#}").contains("workspace lease"));
        let marker_after_clear_expiry = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after cleanup-clear rollback")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker remains after cleanup-clear rollback");
        assert_eq!(
            marker_after_clear_expiry, marker_after_preclaim,
            "post-cleanup-clear expiry must retain durable recovery evidence"
        );
        scope
            .assert_workspace_fence_live(&*db_conn)
            .await
            .expect("post-cleanup-clear expiry trigger must roll back with the marker");
    }

    /// crash_after_objects_before_ref_leaves_only_gc_objects AND
    /// crash_after_ref_before_catalog_is_impossible_or_atomically_recovers:
    /// with a transactional extra, ref + companion writes are one atomic
    /// unit. A failing extra (simulating the claim/catalog write dying after
    /// objects were built) must leave the ref UNMOVED — only unreachable
    /// loose objects remain; the success path lands ref + companion row
    /// together, so a "ref moved but catalog missing" window cannot exist.
    #[tokio::test]
    async fn crash_between_objects_ref_and_catalog_is_atomic() {
        struct FailingExtra;
        #[async_trait::async_trait]
        impl TracesTxnExtra for FailingExtra {
            async fn apply(
                &self,
                _txn: &DatabaseTransaction,
                _ctx: &TracesCommitCtx,
            ) -> Result<()> {
                anyhow::bail!("simulated crash after objects, inside the final transaction")
            }
        }
        struct MarkerExtra;
        #[async_trait::async_trait]
        impl TracesTxnExtra for MarkerExtra {
            async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()> {
                // Stand-in for the catalog INSERT: a reference row keyed by
                // the commit, written in the SAME transaction as the ref.
                let marker = reference::ActiveModel {
                    name: Set(Some(format!("marker/{}", ctx.commit_hash))),
                    kind: Set(ConfigKind::Branch),
                    commit: Set(Some(ctx.commit_hash.clone())),
                    remote: Set(None),
                    ..Default::default()
                };
                marker.insert(txn).await?;
                Ok(())
            }
        }

        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());

        // Seed head H0.
        let seeded = append_test_checkpoint(
            &manager,
            "aaaa0000-0000-0000-0000-00000000000a",
            &blobs,
            None,
        )
        .await
        .expect("seed");
        let h0 = seeded.commit_hash;

        // Failing extra: append errors, ref must NOT move (objects on disk
        // are the only residue — the documented GC-only window).
        let failing = FailingExtra;
        let err = append_test_checkpoint(
            &manager,
            "bbbb0000-0000-0000-0000-00000000000b",
            &blobs,
            Some(&failing),
        )
        .await
        .expect_err("failing extra must fail the append closed");
        assert!(
            format!("{err:#}").contains("simulated crash"),
            "got {err:#}"
        );
        assert_eq!(
            manager.resolve_history_head().await.unwrap().unwrap(),
            h0,
            "ref must not move when the companion transaction fails"
        );

        // Success path: ref + companion row land atomically.
        let marker = MarkerExtra;
        let committed = append_test_checkpoint(
            &manager,
            "cccc0000-0000-0000-0000-00000000000c",
            &blobs,
            Some(&marker),
        )
        .await
        .expect("append with marker extra");
        assert_eq!(
            manager.resolve_history_head().await.unwrap().unwrap(),
            committed.commit_hash
        );
        let marker_row = reference::Entity::find()
            .filter(reference::Column::Name.eq(format!("marker/{}", committed.commit_hash)))
            .one(&*db_conn)
            .await
            .unwrap();
        assert!(
            marker_row.is_some(),
            "companion row must exist the instant the ref moved (same txn)"
        );
    }

    #[tokio::test]
    async fn test_update_ref_if_matches_rejects_stale_history_head() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).unwrap();
        let objects_dir = repo_path.join("objects");

        let storage = Arc::new(LocalStorage::new(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new(storage, repo_path, db_conn);

        let task_hash = crate::internal::ai::util::parse_repo_object_id(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .unwrap();
        let plan_hash = crate::internal::ai::util::parse_repo_object_id(
            "f4e6d0434b8b29ae775ad8c2e48c5391e69de29b",
        )
        .unwrap();
        let frame_hash = crate::internal::ai::util::parse_repo_object_id(
            "a4e6d0434b8b29ae775ad8c2e48c5391e69de29b",
        )
        .unwrap();

        manager.append("task", "task-1", task_hash).await.unwrap();
        let stale_head = manager.resolve_history_head().await.unwrap();
        let stale_commit = manager
            .create_append_commit(stale_head, "plan", "plan-1", plan_hash)
            .expect("stale append commit should be created");

        manager
            .append("context_frame", "frame-1", frame_hash)
            .await
            .unwrap();

        let outcome = manager
            .update_ref_if_matches(AI_REF, stale_head, stale_commit)
            .await
            .expect("stale ref update should not error");
        assert_eq!(outcome, RefUpdateOutcome::HeadChanged);

        manager.append("plan", "plan-1", plan_hash).await.unwrap();

        assert!(
            manager
                .get_object_hash("context_frame", "frame-1")
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            manager
                .get_object_hash("plan", "plan-1")
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn fenced_expired_writer_cannot_adopt_same_id_takeover_marker() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let marker = TracesInflightMarker::new("fenced-session", "fenced-attempt", 0);
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("write expired writer marker");
        let stale_fence = TracesWriterFence {
            session_id: marker.session_id.clone(),
            attempt_id: marker.attempt_id.clone(),
            generation: marker.generation.clone().expect("stale marker generation"),
        };
        let replacement = TracesInflightMarker::new(
            "fenced-session",
            "fenced-attempt",
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(&*db_conn, &replacement)
            .await
            .expect("replace marker with takeover generation");
        let blobs = RedactedBytes::new_unchecked(b"{}".to_vec());
        let public_error = manager
            .append_checkpoint_commit(CheckpointCommitParams {
                checkpoint_id: "fenced-attempt",
                session_id: "fenced-session",
                marker_generation: &stale_fence.generation,
                capture_scope: None,
                agent_kind: "claude_code",
                parent_commit: None,
                scope: CheckpointScope::Subagent,
                tool_use_id: None,
                metadata_json: &blobs,
                transcript_redacted: &blobs,
                lifecycle_events_jsonl: &blobs,
                redaction_report_json: &blobs,
                txn_extra: None,
                deadline: None,
            })
            .await
            .expect_err("public append must not adopt a takeover marker generation");
        assert!(
            format!("{public_error:#}").contains("fenced or replaced before append"),
            "unexpected error: {public_error:#}"
        );
        let public_fence = manager
            .load_traces_writer_fence("fenced-session", "fenced-attempt", None)
            .await
            .expect("load takeover writer fence after rejected public append");
        assert_eq!(
            public_fence.generation,
            replacement
                .generation
                .as_deref()
                .expect("takeover marker generation"),
            "stale public append mutated the takeover marker"
        );
        let new_head = write_git_object(&manager.repo_path, "blob", b"stalled writer commit")
            .expect("write stalled writer object");

        let preclaim_error = manager
            .persist_attempt_oid_before_write(&stale_fence, None, &new_head, None)
            .await
            .expect_err("replacement generation must fence stale object preclaims");
        assert!(
            format!("{preclaim_error:#}").contains("generation was fenced or replaced"),
            "unexpected error: {preclaim_error:#}"
        );
        let current_fence = manager
            .load_traces_writer_fence("fenced-session", "fenced-attempt", None)
            .await
            .expect("load takeover writer fence after rejected preclaim");
        assert_eq!(
            current_fence.generation,
            replacement
                .generation
                .as_deref()
                .expect("takeover marker generation"),
            "stale object preclaim mutated the takeover marker"
        );

        let error = manager
            .update_ref_if_matches_with_extra(
                crate::internal::branch::TRACES_BRANCH,
                None,
                new_head,
                None,
                None,
                Some(&stale_fence),
                None,
            )
            .await
            .expect_err("replacement generation must fence a resumed writer");
        assert!(
            format!("{error:#}").contains("generation was fenced or replaced"),
            "unexpected error: {error:#}"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces head")
                .is_none(),
            "fenced writer moved the traces ref"
        );
    }

    // -------------------------------------------------------------------
    // AG-20: E5 line-safe chunking
    // -------------------------------------------------------------------

    /// Small inputs (≤ max) come back as one borrowed chunk, unsplit.
    #[test]
    fn chunker_returns_single_chunk_at_or_below_threshold() {
        let content = b"line-1\nline-2\n";
        let chunks = chunk_transcript_line_safe(content, content.len()).unwrap();
        assert_eq!(chunks, vec![&content[..]]);
        // Empty input still yields one (empty) chunk to name.
        let empty = chunk_transcript_line_safe(b"", 16).unwrap();
        assert_eq!(empty, vec![&b""[..]]);
    }

    /// Chunks cut ONLY at line boundaries, each stays within the limit,
    /// and concatenating them reproduces the input byte-for-byte.
    #[test]
    fn chunker_splits_on_line_boundaries_and_roundtrips() {
        let mut content = Vec::new();
        for index in 0..100 {
            content.extend_from_slice(format!("{{\"line\":{index}}}\n").as_bytes());
        }
        let max = 64;
        let chunks = chunk_transcript_line_safe(&content, max).unwrap();
        assert!(chunks.len() > 1, "must actually chunk");
        for chunk in &chunks {
            assert!(chunk.len() <= max, "chunk of {} exceeds {max}", chunk.len());
            assert!(
                chunk.ends_with(b"\n"),
                "every newline-terminated input chunk must end at a line boundary"
            );
        }
        let owned: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        assert_eq!(reassemble_transcript_chunks(&owned), content);
    }

    /// A final unterminated line is preserved verbatim (no invented `\n`).
    #[test]
    fn chunker_preserves_final_unterminated_line() {
        let content = b"aaaa\nbbbb\ncccc-tail";
        let chunks = chunk_transcript_line_safe(content, 10).unwrap();
        let owned: Vec<Vec<u8>> = chunks.iter().map(|c| c.to_vec()).collect();
        assert_eq!(reassemble_transcript_chunks(&owned), content.to_vec());
        assert!(chunks.last().unwrap().ends_with(b"cccc-tail"));
    }

    /// E5 hard error: a single line larger than the threshold refuses to
    /// split mid-line.
    #[test]
    fn chunker_rejects_single_line_over_threshold() {
        let long_line = vec![b'x'; 100];
        let err = chunk_transcript_line_safe(&long_line, 64).unwrap_err();
        assert!(
            err.to_string().contains("exceeds"),
            "error must explain the oversized line: {err}"
        );
        // Terminated variant errors too.
        let mut terminated = long_line.clone();
        terminated.push(b'\n');
        assert!(chunk_transcript_line_safe(&terminated, 64).is_err());
        // Zero max is rejected outright.
        assert!(chunk_transcript_line_safe(b"x", 0).is_err());
    }

    /// The durable writer, not only the pure splitter, honors the in-process
    /// test threshold and emits the manifest/tree E5 layout. Production keeps
    /// the fixed 50 MiB threshold; this scoped override cannot leak into a
    /// hook binary or another test task.
    #[tokio::test]
    async fn checkpoint_writer_chunks_transcript_with_task_scoped_test_threshold() {
        let dir = tempdir().expect("create checkpoint fixture directory");
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn);
        let checkpoint_id = "e5000000-0000-0000-0000-000000000001";
        let mut transcript = Vec::new();
        for index in 0..40 {
            transcript.extend_from_slice(
                format!("{{\"turn\":{index:04},\"text\":\"chunk me\"}}\n").as_bytes(),
            );
        }
        let blobs = RedactedBytes::new_unchecked(transcript.clone());

        let commit = crate::internal::ai::traces::with_test_transcript_chunk_threshold(
            256,
            append_test_checkpoint(&manager, checkpoint_id, &blobs, None),
        )
        .await
        .expect("append chunked checkpoint");

        let root = manager
            .load_commit_tree(&commit.commit_hash)
            .expect("load chunked checkpoint root");
        let inner_oid = manager
            .checkpoint_inner_tree_from_root(&root, checkpoint_id)
            .expect("locate checkpoint leaf")
            .expect("checkpoint leaf exists");
        let inner = manager.load_tree(&inner_oid).expect("load checkpoint leaf");
        let transcript_tree = inner
            .iter()
            .find(|entry| entry.name == "transcript")
            .expect("checkpoint transcript tree");
        let parts = manager
            .load_tree(&transcript_tree.id)
            .expect("load transcript parts");
        assert!(parts.len() > 1, "the writer must emit E5 chunk parts");
        assert!(
            parts
                .iter()
                .all(|part| part.name.starts_with("claude_code.jsonl.")),
            "chunked writer must not retain an unchunked transcript blob: {parts:?}"
        );

        let manifest = inner
            .iter()
            .find(|entry| entry.name == "manifest.json")
            .expect("manifest entry");
        let manifest_bytes =
            read_git_object(&manager.repo_path, &manifest.id).expect("read checkpoint manifest");
        let manifest: serde_json::Value =
            serde_json::from_slice(&manifest_bytes).expect("parse checkpoint manifest");
        let declared_parts = manifest["entries"]["transcript"]["parts"]
            .as_array()
            .expect("manifest declares ordered transcript parts");
        assert_eq!(declared_parts.len(), parts.len());
        assert_eq!(
            manifest["entries"]["transcript"]["chunked"],
            serde_json::Value::Bool(true)
        );
        let declared_size = declared_parts
            .iter()
            .map(|part| part["byte_len"].as_u64().expect("part byte_len"))
            .sum::<u64>();
        assert_eq!(declared_size, transcript.len() as u64);
    }

    // -------------------------------------------------------------------
    // AG-20: content hash format + reader tolerance
    // -------------------------------------------------------------------

    /// Writer format is `sha256:` + 64 lowercase hex, no trailing newline,
    /// and equals the sha256 of the concatenated sections.
    #[test]
    fn content_hash_has_pinned_format_and_value() {
        let hash = checkpoint_content_hash(&[b"alpha", b"beta"]);
        assert!(hash.starts_with("sha256:"));
        let hex = &hash["sha256:".len()..];
        assert_eq!(hex.len(), 64);
        assert!(hex.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(!hash.ends_with('\n'));
        // Concatenation order matters and is deterministic.
        assert_eq!(hash, checkpoint_content_hash(&[b"alphabeta"]));
        assert_ne!(hash, checkpoint_content_hash(&[b"beta", b"alpha"]));
    }

    /// Reader tolerance (E4-entire table): the prefix form and legacy bare
    /// hex both parse to the same digest; garbage does not parse.
    #[test]
    fn parse_content_hash_accepts_prefix_and_legacy_bare_hex() {
        let digest = "a".repeat(64);
        assert_eq!(
            parse_content_hash(&format!("sha256:{digest}")),
            Some(digest.clone())
        );
        assert_eq!(parse_content_hash(&digest), Some(digest.clone()));
        // Whitespace slack (e.g. a stray trailing newline) is tolerated.
        assert_eq!(
            parse_content_hash(&format!("sha256:{digest}\n")),
            Some(digest.clone())
        );
        // Uppercase hex normalises to lowercase.
        assert_eq!(
            parse_content_hash(&digest.to_uppercase()),
            Some(digest.clone())
        );
        assert_eq!(parse_content_hash("sha256:tooshort"), None);
        assert_eq!(parse_content_hash(&"z".repeat(64)), None);
        assert_eq!(parse_content_hash(""), None);
    }

    // -------------------------------------------------------------------
    // AG-20: in-flight marker liveness math
    // -------------------------------------------------------------------

    #[test]
    fn inflight_marker_liveness_respects_ttl() {
        let marker = TracesInflightMarker::new("session-a", "attempt-1", 1_000);
        assert!(marker.is_live(1_000));
        assert!(marker.is_live(1_000 + AGENT_TRACES_INFLIGHT_TTL_MS - 1));
        assert!(!marker.is_live(1_000 + AGENT_TRACES_INFLIGHT_TTL_MS));
        // Marker JSON round-trips (schema pin for the prune side).
        let json = serde_json::to_string(&marker).unwrap();
        let back: TracesInflightMarker = serde_json::from_str(&json).unwrap();
        assert_eq!(back.session_id, "session-a");
        assert_eq!(back.attempt_id, "attempt-1");
        assert_eq!(back.ttl_ms, AGENT_TRACES_INFLIGHT_TTL_MS);
        assert_eq!(back.schema_version, 3);
        assert!(
            back.generation
                .as_deref()
                .is_some_and(|generation| uuid::Uuid::parse_str(generation).is_ok())
        );
        assert!(back.commit.is_none());
        assert!(back.oids.is_empty());
        assert!(back.created_oids.is_empty());
        assert!(!back.cleanup_pending);
    }

    #[tokio::test]
    async fn rejected_cleanup_registration_under_expired_deadline_is_bounded_and_preserves_marker()
    {
        let dir = tempdir().expect("create rejected cleanup grace fixture");
        let repo_path = dir.path().join(".libra");
        std::fs::create_dir(&repo_path).expect("create rejected cleanup repository");
        let objects_dir = repo_path.join("objects");
        std::fs::create_dir(&objects_dir).expect("create rejected cleanup objects directory");
        let database_path = repo_path.join("libra.db");
        let db_conn = Arc::new(
            db::create_database(
                database_path
                    .to_str()
                    .expect("rejected cleanup database path is utf-8"),
            )
            .await
            .expect("create rejected cleanup database"),
        );
        let manager = HistoryManager::new_with_ref(
            Arc::new(LocalStorage::new(objects_dir)),
            repo_path.clone(),
            db_conn.clone(),
            crate::internal::branch::TRACES_BRANCH,
        );
        let fence =
            seed_test_writer_fence(&db_conn, "cleanup-grace-session", "cleanup-grace-attempt")
                .await;
        let (candidate, created) = write_git_object_with_status(
            &repo_path,
            "blob",
            b"rejected cleanup recovery candidate",
        )
        .expect("write rejected cleanup recovery candidate");
        assert!(created, "fixture candidate must be newly created");
        manager
            .finalize_attempt_oid_after_write(&fence, None, &candidate, true, None)
            .await
            .expect("persist original marker ownership before lock contention");
        let original = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list original rejected cleanup marker")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("original rejected cleanup marker exists");
        assert_eq!(original.created_oids, vec![candidate.to_string()]);

        let locker = db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("rejected cleanup database path is utf-8"),
            Duration::from_millis(50),
        )
        .await
        .expect("open rejected cleanup lock holder");
        let lock = db::begin_write_transaction(&locker)
            .await
            .expect("acquire rejected cleanup writer lock");
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_millis(1), 0);
        let cleanup = tokio::time::timeout(
            Duration::from_secs(2),
            manager.cleanup_rejected_checkpoint_objects_until(
                &fence,
                None,
                &HashSet::from([candidate.to_string()]),
                Some(expired),
            ),
        )
        .await
        .expect("expired append cleanup must use its short recovery grace");
        lock.rollback()
            .await
            .expect("release rejected cleanup writer lock");

        let error =
            cleanup.expect_err("writer contention must defer rejected cleanup registration");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<RejectedCheckpointCleanupDeferred>()),
            "bounded cleanup must report retained recovery evidence: {error:#}"
        );
        let retained = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after bounded rejected cleanup")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("deferred cleanup must retain original marker evidence");
        assert_eq!(
            retained, original,
            "timed-out cleanup registration must not alter the durable marker"
        );
    }

    #[tokio::test]
    async fn checkpoint_precommit_read_deadline_preserves_marker_without_late_update() {
        let (_dir, _database_path, db_conn, manager, fence) = deadline_contention_fixture().await;
        let original = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("load marker before delayed checkpoint read")
        .expect("seeded marker exists before delayed checkpoint read")
        .value;
        let (oid, created) = write_git_object_with_status(
            &manager.repo_path,
            "blob",
            b"delayed checkpoint marker read candidate",
        )
        .expect("write delayed checkpoint marker candidate");
        assert!(created, "fixture candidate must be newly created");
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_millis(30))
            .expect("establish short checkpoint read deadline");
        let result = with_checkpoint_precommit_read_delay(
            Duration::from_millis(80),
            manager.persist_attempt_oid_before_write(&fence, None, &oid, Some(deadline)),
        )
        .await;
        let error = result.expect_err("delayed checkpoint marker read must observe its deadline");
        assert!(
            format!("{error:#}").contains("checkpoint append exceeded"),
            "unexpected delayed checkpoint read error: {error:#}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let retained = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("load marker after delayed checkpoint read")
        .expect("delayed read must retain the original marker")
        .value;
        assert_eq!(
            retained, original,
            "a checkpoint read that reaches its deadline must not publish a late marker update"
        );
    }

    #[tokio::test]
    async fn checkpoint_precommit_read_rechecks_deadline_after_ready_read() {
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_millis(20))
            .expect("establish ready-read checkpoint deadline");
        let result = await_checkpoint_precommit_read_until(
            Some(deadline),
            "complete a deliberately late ready read",
            async {
                // This blocks in one poll and then returns Ready. `timeout_at`
                // can therefore observe the ready result before its timer;
                // the post-read deadline check is what closes that edge.
                std::thread::sleep(Duration::from_millis(60));
                Ok::<(), anyhow::Error>(())
            },
        )
        .await;
        let error = result.expect_err("a ready read that crossed the deadline must be rejected");
        assert!(
            format!("{error:#}").contains("checkpoint append exceeded"),
            "unexpected ready-read deadline error: {error:#}"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_scoped_inner_fence_deadline_preserves_marker_without_late_write() {
        let (_dir, _database_path, db_conn, manager, fence) = deadline_contention_fixture().await;
        let scope = CaptureScope {
            repo_id: "rejected-cleanup-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("rejected-cleanup-deadline-workspace".to_string()),
            workspace_fence: Some(71),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                           'active', 'rejected-cleanup-deadline-owner', ?,
                           unixepoch('now') * 1000 + 600000, 1, 1)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    manager.repo_path.to_string_lossy().into_owned().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live scoped rejected-cleanup workspace lease");
        let original = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("load marker before scoped rejected-cleanup delay")
        .expect("seeded scoped rejected-cleanup marker exists")
        .value;
        let (oid, created) = write_git_object_with_status(
            &manager.repo_path,
            "blob",
            b"rejected cleanup scoped deadline candidate",
        )
        .expect("write scoped rejected-cleanup candidate object");
        assert!(created, "fixture candidate must be newly created");
        let updated = HashSet::from([oid.to_string()]);
        let empty = HashSet::new();
        for newly_written in [&updated, &empty] {
            let cleanup = tokio::time::timeout(
                Duration::from_secs(2),
                crate::internal::ai::traces::with_traces_marker_precommit_read_delay(
                    Duration::from_millis(350),
                    manager.cleanup_rejected_checkpoint_objects_until(
                        &fence,
                        Some(&scope),
                        newly_written,
                        None,
                    ),
                ),
            )
            .await
            .expect("scoped rejected-cleanup inner fence must honor its recovery deadline");
            let error = cleanup.expect_err(
                "a delayed scoped marker fence must defer rejected-cleanup registration",
            );
            assert!(
                error
                    .chain()
                    .any(|cause| cause.is::<RejectedCheckpointCleanupDeferred>()),
                "scoped cleanup deadline must retain recovery evidence: {error:#}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
            let retained = crate::internal::metadata::MetadataKv::get_with_conn(
                &*db_conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                &fence.session_id,
                &fence.attempt_id,
            )
            .await
            .expect("load marker after scoped rejected-cleanup deadline")
            .expect("scoped rejected cleanup must retain the original marker")
            .value;
            assert_eq!(
                retained, original,
                "a delayed scoped fence must not publish a late rejected-cleanup mutation"
            );
        }
    }

    #[tokio::test]
    async fn rejected_cleanup_preserves_expired_primary_sqlite_deadline() {
        let (_dir, _database_path, db_conn, manager, fence) = deadline_contention_fixture().await;
        let scope = CaptureScope {
            repo_id: "rejected-cleanup-sqlite-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("rejected-cleanup-sqlite-deadline-workspace".to_string()),
            workspace_fence: Some(72),
        };
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO workspace_record (
                    workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                    state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
                 ) VALUES (?, ?, 'task_copy', NULL, ?, 'agent',
                           'active', 'rejected-cleanup-sqlite-deadline-owner', ?,
                           unixepoch('now') * 1000 + 600000, 1, 1)",
                [
                    scope.workspace_id.clone().into(),
                    scope.repo_id.clone().into(),
                    manager.repo_path.to_string_lossy().into_owned().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .expect("seed live scoped rejected-cleanup workspace lease");
        let original = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("load marker before expired-SQLite cleanup")
        .expect("seeded rejected-cleanup marker exists")
        .value;
        let (oid, created) = write_git_object_with_status(
            &manager.repo_path,
            "blob",
            b"rejected cleanup expired sqlite deadline candidate",
        )
        .expect("write expired-SQLite cleanup candidate object");
        assert!(created, "fixture candidate must be newly created");
        let newly_written = HashSet::from([oid.to_string()]);
        let cleanup = manager
            .cleanup_rejected_checkpoint_objects_until(
                &fence,
                Some(&scope),
                &newly_written,
                Some(CaptureCommitDeadline::from_test_pair(
                    Instant::now() + Duration::from_secs(5),
                    0,
                )),
            )
            .await;
        let error = cleanup.expect_err(
            "an expired primary SQLite deadline must reject cleanup-marker registration",
        );
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<RejectedCheckpointCleanupDeferred>()),
            "expired primary SQLite deadline must retain cleanup evidence: {error:#}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        let retained = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("load marker after expired-SQLite cleanup")
        .expect("expired SQLite deadline must retain the original marker")
        .value;
        assert_eq!(
            retained, original,
            "an expired primary SQLite deadline must not publish a cleanup marker mutation"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_defers_oversized_marker_without_mutating_durable_evidence() {
        let (_dir, _database_path, db_conn, manager, fence) = deadline_contention_fixture().await;
        let entry = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("load seeded marker")
        .expect("seeded marker exists");
        let mut raw: serde_json::Value =
            serde_json::from_str(&entry.value).expect("decode seeded marker JSON");
        raw["legacy_extension"] = serde_json::Value::String("x".repeat(
            crate::internal::ai::traces::TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES,
        ));
        let oversized = serde_json::to_string(&raw).expect("encode oversized legacy marker");
        assert!(
            oversized.len()
                > crate::internal::ai::traces::TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES,
            "fixture must exceed the bounded recovery input"
        );
        crate::internal::metadata::MetadataKv::set_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
            &oversized,
            crate::internal::metadata::MetadataValueType::Text,
        )
        .await
        .expect("replace marker with oversized legacy evidence");

        let cleanup = tokio::time::timeout(
            Duration::from_secs(2),
            manager.cleanup_rejected_checkpoint_objects_until(&fence, None, &HashSet::new(), None),
        )
        .await
        .expect("oversized marker recovery must remain bounded");
        let error = cleanup.expect_err("oversized marker must defer short recovery");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<RejectedCheckpointCleanupDeferred>()),
            "oversized marker must retain its cleanup evidence for doctor: {error:#}"
        );

        let retained = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("reload deferred marker")
        .expect("deferred marker must remain present");
        assert_eq!(
            retained.value, oversized,
            "bounded recovery must leave oversized legacy evidence unchanged"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_defers_oversized_newly_written_set_before_vector_merge() {
        let (_dir, _database_path, db_conn, manager, fence) = deadline_contention_fixture().await;
        let original = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("load seeded marker")
        .expect("seeded marker exists")
        .value;
        let newly_written = (0..=TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES)
            .map(|index| {
                crate::internal::object_format::digest(
                    git_internal::hash::get_hash_kind(),
                    format!("bounded-recovery-candidate-{index}").as_bytes(),
                )
                .to_string()
            })
            .collect::<HashSet<_>>();
        assert_eq!(
            newly_written.len(),
            TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES + 1,
            "fixture must exceed the aggregate OID recovery bound"
        );

        let cleanup = tokio::time::timeout(
            Duration::from_secs(2),
            manager.cleanup_rejected_checkpoint_objects_until(&fence, None, &newly_written, None),
        )
        .await
        .expect("oversized ownership merge must remain bounded");
        let error = cleanup.expect_err("oversized ownership merge must defer recovery");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<RejectedCheckpointCleanupDeferred>()),
            "oversized ownership set must retain its marker for doctor: {error:#}"
        );

        let retained = crate::internal::metadata::MetadataKv::get_with_conn(
            &*db_conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &fence.session_id,
            &fence.attempt_id,
        )
        .await
        .expect("reload deferred marker")
        .expect("deferred marker must remain present");
        assert_eq!(
            retained.value, original,
            "bounded recovery must not materialize an oversized ownership set"
        );
    }

    #[tokio::test]
    async fn checkpoint_deadline_bounds_marker_and_ref_writer_acquisition() {
        let (_dir, database_path, db_conn, manager, fence) = deadline_contention_fixture().await;
        let original = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list original writer marker")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("seeded writer marker exists");
        let oid = crate::internal::object_format::parse_repo_oid(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .expect("parse deterministic marker oid");
        let locker = db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("deadline contention database path is utf-8"),
            Duration::from_millis(50),
        )
        .await
        .expect("open deadline contention lock holder");

        let lock = db::begin_write_transaction(&locker)
            .await
            .expect("acquire preclaim writer lock");
        let preclaim = tokio::time::timeout(
            Duration::from_secs(2),
            manager.persist_attempt_oid_before_write(
                &fence,
                None,
                &oid,
                Some(deadline_for_contention_test()),
            ),
        )
        .await
        .expect("deadline must bound marker preclaim writer acquisition")
        .expect_err("contended marker preclaim must stop at the capture deadline");
        lock.rollback().await.expect("release preclaim writer lock");
        assert!(
            format!("{preclaim:#}").contains("historical import execution deadline"),
            "unexpected bounded preclaim error: {preclaim:#}"
        );

        let lock = db::begin_write_transaction(&locker)
            .await
            .expect("acquire finalization writer lock");
        let finalization = tokio::time::timeout(
            Duration::from_secs(2),
            manager.finalize_attempt_oid_after_write(
                &fence,
                None,
                &oid,
                true,
                Some(deadline_for_contention_test()),
            ),
        )
        .await
        .expect("deadline must bound marker finalization writer acquisition")
        .expect_err("contended marker finalization must stop at the capture deadline");
        lock.rollback()
            .await
            .expect("release finalization writer lock");
        assert!(
            format!("{finalization:#}").contains("historical import execution deadline"),
            "unexpected bounded finalization error: {finalization:#}"
        );

        let lock = db::begin_write_transaction(&locker)
            .await
            .expect("acquire ref compare-and-swap writer lock");
        let ref_update = tokio::time::timeout(
            Duration::from_secs(2),
            manager.update_ref_if_matches_with_extra(
                crate::internal::branch::TRACES_BRANCH,
                None,
                oid,
                None,
                Some(deadline_for_contention_test()),
                None,
                None,
            ),
        )
        .await
        .expect("deadline must bound ref writer acquisition")
        .expect_err("contended ref compare-and-swap must stop at the capture deadline");
        lock.rollback()
            .await
            .expect("release ref compare-and-swap writer lock");
        assert!(
            format!("{ref_update:#}").contains("historical import execution deadline"),
            "unexpected bounded ref-update error: {ref_update:#}"
        );

        let retained = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list marker after bounded writer acquisitions")
            .into_iter()
            .find(|marker| {
                marker.session_id == fence.session_id && marker.attempt_id == fence.attempt_id
            })
            .expect("marker must survive before any writer transaction starts");
        assert_eq!(
            retained, original,
            "deadline-cancelled writer acquisition must not mutate recovery evidence"
        );
        assert!(
            manager
                .resolve_history_head()
                .await
                .expect("read traces head after bounded ref acquisition")
                .is_none(),
            "deadline-cancelled ref acquisition must not move the ref"
        );
    }

    #[tokio::test]
    async fn checkpoint_deadline_bounds_fence_and_head_reads_under_exclusive_lock() {
        let (_dir, database_path, _db_conn, manager, fence) = deadline_contention_fixture().await;
        let oid = crate::internal::object_format::parse_repo_oid(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .expect("parse deterministic ref oid");
        manager
            .update_ref(crate::internal::branch::TRACES_BRANCH, oid)
            .await
            .expect("seed history head before exclusive read lock");
        let locker = db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("deadline contention database path is utf-8"),
            Duration::from_millis(50),
        )
        .await
        .expect("open exclusive read lock holder");
        let backend = locker.get_database_backend();

        locker
            .execute_raw(Statement::from_string(backend, "BEGIN EXCLUSIVE"))
            .await
            .expect("acquire exclusive lock before marker read");
        let marker_read = tokio::time::timeout(
            Duration::from_secs(2),
            manager.load_traces_writer_fence(
                &fence.session_id,
                &fence.attempt_id,
                Some(deadline_for_contention_test()),
            ),
        )
        .await
        .expect("deadline must bound marker fence read")
        .expect_err("exclusive lock must stop marker fence read at capture deadline");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK"))
            .await
            .expect("release exclusive marker-read lock");
        assert!(
            format!("{marker_read:#}").contains("historical import execution deadline"),
            "unexpected bounded marker-read error: {marker_read:#}"
        );

        locker
            .execute_raw(Statement::from_string(backend, "BEGIN EXCLUSIVE"))
            .await
            .expect("acquire exclusive lock before head read");
        let head_read = tokio::time::timeout(
            Duration::from_secs(2),
            manager.resolve_history_head_until(Some(deadline_for_contention_test())),
        )
        .await
        .expect("deadline must bound history-head read")
        .expect_err("exclusive lock must stop history-head read at capture deadline");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK"))
            .await
            .expect("release exclusive head-read lock");
        assert!(
            format!("{head_read:#}").contains("historical import execution deadline"),
            "unexpected bounded history-head error: {head_read:#}"
        );
        assert_eq!(
            manager
                .resolve_history_head()
                .await
                .expect("read history head after exclusive lock release"),
            Some(oid),
            "a cancelled checkpoint read must not change the current ref"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_job_survives_an_unrelated_live_writer() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let manager = traces_manager(&dir, db_conn.clone());
        let active = TracesInflightMarker::new(
            "session-active",
            "attempt-active",
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(&*db_conn, &active)
            .await
            .expect("write unrelated active marker");

        let (oid, created) = write_git_object_with_status(
            &dir.path().join(".libra"),
            "blob",
            b"rejected-cleanup-candidate",
        )
        .expect("write cleanup candidate");
        assert!(created);
        let candidates = HashSet::from([oid.to_string()]);
        let rejected_fence =
            seed_test_writer_fence(&db_conn, "session-rejected", "attempt-rejected").await;
        manager
            .cleanup_rejected_checkpoint_objects(&rejected_fence, None, &candidates)
            .await
            .expect("live peer should defer, not discard, cleanup");
        let markers = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list durable cleanup markers");
        let pending = markers
            .iter()
            .find(|marker| marker.attempt_id == "attempt-rejected")
            .expect("cleanup job must remain durable");
        assert!(pending.cleanup_pending);
        assert_eq!(pending.created_oids, vec![oid.to_string()]);
        let object_path = dir
            .path()
            .join(".libra/objects")
            .join(&oid.to_string()[..2])
            .join(&oid.to_string()[2..]);
        assert!(object_path.exists(), "live peer must defer deletion");

        let mut expired = active;
        expired.started_at_ms = 0;
        expired.ttl_ms = 0;
        write_traces_inflight_marker(&*db_conn, &expired)
            .await
            .expect("expire unrelated marker");
        drain_rejected_cleanup_in_invocation_scope(&manager)
            .await
            .expect("drain persisted cleanup after live peer exits");
        assert!(
            object_path.exists(),
            "inline cleanup must leave shared object reclamation to repository GC"
        );
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list post-drain markers")
                .iter()
                .all(|marker| marker.attempt_id != "attempt-rejected"),
            "cleanup ownership marker survived successful drain"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_never_deletes_an_unresolved_preclaim() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let (oid, created) = write_git_object_with_status(
            &manager.repo_path,
            "blob",
            b"published by a concurrent writer",
        )
        .expect("write concurrent object");
        assert!(created);

        let mut marker = TracesInflightMarker::new("preclaim-session", "preclaim-attempt", 0);
        marker.ttl_ms = 0;
        marker.oids.push(oid.to_string());
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("write unresolved preclaim marker");

        drain_rejected_cleanup_in_invocation_scope(&manager)
            .await
            .expect("retire unresolved preclaim without deleting payload");
        let oid_text = oid.to_string();
        assert!(
            manager
                .repo_path
                .join("objects")
                .join(&oid_text[..2])
                .join(&oid_text[2..])
                .exists(),
            "an unresolved preclaim deleted an object that may belong to another writer"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_preserves_reflog_only_root() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let manager = traces_manager(&dir, db_conn.clone());
        let (candidate, created) =
            write_git_object_with_status(&manager.repo_path, "blob", b"reflog-only root")
                .expect("write reflog candidate");
        assert!(created);
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE reflog (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    ref_name TEXT NOT NULL, old_oid TEXT NOT NULL,
                    new_oid TEXT NOT NULL, timestamp INTEGER NOT NULL,
                    committer_name TEXT NOT NULL, committer_email TEXT NOT NULL,
                    action TEXT NOT NULL, message TEXT NOT NULL,
                    worktree_id TEXT
                 )"
                .to_string(),
            ))
            .await
            .expect("create reflog root table");
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO reflog (
                    ref_name, old_oid, new_oid, timestamp, committer_name,
                    committer_email, action, message, worktree_id
                 ) VALUES ('HEAD', ?, ?, 0, 'Libra', 'history@libra',
                           'test', 'protect candidate', NULL)",
                [
                    candidate.to_string().into(),
                    "0000000000000000000000000000000000000000".into(),
                ],
            ))
            .await
            .expect("seed reflog-only root");

        let reflog_fence =
            seed_test_writer_fence(&db_conn, "reflog-session", "reflog-attempt").await;
        manager
            .cleanup_rejected_checkpoint_objects(
                &reflog_fence,
                None,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("cleanup with reflog root");
        drain_rejected_cleanup_in_invocation_scope(&manager)
            .await
            .expect("drain reflog-root cleanup job");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list reflog cleanup markers")
                .iter()
                .all(|marker| marker.attempt_id != "reflog-attempt")
        );
        let candidate = candidate.to_string();
        assert!(
            manager
                .repo_path
                .join("objects")
                .join(&candidate[..2])
                .join(&candidate[2..])
                .exists(),
            "reflog-only candidate was deleted"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_preserves_worktree_index_only_root() {
        use git_internal::internal::index::{Index, IndexEntry};

        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        prepare_checkpoint_test_schema(&db_conn).await;
        let index_fence = seed_test_writer_fence(&db_conn, "index-session", "index-attempt").await;
        let manager = traces_manager(&dir, db_conn.clone());
        let (candidate, created) =
            write_git_object_with_status(&manager.repo_path, "blob", b"index-only root")
                .expect("write index candidate");
        assert!(created);
        let mut index = Index::new();
        index.add(IndexEntry::new_from_blob(
            "staged.txt".to_string(),
            candidate,
            15,
        ));
        index
            .save(manager.repo_path.join("index"))
            .expect("write worktree index");

        manager
            .cleanup_rejected_checkpoint_objects(
                &index_fence,
                None,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("cleanup with index root");
        drain_rejected_cleanup_in_invocation_scope(&manager)
            .await
            .expect("drain index-root cleanup job");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list index cleanup markers")
                .iter()
                .all(|marker| marker.attempt_id != "index-attempt")
        );
        let candidate = candidate.to_string();
        assert!(
            manager
                .repo_path
                .join("objects")
                .join(&candidate[..2])
                .join(&candidate[2..])
                .exists(),
            "index-only candidate was deleted"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_rejects_malformed_durable_object_ids_without_panicking() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO metadata_kv (
                    scope, target, `key`, value, value_type, created_at, updated_at
                 ) VALUES ('agent_traces_inflight', ?, ?, ?, 'text', 0, 0)",
                [
                    "damaged-session".into(),
                    "damaged-attempt".into(),
                    serde_json::json!({
                        "schema_version": 1,
                        "session_id": "damaged-session",
                        "attempt_id": "damaged-attempt",
                        "started_at_ms": 0,
                        "ttl_ms": 0,
                        "oids": ["a"],
                        "cleanup_pending": true,
                    })
                    .to_string()
                    .into(),
                ],
            ))
            .await
            .expect("seed malformed durable cleanup marker");

        let manager = traces_manager(&dir, db_conn);
        let error = drain_rejected_cleanup_in_invocation_scope(&manager)
            .await
            .expect_err("malformed cleanup marker must fail closed");
        let message = format!("{error:#}");
        assert!(message.contains("invalid object id"), "{message}");
        assert!(message.contains("libra agent doctor"), "{message}");
    }

    #[tokio::test]
    async fn expired_empty_marker_is_reaped_without_ref_traversal() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let marker = TracesInflightMarker::new("expired-session", "expired-attempt", 0);
        write_traces_inflight_marker(&*db_conn, &marker)
            .await
            .expect("seed expired empty marker");
        let manager = traces_manager(&dir, db_conn.clone());

        assert!(
            repair_expired_marker_in_invocation_scope(
                &manager,
                "expired-session",
                "expired-attempt",
                chrono::Utc::now().timestamp_millis(),
            )
            .await
            .expect("repair expired empty marker")
        );
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list markers after repair")
                .is_empty()
        );
    }

    #[tokio::test]
    async fn corrupt_unrelated_ref_does_not_block_nondestructive_rejected_cleanup() {
        use std::io::Write as _;

        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let repo_path = dir.path().join(".libra");
        let (candidate, _) = write_git_object_with_status(
            &repo_path,
            "blob",
            b"candidate held while a ref is corrupt",
        )
        .expect("write cleanup candidate");

        let corrupt_oid = git_object_hash("blob", b"expected bytes");
        let corrupt_text = corrupt_oid.to_string();
        let corrupt_path = repo_path
            .join("objects")
            .join(&corrupt_text[..2])
            .join(&corrupt_text[2..]);
        std::fs::create_dir_all(corrupt_path.parent().unwrap()).unwrap();
        let file = std::fs::File::create(&corrupt_path).unwrap();
        let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
        encoder.write_all(b"blob 15\0different bytes").unwrap();
        encoder.finish().unwrap();
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
                 VALUES ('broken-ref', 'Branch', ?, NULL, NULL)",
                [corrupt_text.into()],
            ))
            .await
            .expect("seed corrupt ref");

        let manager = traces_manager(&dir, db_conn.clone());
        let deferred_fence =
            seed_test_writer_fence(&db_conn, "deferred-session", "deferred-attempt").await;
        manager
            .cleanup_rejected_checkpoint_objects(
                &deferred_fence,
                None,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("ordinary append cleanup should defer a corrupt unrelated ref");
        let pending = list_all_traces_inflight_markers(&*db_conn)
            .await
            .expect("list deferred marker");
        assert!(
            pending.iter().any(|marker| {
                marker.attempt_id == "deferred-attempt" && marker.cleanup_pending
            })
        );
        assert!(
            repo_path
                .join("objects")
                .join(&candidate.to_string()[..2])
                .join(&candidate.to_string()[2..])
                .exists(),
            "fail-closed cleanup deleted a candidate"
        );
        drain_rejected_cleanup_in_invocation_scope(&manager)
            .await
            .expect("non-destructive marker retirement must not read an unrelated corrupt ref");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list markers after non-destructive cleanup")
                .is_empty(),
            "cleanup ownership marker survived successful retirement"
        );
        let candidate = candidate.to_string();
        assert!(
            repo_path
                .join("objects")
                .join(&candidate[..2])
                .join(&candidate[2..])
                .exists(),
            "non-destructive cleanup removed a rejected payload"
        );
    }

    struct SlowBoundedStorage;

    #[async_trait::async_trait]
    impl Storage for SlowBoundedStorage {
        async fn get(
            &self,
            _hash: &ObjectHash,
        ) -> std::result::Result<(Vec<u8>, ObjectType), git_internal::errors::GitError> {
            Err(git_internal::errors::GitError::InvalidObjectInfo(
                "unused slow storage read".to_string(),
            ))
        }

        async fn get_with_limit(
            &self,
            _hash: &ObjectHash,
            _limit: u64,
        ) -> std::result::Result<(Vec<u8>, ObjectType), git_internal::errors::GitError> {
            sleep(Duration::from_secs(5)).await;
            Err(git_internal::errors::GitError::InvalidObjectInfo(
                "slow storage read completed unexpectedly".to_string(),
            ))
        }

        async fn put(
            &self,
            hash: &ObjectHash,
            _data: &[u8],
            _obj_type: ObjectType,
        ) -> std::result::Result<String, git_internal::errors::GitError> {
            Ok(hash.to_string())
        }

        async fn exist(&self, _hash: &ObjectHash) -> bool {
            false
        }

        async fn search(&self, _prefix: &str) -> Vec<ObjectHash> {
            Vec::new()
        }
    }

    #[tokio::test]
    async fn rejected_reachability_read_is_bounded() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        let manager = traces_manager(&dir, db_conn.clone());
        let root = write_git_object(&manager.repo_path, "blob", &[b'x'; 128])
            .expect("write oversized ref root");
        let error = manager
            .reachable_rejected_objects_with_limit(vec![root], &HashSet::new(), 32)
            .await
            .expect_err("bounded reachability must reject an oversized root");
        assert!(
            format!("{error:#}").contains("exceeds preview limit of 32 bytes"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn rejected_reachability_deadline_interrupts_one_slow_storage_read() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new_with_ref(
            Arc::new(SlowBoundedStorage),
            dir.path().join(".libra"),
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        );
        let root = crate::internal::ai::util::parse_repo_object_id(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .expect("valid test oid");
        let started = Instant::now();
        let error = manager
            .reachable_rejected_objects_with_limits(
                vec![root],
                &HashSet::new(),
                1024,
                10,
                Instant::now() + Duration::from_millis(25),
            )
            .await
            .expect_err("slow individual read must honor the traversal deadline");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "one storage read escaped the traversal deadline"
        );
        assert!(
            format!("{error:#}").contains("traversal deadline"),
            "unexpected error: {error:#}"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn rejected_reachability_production_client_rejects_fifo_without_blocking() {
        use std::{ffi::CString, io::Write as _, os::unix::ffi::OsStrExt as _};

        use crate::utils::client_storage::ClientStorage;

        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        let objects_dir = repo_path.join("objects");
        let root = crate::internal::ai::util::parse_repo_object_id(
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391",
        )
        .expect("valid FIFO object id");
        let root_text = root.to_string();
        let shard = objects_dir.join(&root_text[..2]);
        std::fs::create_dir_all(&shard).expect("create FIFO object shard");
        let fifo = shard.join(&root_text[2..]);
        let fifo_name = CString::new(fifo.as_os_str().as_bytes()).expect("FIFO path has no NUL");
        // SAFETY: fifo_name is NUL-terminated and points to a path owned by
        // this test's temporary directory.
        assert_eq!(unsafe { libc::mkfifo(fifo_name.as_ptr(), 0o600) }, 0);

        // Release the intentionally blocked local read after the deadline
        // assertion. This lets Tokio's cancelled spawn_blocking task finish so
        // the test runtime can shut down cleanly.
        let release_fifo = fifo.clone();
        let release = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(250));
            let mut writer = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(release_fifo)
                .expect("open self-held FIFO writer after reader blocks or rejects the FIFO");
            writer.write_all(b"not-zlib").expect("release FIFO reader");
        });
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new_with_ref(
            Arc::new(ClientStorage::init(objects_dir)),
            repo_path,
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        );
        let started = Instant::now();
        let error = manager
            .reachable_rejected_objects_with_limits(
                vec![root],
                &HashSet::new(),
                1024,
                10,
                Instant::now() + Duration::from_millis(25),
            )
            .await
            .expect_err("production ClientStorage must reject a non-regular loose object");
        let elapsed = started.elapsed();
        release.join().expect("join FIFO release writer");
        assert!(
            elapsed < Duration::from_millis(150),
            "ClientStorage blocked while rejecting a FIFO for {elapsed:?}"
        );
        assert!(
            format!("{error:#}").contains("is not a regular file"),
            "unexpected error: {error:#}"
        );
    }

    #[tokio::test]
    async fn rejected_reachability_reads_bounded_objects_from_alternates() {
        let dir = tempdir().unwrap();
        let repo_path = dir.path().join(".libra");
        let objects_dir = repo_path.join("objects");
        std::fs::create_dir_all(objects_dir.join("info")).unwrap();

        let alternate_repo = dir.path().join("alternate");
        std::fs::create_dir_all(&alternate_repo).unwrap();
        let root = write_git_object(&alternate_repo, "blob", b"alternate root")
            .expect("write alternate-only ref root");
        let alternate_objects = alternate_repo.join("objects");
        std::fs::write(
            objects_dir.join("info/alternates"),
            format!("{}\n", alternate_objects.display()),
        )
        .expect("configure alternate object store");

        let storage = Arc::new(LocalStorage::new_with_alternates(objects_dir));
        let db_conn = Arc::new(setup_test_db().await);
        let manager = HistoryManager::new_with_ref(
            storage,
            repo_path,
            db_conn,
            crate::internal::branch::TRACES_BRANCH,
        );
        let reachable = manager
            .reachable_rejected_objects_with_limit(
                vec![root],
                &HashSet::from([root.to_string()]),
                1024,
            )
            .await
            .expect("bounded alternate read should prove reachability");
        assert_eq!(reachable, HashSet::from([root.to_string()]));
    }

    #[tokio::test]
    async fn rejected_reachability_total_work_is_bounded() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        let manager = traces_manager(&dir, db_conn.clone());
        let first = write_git_object(&manager.repo_path, "blob", b"first").unwrap();
        let second = write_git_object(&manager.repo_path, "blob", b"second").unwrap();

        let count_error = manager
            .reachable_rejected_objects_with_limits(
                vec![first, second],
                &HashSet::new(),
                1024,
                1,
                Instant::now() + Duration::from_secs(5),
            )
            .await
            .expect_err("visited-object cap must fail closed");
        assert!(
            format!("{count_error:#}").contains("1 object traversal limit"),
            "unexpected error: {count_error:#}"
        );

        let deadline_error = manager
            .reachable_rejected_objects_with_limits(
                vec![first],
                &HashSet::new(),
                1024,
                10,
                Instant::now(),
            )
            .await
            .expect_err("expired traversal deadline must fail closed");
        assert!(
            format!("{deadline_error:#}").contains("traversal deadline"),
            "unexpected error: {deadline_error:#}"
        );
    }

    #[tokio::test]
    async fn rejected_cleanup_preserves_candidates_reachable_only_from_annotated_tag() {
        let dir = tempdir().unwrap();
        let db_conn = Arc::new(setup_test_db().await);
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                include_str!("../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
            ))
            .await
            .expect("create marker registry");
        db_conn
            .execute_raw(Statement::from_string(
                db_conn.get_database_backend(),
                "CREATE TABLE agent_checkpoint (checkpoint_id TEXT PRIMARY KEY)".to_string(),
            ))
            .await
            .expect("create cleanup catalog probe");
        let repo_path = dir.path().join(".libra");
        let (candidate, created) = write_git_object_with_status(
            &repo_path,
            "blob",
            b"candidate preserved by annotated tag",
        )
        .expect("write tagged cleanup candidate");
        assert!(created);
        let tag_data = format!(
            "object {candidate}\ntype blob\ntag keep-candidate\ntagger Libra <history@libra> 0 +0000\n\nkeep\n"
        );
        let tag = write_git_object(&repo_path, "tag", tag_data.as_bytes())
            .expect("write annotated tag object");
        db_conn
            .execute_raw(Statement::from_sql_and_values(
                db_conn.get_database_backend(),
                "INSERT INTO reference (name, kind, `commit`, remote, worktree_id)
                 VALUES ('keep-candidate', 'Tag', ?, NULL, NULL)",
                [tag.to_string().into()],
            ))
            .await
            .expect("seed annotated tag ref");

        let tag_fence = seed_test_writer_fence(&db_conn, "tag-session", "tag-attempt").await;
        let manager = traces_manager(&dir, db_conn.clone());
        manager
            .cleanup_rejected_checkpoint_objects(
                &tag_fence,
                None,
                &HashSet::from([candidate.to_string()]),
            )
            .await
            .expect("annotated tag reachability should protect candidate");
        drain_rejected_cleanup_in_invocation_scope(&manager)
            .await
            .expect("drain annotated-tag cleanup job");
        assert!(
            list_all_traces_inflight_markers(&*db_conn)
                .await
                .expect("list annotated-tag cleanup markers")
                .iter()
                .all(|marker| marker.attempt_id != "tag-attempt")
        );
        let candidate_string = candidate.to_string();
        assert!(
            repo_path
                .join("objects")
                .join(&candidate_string[..2])
                .join(&candidate_string[2..])
                .exists(),
            "candidate reachable only through an annotated tag was deleted"
        );
    }

    /// SW-01 (M-FMT F6, plan issues/490): the held-index cleanup parser accepts
    /// a version-3 index with CE_SKIP_WORKTREE and extracts its roots through
    /// the git-internal extended-flag decoder (no second state machine).
    #[test]
    fn parse_cleanup_index_roots_accepts_v3_extended_flags() {
        use git_internal::{
            hash::{HashKind, ObjectHash, set_hash_kind_for_test},
            internal::index::{Index as GitIndex, IndexEntry},
        };

        let _guard = set_hash_kind_for_test(HashKind::Sha1);
        let dir = tempdir().unwrap();
        let path = dir.path().join("held-index");
        let mut index = GitIndex::new();
        let oid =
            ObjectHash::from_bytes_for_kind(git_internal::hash::get_hash_kind(), &[0x31u8; 20])
                .unwrap();
        let mut entry = IndexEntry::new_from_blob("skip.txt".to_string(), oid, 3);
        entry.flags.skip_worktree = true;
        index.update(entry);
        index.to_file(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        let roots = super::parse_cleanup_index_roots(&bytes, 20, "held index")
            .expect("v3 index with skip-worktree must parse");
        let expected: std::collections::HashSet<String> =
            std::iter::once(oid.to_string()).collect();
        assert_eq!(roots, expected);

        // An unknown extended bit still fails closed.
        let mut patched = bytes.clone();
        let word_offset = patched.len() - 20 - 3 - 2 - 2; // checksum + name + padding + ext word guess
        // Locate the extended word robustly: flags at 12+40+20, extended at +2.
        let ext = 12 + 40 + 20 + 2;
        let _ = word_offset;
        patched[ext] = 0x10;
        patched[ext + 1] = 0x00;
        let mut hasher = git_internal::utils::HashAlgorithm::new_for_kind(HashKind::Sha1);
        hasher.update(&patched[..patched.len() - 20]);
        let checksum = hasher.finalize_object_hash();
        patched.truncate(patched.len() - 20);
        patched.extend_from_slice(checksum.as_ref());
        assert!(
            super::parse_cleanup_index_roots(&patched, 20, "held index").is_err(),
            "unknown extended bits must fail closed"
        );
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn history_cleanup_index_roots_blake3_checksum() {
        use git_internal::{
            hash::{HashKind, ObjectHash, set_hash_kind_for_test},
            internal::index::{Index as GitIndex, IndexEntry},
        };

        let _guard = set_hash_kind_for_test(HashKind::Blake3);
        let dir = tempdir().unwrap();
        let path = dir.path().join("held-index-blake3");
        let mut index = GitIndex::new();
        let oid = ObjectHash::from_bytes_for_kind(HashKind::Blake3, &[0x42u8; 32]).unwrap();
        let entry = IndexEntry::new_from_blob("blake3.txt".to_string(), oid, 3);
        index.update(entry);
        index.to_file(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();

        let roots = super::parse_cleanup_index_roots(&bytes, 32, "held blake3 index")
            .expect("blake3 index checksum must validate via helper digest");
        let expected: std::collections::HashSet<String> =
            std::iter::once(oid.to_string()).collect();
        assert_eq!(roots, expected);

        // Flip a checksum byte → fail closed.
        let mut patched = bytes.clone();
        let last = patched.len() - 1;
        patched[last] ^= 0xff;
        assert!(
            super::parse_cleanup_index_roots(&patched, 32, "held blake3 index").is_err(),
            "corrupt blake3 index checksum must fail"
        );
    }
}
