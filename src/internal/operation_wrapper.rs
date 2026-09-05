//! Transaction wrapper contract for operation-level audit logging.
//!
//! Commit 1 introduces only stable wrapper-facing types that are required by
//! A-5: metadata, snapshot scope, wrapper result, and stage-specific errors.
//! Commit 2 adds transaction skeleton execution (begin -> business -> commit)
//! without snapshot capture/persistence.

use std::{
    collections::HashSet,
    future::Future,
    pin::Pin,
    sync::{Mutex, OnceLock},
    time::{Duration, Instant},
};

use chrono::Utc;
use sea_orm::{
    ColumnTrait, DatabaseConnection, DatabaseTransaction, DbErr, EntityTrait, QueryFilter,
};
use thiserror::Error;
use tokio::time::sleep;
use uuid::Uuid;

use crate::internal::{
    branch::Branch,
    head::Head,
    model::reference,
    operation::{
        OperationGraphRecord, OperationParentRecord, OperationQueryPage, OperationRecord,
        OperationService, OperationStatus, OperationViewRecord, OperationViewRefRecord,
        OperationViewWorkspaceRecord,
    },
};

const PARENT_RESOLUTION_PAGE_SIZE: u64 = 200;
const DEDUP_WINDOW_SECS: i64 = 5;
/// How many already-matching candidates the duplicate query needs. One is
/// enough to refuse; a handful keeps the check honest if timestamps tie.
const DEDUP_CANDIDATE_LIMIT: u64 = 8;
const SQLITE_BUSY_MAX_RETRIES: usize = 8;
const SQLITE_BUSY_RETRY_BASE_MS: u64 = 25;

static ACTIVE_OPERATION_KEYS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParentSelectionMode {
    SingleLatestSuccess,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentSelectionResult {
    pub selected: Vec<String>,
    pub scanned_pages: u64,
    pub scanned_items: u64,
    pub success_candidates: u64,
    pub mode: ParentSelectionMode,
}

/// Required command metadata captured by `with_operation_log`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationMeta {
    pub command_name: String,
    pub description: String,
    pub actor: String,
    pub repo_id: String,
    pub args_digest: Option<String>,
}

impl OperationMeta {
    /// The digest as it is STORED and QUERIED: trimmed, empty treated as
    /// absent.
    ///
    /// The duplicate check is an SQL equality now, so the value written and
    /// the value searched for have to be the same string. They were not: the
    /// old in-memory comparison trimmed both sides, so a caller passing
    /// `Some(" digest ")` persisted the padding, and the next identical
    /// submission searched for `digest`, found nothing, and skipped
    /// deduplication entirely.
    pub(crate) fn normalized_digest(&self) -> Option<&str> {
        self.args_digest
            .as_deref()
            // ASCII whitespace, NOT `str::trim`'s Unicode set: the SQL
            // migration that canonicalizes existing rows can only express the
            // ASCII set, and the two definitions have to be the SAME
            // function or a legacy row and a new submission disagree again.
            // A digest is a hex/prefixed ASCII token, so nothing legitimate
            // is left untrimmed.
            .map(|digest| digest.trim_matches(|c: char| c.is_ascii_whitespace()))
            .filter(|digest| !digest.is_empty())
    }

    /// Validate required fields before entering transaction orchestration.
    pub fn validate(&self) -> Result<(), OperationError> {
        if self.command_name.trim().is_empty() {
            return Err(OperationError::validation("command_name must not be empty"));
        }
        if self.description.trim().is_empty() {
            return Err(OperationError::validation("description must not be empty"));
        }
        if self.actor.trim().is_empty() {
            return Err(OperationError::validation("actor must not be empty"));
        }
        if self.repo_id.trim().is_empty() {
            return Err(OperationError::validation("repo_id must not be empty"));
        }

        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationParentPolicy {
    pub allow_multi_parent: bool,
    pub max_parents: usize,
}

impl Default for OperationParentPolicy {
    fn default() -> Self {
        Self {
            allow_multi_parent: false,
            max_parents: 1,
        }
    }
}

/// Controls which parts of the final repository view should be captured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OperationScope {
    pub include_refs: bool,
    pub include_workspace: bool,
    pub include_remote_tracking: bool,
    pub parent_policy: OperationParentPolicy,
    /// Whether the five-second succeeded-window duplicate check applies.
    ///
    /// It is a heuristic for accidental double submission, and it fits a
    /// repository-scope command whose repeat within five seconds is almost
    /// certainly a slip. It does NOT fit a sequencer control: re-running
    /// `rebase --continue` at the same position is ordinary — the last
    /// `--continue` dropped an empty commit, a hook was fixed, an editor was
    /// aborted — and refusing it would break driving a sequence. Control
    /// actions are excluded from the window and rely on the worktree-wide
    /// control slot, which is a real mutex rather than a heuristic.
    pub duplicate_window: bool,
    /// What this operation OWNS, declared by the caller (§C.4.1 / §C.9).
    ///
    /// The wrapper can derive `main` vs `linked` from the request scope, but it
    /// cannot know whether the state an operation mutates is worktree-local or
    /// repository-wide — only the caller knows that. A `branch` reset moves a
    /// SHARED ref, and its snapshot contains every local branch, so replaying
    /// it into one worktree touches state every worktree depends on.
    pub ownership: OperationOwnership,
}

/// Whether an operation's effects belong to one worktree or the repository.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum OperationOwnership {
    /// Worktree-local: HEAD, the index, this worktree's own refs.
    #[default]
    Worktree,
    /// Repository-wide: shared branch refs, notes, anything every worktree
    /// reads. Recorded with `scope_kind = "repository"`, which `op restore`
    /// refuses — repository-wide recovery is deferred to LR-02 (§C.9).
    Repository,
}

impl Default for OperationScope {
    fn default() -> Self {
        Self {
            include_refs: true,
            include_workspace: true,
            include_remote_tracking: false,
            parent_policy: OperationParentPolicy::default(),
            duplicate_window: true,
            ownership: OperationOwnership::Worktree,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParentSelectionMetrics {
    pub resolver_mode: ParentSelectionMode,
    pub scanned_pages: u64,
    pub scanned_items: u64,
    pub success_candidates: u64,
    pub selected_parent_count: u64,
    pub selection_latency_us: u64,
}

/// Wrapper return shape: business result and operation identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationResult<T> {
    pub payload: T,
    pub op_id: String,
    pub view_id: String,
    pub end_ts: i64,
    pub view: OperationViewSnapshot,
    pub parent_metrics: ParentSelectionMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationViewSnapshot {
    pub head_kind: String,
    pub head_target: String,
    pub refs: Vec<OperationViewRefRecord>,
    pub workspace: Vec<OperationViewWorkspaceRecord>,
}

/// Stage-specific failures for with_operation_log.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OperationError {
    #[error("invalid operation metadata: {0}")]
    Validation(String),
    #[error("failed to begin operation transaction: {0}")]
    Begin(String),
    #[error("operation business write failed: {0}")]
    Business(String),
    #[error("failed to capture operation snapshot: {0}")]
    Snapshot(String),
    #[error("failed to persist operation record: {0}")]
    Persist(String),
    #[error("failed to commit operation transaction: {0}")]
    Commit(String),
    #[error("failed to rollback operation transaction: {0}")]
    Rollback(String),
}

impl OperationError {
    pub fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    pub fn begin(message: impl Into<String>) -> Self {
        Self::Begin(message.into())
    }

    pub fn business(message: impl Into<String>) -> Self {
        Self::Business(message.into())
    }

    pub fn snapshot(message: impl Into<String>) -> Self {
        Self::Snapshot(message.into())
    }

    pub fn persist(message: impl Into<String>) -> Self {
        Self::Persist(message.into())
    }

    pub fn commit(message: impl Into<String>) -> Self {
        Self::Commit(message.into())
    }

    pub fn rollback(message: impl Into<String>) -> Self {
        Self::Rollback(message.into())
    }
}

fn is_sqlite_busy_operation_error(err: &OperationError) -> bool {
    match err {
        OperationError::Begin(message)
        | OperationError::Snapshot(message)
        | OperationError::Persist(message)
        | OperationError::Commit(message)
        | OperationError::Rollback(message)
        | OperationError::Business(message) => {
            message.contains("database is locked") || message.contains("database schema is locked")
        }
        OperationError::Validation(_) => false,
    }
}

/// Part C W1 (§C.9): the dedup key includes the WORKTREE scope — the same
/// command with identical arguments run concurrently in two worktrees is two
/// legitimate operations, not a duplicate submission.
/// The scope component of a dedup identity.
///
/// A REPOSITORY-scope operation must deduplicate repository-wide: two identical
/// resets of the same shared ref from two worktrees are the same operation, and
/// keying them by worktree would let both through.
fn dedup_scope_key(scope_key: &str, ownership: OperationOwnership) -> Option<String> {
    match ownership {
        // No worktree dimension at all: the SQL filter is omitted and the
        // in-process key uses a sentinel no storage key can collide with.
        OperationOwnership::Repository => None,
        OperationOwnership::Worktree => Some(scope_key.to_string()),
    }
}

fn operation_dedup_key(meta: &OperationMeta, scope_key: Option<&str>) -> Option<String> {
    let scope = scope_key.unwrap_or("\u{0}repository");
    meta.normalized_digest().map(|digest| {
        format!(
            "{}::{}::{}::{}",
            meta.repo_id, scope, meta.command_name, digest
        )
    })
}

struct ActiveDedupGuard {
    key: String,
}

impl Drop for ActiveDedupGuard {
    fn drop(&mut self) {
        if let Some(lock) = ACTIVE_OPERATION_KEYS.get()
            && let Ok(mut keys) = lock.lock()
        {
            keys.remove(&self.key);
        }
    }
}

fn try_acquire_active_dedup_guard(key: String) -> Result<ActiveDedupGuard, OperationError> {
    let lock = ACTIVE_OPERATION_KEYS.get_or_init(|| Mutex::new(HashSet::new()));
    let mut keys = lock
        .lock()
        .map_err(|_| OperationError::begin("failed to lock active operation key set"))?;
    if keys.contains(&key) {
        return Err(OperationError::business(format!(
            "duplicate operation in progress for key '{}'",
            key
        )));
    }
    keys.insert(key.clone());
    Ok(ActiveDedupGuard { key })
}

async fn ensure_not_recent_duplicate_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    meta: &OperationMeta,
    now_ts: i64,
    scope_key: Option<&str>,
) -> Result<(), OperationError> {
    let Some(digest) = meta.normalized_digest() else {
        return Ok(());
    };

    // Part C W1 (§C.9): a SCOPE POINT QUERY, not a repo-wide page filtered in
    // memory. The old form took the newest 50 rows for the whole repository
    // and kept the ones belonging to this worktree — so fifty newer
    // operations in OTHER worktrees pushed a same-scope operation out of the
    // window, and the duplicate it should have refused went through. Every
    // predicate is in the query now, including the time window, so the limit
    // bounds a set that is already the right one.
    let earliest_end_ts = now_ts.saturating_sub(DEDUP_WINDOW_SECS);
    let records = OperationService::recent_duplicate_candidates_with_conn(
        db,
        &meta.repo_id,
        scope_key,
        &meta.command_name,
        digest,
        earliest_end_ts,
        DEDUP_CANDIDATE_LIMIT,
    )
    .await
    .map_err(|err| {
        OperationError::begin(format!(
            "failed to query recent operations for repository '{}': {err}",
            meta.repo_id
        ))
    })?;

    // The query already matched scope, command, digest, status and the window
    // from below; this only re-checks the upper bound (a row cannot end in
    // the future).
    let duplicated = records.into_iter().any(|record| {
        record
            .end_ts
            .map(|end_ts| now_ts.saturating_sub(end_ts) <= DEDUP_WINDOW_SECS)
            .unwrap_or(false)
    });

    if duplicated {
        return Err(OperationError::business(format!(
            "duplicate operation rejected within {}s window for command '{}'",
            DEDUP_WINDOW_SECS, meta.command_name
        )));
    }

    Ok(())
}

fn validate_parent_policy(policy: OperationParentPolicy) -> Result<(), OperationError> {
    if policy.max_parents == 0 {
        return Err(OperationError::validation(
            "parent_policy.max_parents must be greater than 0",
        ));
    }
    if !policy.allow_multi_parent && policy.max_parents > 1 {
        return Err(OperationError::validation(
            "parent_policy.max_parents must be 1 when allow_multi_parent is false",
        ));
    }
    Ok(())
}

/// Execute one business write closure in a transaction and return operation ids.
///
/// Commit 2 scope:
/// 1. Validate metadata.
/// 2. Begin transaction.
/// 3. Execute business closure.
/// 4. Commit on success, rollback on business failure.
///
/// Snapshot capture and operation graph persistence are added in later commits.
pub async fn with_operation_log<T, F>(
    meta: OperationMeta,
    scope: OperationScope,
    operation: F,
) -> Result<OperationResult<T>, OperationError>
where
    for<'b> F: FnOnce(
        &'b DatabaseTransaction,
    ) -> Pin<Box<dyn Future<Output = Result<T, DbErr>> + Send + 'b>>,
    F: Send + 'static,
{
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(OperationError::begin)?;
    with_operation_log_with_conn(&db, meta, scope, operation).await
}

/// Same as [`with_operation_log`] but uses caller-provided database connection.
///
/// This helper is designed for tests and advanced internal callers.
pub async fn with_operation_log_with_conn<T, F>(
    db: &DatabaseConnection,
    meta: OperationMeta,
    scope: OperationScope,
    operation: F,
) -> Result<OperationResult<T>, OperationError>
where
    for<'b> F: FnOnce(
        &'b DatabaseTransaction,
    ) -> Pin<Box<dyn Future<Output = Result<T, DbErr>> + Send + 'b>>,
    F: Send + 'static,
{
    meta.validate()?;
    validate_parent_policy(scope.parent_policy)?;

    let op_id = Uuid::now_v7().to_string();
    let view_id = Uuid::now_v7().to_string();
    let start_ts = Utc::now().timestamp();

    let scope_key = crate::internal::worktree_scope::WorktreeScope::for_request()
        .storage_key()
        .to_string();
    let dedup_scope = dedup_scope_key(&scope_key, scope.ownership);
    let _active_dedup_guard = operation_dedup_key(&meta, dedup_scope.as_deref())
        .map(try_acquire_active_dedup_guard)
        .transpose()?;

    ensure_not_recent_duplicate_with_conn(db, &meta, start_ts, dedup_scope.as_deref()).await?;

    let mut txn = None;
    let mut parent_selection = None;
    for attempt in 0..=SQLITE_BUSY_MAX_RETRIES {
        let opened = crate::internal::db::begin_write_transaction(db)
            .await
            .map_err(|err| {
                OperationError::begin(format!(
                    "failed to open operation transaction for command '{}': {err}",
                    meta.command_name
                ))
            });

        let opened = match opened {
            Ok(v) => v,
            Err(err)
                if is_sqlite_busy_operation_error(&err) && attempt < SQLITE_BUSY_MAX_RETRIES =>
            {
                sleep(Duration::from_millis(
                    SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                ))
                .await;
                continue;
            }
            Err(err) => return Err(err),
        };

        let selection_started_at = Instant::now();
        let selected = resolve_parent_selection_with_conn(
            &opened,
            &meta.repo_id,
            ParentSelectionMode::SingleLatestSuccess,
        )
        .await;

        match selected {
            Ok(result) => {
                parent_selection =
                    Some((result, selection_started_at.elapsed().as_micros() as u64));
                txn = Some(opened);
                break;
            }
            Err(err)
                if is_sqlite_busy_operation_error(&err) && attempt < SQLITE_BUSY_MAX_RETRIES =>
            {
                let _ = opened.rollback().await;
                sleep(Duration::from_millis(
                    SQLITE_BUSY_RETRY_BASE_MS * (attempt as u64 + 1),
                ))
                .await;
            }
            Err(err) => {
                let _ = opened.rollback().await;
                return Err(err);
            }
        }
    }

    let txn = txn.ok_or_else(|| {
        OperationError::begin("failed to initialize operation transaction after retries")
    })?;
    let (parent_selection, selection_latency_us) = parent_selection
        .ok_or_else(|| OperationError::begin("failed to resolve parent selection after retries"))?;
    let selected_parents = parent_selection
        .selected
        .into_iter()
        .take(scope.parent_policy.max_parents)
        .collect::<Vec<_>>();

    let payload = match operation(&txn).await {
        Ok(payload) => payload,
        Err(err) => {
            txn.rollback().await.map_err(|rollback_err| {
                OperationError::rollback(format!(
                    "business step failed with '{err}', and rollback also failed: {rollback_err}"
                ))
            })?;
            return Err(OperationError::business(format!(
                "command '{}' business write failed: {err}",
                meta.command_name
            )));
        }
    };

    let end_ts = Utc::now().timestamp();
    let view = collect_final_view_with_conn(&txn, &meta.repo_id, &view_id, scope)
        .await
        .map_err(|err| {
            OperationError::snapshot(format!(
                "failed to collect final transactional view for command '{}': {err}",
                meta.command_name
            ))
        })?;

    let selected_parent_count = selected_parents.len() as u64;
    let parent_metrics = ParentSelectionMetrics {
        resolver_mode: parent_selection.mode,
        scanned_pages: parent_selection.scanned_pages,
        scanned_items: parent_selection.scanned_items,
        success_candidates: parent_selection.success_candidates,
        selected_parent_count,
        selection_latency_us,
    };
    let operation_record = OperationRecord {
        op_id: op_id.clone(),
        repo_id: meta.repo_id.clone(),
        view_id: view_id.clone(),
        command_name: meta.command_name.clone(),
        description: format!(
            "{} | resolver_mode={} scanned_pages={} scanned_items={} success_candidates={} selected_parents={} selection_latency_us={}",
            meta.description,
            match parent_metrics.resolver_mode {
                ParentSelectionMode::SingleLatestSuccess => "single_latest_success",
            },
            parent_metrics.scanned_pages,
            parent_metrics.scanned_items,
            parent_metrics.success_candidates,
            parent_metrics.selected_parent_count,
            parent_metrics.selection_latency_us,
        ),
        actor: meta.actor.clone(),
        // STORED normalized, so the SQL equality the duplicate check uses
        // matches what a later identical submission searches for.
        args_digest: meta.normalized_digest().map(str::to_string),
        start_ts,
        end_ts: Some(end_ts),
        status: OperationStatus::Succeeded,
        worktree_id: scope_key.clone(),
        // W0 §C.11: this process resolved its own scope, so the value is
        // DECLARED. Only migration 2026072902 ever writes `unknown`.
        scope_provenance: "declared".to_string(),
        scope_kind: declared_scope_kind(&scope_key, scope.ownership),
        // The closure form writes HEAD/refs inside the transaction, which is
        // exactly what the snapshot restores. Boundary-recorded control
        // actions declare otherwise — see `OperationBoundary`.
        restorable: true,
        control_slot: None,
        claim_owner: None,
    };
    let parents = selected_parents
        .into_iter()
        .map(|parent| OperationParentRecord {
            op_id: op_id.clone(),
            parent_op_id: parent,
        })
        .collect::<Vec<_>>();
    let graph = OperationGraphRecord {
        operation: operation_record,
        parents,
        view: OperationViewRecord {
            view_id: view_id.clone(),
            repo_id: meta.repo_id.clone(),
            head_kind: view.head_kind.clone(),
            head_target: view.head_target.clone(),
            created_at: end_ts,
        },
        refs: view.refs.clone(),
        workspace: view.workspace.clone(),
    };

    let persist_result = OperationService::persist_operation_graph_with_conn(&txn, &graph).await;
    if let Err(err) = persist_result {
        let persist_message = format!(
            "failed to persist operation graph for command '{}': {err}",
            meta.command_name
        );
        match txn.rollback().await {
            Ok(()) => return Err(OperationError::persist(persist_message)),
            Err(rollback_err) => {
                return Err(OperationError::rollback(format!(
                    "{persist_message}; rollback after persist failure also failed: {rollback_err}"
                )));
            }
        }
    }

    txn.commit().await.map_err(|err| {
        OperationError::commit(format!(
            "failed to commit operation transaction for command '{}': {err}",
            meta.command_name
        ))
    })?;

    Ok(OperationResult {
        payload,
        op_id,
        view_id,
        end_ts,
        view,
        parent_metrics,
    })
}

#[cfg(test)]
mod claim_owner_tests {
    use super::*;

    /// The owner string must let a LATER process decide three things: same
    /// machine incarnation, same pid, same process. Getting any of them wrong
    /// either revokes a live control action or leaves a dead claim forever.
    #[test]
    fn a_live_process_is_never_reported_gone() {
        let owner = claim_owner();
        assert!(
            !owner_is_gone(&owner),
            "this very process must not read as gone: {owner}"
        );
    }

    #[test]
    fn an_owner_from_another_machine_is_never_reclaimed() {
        // Same pid, different incarnation: a pid says nothing across machines
        // or across boots, so it must not be reclaimed.
        let alien = format!("some-other-machine/{}/1", std::process::id());
        assert!(!owner_is_gone(&alien));
    }

    #[test]
    fn an_owner_without_a_birth_token_is_never_reclaimed() {
        // Two fields only. A recycled pid would otherwise read as the same
        // process, so this shape is deliberately unreclaimable.
        let Some(machine) = claim_machine_identity() else {
            return; // platform cannot prove liveness at all; nothing to assert
        };
        let legacy = format!("{machine}/{}", std::process::id());
        assert!(!owner_is_gone(&legacy));
        assert!(!owner_is_gone("garbage"));
        assert!(!owner_is_gone(""));
    }

    #[test]
    fn a_recycled_pid_does_not_keep_a_dead_claim_alive() {
        let Some(machine) = claim_machine_identity() else {
            return;
        };
        // This pid exists, but with a DIFFERENT birth token — which is exactly
        // what a recycled pid looks like to the process that claimed first.
        let recycled = format!("{machine}/{}/not-the-original-birth", std::process::id());
        assert!(
            owner_is_gone(&recycled),
            "a live pid with another incarnation's birth token is a dead claim"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_pid_that_does_not_exist_is_gone() {
        let Some(machine) = claim_machine_identity() else {
            return;
        };
        // Pid 0 is rejected by shape; use a pid far above the usual maximum
        // that is overwhelmingly unlikely to exist.
        let dead = format!("{machine}/4194303/12345");
        // If that pid happens to exist on this machine the assertion would be
        // wrong, so only assert when it really is absent.
        let exists = unsafe { libc::kill(4194303, 0) } == 0;
        if !exists {
            assert!(owner_is_gone(&dead));
        }
    }
}

/// A durable, cross-process claim on one operation identity, held while the
/// command body runs OUTSIDE any transaction (plan-20260714 §C.9, §C.11 W1).
///
/// [`with_operation_log`] holds a write transaction for the whole closure. A
/// sequencer control cannot run inside one: it checks out trees and moves
/// refs through the POOLED entry points, and this codebase documents that
/// combination as a deadlock (`internal/head.rs:41`, `internal/branch.rs:298`).
/// Boundary recording splits the wrapper in two — a short transaction that
/// claims the identity and records the operation as `running`, the body on
/// pooled connections, then a short transaction that collects the view and
/// closes the row.
///
/// The claim is the INSERT itself, against the partial unique index from
/// migration 2026073003, so two processes racing the same identity cannot both
/// win: a query-then-insert would let them. A boundary-recorded operation is
/// never restorable — the snapshot covers HEAD and refs, and a control action
/// also moved an index, a working tree and sequencer state.
///
/// Dropping this value without calling [`OperationBoundary::finish`] leaves a
/// `running` row behind. That is deliberate: a crashed control action DID
/// change the repository, and a row that says so is more useful than silence.
/// `op restore` refuses `running` rows, so a stale claim can never be replayed.
#[must_use = "an unfinished boundary leaves the operation recorded as running"]
pub struct OperationBoundary {
    op_id: String,
    view_id: String,
    start_ts: i64,
    meta: OperationMeta,
    scope: OperationScope,
    scope_key: String,
    selected_parents: Vec<String>,
    parent_metrics: ParentSelectionMetrics,
    _dedup_guard: Option<ActiveDedupGuard>,
}

/// How a boundary-recorded operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryOutcome {
    Succeeded,
    Failed,
}

/// Open a boundary-recorded operation: dedup, then claim.
pub async fn begin_operation(
    meta: OperationMeta,
    scope: OperationScope,
) -> Result<OperationBoundary, OperationError> {
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(OperationError::begin)?;
    begin_operation_with_conn(&db, meta, scope).await
}

/// [`begin_operation`] against a caller-provided connection.
pub async fn begin_operation_with_conn(
    db: &DatabaseConnection,
    meta: OperationMeta,
    scope: OperationScope,
) -> Result<OperationBoundary, OperationError> {
    meta.validate()?;
    validate_parent_policy(scope.parent_policy)?;

    let op_id = Uuid::now_v7().to_string();
    let view_id = Uuid::now_v7().to_string();
    let start_ts = Utc::now().timestamp();

    let scope_key = crate::internal::worktree_scope::WorktreeScope::for_request()
        .storage_key()
        .to_string();
    let dedup_scope = dedup_scope_key(&scope_key, scope.ownership);
    let dedup_guard = operation_dedup_key(&meta, dedup_scope.as_deref())
        .map(try_acquire_active_dedup_guard)
        .transpose()?;

    // The in-process guard above is advisory; the INSERT below is what
    // actually excludes a concurrent claim. The five-second window is a
    // separate, heuristic rule — see `OperationScope::duplicate_window` for
    // why a control action is not subject to it.
    if scope.duplicate_window {
        ensure_not_recent_duplicate_with_conn(db, &meta, start_ts, dedup_scope.as_deref()).await?;
    }

    // The claim transaction READS (parent selection) before it WRITES (the
    // claim row), which is the one shape SQLite refuses to run its busy
    // handler for — it returns "database is locked" immediately rather than
    // waiting, so two worktrees starting a rebase at the same moment failed
    // one of them outright. Take the write lock up front; see
    // `db::begin_write_transaction`.
    let txn = crate::internal::db::begin_write_transaction(db)
        .await
        .map_err(|err| {
            OperationError::begin(format!(
                "failed to open the operation claim transaction for command '{}': {err}",
                meta.command_name
            ))
        })?;
    let selection_started_at = Instant::now();
    let selection = match resolve_parent_selection_with_conn(
        &txn,
        &meta.repo_id,
        ParentSelectionMode::SingleLatestSuccess,
    )
    .await
    {
        Ok(selection) => selection,
        Err(err) => {
            let _ = txn.rollback().await;
            return Err(err);
        }
    };
    let selection_latency_us = selection_started_at.elapsed().as_micros() as u64;
    let selected_parents = selection
        .selected
        .iter()
        .take(scope.parent_policy.max_parents)
        .cloned()
        .collect::<Vec<_>>();

    let claim = OperationRecord {
        op_id: op_id.clone(),
        repo_id: meta.repo_id.clone(),
        view_id: view_id.clone(),
        command_name: meta.command_name.clone(),
        description: meta.description.clone(),
        actor: meta.actor.clone(),
        args_digest: meta.normalized_digest().map(str::to_string),
        start_ts,
        end_ts: None,
        status: OperationStatus::Running,
        worktree_id: scope_key.clone(),
        scope_provenance: "declared".to_string(),
        scope_kind: declared_scope_kind(&scope_key, scope.ownership),
        restorable: false,
        // The worktree-wide control slot: what actually serializes DISTINCT
        // control actions in one worktree, which a per-identity key cannot.
        control_slot: Some("sequencer".to_string()),
        claim_owner: Some(claim_owner()),
    };
    if let Err(err) = OperationService::insert_operation_with_conn(&txn, &claim).await {
        let text = err.to_string().to_ascii_lowercase();
        if !(text.contains("unique constraint failed") || text.contains("constraint violation")) {
            let _ = txn.rollback().await;
            return Err(OperationError::persist(format!(
                "failed to claim operation for command '{}': {err}",
                meta.command_name
            )));
        }

        // This worktree's control slot is taken. Whether that is a live
        // command or the wreckage of a killed one is decided by PROOF, not by
        // age: a control action can legitimately sit for a long time in an
        // editor or a hook, and revoking a live claim is worse than refusing
        // a dead one.
        match reclaim_dead_claim(&txn, &claim, start_ts).await {
            Ok(Reclaim::TookOver { op_id, command }) => {
                crate::utils::error::emit_warning(format!(
                    "released the control claim of '{command}' (operation {}) — the process that \
                      held it is gone; its record is kept as failed",
                    &op_id[..8.min(op_id.len())]
                ));
            }
            Ok(Reclaim::Held {
                command,
                owner,
                age,
            }) => {
                let _ = txn.rollback().await;
                return Err(OperationError::business(format!(
                    "'{command}' is already running in this worktree (owner {owner}, started \
                      {age}s ago); wait for it to finish, or stop that process"
                )));
            }
            Err(err) => {
                let _ = txn.rollback().await;
                return Err(err);
            }
        }
    }
    txn.commit().await.map_err(|err| {
        OperationError::commit(format!(
            "failed to commit the operation claim for command '{}': {err}",
            meta.command_name
        ))
    })?;

    // Test rendezvous (§C.12): hold HERE, with the claim committed and visible
    // to other processes, so a test can prove two worktrees hold their own
    // control slot AT THE SAME TIME. Without a hold, a sequential pair of
    // subprocesses passes even if the slot were repository-wide — each one
    // releases before the next claims, which is what made the previous
    // regression unable to fail.
    hold_for_claim_rendezvous();

    Ok(OperationBoundary {
        op_id,
        view_id,
        start_ts,
        meta,
        scope,
        scope_key,
        selected_parents,
        parent_metrics: ParentSelectionMetrics {
            resolver_mode: selection.mode,
            scanned_pages: selection.scanned_pages,
            scanned_items: selection.scanned_items,
            success_candidates: selection.success_candidates,
            selected_parent_count: 0,
            selection_latency_us,
        },
        _dedup_guard: dedup_guard,
    })
}

/// Block after committing a control claim, when the test harness asks.
///
/// `LIBRA_TEST=1` plus `LIBRA_TEST_HOLD_CLAIM_MS=<n>` — debug builds only, and
/// gated on the same `LIBRA_TEST` sentinel as every other failpoint, so a
/// release binary has no path to it.
#[cfg(debug_assertions)]
fn hold_for_claim_rendezvous() {
    if std::env::var_os("LIBRA_TEST").is_none() {
        return;
    }
    let Some(raw) = std::env::var_os("LIBRA_TEST_HOLD_CLAIM_MS") else {
        return;
    };
    let Some(ms) = raw.to_str().and_then(|value| value.parse::<u64>().ok()) else {
        return;
    };
    std::thread::sleep(Duration::from_millis(ms));
}

#[cfg(not(debug_assertions))]
fn hold_for_claim_rendezvous() {}

/// The typed scope kind this process is recording in (§C.9).
///
/// Every operation this binary writes is one it resolved itself, so it is
/// `main` or `linked` — never `unknown`, which exists only for rows migration
/// 2026072902 could not attribute. `repository` is reserved for operations
/// that act on repository-wide state; nothing writes it yet, and `op restore`
/// refuses it, so the reservation is fail-closed rather than aspirational.
fn declared_scope_kind(scope_key: &str, ownership: OperationOwnership) -> String {
    match ownership {
        // The caller declared repository-wide effects; which worktree it ran
        // FROM is not what governs whether the snapshot can be replayed.
        OperationOwnership::Repository => "repository".to_string(),
        OperationOwnership::Worktree if scope_key.is_empty() => "main".to_string(),
        OperationOwnership::Worktree => "linked".to_string(),
    }
}

/// Outcome of finding this worktree's control slot already taken.
enum Reclaim {
    /// The holder was proven dead; its row is closed and the slot is ours.
    TookOver { op_id: String, command: String },
    /// The holder is alive (or unprovable), so the slot stays taken.
    Held {
        command: String,
        owner: String,
        age: i64,
    },
}

/// `<machine>/<pid>` — the identity a claim records so a later process can ask
/// whether the holder still exists.
///
/// A hostname is NOT a machine identity: two containers sharing a repository
/// over a bind mount are routinely both `localhost` while having separate PID
/// namespaces, and reading one's pid in the other's namespace would revoke a
/// live claim. The machine part is therefore the boot id joined with the PID
/// namespace inode — a pid is only meaningful within exactly that pair — and
/// when neither can be read the identity is deliberately unusable, so
/// reclamation fails closed rather than guessing.
fn claim_owner() -> String {
    let pid = std::process::id();
    match pid_birth_token(pid) {
        Some(birth) => format!("{}/{pid}/{birth}", claim_machine()),
        // No birth token: record the pid alone. `owner_is_gone` then refuses
        // to reclaim it, which is the safe direction.
        None => format!("{}/{pid}", claim_machine()),
    }
}

/// The machine incarnation a pid is meaningful in, or `None` when this
/// platform cannot prove one.
///
/// Cached: it cannot change within a process, and a control action must not
/// pay for re-reading it.
fn claim_machine_identity() -> Option<String> {
    static IDENTITY: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    IDENTITY.get_or_init(read_machine_identity).clone()
}

fn read_machine_identity() -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id")
            .ok()
            .map(|id| id.trim().to_string())
            .filter(|id| !id.is_empty())?;
        // The namespace link reads as `pid:[4026531836]`; the inode is what
        // distinguishes two namespaces on one kernel.
        let pidns = std::fs::read_link("/proc/self/ns/pid")
            .ok()
            .map(|link| link.to_string_lossy().into_owned())
            .filter(|link| !link.is_empty())?;
        Some(format!("{boot}:{pidns}"))
    }
    #[cfg(target_os = "macos")]
    {
        // There is no boot id, so the incarnation is the host UUID (stable per
        // machine, distinct across machines sharing a repository) joined with
        // the boot time (distinct across reboots, which is when pids restart).
        let host = sysctl_value("kern.uuid")?;
        let boot = sysctl_value("kern.boottime")?;
        Some(format!("{host}:{boot}"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "macos")]
fn sysctl_value(name: &str) -> Option<String> {
    let out = std::process::Command::new("/usr/sbin/sysctl")
        .args(["-n", name])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
    (!value.is_empty()).then_some(value)
}

/// A token that identifies THIS INCARNATION of a pid, so a recycled pid is not
/// mistaken for the process that made the claim.
///
/// `None` when it cannot be read, which makes the owner unprovable and
/// therefore never reclaimable — the safe direction.
fn pid_birth_token(pid: u32) -> Option<String> {
    #[cfg(target_os = "linux")]
    {
        // Field 22 of `/proc/<pid>/stat` is the process start time in clock
        // ticks since boot. The comm field can contain spaces and parentheses,
        // so parse after the LAST ')'.
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rest = stat.rsplit_once(')')?.1;
        rest.split_whitespace().nth(19).map(str::to_string)
    }
    #[cfg(target_os = "macos")]
    {
        let out = std::process::Command::new("/bin/ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let value = String::from_utf8_lossy(&out.stdout).trim().to_string();
        (!value.is_empty()).then_some(value)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        None
    }
}

fn claim_machine() -> String {
    // `unprovable` never equals itself for reclamation purposes — see
    // `owner_is_gone`, which refuses to act on it.
    claim_machine_identity().unwrap_or_else(|| "unprovable".to_string())
}

/// Whether `owner` names a process that is PROVABLY gone.
///
/// True only when the claim was made in this exact boot and PID namespace and
/// the kernel says the pid does not exist. Anything else — another machine,
/// another namespace, an unparseable owner, a platform that cannot prove the
/// pairing — counts as alive: refusing a command is recoverable, revoking a
/// live control action is not.
fn owner_is_gone(owner: &str) -> bool {
    // `<machine>/<pid>/<birth>`; an owner without a birth token is never
    // reclaimed, because a recycled pid would then read as the same process.
    let mut parts = owner.rsplitn(3, '/');
    let Some(birth) = parts.next() else {
        return false;
    };
    let (Some(pid), Some(machine)) = (parts.next(), parts.next()) else {
        return false;
    };
    let Some(this_machine) = claim_machine_identity() else {
        return false;
    };
    if machine != this_machine {
        return false;
    }
    let Ok(pid) = pid.parse::<u32>() else {
        return false;
    };
    if pid == 0 {
        return false;
    }
    #[cfg(unix)]
    {
        // `kill(pid, 0)` asks the kernel whether the pid exists without
        // signalling it. EPERM means it exists and is someone else's.
        let exists = unsafe { libc::kill(pid as i32, 0) } == 0
            || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM);
        if !exists {
            return true;
        }
        // The pid exists — but is it the SAME process? A recycled pid has a
        // different birth token, and that claim's owner really is gone.
        match pid_birth_token(pid) {
            Some(current) => current != birth,
            // Cannot tell: treat as alive.
            None => false,
        }
    }
    #[cfg(not(unix))]
    {
        let _ = (pid, birth);
        false
    }
}

/// Inspect the running control claim; take the slot only if its owner is
/// provably gone. The abandoned row is marked FAILED rather than deleted —
/// that both frees the partial unique index and keeps the evidence that a
/// control action died mid-flight.
async fn reclaim_dead_claim(
    txn: &DatabaseTransaction,
    claim: &OperationRecord,
    now: i64,
) -> Result<Reclaim, OperationError> {
    let existing =
        OperationService::running_control_claim_with_conn(txn, &claim.repo_id, &claim.worktree_id)
            .await
            .map_err(|err| {
                OperationError::persist(format!("failed to inspect the control claim: {err}"))
            })?;
    let Some((holder_id, holder_command, holder_start, holder_owner)) = existing else {
        // The holder finished between the failed insert and this read.
        retry_claim(txn, claim).await?;
        return Ok(Reclaim::TookOver {
            op_id: String::new(),
            command: claim.command_name.clone(),
        });
    };
    let owner = holder_owner.unwrap_or_else(|| "unknown".to_string());
    let age = now.saturating_sub(holder_start);
    if !owner_is_gone(&owner) {
        return Ok(Reclaim::Held {
            command: holder_command,
            owner,
            age,
        });
    }
    OperationService::abandon_claim_with_conn(txn, &holder_id, now)
        .await
        .map_err(|err| {
            OperationError::persist(format!(
                "failed to close the abandoned claim '{holder_id}': {err}"
            ))
        })?;
    retry_claim(txn, claim).await?;
    Ok(Reclaim::TookOver {
        op_id: holder_id,
        command: holder_command,
    })
}

async fn retry_claim(
    txn: &DatabaseTransaction,
    claim: &OperationRecord,
) -> Result<(), OperationError> {
    OperationService::insert_operation_with_conn(txn, claim)
        .await
        .map(|_| ())
        .map_err(|err| {
            OperationError::business(format!(
                "a control action is already running in this worktree ({err})"
            ))
        })
}

impl OperationBoundary {
    /// The operation id this boundary claimed.
    pub fn op_id(&self) -> &str {
        &self.op_id
    }

    /// Close the claim: collect the view and record the outcome, in one short
    /// transaction, with no command work inside it.
    pub async fn finish(self, outcome: BoundaryOutcome) -> Result<(), OperationError> {
        let db = crate::internal::sequencer::request_db_checked()
            .await
            .map_err(OperationError::begin)?;
        self.finish_with_conn(&db, outcome).await
    }

    /// [`OperationBoundary::finish`] against a caller-provided connection.
    pub async fn finish_with_conn(
        self,
        db: &DatabaseConnection,
        outcome: BoundaryOutcome,
    ) -> Result<(), OperationError> {
        let end_ts = Utc::now().timestamp();
        // Completion READS the final view before it replaces the claim row,
        // so it takes the write lock up front — otherwise a concurrent writer
        // makes a control action unable to CLOSE its own boundary
        // (`db::begin_write_transaction`).
        let txn = crate::internal::db::begin_write_transaction(db)
            .await
            .map_err(|err| {
                OperationError::begin(format!(
                    "failed to open the operation completion transaction for command '{}': {err}",
                    self.meta.command_name
                ))
            })?;

        let view =
            match collect_final_view_with_conn(&txn, &self.meta.repo_id, &self.view_id, self.scope)
                .await
            {
                Ok(view) => view,
                Err(err) => {
                    let _ = txn.rollback().await;
                    return Err(OperationError::snapshot(format!(
                        "failed to collect the final view for command '{}': {err}",
                        self.meta.command_name
                    )));
                }
            };

        let mut metrics = self.parent_metrics;
        metrics.selected_parent_count = self.selected_parents.len() as u64;
        let record = OperationRecord {
            op_id: self.op_id.clone(),
            repo_id: self.meta.repo_id.clone(),
            view_id: self.view_id.clone(),
            command_name: self.meta.command_name.clone(),
            description: format!(
                "{} | resolver_mode={} scanned_pages={} scanned_items={} success_candidates={} \
                  selected_parents={} selection_latency_us={}",
                self.meta.description,
                match metrics.resolver_mode {
                    ParentSelectionMode::SingleLatestSuccess => "single_latest_success",
                },
                metrics.scanned_pages,
                metrics.scanned_items,
                metrics.success_candidates,
                metrics.selected_parent_count,
                metrics.selection_latency_us,
            ),
            actor: self.meta.actor.clone(),
            args_digest: self.meta.normalized_digest().map(str::to_string),
            start_ts: self.start_ts,
            end_ts: Some(end_ts),
            status: match outcome {
                BoundaryOutcome::Succeeded => OperationStatus::Succeeded,
                BoundaryOutcome::Failed => OperationStatus::Failed,
            },
            worktree_id: self.scope_key.clone(),
            scope_provenance: "declared".to_string(),
            scope_kind: declared_scope_kind(&self.scope_key, self.scope.ownership),
            // Never: the snapshot below is HEAD and refs, and a control action
            // also moved an index, a working tree and sequencer state.
            restorable: false,
            // A finished operation holds no slot.
            control_slot: None,
            claim_owner: None,
        };
        let graph = OperationGraphRecord {
            operation: record,
            parents: self
                .selected_parents
                .iter()
                .map(|parent| OperationParentRecord {
                    op_id: self.op_id.clone(),
                    parent_op_id: parent.clone(),
                })
                .collect(),
            view: OperationViewRecord {
                view_id: self.view_id.clone(),
                repo_id: self.meta.repo_id.clone(),
                head_kind: view.head_kind.clone(),
                head_target: view.head_target.clone(),
                created_at: end_ts,
            },
            refs: view.refs.clone(),
            workspace: view.workspace.clone(),
        };

        // `persist_operation_graph_with_conn` inserts; the claim row is
        // already there under this op_id, so replace it in the same
        // transaction rather than leaving a `running` duplicate. Deleting
        // first also releases the partial unique index inside the transaction,
        // so a follow-up control action in this worktree is not blocked by a
        // row that has already finished.
        match OperationService::delete_operation_with_conn(&txn, &self.op_id).await {
            // Our claim is gone: another process proved this one dead and took
            // the slot while it was still running. Writing the finished graph
            // now would resurrect a record the takeover already accounted for
            // — and would claim success for work that was concurrently
            // duplicated. Report it and leave the takeover's evidence alone.
            Ok(false) => {
                let _ = txn.rollback().await;
                return Err(OperationError::business(format!(
                    "the operation claim for '{}' was released by another process while it ran; \
                      its record was not written",
                    self.meta.command_name
                )));
            }
            Ok(true) => {}
            Err(err) => {
                let _ = txn.rollback().await;
                return Err(OperationError::persist(format!(
                    "failed to release the operation claim for command '{}': {err}",
                    self.meta.command_name
                )));
            }
        }
        if let Err(err) = OperationService::persist_operation_graph_with_conn(&txn, &graph).await {
            let _ = txn.rollback().await;
            return Err(OperationError::persist(format!(
                "failed to persist the operation graph for command '{}': {err}",
                self.meta.command_name
            )));
        }
        txn.commit().await.map_err(|err| {
            OperationError::commit(format!(
                "failed to commit the operation completion for command '{}': {err}",
                self.meta.command_name
            ))
        })
    }
}

/// Resolve parent operations using a stable strategy entrypoint.
///
/// v1 uses single-parent latest-success strategy. The result keeps a vector
/// shape to reserve forward-compatible multi-parent extension.
pub async fn resolve_parent_selection_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
    mode: ParentSelectionMode,
) -> Result<ParentSelectionResult, OperationError> {
    if repo_id.trim().is_empty() {
        return Err(OperationError::validation("repo_id must not be empty"));
    }

    let mut page: u64 = 1;
    let mut scanned_pages = 0;
    let mut scanned_items = 0;

    loop {
        let records = OperationService::list_operations_by_repo_paginated_with_conn(
            db,
            repo_id,
            OperationQueryPage {
                page,
                per_page: PARENT_RESOLUTION_PAGE_SIZE,
            },
        )
        .await
        .map_err(|err| {
            OperationError::begin(format!(
                "failed to resolve parent operation for repository '{}': {err}",
                repo_id
            ))
        })?;

        scanned_pages += 1;
        let items_len = records.items.len() as u64;
        scanned_items += items_len;

        let mut success_candidates = 0;
        let mut selected_parent = None;
        for item in records.items {
            if item.status == OperationStatus::Succeeded {
                success_candidates += 1;
                if selected_parent.is_none() {
                    selected_parent = Some(item.op_id);
                }
            }
        }

        if let Some(parent) = selected_parent {
            return Ok(ParentSelectionResult {
                selected: vec![parent],
                scanned_pages,
                scanned_items,
                success_candidates,
                mode,
            });
        }

        if items_len < records.per_page {
            return Ok(ParentSelectionResult {
                selected: Vec::new(),
                scanned_pages,
                scanned_items,
                success_candidates,
                mode,
            });
        }

        page += 1;
    }
}

/// Resolve the most recent successful operation in a repository for v1 parent strategy.
///
/// The resolver scans recent operations in reverse chronological order and returns the
/// first successful operation id, or `None` when no successful parent exists.
pub async fn resolve_parent_operation_id_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
) -> Result<Option<String>, OperationError> {
    let selection =
        resolve_parent_selection_with_conn(db, repo_id, ParentSelectionMode::SingleLatestSuccess)
            .await?;
    Ok(selection.selected.first().cloned())
}

async fn collect_final_view_with_conn<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
    view_id: &str,
    scope: OperationScope,
) -> Result<OperationViewSnapshot, DbErr> {
    let head = Head::current_result_with_conn(db).await.map_err(|err| {
        DbErr::Custom(format!(
            "failed to resolve head while collecting operation view: {err}"
        ))
    })?;

    let (head_kind, head_target) = match head {
        Head::Branch(name) => ("branch".to_string(), name),
        Head::Detached(hash) => ("detached".to_string(), hash.to_string()),
    };

    let refs = if scope.include_refs {
        let mut records = Vec::new();

        let local_branches = Branch::list_branches_result_with_conn(db, None)
            .await
            .map_err(|err| DbErr::Custom(format!("failed to list local branches: {err}")))?;
        for branch in local_branches {
            records.push(OperationViewRefRecord {
                view_id: view_id.to_string(),
                ref_kind: "branch".to_string(),
                ref_name: branch.name,
                ref_remote: None,
                target_oid: branch.commit.to_string(),
            });
        }

        if scope.include_remote_tracking {
            let remote_refs = reference::Entity::find()
                .filter(reference::Column::Kind.eq(reference::ConfigKind::Branch))
                .filter(reference::Column::Remote.is_not_null())
                .all(db)
                .await?;
            for remote_ref in remote_refs {
                let Some(name) = remote_ref.name else {
                    continue;
                };
                let Some(commit) = remote_ref.commit else {
                    continue;
                };
                records.push(OperationViewRefRecord {
                    view_id: view_id.to_string(),
                    ref_kind: "remote_branch".to_string(),
                    ref_name: name,
                    ref_remote: remote_ref.remote,
                    target_oid: commit,
                });
            }
        }

        records
    } else {
        Vec::new()
    };

    let workspace = if scope.include_workspace {
        vec![OperationViewWorkspaceRecord {
            view_id: view_id.to_string(),
            pointer_kind: "head".to_string(),
            pointer_value: head_target.clone(),
        }]
    } else {
        Vec::new()
    };

    let _ = repo_id;

    Ok(OperationViewSnapshot {
        head_kind,
        head_target,
        refs,
        workspace,
    })
}

/*
#[cfg(test)]
mod tests {
    use chrono::Utc;
    use sea_orm::{
        ConnectionTrait, Database, DbBackend, DbErr, Statement,
    };

    use super::{
        ensure_not_recent_duplicate_with_conn, operation_dedup_key,
        resolve_parent_operation_id_with_conn, OperationError, OperationMeta, OperationScope,
        with_operation_log_with_conn,
    };
    use crate::internal::operation::{OperationRecord, OperationService, OperationStatus};

    fn valid_meta() -> OperationMeta {
        OperationMeta {
            command_name: "commit".to_string(),
            description: "record snapshot".to_string(),
            actor: "alice".to_string(),
            repo_id: "repo_1".to_string(),
            args_digest: Some("sha256:abcd".to_string()),
        }
    }

    #[test]
    fn meta_validation_rejects_empty_fields() {
        let mut meta = valid_meta();
        meta.command_name = " ".to_string();
        assert!(matches!(meta.validate(), Err(OperationError::Validation(_))));

        let mut meta = valid_meta();
        meta.repo_id = " ".to_string();
        assert!(matches!(meta.validate(), Err(OperationError::Validation(_))));
    }

    #[test]
    fn scope_default_matches_a5_contract() {
        let scope = OperationScope::default();
        assert!(scope.include_refs);
        assert!(scope.include_workspace);
        assert!(!scope.include_remote_tracking);
    }

    /// Part C W1 (§C.9): the same command + args submitted from ANOTHER
    /// worktree within the 5s window is NOT a duplicate — the window is
    /// scoped by `worktree_id`. The same scope IS still rejected.
    #[tokio::test]
    async fn concurrent_identical_control_actions_in_two_worktrees_not_deduped() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        let now = Utc::now().timestamp();
        // Worktree "wt-a" just succeeded with this exact command + digest.
        let mut record = sample_record("op_prev", OperationStatus::Succeeded, now - 1);
        record.worktree_id = "wt-a".to_string();
        OperationService::insert_operation_with_conn(&db, &record)
            .await
            .unwrap();

        // The identical submission from MAIN ("") passes...
        ensure_not_recent_duplicate_with_conn(&db, &valid_meta(), now, "")
            .await
            .expect("another worktree's history must not dedup this scope");
        // ...and from a third worktree too.
        ensure_not_recent_duplicate_with_conn(&db, &valid_meta(), now, "wt-b")
            .await
            .expect("scopes are independent");
        // The SAME scope within the window is still rejected.
        let err = ensure_not_recent_duplicate_with_conn(&db, &valid_meta(), now, "wt-a")
            .await
            .expect_err("same-scope duplicate within the window is rejected");
        assert!(matches!(err, OperationError::Business(_)));
    }

    /// Part C W1 (§C.9): the in-process active-key set is scope-aware too —
    /// the key embeds the worktree id, so identical metas from different
    /// scopes never collide (`op restore`'s digest is the target op id,
    /// identical in every worktree that restores it).
    #[test]
    fn op_restore_dedup_key_is_scope_aware() {
        let mut meta = valid_meta();
        meta.command_name = "op-restore".to_string();
        meta.args_digest = Some("op_0123".to_string());
        let main_key = operation_dedup_key(&meta, "").expect("key");
        let linked_key = operation_dedup_key(&meta, "wt-a").expect("key");
        assert_ne!(main_key, linked_key, "scope must discriminate the key");
        assert!(linked_key.contains("::wt-a::"));
    }

    fn sample_record(op_id: &str, status: OperationStatus, end_ts: i64) -> OperationRecord {
        OperationRecord {
            op_id: op_id.to_string(),
            repo_id: "repo_1".to_string(),
            view_id: format!("view_{op_id}"),
            command_name: "commit".to_string(),
            description: format!("desc_{op_id}"),
            actor: "alice".to_string(),
            args_digest: Some("sha256:abcd".to_string()),
            start_ts: end_ts - 5,
            end_ts: Some(end_ts),
            status,
            worktree_id: String::new(),
            scope_provenance: "declared".to_string(),
        }
    }

    async fn create_operation_table(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            r#"
             CREATE TABLE legacy_operation (
                 op_id TEXT PRIMARY KEY,
                 repo_id TEXT NOT NULL,
                 view_id TEXT NOT NULL,
                 command_name TEXT NOT NULL,
                 description TEXT NOT NULL,
                 actor TEXT NOT NULL,
                 args_digest TEXT,
                 start_ts INTEGER NOT NULL,
                 end_ts INTEGER,
                 status TEXT NOT NULL,
                 worktree_id TEXT NOT NULL DEFAULT '',
                 scope_provenance TEXT NOT NULL DEFAULT 'declared'
             )
             "#
            .to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_operation_graph_tables_missing_view(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE legacy_operation_parent (op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE legacy_operation_view_ref (view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE legacy_operation_view_workspace (view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind))".to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_operation_graph_tables(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE legacy_operation_parent (op_id TEXT NOT NULL,parent_op_id TEXT NOT NULL,PRIMARY KEY (op_id,parent_op_id))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE legacy_operation_view (view_id TEXT PRIMARY KEY,repo_id TEXT NOT NULL,head_kind TEXT NOT NULL,head_target TEXT NOT NULL,created_at INTEGER NOT NULL)".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE legacy_operation_view_ref (view_id TEXT NOT NULL,ref_kind TEXT NOT NULL,ref_name TEXT NOT NULL,ref_remote TEXT NOT NULL,target_oid TEXT NOT NULL,PRIMARY KEY (view_id,ref_kind,ref_name,ref_remote))".to_string(),
        ))
        .await
        .unwrap();
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE legacy_operation_view_workspace (view_id TEXT NOT NULL,pointer_kind TEXT NOT NULL,pointer_value TEXT NOT NULL,PRIMARY KEY (view_id,pointer_kind))".to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_reference_table_without_head(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT,kind TEXT NOT NULL,\"commit\" TEXT,remote TEXT)".to_string(),
        ))
        .await
        .unwrap();
    }

    async fn create_reference_table_with_head(db: &sea_orm::DatabaseConnection) {
        create_reference_table_without_head(db).await;
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Head', NULL, NULL)"
                .to_string(),
        ))
        .await
        .unwrap();
    }


    #[tokio::test]
    async fn resolve_parent_operation_picks_latest_successful_record() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;

        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_old_success", OperationStatus::Succeeded, 10),
        )
        .await
        .unwrap();
        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_new_failed", OperationStatus::Failed, 30),
        )
        .await
        .unwrap();
        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_latest_success", OperationStatus::Succeeded, 40),
        )
        .await
        .unwrap();

        let parent = resolve_parent_operation_id_with_conn(&db, "repo_1")
            .await
            .unwrap();

        assert_eq!(parent.as_deref(), Some("op_latest_success"));
    }

    #[tokio::test]
    async fn resolve_parent_operation_returns_none_when_no_success_exists() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;

        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_failed", OperationStatus::Failed, 10),
        )
        .await
        .unwrap();
        OperationService::insert_operation_with_conn(
            &db,
            &sample_record("op_running", OperationStatus::Running, 20),
        )
        .await
        .unwrap();

        let parent = resolve_parent_operation_id_with_conn(&db, "repo_1")
            .await
            .unwrap();

        assert!(parent.is_none());
    }

    async fn create_tx_probe_table(db: &sea_orm::DatabaseConnection) {
        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "CREATE TABLE tx_probe (id INTEGER PRIMARY KEY)".to_string(),
        ))
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn with_operation_log_returns_payload_and_ids_on_success() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();
        let before = Utc::now().timestamp();
        let result = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) }),
        )
        .await
        .unwrap();
        let after = Utc::now().timestamp();

        assert_eq!(result.payload, "ok");
        assert!(!result.op_id.is_empty());
        assert!(!result.view_id.is_empty());
        assert!(result.end_ts >= before);
        assert!(result.end_ts <= after);

        let op = OperationService::find_operation_by_id_with_conn(&db, &result.op_id)
            .await
            .unwrap()
            .unwrap();
        let persisted_end_ts = op.end_ts.expect("persisted operation must have end_ts");
        assert!(op.start_ts <= persisted_end_ts);
    }

    #[tokio::test]
    async fn with_operation_log_captures_final_view_and_persists_graph() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();

        let parent_seed = sample_record("op_seed_success", OperationStatus::Succeeded, 10);
        OperationService::insert_operation_with_conn(&db, &parent_seed)
            .await
            .unwrap();

        let result = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("ok".to_string()) }),
        )
        .await
        .unwrap();

        assert_eq!(result.payload, "ok");
        assert_eq!(result.view.head_kind, "branch");
        assert_eq!(result.view.head_target, "main");
        assert_eq!(result.view.workspace.len(), 1);
        assert_eq!(result.view.workspace[0].pointer_kind, "head");
        assert_eq!(result.view.workspace[0].pointer_value, "main");
        assert_eq!(result.view.refs.len(), 1);
        assert_eq!(result.view.refs[0].ref_kind, "branch");
        assert_eq!(result.view.refs[0].ref_name, "main");
        assert_eq!(result.view.refs[0].target_oid, "1111111111111111111111111111111111111111");

        let graph = OperationService::load_restore_view_by_operation_with_conn(&db, &result.op_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(graph.operation.view_id, result.view_id);
        assert_eq!(graph.view.head_kind, "branch");
        assert_eq!(graph.view.head_target, "main");
        assert_eq!(graph.refs.len(), 1);
        assert_eq!(graph.workspace.len(), 1);
        assert_eq!(graph.parents.len(), 1);
        assert_eq!(graph.parents[0].parent_op_id, "op_seed_success");
    }

    #[tokio::test]
    async fn with_operation_log_rolls_back_when_persist_fails() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables_missing_view(&db).await;
        create_reference_table_with_head(&db).await;
        create_tx_probe_table(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();

        let error = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |txn| {
                Box::pin(async move {
                    txn.execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "INSERT INTO tx_probe(id) VALUES(2)".to_string(),
                    ))
                    .await?;
                    Ok::<_, DbErr>(())
                })
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, OperationError::Persist(_) | OperationError::Rollback(_)));

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM tx_probe WHERE id = 2".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let count: i64 = row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(count, 0);
    }

    #[tokio::test]
    async fn with_operation_log_rolls_back_on_snapshot_failure() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_without_head(&db).await;
        create_tx_probe_table(&db).await;

        let error = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |txn| {
                Box::pin(async move {
                    txn.execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "INSERT INTO tx_probe(id) VALUES(3)".to_string(),
                    ))
                    .await?;
                    Ok::<_, DbErr>(())
                })
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, OperationError::Snapshot(_)));

        let tx_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM tx_probe WHERE id = 3".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let tx_count: i64 = tx_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(tx_count, 0);

        let op_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM legacy_operation".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let op_count: i64 = op_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(op_count, 0);

        let view_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM legacy_operation_view".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let view_count: i64 = view_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(view_count, 0);

        let parent_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM legacy_operation_parent".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let parent_count: i64 = parent_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(parent_count, 0);
    }

    #[tokio::test]
    async fn with_operation_log_builds_parent_chain_and_restore_graphs() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();

        let first = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("first".to_string()) }),
        )
        .await
        .unwrap();

        let second = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |_txn| Box::pin(async move { Ok::<_, DbErr>("second".to_string()) }),
        )
        .await
        .unwrap();

        let first_graph = OperationService::load_restore_view_by_operation_with_conn(&db, &first.op_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(first_graph.parents.len(), 0);

        let second_graph = OperationService::load_restore_view_by_operation_with_conn(&db, &second.op_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(second_graph.parents.len(), 1);
        assert_eq!(second_graph.parents[0].parent_op_id, first.op_id);
        assert_eq!(second_graph.refs.len(), 1);
        assert_eq!(second_graph.workspace.len(), 1);
    }

    #[tokio::test]
    async fn with_operation_log_rolls_back_on_business_failure() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        create_operation_table(&db).await;
        create_operation_graph_tables(&db).await;
        create_reference_table_with_head(&db).await;
        create_tx_probe_table(&db).await;

        db.execute(Statement::from_string(
            DbBackend::Sqlite,
            "INSERT INTO reference(name, kind, \"commit\", remote) VALUES('main', 'Branch', '1111111111111111111111111111111111111111', NULL)"
                .to_string(),
        ))
        .await
        .unwrap();
        let error = with_operation_log_with_conn(
            &db,
            valid_meta(),
            OperationScope::default(),
            |txn| {
                Box::pin(async move {
                    txn.execute(Statement::from_string(
                        DbBackend::Sqlite,
                        "INSERT INTO tx_probe(id) VALUES(1)".to_string(),
                    ))
                    .await?;
                    Err::<(), DbErr>(DbErr::Custom("boom".to_string()))
                })
            },
        )
        .await
        .unwrap_err();

        assert!(matches!(error, OperationError::Business(_)));

        let row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM tx_probe".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let count: i64 = row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(count, 0);

        let op_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM legacy_operation".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let op_count: i64 = op_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(op_count, 0);

        let view_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM legacy_operation_view".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let view_count: i64 = view_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(view_count, 0);

        let parent_row = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) FROM legacy_operation_parent".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        let parent_count: i64 = parent_row.try_get_by_index(0).unwrap_or_default();
        assert_eq!(parent_count, 0);
    }
}
*/
