//! Authorized, bounded transcript snapshots for Agent Capture.
//!
//! Provider-native bytes may exist only while this module reads one
//! [`TranscriptSource`]. The resulting [`CaptureSnapshot`] exposes redacted
//! content and safe provenance only; it intentionally has no raw-byte
//! accessor, `Serialize` implementation, or content-bearing `Debug` output.
//! This makes the snapshot service the one common boundary for live capture,
//! historical import, and later capture-push consumers.

use std::{fmt, time::Instant};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) use crate::internal::ai::capture::extraction::DeadlineExtractionResult;
use crate::internal::ai::{
    authorized_read::{
        LiveClaudeSourceRead, SOURCE_IDENTITY_NOT_RETAINED, capture_redacted_output_cap,
        capture_redaction_working_set_cap, provider_file_identity, read_live_claude_source_until,
    },
    observed_agents::{
        AgentKind, AgentSessionCtx, ObservedAgent, RedactedBytes, RedactionReport, Redactor,
        TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptReadError, TranscriptSource,
        TranscriptSourceResolution, resolve_live_transcript_source_until,
        transcript_source::InMemoryTranscriptOrigin,
    },
};

/// Private argv and byte caps for the redacted-only extraction worker. These
/// are re-exported through the snapshot service so the binary dispatcher does
/// not reach into hook-runtime implementation details.
pub const CAPTURE_EXTRACTION_HELPER_ARG: &str =
    crate::internal::ai::capture::extraction::CAPTURE_EXTRACTION_HELPER_ARG;
pub const CAPTURE_EXTRACTION_HELPER_INPUT_CAP: u64 =
    crate::internal::ai::capture::extraction::CAPTURE_EXTRACTION_HELPER_INPUT_CAP;
pub const CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP: u64 =
    crate::internal::ai::capture::extraction::CAPTURE_EXTRACTION_HELPER_OUTPUT_CAP;

/// Execute the redacted-only extraction worker before normal CLI startup.
#[doc(hidden)]
pub fn run_capture_extraction_helper(input: Vec<u8>) -> Vec<u8> {
    crate::internal::ai::capture::extraction::run_capture_extraction_helper(input)
}

/// A source-read policy supplied by a capture coordinator.
///
/// [`CaptureSnapshotService::capture_live_until`] checks an absolute deadline
/// before and after it resolves Claude's provider-root authorization into one
/// held descriptor. With a deadline, the resolver delegates preparer/flush
/// work to the helper; the parent only rewinds and transfers the descriptor.
/// The helper owns the bounded read, redaction, and digest under the remaining
/// cooperative deadline. The descriptor is inherited as stdin; the parent
/// sends no path or provider identity and receives only a bounded redacted
/// frame plus safe report/provenance.
///
/// Host filesystem calls may still be subject to platform scheduling and I/O
/// behavior; this is a cooperative operation deadline, not a host-wide hard
/// real-time guarantee.
#[derive(Clone, Copy, Debug)]
pub struct CaptureSnapshotPolicy {
    deadline: Option<Instant>,
    max_bytes: u64,
}

impl CaptureSnapshotPolicy {
    /// Use the repository-wide transcript hard cap with an optional absolute
    /// capture deadline.
    pub fn with_deadline(deadline: Option<Instant>) -> Self {
        Self {
            deadline,
            max_bytes: TRANSCRIPT_READ_HARD_CAP_BYTES,
        }
    }

    /// The absolute deadline forwarded to source preparation.
    pub fn deadline(self) -> Option<Instant> {
        self.deadline
    }

    #[cfg(test)]
    fn with_max_bytes(deadline: Option<Instant>, max_bytes: u64) -> Self {
        Self {
            deadline,
            max_bytes,
        }
    }
}

impl Default for CaptureSnapshotPolicy {
    fn default() -> Self {
        Self::with_deadline(None)
    }
}

/// Whether the snapshot contains a complete, authorized redacted transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSnapshotCompleteness {
    Complete,
    Partial,
}

/// Safe, typed reason why a snapshot is partial.
///
/// These variants deliberately contain no filesystem path, raw error text, or
/// provider payload. A checkpoint may persist the enum's wire spelling as a
/// diagnostic without turning a failed read into an apparent complete empty
/// snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSnapshotPartialReason {
    SourceAbsent,
    SourceUntrusted,
    SourceAuthorizationMismatch,
    SourceCommitmentUnavailable,
    SourceOversize,
    SourceReadError,
    SourceEmpty,
    DeadlineExceeded,
}

/// Safe source classification retained with a snapshot.
///
/// `identity` is the fixed `not_retained:v1` sentinel.  It deliberately does
/// not commit a provider-relative source key, session id, or an unkeyed public
/// digest of either: those values would enable offline enumeration by a
/// metadata reader. The legacy wire name `digest_sha256` holds a transient
/// SHA-256 only inside the restricted snapshot boundary; before durable use it
/// is replaced with a repository-keyed source HMAC over the redacted bytes.
#[derive(Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSnapshotSource {
    pub kind: CaptureSnapshotSourceKind,
    pub identity: String,
    pub digest_sha256: Option<String>,
    pub byte_len: Option<u64>,
}

impl fmt::Debug for CaptureSnapshotSource {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureSnapshotSource")
            .field("kind", &self.kind)
            .field("identity", &"<opaque>")
            .field(
                "digest_sha256",
                &self.digest_sha256.as_ref().map(|_| "<redacted>"),
            )
            .field("byte_len", &self.byte_len)
            .finish()
    }
}

/// The authorization shape of a snapshot source.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CaptureSnapshotSourceKind {
    ProviderFile,
    TrustedExport,
    DiscoveredSubagent,
}

/// Serializable, redacted-only projection suitable for checkpoint metadata or
/// diagnostics. It intentionally omits the transcript bytes themselves.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CaptureSnapshotProjection {
    pub completeness: CaptureSnapshotCompleteness,
    pub partial_reason: Option<CaptureSnapshotPartialReason>,
    pub source: Option<CaptureSnapshotSource>,
    pub transcript_redacted_bytes: usize,
    pub redaction_match_count: usize,
    pub redaction_bytes_scanned: usize,
    pub redaction_bytes_redacted: usize,
}

impl CaptureSnapshotProjection {
    /// Recover the helper-produced checksum only while this projection remains
    /// transient. Callers must replace it with a repository-scoped commitment
    /// before serializing the projection into catalog or checkpoint metadata.
    pub(crate) fn redacted_digest_preimage(&self) -> Option<[u8; 32]> {
        redacted_digest_preimage(self.source.as_ref())
    }

    /// Replace the helper's transient checksum with a durable source
    /// commitment. This is intentionally available on the projection because
    /// the descriptor helper returns a projection, not a `CaptureSnapshot`.
    pub(crate) fn bind_source_commitment(&mut self, commitment: String) -> bool {
        bind_source_commitment(&mut self.source, commitment)
    }

    /// A projection may cross a durable checkpoint/catalog boundary only with
    /// no source digest or with the repository-keyed V2 commitment. The
    /// helper's transient `sha256:` preimage is valid only inside the
    /// immediately consuming process.
    pub(crate) fn source_commitment_is_durable_or_absent(&self) -> bool {
        source_commitment_is_durable_or_absent(self.source.as_ref())
    }

    /// Return whether this projection carries a repository-keyed source
    /// commitment. Complete source snapshots require this before persistence.
    pub(crate) fn has_durable_source_commitment(&self) -> bool {
        has_durable_source_commitment(self.source.as_ref())
    }
}

/// A captured transcript after its raw source was bounded and redacted.
///
/// Raw bytes are deliberately local to [`CaptureSnapshotService`]. Consumers
/// can read only [`RedactedBytes`] and a safe projection. Do not add a raw
/// accessor: normalizers/extractors must consume the redacted transcript or
/// move into this service.
pub struct CaptureSnapshot {
    completeness: CaptureSnapshotCompleteness,
    partial_reason: Option<CaptureSnapshotPartialReason>,
    source: Option<CaptureSnapshotSource>,
    transcript_redacted: Option<RedactedBytes>,
    redaction_report: RedactionReport,
}

impl fmt::Debug for CaptureSnapshot {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CaptureSnapshot")
            .field("completeness", &self.completeness)
            .field("partial_reason", &self.partial_reason)
            .field("source", &self.source)
            .field("transcript_present", &self.transcript_redacted.is_some())
            .field(
                "transcript_redacted_bytes",
                &self
                    .transcript_redacted
                    .as_ref()
                    .map_or(0usize, RedactedBytes::len),
            )
            .field(
                "redaction_match_count",
                &self.redaction_report.match_count(),
            )
            .finish()
    }
}

impl CaptureSnapshot {
    fn complete(
        source: CaptureSnapshotSource,
        transcript_redacted: RedactedBytes,
        redaction_report: RedactionReport,
    ) -> Self {
        Self {
            completeness: CaptureSnapshotCompleteness::Complete,
            partial_reason: None,
            source: Some(source),
            transcript_redacted: Some(transcript_redacted),
            redaction_report,
        }
    }

    fn partial(
        reason: CaptureSnapshotPartialReason,
        source: Option<CaptureSnapshotSource>,
    ) -> Self {
        Self {
            completeness: CaptureSnapshotCompleteness::Partial,
            partial_reason: Some(reason),
            source,
            transcript_redacted: None,
            redaction_report: RedactionReport::default(),
        }
    }

    /// Whether an authorized source was read, redacted, and completed within
    /// the supplied policy deadline.
    pub fn completeness(&self) -> CaptureSnapshotCompleteness {
        self.completeness
    }

    /// The explicit safe reason for a partial result, if any.
    pub fn partial_reason(&self) -> Option<CaptureSnapshotPartialReason> {
        self.partial_reason
    }

    /// Already-redacted transcript bytes. `None` means partial: callers must
    /// use an independently redacted fallback and record the safe reason.
    pub fn transcript(&self) -> Option<&RedactedBytes> {
        self.transcript_redacted.as_ref()
    }

    /// Consume the snapshot and return its already-redacted transcript. This
    /// is intentionally the only consuming content accessor; it cannot expose
    /// the raw source bytes that were used during capture.
    pub fn into_redacted_transcript(self) -> Option<RedactedBytes> {
        self.transcript_redacted
    }

    /// Rule-hit report for the redaction pass that produced
    /// [`Self::transcript`]. It contains only rule identifiers and offsets.
    pub fn redaction_report(&self) -> &RedactionReport {
        &self.redaction_report
    }

    /// Redacted-only metadata whose transient source checksum must be bound
    /// to a repository-keyed commitment before durable projection.
    pub fn safe_projection(&self) -> CaptureSnapshotProjection {
        CaptureSnapshotProjection {
            completeness: self.completeness,
            partial_reason: self.partial_reason,
            source: self.source.clone(),
            transcript_redacted_bytes: self
                .transcript_redacted
                .as_ref()
                .map_or(0usize, RedactedBytes::len),
            redaction_match_count: self.redaction_report.match_count(),
            redaction_bytes_scanned: self.redaction_report.bytes_scanned,
            redaction_bytes_redacted: self.redaction_report.bytes_redacted,
        }
    }

    /// Return the helper-produced redacted-content digest only as a fixed
    /// transient preimage for the repository-scoped commitment step.  The
    /// `sha256:` spelling must never reach a durable live projection: it
    /// would let a metadata reader test guessed redacted transcripts offline.
    pub(crate) fn redacted_digest_preimage(&self) -> Option<[u8; 32]> {
        redacted_digest_preimage(self.source.as_ref())
    }

    /// Replace the transient redacted checksum with the repository-keyed
    /// source commitment before this snapshot becomes checkpoint metadata or
    /// a terminal-finalizer fence.
    pub(crate) fn bind_source_commitment(&mut self, commitment: String) -> bool {
        bind_source_commitment(&mut self.source, commitment)
    }

    /// Remove a helper-only digest without changing the snapshot's
    /// completeness or partial reason. This is used when a deadline has
    /// already made a capture partial: changing its reason would hide the
    /// actual failure, while retaining the checksum would leak it durably.
    pub(crate) fn clear_non_durable_source_digest(&mut self) -> bool {
        clear_non_durable_source_digest(&mut self.source)
    }

    /// Do not persist a complete source snapshot when its scoped commitment
    /// cannot be minted.  The already-redacted bytes are intentionally
    /// discarded too, so callers take their explicit redacted event fallback
    /// rather than creating an unauthenticated source projection.
    pub(crate) fn downgrade_for_commitment_failure(
        &mut self,
        reason: CaptureSnapshotPartialReason,
    ) {
        self.completeness = CaptureSnapshotCompleteness::Partial;
        self.partial_reason = Some(reason);
        if let Some(source) = self.source.as_mut() {
            source.digest_sha256 = None;
        }
        self.transcript_redacted = None;
        self.redaction_report = RedactionReport::default();
    }
}

fn redacted_digest_preimage(source: Option<&CaptureSnapshotSource>) -> Option<[u8; 32]> {
    let digest = source?.digest_sha256.as_deref()?;
    let hex = digest.strip_prefix("sha256:")?;
    let mut preimage = [0_u8; 32];
    hex::decode_to_slice(hex, &mut preimage).ok()?;
    Some(preimage)
}

fn bind_source_commitment(source: &mut Option<CaptureSnapshotSource>, commitment: String) -> bool {
    let Some(source) = source.as_mut() else {
        return false;
    };
    if !is_durable_source_commitment(&commitment) {
        return false;
    }
    source.digest_sha256 = Some(commitment);
    true
}

fn source_commitment_is_durable_or_absent(source: Option<&CaptureSnapshotSource>) -> bool {
    match source.and_then(|source| source.digest_sha256.as_deref()) {
        Some(commitment) => is_durable_source_commitment(commitment),
        None => true,
    }
}

fn has_durable_source_commitment(source: Option<&CaptureSnapshotSource>) -> bool {
    source
        .and_then(|source| source.digest_sha256.as_deref())
        .is_some_and(is_durable_source_commitment)
}

fn clear_non_durable_source_digest(source: &mut Option<CaptureSnapshotSource>) -> bool {
    let Some(source) = source.as_mut() else {
        return false;
    };
    if source
        .digest_sha256
        .as_deref()
        .is_some_and(|digest| !is_durable_source_commitment(digest))
    {
        source.digest_sha256 = None;
        return true;
    }
    false
}

fn is_durable_source_commitment(value: &str) -> bool {
    value.strip_prefix("source/hmac-v2/").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
    })
}

/// The single raw-byte boundary for capture snapshots.
pub struct CaptureSnapshotService;

impl CaptureSnapshotService {
    /// Build extraction metadata over already-redacted parent/child snapshots
    /// without a deadline. Historical import retains this established
    /// in-process behavior; SessionEnd uses the helper-backed method below.
    pub(crate) fn build_extraction_projection(
        agent_kind: &str,
        redacted_parent: Option<&RedactedBytes>,
        redacted_subagents: &[RedactedBytes],
        subagent_snapshot_warnings: &[String],
    ) -> serde_json::Value {
        crate::internal::ai::capture::extraction::build_extraction_projection(
            agent_kind,
            redacted_parent,
            redacted_subagents,
            subagent_snapshot_warnings,
        )
    }

    /// Run parser/extractor/projection work in the registered private helper
    /// under the same absolute capture deadline. The request contains only
    /// redacted parent bytes and fixed flags; child native sources are marked
    /// partial rather than being synchronously processed in the hook parent.
    pub(crate) async fn build_extraction_projection_until(
        agent_kind: &str,
        redacted_parent: Option<&RedactedBytes>,
        children_omitted: bool,
        deadline: Instant,
    ) -> DeadlineExtractionResult {
        crate::internal::ai::capture::extraction::build_extraction_projection_until(
            agent_kind,
            redacted_parent,
            children_omitted,
            deadline,
        )
        .await
    }

    /// Safe partial metadata for a deadline worker that did not produce a
    /// validated projection. It accepts no transcript bytes and never invokes
    /// an in-process extractor as a fallback.
    pub(crate) fn deadline_extraction_partial(deadline_exceeded: bool) -> serde_json::Value {
        crate::internal::ai::capture::extraction::deadline_extraction_partial(deadline_exceeded)
    }

    /// Resolve, authorize, bound, redact, and project one live source.
    ///
    /// The service receives only a typed session context and an adapter. It
    /// never accepts a raw path or arbitrary byte buffer from a caller.
    pub fn capture_live(
        adapter: &dyn ObservedAgent,
        context: &AgentSessionCtx,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::DeadlineExceeded, None);
        }
        // A synchronous caller cannot safely wait on an uninterruptible
        // filesystem operation. Deadline-bound live capture must use the
        // async helper-backed API below; fail closed rather than silently
        // recreating the former parent-side resolver/read path.
        if policy.deadline().is_some() {
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceReadError, None);
        }
        let resolution = match resolve_live_transcript_source_until(adapter, context, None) {
            Ok(resolution) => resolution,
            Err(_) => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceReadError,
                    None,
                );
            }
        };
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::DeadlineExceeded, None);
        }
        match resolution {
            TranscriptSourceResolution::Authorized(source) => Self::capture_authorized(
                source,
                adapter.provider_kind().as_db_str(),
                &context.session_id,
                policy,
            ),
            TranscriptSourceResolution::Absent => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceAbsent, None)
            }
            TranscriptSourceResolution::Untrusted => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceUntrusted, None)
            }
        }
    }

    /// Capture one live Claude source through the killable helper boundary.
    ///
    /// After scope-bound callers derive the provider candidate, this service
    /// pins it beneath the provider root and transfers that exact descriptor
    /// as helper stdin. The helper returns only redacted bytes and safe
    /// metadata; it never receives a locator, provider/session identity, or
    /// raw request frame. Other adapters have no descriptor-only live-source
    /// contract yet, so a deadline-bound call for them fails closed.
    pub async fn capture_live_until(
        adapter: &dyn ObservedAgent,
        context: &AgentSessionCtx,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::DeadlineExceeded, None);
        }
        let Some(deadline) = policy.deadline() else {
            return Self::capture_live(adapter, context, policy);
        };
        if adapter.provider_kind() != AgentKind::ClaudeCode {
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceReadError, None);
        }
        let source = match resolve_live_transcript_source_until(adapter, context, Some(deadline)) {
            Ok(TranscriptSourceResolution::Authorized(TranscriptSource::File { file, .. })) => {
                match file.into_rewound_inner() {
                    Ok(file) => file,
                    Err(_) => {
                        return CaptureSnapshot::partial(
                            CaptureSnapshotPartialReason::SourceReadError,
                            None,
                        );
                    }
                }
            }
            Ok(TranscriptSourceResolution::Authorized(TranscriptSource::Bytes { .. })) => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
                    None,
                );
            }
            Ok(TranscriptSourceResolution::Absent) => {
                return CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceAbsent, None);
            }
            Ok(TranscriptSourceResolution::Untrusted) => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceUntrusted,
                    None,
                );
            }
            Err(_) => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceReadError,
                    None,
                );
            }
        };
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::DeadlineExceeded, None);
        }

        match read_live_claude_source_until(source, policy.max_bytes, deadline).await {
            LiveClaudeSourceRead::Complete {
                transcript_redacted,
                redaction_report,
                raw_bytes,
                digest_sha256,
            } => Self::capture_helper_redacted_source(
                transcript_redacted,
                redaction_report,
                CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::ProviderFile,
                    identity: SOURCE_IDENTITY_NOT_RETAINED.to_string(),
                    digest_sha256: None,
                    byte_len: Some(raw_bytes),
                },
                digest_sha256,
                policy,
            ),
            LiveClaudeSourceRead::Oversize => CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceOversize,
                Some(CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::ProviderFile,
                    identity: SOURCE_IDENTITY_NOT_RETAINED.to_string(),
                    digest_sha256: None,
                    byte_len: None,
                }),
            ),
            LiveClaudeSourceRead::Absent => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceAbsent, None)
            }
            LiveClaudeSourceRead::Untrusted => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceUntrusted, None)
            }
            LiveClaudeSourceRead::DeadlineExceeded => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::DeadlineExceeded, None)
            }
            LiveClaudeSourceRead::Failed => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceReadError, None)
            }
        }
    }

    /// Capture an already-authorized source, such as a descriptor-pinned
    /// provider file, trusted exporter response, or securely discovered child
    /// source. This consumes the source exactly once and never reopens a path.
    pub fn capture_authorized(
        source: TranscriptSource,
        agent_kind: &str,
        libra_session_id: &str,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        let mut source_projection = source_projection(&source);
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::DeadlineExceeded,
                Some(source_projection),
            );
        }

        let source_len = match source.authorized_len() {
            Ok(length) => length,
            Err(_) => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceReadError,
                    Some(source_projection),
                );
            }
        };
        source_projection.byte_len = Some(source_len);
        if source_len > policy.max_bytes {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceOversize,
                Some(source_projection),
            );
        }

        let bytes = match source {
            TranscriptSource::File { mut file, .. } => file.read_bounded(policy.max_bytes),
            TranscriptSource::Bytes { bytes, auth } => {
                if auth.matches(agent_kind, libra_session_id, &bytes) {
                    Ok(bytes)
                } else {
                    return CaptureSnapshot::partial(
                        CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
                        Some(source_projection),
                    );
                }
            }
        };
        let bytes = match bytes {
            Ok(bytes) => bytes,
            Err(error) if error.downcast_ref::<TranscriptReadError>().is_some() => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceOversize,
                    Some(source_projection),
                );
            }
            Err(_) => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceReadError,
                    Some(source_projection),
                );
            }
        };
        Self::capture_authorized_bytes(bytes, source_projection, policy)
    }

    fn capture_authorized_bytes(
        bytes: Vec<u8>,
        mut source_projection: CaptureSnapshotSource,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        // A descriptor-pinned file may change between its metadata probe and
        // bounded read. Complete durable evidence must describe exactly the
        // bytes that were read and redacted, not the earlier inode length.
        let raw_bytes = u64::try_from(bytes.len()).unwrap_or(u64::MAX);
        source_projection.byte_len = Some(raw_bytes);
        if bytes.is_empty() {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceEmpty,
                Some(source_projection),
            );
        }
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::DeadlineExceeded,
                Some(source_projection),
            );
        }

        // Apply the same output and two-buffer budget as the helper-backed
        // path. A redaction placeholder may expand a short secret, but an
        // unbounded expansion must become a content-free partial here rather
        // than producing a complete snapshot the durable catalog rejects.
        let max_output_bytes =
            usize::try_from(capture_redacted_output_cap(raw_bytes)).unwrap_or(usize::MAX);
        let max_working_set_bytes =
            usize::try_from(capture_redaction_working_set_cap(raw_bytes)).unwrap_or(usize::MAX);
        let Some((transcript_redacted, redaction_report)) = Redactor::new_default()
            .redact_owned_bounded(bytes, max_output_bytes, max_working_set_bytes)
        else {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceReadError,
                Some(source_projection),
            );
        };
        // `source_projection` is durable metadata and is later bound into
        // finalizer receipts. Commit only the redacted representation: a raw
        // SHA-256 would let an observer test a guessed secret offline.
        source_projection.digest_sha256 = Some(digest_hex(transcript_redacted.bytes()));
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::DeadlineExceeded,
                Some(source_projection),
            );
        }
        CaptureSnapshot::complete(source_projection, transcript_redacted, redaction_report)
    }

    /// Complete a live helper result that was redacted in the registered
    /// helper process. `read_live_claude_source_until` validates the bounded
    /// frame, noncorrelating source classification, and report invariants before it can
    /// construct these types, so this method must never accept an arbitrary
    /// raw buffer or fall back to parent-side redaction.
    fn capture_helper_redacted_source(
        transcript_redacted: RedactedBytes,
        redaction_report: RedactionReport,
        mut source_projection: CaptureSnapshotSource,
        digest_sha256: String,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        if transcript_redacted.is_empty() {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceEmpty,
                Some(source_projection),
            );
        }
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::DeadlineExceeded,
                Some(source_projection),
            );
        }
        source_projection.digest_sha256 = Some(digest_sha256);
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::DeadlineExceeded,
                Some(source_projection),
            );
        }
        CaptureSnapshot::complete(source_projection, transcript_redacted, redaction_report)
    }
}

fn deadline_elapsed(policy: CaptureSnapshotPolicy) -> bool {
    policy
        .deadline()
        .is_some_and(|deadline| Instant::now() >= deadline)
}

fn source_projection(source: &TranscriptSource) -> CaptureSnapshotSource {
    match source {
        TranscriptSource::File { source_id, .. } => CaptureSnapshotSource {
            kind: CaptureSnapshotSourceKind::ProviderFile,
            identity: provider_file_identity(source_id),
            digest_sha256: None,
            byte_len: None,
        },
        TranscriptSource::Bytes { auth, .. } => match auth.origin() {
            InMemoryTranscriptOrigin::TrustedExport => {
                // `ExportAuthorized::content_digest` is a raw-byte integrity
                // proof used only to validate this handoff. The redacted
                // content digest is added after redaction below; provenance
                // itself is deliberately noncorrelating.
                CaptureSnapshotSource {
                    kind: CaptureSnapshotSourceKind::TrustedExport,
                    identity: SOURCE_IDENTITY_NOT_RETAINED.to_string(),
                    digest_sha256: None,
                    byte_len: None,
                }
            }
            InMemoryTranscriptOrigin::DiscoveredSubagent => CaptureSnapshotSource {
                kind: CaptureSnapshotSourceKind::DiscoveredSubagent,
                // Child discovery may retain a provider-relative key only in
                // its transient linking state. Snapshot metadata intentionally
                // carries no correlating derivative of that key.
                identity: SOURCE_IDENTITY_NOT_RETAINED.to_string(),
                digest_sha256: None,
                byte_len: None,
            },
        },
    }
}

fn digest_hex(bytes: &[u8]) -> String {
    // The tag distinguishes this redacted-snapshot checksum from a bare
    // source SHA-256. New V2 metadata may retain only this explicit,
    // redacted-content form; legacy bare values remain readable elsewhere.
    format!("sha256:{}", hex::encode(Sha256::digest(bytes)))
}

#[cfg(test)]
mod tests {
    use std::{
        path::{Path, PathBuf},
        time::Duration,
    };

    use serial_test::serial;

    use super::*;

    #[test]
    fn snapshot_source_debug_redacts_transient_digest_and_identity() {
        let secret_digest = format!("sha256:{}", "a".repeat(64));
        let source = CaptureSnapshotSource {
            kind: CaptureSnapshotSourceKind::ProviderFile,
            identity: "private-provider-path-marker".into(),
            digest_sha256: Some(secret_digest.clone()),
            byte_len: Some(42),
        };
        let debug = format!("{source:?}");

        assert!(!debug.contains(&secret_digest));
        assert!(!debug.contains("private-provider-path-marker"));
        assert!(debug.contains("<redacted>"));
    }

    #[test]
    fn commitment_failure_downgrades_without_retaining_transient_digest() {
        let bytes = b"redacted source commitment fixture".to_vec();
        let snapshot = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", "session-marker", &bytes),
            bytes,
        };
        let mut snapshot = CaptureSnapshotService::capture_authorized(
            snapshot,
            "opencode",
            "session-marker",
            CaptureSnapshotPolicy::default(),
        );
        assert!(snapshot.redacted_digest_preimage().is_some());

        snapshot.downgrade_for_commitment_failure(
            CaptureSnapshotPartialReason::SourceCommitmentUnavailable,
        );
        let projection = snapshot.safe_projection();

        assert_eq!(
            projection.partial_reason,
            Some(CaptureSnapshotPartialReason::SourceCommitmentUnavailable)
        );
        assert_eq!(
            projection.source.and_then(|source| source.digest_sha256),
            None
        );
        assert!(snapshot.transcript().is_none());
    }
    use crate::internal::ai::observed_agents::{
        AgentKind, ClaudeCodeObservedAgent, ExportAuthorized,
    };

    struct HomeGuard {
        prior: Option<std::ffi::OsString>,
    }

    impl HomeGuard {
        fn set(path: &Path) -> Self {
            let prior = std::env::var_os("LIBRA_TEST_HOME");
            // SAFETY: tests holding the `env` serial lane are the only
            // concurrent readers/writers of this process-global test hook.
            unsafe { std::env::set_var("LIBRA_TEST_HOME", path) };
            Self { prior }
        }
    }

    impl Drop for HomeGuard {
        fn drop(&mut self) {
            // SAFETY: paired with `HomeGuard::set` under the same serial lane.
            unsafe {
                match &self.prior {
                    Some(value) => std::env::set_var("LIBRA_TEST_HOME", value),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }

    fn context(path: Option<PathBuf>) -> AgentSessionCtx {
        AgentSessionCtx {
            session_id: "claude_code__snapshot-test".to_string(),
            provider_session_id: "snapshot-test".to_string(),
            working_dir: PathBuf::from("/tmp/snapshot-worktree"),
            transcript_path: path,
        }
    }

    fn make_claude_transcript(home: &Path, name: &str, bytes: &[u8]) -> PathBuf {
        let directory = home.join(".claude/projects/snapshot-worktree");
        std::fs::create_dir_all(&directory).expect("create provider fixture directory");
        let path = directory.join(name);
        std::fs::write(&path, bytes).expect("write provider fixture");
        path
    }

    #[test]
    #[serial(env)]
    fn live_snapshot_redacts_and_projects_opaque_source_facts() {
        let home = tempfile::tempdir().expect("temporary home");
        let _home = HomeGuard::set(home.path());
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB";
        let path = make_claude_transcript(
            home.path(),
            "snapshot.jsonl",
            format!("{{\"token\":\"{secret}\"}}\n").as_bytes(),
        );
        let snapshot = CaptureSnapshotService::capture_live(
            &ClaudeCodeObservedAgent::new(),
            &context(Some(path)),
            CaptureSnapshotPolicy::default(),
        );

        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        assert_eq!(snapshot.partial_reason(), None);
        let transcript = snapshot.transcript().expect("complete transcript");
        assert!(!String::from_utf8_lossy(transcript.bytes()).contains(secret));
        let projection = snapshot.safe_projection();
        let source = projection.source.expect("source projection");
        assert_eq!(source.kind, CaptureSnapshotSourceKind::ProviderFile);
        assert_eq!(source.identity, SOURCE_IDENTITY_NOT_RETAINED);
        assert!(source.digest_sha256.is_some());
        assert!(
            !source
                .identity
                .contains(home.path().to_string_lossy().as_ref())
        );
        assert!(!format!("{snapshot:?}").contains(secret));
    }

    #[test]
    #[serial(env)]
    fn exact_cap_live_snapshot_redacts_without_reallocating_the_raw_source() {
        let home = tempfile::tempdir().expect("temporary home");
        let _home = HomeGuard::set(home.path());
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB";
        let cap = usize::try_from(TRANSCRIPT_READ_HARD_CAP_BYTES)
            .expect("test transcript cap fits usize");
        let mut bytes = vec![b'x'; cap];
        bytes[..secret.len()].copy_from_slice(secret.as_bytes());
        bytes[secret.len()] = b'\n';
        let path = make_claude_transcript(home.path(), "exact-cap.jsonl", &bytes);

        let snapshot = CaptureSnapshotService::capture_live(
            &ClaudeCodeObservedAgent::new(),
            &context(Some(path)),
            CaptureSnapshotPolicy::default(),
        );

        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete,
            "an exact-cap descriptor source must not become a read failure during its EOF probe"
        );
        let transcript = snapshot.transcript().expect("complete snapshot transcript");
        assert!(
            !String::from_utf8_lossy(transcript.bytes()).contains(secret),
            "the exact-cap source must still traverse the default redactor"
        );
        assert_eq!(
            snapshot.safe_projection().redaction_bytes_scanned,
            cap,
            "the redactor must receive every byte accepted at the hard cap"
        );
    }

    #[test]
    fn source_digest_commits_only_redacted_snapshot_bytes() {
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB";
        let raw_bytes = format!("{{\"token\":\"{secret}\"}}\n").into_bytes();
        let raw_digest = digest_hex(&raw_bytes);
        let source = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", "libra-session", &raw_bytes),
            bytes: raw_bytes,
        };

        let snapshot = CaptureSnapshotService::capture_authorized(
            source,
            "opencode",
            "libra-session",
            CaptureSnapshotPolicy::default(),
        );

        let transcript = snapshot.transcript().expect("complete transcript");
        assert!(
            !String::from_utf8_lossy(transcript.bytes()).contains(secret),
            "the fixture must exercise a redaction rule"
        );
        let persisted_digest = snapshot
            .safe_projection()
            .source
            .and_then(|source| source.digest_sha256)
            .expect("complete snapshots persist a source digest");
        assert_ne!(
            persisted_digest, raw_digest,
            "durable source metadata must not commit the unredacted secret"
        );
        assert_eq!(
            persisted_digest,
            digest_hex(transcript.bytes()),
            "the persisted source digest must commit exactly the redacted bytes"
        );
    }

    #[test]
    fn authorized_snapshot_uses_the_successfully_read_length_not_a_stale_probe() {
        let bytes = b"safe transcript".to_vec();
        let snapshot = CaptureSnapshotService::capture_authorized_bytes(
            bytes.clone(),
            CaptureSnapshotSource {
                kind: CaptureSnapshotSourceKind::ProviderFile,
                identity: SOURCE_IDENTITY_NOT_RETAINED.to_string(),
                digest_sha256: None,
                byte_len: Some(999),
            },
            CaptureSnapshotPolicy::default(),
        );

        let projection = snapshot.safe_projection();
        assert_eq!(
            projection.source.and_then(|source| source.byte_len),
            Some(u64::try_from(bytes.len()).expect("test byte length fits u64")),
            "complete snapshot metadata must match the redaction scan input"
        );
        assert_eq!(
            projection.redaction_bytes_scanned,
            bytes.len(),
            "the capture projection must retain the actual bounded read length"
        );
    }

    #[test]
    fn trusted_export_identity_is_noncorrelating_and_never_commits_raw_content() {
        let first_secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB";
        let second_secret = "ghp_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210CD";
        let first_raw = format!("{{\"token\":\"{first_secret}\"}}\n").into_bytes();
        let second_raw = format!("{{\"token\":\"{second_secret}\"}}\n").into_bytes();
        assert_ne!(first_raw, second_raw);

        let capture_export = |bytes: Vec<u8>| {
            let auth = ExportAuthorized::issue("opencode", "libra-session", &bytes);
            CaptureSnapshotService::capture_authorized(
                TranscriptSource::Bytes { auth, bytes },
                "opencode",
                "libra-session",
                CaptureSnapshotPolicy::default(),
            )
        };
        let first = capture_export(first_raw.clone());
        let second = capture_export(second_raw.clone());

        let first_redacted = first
            .transcript()
            .expect("first complete transcript")
            .bytes()
            .to_vec();
        let second_redacted = second
            .transcript()
            .expect("second complete transcript")
            .bytes()
            .to_vec();
        assert_eq!(
            first_redacted, second_redacted,
            "the fixtures must differ only in the redacted token"
        );

        let first_identity = first
            .safe_projection()
            .source
            .map(|source| source.identity)
            .expect("first source projection");
        let second_identity = second
            .safe_projection()
            .source
            .map(|source| source.identity)
            .expect("second source projection");
        assert_eq!(first_identity, SOURCE_IDENTITY_NOT_RETAINED);
        assert_eq!(
            first_identity, second_identity,
            "different raw secrets with the same redacted content must not change durable identity"
        );
        for forbidden in [
            first_secret,
            second_secret,
            "libra-session",
            &digest_hex(&first_raw),
            &digest_hex(&second_raw),
        ] {
            assert!(
                !first_identity.contains(forbidden),
                "durable identity must not retain an enumerable source value"
            );
        }
    }

    #[test]
    fn discovered_subagent_identity_is_noncorrelating() {
        let bytes = br#"{"type":"assistant","message":"safe"}\n"#.to_vec();
        let snapshot = CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue_discovered_subagent(
                    "claude_code",
                    "parent-session-secret",
                    &bytes,
                ),
                bytes,
            },
            "claude_code",
            "parent-session-secret",
            CaptureSnapshotPolicy::default(),
        );
        let source = snapshot
            .safe_projection()
            .source
            .expect("discovered child projection");
        assert_eq!(source.kind, CaptureSnapshotSourceKind::DiscoveredSubagent);
        assert_eq!(source.identity, SOURCE_IDENTITY_NOT_RETAINED);
        assert!(!source.identity.contains("parent-session-secret"));
    }

    #[test]
    #[serial(env)]
    fn live_snapshot_distinguishes_absent_and_untrusted_sources() {
        let home = tempfile::tempdir().expect("temporary home");
        let _home = HomeGuard::set(home.path());
        let adapter = ClaudeCodeObservedAgent::new();
        let absent = CaptureSnapshotService::capture_live(
            &adapter,
            &context(None),
            CaptureSnapshotPolicy::default(),
        );
        assert_eq!(
            absent.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceAbsent)
        );

        let outside = home.path().join("outside.jsonl");
        std::fs::write(&outside, b"not provider owned").expect("write untrusted fixture");
        let untrusted = CaptureSnapshotService::capture_live(
            &adapter,
            &context(Some(outside)),
            CaptureSnapshotPolicy::default(),
        );
        assert_eq!(
            untrusted.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceUntrusted)
        );
    }

    #[test]
    fn authorized_export_rejects_mismatch_and_oversize_without_payload() {
        let bytes = b"abcdef".to_vec();
        let mismatch = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", "other-session", &bytes),
            bytes: bytes.clone(),
        };
        let mismatch = CaptureSnapshotService::capture_authorized(
            mismatch,
            AgentKind::OpenCode.as_db_str(),
            "libra-session",
            CaptureSnapshotPolicy::with_max_bytes(None, 64),
        );
        assert_eq!(
            mismatch.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
        );
        assert!(mismatch.transcript().is_none());

        let oversize = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", "libra-session", &bytes),
            bytes,
        };
        let oversize = CaptureSnapshotService::capture_authorized(
            oversize,
            AgentKind::OpenCode.as_db_str(),
            "libra-session",
            CaptureSnapshotPolicy::with_max_bytes(None, 4),
        );
        assert_eq!(
            oversize.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceOversize)
        );
        assert!(oversize.transcript().is_none());
    }

    #[test]
    fn authorized_export_fails_closed_when_redaction_expansion_exceeds_shared_budget() {
        // This credential URI is shorter than its durable replacement. The
        // normal historical-import path must apply the same redaction budget
        // as the helper-backed path, or it can produce a complete snapshot
        // that the catalog rejects only after import ownership is prepared.
        let bytes = b"redis://a:b@c".to_vec();
        let source = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", "libra-session", &bytes),
            bytes,
        };

        let snapshot = CaptureSnapshotService::capture_authorized(
            source,
            AgentKind::OpenCode.as_db_str(),
            "libra-session",
            CaptureSnapshotPolicy::with_max_bytes(None, 64),
        );

        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceReadError),
            "redaction output outside the shared capture budget must fail closed before import persistence"
        );
        assert!(snapshot.transcript().is_none());
        let projection = snapshot.safe_projection();
        assert_eq!(projection.transcript_redacted_bytes, 0);
        assert_eq!(projection.redaction_bytes_scanned, 0);
        assert_eq!(projection.redaction_bytes_redacted, 0);
    }

    #[test]
    fn deadline_returns_explicit_partial_without_reading_payload() {
        let bytes = b"payload".to_vec();
        let source = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", "libra-session", &bytes),
            bytes,
        };
        let deadline = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("instant supports a past deadline");
        let snapshot = CaptureSnapshotService::capture_authorized(
            source,
            "opencode",
            "libra-session",
            CaptureSnapshotPolicy::with_max_bytes(Some(deadline), 64),
        );
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::DeadlineExceeded)
        );
        assert!(snapshot.transcript().is_none());
    }

    /// Once a source descriptor is acquired, the SessionEnd helper must not
    /// perform parent-side provider source I/O. A helper that never reads its
    /// bounded descriptor input models a stalled flush preparation or
    /// descriptor read; the parent must still return at the original helper
    /// deadline and eventually reap the child. This test covers the helper
    /// phase only, not synchronous descriptor acquisition.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    #[serial(env)]
    async fn live_deadline_helper_kills_and_reaps_a_blocked_source_operation() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().expect("create helper deadline home");
        let _home = HomeGuard::set(home.path());
        let transcript = make_claude_transcript(home.path(), "blocked.jsonl", b"unused");
        let fixture = tempfile::tempdir().expect("create helper deadline fixture");
        let pid_file = fixture.path().join("blocked-live-source.pid");
        let script = fixture.path().join("blocked-live-source.sh");
        let escaped_pid_file = pid_file.to_string_lossy().replace('\'', "'\\\"'\\\"'");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s' \"$$\" > '{escaped_pid_file}'\nexec /bin/sleep 60\n"),
        )
        .expect("write blocked live source helper");
        let mut permissions = std::fs::metadata(&script)
            .expect("read blocked live source helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions)
            .expect("make blocked live source helper executable");

        let started = Instant::now();
        let adapter = ClaudeCodeObservedAgent::new();
        let snapshot = crate::internal::ai::authorized_read::with_test_helper_program(
            script,
            CaptureSnapshotService::capture_live_until(
                &adapter,
                &context(Some(transcript)),
                CaptureSnapshotPolicy::with_deadline(Some(Instant::now() + Duration::from_secs(2))),
            ),
        )
        .await;
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::DeadlineExceeded)
        );
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "blocked source helper outlived its absolute capture deadline: {:?}",
            started.elapsed()
        );

        let pid = async {
            for _ in 0..250 {
                if let Ok(value) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            None
        }
        .await
        .expect("blocked helper must publish its PID before sleeping");
        let reaped = async {
            for _ in 0..250 {
                // SAFETY: signal zero only probes this helper PID.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            false
        }
        .await;
        assert!(
            reaped,
            "blocked live source helper was not killed and reaped"
        );
    }

    /// Extraction must use the same bounded private-helper lifecycle as the
    /// live source reader: an extractor stalled before consuming stdin cannot
    /// hold a SessionEnd hook past its absolute deadline or leave a child.
    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    #[serial(env)]
    async fn extraction_deadline_helper_kills_and_reaps_a_blocked_worker() {
        use std::os::unix::fs::PermissionsExt;

        let fixture = tempfile::tempdir().expect("create extraction helper fixture");
        let pid_file = fixture.path().join("blocked-extraction.pid");
        let script = fixture.path().join("blocked-extraction.sh");
        let escaped_pid_file = pid_file.to_string_lossy().replace('\'', "'\\\"'\\\"'");
        std::fs::write(
            &script,
            format!("#!/bin/sh\nprintf '%s' \"$$\" > '{escaped_pid_file}'\nexec /bin/sleep 60\n"),
        )
        .expect("write blocked extraction helper");
        let mut permissions = std::fs::metadata(&script)
            .expect("read blocked extraction helper permissions")
            .permissions();
        permissions.set_mode(0o700);
        std::fs::set_permissions(&script, permissions)
            .expect("make blocked extraction helper executable");

        let transcript = RedactedBytes::new_unchecked(
            br#"{"type":"user","message":{"content":"bounded"}}\n"#.to_vec(),
        );
        let started = Instant::now();
        let outcome = crate::internal::ai::authorized_read::with_test_helper_program(
            script,
            CaptureSnapshotService::build_extraction_projection_until(
                "claude_code",
                Some(&transcript),
                true,
                Instant::now() + Duration::from_secs(2),
            ),
        )
        .await;
        assert!(matches!(
            outcome,
            DeadlineExtractionResult::DeadlineExceeded
        ));
        assert!(
            started.elapsed() < Duration::from_secs(4),
            "blocked extraction worker outlived its absolute capture deadline: {:?}",
            started.elapsed()
        );

        let pid = async {
            for _ in 0..250 {
                if let Ok(value) = std::fs::read_to_string(&pid_file)
                    && let Ok(pid) = value.trim().parse::<libc::pid_t>()
                {
                    return Some(pid);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            None
        }
        .await
        .expect("blocked extraction helper must publish its PID before sleeping");
        let reaped = async {
            for _ in 0..250 {
                // SAFETY: signal zero only probes this helper PID.
                if unsafe { libc::kill(pid, 0) } == -1
                    && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
                {
                    return true;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            false
        }
        .await;
        assert!(
            reaped,
            "blocked extraction helper was not killed and reaped"
        );
    }
}
