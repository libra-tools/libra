//! `refs/libra/traces` persistence API — plan-20260920 RC-02.
//!
//! This module is the single source of truth for traces commit / inflight /
//! prune / catalog-rebuild types. [`crate::internal::ai::history::HistoryManager`]
//! still owns the object-store CAS implementation and `pub use`s these items
//! until remaining Code-side callers are deleted.

use std::time::Instant;
#[cfg(test)]
use std::{
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};

use anyhow::{Context, Result, anyhow, bail};
use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, Value};

use crate::internal::ai::{
    capture_scope::{
        CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
        authorize_final_capture_commit,
    },
    hooks::lifecycle::LIFECYCLE_EVENT_JSONL_SCHEMA_VERSION,
    observed_agents::RedactedBytes,
};

/// A caller-owned absolute deadline elapsed while a standalone traces-marker
/// transaction was waiting on SQLite or reaching its final commit fence.
/// Subagent capture translates this into its public deadline classification.
#[derive(Debug, thiserror::Error)]
#[error("traces marker mutation exceeded its caller deadline")]
pub(crate) struct TracesMarkerDeadlineExceeded;

/// Test-only seam for the SQLx commit-dispatch race. The commit future is
/// polled exactly once; SQLx queues `COMMIT` before that poll can return
/// pending. The test holds a real SQLite reader lock, waits past the capture
/// deadline, then releases this receiver so the already-dispatched commit can
/// acknowledge successfully.
#[cfg(test)]
struct TracesCommitDispatchPause {
    dispatched: Mutex<Option<tokio::sync::oneshot::Sender<bool>>>,
    release: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

#[cfg(test)]
impl TracesCommitDispatchPause {
    fn new() -> (
        Arc<Self>,
        tokio::sync::oneshot::Receiver<bool>,
        tokio::sync::oneshot::Sender<()>,
    ) {
        let (dispatched_tx, dispatched_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        (
            Arc::new(Self {
                dispatched: Mutex::new(Some(dispatched_tx)),
                release: Mutex::new(Some(release_rx)),
            }),
            dispatched_rx,
            release_tx,
        )
    }
}

#[cfg(test)]
tokio::task_local! {
    static TEST_TRACES_COMMIT_DISPATCH_PAUSE: Option<Arc<TracesCommitDispatchPause>>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_TRACES_MARKER_BEFORE_DML_DELAY: Option<Duration>;
}

#[cfg(test)]
tokio::task_local! {
    static TEST_TRACES_MARKER_PRECOMMIT_READ_DELAY: Option<Duration>;
}

#[cfg(test)]
async fn with_traces_commit_dispatch_pause<F>(
    pause: Arc<TracesCommitDispatchPause>,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_TRACES_COMMIT_DISPATCH_PAUSE
        .scope(Some(pause), future)
        .await
}

#[cfg(test)]
async fn with_traces_marker_before_dml_delay<F>(delay: Duration, future: F) -> F::Output
where
    F: std::future::Future,
{
    TEST_TRACES_MARKER_BEFORE_DML_DELAY
        .scope(Some(delay), future)
        .await
}

/// Delay a marker pre-mutation read inside its deadline wrapper. This is a
/// test-only seam for callers that must prove a stalled fence query cannot
/// reach marker DML after its absolute budget elapses.
#[cfg(test)]
pub(crate) async fn with_traces_marker_precommit_read_delay<F>(
    delay: Duration,
    future: F,
) -> F::Output
where
    F: std::future::Future,
{
    TEST_TRACES_MARKER_PRECOMMIT_READ_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
async fn traces_marker_test_delay_before_precommit_read() {
    if let Ok(Some(delay)) =
        TEST_TRACES_MARKER_PRECOMMIT_READ_DELAY.try_with(|configured| *configured)
    {
        tokio::time::sleep(delay).await;
    }
}

fn ensure_before_traces_marker_deadline(deadline: Option<CaptureCommitDeadline>) -> Result<()> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        return Err(TracesMarkerDeadlineExceeded.into());
    }
    Ok(())
}

fn ensure_before_traces_marker_dml(deadline: Option<CaptureCommitDeadline>) -> Result<()> {
    #[cfg(test)]
    if let Ok(Some(delay)) = TEST_TRACES_MARKER_BEFORE_DML_DELAY.try_with(|configured| *configured)
    {
        std::thread::sleep(delay);
    }
    ensure_before_traces_marker_deadline(deadline)
}

/// Bound only SQLite writer acquisition. Mutating statements and COMMIT are
/// deliberately outside this timeout: after SQLite accepts either, dropping
/// its future cannot prove the durable outcome was rolled back.
async fn begin_traces_marker_write_transaction_until(
    conn: &DatabaseConnection,
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
) -> Result<DatabaseTransaction> {
    ensure_before_traces_marker_deadline(deadline)?;
    let result = match deadline {
        Some(deadline) => tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic()),
            crate::internal::db::begin_write_transaction(conn),
        )
        .await
        .map_err(|_| anyhow::Error::from(TracesMarkerDeadlineExceeded))?,
        None => crate::internal::db::begin_write_transaction(conn).await,
    };
    result.context(operation)
}

/// Bound a pre-mutation read while preserving the transaction rollback path.
///
/// This is deliberately unavailable to marker DML and COMMIT: cancelling a
/// read has no durable effect, whereas cancelling a dispatched mutation or
/// commit would make its durable outcome unknowable.
async fn await_traces_marker_precommit_read_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    operation: &'static str,
    read: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    ensure_before_traces_marker_deadline(deadline)?;
    #[cfg(test)]
    let read = async {
        traces_marker_test_delay_before_precommit_read().await;
        read.await
    };
    let result = match deadline {
        Some(deadline) => {
            tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.monotonic()), read)
                .await
                .map_err(|_| anyhow::Error::from(TracesMarkerDeadlineExceeded))?
        }
        None => read.await,
    };
    // A future can complete ready in the same poll in which the timer fires.
    // Never let that scheduling edge carry a marker update past its deadline.
    ensure_before_traces_marker_deadline(deadline)?;
    result.context(operation)
}

/// Inputs to [`crate::internal::ai::history::HistoryManager::append_checkpoint_commit`].
///
/// All byte slices live for the duration of the call; the function does not
/// retain references after returning.
#[derive(Debug)]
pub struct CheckpointCommitParams<'a> {
    /// UUIDv4 of the checkpoint, used both as the row primary key and as
    /// the leaf path under `checkpoint/<id[:2]>/<id[2:]>/...`.
    pub checkpoint_id: &'a str,
    /// `agent_session.session_id` this checkpoint belongs to.
    pub session_id: &'a str,
    /// Random generation returned by the marker registration that authorized
    /// this specific writer. The append entry point compares it before any
    /// object is created, so a stalled writer cannot adopt a replacement
    /// marker registered under the same session/checkpoint key.
    pub marker_generation: &'a str,
    /// Optional workspace lease carried from a live/import capture scope.
    /// Every marker/object/ref mutation verifies it inside its own SQLite
    /// write transaction. Legacy history callers intentionally pass `None`.
    pub capture_scope: Option<&'a CaptureScope>,
    /// `agent_session.agent_kind` (snake_case form, e.g. `claude_code`).
    /// Also the file-name stem of `transcript/<agent_kind>.jsonl` — E4-libra
    /// pins the snake_case db tag here, never the CLI slug (`claude-code`).
    pub agent_kind: &'a str,
    /// User-branch HEAD oid at the moment the checkpoint was taken.
    pub parent_commit: Option<&'a str>,
    /// Scope category: temporary, committed, or subagent.
    pub scope: CheckpointScope,
    /// Optional tool-use id when the checkpoint was triggered by a tool call.
    pub tool_use_id: Option<&'a str>,
    /// Pre-serialised metadata JSON to land at `metadata.json`. Typed as
    /// [`RedactedBytes`] (AG-19 / G4) so the traces write path can only ever
    /// receive bytes that passed through the redaction type.
    pub metadata_json: &'a RedactedBytes,
    /// Already-redacted transcript bytes. Typed as [`RedactedBytes`]
    /// (not `&[u8]`) so the traces write path can only ever receive
    /// bytes that passed through the redaction type — entire.md §8.1 /
    /// §13 P0: every transcript blob written to `traces` must go
    /// through `RedactedBytes`.
    pub transcript_redacted: &'a RedactedBytes,
    /// E3-canonical lifecycle JSONL bytes to land at
    /// `events/lifecycle.jsonl` — one already-redacted canonical event per
    /// line (see `hooks::lifecycle::lifecycle_events_to_canonical_jsonl`).
    /// Today the runtime passes the single triggering event; multi-event
    /// batches are just additional lines. Typed as [`RedactedBytes`]
    /// (AG-19 / G4) so no `&[u8]` can reach the checkpoint sink.
    pub lifecycle_events_jsonl: &'a RedactedBytes,
    /// The aggregated redaction-report JSON (same document that lands in
    /// `agent_session.redaction_report` / metadata.json) to land at
    /// `redaction_report.json`. Rule-hit statistics only — never raw text.
    /// Typed as [`RedactedBytes`] (AG-19 / G4) to keep the whole checkpoint
    /// tree behind the redaction type.
    pub redaction_report_json: &'a RedactedBytes,
    /// plan-20260713 DR-05c-0 (ADR-DR-10): extra SQL applied INSIDE the
    /// winning ref-CAS transaction — catalog row, coverage revision inserts
    /// and claim advances commit atomically with the ref update, or the
    /// whole transaction (ref included) rolls back. `None` keeps the legacy
    /// behavior (catalog inserted separately after the CAS).
    pub txn_extra: Option<&'a dyn TracesTxnExtra>,
    /// Paired command deadline for historical imports. Live/export writers
    /// pass `None`; import object construction and CAS use the monotonic half,
    /// while final SQLite authorization uses the immutable wall-clock half.
    pub deadline: Option<CaptureCommitDeadline>,
}

/// Per-attempt commit identifiers handed to [`TracesTxnExtra::apply`] — the
/// commit hash and root tree change on every CAS rebuild, so the extra must
/// receive them at apply time rather than capture them up front.
#[derive(Debug, Clone)]
pub struct TracesCommitCtx {
    pub commit_hash: String,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
}

/// A catalog row reconstructed from a `refs/libra/traces` checkpoint commit
/// (plan-20260713 DR-05c-0): the SHARED classification boundary used by
/// doctor's class-2 repair and by claim recovery, so both apply the same
/// fail-closed rules instead of duplicating them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RebuiltCatalogRow {
    Committed {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
    Subagent {
        checkpoint_id: String,
        session_id: String,
        parent_commit: Option<String>,
        parent_checkpoint_id: Option<String>,
        subagent_session_id: Option<String>,
        tool_use_id: Option<String>,
        description: Option<String>,
        tree_oid: String,
        metadata_blob_oid: String,
        traces_commit: String,
        created_at: i64,
    },
}

/// Inputs for [`rebuild_catalog_row_from_traces_ref`] — the fields both a
/// checkpoint commit's `metadata.json` and its ref position provide.
#[derive(Debug, Clone, Default)]
pub struct RebuildCatalogRowInputs {
    pub scope: String,
    pub checkpoint_id: String,
    pub session_id: String,
    pub parent_commit: Option<String>,
    pub parent_checkpoint_id: Option<String>,
    pub subagent_session_id: Option<String>,
    pub tool_use_id: Option<String>,
    pub description: Option<String>,
    pub tree_oid: String,
    pub metadata_blob_oid: String,
    pub traces_commit: String,
    pub created_at: i64,
}

/// Classify + assemble a rebuildable catalog row from traces-ref evidence.
/// Fail-closed: any scope other than `committed` / `subagent` is an error —
/// the caller must route it to manual review, never guess a row shape.
pub fn rebuild_catalog_row_from_traces_ref(
    inputs: RebuildCatalogRowInputs,
) -> Result<RebuiltCatalogRow> {
    match inputs.scope.as_str() {
        "committed" => Ok(RebuiltCatalogRow::Committed {
            checkpoint_id: inputs.checkpoint_id,
            session_id: inputs.session_id,
            parent_commit: inputs.parent_commit,
            tree_oid: inputs.tree_oid,
            metadata_blob_oid: inputs.metadata_blob_oid,
            traces_commit: inputs.traces_commit,
            created_at: inputs.created_at,
        }),
        "subagent" => Ok(RebuiltCatalogRow::Subagent {
            checkpoint_id: inputs.checkpoint_id,
            session_id: inputs.session_id,
            parent_commit: inputs.parent_commit,
            parent_checkpoint_id: inputs.parent_checkpoint_id,
            subagent_session_id: inputs.subagent_session_id,
            tool_use_id: inputs.tool_use_id,
            description: inputs.description,
            tree_oid: inputs.tree_oid,
            metadata_blob_oid: inputs.metadata_blob_oid,
            traces_commit: inputs.traces_commit,
            created_at: inputs.created_at,
        }),
        other => Err(anyhow!(
            "checkpoint scope '{other}' is not auto-rebuildable (fail-closed; manual review)"
        )),
    }
}

/// Transactional companion writes for a traces ref update (ADR-DR-10).
///
/// `apply` runs inside the SAME SQLite transaction as the successful ref
/// CAS, after the ref row write and before COMMIT. Returning an error rolls
/// the entire transaction back — the ref does not move, and the caller's
/// checkpoint write fails closed.
#[async_trait::async_trait]
pub trait TracesTxnExtra: Send + Sync {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> Result<()>;
}

impl std::fmt::Debug for dyn TracesTxnExtra + '_ {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("TracesTxnExtra")
    }
}

/// Scope tag stamped on each checkpoint, mirroring the
/// `agent_checkpoint.scope` CHECK constraint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CheckpointScope {
    Temporary,
    Committed,
    Subagent,
}

impl CheckpointScope {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Temporary => "temporary",
            Self::Committed => "committed",
            Self::Subagent => "subagent",
        }
    }
}

/// Output from [`crate::internal::ai::history::HistoryManager::append_checkpoint_commit`]; what the caller
/// stores in `agent_checkpoint`.
///
/// Naming discipline (AG-20): `commit_hash` is the freshly-written commit on
/// `refs/libra/traces`; the DB column it lands in is
/// `agent_checkpoint.traces_commit`. Keep the two names distinct — the Rust
/// side never calls it `traces_commit` and the SQL side never `commit_hash`.
#[derive(Debug, Clone)]
pub struct CheckpointCommit {
    pub commit_hash: ObjectHash,
    pub tree_oid: ObjectHash,
    pub metadata_blob_oid: ObjectHash,
    /// Exact marker generation consumed by this append. Callers use it when
    /// retiring the marker after their catalog write completes.
    pub marker_generation: String,
    /// Number of head-conflict retries the ref CAS loop needed (0 = first
    /// attempt won). Recorded on the `agent.checkpoint.write` span.
    pub cas_retries: u64,
    /// Objects written/enqueued for this checkpoint (blobs + trees +
    /// commit, counted across CAS attempts). Recorded on the
    /// `agent.checkpoint.write` span.
    pub object_count: u64,
}

/// Outcome of [`crate::internal::ai::history::HistoryManager::erase_session_local`] — the three-face
/// local erasure result for one session (AG-24a).
#[derive(Debug, Clone)]
pub struct SessionEraseOutcome {
    /// Whether an `agent_session` row was deleted.
    pub session_deleted: bool,
    /// Checkpoints removed from the catalog + ref.
    pub removed_checkpoints: u64,
    /// Whether `refs/libra/traces` was rewritten.
    pub ref_rewritten: bool,
    /// `object_index` rows dropped for now-unreachable OIDs.
    pub deleted_object_index_rows: u64,
}

/// Result of pruning checkpoint commits from `refs/libra/traces`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckpointPruneOutcome {
    pub removed_checkpoints: u64,
    pub rewritten_checkpoints: usize,
    pub ref_rewritten: bool,
    /// Which AG-20 window-guard path the prune took. `"noop"` when there
    /// was nothing to prune (guards skipped),
    /// `"markers_and_catalog_verified"` when both the live in-flight
    /// marker check and the ref-vs-catalog comparison ran and passed.
    /// Recorded on the `agent.clean.prune` span.
    pub window_guard: &'static str,
    /// `object_index` rows deleted for OIDs the prune made unreachable
    /// (conservative: only OIDs exclusively referenced by the removed
    /// checkpoints). Recorded on the `agent.clean.prune` span as
    /// `deleted_objects`.
    pub deleted_object_index_rows: u64,
    /// Import job identities physically deleted after their final coverage
    /// claim disappeared in this prune transaction.
    pub deleted_import_identities: u64,
}

/// Fail-closed refusals raised by [`HistoryManager::prune_checkpoint_commits`]
/// before it rewrites `refs/libra/traces` (AG-20 window A/B closure,
/// `agent.md` write-sequence matrix).
///
/// Callers can `downcast_ref` through the `anyhow` chain to distinguish a
/// deterministic guard refusal (retry later / run doctor) from a real
/// storage failure.
#[derive(Debug, thiserror::Error)]
#[error(
    "rejected checkpoint cleanup is deferred because {reason}; candidate ownership remains durable — inspect with `libra agent doctor`, run `libra agent doctor --repair` for safe repairs, and retry"
)]
pub(crate) struct RejectedCheckpointCleanupDeferred {
    pub(crate) reason: String,
}

/// M5 reservation→marker guard kept crate-private so adding this refusal does
/// not add a variant to the exhaustively matchable public guard enum.
#[derive(Debug, thiserror::Error)]
#[error(
    "refusing to prune traces checkpoints: a subagent content write is reserved \
     (session '{session_id}', attempt '{attempt_id}', lease expires at \
     {lease_expires_at}); retry once the writer finishes or the lease expires"
)]
pub(crate) struct SubagentContentReservationPruneGuard {
    pub(crate) session_id: String,
    pub(crate) attempt_id: String,
    pub(crate) lease_expires_at: i64,
}

#[derive(Debug, thiserror::Error)]
pub enum CheckpointPruneGuardError {
    /// Window A/B: a writer's in-flight marker is still live. The prune is
    /// a whole-chain rewrite of the shared ref and catalog, so ANY live
    /// marker — regardless of which session it belongs to — blocks the
    /// prune (safest granularity: a concurrent writer between stages
    /// (a)–(d) may hold objects/commits that neither the ref nor the
    /// catalog reaches yet, and its parent head may be about to be
    /// rewritten away). Markers expire after
    /// [`AGENT_TRACES_INFLIGHT_TTL_MS`], so the refusal is temporary.
    #[error(
        "refusing to prune traces checkpoints: a checkpoint write is in flight \
         (session '{session_id}', attempt '{attempt_id}'); in-flight markers \
         expire {ttl_ms} ms after the write starts — retry once the writer \
         finishes or the marker expires"
    )]
    LiveWriterMarker {
        session_id: String,
        attempt_id: String,
        ttl_ms: i64,
    },
    /// Window B residue: `refs/libra/traces` reaches commits that have no
    /// `agent_checkpoint` catalog row. The prune rebuild is catalog-driven,
    /// so rewriting now would silently drop those legal checkpoints;
    /// repairing the catalog is doctor's job.
    #[error(
        "refusing to prune traces checkpoints: refs/libra/traces reaches \
         {orphan_count} commit(s) with no agent_checkpoint catalog row \
         (first: {first_commit}); run `libra agent doctor --repair` to \
         backfill the catalog, then retry"
    )]
    RefCatalogOrphans {
        orphan_count: usize,
        first_commit: String,
    },
}

// ---------------------------------------------------------------------------
// AG-20 E4-libra checkpoint layout: chunking, content hash, manifest
// ---------------------------------------------------------------------------

/// E5 transcript chunking threshold: transcripts strictly larger than this
/// split into line-boundary-safe `.jsonl.%03d` parts. Frozen wire value —
/// matches the entire.io archive envelope (`50 * 1024 * 1024`).
pub const TRANSCRIPT_CHUNK_THRESHOLD_BYTES: usize = 50 * 1024 * 1024;

/// File name of the canonical lifecycle event stream inside a checkpoint
/// tree (`events/lifecycle.jsonl`, E4-libra).
pub const CHECKPOINT_LIFECYCLE_EVENTS_FILE: &str = "lifecycle.jsonl";

/// Per-role external schema stamped on `entries.lifecycle_events` in a
/// checkpoint manifest.  This follows the JSONL line schema rather than the
/// manifest container schema: v2 lines require typed `identity_scheme`.
pub const CHECKPOINT_LIFECYCLE_EVENTS_SCHEMA_VERSION: u32 = LIFECYCLE_EVENT_JSONL_SCHEMA_VERSION;

/// `metadata.json` external schema version written by the AG-20 writer.
///
/// v1 (pre-AG-20): `schema_version`, `checkpoint_id`, `session_id`,
/// `agent_kind`, `scope`, `provider_session_id`, `working_dir`,
/// `redaction_report`, `created_at`.
/// v2 (AG-20): all v1 fields (strictly additive — v1 readers keep working)
/// plus `model` (from the triggering lifecycle event when present, else
/// `"unknown"`, mirroring the E4-entire missing-`model` tolerance).
pub const CHECKPOINT_METADATA_SCHEMA_VERSION: u32 = 2;

/// `manifest.json` external schema version (first version).
pub const CHECKPOINT_MANIFEST_SCHEMA_VERSION: u32 = 1;

/// Ordered coverage roles for `content_hash.txt` — the sha256 runs over the
/// concatenation of these manifest entries' bytes in exactly this order.
/// `manifest.json` (written after the hash) and `content_hash.txt` itself
/// are excluded by construction. The transcript role contributes its
/// logical byte stream (chunks concatenated in part order), so the hash is
/// invariant under re-chunking. Mirrored in the manifest's
/// `content_hash.coverage` array so every checkpoint self-describes the
/// definition.
pub const CHECKPOINT_CONTENT_HASH_COVERAGE: [&str; 4] = [
    "metadata",
    "lifecycle_events",
    "transcript",
    "redaction_report",
];

#[cfg(test)]
tokio::task_local! {
    /// A test-only, task-scoped override for the fixed writer threshold.
    ///
    /// This deliberately does not use a process environment variable: a
    /// user-controlled debug environment must never change checkpoint layout.
    static TEST_TRANSCRIPT_CHUNK_THRESHOLD: usize;
}

/// Run a library test with a smaller in-process chunking threshold.
///
/// The override is task-local so parallel tests cannot change another
/// checkpoint writer's layout. Integration binaries intentionally cannot use
/// this seam; their coverage belongs in the in-crate writer tests.
#[cfg(test)]
pub(crate) async fn with_test_transcript_chunk_threshold<F: std::future::Future>(
    threshold: usize,
    future: F,
) -> F::Output {
    assert!(threshold > 0, "test transcript threshold must be non-zero");
    TEST_TRANSCRIPT_CHUNK_THRESHOLD
        .scope(threshold, future)
        .await
}

/// Resolve the fixed E5 chunking threshold.
///
/// Checkpoint layout is a durable wire contract, so production never reads a
/// process environment override. Library tests can exercise the chunked path
/// through the cfg(test) `with_test_transcript_chunk_threshold` scope.
pub fn transcript_chunk_threshold() -> usize {
    #[cfg(test)]
    if let Ok(threshold) = TEST_TRANSCRIPT_CHUNK_THRESHOLD.try_with(|threshold| *threshold) {
        return threshold;
    }

    TRANSCRIPT_CHUNK_THRESHOLD_BYTES
}

/// Split JSONL bytes into chunks of at most `max_size` bytes, cutting only
/// at line boundaries (`\n` stays with the line it terminates). E5 contract:
/// a single line whose bytes (including its terminator) exceed `max_size`
/// is a **hard error** — silently splitting mid-line would corrupt the JSONL
/// framing for every downstream reader.
///
/// Returns borrowed sub-slices (no copy); an empty input yields one empty
/// chunk so callers always have at least one part to name.
pub fn chunk_transcript_line_safe(content: &[u8], max_size: usize) -> Result<Vec<&[u8]>> {
    if max_size == 0 {
        return Err(anyhow!("transcript chunk size must be greater than zero"));
    }
    if content.len() <= max_size {
        return Ok(vec![content]);
    }

    let mut chunks = Vec::new();
    let mut chunk_start = 0usize;
    let mut line_start = 0usize;
    while line_start < content.len() {
        let line_end = match content[line_start..].iter().position(|&b| b == b'\n') {
            Some(offset) => line_start + offset + 1, // keep the terminator
            None => content.len(),                   // final unterminated line
        };
        let line_len = line_end - line_start;
        if line_len > max_size {
            return Err(anyhow!(
                "transcript line of {line_len} bytes exceeds the {max_size}-byte chunk \
                 threshold; refusing to split mid-line (E5). Raise the threshold or fix \
                 the producer emitting the oversized line"
            ));
        }
        if line_end - chunk_start > max_size {
            chunks.push(&content[chunk_start..line_start]);
            chunk_start = line_start;
        }
        line_start = line_end;
    }
    if chunk_start < content.len() {
        chunks.push(&content[chunk_start..]);
    }
    Ok(chunks)
}

/// Reassemble E5 chunks back into the logical transcript byte stream.
/// Inverse of [`chunk_transcript_line_safe`]: parts must be supplied in
/// manifest-declared order.
pub fn reassemble_transcript_chunks(chunks: &[Vec<u8>]) -> Vec<u8> {
    let total = chunks.iter().map(Vec::len).sum();
    let mut out = Vec::with_capacity(total);
    for chunk in chunks {
        out.extend_from_slice(chunk);
    }
    out
}

/// Compute `content_hash.txt`'s value: `sha256:` + 64 lowercase hex over the
/// concatenation of `sections` in the order given (callers pass the
/// [`CHECKPOINT_CONTENT_HASH_COVERAGE`] roles' bytes). No trailing newline —
/// the string IS the file content, mirroring the E4-entire format.
pub fn checkpoint_content_hash(sections: &[&[u8]]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for section in sections {
        hasher.update(section);
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Parse a `content_hash.txt` payload into its 64-lowercase-hex digest.
///
/// Writer output always carries the `sha256:` prefix; the reader ALSO
/// accepts legacy bare hex (E4-entire compatibility table) and surrounding
/// whitespace/newline slack. Returns `None` for anything else, so callers
/// fail closed on garbage.
pub fn parse_content_hash(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let hex = trimmed.strip_prefix("sha256:").unwrap_or(trimmed);
    let normalized = hex.to_ascii_lowercase();
    (normalized.len() == 64 && normalized.bytes().all(|b| b.is_ascii_hexdigit()))
        .then_some(normalized)
}

/// One written transcript part (single file or E5 chunk): tree-entry name,
/// blob OID, byte length.
#[derive(Debug, Clone)]
pub(crate) struct TranscriptPartRef {
    pub(crate) name: String,
    pub(crate) oid: ObjectHash,
    pub(crate) byte_len: usize,
}

/// (OID, byte length) pair for one single-blob manifest entry.
#[derive(Debug, Clone, Copy)]
pub(crate) struct ManifestBlobRef {
    pub(crate) oid: ObjectHash,
    pub(crate) byte_len: usize,
}

impl ManifestBlobRef {
    pub(crate) fn new(oid: ObjectHash, byte_len: usize) -> Self {
        Self { oid, byte_len }
    }
}

pub(crate) fn manifest_entry(
    path: &str,
    blob: ManifestBlobRef,
    media_type: &str,
    redaction: &str,
    schema_version: u32,
) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "oid": blob.oid.to_string(),
        "byte_len": blob.byte_len,
        "media_type": media_type,
        "compression": "none",
        "redaction": redaction,
        "schema_version": schema_version})
}

/// Serialise `manifest.json` for one E4-libra checkpoint: logical role →
/// `{path, oid, byte_len, media_type, compression, redaction,
/// schema_version}`. Paths are manifest-relative (relative to the
/// checkpoint's inner tree). A chunked transcript omits the single-blob
/// `oid` and instead declares ordered `parts` (E5: doctor/export/transcript
/// readers resolve chunks ONLY through this list, never by globbing tree
/// names). `redaction` is `"redacted"` for entries carrying scrubbed
/// content, `"report"` for the rule-hit report, `"none"` for derived
/// artifacts with no user content (content_hash).
#[allow(clippy::too_many_arguments)]
pub(crate) fn build_checkpoint_manifest_json(
    checkpoint_id: &str,
    transcript_file_name: &str,
    metadata: ManifestBlobRef,
    lifecycle_events: ManifestBlobRef,
    transcript_parts: &[TranscriptPartRef],
    transcript_total_len: usize,
    redaction_report: ManifestBlobRef,
    content_hash: ManifestBlobRef,
) -> Result<Vec<u8>> {
    let transcript_logical_path = format!("transcript/{transcript_file_name}");
    let mut transcript_entry = serde_json::json!({
        "path": transcript_logical_path,
        "byte_len": transcript_total_len,
        "media_type": "application/x-ndjson",
        "compression": "none",
        "redaction": "redacted",
        "schema_version": 1});
    // INVARIANT: transcript_entry is constructed as a JSON object above.
    let transcript_obj = transcript_entry
        .as_object_mut()
        .expect("transcript manifest entry is an object");
    if transcript_parts.len() == 1 {
        transcript_obj.insert(
            "oid".to_string(),
            serde_json::json!(transcript_parts[0].oid.to_string()),
        );
    } else {
        transcript_obj.insert("chunked".to_string(), serde_json::json!(true));
        transcript_obj.insert(
            "parts".to_string(),
            serde_json::json!(
                transcript_parts
                    .iter()
                    .map(|part| {
                        serde_json::json!({
                            "path": format!("transcript/{}", part.name),
                            "oid": part.oid.to_string(),
                            "byte_len": part.byte_len})
                    })
                    .collect::<Vec<_>>()
            ),
        );
    }

    let manifest = serde_json::json!({
    "schema_version": CHECKPOINT_MANIFEST_SCHEMA_VERSION,
    "checkpoint_id": checkpoint_id,
    "content_hash": {
        "algorithm": "sha256",
        "path": "content_hash.txt",
        // Self-describing hash definition: sha256 over the
        // concatenation of these roles' bytes in THIS order (the
        // transcript contributes its logical, reassembled stream).
        "coverage": CHECKPOINT_CONTENT_HASH_COVERAGE},
    "entries": {
        "metadata": manifest_entry(
            "metadata.json",
            metadata,
            "application/json",
            "redacted",
            CHECKPOINT_METADATA_SCHEMA_VERSION,
        ),
        "lifecycle_events": manifest_entry(
            "events/lifecycle.jsonl",
            lifecycle_events,
            "application/x-ndjson",
            "redacted",
            CHECKPOINT_LIFECYCLE_EVENTS_SCHEMA_VERSION,
        ),
        "transcript": transcript_entry,
        "redaction_report": manifest_entry(
            "redaction_report.json",
            redaction_report,
            "application/json",
            "report",
            1,
        ),
        "content_hash": manifest_entry(
            "content_hash.txt",
            content_hash,
            "text/plain",
            "none",
            1,
        )}});
    serde_json::to_vec_pretty(&manifest).context("failed to serialize checkpoint manifest.json")
}

// ---------------------------------------------------------------------------
// AG-20 window A/B closure: traces writer in-flight markers
// ---------------------------------------------------------------------------

/// TTL for traces-writer in-flight markers: markers older than this are
/// considered stale leftovers of a crashed writer and stop protecting
/// their OIDs. Ten minutes comfortably bounds a checkpoint write (which is
/// local-only I/O) while keeping crashed-writer garbage collectable.
pub const AGENT_TRACES_INFLIGHT_TTL_MS: i64 = 10 * 60 * 1000;

/// One in-flight traces-writer marker (window A/B guard, AG-20).
///
/// Stored as JSON in `metadata_kv` under scope
/// [`crate::internal::metadata::MetadataScope::AgentTracesInflight`] with
/// `target` = the Libra agent session id and `key` = the write attempt's
/// checkpoint UUID. The writer creates the marker BEFORE stage (a) (blob
/// writes) and clears it AFTER stage (d) (`agent_checkpoint` INSERT), so a
/// live marker tells the prune side "objects for this attempt may exist
/// that neither the ref nor the catalog reaches yet — do not collect".
///
/// Marker registration is fail-closed and precedes object construction. A
/// cleanup-pending marker remains durable beyond its normal TTL until its
/// candidate OIDs have been checked against repository roots and ownership is
/// retired for later repository GC.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TracesInflightMarker {
    /// Marker JSON schema version (additive evolution only).
    pub schema_version: u32,
    /// Libra agent session id (`agent_session.session_id`).
    pub session_id: String,
    /// Attempt UUID — the checkpoint id this write will (try to) catalog.
    pub attempt_id: String,
    /// Unpredictable writer generation. The `(session_id, attempt_id)` key is
    /// stable across takeover, so every marker mutation and final CAS must
    /// additionally compare this token.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    /// Unix epoch milliseconds when the writer created the marker.
    pub started_at_ms: i64,
    /// Time-to-live in milliseconds; `started_at_ms + ttl_ms <= now` means
    /// expired.
    pub ttl_ms: i64,
    /// Traces commit hash, filled in (best-effort) once the ref CAS
    /// succeeded — lets prune protect the exact commit during window B.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// OIDs written for this attempt (tree/metadata-blob level), filled in
    /// best-effort after stage (b) — lets prune protect loose objects
    /// during window A.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub oids: Vec<String>,
    /// OIDs this attempt is proven to have published with `create_new`
    /// semantics. Destructive recovery considers only this set. `oids`
    /// contains unresolved preclaims and is deliberately leak-safe after a
    /// crash between publish and ownership finalization.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_oids: Vec<String>,
    /// A rejected append owns the listed newly-created OIDs until a serialized
    /// reachability pass drains them. Pending cleanup ignores the ordinary
    /// marker TTL and blocks erasure from reporting success.
    #[serde(default, skip_serializing_if = "is_false")]
    pub cleanup_pending: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl TracesInflightMarker {
    /// A fresh marker for a write attempt starting now.
    pub fn new(session_id: &str, attempt_id: &str, started_at_ms: i64) -> Self {
        Self {
            schema_version: 3,
            session_id: session_id.to_string(),
            attempt_id: attempt_id.to_string(),
            generation: Some(uuid::Uuid::new_v4().to_string()),
            started_at_ms,
            ttl_ms: AGENT_TRACES_INFLIGHT_TTL_MS,
            commit: None,
            oids: Vec::new(),
            created_oids: Vec::new(),
            cleanup_pending: false,
        }
    }

    /// Whether the marker is still live at `now_ms`.
    ///
    /// BOUNDED (W2 §C.4.3): a deterministic absolute deadline from the
    /// persisted fields with `ttl_ms` capped at
    /// [`TRACES_INFLIGHT_MAX_LIVE_MS`]. Future-dated rows are handled by
    /// [`Self::time_fields_trustworthy`] (the listing fails closed on them
    /// and doctor retires them) — this predicate never re-anchors to the
    /// reading clock. Every liveness consumer inherits these definitions.
    pub fn is_live(&self, now_ms: i64) -> bool {
        // DETERMINISTIC absolute deadline from the PERSISTED fields only —
        // never re-anchored to the reading clock. TTL is capped so nothing
        // counts as live more than 24h past its recorded start. Rows whose
        // start lies beyond clock-skew tolerance in the FUTURE never reach
        // this predicate through the listing: `time_fields_trustworthy`
        // fails the listing CLOSED for them (they are corrupt, and silently
        // dropping them would strip a possibly-writing session's only
        // protection).
        self.started_at_ms
            .saturating_add(self.ttl_ms.clamp(0, TRACES_INFLIGHT_MAX_LIVE_MS))
            > now_ms
    }

    /// Whether the persisted time fields are plausible at `now_ms`: a start
    /// more than [`TRACES_INFLIGHT_FUTURE_SKEW_MS`] in the future cannot
    /// come from a healthy writer — the row is corrupt and every
    /// destructive consumer must stop (fail closed) rather than guess.
    pub fn time_fields_trustworthy(&self, now_ms: i64) -> bool {
        self.started_at_ms <= now_ms.saturating_add(TRACES_INFLIGHT_FUTURE_SKEW_MS)
    }
}

/// Upper bound on how long ANY ordinary in-flight marker may count as live,
/// regardless of what its persisted `ttl_ms` claims (fail-safe clamp; the
/// ordinary writer TTL is 10 minutes, so 24h is generous headroom).
pub const TRACES_INFLIGHT_MAX_LIVE_MS: i64 = 24 * 60 * 60 * 1000;

/// Clock-skew tolerance for `started_at_ms` (a healthy writer stamps "now";
/// anything further in the future is a corrupt row, not a long-lived one).
pub const TRACES_INFLIGHT_FUTURE_SKEW_MS: i64 = 5 * 60 * 1000;

/// The rejected-append fast-recovery transaction has only 250ms to retain
/// ownership evidence. Keep the marker it decodes comfortably below work
/// that could monopolize that SQLite writer. This is deliberately a recovery
/// read bound, not a wire-schema limit: doctor and ordinary marker consumers
/// still decode older, larger rows for deliberate inspection/repair.
pub(crate) const TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES: usize = 64 * 1024;

/// Aggregate `oids` plus `created_oids` entries the rejected-append recovery
/// path will validate and sort under its fixed grace. Current checkpoint
/// writers create only a small, bounded set of sidecars and trees; 768 keeps
/// substantial backward-compatible headroom without allowing a corrupt row
/// to turn a 250ms recovery transaction into unbounded CPU/allocation work.
/// It also keeps a SHA-256 marker vector below the serialized-byte bound.
pub(crate) const TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES: usize = 768;

/// A marker exceeded a bound that applies only to the short rejected-append
/// recovery transaction. The caller must leave the row untouched so doctor
/// or repository GC can inspect it outside that transaction's deadline.
#[derive(Debug, thiserror::Error)]
#[error(
    "traces in-flight marker has {actual} {kind}, exceeding the \
     {limit} {kind} rejected-cleanup recovery limit; inspect it with `libra agent doctor` before retrying"
)]
pub(crate) struct TracesInflightRejectedCleanupMarkerBoundExceeded {
    actual: usize,
    limit: usize,
    kind: &'static str,
}

fn validate_traces_inflight_marker(
    marker: &TracesInflightMarker,
    expected_session_id: &str,
    expected_attempt_id: &str,
) -> Result<()> {
    if marker.session_id != expected_session_id || marker.attempt_id != expected_attempt_id {
        bail!(
            "traces in-flight marker identity does not match its metadata row; inspect it with `libra agent doctor` before retrying"
        );
    }
    if marker.schema_version >= 3 {
        let generation = marker.generation.as_deref().ok_or_else(|| {
            anyhow!(
                "traces in-flight marker has no writer generation; inspect it with `libra agent doctor` before retrying"
            )
        })?;
        uuid::Uuid::parse_str(generation).map_err(|_| {
            anyhow!(
                "traces in-flight marker has an invalid writer generation; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    for oid in &marker.oids {
        crate::internal::ai::util::parse_repo_object_id(oid).map_err(|_| {
            anyhow!(
                "traces in-flight marker contains an invalid object identifier; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    for oid in &marker.created_oids {
        crate::internal::ai::util::parse_repo_object_id(oid).map_err(|_| {
            anyhow!(
                "traces in-flight marker contains an invalid created-object identifier; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    if let Some(commit) = marker.commit.as_deref() {
        crate::internal::ai::util::parse_commit_anchor_for_kind(
            git_internal::hash::get_hash_kind(),
            commit,
        )
        .and_then(|r| r.to_object_hash())
        .map_err(|_| {
            anyhow!(
                "traces in-flight marker contains an invalid commit identifier; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    Ok(())
}

pub(crate) fn decode_and_validate_traces_inflight_marker(
    value: &str,
    expected_session_id: &str,
    expected_attempt_id: &str,
) -> Result<TracesInflightMarker> {
    let marker = serde_json::from_str::<TracesInflightMarker>(value).map_err(|_| {
        anyhow!(
            "traces in-flight marker cannot be decoded; inspect it with `libra agent doctor` before retrying"
        )
    })?;
    validate_traces_inflight_marker(&marker, expected_session_id, expected_attempt_id)?;
    Ok(marker)
}

/// Decode a marker only when the rejected-append fast-recovery transaction
/// can safely validate and merge its ownership vectors before its fixed
/// deadline. Unlike [`decode_and_validate_traces_inflight_marker`], this
/// intentionally leaves oversized legacy/corrupt rows for the slower doctor
/// and GC paths rather than treating them as a schema incompatibility.
pub(crate) fn decode_and_validate_traces_inflight_marker_for_rejected_cleanup(
    value: &str,
    expected_session_id: &str,
    expected_attempt_id: &str,
) -> Result<TracesInflightMarker> {
    if value.len() > TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES {
        return Err(TracesInflightRejectedCleanupMarkerBoundExceeded {
            actual: value.len(),
            limit: TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES,
            kind: "serialized bytes",
        }
        .into());
    }
    let marker = decode_and_validate_traces_inflight_marker(
        value,
        expected_session_id,
        expected_attempt_id,
    )?;
    let oid_entries = marker.oids.len().saturating_add(marker.created_oids.len());
    if oid_entries > TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES {
        return Err(TracesInflightRejectedCleanupMarkerBoundExceeded {
            actual: oid_entries,
            limit: TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES,
            kind: "object-id entries",
        }
        .into());
    }
    Ok(marker)
}

/// Fence a stale writer marker without losing crash-recovery ownership.
/// Empty attempts can be removed immediately; attempts that pre-registered
/// any OID are promoted to a durable cleanup job for the normal reachability
/// drain.
pub async fn retire_stale_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
) -> Result<()> {
    let entry = crate::internal::metadata::MetadataKv::get_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        session_id,
        attempt_id,
    )
    .await
    .context("load stale traces writer marker")?;
    let Some(entry) = entry else {
        return Ok(());
    };
    let mut marker =
        decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)?;
    if marker.created_oids.is_empty() {
        clear_traces_inflight_marker(conn, session_id, attempt_id).await?;
    } else {
        marker.schema_version = marker.schema_version.max(2);
        marker.cleanup_pending = true;
        write_traces_inflight_marker(conn, &marker)
            .await
            .context("promote stale traces marker to durable cleanup")?;
    }
    Ok(())
}

/// Expected coverage fence for fail-closed writer registration. Empty plans
/// are valid for metadata/subagent checkpoints, but every claimed live/export
/// turn must still be owned before any object is built.
pub struct TracesCoverageFence<'a> {
    pub logical_turn_key: &'a str,
    pub owner: &'a str,
    pub fence_token: i64,
    pub reservation_state: &'a str,
}

/// Result of attempting to establish the one writer marker for a replayable
/// checkpoint. A duplicate delivery using the exact persisted generation is
/// not a failed write and must not consume a terminal retry/finalizer budget.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TracesWriteAttemptRegistration {
    Registered,
    AlreadyInFlightSameGeneration,
    /// The ref/catalog transaction committed while this delivery was waiting
    /// for the marker-registration writer lock. The caller must replay the
    /// durable checkpoint rather than register a fresh marker and append a
    /// second traces commit.
    AlreadyCommitted,
    /// The catalog receipt completed while this duplicate delivery was
    /// waiting for the marker-registration writer lock.  This is distinct
    /// from `AlreadyCommitted`: a covered terminal replay may acknowledge a
    /// receipt without the current action having its own checkpoint row.
    /// The caller must surface an idempotent terminal acknowledgement and
    /// must not charge a finalizer retry or register another marker.
    TerminalReceiptAlreadyComplete,
}

/// Registration state of the exact marker elected by a terminal receipt.
/// Catalog uses this only while deciding whether a changed-source duplicate
/// can safely transition an *unregistered* attempt to repair-required
/// quarantine. Any malformed or different marker is intentionally
/// incompatible rather than treated as absent.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TerminalAttemptMarkerStatus {
    Absent,
    RegisteredExact,
    Incompatible,
}

/// Read the durable marker slot in the caller's transaction. This is kept
/// content-free: the returned state exposes no marker payload or source
/// metadata. A writer transaction serializes this observation with both a
/// catalog source-fence transition and later marker registration.
pub(crate) async fn terminal_attempt_marker_status<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    checkpoint_id: &str,
    marker_generation: &str,
) -> Result<TerminalAttemptMarkerStatus> {
    let entry = crate::internal::metadata::MetadataKv::get_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        session_id,
        checkpoint_id,
    )
    .await
    .context("read terminal traces marker state")?;
    let Some(entry) = entry else {
        return Ok(TerminalAttemptMarkerStatus::Absent);
    };
    let Ok(marker) =
        decode_and_validate_traces_inflight_marker(&entry.value, session_id, checkpoint_id)
    else {
        return Ok(TerminalAttemptMarkerStatus::Incompatible);
    };
    if marker.generation.as_deref() == Some(marker_generation) {
        Ok(TerminalAttemptMarkerStatus::RegisteredExact)
    } else {
        Ok(TerminalAttemptMarkerStatus::Incompatible)
    }
}

/// Authorize the final durable marker commit at one SQLite statement
/// boundary. A read-only lease preflight is insufficient: the lease or the
/// immutable capture deadline can expire while a writer prepares its marker.
///
/// Callers that own a larger transaction may perform further scoped work
/// afterwards, but must take this same authorization again immediately before
/// their transaction commits. The caller must not issue SQL after success;
/// it must await the commit acknowledgement without cancelling it.
async fn authorize_traces_marker_final_commit<C: ConnectionTrait>(
    conn: &C,
    capture_scope: Option<&CaptureScope>,
    operation: &'static str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<()> {
    ensure_before_traces_marker_deadline(deadline)?;
    match authorize_final_capture_commit(capture_scope, conn, deadline).await {
        Ok(()) => Ok(()),
        Err(CaptureFinalCommitAuthorizationError::DeadlineElapsed) => {
            Err(TracesMarkerDeadlineExceeded.into())
        }
        Err(error) => Err(anyhow::Error::new(error))
            .with_context(|| format!("authorize final capture commit before {operation}")),
    }
}

async fn await_traces_marker_commit(
    txn: DatabaseTransaction,
    commit_context: &'static str,
) -> Result<()> {
    #[cfg(test)]
    if let Some(pause) = TEST_TRACES_COMMIT_DISPATCH_PAUSE
        .try_with(|pause| pause.clone())
        .ok()
        .flatten()
    {
        let mut commit = std::pin::pin!(txn.commit());
        let first_poll = std::future::poll_fn(|context| match commit.as_mut().poll(context) {
            Poll::Ready(result) => Poll::Ready(Some(result)),
            Poll::Pending => Poll::Ready(None),
        })
        .await;
        let dispatched = pause
            .dispatched
            .lock()
            .expect("test commit-dispatch notifier mutex is not poisoned")
            .take()
            .expect("test commit-dispatch notifier is installed exactly once");
        let was_pending = first_poll.is_none();
        let _ = dispatched.send(was_pending);
        if let Some(result) = first_poll {
            return result.context(commit_context);
        }
        let release = pause
            .release
            .lock()
            .expect("test commit-dispatch release mutex is not poisoned")
            .take()
            .expect("test commit-dispatch release is installed exactly once");
        release
            .await
            .context("test commit-dispatch release sender dropped before reader lock release")?;
        return commit.await.context(commit_context);
    }

    txn.commit().await.context(commit_context)
}

/// The SQLite authorization is the durable deadline linearization point.
/// Once it succeeds, SQLx may already dispatch COMMIT on the next poll, so
/// this helper deliberately awaits that acknowledgement without a timeout or
/// any subsequent deadline recheck.
async fn commit_traces_marker_transaction<T>(
    txn: DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    operation: &'static str,
    deadline: Option<CaptureCommitDeadline>,
    commit_context: &'static str,
    value: T,
) -> Result<T> {
    if let Err(error) =
        authorize_traces_marker_final_commit(&txn, capture_scope, operation, deadline).await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    await_traces_marker_commit(txn, commit_context).await?;
    Ok(value)
}

/// Establish one writer attempt under the same SQLite writer lock used by
/// erasure. The session/tombstone barrier and any coverage fences are checked
/// in the marker transaction; a failed marker write aborts the checkpoint
/// before loose objects or object-index tasks can exist.
pub async fn register_traces_write_attempt(
    conn: &DatabaseConnection,
    marker: &TracesInflightMarker,
    coverage_fences: &[TracesCoverageFence<'_>],
) -> Result<()> {
    match register_traces_write_attempt_with_capture_scope(
        conn,
        None,
        marker,
        coverage_fences,
        None,
        None,
    )
    .await?
    {
        TracesWriteAttemptRegistration::Registered => Ok(()),
        // This legacy wrapper is only used by fresh-generation callers. A
        // same-generation contender must use the typed checkpoint boundary
        // so it can return an in-flight outcome without mutating retry state.
        TracesWriteAttemptRegistration::AlreadyInFlightSameGeneration => {
            bail!("traces writer attempt is already in flight for this marker generation")
        }
        TracesWriteAttemptRegistration::AlreadyCommitted => {
            bail!("traces checkpoint is already durably committed")
        }
        TracesWriteAttemptRegistration::TerminalReceiptAlreadyComplete => {
            bail!("terminal traces receipt is already complete")
        }
    }
}

/// Scope-aware marker registration. The workspace lease is checked after the
/// registration transaction has begun, so an expiry between a hook's catalog
/// reservation and marker creation cannot leave a mutable writer attempt.
enum TracesWriteAttemptPreparation {
    /// The marker DML is staged in `txn`; the caller must run the single final
    /// database authorization and then await the non-cancellable commit.
    Commit(DatabaseTransaction),
    /// No marker DML remains to commit because an idempotent outcome already
    /// rolled its transaction back.
    Complete(TracesWriteAttemptRegistration),
}

pub(crate) async fn register_traces_write_attempt_with_capture_scope(
    conn: &DatabaseConnection,
    capture_scope: Option<&crate::internal::ai::capture_scope::CaptureScope>,
    marker: &TracesInflightMarker,
    coverage_fences: &[TracesCoverageFence<'_>],
    expected_checkpoint_scope: Option<CheckpointScope>,
    terminal_attempt_fence: Option<
        &crate::internal::ai::capture::catalog::CaptureCatalogTerminalAttemptFence,
    >,
) -> Result<TracesWriteAttemptRegistration> {
    let preparation = register_traces_write_attempt_with_capture_scope_prepare(
        conn,
        capture_scope,
        marker,
        coverage_fences,
        expected_checkpoint_scope,
        terminal_attempt_fence,
        None,
    )
    .await?;
    match preparation {
        TracesWriteAttemptPreparation::Commit(txn) => {
            commit_traces_marker_transaction(
                txn,
                capture_scope,
                "committing marker registration",
                None,
                "commit traces writer-attempt registration",
                TracesWriteAttemptRegistration::Registered,
            )
            .await
        }
        TracesWriteAttemptPreparation::Complete(outcome) => Ok(outcome),
    }
}

/// Deadline-aware scoped marker registration for capture paths that must not
/// publish a new durable writer claim after their absolute budget expires.
/// Existing callers retain the unbounded wrapper above for legacy/live flows.
pub(crate) async fn register_traces_write_attempt_with_capture_scope_until(
    conn: &DatabaseConnection,
    capture_scope: Option<&crate::internal::ai::capture_scope::CaptureScope>,
    marker: &TracesInflightMarker,
    coverage_fences: &[TracesCoverageFence<'_>],
    expected_checkpoint_scope: Option<CheckpointScope>,
    terminal_attempt_fence: Option<
        &crate::internal::ai::capture::catalog::CaptureCatalogTerminalAttemptFence,
    >,
    deadline: CaptureCommitDeadline,
) -> Result<TracesWriteAttemptRegistration> {
    let preparation = register_traces_write_attempt_with_capture_scope_prepare(
        conn,
        capture_scope,
        marker,
        coverage_fences,
        expected_checkpoint_scope,
        terminal_attempt_fence,
        Some(deadline),
    )
    .await?;
    match preparation {
        TracesWriteAttemptPreparation::Commit(txn) => {
            commit_traces_marker_transaction(
                txn,
                capture_scope,
                "committing marker registration",
                Some(deadline),
                "commit traces writer-attempt registration",
                TracesWriteAttemptRegistration::Registered,
            )
            .await
        }
        TracesWriteAttemptPreparation::Complete(outcome) => Ok(outcome),
    }
}

async fn register_traces_write_attempt_with_capture_scope_prepare(
    conn: &DatabaseConnection,
    capture_scope: Option<&crate::internal::ai::capture_scope::CaptureScope>,
    marker: &TracesInflightMarker,
    coverage_fences: &[TracesCoverageFence<'_>],
    expected_checkpoint_scope: Option<CheckpointScope>,
    terminal_attempt_fence: Option<
        &crate::internal::ai::capture::catalog::CaptureCatalogTerminalAttemptFence,
    >,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<TracesWriteAttemptPreparation> {
    ensure_before_traces_marker_deadline(deadline)?;
    let txn = begin_traces_marker_write_transaction_until(
        conn,
        deadline,
        "begin traces writer-attempt registration",
    )
    .await?;
    if let Some(scope) = capture_scope
        && let Err(error) = await_traces_marker_precommit_read_until(
            deadline,
            "verify capture workspace lease before marker registration",
            scope.assert_workspace_fence_live(&txn),
        )
        .await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    // A terminal writer is elected by a prior catalog transaction because
    // snapshot preparation may be expensive. Revalidate that opaque election
    // under this *same* writer lock before publishing its marker: a changed
    // source may have safely quarantined an unregistered attempt while this
    // process was paused, and that stale writer must never append afterward.
    if let Some(fence) = terminal_attempt_fence {
        let marker_generation = marker.generation.as_deref().ok_or_else(|| {
            anyhow!(
                "terminal checkpoint writer marker has no generation before registration; inspect it with `libra agent doctor` before retrying"
            )
        })?;
        let registration = match await_traces_marker_precommit_read_until(
            deadline,
            "verify catalog terminal attempt before marker registration",
            async {
                crate::internal::ai::capture::catalog::verify_terminal_attempt_registration(
                    &txn,
                    fence,
                    &marker.attempt_id,
                    marker_generation,
                )
                .await
                .context("verify terminal attempt registration")
            },
        )
        .await
        {
            Ok(registration) => registration,
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error);
            }
        };
        match registration {
            crate::internal::ai::capture::catalog::CaptureCatalogTerminalAttemptRegistration::Authorized => {}
            crate::internal::ai::capture::catalog::CaptureCatalogTerminalAttemptRegistration::TerminalReceiptAlreadyComplete => {
                txn.rollback().await.ok();
                return Ok(TracesWriteAttemptPreparation::Complete(
                    TracesWriteAttemptRegistration::TerminalReceiptAlreadyComplete,
                ));
            }
        }
    }
    let writable = match await_traces_marker_precommit_read_until(
        deadline,
        "verify traces writer session/tombstone barrier",
        async {
            txn.query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 AS writable
                 FROM agent_session s
                 WHERE s.session_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_import_tombstone t
                     WHERE t.agent_kind = s.agent_kind
                       AND t.provider_session_id = s.provider_session_id
                   )",
                [marker.session_id.clone().into()],
            ))
            .await
            .context("query traces writer session/tombstone barrier")
        },
    )
    .await
    {
        Ok(writable) => writable,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if writable.is_none() {
        txn.rollback().await.ok();
        bail!("agent session was erased or is unavailable for checkpoint writing");
    }

    // The optimistic replay probe happens before this call. Re-check the
    // durable checkpoint under this same writer lock as marker registration:
    // another delivery may have committed the ref/catalog and retired its
    // marker while this delivery was between that probe and this transaction.
    // A checkpoint id is globally unique, so a mismatched session/scope is a
    // hard identity conflict rather than permission to append another ref.
    if let Some(expected_scope) = expected_checkpoint_scope {
        let committed = match await_traces_marker_precommit_read_until(
            deadline,
            "verify durable checkpoint before marker registration",
            async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT session_id, scope FROM agent_checkpoint
                     WHERE checkpoint_id = ? LIMIT 1",
                    [marker.attempt_id.clone().into()],
                ))
                .await
                .context("query durable checkpoint before marker registration")
            },
        )
        .await
        {
            Ok(committed) => committed,
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error);
            }
        };
        if let Some(committed) = committed {
            let session_id: String = committed
                .try_get_by("session_id")
                .context("decode durable checkpoint session before marker registration")?;
            let scope: String = committed
                .try_get_by("scope")
                .context("decode durable checkpoint scope before marker registration")?;
            txn.rollback().await.ok();
            if session_id == marker.session_id && scope == expected_scope.as_str() {
                return Ok(TracesWriteAttemptPreparation::Complete(
                    TracesWriteAttemptRegistration::AlreadyCommitted,
                ));
            }
            bail!("checkpoint identity is already durably committed for another session or scope");
        }
    }
    for fence in coverage_fences {
        let owned = match await_traces_marker_precommit_read_until(
            deadline,
            "verify traces writer coverage fence",
            async {
                txn.query_one_raw(Statement::from_sql_and_values(
                    txn.get_database_backend(),
                    "SELECT 1 AS owned FROM agent_coverage_claim
                     WHERE session_id = ? AND logical_turn_key = ?
                       AND coverage_schema_version = 1 AND state = ?
                       AND owner = ? AND fence_token = ?",
                    [
                        marker.session_id.clone().into(),
                        fence.logical_turn_key.into(),
                        fence.reservation_state.into(),
                        fence.owner.into(),
                        fence.fence_token.into(),
                    ],
                ))
                .await
                .context("query traces writer coverage fence")
            },
        )
        .await
        {
            Ok(owned) => owned,
            Err(error) => {
                txn.rollback().await.ok();
                return Err(error);
            }
        };
        if owned.is_none() {
            txn.rollback().await.ok();
            bail!(
                "coverage fence for turn '{}' is no longer owned; checkpoint write aborted",
                fence.logical_turn_key
            );
        }
    }

    // A checkpoint id is replay-stable, so concurrent deliveries can arrive
    // with the same persisted terminal generation. Do not turn that into an
    // upsert race: an existing live marker owns the one active append, and a
    // different generation is an explicit fence loss. Only an expired empty
    // marker may be re-registered for a crash retry of the *same* generation.
    let existing_marker = match await_traces_marker_precommit_read_until(
        deadline,
        "load existing traces writer marker before registration",
        crate::internal::metadata::MetadataKv::get_with_conn(
            &txn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &marker.session_id,
            &marker.attempt_id,
        ),
    )
    .await
    {
        Ok(existing_marker) => existing_marker,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    if let Some(entry) = existing_marker {
        let existing =
            decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)?;
        let requested_generation = marker.generation.as_deref().ok_or_else(|| {
            anyhow!(
                "checkpoint writer marker has no generation before registration; inspect it with `libra agent doctor` before retrying"
            )
        })?;
        if existing.generation.as_deref() != Some(requested_generation) {
            txn.rollback().await.ok();
            bail!(
                "checkpoint writer marker generation was fenced or replaced; retry the operation"
            );
        }
        let now_ms = chrono::Utc::now().timestamp_millis();
        // Match the listing/maintenance consumers: a marker whose clock is
        // implausibly ahead is corrupt, not a healthy long-lived writer.
        // Returning an in-flight outcome here would hide the repair-needed
        // state until the persisted future deadline elapsed.
        if !existing.time_fields_trustworthy(now_ms) {
            txn.rollback().await.ok();
            bail!(
                "checkpoint writer marker carries an implausible timestamp; inspect it with \
                 `libra agent doctor` before retrying",
            );
        }
        if existing.cleanup_pending {
            txn.rollback().await.ok();
            bail!("checkpoint writer marker requires durable cleanup before retrying");
        }
        if existing.is_live(now_ms) {
            txn.rollback().await.ok();
            return Ok(TracesWriteAttemptPreparation::Complete(
                TracesWriteAttemptRegistration::AlreadyInFlightSameGeneration,
            ));
        }
        // A live writer persists every newly created object into its marker
        // before it can publish the ref/catalog transaction. That ownership
        // evidence does not turn a healthy writer into a cleanup job: a
        // duplicate must observe it as in-flight, not consume a terminal
        // finalizer retry budget while the elected writer is appending.
        if !existing.created_oids.is_empty() {
            txn.rollback().await.ok();
            bail!("checkpoint writer marker requires durable cleanup before retrying");
        }
    }
    if let Err(error) = ensure_before_traces_marker_dml(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("verify marker deadline before marker persistence");
    }
    if let Err(error) = write_traces_inflight_marker(&txn, marker)
        .await
        .context("register traces writer marker")
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    Ok(TracesWriteAttemptPreparation::Commit(txn))
}

/// Upsert an in-flight marker row. Exported (not a stable API) so the
/// writer, the prune side, and integration tests share one implementation.
pub async fn write_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    marker: &TracesInflightMarker,
) -> Result<()> {
    validate_traces_inflight_marker(marker, &marker.session_id, &marker.attempt_id)?;
    let value =
        serde_json::to_string(marker).context("failed to serialize traces in-flight marker")?;
    crate::internal::metadata::MetadataKv::set_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        &marker.session_id,
        &marker.attempt_id,
        &value,
        crate::internal::metadata::MetadataValueType::Text,
    )
    .await
    .context("failed to persist traces in-flight marker")?;
    Ok(())
}

/// Update an existing marker without allowing a stale writer to overwrite a
/// replacement generation registered under the same stable metadata key.
pub async fn update_traces_inflight_marker_if_generation<C: ConnectionTrait>(
    conn: &C,
    marker: &TracesInflightMarker,
    expected_generation: &str,
) -> Result<bool> {
    validate_traces_inflight_marker(marker, &marker.session_id, &marker.attempt_id)?;
    if marker.generation.as_deref() != Some(expected_generation) {
        bail!("refusing to update a traces marker with a mismatched writer generation");
    }
    let value =
        serde_json::to_string(marker).context("failed to serialize traces in-flight marker")?;
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "UPDATE metadata_kv
             SET value = ?, value_type = 'text', updated_at = ?
             WHERE scope = 'agent_traces_inflight' AND target = ? AND key = ?
               AND json_extract(value, '$.generation') = ?",
            [
                value.into(),
                chrono::Utc::now().to_rfc3339().into(),
                marker.session_id.clone().into(),
                marker.attempt_id.clone().into(),
                expected_generation.into(),
            ],
        ))
        .await
        .context("failed to update exact traces writer generation")?;
    Ok(result.rows_affected() == 1)
}

/// Scope-aware conditional marker update for callers that already own the
/// SQLite write transaction. Both the preflight and the final lease DML run
/// in that transaction, so a caller must roll it back if this returns an
/// error. Requiring `DatabaseTransaction` makes that atomicity explicit.
async fn update_traces_inflight_marker_if_generation_with_capture_scope_inner(
    txn: &DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    marker: &TracesInflightMarker,
    expected_generation: &str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<bool> {
    if let Some(scope) = capture_scope {
        await_traces_marker_precommit_read_until(
            deadline,
            "verify capture workspace lease before marker update",
            scope.assert_workspace_fence_live(txn),
        )
        .await?;
    }
    ensure_before_traces_marker_dml(deadline)?;
    let updated =
        update_traces_inflight_marker_if_generation(txn, marker, expected_generation).await?;
    if updated {
        authorize_traces_marker_final_commit(
            txn,
            capture_scope,
            "committing marker update",
            deadline,
        )
        .await?;
    }
    Ok(updated)
}

/// Scope-aware conditional marker update for callers that already own the
/// SQLite write transaction. Both the preflight and the final lease DML run
/// in that transaction, so a caller must roll it back if this returns an
/// error. Requiring `DatabaseTransaction` makes that atomicity explicit.
pub async fn update_traces_inflight_marker_if_generation_with_capture_scope(
    txn: &DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    marker: &TracesInflightMarker,
    expected_generation: &str,
) -> Result<bool> {
    update_traces_inflight_marker_if_generation_with_capture_scope_inner(
        txn,
        capture_scope,
        marker,
        expected_generation,
        None,
    )
    .await
}

/// Deadline-aware scoped marker update for recovery paths that may retain
/// evidence but must not publish a new marker state after their bounded grace
/// period. Only the lease preflight read is cancellable; marker DML, final
/// authorization, and COMMIT remain outside a timeout once they start.
pub(crate) async fn update_traces_inflight_marker_if_generation_with_capture_scope_until(
    txn: &DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    marker: &TracesInflightMarker,
    expected_generation: &str,
    deadline: CaptureCommitDeadline,
) -> Result<bool> {
    update_traces_inflight_marker_if_generation_with_capture_scope_inner(
        txn,
        capture_scope,
        marker,
        expected_generation,
        Some(deadline),
    )
    .await
}

/// Remove one in-flight marker (stage (d) complete, or prune-side cleanup
/// of an expired marker). Returns whether a row was removed.
pub async fn clear_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
) -> Result<bool> {
    crate::internal::metadata::MetadataKv::unset_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
        session_id,
        attempt_id,
    )
    .await
    .context("failed to clear traces in-flight marker")
}

/// Remove one marker only when the metadata row still belongs to the exact
/// writer generation that the caller registered.
pub async fn clear_traces_inflight_marker_if_generation<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
) -> Result<bool> {
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM metadata_kv
             WHERE scope = 'agent_traces_inflight' AND target = ? AND key = ?
               AND json_extract(value, '$.generation') = ?",
            [session_id.into(), attempt_id.into(), generation.into()],
        ))
        .await
        .context("failed to clear exact traces writer generation")?;
    Ok(result.rows_affected() == 1)
}

/// Scope-aware conditional marker removal for callers that already own the
/// SQLite write transaction. The final lease DML follows a successful delete,
/// so an expired capture worker cannot commit removal of recovery evidence.
async fn clear_traces_inflight_marker_if_generation_with_capture_scope_inner(
    txn: &DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<bool> {
    if let Some(scope) = capture_scope {
        await_traces_marker_precommit_read_until(
            deadline,
            "verify capture workspace lease before marker removal",
            scope.assert_workspace_fence_live(txn),
        )
        .await?;
    }
    ensure_before_traces_marker_dml(deadline)?;
    let cleared =
        clear_traces_inflight_marker_if_generation(txn, session_id, attempt_id, generation).await?;
    if cleared {
        authorize_traces_marker_final_commit(
            txn,
            capture_scope,
            "committing marker removal",
            deadline,
        )
        .await?;
    }
    Ok(cleared)
}

/// Scope-aware conditional marker removal for callers that already own the
/// SQLite write transaction. The final lease DML follows a successful delete,
/// so an expired capture worker cannot commit removal of recovery evidence.
pub async fn clear_traces_inflight_marker_if_generation_with_capture_scope(
    txn: &DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
) -> Result<bool> {
    clear_traces_inflight_marker_if_generation_with_capture_scope_inner(
        txn,
        capture_scope,
        session_id,
        attempt_id,
        generation,
        None,
    )
    .await
}

/// Deadline-aware scoped marker removal for a caller-owned transaction.
/// Only its lease preflight read is cancellable; the conditional DELETE and
/// final authorization remain non-cancellable once dispatched.
pub(crate) async fn clear_traces_inflight_marker_if_generation_with_capture_scope_until(
    txn: &DatabaseTransaction,
    capture_scope: Option<&CaptureScope>,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
    deadline: CaptureCommitDeadline,
) -> Result<bool> {
    clear_traces_inflight_marker_if_generation_with_capture_scope_inner(
        txn,
        capture_scope,
        session_id,
        attempt_id,
        generation,
        Some(deadline),
    )
    .await
}

/// Clear a writer marker only while it still represents an ordinary live
/// attempt. Rejected-append cleanup upgrades the same row to
/// `cleanup_pending`; error finalizers must never erase that durable job.
pub async fn clear_non_cleanup_traces_inflight_marker<C: ConnectionTrait>(
    conn: &C,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
) -> Result<bool> {
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "DELETE FROM metadata_kv
             WHERE scope = 'agent_traces_inflight' AND target = ? AND key = ?
               AND json_extract(value, '$.generation') = ?
               AND COALESCE(json_extract(value, '$.cleanup_pending'), 0) = 0",
            [session_id.into(), attempt_id.into(), generation.into()],
        ))
        .await
        .context("failed to clear exact ordinary traces writer generation")?;
    Ok(result.rows_affected() == 1)
}

/// Scope-aware ordinary-marker retirement. Cleanup is a mutation too: once a
/// workspace lease has expired, an old hook must neither publish a marker nor
/// erase the durable recovery evidence owned by the new lease holder.
pub async fn clear_non_cleanup_traces_inflight_marker_with_capture_scope(
    conn: &DatabaseConnection,
    capture_scope: Option<&crate::internal::ai::capture_scope::CaptureScope>,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
) -> Result<bool> {
    let (txn, cleared) = clear_non_cleanup_traces_inflight_marker_with_capture_scope_prepare(
        conn,
        capture_scope,
        session_id,
        attempt_id,
        generation,
        None,
    )
    .await?;
    commit_traces_marker_transaction(
        txn,
        capture_scope,
        "committing ordinary marker retirement",
        None,
        "commit traces writer-marker retirement transaction",
        cleared,
    )
    .await
}

/// Deadline-aware ordinary-marker retirement for capture paths. It is kept
/// separate from recovery cleanup: an elapsed capture deadline must not erase
/// a marker that another worker may need to diagnose or repair.
pub(crate) async fn clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
    conn: &DatabaseConnection,
    capture_scope: Option<&crate::internal::ai::capture_scope::CaptureScope>,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
    deadline: CaptureCommitDeadline,
) -> Result<bool> {
    let (txn, cleared) = clear_non_cleanup_traces_inflight_marker_with_capture_scope_prepare(
        conn,
        capture_scope,
        session_id,
        attempt_id,
        generation,
        Some(deadline),
    )
    .await?;
    commit_traces_marker_transaction(
        txn,
        capture_scope,
        "committing ordinary marker retirement",
        Some(deadline),
        "commit traces writer-marker retirement transaction",
        cleared,
    )
    .await
}

async fn clear_non_cleanup_traces_inflight_marker_with_capture_scope_prepare(
    conn: &DatabaseConnection,
    capture_scope: Option<&crate::internal::ai::capture_scope::CaptureScope>,
    session_id: &str,
    attempt_id: &str,
    generation: &str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<(DatabaseTransaction, bool)> {
    ensure_before_traces_marker_deadline(deadline)?;
    let txn = begin_traces_marker_write_transaction_until(
        conn,
        deadline,
        "begin traces writer-marker retirement transaction",
    )
    .await?;
    if let Some(scope) = capture_scope
        && let Err(error) = await_traces_marker_precommit_read_until(
            deadline,
            "verify capture workspace lease before marker retirement",
            scope.assert_workspace_fence_live(&txn),
        )
        .await
    {
        txn.rollback().await.ok();
        return Err(error);
    }
    if let Err(error) = ensure_before_traces_marker_dml(deadline) {
        txn.rollback().await.ok();
        return Err(error).context("verify marker deadline before marker retirement");
    }
    let result = match txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "DELETE FROM metadata_kv
             WHERE scope = 'agent_traces_inflight' AND target = ? AND key = ?
               AND json_extract(value, '$.generation') = ?
               AND COALESCE(json_extract(value, '$.cleanup_pending'), 0) = 0",
            [session_id.into(), attempt_id.into(), generation.into()],
        ))
        .await
        .context("failed to clear exact ordinary traces writer generation")
    {
        Ok(result) => result,
        Err(error) => {
            txn.rollback().await.ok();
            return Err(error);
        }
    };
    Ok((txn, result.rows_affected() == 1))
}

/// List the LIVE (non-expired at `now_ms`) or durable cleanup-pending
/// in-flight markers across all sessions — the prune-side entry point: any
/// OID/commit named by a returned marker must be treated as reachable, and
/// (fail-closed) either kind of marker for a session should defer pruning
/// that session's chain. Cleanup ownership deliberately outlives the ordinary
/// writer TTL until doctor/GC retires it.
///
/// Malformed rows fail the listing closed: their OID ownership and TTL cannot
/// be trusted, so destructive prune/erasure must stop until
/// `libra agent doctor` reports the row for manual recovery.
pub async fn list_live_traces_inflight_markers<C: ConnectionTrait>(
    conn: &C,
    now_ms: i64,
) -> Result<Vec<TracesInflightMarker>> {
    let entries = crate::internal::metadata::MetadataKv::list_scope_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
    )
    .await
    .context("failed to list traces in-flight markers")?;
    let mut live = Vec::new();
    for entry in entries {
        match decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key) {
            Ok(marker) => {
                // A future-dated start beyond skew tolerance is a CORRUPT
                // row, not an expired one: silently filtering it would strip
                // a possibly-still-writing session's only protection, so the
                // listing fails CLOSED and destructive consumers stop.
                if !marker.time_fields_trustworthy(now_ms) {
                    bail!(
                        "traces in-flight marker carries an implausible timestamp; inspect it \
                         with `libra agent doctor` before destructive maintenance"
                    );
                }
                if marker.cleanup_pending || marker.is_live(now_ms) {
                    live.push(marker);
                }
            }
            Err(err) => return Err(err),
        }
    }
    Ok(live)
}

pub(crate) async fn list_all_traces_inflight_markers<C: ConnectionTrait>(
    conn: &C,
) -> Result<Vec<TracesInflightMarker>> {
    let entries = crate::internal::metadata::MetadataKv::list_scope_with_conn(
        conn,
        crate::internal::metadata::MetadataScope::AgentTracesInflight,
    )
    .await
    .context("failed to list traces in-flight markers")?;
    entries
        .into_iter()
        .map(|entry| {
            decode_and_validate_traces_inflight_marker(&entry.value, &entry.target, &entry.key)
        })
        .collect()
}

/// Probe the checkpoint catalog by traces commit hash: returns the
/// `checkpoint_id` of the row whose `traces_commit` equals `commit_hash`,
/// if any. The writer calls this between ref CAS and catalog INSERT so a
/// crash-retry (or a doctor repair that already backfilled the row from
/// the ref) skips the INSERT instead of duplicating the commit's catalog
/// entry; doctor's window-B repair uses the same probe for idempotency.
pub async fn agent_checkpoint_id_for_traces_commit<C: ConnectionTrait>(
    conn: &C,
    commit_hash: &str,
) -> Result<Option<String>> {
    let backend = conn.get_database_backend();
    let row = conn
        .query_one_raw(Statement::from_sql_and_values(
            backend,
            "SELECT checkpoint_id FROM agent_checkpoint WHERE traces_commit = ? LIMIT 1",
            [Value::from(commit_hash)],
        ))
        .await
        .context("failed to probe agent_checkpoint by traces_commit")?;
    row.map(|row| {
        row.try_get_by("checkpoint_id")
            .context("decode agent_checkpoint.checkpoint_id")
    })
    .transpose()
}

#[cfg(test)]
mod tests {
    use std::{path::Path, time::Duration};

    use sea_orm::{ConnectOptions, Database, Statement, TransactionTrait};
    use tempfile::TempDir;

    use super::*;

    async fn scoped_marker_fixture() -> (DatabaseConnection, CaptureScope, TracesInflightMarker) {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open scoped traces fixture");
        let (scope, marker) = seed_scoped_marker_fixture(&conn).await;
        (conn, scope, marker)
    }

    async fn seed_scoped_marker_fixture(
        conn: &DatabaseConnection,
    ) -> (CaptureScope, TracesInflightMarker) {
        let backend = conn.get_database_backend();
        for statement in [
            "CREATE TABLE config_kv (id INTEGER PRIMARY KEY)",
            "CREATE TABLE agent_session (
                session_id TEXT PRIMARY KEY,
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL
            )",
            "CREATE TABLE agent_import_tombstone (
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL
            )",
            "CREATE TABLE workspace_record (
                workspace_id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                lease_fence INTEGER NOT NULL,
                state TEXT NOT NULL,
                lease_owner TEXT,
                lease_expires_at INTEGER
            )",
            "CREATE TABLE metadata_kv (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                target TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                value_type TEXT NOT NULL DEFAULT 'text',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(scope, target, key)
            )",
        ] {
            conn.execute_raw(Statement::from_string(backend, statement.to_string()))
                .await
                .expect("create scoped traces fixture table");
        }
        conn.execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id)
             VALUES ('scope-marker-session', 'claude_code', 'scope-marker-provider')"
                .to_string(),
        ))
        .await
        .expect("seed scoped traces session");
        conn.execute_raw(Statement::from_string(
            backend,
            "INSERT INTO workspace_record (
                workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
             ) VALUES ('scope-marker-workspace', 'scope-marker-repo', 7,
                       'active', 'scope-marker-owner', unixepoch('now') * 1000 + 600000)"
                .to_string(),
        ))
        .await
        .expect("seed live capture workspace lease");
        let scope = CaptureScope {
            repo_id: "scope-marker-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("scope-marker-workspace".to_string()),
            workspace_fence: Some(7),
        };
        let marker = TracesInflightMarker::new(
            "scope-marker-session",
            "scope-marker-checkpoint",
            chrono::Utc::now().timestamp_millis(),
        );
        (scope, marker)
    }

    async fn connect_file_sqlite(path: &Path) -> DatabaseConnection {
        let mut options = ConnectOptions::new(format!("sqlite://{}", path.display()));
        options.sqlx_logging(false);
        options.map_sqlx_sqlite_opts(|sqlite| sqlite.busy_timeout(Duration::from_secs(5)));
        Database::connect(options)
            .await
            .expect("open file-backed traces SQLite fixture")
    }

    async fn file_scoped_marker_fixture() -> (
        TempDir,
        DatabaseConnection,
        DatabaseConnection,
        CaptureScope,
        TracesInflightMarker,
    ) {
        let directory = tempfile::tempdir().expect("create file-backed traces SQLite fixture");
        let path = directory.path().join("traces.sqlite");
        std::fs::File::create(&path).expect("create file-backed traces SQLite database");
        let writer = connect_file_sqlite(&path).await;
        let reader = connect_file_sqlite(&path).await;
        let backend = writer.get_database_backend();
        for connection in [&writer, &reader] {
            connection
                .execute_raw(Statement::from_string(
                    backend,
                    "PRAGMA journal_mode = DELETE".to_string(),
                ))
                .await
                .expect("force rollback-journal mode for commit-lock regression");
        }
        let (scope, marker) = seed_scoped_marker_fixture(&writer).await;
        (directory, writer, reader, scope, marker)
    }

    async fn marker_value(
        conn: &DatabaseConnection,
        marker: &TracesInflightMarker,
    ) -> Option<String> {
        crate::internal::metadata::MetadataKv::get_with_conn(
            conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            &marker.session_id,
            &marker.attempt_id,
        )
        .await
        .expect("read scoped traces marker")
        .map(|entry| entry.value)
    }

    /// A trigger expires the workspace lease *after* each marker DML but
    /// before the new final lease DML. This is the interleaving a read-only
    /// preflight misses: every affected transaction must roll back, leaving
    /// neither a new marker nor a replacement/cleared marker behind.
    #[tokio::test]
    async fn scoped_marker_mutations_roll_back_when_final_fence_expires_after_dml() {
        let (conn, scope, marker) = scoped_marker_fixture().await;
        let backend = conn.get_database_backend();

        conn.execute_raw(Statement::from_string(
            backend,
            "CREATE TRIGGER expire_scope_after_marker_insert
             AFTER INSERT ON metadata_kv
             WHEN NEW.scope = 'agent_traces_inflight'
             BEGIN
                UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'scope-marker-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install registration expiry trigger");
        let registration = register_traces_write_attempt_with_capture_scope(
            &conn,
            Some(&scope),
            &marker,
            &[],
            None,
            None,
        )
        .await;
        assert!(
            registration.is_err(),
            "the final workspace fence must reject expiry after marker registration"
        );
        assert!(
            marker_value(&conn, &marker).await.is_none(),
            "a failed final fence must roll back the newly registered marker"
        );
        conn.execute_raw(Statement::from_string(
            backend,
            "DROP TRIGGER expire_scope_after_marker_insert".to_string(),
        ))
        .await
        .expect("remove registration expiry trigger");

        write_traces_inflight_marker(&conn, &marker)
            .await
            .expect("seed ordinary scoped marker");
        let original_marker = marker_value(&conn, &marker)
            .await
            .expect("seed marker remains readable");

        conn.execute_raw(Statement::from_string(
            backend,
            "CREATE TRIGGER expire_scope_after_marker_update
             AFTER UPDATE OF value ON metadata_kv
             WHEN NEW.scope = 'agent_traces_inflight'
             BEGIN
                UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'scope-marker-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install marker-update expiry trigger");
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .expect("begin scoped marker update");
        let mut refreshed = marker.clone();
        refreshed.ttl_ms = refreshed.ttl_ms.saturating_add(1);
        let generation = refreshed
            .generation
            .clone()
            .expect("fresh marker has a writer generation");
        assert!(
            update_traces_inflight_marker_if_generation_with_capture_scope(
                &txn,
                Some(&scope),
                &refreshed,
                &generation,
            )
            .await
            .is_err(),
            "the final workspace fence must reject expiry after marker refresh"
        );
        txn.rollback()
            .await
            .expect("roll back failed marker refresh");
        assert_eq!(
            marker_value(&conn, &marker).await,
            Some(original_marker.clone()),
            "failed refresh must leave the prior marker byte-identical"
        );
        conn.execute_raw(Statement::from_string(
            backend,
            "DROP TRIGGER expire_scope_after_marker_update".to_string(),
        ))
        .await
        .expect("remove marker-update expiry trigger");

        conn.execute_raw(Statement::from_string(
            backend,
            "CREATE TRIGGER expire_scope_after_marker_delete
             AFTER DELETE ON metadata_kv
             WHEN OLD.scope = 'agent_traces_inflight'
             BEGIN
                UPDATE workspace_record SET lease_expires_at = 0
                 WHERE workspace_id = 'scope-marker-workspace';
             END"
            .to_string(),
        ))
        .await
        .expect("install marker-delete expiry trigger");
        assert!(
            clear_non_cleanup_traces_inflight_marker_with_capture_scope(
                &conn,
                Some(&scope),
                &marker.session_id,
                &marker.attempt_id,
                &generation,
            )
            .await
            .is_err(),
            "the final workspace fence must reject expiry after marker retirement"
        );
        assert_eq!(
            marker_value(&conn, &marker).await,
            Some(original_marker),
            "failed marker retirement must preserve the exact recovery evidence"
        );
    }

    #[tokio::test]
    async fn deadline_aware_marker_mutations_roll_back_on_final_sqlite_deadline() {
        let (conn, scope, marker) = scoped_marker_fixture().await;
        // Keep the process-side deadline live while making the immutable
        // SQLite authorization deadline expired. This proves the final DML,
        // not a late `Instant` check, rejects both marker mutations.
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_secs(1),
            0,
        );
        let registration = register_traces_write_attempt_with_capture_scope_until(
            &conn,
            Some(&scope),
            &marker,
            &[],
            None,
            None,
            deadline,
        )
        .await;
        let error = registration
            .expect_err("expired final SQLite authorization must reject marker registration");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<TracesMarkerDeadlineExceeded>()),
            "unexpected final registration authorization error: {error:#}"
        );
        assert!(
            marker_value(&conn, &marker).await.is_none(),
            "the final authorization rejection must roll back the new marker"
        );

        write_traces_inflight_marker(&conn, &marker)
            .await
            .expect("seed ordinary marker for deadline-aware retirement");
        let original_marker = marker_value(&conn, &marker)
            .await
            .expect("seed marker remains readable");
        let generation = marker
            .generation
            .as_deref()
            .expect("fresh marker has a writer generation");
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + std::time::Duration::from_secs(1),
            0,
        );
        let retirement = clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
            &conn,
            Some(&scope),
            &marker.session_id,
            &marker.attempt_id,
            generation,
            deadline,
        )
        .await;
        let error = retirement
            .expect_err("expired final SQLite authorization must reject marker retirement");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<TracesMarkerDeadlineExceeded>()),
            "unexpected final retirement authorization error: {error:#}"
        );
        assert_eq!(
            marker_value(&conn, &marker).await,
            Some(original_marker),
            "the final authorization rejection must preserve recovery evidence"
        );
    }

    #[tokio::test]
    async fn deadline_crossed_before_marker_dml_publishes_no_mutation() {
        let (conn, scope, marker) = scoped_marker_fixture().await;
        let registration = with_traces_marker_before_dml_delay(
            Duration::from_millis(80),
            register_traces_write_attempt_with_capture_scope_until(
                &conn,
                Some(&scope),
                &marker,
                &[],
                None,
                None,
                CaptureCommitDeadline::from_budget(Duration::from_millis(20))
                    .expect("establish short marker registration deadline"),
            ),
        )
        .await;
        let error = registration.expect_err("the pre-DML deadline must reject registration");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<TracesMarkerDeadlineExceeded>()),
            "unexpected pre-DML registration error: {error:#}"
        );
        assert!(
            marker_value(&conn, &marker).await.is_none(),
            "a pre-DML deadline must not publish a marker"
        );

        write_traces_inflight_marker(&conn, &marker)
            .await
            .expect("seed ordinary marker for pre-DML retirement regression");
        let original_marker = marker_value(&conn, &marker)
            .await
            .expect("seed marker remains readable");
        let generation = marker
            .generation
            .as_deref()
            .expect("fresh marker has a writer generation");
        let retirement = with_traces_marker_before_dml_delay(
            Duration::from_millis(80),
            clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
                &conn,
                Some(&scope),
                &marker.session_id,
                &marker.attempt_id,
                generation,
                CaptureCommitDeadline::from_budget(Duration::from_millis(20))
                    .expect("establish short marker retirement deadline"),
            ),
        )
        .await;
        let error = retirement.expect_err("the pre-DML deadline must reject retirement");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<TracesMarkerDeadlineExceeded>()),
            "unexpected pre-DML retirement error: {error:#}"
        );
        assert_eq!(
            marker_value(&conn, &marker).await,
            Some(original_marker),
            "a pre-DML deadline must preserve the existing marker"
        );
    }

    #[tokio::test]
    async fn deadline_bounds_file_sqlite_marker_writer_acquisition_without_delayed_mutation() {
        let (_directory, writer, locker, scope, marker) = file_scoped_marker_fixture().await;
        let lock = crate::internal::db::begin_write_transaction(&locker)
            .await
            .expect("acquire traces marker writer lock");
        let registration = tokio::time::timeout(
            Duration::from_secs(2),
            register_traces_write_attempt_with_capture_scope_until(
                &writer,
                Some(&scope),
                &marker,
                &[],
                None,
                None,
                CaptureCommitDeadline::from_budget(Duration::from_millis(30))
                    .expect("establish short marker registration deadline"),
            ),
        )
        .await
        .expect("marker writer acquisition must honor its deadline");
        lock.rollback()
            .await
            .expect("release traces marker writer lock");
        let error = registration.expect_err("locked marker registration must time out");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<TracesMarkerDeadlineExceeded>()),
            "unexpected locked marker registration error: {error:#}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert!(
            marker_value(&writer, &marker).await.is_none(),
            "a cancelled marker writer acquisition must not publish a delayed marker"
        );

        write_traces_inflight_marker(&writer, &marker)
            .await
            .expect("seed ordinary marker for locked retirement regression");
        let original_marker = marker_value(&writer, &marker)
            .await
            .expect("seed ordinary marker remains readable");
        let generation = marker
            .generation
            .as_deref()
            .expect("fresh marker has a writer generation");
        let lock = crate::internal::db::begin_write_transaction(&locker)
            .await
            .expect("reacquire traces marker writer lock");
        let retirement = tokio::time::timeout(
            Duration::from_secs(2),
            clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
                &writer,
                Some(&scope),
                &marker.session_id,
                &marker.attempt_id,
                generation,
                CaptureCommitDeadline::from_budget(Duration::from_millis(30))
                    .expect("establish short marker retirement deadline"),
            ),
        )
        .await
        .expect("marker retirement writer acquisition must honor its deadline");
        lock.rollback()
            .await
            .expect("release traces marker retirement lock");
        let error = retirement.expect_err("locked marker retirement must time out");
        assert!(
            error
                .chain()
                .any(|cause| cause.is::<TracesMarkerDeadlineExceeded>()),
            "unexpected locked marker retirement error: {error:#}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            marker_value(&writer, &marker).await,
            Some(original_marker),
            "a cancelled marker retirement acquisition must preserve its recovery evidence"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dispatched_traces_marker_commit_ack_after_deadline_is_success_and_durable() {
        let (_directory, writer, reader, scope, marker) = file_scoped_marker_fixture().await;
        // A rollback-journal reader holds SHARED until this explicit rollback.
        // SQLite permits the writer's marker DML and final authorization under
        // that lock, but its COMMIT cannot acknowledge until the reader exits.
        let reader_marker = TracesInflightMarker::new(
            "scope-marker-session",
            "commit-dispatch-reader-lock",
            chrono::Utc::now().timestamp_millis(),
        );
        write_traces_inflight_marker(&writer, &reader_marker)
            .await
            .expect("seed row for rollback-journal reader lock");
        let reader_txn = reader
            .begin()
            .await
            .expect("begin rollback-journal reader transaction");
        assert!(
            reader_txn
                .query_one_raw(Statement::from_string(
                    reader.get_database_backend(),
                    "SELECT value FROM metadata_kv LIMIT 1".to_string(),
                ))
                .await
                .expect("acquire rollback-journal reader lock")
                .is_some(),
            "the reader transaction must observe a row before it can hold SHARED"
        );

        let budget = Duration::from_secs(1);
        let deadline = CaptureCommitDeadline::from_established_pair(
            Instant::now() + budget,
            chrono::Utc::now()
                .timestamp_millis()
                .saturating_add(i64::try_from(budget.as_millis()).expect("deadline fits i64")),
        );
        let (pause, dispatched, release) = TracesCommitDispatchPause::new();
        let writer_connection = writer.clone();
        let writer_scope = scope.clone();
        let writer_marker = marker.clone();
        let mut registration = tokio::spawn(async move {
            with_traces_commit_dispatch_pause(
                pause,
                register_traces_write_attempt_with_capture_scope_until(
                    &writer_connection,
                    Some(&writer_scope),
                    &writer_marker,
                    &[],
                    None,
                    None,
                    deadline,
                ),
            )
            .await
        });

        assert!(
            tokio::time::timeout(Duration::from_secs(2), dispatched)
                .await
                .expect("COMMIT must be dispatched before the test timeout")
                .expect("commit-dispatch seam must remain alive"),
            "the first poll must dispatch COMMIT and remain pending on the reader lock"
        );
        tokio::time::sleep_until(tokio::time::Instant::from_std(
            deadline.monotonic() + Duration::from_millis(25),
        ))
        .await;
        assert!(
            Instant::now() >= deadline.monotonic(),
            "the capture deadline must elapse while the COMMIT acknowledgement is blocked"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut registration)
                .await
                .is_err(),
            "the dispatched COMMIT acknowledgement must still wait for the reader lock"
        );

        reader_txn
            .rollback()
            .await
            .expect("release rollback-journal reader lock");
        release
            .send(())
            .expect("allow the already-dispatched COMMIT to await its acknowledgement");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), registration)
                .await
                .expect("COMMIT acknowledgement must arrive after reader release")
                .expect("writer task must not panic")
                .expect("a post-deadline COMMIT acknowledgement must remain successful"),
            TracesWriteAttemptRegistration::Registered,
            "the caller must not receive a deadline error after SQLite accepted final authorization"
        );
        assert!(
            marker_value(&writer, &marker).await.is_some(),
            "the successful post-deadline COMMIT acknowledgement must leave durable marker state"
        );
    }

    #[tokio::test]
    async fn live_marker_with_owned_objects_is_still_an_inflight_writer() {
        let (conn, scope, marker) = scoped_marker_fixture().await;
        let mut live_marker = marker.clone();
        live_marker.created_oids.push(
            crate::internal::object_format::digest(
                git_internal::hash::get_hash_kind(),
                b"live-traces-marker-owned-object",
            )
            .to_string(),
        );
        write_traces_inflight_marker(&conn, &live_marker)
            .await
            .expect("seed live writer marker with owned object evidence");

        assert_eq!(
            register_traces_write_attempt_with_capture_scope(
                &conn,
                Some(&scope),
                &marker,
                &[],
                None,
                None,
            )
            .await
            .expect("duplicate registration observes the live writer"),
            TracesWriteAttemptRegistration::AlreadyInFlightSameGeneration,
            "owned-object evidence belongs to the active writer and must not be misclassified as durable cleanup"
        );
    }

    #[tokio::test]
    async fn future_dated_marker_is_rejected_before_inflight_classification() {
        let (conn, scope, marker) = scoped_marker_fixture().await;
        let mut future_marker = marker.clone();
        future_marker.started_at_ms = chrono::Utc::now()
            .timestamp_millis()
            .saturating_add(TRACES_INFLIGHT_FUTURE_SKEW_MS)
            .saturating_add(60_000);
        write_traces_inflight_marker(&conn, &future_marker)
            .await
            .expect("seed corrupt future-dated writer marker");
        let original_marker = marker_value(&conn, &marker)
            .await
            .expect("seed corrupt writer marker remains readable");

        let error = register_traces_write_attempt_with_capture_scope(
            &conn,
            Some(&scope),
            &marker,
            &[],
            None,
            None,
        )
        .await
        .expect_err("a future-dated marker must not be reported as an in-flight writer");
        assert!(
            error.to_string().contains("implausible timestamp"),
            "unexpected registration error: {error:#}"
        );
        assert!(
            error.to_string().contains("libra agent doctor"),
            "corrupt marker errors must direct the operator to repair: {error:#}"
        );
        assert_eq!(
            marker_value(&conn, &marker).await,
            Some(original_marker),
            "the rejected registration must preserve the corrupt marker for doctor"
        );
    }

    #[test]
    fn rejected_cleanup_decoder_bounds_only_the_short_recovery_path() {
        let marker = TracesInflightMarker::new("legacy-session", "legacy-attempt", 0);
        let mut oversized_json = serde_json::to_value(&marker).expect("serialize marker value");
        oversized_json["legacy_extension"] = serde_json::Value::String(
            "x".repeat(TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES),
        );
        let oversized_json =
            serde_json::to_string(&oversized_json).expect("serialize extended legacy marker");
        assert!(
            oversized_json.len() > TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES,
            "fixture must exceed the short recovery input bound"
        );

        // Generic decoding deliberately remains compatible with an older
        // marker extension; only the fixed-grace recovery path defers it.
        decode_and_validate_traces_inflight_marker(
            &oversized_json,
            "legacy-session",
            "legacy-attempt",
        )
        .expect("ordinary marker consumers retain legacy compatibility");
        let error = decode_and_validate_traces_inflight_marker_for_rejected_cleanup(
            &oversized_json,
            "legacy-session",
            "legacy-attempt",
        )
        .expect_err("short recovery must not parse an oversized marker");
        assert!(
            error
                .downcast_ref::<TracesInflightRejectedCleanupMarkerBoundExceeded>()
                .is_some(),
            "recovery bound must remain distinguishable for deferred cleanup: {error:#}"
        );
        assert!(
            !format!("{error:#}").contains("legacy_extension"),
            "bound error must not echo untrusted marker content"
        );

        let valid_oid = crate::internal::object_format::digest(
            git_internal::hash::get_hash_kind(),
            b"legacy-marker-ownership",
        )
        .to_string();
        let mut too_many_oids = marker;
        too_many_oids.oids =
            vec![valid_oid; TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_OID_ENTRIES + 1];
        let too_many_oids =
            serde_json::to_string(&too_many_oids).expect("serialize marker ownership vector");
        assert!(
            too_many_oids.len() <= TRACES_INFLIGHT_REJECTED_CLEANUP_MARKER_MAX_BYTES,
            "OID-count fixture must exercise the vector bound rather than the byte bound"
        );
        decode_and_validate_traces_inflight_marker(
            &too_many_oids,
            "legacy-session",
            "legacy-attempt",
        )
        .expect("ordinary decoder remains compatible with a legacy ownership vector");
        let error = decode_and_validate_traces_inflight_marker_for_rejected_cleanup(
            &too_many_oids,
            "legacy-session",
            "legacy-attempt",
        )
        .expect_err("short recovery must bound marker ownership vectors");
        assert!(
            error
                .downcast_ref::<TracesInflightRejectedCleanupMarkerBoundExceeded>()
                .is_some(),
            "OID vector bound must remain distinguishable for deferred cleanup: {error:#}"
        );
        assert!(
            format!("{error:#}").contains("object-id entries"),
            "OID-count failure must identify the bounded field class: {error:#}"
        );
    }
}
