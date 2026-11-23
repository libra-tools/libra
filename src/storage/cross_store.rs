//! Cross-store durable intent outbox, mandatory write-before-advance replay,
//! and three-step read-only consistent snapshot coordinator (`ADR-RB-06`,
//! `RB-06b`).
//!
//! Coordinates atomic logical commits spanning `git.redb` ([`RedbGitStore`])
//! and `capture.redb` ([`RedbCaptureStore`]):
//!
//! 1. **Lock order (`GC-RB-04`):** Cross-store writers always acquire the
//!    `git.redb` write transaction *before* acquiring the `capture.redb` write
//!    transaction.
//! 2. **Write-before-advance mandatory reconciliation (`ADR-RB-06` #1):** Before
//!    allocating `next_gen = git_gen + 1`, the coordinator checks
//!    `git.META_GENERATION` against `capture.META_GENERATION`. If `capture.redb`
//!    is unreachable, corrupt, or diverged (`cap_gen > git_gen`), the writer
//!    immediately fails closed (`LBR-IO-001`) without advancing `git.redb`. If
//!    `cap_gen < git_gen`, every pending intent in `(cap_gen + 1)..=git_gen` is
//!    replayed in strictly ascending generation order into `capture.redb` first
//!    (never masking a gap with `max + 1`).
//! 3. **Two-step durable commit:**
//!    - Step 1: In one `git.redb` write transaction, apply the Git mutation,
//!      set `META_GENERATION = git_gen + 1`, and persist
//!      `META_CROSS_STORE_INTENT[next_gen]` in versioned postcard format.
//!    - Step 2: In one `capture.redb` write transaction, idempotently apply the
//!      intent's [`CaptureBatchItem`] payloads keyed on semantic `dedup_key`
//!      and advance `capture.META_GENERATION = next_gen`.
//! 4. **Three-step read-only consistent snapshot (`ADR-RB-06` #3):** Open
//!    `git_rtx1` (`g1`) → open `capture_rtx` (`c1`) → open short `git_rtx2`
//!    (`g2`). Freeze only when `g1 == c1 && g1 == g2`; otherwise retry up to
//!    [`CROSS_STORE_SNAPSHOT_MAX_RETRIES`] (`5`) times and fail closed with
//!    `LBR-IO-001` (never upgrading to a writable handle).

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU8, Ordering},
    },
    time::Duration,
};

use redb::ReadTransaction;
use serde::{Deserialize, Serialize};

use crate::storage::{
    capture_store::{
        AgentSessionRecord, CaptureBatchItem, CaptureStore, RedbCaptureStore,
        validate_capture_batch_item,
    },
    codec::{decode_versioned, encode_versioned},
    error::{StorageBackendError, StorageResult},
    git_store::{
        BridgePublicationRecord, RedbGitStore, RefCasOutcome, RefKey, RefTarget, ReflogWrite,
    },
    opener::RedbRepairContext,
    redb::map_redb_commit_err,
};

/// Current postcard codec version for [`CrossStoreIntentRecord`] entries stored
/// in `git.redb`'s `META_CROSS_STORE_INTENT` table.
pub const CROSS_STORE_INTENT_CODEC_VERSION: u16 = 1;

/// Maximum number of retries performed by [`CrossStoreCoordinator::snapshot_consistent_read_only`]
/// before failing closed with `LBR-IO-001` (`ADR-RB-06` #3).
pub const CROSS_STORE_SNAPSHOT_MAX_RETRIES: usize = 5;

/// Default delay between read-only snapshot consistency retries (`5ms`).
pub const DEFAULT_SNAPSHOT_RETRY_BACKOFF: Duration = Duration::from_millis(5);

/// Stable error prefix when a three-step read-only snapshot exhausts retries
/// due to generation divergence (`g1 != c1` or `g1 != g2`).
pub const CROSS_STORE_INCONSISTENT_SNAPSHOT_PREFIX: &str = "cross-store consistent snapshot failed";

/// Stable error prefix when a cross-store writer cannot reach or validate
/// `capture.redb` and refuses to advance `git.redb`'s generation.
pub const CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX: &str = "cross-store capture store unavailable";

/// Returns `true` when `err` indicates a fail-closed read-only snapshot
/// generation mismatch (`ADR-RB-06` #3).
#[must_use]
pub fn is_cross_store_inconsistent_snapshot_error(err: &StorageBackendError) -> bool {
    matches!(
        err,
        StorageBackendError::BackendInternal { detail }
            if detail.starts_with(CROSS_STORE_INCONSISTENT_SNAPSHOT_PREFIX)
    )
}

/// Returns `true` when `err` indicates `capture.redb` was unreachable, corrupt,
/// or rejected pre-advance reconciliation (`RB-06b` AC4).
#[must_use]
pub fn is_cross_store_capture_unavailable_error(err: &StorageBackendError) -> bool {
    matches!(
        err,
        StorageBackendError::BackendInternal { detail }
            if detail.starts_with(CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX)
    )
}

/// Durable outbox payload persisted under `META_CROSS_STORE_INTENT[generation]`
/// inside `git.redb` (`ADR-RB-06` #1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CrossStoreIntentRecord {
    /// Monotonic cross-store generation (`git_gen + 1`).
    pub generation: u64,
    /// Idempotent capture catalog mutations to apply in `capture.redb`.
    pub items: Vec<CaptureBatchItem>,
}

/// Git reference CAS + optional reflog + optional `agent_bridge_publication`
/// mutation executed atomically in Step 1 of a cross-store commit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossStoreRefCasOp {
    pub key: RefKey,
    pub expected: Option<RefTarget>,
    pub new_target: Option<RefTarget>,
    pub reflogs: Vec<ReflogWrite>,
    pub publication: Option<BridgePublicationRecord>,
}

/// Result of a cross-store durable commit (`ADR-RB-06` #1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossStoreCommitOutcome {
    /// Final cross-store generation committed (or unchanged `git_gen` on CAS conflict).
    pub generation: u64,
    /// Outcome of the Git reference CAS inside `git.redb`.
    pub ref_outcome: RefCasOutcome,
    /// Pending outbox generations replayed into `capture.redb` in ascending
    /// order before allocating `generation`.
    pub replayed_generations: Vec<u64>,
    /// Number of newly applied capture batch items in Step 2.
    pub capture_applied: usize,
    /// Number of capture batch items already deduplicated in Step 2.
    pub capture_deduplicated: usize,
}

/// Frozen read-only MVCC snapshot spanning both `git.redb` and `capture.redb`
/// verified via the three-step `g1 == c1 && g1 == g2` protocol (`ADR-RB-06` #3).
pub struct CrossStoreConsistentSnapshot {
    generation: u64,
    attempts: usize,
    git_rtx: ReadTransaction,
    capture_rtx: ReadTransaction,
}

impl std::fmt::Debug for CrossStoreConsistentSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CrossStoreConsistentSnapshot")
            .field("generation", &self.generation)
            .field("attempts", &self.attempts)
            .finish_non_exhaustive()
    }
}

impl CrossStoreConsistentSnapshot {
    /// Consistent cross-store generation satisfied by `g1 == c1 == g2`.
    #[must_use]
    pub const fn generation(&self) -> u64 {
        self.generation
    }

    /// Number of three-step verification probes performed (`1` on clean match).
    #[must_use]
    pub const fn attempts(&self) -> usize {
        self.attempts
    }

    /// Borrow the frozen read-only `git.redb` MVCC transaction.
    #[must_use]
    pub const fn git_rtx(&self) -> &ReadTransaction {
        &self.git_rtx
    }

    /// Borrow the frozen read-only `capture.redb` MVCC transaction.
    #[must_use]
    pub const fn capture_rtx(&self) -> &ReadTransaction {
        &self.capture_rtx
    }

    /// Read a Git reference from the frozen consistent `git.redb` snapshot.
    pub fn get_git_ref(&self, key: &RefKey) -> StorageResult<Option<RefTarget>> {
        RedbGitStore::read_ref_in_rtx(&self.git_rtx, key)
    }

    /// Read an `agent_session` from the frozen consistent `capture.redb` snapshot.
    pub fn get_capture_session(
        &self,
        session_id: &str,
    ) -> StorageResult<Option<AgentSessionRecord>> {
        RedbCaptureStore::read_session_in_rtx(&self.capture_rtx, session_id)
    }
}

const CANCEL_STATE_ACTIVE: u8 = 0;
const CANCEL_BIT_CANCELLED: u8 = 0b001;
const CANCEL_BIT_IN_REPLAY_COMMIT: u8 = 0b010;
const CANCEL_BIT_IN_FINAL_COMMIT: u8 = 0b100;

/// Cooperative cancellation guard attached to async cross-store commits.
///
/// If the caller drops or aborts the async future before a durable commit
/// boundary (either pre-advance replay commit or Step 1 `git.redb` commit),
/// [`Drop`] sets [`CANCEL_BIT_CANCELLED`] without blocking so the worker
/// rolls back uncommitted transactions instead of mutating either store in the
/// background.
struct CrossStoreCancelGuard {
    state: Arc<AtomicU8>,
    completed: bool,
}

impl CrossStoreCancelGuard {
    fn new(state: Arc<AtomicU8>) -> Self {
        Self {
            state,
            completed: false,
        }
    }

    fn disarm(&mut self) {
        self.completed = true;
    }
}

impl Drop for CrossStoreCancelGuard {
    fn drop(&mut self) {
        if !self.completed {
            self.state.fetch_or(CANCEL_BIT_CANCELLED, Ordering::AcqRel);
        }
    }
}

fn check_not_cancelled(cancel_state: &AtomicU8) -> StorageResult<()> {
    if (cancel_state.load(Ordering::Acquire) & CANCEL_BIT_CANCELLED) != 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: "cross-store commit cancelled before commit".to_string(),
        });
    }
    Ok(())
}

/// RAII scope guarding the pre-advance pending-intent replay commit boundary.
///
/// Acquires [`CANCEL_BIT_IN_REPLAY_COMMIT`] only if [`CANCEL_BIT_CANCELLED`] is
/// not set; clears [`CANCEL_BIT_IN_REPLAY_COMMIT`] on drop while preserving any
/// [`CANCEL_BIT_CANCELLED`] bit set concurrently during replay commit so the
/// subsequent Step 1 `git.redb` commit still aborts cleanly.
struct ReplayCommitBoundaryGuard<'a> {
    state: &'a AtomicU8,
}

impl<'a> ReplayCommitBoundaryGuard<'a> {
    fn enter(state: &'a AtomicU8) -> StorageResult<Self> {
        match state.compare_exchange(
            CANCEL_STATE_ACTIVE,
            CANCEL_BIT_IN_REPLAY_COMMIT,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => Ok(Self { state }),
            Err(_) => Err(StorageBackendError::BackendInternal {
                detail: "cross-store commit cancelled before pending-intent replay commit"
                    .to_string(),
            }),
        }
    }
}

impl Drop for ReplayCommitBoundaryGuard<'_> {
    fn drop(&mut self) {
        self.state
            .fetch_and(!CANCEL_BIT_IN_REPLAY_COMMIT, Ordering::AcqRel);
    }
}

fn enter_commit_boundary(cancel_state: &AtomicU8) -> StorageResult<()> {
    match cancel_state.compare_exchange(
        CANCEL_STATE_ACTIVE,
        CANCEL_BIT_IN_FINAL_COMMIT,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => Ok(()),
        Err(_) => Err(StorageBackendError::BackendInternal {
            detail: "cross-store commit cancelled before commit".to_string(),
        }),
    }
}

#[cfg(test)]
type BlockingLifecycleCallback = Arc<dyn Fn() + Send + Sync>;

#[cfg(test)]
type AfterGitWtxCallback = Arc<dyn Fn(u64) + Send + Sync>;

#[cfg(test)]
type AfterGitCommitCallback = Arc<dyn Fn(u64) -> Option<StorageBackendError> + Send + Sync>;

/// Test-only fault/synchronization hooks for deterministic boundary testing.
#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct CrossStoreCommitHook {
    pub(crate) on_blocking_enter: Option<BlockingLifecycleCallback>,
    pub(crate) on_blocking_exit: Option<BlockingLifecycleCallback>,
    pub(crate) after_git_wtx_acquired: Option<AfterGitWtxCallback>,
    pub(crate) on_inside_replay_commit_boundary: Option<BlockingLifecycleCallback>,
    pub(crate) after_git_commit: Option<AfterGitCommitCallback>,
}

#[cfg(test)]
impl CrossStoreCommitHook {
    pub(crate) fn none() -> Self {
        Self::default()
    }

    pub(crate) fn fail_after_git_commit() -> Self {
        Self {
            after_git_commit: Some(Arc::new(|next_gen| {
                Some(StorageBackendError::BackendInternal {
                    detail: format!(
                        "injected crash after git.redb commit at generation {next_gen} before capture.redb commit"
                    ),
                })
            })),
            ..Self::default()
        }
    }

    fn fire_blocking_enter(&self) {
        if let Some(cb) = &self.on_blocking_enter {
            cb();
        }
    }

    fn fire_blocking_exit(&self) {
        if let Some(cb) = &self.on_blocking_exit {
            cb();
        }
    }

    fn on_after_git_wtx_acquired(&self, git_gen: u64) {
        if let Some(cb) = &self.after_git_wtx_acquired {
            cb(git_gen);
        }
    }

    fn fire_inside_replay_commit_boundary(&self) {
        if let Some(cb) = &self.on_inside_replay_commit_boundary {
            cb();
        }
    }

    fn on_after_git_commit(&self, next_gen: u64) -> Option<StorageBackendError> {
        self.after_git_commit.as_ref().and_then(|cb| cb(next_gen))
    }
}

/// Test-only hook invoked inside [`snapshot_consistent_read_only_with_hook`]
/// right after reading `capture_rtx` (`c1`) and before opening `git_rtx2` (`g2`),
/// allowing deterministic testing of `g1 == c1 && g1 != g2` interleavings.
#[cfg(test)]
pub(crate) type SnapshotBetweenC1AndG2Hook = std::sync::Arc<dyn Fn(usize, u64, u64) + Send + Sync>;

#[derive(Debug, Clone)]
enum CaptureEndpoint {
    Handle(RedbCaptureStore),
    Path {
        path: PathBuf,
        context: RedbRepairContext,
    },
}

/// Cross-store coordinator enforcing `ADR-RB-06` durable outbox commits,
/// write-before-advance ascending replay, unreachable-capture refusal, and
/// three-step read-only consistent snapshots.
#[derive(Debug, Clone)]
pub struct CrossStoreCoordinator {
    git: RedbGitStore,
    capture: CaptureEndpoint,
    snapshot_max_retries: usize,
    snapshot_retry_backoff: Duration,
}

impl CrossStoreCoordinator {
    /// Construct a coordinator bound to live [`RedbGitStore`] and
    /// [`RedbCaptureStore`] handles.
    #[must_use]
    pub fn new(git: RedbGitStore, capture: RedbCaptureStore) -> Self {
        Self {
            git,
            capture: CaptureEndpoint::Handle(capture),
            snapshot_max_retries: CROSS_STORE_SNAPSHOT_MAX_RETRIES,
            snapshot_retry_backoff: DEFAULT_SNAPSHOT_RETRY_BACKOFF,
        }
    }

    /// Construct a coordinator bound to a [`RedbGitStore`] and a `capture.redb`
    /// filesystem path opened on demand (`open_read_write` for writers,
    /// `open_read_only` for read-only snapshots — never upgrading readers).
    #[must_use]
    pub fn with_capture_path(
        git: RedbGitStore,
        capture_path: impl Into<PathBuf>,
        context: RedbRepairContext,
    ) -> Self {
        Self {
            git,
            capture: CaptureEndpoint::Path {
                path: capture_path.into(),
                context,
            },
            snapshot_max_retries: CROSS_STORE_SNAPSHOT_MAX_RETRIES,
            snapshot_retry_backoff: DEFAULT_SNAPSHOT_RETRY_BACKOFF,
        }
    }

    /// Override the retry backoff used by [`Self::snapshot_consistent_read_only`].
    #[must_use]
    pub const fn with_snapshot_retry_backoff(mut self, backoff: Duration) -> Self {
        self.snapshot_retry_backoff = backoff;
        self
    }

    /// Borrow the underlying [`RedbGitStore`].
    #[must_use]
    pub const fn git_store(&self) -> &RedbGitStore {
        &self.git
    }

    async fn resolve_capture_for_write(&self) -> StorageResult<RedbCaptureStore> {
        match &self.capture {
            CaptureEndpoint::Handle(store) => {
                if store.is_read_only() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: capture store handle is read-only; refusing to advance git generation"
                        ),
                    });
                }
                Ok(store.clone())
            }
            CaptureEndpoint::Path { path, context } => {
                RedbCaptureStore::open_read_write(path, *context)
                    .await
                    .map_err(|err| StorageBackendError::BackendInternal {
                        detail: format!(
                            "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: unable to open writable capture store ({}): refusing to advance git generation",
                            err.stable_code_str()
                        ),
                    })
            }
        }
    }

    async fn resolve_capture_for_read_only(&self) -> StorageResult<RedbCaptureStore> {
        match &self.capture {
            CaptureEndpoint::Handle(store) => Ok(store.clone()),
            CaptureEndpoint::Path { path, .. } => {
                RedbCaptureStore::open_read_only(path)
                    .await
                    .map_err(|err| StorageBackendError::BackendInternal {
                        detail: format!(
                            "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: unable to open read-only capture store ({})",
                            err.stable_code_str()
                        ),
                    })
            }
        }
    }

    /// Execute a durable two-step cross-store commit (`ADR-RB-06` #1):
    ///
    /// 1. Validates all capture items up front and opens/verifies `capture.redb`.
    /// 2. Acquires `git.redb` write lock FIRST, then `capture.redb` write lock
    ///    (`GC-RB-04` lock order). If `capture.redb` is unreachable, corrupt, or
    ///    has `cap_gen > git_gen`, immediately fails closed without advancing
    ///    `git.redb`.
    /// 3. If `cap_gen < git_gen`, replays all pending `META_CROSS_STORE_INTENT`
    ///    entries in strictly ascending generation order `(cap_gen + 1)..=git_gen`
    ///    into `capture.redb` and commits `capture.redb` BEFORE allocating
    ///    `next_gen = git_gen + 1`.
    /// 4. Applies `git_op` inside `git_wtx`, stages and validates all
    ///    `capture_items` inside the exclusively held `cap_wtx` (preventing any
    ///    invalid capture mutation from poisoning `META_CROSS_STORE_INTENT`),
    ///    and commits Step 1 (`git.redb`: Git mutation + `META_GENERATION` +
    ///    `META_CROSS_STORE_INTENT[next_gen]`).
    /// 5. Commits Step 2 (`capture.redb`: idempotent capture mutations +
    ///    matching `META_GENERATION = next_gen`).
    pub async fn commit_ref_cas_with_capture(
        &self,
        git_op: &CrossStoreRefCasOp,
        capture_items: &[CaptureBatchItem],
    ) -> StorageResult<CrossStoreCommitOutcome> {
        #[cfg(test)]
        {
            self.commit_ref_cas_with_capture_inner(
                git_op,
                capture_items,
                CrossStoreCommitHook::none(),
            )
            .await
        }
        #[cfg(not(test))]
        {
            self.commit_ref_cas_with_capture_impl(git_op, capture_items)
                .await
        }
    }

    #[cfg(test)]
    pub(crate) async fn commit_ref_cas_with_capture_inner(
        &self,
        git_op: &CrossStoreRefCasOp,
        capture_items: &[CaptureBatchItem],
        hook: CrossStoreCommitHook,
    ) -> StorageResult<CrossStoreCommitOutcome> {
        for item in capture_items {
            validate_capture_batch_item(item)?;
        }
        let capture = self.resolve_capture_for_write().await?;
        let git = self.git.clone();
        let git_op = git_op.clone();
        let items = capture_items.to_vec();
        let cancel_state = Arc::new(AtomicU8::new(CANCEL_STATE_ACTIVE));
        let mut cancel_guard = CrossStoreCancelGuard::new(Arc::clone(&cancel_state));

        let res = tokio::task::spawn_blocking(move || {
            let outcome = commit_cross_store_blocking(
                &git,
                &capture,
                &git_op,
                &items,
                cancel_state.as_ref(),
                &hook,
            );
            hook.fire_blocking_exit();
            outcome
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("cross-store commit task failed: {err}"),
        })?;
        cancel_guard.disarm();
        res
    }

    #[cfg(not(test))]
    async fn commit_ref_cas_with_capture_impl(
        &self,
        git_op: &CrossStoreRefCasOp,
        capture_items: &[CaptureBatchItem],
    ) -> StorageResult<CrossStoreCommitOutcome> {
        for item in capture_items {
            validate_capture_batch_item(item)?;
        }
        let capture = self.resolve_capture_for_write().await?;
        let git = self.git.clone();
        let git_op = git_op.clone();
        let items = capture_items.to_vec();
        let cancel_state = Arc::new(AtomicU8::new(CANCEL_STATE_ACTIVE));
        let mut cancel_guard = CrossStoreCancelGuard::new(Arc::clone(&cancel_state));

        let res = tokio::task::spawn_blocking(move || {
            commit_cross_store_blocking(&git, &capture, &git_op, &items, cancel_state.as_ref())
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("cross-store commit task failed: {err}"),
        })?;
        cancel_guard.disarm();
        res
    }

    /// Acquire a frozen read-only three-step consistent snapshot spanning both
    /// `git.redb` and `capture.redb` (`ADR-RB-06` #3):
    ///
    /// Opens `git_rtx1` (`g1`) → `capture_rtx` (`c1`) → short `git_rtx2` (`g2`).
    /// Returns [`CrossStoreConsistentSnapshot`] only when `g1 == c1 && g1 == g2`.
    /// Retries up to [`CROSS_STORE_SNAPSHOT_MAX_RETRIES`] (`5`) times, then fails
    /// closed with `LBR-IO-001` advising `libra agent doctor --repair`. Never
    /// upgrades to a writable handle or mutates disk.
    pub async fn snapshot_consistent_read_only(
        &self,
    ) -> StorageResult<CrossStoreConsistentSnapshot> {
        let capture = self.resolve_capture_for_read_only().await?;
        let git = self.git.clone();
        let max_retries = self.snapshot_max_retries;
        let backoff = self.snapshot_retry_backoff;

        tokio::task::spawn_blocking(move || {
            snapshot_consistent_read_only_blocking(&git, &capture, max_retries, backoff)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("cross-store snapshot task failed: {err}"),
        })?
    }

    #[cfg(test)]
    pub(crate) async fn snapshot_consistent_read_only_with_hook(
        &self,
        between_c1_and_g2: SnapshotBetweenC1AndG2Hook,
    ) -> StorageResult<CrossStoreConsistentSnapshot> {
        let capture = self.resolve_capture_for_read_only().await?;
        let git = self.git.clone();
        let max_retries = self.snapshot_max_retries;
        let backoff = self.snapshot_retry_backoff;

        tokio::task::spawn_blocking(move || {
            snapshot_consistent_read_only_blocking_inner(
                &git,
                &capture,
                max_retries,
                backoff,
                Some(&between_c1_and_g2),
            )
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("cross-store snapshot task failed: {err}"),
        })?
    }
}

/// Strictly read-only three-step snapshot verification (`g1 == c1 && g1 == g2`)
/// with bounded retry (`ADR-RB-06` #3).
pub fn snapshot_consistent_read_only_blocking(
    git: &RedbGitStore,
    capture: &RedbCaptureStore,
    max_retries: usize,
    retry_backoff: Duration,
) -> StorageResult<CrossStoreConsistentSnapshot> {
    snapshot_consistent_read_only_blocking_inner(
        git,
        capture,
        max_retries,
        retry_backoff,
        #[cfg(test)]
        None,
    )
}

fn snapshot_consistent_read_only_blocking_inner(
    git: &RedbGitStore,
    capture: &RedbCaptureStore,
    max_retries: usize,
    retry_backoff: Duration,
    #[cfg(test)] between_c1_and_g2: Option<&SnapshotBetweenC1AndG2Hook>,
) -> StorageResult<CrossStoreConsistentSnapshot> {
    let mut last_g1 = 0u64;
    let mut last_c1 = 0u64;
    let mut last_g2 = 0u64;

    for attempt in 0..=max_retries {
        let git_rtx1 = git.begin_checked_read_tx()?;
        let g1 = RedbGitStore::read_cross_store_generation_in_rtx(&git_rtx1)?;

        let cap_rtx = capture.begin_checked_read_tx()?;
        let c1 = RedbCaptureStore::read_cross_store_generation_in_rtx(&cap_rtx)?;

        #[cfg(test)]
        if let Some(hook) = between_c1_and_g2 {
            hook(attempt, g1, c1);
        }

        let git_rtx2 = git.begin_checked_read_tx()?;
        let g2 = RedbGitStore::read_cross_store_generation_in_rtx(&git_rtx2)?;
        drop(git_rtx2);

        if g1 == c1 && g1 == g2 {
            return Ok(CrossStoreConsistentSnapshot {
                generation: g1,
                attempts: attempt + 1,
                git_rtx: git_rtx1,
                capture_rtx: cap_rtx,
            });
        }

        last_g1 = g1;
        last_c1 = c1;
        last_g2 = g2;
        drop(cap_rtx);
        drop(git_rtx1);

        if attempt < max_retries && !retry_backoff.is_zero() {
            std::thread::sleep(retry_backoff);
        }
    }

    Err(StorageBackendError::BackendInternal {
        detail: format!(
            "{CROSS_STORE_INCONSISTENT_SNAPSHOT_PREFIX}: generation mismatch persisted after {max_retries} retries (git_g1={last_g1}, capture_c1={last_c1}, git_g2={last_g2}); run `libra agent doctor --repair`"
        ),
    })
}

/// Acquire `capture.redb` write lock SECOND (under an already-held `git_wtx`,
/// preserving `GC-RB-04` `GitStore -> CaptureStore` lock order), validate
/// `capture.META_GENERATION`, replay any pending `(cap_gen + 1)..=git_gen`
/// intents in strictly ascending order, and return an exclusively held
/// `WriteTransaction` on `capture.redb` synchronized at `git_gen`.
fn reconcile_and_hold_capture_wtx(
    git_wtx: &redb::WriteTransaction,
    capture: &RedbCaptureStore,
    git_gen: u64,
    cancel_state: &AtomicU8,
    #[cfg(test)] hook: &CrossStoreCommitHook,
) -> StorageResult<(Vec<u64>, redb::WriteTransaction)> {
    // Acquire `capture.redb` write tx SECOND (strict `GitStore -> CaptureStore` lock order).
    let cap_wtx = capture.begin_checked_write_tx().map_err(|err| {
        StorageBackendError::BackendInternal {
            detail: format!(
                "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: cannot begin capture write transaction ({}): refusing to advance git generation",
                err.stable_code_str()
            ),
        }
    })?;
    check_not_cancelled(cancel_state)?;

    let cap_gen = RedbCaptureStore::read_cross_store_generation_in_wtx(&cap_wtx).map_err(|err| {
        StorageBackendError::BackendInternal {
            detail: format!(
                "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: cannot read capture generation ({}): refusing to advance git generation",
                err.stable_code_str()
            ),
        }
    })?;
    if cap_gen > git_gen {
        drop(cap_wtx);
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: capture generation ({cap_gen}) exceeds git generation ({git_gen}); refusing to mask divergence"
            ),
        });
    }
    if cap_gen == git_gen {
        return Ok((Vec::new(), cap_wtx));
    }

    // `cap_gen < git_gen`: mandatory ascending replay of every missing intent in
    // `(cap_gen + 1)..=git_gen` before any new generation may be allocated.
    let pending_raw = RedbGitStore::read_pending_cross_store_intents_in_wtx(git_wtx, cap_gen)?;
    let expected_count =
        usize::try_from(git_gen - cap_gen).map_err(|_| StorageBackendError::BackendInternal {
            detail: "cross-store pending generation gap exceeds platform bounds".to_string(),
        })?;
    if pending_raw.len() < expected_count {
        drop(cap_wtx);
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "cross-store outbox is missing pending intent(s) between generation {} and {git_gen}; refusing to skip gap",
                cap_gen + 1
            ),
        });
    }

    let mut replayed = Vec::with_capacity(expected_count);
    let mut expected_gen =
        cap_gen
            .checked_add(1)
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "cross-store generation overflow during pending intent replay".to_string(),
            })?;

    for (key_gen, raw_bytes) in pending_raw {
        if key_gen > git_gen {
            drop(cap_wtx);
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "cross-store outbox contains orphan intent generation {key_gen} beyond git generation {git_gen}"
                ),
            });
        }
        if key_gen != expected_gen {
            drop(cap_wtx);
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "cross-store outbox generation gap: expected intent {expected_gen}, found {key_gen}; refusing to skip gap"
                ),
            });
        }
        let intent: CrossStoreIntentRecord =
            decode_versioned(&raw_bytes, CROSS_STORE_INTENT_CODEC_VERSION)?;
        if intent.generation != key_gen {
            drop(cap_wtx);
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "cross-store intent payload generation ({}) does not match outbox key ({key_gen})",
                    intent.generation
                ),
            });
        }
        for item in &intent.items {
            validate_capture_batch_item(item)?;
        }
        let _ = RedbCaptureStore::apply_capture_batch_items_in_wtx(&cap_wtx, &intent.items)?;
        RedbCaptureStore::write_cross_store_generation_in_wtx(&cap_wtx, key_gen)?;
        replayed.push(key_gen);
        expected_gen = expected_gen.saturating_add(1);
    }

    if replayed.len() != expected_count {
        drop(cap_wtx);
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "cross-store outbox replay incomplete: replayed {} of {expected_count} pending intent(s)",
                replayed.len()
            ),
        });
    }

    // Atomic pre-commit cancellation boundary before persisting pending-intent replay:
    // if the async caller dropped/aborted while waiting on `cap_wtx`, drop `cap_wtx`
    // uncommitted so neither `capture.redb` nor `git.redb` changes.
    {
        let _replay_boundary = ReplayCommitBoundaryGuard::enter(cancel_state)?;
        #[cfg(test)]
        hook.fire_inside_replay_commit_boundary();
        cap_wtx
            .commit()
            .map_err(|err| map_redb_commit_err(err, "commit replayed cross-store intents"))?;
        capture.record_write_tx_commit();
    }
    check_not_cancelled(cancel_state)?;

    // Immediately re-acquire `cap_wtx` while still holding `git_wtx` so the new
    // capture batch is validated and staged under exclusive capture synchronization.
    let next_cap_wtx = capture.begin_checked_write_tx().map_err(|err| {
        StorageBackendError::BackendInternal {
            detail: format!(
                "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: cannot re-acquire capture write transaction after replay ({}): refusing to advance git generation",
                err.stable_code_str()
            ),
        }
    })?;
    check_not_cancelled(cancel_state)?;
    let verified_cap_gen = RedbCaptureStore::read_cross_store_generation_in_wtx(&next_cap_wtx)
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!(
                "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: cannot verify capture generation after replay ({}): refusing to advance git generation",
                err.stable_code_str()
            ),
        })?;
    if verified_cap_gen != git_gen {
        drop(next_cap_wtx);
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "{CROSS_STORE_CAPTURE_UNREACHABLE_PREFIX}: capture generation ({verified_cap_gen}) diverged from git generation ({git_gen}) after replay"
            ),
        });
    }

    Ok((replayed, next_cap_wtx))
}

fn commit_cross_store_blocking(
    git: &RedbGitStore,
    capture: &RedbCaptureStore,
    git_op: &CrossStoreRefCasOp,
    capture_items: &[CaptureBatchItem],
    cancel_state: &AtomicU8,
    #[cfg(test)] hook: &CrossStoreCommitHook,
) -> StorageResult<CrossStoreCommitOutcome> {
    #[cfg(test)]
    hook.fire_blocking_enter();
    check_not_cancelled(cancel_state)?;

    // Phase 0 + Phase 1: Acquire `git.redb` write lock FIRST (`GC-RB-04`).
    let git_wtx = git.begin_checked_write_tx()?;
    check_not_cancelled(cancel_state)?;
    let git_gen = RedbGitStore::read_cross_store_generation_in_wtx(&git_wtx)?;

    #[cfg(test)]
    hook.on_after_git_wtx_acquired(git_gen);

    // Mandatory pre-advance reconciliation (acquires `capture.redb` write lock SECOND
    // and keeps `cap_wtx` held exclusively across validation and Step 1 commit).
    // Fails closed if `capture.redb` is unreachable, corrupt, or diverged — `git_wtx`
    // drops uncommitted so `git.redb` never advances.
    let (replayed_generations, cap_wtx) = reconcile_and_hold_capture_wtx(
        &git_wtx,
        capture,
        git_gen,
        cancel_state,
        #[cfg(test)]
        hook,
    )?;
    check_not_cancelled(cancel_state)?;

    // Apply Git reference CAS atomically inside `git_wtx`.
    let (applied, ref_outcome) = RedbGitStore::apply_ref_cas_in_wtx(
        &git_wtx,
        &git_op.key,
        git_op.expected.as_ref(),
        git_op.new_target.as_ref(),
        &git_op.reflogs,
        git_op.publication.as_ref(),
    )?;
    if !applied {
        drop(cap_wtx);
        drop(git_wtx);
        return Ok(CrossStoreCommitOutcome {
            generation: git_gen,
            ref_outcome,
            replayed_generations,
            capture_applied: 0,
            capture_deduplicated: 0,
        });
    }

    let next_gen = git_gen
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "cross-store generation counter overflow".to_string(),
        })?;

    // Validate and stage the entire capture batch inside the exclusively locked
    // `cap_wtx` BEFORE committing `git_wtx`. If any FK, tombstone, uniqueness, or
    // append-only constraint fails, both `cap_wtx` and `git_wtx` roll back cleanly
    // so an invalid capture batch can never poison `META_CROSS_STORE_INTENT`.
    let (capture_applied, capture_deduplicated) =
        RedbCaptureStore::apply_capture_batch_items_in_wtx(&cap_wtx, capture_items)?;
    RedbCaptureStore::write_cross_store_generation_in_wtx(&cap_wtx, next_gen)?;

    let intent = CrossStoreIntentRecord {
        generation: next_gen,
        items: capture_items.to_vec(),
    };
    let encoded_intent = encode_versioned(CROSS_STORE_INTENT_CODEC_VERSION, &intent)?;

    RedbGitStore::write_cross_store_generation_in_wtx(&git_wtx, next_gen)?;
    RedbGitStore::write_cross_store_intent_in_wtx(&git_wtx, next_gen, &encoded_intent)?;

    // Atomic pre-commit cancellation boundary: if the async caller dropped or
    // aborted its future before this point, abort cleanly and drop both
    // transactions uncommitted without modifying either store.
    enter_commit_boundary(cancel_state)?;

    // Step 1: Commit `git.redb` FIRST (durable Git mutation + META_GENERATION + outbox intent).
    // `cap_wtx` stays held exclusively across this boundary so no concurrent capture
    // writer can invalidate the staged capture batch before Step 2 commits.
    git_wtx
        .commit()
        .map_err(|err| map_redb_commit_err(err, "commit cross-store git step"))?;

    #[cfg(test)]
    if let Some(err) = hook.on_after_git_commit(next_gen) {
        drop(cap_wtx);
        return Err(err);
    }

    // Step 2: Commit `capture.redb` SECOND (durable capture batch + matching META_GENERATION).
    cap_wtx
        .commit()
        .map_err(|err| map_redb_commit_err(err, "commit cross-store capture step"))?;
    capture.record_write_tx_commit();

    Ok(CrossStoreCommitOutcome {
        generation: next_gen,
        ref_outcome,
        replayed_generations,
        capture_applied,
        capture_deduplicated,
    })
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::db::schema::DatabaseRole,
        storage::{
            capture_store::{
                AgentSessionRecord, CaptureAgentKind, CaptureBatchPayload, CaptureScopeState,
                CaptureSessionState, CaptureStore,
            },
            git_store::GitStore,
            meta::write_migration_tombstone,
            opener::open_redb_writable,
        },
    };

    fn sample_oid(ch: char) -> String {
        std::iter::repeat_n(ch, 40).collect()
    }

    fn sample_session_item(
        dedup_key: &str,
        session_id: &str,
        provider_session_id: &str,
        started_at: i64,
    ) -> CaptureBatchItem {
        CaptureBatchItem {
            dedup_key: dedup_key.to_string(),
            payload: CaptureBatchPayload::UpsertSession(AgentSessionRecord {
                session_id: session_id.to_string(),
                agent_kind: CaptureAgentKind::Codex,
                provider_session_id: provider_session_id.to_string(),
                thread_id: None,
                state: CaptureSessionState::Active,
                working_dir: "/workspace/repo".to_string(),
                worktree_id: Some("wt-main".to_string()),
                parent_commit: None,
                parent_session_id: None,
                metadata_json: "{}".to_string(),
                redaction_report: "{}".to_string(),
                started_at,
                last_event_at: started_at + 10,
                stopped_at: None,
                schema_version: 1,
                sync_revision: 1,
                repo_id: Some("repo-rb06b".to_string()),
                workspace_id: None,
                workspace_fence: None,
                scope_state: CaptureScopeState::Scoped,
            }),
        }
    }

    fn branch_cas_op(
        branch: &str,
        expected_oid: Option<String>,
        next_oid: String,
    ) -> CrossStoreRefCasOp {
        CrossStoreRefCasOp {
            key: RefKey::LocalBranch {
                name: branch.to_string(),
            },
            expected: expected_oid.map(RefTarget::Direct),
            new_target: Some(RefTarget::Direct(next_oid)),
            reflogs: Vec::new(),
            publication: None,
        }
    }

    /// `RB-06b` AC1–AC3:
    /// - AC1: Normal cross-store commit atomically writes Git change + `META_GENERATION = git_gen + 1`
    ///   + `META_CROSS_STORE_INTENT[gen]` in `git.redb`, then idempotently applies the intent and
    ///   writes the matching `gen` in `capture.redb`.
    /// - AC2: Fault injection after `git.redb` commits `gen = N` and before `capture.redb` commits:
    ///   strictly read-only three-step snapshot detects `g1 != c1`, retries 5 times, and fails closed
    ///   with `LBR-IO-001` without upgrading or modifying disk.
    /// - AC3: Subsequent cross-store commit (`N + 1`) replays pending intent `N` into `capture.redb`
    ///   in ascending order BEFORE committing `N + 1`, preserving both `N` and `N + 1` without ever
    ///   masking the gap.
    #[tokio::test]
    async fn partial_commit_fault_blocks_snapshot_and_subsequent_writer_replays_first() {
        let dir = tempdir().expect("tempdir");
        let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
        let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
        fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

        let git = RedbGitStore::create(&git_path).expect("create git.redb");
        let capture = RedbCaptureStore::create(&cap_path)
            .await
            .expect("create capture.redb");
        let coord = CrossStoreCoordinator::new(git.clone(), capture.clone())
            .with_snapshot_retry_backoff(Duration::ZERO);

        // --- AC1: Normal cross-store commit (gen 0 -> 1) ---
        let oid1 = sample_oid('1');
        let item1 = sample_session_item("xstore:gen1:sess1", "sess-gen-1", "prov-gen-1", 1000);
        let outcome1 = coord
            .commit_ref_cas_with_capture(&branch_cas_op("main", None, oid1.clone()), &[item1])
            .await
            .expect("AC1: normal cross-store commit gen=1 must succeed");
        assert_eq!(outcome1.generation, 1);
        assert!(outcome1.replayed_generations.is_empty());
        assert_eq!(outcome1.capture_applied, 1);
        assert_eq!(outcome1.capture_deduplicated, 0);
        assert!(matches!(
            outcome1.ref_outcome,
            RefCasOutcome::Applied {
                previous: None,
                current: Some(RefTarget::Direct(ref o))
            } if o == &oid1
        ));
        assert_eq!(
            git.read_cross_store_generation().expect("git gen 1"),
            1,
            "AC1: git.redb META_GENERATION must be 1"
        );
        assert_eq!(
            capture.read_cross_store_generation().expect("cap gen 1"),
            1,
            "AC1: capture.redb META_GENERATION must be 1"
        );
        assert_eq!(
            git.list_cross_store_intent_generations()
                .expect("intent keys"),
            vec![1],
            "AC1: META_CROSS_STORE_INTENT[1] must be durably recorded in git.redb"
        );
        assert!(
            capture
                .get_session("sess-gen-1")
                .await
                .expect("read sess-gen-1")
                .is_some()
        );

        // Verify clean read-only 3-step snapshot succeeds on attempt 1 when g1 == c1 == g2 == 1.
        let git_ro = RedbGitStore::open_read_only(&git_path).expect("open git read-only");
        let cap_ro = RedbCaptureStore::open_read_only(&cap_path)
            .await
            .expect("open capture read-only");
        let ro_coord = CrossStoreCoordinator::new(git_ro.clone(), cap_ro.clone())
            .with_snapshot_retry_backoff(Duration::ZERO);
        let snap1 = ro_coord
            .snapshot_consistent_read_only()
            .await
            .expect("consistent snapshot at gen=1");
        assert_eq!(snap1.generation(), 1);
        assert_eq!(snap1.attempts(), 1);
        assert_eq!(
            snap1
                .get_git_ref(&RefKey::LocalBranch {
                    name: "main".to_string()
                })
                .expect("read ref from snapshot"),
            Some(RefTarget::Direct(oid1.clone()))
        );
        assert!(
            snap1
                .get_capture_session("sess-gen-1")
                .expect("read session from snapshot")
                .is_some()
        );
        drop(snap1);

        // --- AC2: Fault injection after git.redb commits gen=2, before capture.redb commits ---
        let oid2 = sample_oid('2');
        let item2 = sample_session_item("xstore:gen2:sess2", "sess-gen-2", "prov-gen-2", 2000);
        let fault_err = coord
            .commit_ref_cas_with_capture_inner(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[item2],
                CrossStoreCommitHook::fail_after_git_commit(),
            )
            .await
            .expect_err("AC2: injected crash after git commit must return error");
        assert!(fault_err.to_string().contains("generation 2"));

        // git.redb has gen=2 + ref=oid2 + intent[2], while capture.redb is still at gen=1 without sess-gen-2.
        assert_eq!(git.read_cross_store_generation().expect("git gen 2"), 2);
        assert_eq!(capture.read_cross_store_generation().expect("cap gen 1"), 1);
        assert_eq!(
            git.list_cross_store_intent_generations()
                .expect("intent keys after crash"),
            vec![1, 2]
        );
        assert!(
            capture
                .get_session("sess-gen-2")
                .await
                .expect("read sess-gen-2 during partial state")
                .is_none(),
            "capture.redb must not have sess-gen-2 before replay"
        );

        // Strictly read-only 3-step snapshot detects g1(2) != c1(1), retries 5 times,
        // fails closed with LBR-IO-001, and does NOT mutate capture.redb.
        let snap_err = ro_coord
            .snapshot_consistent_read_only()
            .await
            .expect_err("AC2: read-only snapshot must fail closed on g1 != c1");
        assert_eq!(
            snap_err.stable_code_str(),
            "LBR-IO-001",
            "AC2: inconsistent snapshot must map to stable code LBR-IO-001"
        );
        assert!(is_cross_store_inconsistent_snapshot_error(&snap_err));
        assert!(
            snap_err.to_string().contains("5 retries"),
            "AC2: error must report 5 retries exhausted: {snap_err}"
        );
        assert!(
            snap_err.to_string().contains("libra agent doctor --repair"),
            "AC2: error must advise explicit repair: {snap_err}"
        );
        // Confirm read-only snapshot never upgraded or repaired capture.redb.
        assert_eq!(
            capture
                .read_cross_store_generation()
                .expect("cap gen still 1"),
            1
        );
        assert!(
            capture
                .get_session("sess-gen-2")
                .await
                .expect("sess-gen-2 still absent after read-only snapshot")
                .is_none()
        );

        // --- AC3: Subsequent cross-store commit (gen=3) MUST replay pending intent 2 first ---
        let oid3 = sample_oid('3');
        let item3 = sample_session_item("xstore:gen3:sess3", "sess-gen-3", "prov-gen-3", 3000);
        let outcome3 = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid2.clone()), oid3.clone()),
                &[item3],
            )
            .await
            .expect("AC3: subsequent writer must replay intent 2 first then commit gen=3");
        assert_eq!(outcome3.generation, 3);
        assert_eq!(
            outcome3.replayed_generations,
            vec![2],
            "AC3: writer must replay pending generation 2 before committing generation 3"
        );
        assert_eq!(outcome3.capture_applied, 1);
        assert_eq!(outcome3.capture_deduplicated, 0);

        // Both gen 2 (replayed) AND gen 3 (new) are durably preserved in capture.redb!
        assert_eq!(git.read_cross_store_generation().expect("git gen 3"), 3);
        assert_eq!(capture.read_cross_store_generation().expect("cap gen 3"), 3);
        assert_eq!(
            git.get_ref(&RefKey::LocalBranch {
                name: "main".to_string()
            })
            .await
            .expect("read main ref"),
            Some(RefTarget::Direct(oid3.clone()))
        );
        assert!(
            capture
                .get_session("sess-gen-1")
                .await
                .expect("sess-gen-1")
                .is_some()
        );
        assert!(
            capture
                .get_session("sess-gen-2")
                .await
                .expect("sess-gen-2 preserved via replay")
                .is_some(),
            "AC3: replayed generation 2 session must be preserved"
        );
        assert!(
            capture
                .get_session("sess-gen-3")
                .await
                .expect("sess-gen-3 preserved")
                .is_some(),
            "AC3: new generation 3 session must be preserved"
        );

        // Now read-only 3-step snapshot succeeds cleanly with g1 == c1 == g2 == 3.
        let snap3 = ro_coord
            .snapshot_consistent_read_only()
            .await
            .expect("consistent snapshot after replay+commit");
        assert_eq!(snap3.generation(), 3);
        assert_eq!(snap3.attempts(), 1);
        assert_eq!(
            snap3
                .get_git_ref(&RefKey::LocalBranch {
                    name: "main".to_string()
                })
                .expect("read main from snap3"),
            Some(RefTarget::Direct(oid3.clone()))
        );
        assert!(
            snap3
                .get_capture_session("sess-gen-2")
                .expect("s2")
                .is_some()
        );
        assert!(
            snap3
                .get_capture_session("sess-gen-3")
                .expect("s3")
                .is_some()
        );
        drop(snap3);

        // Bonus: multi-generation ascending replay verification (create a 2-gen gap: 4 and 5
        // committed in git.redb without capture.redb, then writer for gen=6 replays [4, 5] ascending).
        let oid4 = sample_oid('4');
        let oid5 = sample_oid('5');
        let oid6 = sample_oid('6');
        {
            let wtx = git.begin_checked_write_tx().expect("seed multi-gap");
            for (g, prev, next, sid, prov, dkey) in [
                (
                    4u64,
                    oid3.clone(),
                    oid4.clone(),
                    "sess-gen-4",
                    "prov-gen-4",
                    "xstore:gen4",
                ),
                (
                    5u64,
                    oid4.clone(),
                    oid5.clone(),
                    "sess-gen-5",
                    "prov-gen-5",
                    "xstore:gen5",
                ),
            ] {
                let (applied, _) = RedbGitStore::apply_ref_cas_in_wtx(
                    &wtx,
                    &RefKey::LocalBranch {
                        name: "main".to_string(),
                    },
                    Some(&RefTarget::Direct(prev)),
                    Some(&RefTarget::Direct(next)),
                    &[],
                    None,
                )
                .expect("apply ref cas");
                assert!(applied);
                let rec = CrossStoreIntentRecord {
                    generation: g,
                    items: vec![sample_session_item(dkey, sid, prov, 4000 + g as i64)],
                };
                let bytes = encode_versioned(CROSS_STORE_INTENT_CODEC_VERSION, &rec)
                    .expect("encode multi-gap intent");
                RedbGitStore::write_cross_store_generation_in_wtx(&wtx, g).expect("write gen");
                RedbGitStore::write_cross_store_intent_in_wtx(&wtx, g, &bytes)
                    .expect("write intent");
            }
            wtx.commit().expect("commit multi-gap");
        }
        let outcome6 = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid5), oid6),
                &[sample_session_item(
                    "xstore:gen6",
                    "sess-gen-6",
                    "prov-gen-6",
                    6000,
                )],
            )
            .await
            .expect("multi-gap replay + gen=6 must succeed");
        assert_eq!(outcome6.generation, 6);
        assert_eq!(outcome6.replayed_generations, vec![4, 5]);
        assert!(
            capture
                .get_session("sess-gen-4")
                .await
                .expect("s4")
                .is_some()
        );
        assert!(
            capture
                .get_session("sess-gen-5")
                .await
                .expect("s5")
                .is_some()
        );
        assert!(
            capture
                .get_session("sess-gen-6")
                .await
                .expect("s6")
                .is_some()
        );
    }

    /// `RB-06b` AC4: After a partial commit fault (`git.redb` committed `gen = 1`,
    /// `capture.redb` still at `gen = 0`), if `capture.redb` is unreachable
    /// (deleted), corrupted on disk, migration-tombstoned (`u32::MAX`), or opened
    /// read-only, subsequent cross-store writers MUST immediately fail closed
    /// with `LBR-IO-001` and refuse to advance `git.redb`'s `META_GENERATION`
    /// or apply the new Git change.
    #[tokio::test]
    async fn unreachable_capture_blocks_new_gen() {
        let dir = tempdir().expect("tempdir");
        let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
        let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
        fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

        let git = RedbGitStore::create(&git_path).expect("create git.redb");
        let capture = RedbCaptureStore::create(&cap_path)
            .await
            .expect("create capture.redb");

        // Inject partial commit at gen=1 (git committed, capture still at gen=0).
        let oid1 = sample_oid('a');
        let oid2 = sample_oid('b');
        {
            let coord = CrossStoreCoordinator::new(git.clone(), capture.clone());
            let _ = coord
                .commit_ref_cas_with_capture_inner(
                    &branch_cas_op("main", None, oid1.clone()),
                    &[sample_session_item(
                        "xstore:ac4:gen1",
                        "sess-ac4-1",
                        "prov-ac4-1",
                        1000,
                    )],
                    CrossStoreCommitHook::fail_after_git_commit(),
                )
                .await
                .expect_err("fault after git commit at gen=1");
        }
        assert_eq!(git.read_cross_store_generation().expect("git gen 1"), 1);
        assert_eq!(capture.read_cross_store_generation().expect("cap gen 0"), 0);

        // Case 1: Read-only capture handle cannot replay -> fail closed, git_gen stays 1.
        let cap_ro = RedbCaptureStore::open_read_only(&cap_path)
            .await
            .expect("open capture read-only");
        let ro_handle_coord = CrossStoreCoordinator::new(git.clone(), cap_ro);
        let err_ro = ro_handle_coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[sample_session_item(
                    "xstore:ac4:gen2-ro",
                    "sess-ac4-2-ro",
                    "prov-ac4-2-ro",
                    2000,
                )],
            )
            .await
            .expect_err("AC4: read-only capture handle must block new gen");
        assert_eq!(err_ro.stable_code_str(), "LBR-IO-001");
        assert!(is_cross_store_capture_unavailable_error(&err_ro));
        assert_eq!(
            git.read_cross_store_generation().expect("git gen still 1"),
            1
        );
        assert_eq!(
            git.get_ref(&RefKey::LocalBranch {
                name: "main".to_string()
            })
            .await
            .expect("read main"),
            Some(RefTarget::Direct(oid1.clone()))
        );

        // Case 2: Migration-tombstoned (`u32::MAX`) capture.redb handle -> fail closed, git_gen stays 1.
        {
            let raw_cap =
                open_redb_writable(&cap_path, DatabaseRole::Capture, RedbRepairContext::Migrate)
                    .expect("open raw capture for tombstone");
            let wtx = raw_cap.begin_write().expect("begin raw write");
            write_migration_tombstone(&wtx, DatabaseRole::Capture)
                .expect("write capture migration tombstone");
            wtx.commit().expect("commit tombstone");
        }
        let fenced_coord = CrossStoreCoordinator::new(git.clone(), capture);
        let err_fenced = fenced_coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[sample_session_item(
                    "xstore:ac4:gen2-fenced",
                    "sess-ac4-2-fenced",
                    "prov-ac4-2-fenced",
                    2010,
                )],
            )
            .await
            .expect_err("AC4: fenced capture handle must block new gen");
        assert_eq!(err_fenced.stable_code_str(), "LBR-IO-001");
        assert!(is_cross_store_capture_unavailable_error(&err_fenced));
        assert_eq!(
            git.read_cross_store_generation().expect("git gen still 1"),
            1
        );
        drop(fenced_coord);

        // Case 3: Corrupt bytes at `capture.redb` path -> fail closed, git_gen stays 1.
        fs::write(&cap_path, b"not-a-valid-redb-database-file-corrupt")
            .expect("corrupt capture.redb");
        let path_coord = CrossStoreCoordinator::with_capture_path(
            git.clone(),
            &cap_path,
            RedbRepairContext::WriteCommand,
        );
        let err_corrupt = path_coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[sample_session_item(
                    "xstore:ac4:gen2-corrupt",
                    "sess-ac4-2-corrupt",
                    "prov-ac4-2-corrupt",
                    2020,
                )],
            )
            .await
            .expect_err("AC4: corrupted capture.redb must block new gen");
        assert_eq!(err_corrupt.stable_code_str(), "LBR-IO-001");
        assert!(is_cross_store_capture_unavailable_error(&err_corrupt));
        assert_eq!(
            git.read_cross_store_generation().expect("git gen still 1"),
            1
        );
        assert_eq!(
            git.list_cross_store_intent_generations().expect("intents"),
            vec![1],
            "AC4: corrupted capture must not append intent[2]"
        );

        // Case 4: Missing / deleted `capture.redb` -> fail closed, git_gen stays 1.
        fs::remove_file(&cap_path).expect("remove capture.redb");
        let err_missing = path_coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2),
                &[sample_session_item(
                    "xstore:ac4:gen2-missing",
                    "sess-ac4-2-missing",
                    "prov-ac4-2-missing",
                    2030,
                )],
            )
            .await
            .expect_err("AC4: missing capture.redb must block new gen");
        assert_eq!(err_missing.stable_code_str(), "LBR-IO-001");
        assert!(is_cross_store_capture_unavailable_error(&err_missing));
        assert_eq!(
            git.read_cross_store_generation().expect("git gen still 1"),
            1,
            "AC4: git.redb generation must never advance while capture.redb is unreachable"
        );
        assert_eq!(
            git.list_cross_store_intent_generations().expect("intents"),
            vec![1]
        );
        assert_eq!(
            git.get_ref(&RefKey::LocalBranch {
                name: "main".to_string()
            })
            .await
            .expect("main ref untouched"),
            Some(RefTarget::Direct(oid1))
        );
    }

    /// Codex R1 P1 regression: a rejected capture mutation (missing session FK,
    /// tombstoned provider identity, provider uniqueness conflict, or append-only
    /// audit overwrite) MUST fail BEFORE `git.redb` commits so `META_CROSS_STORE_INTENT`
    /// is never poisoned and subsequent commits + snapshots remain completely healthy.
    #[tokio::test]
    async fn rejected_capture_mutation_does_not_poison_outbox() {
        use crate::storage::capture_store::{
            AgentAuditLogRecord, AgentCheckpointRecord, AgentImportTombstoneRecord,
            CaptureCheckpointScope, is_capture_append_only_error, is_capture_tombstone_error,
        };

        let dir = tempdir().expect("tempdir");
        let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
        let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
        fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

        let git = RedbGitStore::create(&git_path).expect("create git.redb");
        let capture = RedbCaptureStore::create(&cap_path)
            .await
            .expect("create capture.redb");
        let coord = CrossStoreCoordinator::new(git.clone(), capture.clone())
            .with_snapshot_retry_backoff(Duration::ZERO);

        // Seed gen=1 with one valid session + one audit log entry + one import tombstone.
        let oid1 = sample_oid('1');
        let oid2 = sample_oid('2');
        let seed_audit = AgentAuditLogRecord {
            audit_id: "audit-existing-1".to_string(),
            timestamp: "2026-11-23T00:00:00Z".to_string(),
            user_id: None,
            user_name: None,
            action: "export".to_string(),
            checkpoint_id: "cp-0".to_string(),
            scope: "session".to_string(),
            export_path: None,
            justification: None,
            granted: true,
        };
        let out1 = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", None, oid1.clone()),
                &[
                    sample_session_item("seed:sess1", "sess-existing", "prov-existing", 1000),
                    CaptureBatchItem {
                        dedup_key: "seed:audit1".to_string(),
                        payload: CaptureBatchPayload::AppendAuditLog(seed_audit.clone()),
                    },
                ],
            )
            .await
            .expect("seed gen=1");
        assert_eq!(out1.generation, 1);

        capture
            .put_import_tombstone(&AgentImportTombstoneRecord {
                tombstone_id: "tomb-1".to_string(),
                agent_kind: "codex".to_string(),
                provider_session_id: "prov-erased".to_string(),
                erased_session_id: "sess-erased".to_string(),
                source_fingerprint: None,
                erased_at: 1100,
            })
            .await
            .expect("insert tombstone");

        // 1) Missing session FK on checkpoint
        let bad_fk = CaptureBatchItem {
            dedup_key: "bad:fk".to_string(),
            payload: CaptureBatchPayload::UpsertCheckpoint(AgentCheckpointRecord {
                checkpoint_id: "cp-orphan".to_string(),
                session_id: "sess-does-not-exist".to_string(),
                parent_checkpoint_id: None,
                scope: CaptureCheckpointScope::Temporary,
                parent_commit: None,
                tree_oid: sample_oid('9'),
                metadata_blob_oid: sample_oid('8'),
                traces_commit: sample_oid('7'),
                tool_use_id: None,
                subagent_session_id: None,
                description: None,
                created_at: 2000,
                sync_revision: 1,
            }),
        };
        let err_fk = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[bad_fk],
            )
            .await
            .expect_err("missing session FK must fail before git commit");
        assert!(err_fk.to_string().contains("foreign key"));

        // 2) Tombstoned provider identity
        let bad_tomb = sample_session_item("bad:tomb", "sess-reimport", "prov-erased", 2001);
        let err_tomb = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[bad_tomb],
            )
            .await
            .expect_err("tombstoned provider identity must fail before git commit");
        assert!(is_capture_tombstone_error(&err_tomb));

        // 3) Provider uniqueness conflict (different session_id, existing (agent_kind, provider_session_id))
        let bad_unique = sample_session_item(
            "bad:unique",
            "sess-duplicate-provider",
            "prov-existing",
            2002,
        );
        let err_unique = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[bad_unique],
            )
            .await
            .expect_err("provider uniqueness conflict must fail before git commit");
        assert!(err_unique.to_string().contains("unique"));

        // 4) Append-only audit log overwrite
        let bad_audit = CaptureBatchItem {
            dedup_key: "bad:audit-overwrite".to_string(),
            payload: CaptureBatchPayload::AppendAuditLog(AgentAuditLogRecord {
                granted: false,
                ..seed_audit
            }),
        };
        let err_audit = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1.clone()), oid2.clone()),
                &[bad_audit],
            )
            .await
            .expect_err("append-only audit overwrite must fail before git commit");
        assert!(is_capture_append_only_error(&err_audit));

        // After ALL four rejected mutations: git_gen == cap_gen == 1, outbox untouched,
        // ref untouched, snapshot healthy, and next valid commit succeeds cleanly!
        assert_eq!(git.read_cross_store_generation().expect("git gen"), 1);
        assert_eq!(capture.read_cross_store_generation().expect("cap gen"), 1);
        assert_eq!(
            git.list_cross_store_intent_generations().expect("intents"),
            vec![1],
            "rejected capture mutations must never insert poisoned outbox intents"
        );
        assert_eq!(
            git.get_ref(&RefKey::LocalBranch {
                name: "main".to_string()
            })
            .await
            .expect("main ref"),
            Some(RefTarget::Direct(oid1.clone()))
        );
        let snap = coord
            .snapshot_consistent_read_only()
            .await
            .expect("snapshot must remain available after rejected capture mutations");
        assert_eq!(snap.generation(), 1);
        drop(snap);

        let out2 = coord
            .commit_ref_cas_with_capture(
                &branch_cas_op("main", Some(oid1), oid2.clone()),
                &[sample_session_item(
                    "good:sess2",
                    "sess-valid-2",
                    "prov-valid-2",
                    3000,
                )],
            )
            .await
            .expect("valid commit after rejected batches must succeed cleanly");
        assert_eq!(out2.generation, 2);
        assert!(out2.replayed_generations.is_empty());
        assert_eq!(
            git.get_ref(&RefKey::LocalBranch {
                name: "main".to_string()
            })
            .await
            .expect("main ref after gen=2"),
            Some(RefTarget::Direct(oid2))
        );
        assert!(
            capture
                .get_session("sess-valid-2")
                .await
                .expect("sess-valid-2")
                .is_some()
        );
    }

    /// Codex R1 P1 regression: missing required `meta_generation` table or its
    /// singleton entry in `capture.redb` (or damaged `meta_generation` in
    /// `git.redb`) MUST fail closed as corruption (`LBR-IO-001`) instead of
    /// silently returning `0` or recreating the missing table.
    #[tokio::test]
    async fn missing_capture_generation_metadata_fails_closed() {
        use redb::{TableDefinition, TableHandle};

        const CAP_META_GEN_TABLE: TableDefinition<'static, &str, &[u8]> =
            TableDefinition::new("meta_generation");
        const GIT_META_GEN_TABLE: TableDefinition<'static, &str, &[u8]> =
            TableDefinition::new("meta_generation");
        const GIT_INTENT_TABLE: TableDefinition<'static, u64, &[u8]> =
            TableDefinition::new("meta_cross_store_intent");

        // --- Sub-case A: Missing `meta_generation` table in capture.redb ---
        {
            let dir = tempdir().expect("tempdir");
            let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
            let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
            fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

            let git = RedbGitStore::create(&git_path).expect("create git.redb");
            let capture = RedbCaptureStore::create(&cap_path)
                .await
                .expect("create capture.redb");
            let coord = CrossStoreCoordinator::new(git.clone(), capture.clone())
                .with_snapshot_retry_backoff(Duration::ZERO);

            // Corrupt capture.redb by deleting the required `meta_generation` table.
            {
                let raw = open_redb_writable(
                    &cap_path,
                    DatabaseRole::Capture,
                    RedbRepairContext::Migrate,
                )
                .expect("open raw capture");
                let wtx = raw.begin_write().expect("begin write");
                assert!(
                    wtx.delete_table(CAP_META_GEN_TABLE)
                        .expect("delete capture meta_generation"),
                    "meta_generation table must exist before deletion"
                );
                wtx.commit().expect("commit table deletion");
            }

            // Reader, snapshot, and writer MUST all fail closed (never treat as gen 0).
            let read_err = capture
                .read_cross_store_generation()
                .expect_err("missing capture meta_generation table must fail closed");
            assert_eq!(read_err.stable_code_str(), "LBR-IO-001");

            let snap_err = coord
                .snapshot_consistent_read_only()
                .await
                .expect_err("snapshot must fail closed on missing capture meta_generation table");
            assert_eq!(snap_err.stable_code_str(), "LBR-IO-001");

            let write_err = coord
                .commit_ref_cas_with_capture(
                    &branch_cas_op("main", None, sample_oid('1')),
                    &[sample_session_item("k1", "s1", "p1", 1000)],
                )
                .await
                .expect_err("writer must fail closed on missing capture meta_generation table");
            assert_eq!(write_err.stable_code_str(), "LBR-IO-001");
            assert!(is_cross_store_capture_unavailable_error(&write_err));
            assert_eq!(
                git.read_cross_store_generation().expect("git gen still 0"),
                0
            );

            // Verify write path did NOT silently recreate `meta_generation` table!
            let rtx = capture.begin_checked_read_tx().expect("rtx");
            assert!(
                !rtx.list_tables()
                    .expect("list tables")
                    .any(|h| h.name() == CAP_META_GEN_TABLE.name()),
                "failed write must never silently recreate missing meta_generation table"
            );
        }

        // --- Sub-case B: Missing singleton row inside `meta_generation` in capture.redb ---
        {
            let dir = tempdir().expect("tempdir");
            let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
            let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
            fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

            let git = RedbGitStore::create(&git_path).expect("create git.redb");
            let capture = RedbCaptureStore::create(&cap_path)
                .await
                .expect("create capture.redb");
            let coord = CrossStoreCoordinator::new(git.clone(), capture.clone())
                .with_snapshot_retry_backoff(Duration::ZERO);

            {
                let raw = open_redb_writable(
                    &cap_path,
                    DatabaseRole::Capture,
                    RedbRepairContext::Migrate,
                )
                .expect("open raw capture");
                let wtx = raw.begin_write().expect("begin write");
                {
                    let mut tbl = wtx
                        .open_table(CAP_META_GEN_TABLE)
                        .expect("open capture meta_generation");
                    assert!(
                        tbl.remove("singleton").expect("remove singleton").is_some(),
                        "singleton must exist before deletion"
                    );
                }
                wtx.commit().expect("commit singleton deletion");
            }

            let read_err = capture
                .read_cross_store_generation()
                .expect_err("missing capture meta_generation singleton must fail closed");
            assert_eq!(read_err.stable_code_str(), "LBR-IO-001");

            let snap_err = coord
                .snapshot_consistent_read_only()
                .await
                .expect_err("snapshot must fail closed on missing capture singleton");
            assert_eq!(snap_err.stable_code_str(), "LBR-IO-001");

            let write_err = coord
                .commit_ref_cas_with_capture(
                    &branch_cas_op("main", None, sample_oid('2')),
                    &[sample_session_item("k2", "s2", "p2", 2000)],
                )
                .await
                .expect_err("writer must fail closed on missing capture singleton");
            assert_eq!(write_err.stable_code_str(), "LBR-IO-001");
            assert!(is_cross_store_capture_unavailable_error(&write_err));
            assert_eq!(
                git.read_cross_store_generation().expect("git gen still 0"),
                0
            );
        }

        // --- Sub-case C: Missing git singleton or orphan outbox without git meta_generation ---
        {
            let dir = tempdir().expect("tempdir");
            let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
            let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
            fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

            let git = RedbGitStore::create(&git_path).expect("create git.redb");
            let capture = RedbCaptureStore::create(&cap_path)
                .await
                .expect("create capture.redb");
            let coord = CrossStoreCoordinator::new(git.clone(), capture.clone())
                .with_snapshot_retry_backoff(Duration::ZERO);
            coord
                .commit_ref_cas_with_capture(
                    &branch_cas_op("main", None, sample_oid('3')),
                    &[sample_session_item("k3", "s3", "p3", 3000)],
                )
                .await
                .expect("seed gen=1");

            // Remove git singleton row while table exists -> fail closed.
            {
                let raw = open_redb_writable(
                    &git_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Migrate,
                )
                .expect("open raw git");
                let wtx = raw.begin_write().expect("begin write");
                {
                    let mut tbl = wtx
                        .open_table(GIT_META_GEN_TABLE)
                        .expect("open git meta_generation");
                    assert!(tbl.remove("singleton").expect("remove").is_some());
                }
                wtx.commit().expect("commit");
            }
            assert_eq!(
                git.read_cross_store_generation()
                    .expect_err("missing git singleton")
                    .stable_code_str(),
                "LBR-IO-001"
            );

            // Delete git meta_generation table while meta_cross_store_intent exists -> fail closed.
            {
                let raw = open_redb_writable(
                    &git_path,
                    DatabaseRole::Repository,
                    RedbRepairContext::Migrate,
                )
                .expect("open raw git");
                let wtx = raw.begin_write().expect("begin write");
                assert!(wtx.delete_table(GIT_META_GEN_TABLE).expect("delete"));
                assert!(
                    wtx.list_tables()
                        .expect("list")
                        .any(|h| h.name() == GIT_INTENT_TABLE.name())
                );
                wtx.commit().expect("commit");
            }
            assert_eq!(
                git.read_cross_store_generation()
                    .expect_err("orphan intent without meta_generation")
                    .stable_code_str(),
                "LBR-IO-001"
            );
            assert_eq!(
                coord
                    .snapshot_consistent_read_only()
                    .await
                    .expect_err("snapshot with orphan git intent table")
                    .stable_code_str(),
                "LBR-IO-001"
            );
        }
    }

    /// Codex R1 P1 regression: deterministic barrier-driven tests for:
    /// 1. Two competing cross-store writers across the Git-commit / Capture-commit
    ///    boundary (both normal completion and post-git-commit crash + replay),
    ///    verifying deadlock-free `GitStore -> CaptureStore` lock ordering and
    ///    preservation of both writers' generations and capture records.
    /// 2. Snapshot three-read interleaving where `g1 == c1` and `g1 != g2`
    ///    (both retry-and-recover and 5-retry fail-closed exhaustion).
    #[tokio::test]
    async fn concurrent_writers_and_snapshot_g1_ne_g2_interleavings() {
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        };

        let dir = tempdir().expect("tempdir");
        let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
        let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
        fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

        let git = RedbGitStore::create(&git_path).expect("create git.redb");
        let capture = RedbCaptureStore::create(&cap_path)
            .await
            .expect("create capture.redb");
        let coord = CrossStoreCoordinator::new(git.clone(), capture.clone())
            .with_snapshot_retry_backoff(Duration::ZERO);

        // --- Sub-case A: Competing writers across Git-commit / Capture-commit (normal completion) ---
        let oid1 = sample_oid('1');
        let oid2 = sample_oid('2');
        {
            let (w1_git_done_tx, w1_git_done_rx) = mpsc::channel::<u64>();
            let (w2_got_git_lock_tx, w2_got_git_lock_rx) = mpsc::channel::<u64>();
            let w1_git_done_tx = Arc::new(std::sync::Mutex::new(Some(w1_git_done_tx)));
            let w2_got_git_lock_rx = Arc::new(std::sync::Mutex::new(w2_got_git_lock_rx));
            let w2_got_git_lock_tx = Arc::new(std::sync::Mutex::new(Some(w2_got_git_lock_tx)));

            let hook1 = CrossStoreCommitHook {
                after_git_commit: Some(Arc::new(move |next_gen| {
                    if let Some(tx) = w1_git_done_tx.lock().expect("lock").take() {
                        tx.send(next_gen).expect("signal git commit");
                    }
                    // Wait until Writer 2 has acquired git_wtx (seeing git_gen=1) before
                    // releasing cap_wtx via Step 2 commit.
                    let seen_by_w2 = w2_got_git_lock_rx
                        .lock()
                        .expect("lock")
                        .recv_timeout(Duration::from_secs(10))
                        .expect("writer 2 must acquire git_wtx while writer 1 holds cap_wtx");
                    assert_eq!(seen_by_w2, 1);
                    None
                })),
                ..CrossStoreCommitHook::none()
            };
            let hook2 = CrossStoreCommitHook {
                after_git_wtx_acquired: Some(Arc::new(move |git_gen| {
                    if let Some(tx) = w2_got_git_lock_tx.lock().expect("lock").take() {
                        tx.send(git_gen).expect("signal w2 git_wtx acquired");
                    }
                })),
                ..CrossStoreCommitHook::none()
            };

            let coord1 = coord.clone();
            let op1 = branch_cas_op("main", None, oid1.clone());
            let h1 = tokio::spawn(async move {
                coord1
                    .commit_ref_cas_with_capture_inner(
                        &op1,
                        &[sample_session_item("conc:1", "sess-c1", "prov-c1", 1000)],
                        hook1,
                    )
                    .await
            });

            // Wait until Writer 1 has committed git.redb (gen=1) and is paused before capture commit.
            let g1_committed = tokio::task::spawn_blocking(move || {
                w1_git_done_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("writer 1 must commit git.redb")
            })
            .await
            .expect("join wait");
            assert_eq!(g1_committed, 1);

            let coord2 = coord.clone();
            let op2 = branch_cas_op("main", Some(oid1.clone()), oid2.clone());
            let h2 = tokio::spawn(async move {
                coord2
                    .commit_ref_cas_with_capture_inner(
                        &op2,
                        &[sample_session_item("conc:2", "sess-c2", "prov-c2", 2000)],
                        hook2,
                    )
                    .await
            });

            let out1 = h1.await.expect("join w1").expect("w1 succeeds");
            let out2 = h2.await.expect("join w2").expect("w2 succeeds");
            assert_eq!(out1.generation, 1);
            assert!(out1.replayed_generations.is_empty());
            assert_eq!(out2.generation, 2);
            assert!(out2.replayed_generations.is_empty());
            assert_eq!(git.read_cross_store_generation().expect("git gen 2"), 2);
            assert_eq!(capture.read_cross_store_generation().expect("cap gen 2"), 2);
            assert!(
                capture
                    .get_session("sess-c1")
                    .await
                    .expect("sess-c1")
                    .is_some()
            );
            assert!(
                capture
                    .get_session("sess-c2")
                    .await
                    .expect("sess-c2")
                    .is_some()
            );
        }

        // --- Sub-case B: Competing writer across boundary when Writer 1 crashes before Step 2 ---
        let oid3 = sample_oid('3');
        let oid4 = sample_oid('4');
        {
            let (w1_git_done_tx, w1_git_done_rx) = mpsc::channel::<u64>();
            let (w2_got_git_lock_tx, w2_got_git_lock_rx) = mpsc::channel::<u64>();
            let w1_git_done_tx = Arc::new(std::sync::Mutex::new(Some(w1_git_done_tx)));
            let w2_got_git_lock_rx = Arc::new(std::sync::Mutex::new(w2_got_git_lock_rx));
            let w2_got_git_lock_tx = Arc::new(std::sync::Mutex::new(Some(w2_got_git_lock_tx)));

            let hook1 = CrossStoreCommitHook {
                after_git_commit: Some(Arc::new(move |next_gen| {
                    if let Some(tx) = w1_git_done_tx.lock().expect("lock").take() {
                        tx.send(next_gen).expect("signal git commit");
                    }
                    let seen_by_w2 = w2_got_git_lock_rx
                        .lock()
                        .expect("lock")
                        .recv_timeout(Duration::from_secs(10))
                        .expect("writer 2 must acquire git_wtx");
                    assert_eq!(seen_by_w2, 3);
                    // Now drop cap_wtx uncommitted (simulating crash between Step 1 and Step 2).
                    Some(StorageBackendError::BackendInternal {
                        detail: format!("simulated concurrent crash at gen {next_gen}"),
                    })
                })),
                ..CrossStoreCommitHook::none()
            };
            let hook2 = CrossStoreCommitHook {
                after_git_wtx_acquired: Some(Arc::new(move |git_gen| {
                    if let Some(tx) = w2_got_git_lock_tx.lock().expect("lock").take() {
                        tx.send(git_gen).expect("signal w2 git_wtx");
                    }
                })),
                ..CrossStoreCommitHook::none()
            };

            let coord1 = coord.clone();
            let op3 = branch_cas_op("main", Some(oid2.clone()), oid3.clone());
            let h1 = tokio::spawn(async move {
                coord1
                    .commit_ref_cas_with_capture_inner(
                        &op3,
                        &[sample_session_item("conc:3", "sess-c3", "prov-c3", 3000)],
                        hook1,
                    )
                    .await
            });

            let g3_committed = tokio::task::spawn_blocking(move || {
                w1_git_done_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("writer 1 commits git.redb gen=3")
            })
            .await
            .expect("join wait");
            assert_eq!(g3_committed, 3);

            let coord2 = coord.clone();
            let op4 = branch_cas_op("main", Some(oid3.clone()), oid4.clone());
            let h2 = tokio::spawn(async move {
                coord2
                    .commit_ref_cas_with_capture_inner(
                        &op4,
                        &[sample_session_item("conc:4", "sess-c4", "prov-c4", 4000)],
                        hook2,
                    )
                    .await
            });

            let _ = h1
                .await
                .expect("join w1")
                .expect_err("w1 crashes after Step 1");
            let out4 = h2
                .await
                .expect("join w2")
                .expect("w2 replays gen=3 then commits gen=4");
            assert_eq!(out4.generation, 4);
            assert_eq!(out4.replayed_generations, vec![3]);
            assert_eq!(git.read_cross_store_generation().expect("git gen 4"), 4);
            assert_eq!(capture.read_cross_store_generation().expect("cap gen 4"), 4);
            assert!(
                capture
                    .get_session("sess-c3")
                    .await
                    .expect("sess-c3 replayed")
                    .is_some()
            );
            assert!(
                capture
                    .get_session("sess-c4")
                    .await
                    .expect("sess-c4 committed")
                    .is_some()
            );
        }

        // --- Sub-case C: Snapshot retries when `g1 == c1` AND `g1 != g2`, then succeeds ---
        let oid5 = sample_oid('5');
        {
            let git_for_hook = git.clone();
            let cap_for_hook = capture.clone();
            let oid4_clone = oid4.clone();
            let oid5_clone = oid5.clone();
            let hook_fires = Arc::new(AtomicUsize::new(0));
            let hook_fires_clone = Arc::clone(&hook_fires);

            let between_hook: SnapshotBetweenC1AndG2Hook = Arc::new(move |attempt, g1, c1| {
                if attempt == 0 {
                    assert_eq!(g1, 4);
                    assert_eq!(c1, 4);
                    hook_fires_clone.fetch_add(1, Ordering::SeqCst);
                    // Advance BOTH git.redb and capture.redb to gen=5 right between c1 and g2
                    // so attempt 0 observes g1=4, c1=4, g2=5 (`g1 == c1` but `g1 != g2`).
                    let op5 = branch_cas_op("main", Some(oid4_clone.clone()), oid5_clone.clone());
                    let items5 = [sample_session_item("conc:5", "sess-c5", "prov-c5", 5000)];
                    let cancel = AtomicU8::new(CANCEL_STATE_ACTIVE);
                    let res = commit_cross_store_blocking(
                        &git_for_hook,
                        &cap_for_hook,
                        &op5,
                        &items5,
                        &cancel,
                        &CrossStoreCommitHook::none(),
                    )
                    .expect("concurrent commit between c1 and g2");
                    assert_eq!(res.generation, 5);
                }
            });

            let git_ro = RedbGitStore::open_read_only(&git_path).expect("open git ro");
            let cap_ro = RedbCaptureStore::open_read_only(&cap_path)
                .await
                .expect("open cap ro");
            let ro_coord = CrossStoreCoordinator::new(git_ro, cap_ro)
                .with_snapshot_retry_backoff(Duration::ZERO);

            let snap = ro_coord
                .snapshot_consistent_read_only_with_hook(between_hook)
                .await
                .expect("snapshot must retry on g1 != g2 and succeed on attempt 2");
            assert_eq!(hook_fires.load(Ordering::SeqCst), 1);
            assert_eq!(snap.attempts(), 2);
            assert_eq!(snap.generation(), 5);
            assert_eq!(
                snap.get_git_ref(&RefKey::LocalBranch {
                    name: "main".to_string()
                })
                .expect("read main in snap"),
                Some(RefTarget::Direct(oid5.clone()))
            );
            assert!(
                snap.get_capture_session("sess-c5")
                    .expect("read sess-c5 in snap")
                    .is_some()
            );
        }

        // --- Sub-case D: Persistent `g1 == c1 != g2` across all retries fails closed ---
        {
            let git_for_hook = git.clone();
            let cap_for_hook = capture.clone();
            let prev_oid = Arc::new(std::sync::Mutex::new(oid5));
            let probe_count = Arc::new(AtomicUsize::new(0));
            let probe_count_clone = Arc::clone(&probe_count);

            let always_advance: SnapshotBetweenC1AndG2Hook = Arc::new(move |attempt, g1, c1| {
                assert_eq!(g1, c1, "each probe starts with g1 == c1 before g2 races");
                let n = probe_count_clone.fetch_add(1, Ordering::SeqCst);
                let mut guard = prev_oid.lock().expect("lock oid");
                let next_oid = format!("{:040x}", 0x1000usize + n);
                let op = branch_cas_op("main", Some(guard.clone()), next_oid.clone());
                let items = [sample_session_item(
                    &format!("conc:race:{attempt}"),
                    &format!("sess-race-{attempt}"),
                    &format!("prov-race-{attempt}"),
                    6000 + attempt as i64,
                )];
                let cancel = AtomicU8::new(CANCEL_STATE_ACTIVE);
                let _ = commit_cross_store_blocking(
                    &git_for_hook,
                    &cap_for_hook,
                    &op,
                    &items,
                    &cancel,
                    &CrossStoreCommitHook::none(),
                )
                .expect("advance git+capture between c1 and g2");
                *guard = next_oid;
            });

            let git_ro = RedbGitStore::open_read_only(&git_path).expect("open git ro");
            let cap_ro = RedbCaptureStore::open_read_only(&cap_path)
                .await
                .expect("open cap ro");
            let ro_coord = CrossStoreCoordinator::new(git_ro, cap_ro)
                .with_snapshot_retry_backoff(Duration::ZERO);

            let err = ro_coord
                .snapshot_consistent_read_only_with_hook(always_advance)
                .await
                .expect_err("persistent g1 != g2 must fail closed after 5 retries");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert!(is_cross_store_inconsistent_snapshot_error(&err));
            assert_eq!(
                probe_count.load(Ordering::SeqCst),
                CROSS_STORE_SNAPSHOT_MAX_RETRIES + 1,
                "must execute initial probe + 5 retries"
            );
            // On the final probe, g1 == c1 == 10 and g2 == 11.
            assert!(
                err.to_string()
                    .contains("git_g1=10, capture_c1=10, git_g2=11")
            );
        }
    }

    /// Codex R2 P1 regression: dropping or aborting the async
    /// `commit_ref_cas_with_capture` future while blocked on either the
    /// `git.redb` write lock or the `capture.redb` write lock MUST trip the
    /// cooperative pre-commit cancellation boundary so the blocking worker
    /// rolls back both transactions cleanly and mutates NEITHER store.
    #[tokio::test]
    async fn cancelled_async_commit_does_not_mutate_either_store() {
        use std::sync::mpsc;

        let dir = tempdir().expect("tempdir");
        let git_path = dir.path().join("redb").join("git.gen-00000001.redb");
        let cap_path = dir.path().join("private").join("capture.gen-00000001.redb");
        fs::create_dir_all(git_path.parent().expect("git parent")).expect("create redb dir");

        let git = RedbGitStore::create(&git_path).expect("create git.redb");
        let capture = RedbCaptureStore::create(&cap_path)
            .await
            .expect("create capture.redb");
        let coord = CrossStoreCoordinator::new(git.clone(), capture.clone())
            .with_snapshot_retry_backoff(Duration::ZERO);

        // Case 1: Cancelled while blocked waiting for `git.redb` write lock.
        {
            let holder_git_wtx = git.begin_checked_write_tx().expect("hold git_wtx");
            let (entered_tx, entered_rx) = mpsc::channel::<()>();
            let (exited_tx, exited_rx) = mpsc::channel::<()>();
            let entered_tx = Arc::new(std::sync::Mutex::new(Some(entered_tx)));
            let exited_tx = Arc::new(std::sync::Mutex::new(Some(exited_tx)));

            let hook = CrossStoreCommitHook {
                on_blocking_enter: Some(Arc::new(move || {
                    if let Some(tx) = entered_tx.lock().expect("lock").take() {
                        tx.send(()).expect("signal entered");
                    }
                })),
                on_blocking_exit: Some(Arc::new(move || {
                    if let Some(tx) = exited_tx.lock().expect("lock").take() {
                        tx.send(()).expect("signal exited");
                    }
                })),
                ..CrossStoreCommitHook::none()
            };

            let coord_clone = coord.clone();
            let op = branch_cas_op("main", None, sample_oid('a'));
            let handle = tokio::spawn(async move {
                coord_clone
                    .commit_ref_cas_with_capture_inner(
                        &op,
                        &[sample_session_item(
                            "cancel:git",
                            "sess-cancel-git",
                            "prov-cancel-git",
                            1000,
                        )],
                        hook,
                    )
                    .await
            });

            // Wait until the blocking worker has definitely started and is blocked on git_wtx.
            tokio::task::spawn_blocking(move || {
                entered_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("worker entered blocking task")
            })
            .await
            .expect("join entered");

            // Abort the async future so CrossStoreCancelGuard trips before git_wtx releases.
            handle.abort();
            let _ = handle.await;

            // Release git_wtx and wait for the background blocking worker to finish unwinding.
            drop(holder_git_wtx);
            tokio::task::spawn_blocking(move || {
                exited_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("cancelled blocking worker must exit cleanly")
            })
            .await
            .expect("join exited");

            assert_eq!(git.read_cross_store_generation().expect("git gen 0"), 0);
            assert_eq!(capture.read_cross_store_generation().expect("cap gen 0"), 0);
            assert!(
                git.list_cross_store_intent_generations()
                    .expect("intents")
                    .is_empty()
            );
            assert_eq!(
                git.get_ref(&RefKey::LocalBranch {
                    name: "main".to_string()
                })
                .await
                .expect("read main"),
                None
            );
            assert!(
                capture
                    .get_session("sess-cancel-git")
                    .await
                    .expect("read cancelled session")
                    .is_none()
            );
        }

        // Case 2: Cancelled after acquiring `git_wtx`, while blocked waiting for `capture.redb` write lock.
        {
            let holder_cap_wtx = capture.begin_checked_write_tx().expect("hold cap_wtx");
            let (got_git_tx, got_git_rx) = mpsc::channel::<u64>();
            let (exited_tx, exited_rx) = mpsc::channel::<()>();
            let got_git_tx = Arc::new(std::sync::Mutex::new(Some(got_git_tx)));
            let exited_tx = Arc::new(std::sync::Mutex::new(Some(exited_tx)));

            let hook = CrossStoreCommitHook {
                after_git_wtx_acquired: Some(Arc::new(move |git_gen| {
                    if let Some(tx) = got_git_tx.lock().expect("lock").take() {
                        tx.send(git_gen).expect("signal git_wtx acquired");
                    }
                })),
                on_blocking_exit: Some(Arc::new(move || {
                    if let Some(tx) = exited_tx.lock().expect("lock").take() {
                        tx.send(()).expect("signal exited");
                    }
                })),
                ..CrossStoreCommitHook::none()
            };

            let coord_clone = coord.clone();
            let op = branch_cas_op("main", None, sample_oid('b'));
            let handle = tokio::spawn(async move {
                coord_clone
                    .commit_ref_cas_with_capture_inner(
                        &op,
                        &[sample_session_item(
                            "cancel:cap",
                            "sess-cancel-cap",
                            "prov-cancel-cap",
                            2000,
                        )],
                        hook,
                    )
                    .await
            });

            let seen_gen = tokio::task::spawn_blocking(move || {
                got_git_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("worker acquired git_wtx and is now waiting on cap_wtx")
            })
            .await
            .expect("join git_wtx signal");
            assert_eq!(seen_gen, 0);

            handle.abort();
            let _ = handle.await;

            drop(holder_cap_wtx);
            tokio::task::spawn_blocking(move || {
                exited_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("cancelled blocking worker must exit cleanly")
            })
            .await
            .expect("join exited");

            assert_eq!(git.read_cross_store_generation().expect("git gen 0"), 0);
            assert_eq!(capture.read_cross_store_generation().expect("cap gen 0"), 0);
            assert!(
                git.list_cross_store_intent_generations()
                    .expect("intents")
                    .is_empty()
            );
            assert_eq!(
                git.get_ref(&RefKey::LocalBranch {
                    name: "main".to_string()
                })
                .await
                .expect("read main"),
                None
            );
            assert!(
                capture
                    .get_session("sess-cancel-cap")
                    .await
                    .expect("read cancelled session")
                    .is_none()
            );
        }

        // Case 3 (Codex R3 P1): Pending intent exists (`git_gen=1, cap_gen=0`),
        // and caller aborts while the worker is blocked waiting on `cap_wtx`.
        // Must NOT replay or commit pending intent `1` to `capture.redb`, and
        // must NOT advance `git.redb` to `2`.
        {
            let oid1 = sample_oid('1');
            let _ = coord
                .commit_ref_cas_with_capture_inner(
                    &branch_cas_op("main", None, oid1.clone()),
                    &[sample_session_item(
                        "cancel:pending1",
                        "sess-pending-1",
                        "prov-pending-1",
                        3000,
                    )],
                    CrossStoreCommitHook::fail_after_git_commit(),
                )
                .await
                .expect_err("seed partial commit at gen=1");
            assert_eq!(git.read_cross_store_generation().expect("git gen 1"), 1);
            assert_eq!(capture.read_cross_store_generation().expect("cap gen 0"), 0);

            let holder_cap_wtx = capture.begin_checked_write_tx().expect("hold cap_wtx");
            let (got_git_tx, got_git_rx) = mpsc::channel::<u64>();
            let (exited_tx, exited_rx) = mpsc::channel::<()>();
            let got_git_tx = Arc::new(std::sync::Mutex::new(Some(got_git_tx)));
            let exited_tx = Arc::new(std::sync::Mutex::new(Some(exited_tx)));

            let hook = CrossStoreCommitHook {
                after_git_wtx_acquired: Some(Arc::new(move |git_gen| {
                    if let Some(tx) = got_git_tx.lock().expect("lock").take() {
                        tx.send(git_gen).expect("signal git_wtx acquired");
                    }
                })),
                on_blocking_exit: Some(Arc::new(move || {
                    if let Some(tx) = exited_tx.lock().expect("lock").take() {
                        tx.send(()).expect("signal exited");
                    }
                })),
                ..CrossStoreCommitHook::none()
            };

            let coord_clone = coord.clone();
            let op2 = branch_cas_op("main", Some(oid1.clone()), sample_oid('2'));
            let handle = tokio::spawn(async move {
                coord_clone
                    .commit_ref_cas_with_capture_inner(
                        &op2,
                        &[sample_session_item(
                            "cancel:pending2",
                            "sess-cancel-2",
                            "prov-cancel-2",
                            4000,
                        )],
                        hook,
                    )
                    .await
            });

            let seen_gen = tokio::task::spawn_blocking(move || {
                got_git_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("worker acquired git_wtx (gen=1) and is now waiting on cap_wtx")
            })
            .await
            .expect("join git_wtx signal");
            assert_eq!(seen_gen, 1);

            handle.abort();
            let _ = handle.await;

            drop(holder_cap_wtx);
            tokio::task::spawn_blocking(move || {
                exited_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("cancelled blocking worker must exit without replaying")
            })
            .await
            .expect("join exited");

            // Neither store changed: git stays at gen=1 (ref=oid1), capture stays at gen=0
            // without replaying sess-pending-1 or applying sess-cancel-2.
            assert_eq!(
                git.read_cross_store_generation().expect("git gen still 1"),
                1
            );
            assert_eq!(
                capture
                    .read_cross_store_generation()
                    .expect("cap gen still 0"),
                0
            );
            assert_eq!(
                git.list_cross_store_intent_generations().expect("intents"),
                vec![1]
            );
            assert_eq!(
                git.get_ref(&RefKey::LocalBranch {
                    name: "main".to_string()
                })
                .await
                .expect("main ref still oid1"),
                Some(RefTarget::Direct(oid1.clone()))
            );
            assert!(
                capture
                    .get_session("sess-pending-1")
                    .await
                    .expect("sess-pending-1 must not be replayed on cancelled commit")
                    .is_none()
            );
            assert!(
                capture
                    .get_session("sess-cancel-2")
                    .await
                    .expect("sess-cancel-2 must not be written")
                    .is_none()
            );

            // Case 4 (Codex R4 P1): Caller aborts AFTER `ReplayCommitBoundaryGuard::enter`
            // has been entered and WHILE replay commit is in flight. Replay of already-committed
            // intent `1` completes cleanly, `ReplayCommitBoundaryGuard::drop` preserves
            // `CANCEL_BIT_CANCELLED`, and the worker aborts before committing new gen `2`.
            let (in_boundary_tx, in_boundary_rx) = mpsc::channel::<()>();
            let (resume_replay_tx, resume_replay_rx) = mpsc::channel::<()>();
            let (exited4_tx, exited4_rx) = mpsc::channel::<()>();
            let in_boundary_tx = Arc::new(std::sync::Mutex::new(Some(in_boundary_tx)));
            let resume_replay_rx = Arc::new(std::sync::Mutex::new(resume_replay_rx));
            let exited4_tx = Arc::new(std::sync::Mutex::new(Some(exited4_tx)));

            let hook4 = CrossStoreCommitHook {
                on_inside_replay_commit_boundary: Some(Arc::new(move || {
                    if let Some(tx) = in_boundary_tx.lock().expect("lock").take() {
                        tx.send(()).expect("signal entered replay commit boundary");
                    }
                    resume_replay_rx
                        .lock()
                        .expect("lock")
                        .recv_timeout(Duration::from_secs(10))
                        .expect("resume replay commit after caller abort");
                })),
                on_blocking_exit: Some(Arc::new(move || {
                    if let Some(tx) = exited4_tx.lock().expect("lock").take() {
                        tx.send(()).expect("signal case 4 worker exited");
                    }
                })),
                ..CrossStoreCommitHook::none()
            };

            let coord4 = coord.clone();
            let op4 = branch_cas_op("main", Some(oid1.clone()), sample_oid('4'));
            let handle4 = tokio::spawn(async move {
                coord4
                    .commit_ref_cas_with_capture_inner(
                        &op4,
                        &[sample_session_item(
                            "cancel:during-replay",
                            "sess-cancel-during-replay",
                            "prov-cancel-during-replay",
                            5000,
                        )],
                        hook4,
                    )
                    .await
            });

            // Wait until the worker is strictly inside `ReplayCommitBoundaryGuard`.
            tokio::task::spawn_blocking(move || {
                in_boundary_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("worker entered replay commit boundary")
            })
            .await
            .expect("join in_boundary");

            // Abort the async future WHILE `CANCEL_BIT_IN_REPLAY_COMMIT` is held.
            handle4.abort();
            let _ = handle4.await;

            // Allow replay commit to finish and observe `ReplayCommitBoundaryGuard::drop`.
            resume_replay_tx.send(()).expect("release replay commit");
            tokio::task::spawn_blocking(move || {
                exited4_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("case 4 worker must exit cleanly without committing gen=2")
            })
            .await
            .expect("join exited4");

            // Replay healed gen=1 in capture.redb, AND cancellation was preserved so
            // git.redb did NOT advance to gen=2 and `sess-cancel-during-replay` was NOT written!
            assert_eq!(
                git.read_cross_store_generation().expect("git gen still 1"),
                1
            );
            assert_eq!(
                capture
                    .read_cross_store_generation()
                    .expect("cap gen healed to 1"),
                1
            );
            assert_eq!(
                git.list_cross_store_intent_generations().expect("intents"),
                vec![1]
            );
            assert_eq!(
                git.get_ref(&RefKey::LocalBranch {
                    name: "main".to_string()
                })
                .await
                .expect("main ref still oid1"),
                Some(RefTarget::Direct(oid1))
            );
            assert!(
                capture
                    .get_session("sess-pending-1")
                    .await
                    .expect("sess-pending-1 healed by in-flight replay")
                    .is_some()
            );
            assert!(
                capture
                    .get_session("sess-cancel-during-replay")
                    .await
                    .expect("new gen=2 capture item must NOT be written after abort")
                    .is_none()
            );
        }
    }
}
