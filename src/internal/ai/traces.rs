//! `refs/libra/traces` persistence API — plan-20260920 RC-02.
//!
//! This module is the single source of truth for traces commit / inflight /
//! prune / catalog-rebuild types. [`crate::internal::ai::history::HistoryManager`]
//! still owns the object-store CAS implementation and `pub use`s these items
//! until remaining Code-side callers are deleted.

use std::{str::FromStr, time::Instant};

use anyhow::{Context, Result, anyhow, bail};
use git_internal::hash::ObjectHash;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, Value};

use crate::internal::ai::observed_agents::RedactedBytes;

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
    /// Absolute command deadline for historical imports. Live/export writers
    /// pass `None`; import object construction and CAS are cancelled when the
    /// deadline is reached.
    pub deadline: Option<Instant>,
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

/// Resolve the effective E5 chunking threshold.
///
/// `LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD` (bytes, test-only — mirrors the
/// `LIBRA_TEST_*` convention) overrides the frozen 50 MiB constant so tests
/// can exercise the chunking path without allocating 50 MiB. Invalid or
/// zero values fall back to the constant rather than erroring: a stray env
/// var must never turn the writer into a per-byte chunker or a hard error.
pub fn transcript_chunk_threshold() -> usize {
    std::env::var("LIBRA_TEST_TRANSCRIPT_CHUNK_THRESHOLD")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&threshold| threshold > 0)
        .unwrap_or(TRANSCRIPT_CHUNK_THRESHOLD_BYTES)
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
        "schema_version": schema_version,
    })
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
        "schema_version": 1,
    });
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
                            "byte_len": part.byte_len,
                        })
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
            "coverage": CHECKPOINT_CONTENT_HASH_COVERAGE,
        },
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
                1,
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
            ),
        },
    });
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

fn validate_traces_inflight_marker(
    marker: &TracesInflightMarker,
    expected_session_id: &str,
    expected_attempt_id: &str,
) -> Result<()> {
    if marker.session_id != expected_session_id || marker.attempt_id != expected_attempt_id {
        bail!(
            "traces in-flight marker identity does not match its metadata row (row {expected_session_id}/{expected_attempt_id}, value {}/{}); inspect it with `libra agent doctor` before retrying",
            marker.session_id,
            marker.attempt_id
        );
    }
    if marker.schema_version >= 3 {
        let generation = marker.generation.as_deref().ok_or_else(|| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} schema {} has no writer generation; inspect it with `libra agent doctor` before retrying",
                marker.schema_version
            )
        })?;
        uuid::Uuid::parse_str(generation).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} has invalid writer generation '{generation}': {error}; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    for oid in &marker.oids {
        ObjectHash::from_str(oid).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} contains invalid object id '{oid}': {error}; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    for oid in &marker.created_oids {
        ObjectHash::from_str(oid).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} contains invalid created object id '{oid}': {error}; inspect it with `libra agent doctor` before retrying"
            )
        })?;
    }
    if let Some(commit) = marker.commit.as_deref() {
        ObjectHash::from_str(commit).map_err(|error| {
            anyhow!(
                "traces in-flight marker {expected_session_id}/{expected_attempt_id} contains invalid commit id '{commit}': {error}; inspect it with `libra agent doctor` before retrying"
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
    let marker = serde_json::from_str::<TracesInflightMarker>(value).with_context(|| {
        format!(
            "decode traces in-flight marker for session {expected_session_id} attempt {expected_attempt_id}; inspect it with `libra agent doctor` before retrying"
        )
    })?;
    validate_traces_inflight_marker(&marker, expected_session_id, expected_attempt_id)?;
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

/// Establish one writer attempt under the same SQLite writer lock used by
/// erasure. The session/tombstone barrier and any coverage fences are checked
/// in the marker transaction; a failed marker write aborts the checkpoint
/// before loose objects or object-index tasks can exist.
pub async fn register_traces_write_attempt(
    conn: &DatabaseConnection,
    marker: &TracesInflightMarker,
    coverage_fences: &[TracesCoverageFence<'_>],
) -> Result<()> {
    let txn = crate::internal::db::begin_write_transaction(conn)
        .await
        .context("begin traces writer-attempt registration")?;
    let writable = txn
        .query_one_raw(Statement::from_sql_and_values(
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
        .context("verify traces writer session/tombstone barrier")?;
    if writable.is_none() {
        txn.rollback().await.ok();
        bail!("agent session was erased or is unavailable for checkpoint writing");
    }
    for fence in coverage_fences {
        let owned = txn
            .query_one_raw(Statement::from_sql_and_values(
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
            .context("verify traces writer coverage fence")?;
        if owned.is_none() {
            txn.rollback().await.ok();
            bail!(
                "coverage fence for turn '{}' is no longer owned; checkpoint write aborted",
                fence.logical_turn_key
            );
        }
    }
    write_traces_inflight_marker(&txn, marker)
        .await
        .context("register traces writer marker")?;
    txn.commit()
        .await
        .context("commit traces writer-attempt registration")?;
    Ok(())
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
                        "traces in-flight marker for session {} attempt {} carries a \
                         future-dated started_at_ms ({} vs now {now_ms}) — the row is \
                         corrupt; inspect it with `libra agent doctor` before destructive \
                         maintenance",
                        marker.session_id,
                        marker.attempt_id,
                        marker.started_at_ms
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
