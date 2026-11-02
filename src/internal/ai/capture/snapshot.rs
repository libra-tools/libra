//! Authorized, bounded transcript snapshots for Agent Capture.
//!
//! Provider-native bytes may exist only while this module reads one
//! [`TranscriptSource`]. The resulting [`CaptureSnapshot`] exposes redacted
//! content and safe provenance only; it intentionally has no raw-byte
//! accessor, `Serialize` implementation, or content-bearing `Debug` output.
//! This makes the snapshot service the one common boundary for live capture,
//! historical import, and later capture-push consumers.

use std::{fmt, path::Path, time::Instant};

use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

pub(crate) use crate::internal::ai::capture::extraction::DeadlineExtractionResult;
use crate::internal::ai::{
    authorized_read::{
        LiveClaudeSourceRead, SOURCE_IDENTITY_NOT_RETAINED, capture_redacted_output_cap,
        capture_redaction_working_set_cap, provider_file_identity, read_live_claude_source_until,
    },
    capture_scope::CaptureScope,
    observed_agents::{
        AgentKind, AgentSessionCtx, ObservedAgent, RedactedBytes, RedactionReport, Redactor,
        TRANSCRIPT_READ_HARD_CAP_BYTES, TranscriptReadError, TranscriptSource,
        TranscriptSourceResolution,
        builtin::pi::PiDeadline,
        codex_reasoning::{
            CodexAttachedProjection,
            authorize_codex_attached_fields as authorize_codex_attached_source,
        },
        coverage::{
            CoverageRedactionLimits, NormalizedTurn, prepare_authorized_reasoning_coverage,
        },
        reasoning::{
            ClaudeAttachedProjection, OpenCodeOrdinaryPartialPermit, ReasoningCoverageFragment,
            ReasoningFieldError, ReasoningSourceDisposition,
            authorize_claude_attached_fields as authorize_claude_attached_source,
            project_claude_attached_fields, take_authorized_reasoning_coverage,
        },
        resolve_live_transcript_source_until,
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
    /// Hermes ordinary thinking is retained without a native issuer.
    UnprovenOrdinary,
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
    #[cfg_attr(windows, allow(dead_code))]
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

/// Existing source authorization facts, not native provenance. A legacy
/// wrapper or a parent-only child cannot supply a verified native mapping.
/// Keep native coordinates out of Debug, metadata and helper protocols.
pub(crate) struct AuthorizedReasoningSourceContext {
    provider: String,
    authorized_session: String,
    capture_session: String,
    provider_session: Option<String>,
    origin: AuthorizedReasoningSourceOrigin,
}

enum AuthorizedReasoningSourceOrigin {
    ScopeVerifiedLive,
    AuthorizedExport,
    VerifiedImport,
    ParentOnlyChild,
    LegacyOrdinary,
}

impl AuthorizedReasoningSourceContext {
    fn live(adapter: &dyn ObservedAgent, session: &AgentSessionCtx) -> Self {
        Self {
            provider: adapter.provider_kind().as_db_str().to_owned(),
            authorized_session: session.session_id.clone(),
            capture_session: session.session_id.clone(),
            provider_session: Some(session.provider_session_id.clone()),
            origin: AuthorizedReasoningSourceOrigin::ScopeVerifiedLive,
        }
    }

    pub(crate) fn authorized_export(
        provider: &str,
        capture_session: &str,
        provider_session: &str,
    ) -> Self {
        Self {
            provider: provider.to_owned(),
            authorized_session: capture_session.to_owned(),
            capture_session: capture_session.to_owned(),
            provider_session: Some(provider_session.to_owned()),
            origin: AuthorizedReasoningSourceOrigin::AuthorizedExport,
        }
    }

    pub(crate) fn verified_import(
        provider: &str,
        authorized_session: &str,
        capture_session: &str,
        provider_session: &str,
    ) -> Self {
        Self {
            provider: provider.to_owned(),
            authorized_session: authorized_session.to_owned(),
            capture_session: capture_session.to_owned(),
            provider_session: Some(provider_session.to_owned()),
            origin: AuthorizedReasoningSourceOrigin::VerifiedImport,
        }
    }

    pub(crate) fn parent_only_child(provider: &str, parent_session: &str) -> Self {
        Self::ordinary(
            provider,
            parent_session,
            AuthorizedReasoningSourceOrigin::ParentOnlyChild,
        )
    }

    fn ordinary(provider: &str, session: &str, origin: AuthorizedReasoningSourceOrigin) -> Self {
        Self {
            provider: provider.to_owned(),
            authorized_session: session.to_owned(),
            capture_session: session.to_owned(),
            provider_session: None,
            origin,
        }
    }

    fn has_verified_mapping(&self) -> bool {
        !self.capture_session.is_empty()
            && self
                .provider_session
                .as_ref()
                .is_some_and(|id| !id.is_empty())
            && matches!(
                self.origin,
                AuthorizedReasoningSourceOrigin::ScopeVerifiedLive
                    | AuthorizedReasoningSourceOrigin::AuthorizedExport
                    | AuthorizedReasoningSourceOrigin::VerifiedImport
            )
    }
}

/// Rejections before the preprocessor still own an authorized in-memory export.
/// Erase that raw allocation before returning a content-free partial result.
fn wipe_in_memory_source(source: &mut TranscriptSource) {
    if let TranscriptSource::Bytes { bytes, .. } = source {
        bytes.zeroize();
    }
}

/// Private source preparation result. Raw typed data never crosses the
/// snapshot boundary, and its ownership stays separate from sealed authority.
struct AuthorizedReasoningPreparation {
    ordinary: Vec<u8>,
    disposition: ReasoningSourceDisposition,
    ordinary_partial_permit: Option<OpenCodeOrdinaryPartialPermit>,
    coverage_fragment: Option<ReasoningCoverageFragment>,
    classifier_rejected: bool,
    source_partial: bool,
}

/// Called once, after source authorization and a bounded read, before Redactor.
fn prepare_authorized_reasoning_source(
    bytes: Vec<u8>,
    adapter: &dyn ObservedAgent,
    context: &AuthorizedReasoningSourceContext,
    policy: CaptureSnapshotPolicy,
) -> Result<AuthorizedReasoningPreparation, CaptureSnapshotPartialReason> {
    let mut bytes = Zeroizing::new(bytes);
    if deadline_elapsed(policy) {
        return Err(CaptureSnapshotPartialReason::DeadlineExceeded);
    }
    if bytes.len() as u64 > policy.max_bytes {
        return Err(CaptureSnapshotPartialReason::SourceOversize);
    }
    if adapter.provider_kind().as_db_str() != context.provider {
        return Err(CaptureSnapshotPartialReason::SourceAuthorizationMismatch);
    }
    let Some(preprocessor) = adapter.as_reasoning_source_preprocessor() else {
        return Ok(AuthorizedReasoningPreparation {
            ordinary: std::mem::take(&mut *bytes),
            disposition: ReasoningSourceDisposition::Ordinary,
            ordinary_partial_permit: None,
            coverage_fragment: None,
            classifier_rejected: false,
            source_partial: false,
        });
    };
    let mut prepared = preprocessor
        .prepare_authorized_source(std::mem::take(&mut *bytes))
        .map_err(reasoning_preparation_partial_reason)?;
    if deadline_elapsed(policy) {
        return Err(CaptureSnapshotPartialReason::DeadlineExceeded);
    }
    let (coverage_fragment, classifier_rejected, source_partial) =
        take_authorized_reasoning_coverage(
            &mut prepared,
            context.has_verified_mapping(),
            &context.provider,
            context.provider_session.as_deref(),
        )
        .map_err(reasoning_preparation_partial_reason)?;
    let (ordinary, disposition, ordinary_partial_permit) =
        ReasoningSourceDisposition::from_prepared_authorized_source_with_partial_permit(
            prepared,
            context.has_verified_mapping(),
            policy.max_bytes,
            &context.provider,
            &context.capture_session,
            context.provider_session.as_deref(),
        )
        .map_err(reasoning_preparation_partial_reason)?;
    Ok(AuthorizedReasoningPreparation {
        ordinary,
        disposition,
        ordinary_partial_permit,
        coverage_fragment,
        classifier_rejected,
        source_partial,
    })
}

fn reasoning_preparation_partial_reason(
    error: ReasoningFieldError,
) -> CaptureSnapshotPartialReason {
    match error {
        ReasoningFieldError::SourceTooLarge => CaptureSnapshotPartialReason::SourceOversize,
        ReasoningFieldError::MalformedSource | ReasoningFieldError::UnsupportedShape => {
            CaptureSnapshotPartialReason::SourceAuthorizationMismatch
        }
    }
}

/// Redacted runtime data only, consumed once before the public snapshot
/// accessors. It grants no source or native authority. Historical import may
/// encode its redacted data and content-free flags only through the closed
/// import DTO codec; this owner itself has no serialization implementation.
#[derive(Default)]
pub(crate) struct CaptureCoverageRuntime {
    pub(crate) turns: Option<Vec<NormalizedTurn>>,
    pub(crate) classifier_rejected: bool,
    pub(crate) source_partial: bool,
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
    reasoning: ReasoningSourceDisposition,
    ordinary_partial_permit: Option<OpenCodeOrdinaryPartialPermit>,
    coverage_runtime: CaptureCoverageRuntime,
    /// Original authorized input boundary for the multi-pass typed pipeline.
    /// The full redaction report separately counts every scan/pass.
    source_scan_bytes: Option<usize>,
}

/// Scope and both Session identities recorded at the actual scoped HMAC bind.
/// This private owner carries no native-source authority or serialization.
pub(crate) struct VerifiedOrdinaryPartialBinding {
    scope: CaptureScope,
    agent_kind: &'static str,
    capture_session: String,
    source_session: String,
}

impl VerifiedOrdinaryPartialBinding {
    pub(crate) fn matches_capture(&self, session: &str, provider: &str) -> bool {
        provider == self.agent_kind && session == self.capture_session
    }
    pub(crate) fn matches_scope(&self, scope: Option<&CaptureScope>) -> bool {
        scope == Some(&self.scope)
    }
    pub(crate) fn capture_session(&self) -> &str {
        &self.capture_session
    }
    pub(crate) fn source_session(&self) -> &str {
        &self.source_session
    }
}

/// Non-Clone storage-only owner, never a native or ordinary readiness flag.
pub(crate) struct VerifiedOrdinaryPartialSnapshot {
    transcript: RedactedBytes,
    projection: CaptureSnapshotProjection,
    redaction_report: RedactionReport,
    binding: VerifiedOrdinaryPartialBinding,
}

impl VerifiedOrdinaryPartialSnapshot {
    pub(crate) fn transcript(&self) -> &RedactedBytes {
        &self.transcript
    }
    pub(crate) fn projection(&self) -> CaptureSnapshotProjection {
        self.projection.clone()
    }
    pub(crate) fn redaction_report(&self) -> &RedactionReport {
        &self.redaction_report
    }
    pub(crate) fn into_payload_parts(
        self,
    ) -> (
        RedactedBytes,
        CaptureSnapshotProjection,
        VerifiedOrdinaryPartialBinding,
    ) {
        (self.transcript, self.projection, self.binding)
    }
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
            reasoning: ReasoningSourceDisposition::Ordinary,
            ordinary_partial_permit: None,
            coverage_runtime: CaptureCoverageRuntime::default(),
            source_scan_bytes: None,
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
            reasoning: ReasoningSourceDisposition::Ordinary,
            ordinary_partial_permit: None,
            coverage_runtime: CaptureCoverageRuntime::default(),
            source_scan_bytes: None,
        }
    }

    /// Move the private redacted companion out exactly once. Public snapshot
    /// projections and checkpoint tuple signatures remain unchanged.
    pub(crate) fn take_coverage_runtime(&mut self) -> CaptureCoverageRuntime {
        std::mem::take(&mut self.coverage_runtime)
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
        self.try_into_ordinary().ok().flatten()
    }

    /// Ordinary-only consumers must retain ownership when typed reasoning
    /// has no ready consumer. A zero-artifact safe partial is still rejected.
    pub fn try_into_ordinary(self) -> Result<Option<RedactedBytes>, Box<Self>> {
        if self.reasoning.is_ordinary() {
            Ok(self.transcript_redacted)
        } else {
            Err(Box::new(self))
        }
    }

    /// A pure routing observation. Generic consumer/native gates stay shut.
    pub(crate) fn has_verified_ordinary_partial_permit(&self) -> bool {
        matches!(self.reasoning, ReasoningSourceDisposition::Unproven)
            && self.ordinary_partial_permit.is_some()
            && self.completeness == CaptureSnapshotCompleteness::Partial
            && self.partial_reason
                == Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
            && self
                .source
                .as_ref()
                .is_some_and(|source| source.kind == CaptureSnapshotSourceKind::TrustedExport)
            && self
                .transcript_redacted
                .as_ref()
                .is_some_and(|bytes| !bytes.is_empty())
    }

    /// Mint only after the actual current scope/key binds these exact bytes;
    /// a prefix-valid caller-supplied HMAC cannot manufacture this owner.
    pub(crate) async fn try_into_verified_ordinary_partial_in_scope_until<C: ConnectionTrait>(
        mut self,
        conn: &C,
        scope: &CaptureScope,
        storage: &Path,
        authorized_root: &Path,
        deadline: Instant,
    ) -> Result<VerifiedOrdinaryPartialSnapshot, Box<Self>> {
        if !self.has_verified_ordinary_partial_permit() || Instant::now() >= deadline {
            self.downgrade_for_commitment_failure(
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
            );
            return Err(Box::new(self));
        }
        let Some(preimage) = self.redacted_digest_preimage() else {
            self.downgrade_for_commitment_failure(
                CaptureSnapshotPartialReason::SourceCommitmentUnavailable,
            );
            return Err(Box::new(self));
        };
        let digest_matches = self
            .transcript_redacted
            .as_ref()
            .is_some_and(|bytes| <[u8; 32]>::from(Sha256::digest(bytes.bytes())) == preimage);
        if !digest_matches {
            self.downgrade_for_commitment_failure(
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
            );
            return Err(Box::new(self));
        }
        let commitment = super::key::derive_snapshot_content_commitment_in_scope_until(
            conn,
            scope,
            storage,
            authorized_root,
            &preimage,
            deadline,
        )
        .await;
        let Ok(commitment) = commitment else {
            self.downgrade_for_commitment_failure(
                CaptureSnapshotPartialReason::SourceCommitmentUnavailable,
            );
            return Err(Box::new(self));
        };
        if Instant::now() >= deadline || !self.bind_source_commitment(commitment) {
            self.downgrade_for_commitment_failure(
                CaptureSnapshotPartialReason::SourceCommitmentUnavailable,
            );
            return Err(Box::new(self));
        }
        let Some(permit) = self.ordinary_partial_permit.take() else {
            self.downgrade_for_commitment_failure(
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
            );
            return Err(Box::new(self));
        };
        let projection = self.safe_projection();
        let Some(transcript) = self.transcript_redacted.take() else {
            self.downgrade_for_commitment_failure(CaptureSnapshotPartialReason::SourceReadError);
            return Err(Box::new(self));
        };
        let binding = VerifiedOrdinaryPartialBinding {
            scope: scope.clone(),
            agent_kind: "opencode",
            capture_session: permit.capture_session().to_owned(),
            source_session: permit.source_session().to_owned(),
        };
        Ok(VerifiedOrdinaryPartialSnapshot {
            transcript,
            projection,
            redaction_report: self.redaction_report,
            binding,
        })
    }

    /// A readiness check only; ownership is still moved to the actual
    /// writer or returned by the ordinary consuming rejection.
    #[cfg(test)]
    pub(crate) fn rejects_ordinary_source_consumer(&self) -> bool {
        !self.reasoning.is_ordinary()
    }

    /// The capture pipeline has a typed consumer for Ordinary and
    /// NativeProven dispositions. Unknown, unmapped, and helper-unsupported
    /// states remain unavailable to every writer.
    pub(crate) fn rejects_capture_source_consumer(&self) -> bool {
        !self.reasoning.has_capture_consumer()
    }

    pub(crate) fn into_checkpoint_parts(
        self,
    ) -> (
        CaptureSnapshotProjection,
        Option<RedactedBytes>,
        ReasoningSourceDisposition,
    ) {
        let projection = self.safe_projection();
        (projection, self.transcript_redacted, self.reasoning)
    }

    /// Rule-hit report for the redaction pass that produced
    /// [`Self::transcript`]. It contains only rule identifiers and offsets.
    pub fn redaction_report(&self) -> &RedactionReport {
        &self.redaction_report
    }

    /// Readable Hermes thinking stored in this snapshot, in source order.
    /// Empty when the retained body is not a usable Hermes frame.
    pub fn hermes_readable_thinking(&self) -> Vec<String> {
        let Some(transcript) = self.transcript_redacted.as_ref() else {
            return Vec::new();
        };
        let Ok(frame) =
            crate::internal::ai::observed_agents::builtin::hermes::hermes_worker_frame_authorized(
                transcript.bytes(),
                None,
            )
        else {
            return Vec::new();
        };
        frame
            .thinking
            .iter()
            .filter_map(|item| item.readable_thinking().map(str::to_string))
            .collect()
    }

    /// Readable Pi thinking stored in this snapshot, in source order: the
    /// readable text of each `thinking` block on the active branch, never
    /// the assistant answer. Empty when the retained body is not a usable Pi
    /// worker frame (PI-33).
    pub fn pi_readable_thinking(&self) -> Vec<String> {
        let Some(transcript) = self.transcript_redacted.as_ref() else {
            return Vec::new();
        };
        let Ok(frame) =
            crate::internal::ai::observed_agents::builtin::pi::pi_worker_frame_authorized(
                transcript.bytes(),
                None,
            )
        else {
            return Vec::new();
        };
        frame
            .thinking
            .iter()
            .filter_map(|item| item.readable.as_ref().map(|text| text.as_str().to_string()))
            .collect()
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
            redaction_bytes_scanned: self
                .source_scan_bytes
                .unwrap_or(self.redaction_report.bytes_scanned),
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
        self.ordinary_partial_permit = None;
        self.coverage_runtime.turns = None;
        self.coverage_runtime.source_partial = true;
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

/// Pure in-memory preview, distinct from source/capture authorization.
fn prepare_redacted_coverage_preview(
    agent_kind: &str,
    bytes: Vec<u8>,
    policy: CaptureSnapshotPolicy,
) -> Result<
    crate::internal::ai::observed_agents::coverage::RedactedCoveragePreparation,
    CaptureSnapshotPartialReason,
> {
    let mut bytes = Zeroizing::new(bytes);
    if deadline_elapsed(policy) {
        return Err(CaptureSnapshotPartialReason::DeadlineExceeded);
    }
    let raw_bytes =
        u64::try_from(bytes.len()).map_err(|_| CaptureSnapshotPartialReason::SourceOversize)?;
    if raw_bytes == 0 {
        return Err(CaptureSnapshotPartialReason::SourceEmpty);
    }
    if raw_bytes > policy.max_bytes || raw_bytes > TRANSCRIPT_READ_HARD_CAP_BYTES {
        return Err(CaptureSnapshotPartialReason::SourceOversize);
    }
    let limits = crate::internal::ai::observed_agents::coverage::CoverageRedactionLimits {
        max_source_bytes: policy.max_bytes,
        max_output_bytes: usize::try_from(capture_redacted_output_cap(raw_bytes))
            .map_err(|_| CaptureSnapshotPartialReason::SourceOversize)?,
        max_working_set_bytes: usize::try_from(capture_redaction_working_set_cap(raw_bytes))
            .map_err(|_| CaptureSnapshotPartialReason::SourceOversize)?,
        deadline: policy.deadline(),
    };
    let result = crate::internal::ai::observed_agents::coverage::prepare_coverage_preview(
        agent_kind,
        std::mem::take(&mut *bytes),
        limits,
    );
    if deadline_elapsed(policy) {
        return Err(CaptureSnapshotPartialReason::DeadlineExceeded);
    }
    result.map_err(reasoning_preparation_partial_reason)
}

/// The single raw-byte boundary for capture snapshots.
pub struct CaptureSnapshotService;

impl CaptureSnapshotService {
    /// Prepare a bounded, redacted coverage preview from caller-owned memory.
    ///
    /// This pure data API grants no source/capture/native authority, reads no
    /// file and writes no checkpoint. It returns only redacted bytes and safe
    /// JSON (envelope 1, per-turn coverage 1/2); no unredacted typed carrier or
    /// opaque artifact can escape. Existing live/import entry points continue
    /// to use their legacy behavior until the separate production activation.
    pub fn preview_redacted_coverage_v2(
        agent_kind: &str,
        bytes: Vec<u8>,
        policy: CaptureSnapshotPolicy,
    ) -> Result<(RedactedBytes, serde_json::Value), CaptureSnapshotPartialReason> {
        use crate::internal::ai::observed_agents::coverage::{
            Completeness, encode_coverage_turn, safe_turn_projection_for_schema,
        };
        let prepared = prepare_redacted_coverage_preview(agent_kind, bytes, policy)?;
        let cap = usize::try_from(capture_redacted_output_cap(policy.max_bytes))
            .map_err(|_| CaptureSnapshotPartialReason::SourceOversize)?;
        let mut encoded_bytes =
            crate::internal::ai::observed_agents::coverage::encode_bounded_json(
                &prepared.report,
                cap,
            )
            .map_err(reasoning_preparation_partial_reason)?
            .len()
            .checked_add(128)
            .filter(|size| *size <= cap)
            .ok_or(CaptureSnapshotPartialReason::SourceOversize)?;
        let mut turns = Vec::with_capacity(prepared.turns.len());
        for turn in &prepared.turns {
            if deadline_elapsed(policy) {
                return Err(CaptureSnapshotPartialReason::DeadlineExceeded);
            }
            // Both ordinary and typed-field redaction have already succeeded.
            // The legacy-v1 projection is never used for typed Reasoning.
            let encoded = encode_coverage_turn(turn)
                .map_err(|_| CaptureSnapshotPartialReason::SourceReadError)?;
            if encoded.canonical_bytes.len() > cap {
                return Err(CaptureSnapshotPartialReason::SourceOversize);
            }
            let version = encoded.schema_version;
            let digest = encoded.digest_hex;
            drop(encoded.canonical_bytes);
            let projection = safe_turn_projection_for_schema(version.as_i64(), agent_kind, turn)
                .map_err(|_| CaptureSnapshotPartialReason::SourceReadError)?;
            let row = serde_json::json!({"coverage_schema_version":version.as_i64(),
                "digest":digest, "projection":projection});
            let row_bytes =
                crate::internal::ai::observed_agents::coverage::encode_bounded_json(&row, cap)
                    .map_err(reasoning_preparation_partial_reason)?
                    .len();
            encoded_bytes = encoded_bytes
                .checked_add(row_bytes)
                .and_then(|size| size.checked_add(1))
                .filter(|size| *size <= cap)
                .ok_or(CaptureSnapshotPartialReason::SourceOversize)?;
            turns.push(row);
        }
        if deadline_elapsed(policy) {
            return Err(CaptureSnapshotPartialReason::DeadlineExceeded);
        }
        let partial = prepared.partial
            || prepared
                .turns
                .iter()
                .any(|turn| turn.completeness != Completeness::Complete);
        let mut projection = serde_json::json!({"schema_version":1,"partial":partial,
            "turns":turns,"redaction_report":prepared.report});
        if agent_kind == "codex" {
            let sidecar = match crate::internal::ai::observed_agents::codex_reasoning::sidecar(
                prepared.transcript.bytes(),
                policy.deadline(),
            ) {
                Ok(value) => value,
                Err(_) => {
                    if deadline_elapsed(policy) {
                        return Err(CaptureSnapshotPartialReason::DeadlineExceeded);
                    }
                    Some(crate::internal::ai::observed_agents::codex_reasoning::unavailable_statistics())
                }
            };
            if let Some(sidecar) = sidecar {
                projection["partial"] = serde_json::json!(partial || sidecar["partial"] == true);
                projection["codex_reasoning"] = sidecar;
            }
        }
        Ok((prepared.transcript, projection))
    }

    /// Index ordinary Claude signature and redacted `data` fields.
    ///
    /// The caller keeps the original JSONL. This view does not create a native
    /// artifact and does not delete retained bytes when a line is rejected.
    pub fn claude_attached_projection(bytes: &[u8]) -> ClaudeAttachedProjection {
        project_claude_attached_fields(bytes)
    }

    /// Re-check the session file leaf before associating attached fields.
    /// A wrong leaf returns no fields on every call.
    pub fn authorize_claude_attached_fields(
        file_name: &str,
        session_id: &str,
        bytes: &[u8],
    ) -> ClaudeAttachedProjection {
        authorize_claude_attached_source(file_name, session_id, bytes)
    }

    /// Index decoded Codex `encrypted_content` only after the caller has
    /// authorized and credential-scanned the ordinary rollout source. Source
    /// bytes remain the durable carrier; this view grants no native artifact.
    pub fn authorize_codex_attached_fields(
        file_name: &str,
        session_id: &str,
        ordinary: &RedactedBytes,
    ) -> CodexAttachedProjection {
        authorize_codex_attached_source(file_name, session_id, ordinary.bytes())
    }

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
        let mut resolution = match resolve_live_transcript_source_until(adapter, context, None) {
            Ok(resolution) => resolution,
            Err(_) => {
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceReadError,
                    None,
                );
            }
        };
        if deadline_elapsed(policy) {
            if let TranscriptSourceResolution::Authorized(source) = &mut resolution {
                wipe_in_memory_source(source);
            }
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::DeadlineExceeded, None);
        }
        match resolution {
            TranscriptSourceResolution::Authorized(source) => {
                Self::capture_authorized_with_reasoning_context(
                    source,
                    adapter,
                    AuthorizedReasoningSourceContext::live(adapter, context),
                    policy,
                )
            }
            TranscriptSourceResolution::Absent => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceAbsent, None)
            }
            TranscriptSourceResolution::Untrusted => {
                CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceUntrusted, None)
            }
        }
    }

    pub(crate) fn live_locator_unavailable() -> CaptureSnapshot {
        CaptureSnapshot::partial(CaptureSnapshotPartialReason::SourceReadError, None)
    }

    /// Capture one live file through the killable descriptor helper.
    ///
    /// After scope-bound callers derive the provider candidate, this service
    /// pins it beneath the provider root and transfers that exact descriptor
    /// as helper stdin. The helper returns only redacted bytes and safe
    /// metadata, never a locator to reopen. Codex also receives the verified
    /// session id/cwd for a pre-redaction header check. Claude and Codex own
    /// this descriptor contract; unsupported adapters remain source-absent.
    pub async fn capture_live_until(
        adapter: &dyn ObservedAgent,
        context: &AgentSessionCtx,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        // The FD helper carries only redacted ordinary bytes. Reject an
        // enabled Claude detector before resolver/preparer/FD work, even when
        // the old ordinary deadline path would otherwise take precedence.
        if policy.deadline().is_some()
            && adapter.provider_kind().as_db_str() == "claude_code"
            && adapter.as_reasoning_source_preprocessor().is_some()
        {
            let mut snapshot = CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
                None,
            );
            snapshot.reasoning = ReasoningSourceDisposition::helper_unsupported();
            return snapshot;
        }
        Self::capture_live_file_until(adapter, context, policy, None).await
    }

    /// The binding threads the optional source from its one registration
    /// fetch. Legacy callers retain the adapter-only entrypoint above.
    pub(crate) async fn capture_live_with_source_until(
        adapter: &dyn ObservedAgent,
        context: &AgentSessionCtx,
        policy: CaptureSnapshotPolicy,
        source_capability: &dyn crate::internal::ai::observed_agents::live_capture::LiveTranscriptSource,
    ) -> CaptureSnapshot {
        Self::capture_live_file_until(adapter, context, policy, Some(source_capability)).await
    }

    async fn capture_live_file_until(
        adapter: &dyn ObservedAgent,
        context: &AgentSessionCtx,
        policy: CaptureSnapshotPolicy,
        source_capability: Option<
            &dyn crate::internal::ai::observed_agents::live_capture::LiveTranscriptSource,
        >,
    ) -> CaptureSnapshot {
        if deadline_elapsed(policy) {
            return CaptureSnapshot::partial(CaptureSnapshotPartialReason::DeadlineExceeded, None);
        }
        if source_capability.is_some() && policy.deadline().is_none() {
            return CaptureSnapshot::partial(
                if context.transcript_path.is_none() {
                    CaptureSnapshotPartialReason::SourceAbsent
                } else {
                    CaptureSnapshotPartialReason::SourceAuthorizationMismatch
                },
                None,
            );
        }
        let Some(deadline) = policy.deadline() else {
            return Self::capture_live(adapter, context, policy);
        };
        if adapter.provider_kind().as_db_str() != "claude_code" && source_capability.is_none() {
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
            Ok(TranscriptSourceResolution::Authorized(TranscriptSource::Bytes {
                mut bytes,
                ..
            })) => {
                bytes.zeroize();
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

        let result = if let Some(capability) = source_capability {
            capability
                .read_source(
                    source,
                    &context.provider_session_id,
                    &context.working_dir,
                    policy.max_bytes,
                    deadline,
                )
                .await
        } else {
            read_live_claude_source_until(source, policy.max_bytes, deadline).await
        };
        match result {
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
            LiveClaudeSourceRead::Untrusted => CaptureSnapshot::partial(
                if source_capability.is_some() {
                    CaptureSnapshotPartialReason::SourceAuthorizationMismatch
                } else {
                    CaptureSnapshotPartialReason::SourceUntrusted
                },
                None,
            ),
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
        Self::capture_authorized_framed(
            source,
            agent_kind,
            libra_session_id,
            policy,
            &policy.deadline(),
        )
    }

    /// [`Self::capture_authorized`] with the deadline the Pi worker frame
    /// checks at each of its steps (PI-33). Every other stage keeps the
    /// policy deadline.
    fn capture_authorized_framed(
        mut source: TranscriptSource,
        agent_kind: &str,
        libra_session_id: &str,
        policy: CaptureSnapshotPolicy,
        pi_frame_deadline: &dyn PiDeadline,
    ) -> CaptureSnapshot {
        let Some(kind) = AgentKind::from_db_str(agent_kind) else {
            wipe_in_memory_source(&mut source);
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
                None,
            );
        };
        Self::capture_authorized_with_reasoning_context_framed(
            source,
            crate::internal::ai::observed_agents::live_capture::selected_snapshot_adapter(kind),
            AuthorizedReasoningSourceContext::ordinary(
                agent_kind,
                libra_session_id,
                AuthorizedReasoningSourceOrigin::LegacyOrdinary,
            ),
            policy,
            pi_frame_deadline,
        )
    }

    pub(crate) fn capture_authorized_with_reasoning_context(
        source: TranscriptSource,
        adapter: &dyn ObservedAgent,
        context: AuthorizedReasoningSourceContext,
        policy: CaptureSnapshotPolicy,
    ) -> CaptureSnapshot {
        Self::capture_authorized_with_reasoning_context_framed(
            source,
            adapter,
            context,
            policy,
            &policy.deadline(),
        )
    }

    fn capture_authorized_with_reasoning_context_framed(
        mut source: TranscriptSource,
        adapter: &dyn ObservedAgent,
        context: AuthorizedReasoningSourceContext,
        policy: CaptureSnapshotPolicy,
        pi_frame_deadline: &dyn PiDeadline,
    ) -> CaptureSnapshot {
        let mut source_projection = source_projection(&source);
        if adapter.provider_kind().as_db_str() != context.provider {
            wipe_in_memory_source(&mut source);
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
                Some(source_projection),
            );
        }
        if deadline_elapsed(policy) {
            wipe_in_memory_source(&mut source);
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::DeadlineExceeded,
                Some(source_projection),
            );
        }

        let source_len = match source.authorized_len() {
            Ok(length) => length,
            Err(_) => {
                wipe_in_memory_source(&mut source);
                return CaptureSnapshot::partial(
                    CaptureSnapshotPartialReason::SourceReadError,
                    Some(source_projection),
                );
            }
        };
        source_projection.byte_len = Some(source_len);
        if source_len > policy.max_bytes {
            wipe_in_memory_source(&mut source);
            return CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::SourceOversize,
                Some(source_projection),
            );
        }

        let bytes = match source {
            TranscriptSource::File { mut file, .. } => file.read_bounded(policy.max_bytes),
            TranscriptSource::Bytes { mut bytes, auth } => {
                if auth.matches(&context.provider, &context.authorized_session, &bytes) {
                    Ok(bytes)
                } else {
                    bytes.zeroize();
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
        Self::capture_authorized_bytes(
            bytes,
            source_projection,
            adapter,
            &context,
            policy,
            pi_frame_deadline,
        )
    }

    fn capture_authorized_bytes(
        bytes: Vec<u8>,
        mut source_projection: CaptureSnapshotSource,
        adapter: &dyn ObservedAgent,
        context: &AuthorizedReasoningSourceContext,
        policy: CaptureSnapshotPolicy,
        pi_frame_deadline: &dyn PiDeadline,
    ) -> CaptureSnapshot {
        let mut bytes = Zeroizing::new(bytes);
        // A descriptor-pinned file may change between its metadata probe and
        // bounded read. Complete durable evidence must describe exactly the
        // bytes that were read and redacted, not the earlier inode length.
        let raw_input_len = bytes.len();
        let raw_bytes = u64::try_from(raw_input_len).unwrap_or(u64::MAX);
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

        let ordinary_preprocessor = adapter.ordinary_source_preprocessor();
        let decoded_report = if let Some(preprocess) = ordinary_preprocessor {
            match preprocess(&bytes, policy.deadline()) {
                Ok((ordinary, report)) => {
                    bytes.zeroize();
                    *bytes = ordinary;
                    report
                }
                Err(error) => {
                    return CaptureSnapshot::partial(
                        if deadline_elapsed(policy) {
                            CaptureSnapshotPartialReason::DeadlineExceeded
                        } else {
                            reasoning_preparation_partial_reason(error)
                        },
                        Some(source_projection),
                    );
                }
            }
        } else {
            RedactionReport::default()
        };
        let prepared = match prepare_authorized_reasoning_source(
            std::mem::take(&mut *bytes),
            adapter,
            context,
            policy,
        ) {
            Ok(prepared) => prepared,
            Err(reason) => {
                let mut snapshot = CaptureSnapshot::partial(reason, Some(source_projection));
                snapshot.reasoning = ReasoningSourceDisposition::Unproven;
                return snapshot;
            }
        };
        let AuthorizedReasoningPreparation {
            ordinary: bytes,
            disposition: reasoning,
            ordinary_partial_permit,
            coverage_fragment,
            classifier_rejected,
            source_partial,
        } = prepared;
        let mut coverage_runtime = CaptureCoverageRuntime {
            turns: None,
            classifier_rejected,
            source_partial,
        };
        // Apply the same output and two-buffer budget as the helper-backed
        // path. A redaction placeholder may expand a short secret, but an
        // unbounded expansion must become a content-free partial here rather
        // than producing a complete snapshot the durable catalog rejects.
        let max_output_bytes =
            usize::try_from(capture_redacted_output_cap(raw_bytes)).unwrap_or(usize::MAX);
        let max_working_set_bytes =
            usize::try_from(capture_redaction_working_set_cap(raw_bytes)).unwrap_or(usize::MAX);
        let source_scan_bytes = (coverage_fragment.is_some() || ordinary_preprocessor.is_some())
            .then_some(raw_input_len);
        let redacted = if let Some(fragment) = coverage_fragment {
            prepare_authorized_reasoning_coverage(
                bytes,
                fragment,
                CoverageRedactionLimits {
                    max_source_bytes: policy.max_bytes,
                    max_output_bytes,
                    max_working_set_bytes,
                    deadline: policy.deadline(),
                },
            )
            .map(|prepared| {
                coverage_runtime.source_partial |= prepared.partial;
                coverage_runtime.classifier_rejected |= prepared.classifier_rejected;
                if !prepared.classifier_rejected {
                    coverage_runtime.turns = Some(prepared.turns);
                }
                (prepared.transcript, prepared.report)
            })
            .map_err(reasoning_preparation_partial_reason)
        } else {
            Redactor::new_default()
                .redact_owned_bounded(bytes, max_output_bytes, max_working_set_bytes)
                .ok_or(CaptureSnapshotPartialReason::SourceReadError)
        };
        let (transcript_redacted, mut redaction_report) = match redacted {
            Ok(redacted) => redacted,
            Err(reason) => {
                let mut snapshot = CaptureSnapshot::partial(reason, Some(source_projection));
                snapshot.reasoning = reasoning;
                coverage_runtime.source_partial = true;
                snapshot.coverage_runtime = coverage_runtime;
                return snapshot;
            }
        };
        crate::internal::ai::observed_agents::coverage::merge_redaction_reports(
            &mut redaction_report,
            decoded_report,
        );
        // `source_projection` is durable metadata and is later bound into
        // finalizer receipts. Commit only the redacted representation: a raw
        // SHA-256 would let an observer test a guessed secret offline.
        source_projection.digest_sha256 = Some(digest_hex(transcript_redacted.bytes()));
        if deadline_elapsed(policy) {
            let mut snapshot = CaptureSnapshot::partial(
                CaptureSnapshotPartialReason::DeadlineExceeded,
                Some(source_projection),
            );
            snapshot.reasoning = reasoning;
            coverage_runtime.turns = None;
            coverage_runtime.source_partial = true;
            snapshot.coverage_runtime = coverage_runtime;
            return snapshot;
        }
        let mut snapshot =
            CaptureSnapshot::complete(source_projection, transcript_redacted, redaction_report);
        snapshot.source_scan_bytes = source_scan_bytes;
        if !matches!(
            reasoning,
            ReasoningSourceDisposition::Ordinary | ReasoningSourceDisposition::NativeProven(_)
        ) || coverage_runtime.source_partial
        {
            snapshot.completeness = CaptureSnapshotCompleteness::Partial;
            snapshot.partial_reason =
                Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch);
        }
        snapshot.reasoning = reasoning;
        snapshot.coverage_runtime = coverage_runtime;
        if snapshot
            .source
            .as_ref()
            .is_some_and(|source| source.kind == CaptureSnapshotSourceKind::TrustedExport)
            && matches!(
                context.origin,
                AuthorizedReasoningSourceOrigin::AuthorizedExport
            )
        {
            snapshot.ordinary_partial_permit = ordinary_partial_permit;
        }
        if context.provider == "hermes"
            && snapshot.completeness == CaptureSnapshotCompleteness::Complete
        {
            apply_hermes_worker_frame(&mut snapshot, policy.deadline());
        }
        if context.provider == "pi"
            && snapshot.completeness == CaptureSnapshotCompleteness::Complete
        {
            apply_pi_worker_frame(&mut snapshot, pi_frame_deadline);
        }
        snapshot
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

fn apply_hermes_worker_frame(snapshot: &mut CaptureSnapshot, deadline: Option<Instant>) {
    use crate::internal::ai::observed_agents::builtin::hermes::{
        HermesWorkerError, hermes_worker_frame_authorized,
    };
    let bytes = snapshot
        .transcript_redacted
        .as_ref()
        .map(|transcript| transcript.bytes())
        .unwrap_or(b"");
    match hermes_worker_frame_authorized(bytes, deadline) {
        Ok(_) => {
            snapshot.completeness = CaptureSnapshotCompleteness::Partial;
            snapshot.partial_reason = Some(CaptureSnapshotPartialReason::UnprovenOrdinary);
        }
        Err(error) => {
            // A refused frame must not remain a readable transcript. Keep the
            // transient source digest so the import parent can still name the
            // failure; the bytes and redaction counts stay absent.
            let reason = match error {
                HermesWorkerError::Budget { .. } => CaptureSnapshotPartialReason::SourceOversize,
                HermesWorkerError::Cancelled => CaptureSnapshotPartialReason::DeadlineExceeded,
                HermesWorkerError::BadFrame => CaptureSnapshotPartialReason::SourceReadError,
            };
            snapshot.completeness = CaptureSnapshotCompleteness::Partial;
            snapshot.partial_reason = Some(reason);
            snapshot.transcript_redacted = None;
            snapshot.redaction_report = RedactionReport::default();
            snapshot.source_scan_bytes = None;
            snapshot.coverage_runtime.turns = None;
        }
    }
}

/// Capture one already-read Hermes export through the ordinary snapshot owner.
pub fn capture_authorized_hermes_bytes(bytes: Vec<u8>, session_id: &str) -> CaptureSnapshot {
    let auth =
        crate::internal::ai::observed_agents::ExportAuthorized::issue("hermes", session_id, &bytes);
    CaptureSnapshotService::capture_authorized(
        crate::internal::ai::observed_agents::TranscriptSource::Bytes { bytes, auth },
        "hermes",
        session_id,
        CaptureSnapshotPolicy::default(),
    )
}

/// Bound the complete redacted body of a `pi` snapshot by the PI-32 worker
/// frame (PI-33). It reads only the bytes the snapshot already holds: the
/// source was read, and the session's lease taken, by the existing owners
/// before this point. An accepted frame keeps the body, thinking and its
/// associated values included, as unproven and partial; a refused frame
/// keeps nothing derived from the transcript.
fn apply_pi_worker_frame(snapshot: &mut CaptureSnapshot, deadline: &dyn PiDeadline) {
    use crate::internal::ai::observed_agents::builtin::pi::{
        PiWorkerError, pi_worker_frame_with_deadline,
    };
    let bytes = snapshot
        .transcript_redacted
        .as_ref()
        .map(|transcript| transcript.bytes())
        .unwrap_or(b"");
    match pi_worker_frame_with_deadline(bytes, deadline) {
        Ok(_) => {
            snapshot.completeness = CaptureSnapshotCompleteness::Partial;
            snapshot.partial_reason = Some(CaptureSnapshotPartialReason::UnprovenOrdinary);
        }
        Err(error) => {
            // As for Hermes: keep the transient source digest so the import
            // parent can still name the failure; the bytes, redaction
            // counts, scan bytes, and coverage turns stay absent.
            let reason = match error {
                PiWorkerError::Budget => CaptureSnapshotPartialReason::SourceOversize,
                PiWorkerError::Cancelled => CaptureSnapshotPartialReason::DeadlineExceeded,
                PiWorkerError::BadFrame => CaptureSnapshotPartialReason::SourceReadError,
            };
            snapshot.completeness = CaptureSnapshotCompleteness::Partial;
            snapshot.partial_reason = Some(reason);
            snapshot.transcript_redacted = None;
            snapshot.redaction_report = RedactionReport::default();
            snapshot.source_scan_bytes = None;
            snapshot.coverage_runtime.turns = None;
        }
    }
}

/// Capture one already-read Pi session through the ordinary snapshot owner
/// (PI-33). The bytes are redacted, then bounded by the Pi worker frame.
pub fn capture_authorized_pi_bytes(bytes: Vec<u8>, session_id: &str) -> CaptureSnapshot {
    let policy = CaptureSnapshotPolicy::default();
    capture_authorized_pi_bytes_until(bytes, session_id, &policy.deadline())
}

/// [`capture_authorized_pi_bytes`] with the deadline the worker frame
/// checks at each of its steps. The stages before the frame run under the
/// default policy, which has no deadline.
pub fn capture_authorized_pi_bytes_until(
    bytes: Vec<u8>,
    session_id: &str,
    deadline: &dyn PiDeadline,
) -> CaptureSnapshot {
    let auth =
        crate::internal::ai::observed_agents::ExportAuthorized::issue("pi", session_id, &bytes);
    CaptureSnapshotService::capture_authorized_framed(
        TranscriptSource::Bytes { bytes, auth },
        "pi",
        session_id,
        CaptureSnapshotPolicy::default(),
        deadline,
    )
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
pub(crate) fn carrier_test_snapshot(
    adapter: &dyn ObservedAgent,
    bytes: Vec<u8>,
) -> CaptureSnapshot {
    use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
    CaptureSnapshotService::capture_authorized_with_reasoning_context(
        TranscriptSource::Bytes {
            auth: crate::internal::ai::observed_agents::ExportAuthorized::issue(
                "claude_code",
                fixture::CAPTURE,
                &bytes,
            ),
            bytes,
        },
        adapter,
        AuthorizedReasoningSourceContext::authorized_export(
            "claude_code",
            fixture::CAPTURE,
            fixture::SOURCE,
        ),
        CaptureSnapshotPolicy::default(),
    )
}

/// Deterministic source seam only: no exporter, CLI, hook or Session runtime.
#[cfg(test)]
#[cfg(unix)]
pub(crate) fn verified_partial_fixture_snapshot(
    capture_session: &str,
    provider_session: &str,
    directory: &Path,
) -> CaptureSnapshot {
    use crate::internal::ai::observed_agents::{ExportAuthorized, agent_for};
    let bytes = serde_json::to_vec(&serde_json::json!({
        "info": {"id":provider_session,"location":{"directory":directory.to_string_lossy()}},
        "messages": [
            {"id":"msg_fixture_user","type":"user","text":"safe ordinary request"},
            {"id":"msg_fixture_assistant","type":"assistant","model":{"id":"fixture"},
             "tokens":{"input":1,"output":1,"reasoning":0,"cache":{"read":0,"write":0}},
             "content":[
                 {"type":"reasoning","text":"PRIVATE_ASSOCIATED_REASONING_CANARY","time":{"start":1,"end":2},"state":{"reasoningField":"PRIVATE_STATE_CANARY"}},
                 {"type":"text","text":"safe ordinary reply"}
             ]}
        ]
    })).expect("encode deterministic partial source");
    CaptureSnapshotService::capture_authorized_with_reasoning_context(
        TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", capture_session, &bytes),
            bytes,
        },
        agent_for(AgentKind::OpenCode),
        AuthorizedReasoningSourceContext::authorized_export(
            "opencode",
            capture_session,
            provider_session,
        ),
        CaptureSnapshotPolicy::with_deadline(None),
    )
}

/// Isolated Rust storage fixture; no cwd/env mutation or external process.
#[cfg(all(test, unix))]
pub(crate) async fn verified_partial_key_fixture() -> (
    tempfile::TempDir,
    std::path::PathBuf,
    sea_orm::DatabaseConnection,
    CaptureScope,
) {
    let dir = tempfile::tempdir().expect("private partial repository");
    let storage = dir.path().join(crate::utils::util::ROOT_DIR);
    std::fs::create_dir_all(storage.join("objects")).expect("private partial object store");
    let conn = crate::internal::db::create_database(
        storage
            .join(crate::utils::util::DATABASE)
            .to_str()
            .expect("UTF-8 partial fixture path"),
    )
    .await
    .expect("private partial database");
    crate::internal::config::ConfigKv::set_with_conn(
        &conn,
        "libra.repoid",
        "verified-partial-test-repo",
        false,
    )
    .await
    .expect("private partial identity");
    let scope = CaptureScope::resolve(&conn, dir.path())
        .await
        .expect("private partial scope");
    (dir, storage, conn, scope)
}

#[cfg(test)]
mod tests {

    /// PI-33 AC4, for the fields no public accessor shows: a refused Pi
    /// frame also drops the scan bytes and the coverage turns.
    #[test]
    fn pi_bounded_capture_refused_frame_clears_coverage_turns() {
        let (transcript, report) =
            Redactor::new_default().redact(b"not a pi session AKIAIOSFODNN7EXAMPLE");
        assert!(report.match_count() > 0);
        let mut snapshot = CaptureSnapshot::complete(
            CaptureSnapshotSource {
                kind: CaptureSnapshotSourceKind::TrustedExport,
                identity: SOURCE_IDENTITY_NOT_RETAINED.to_string(),
                digest_sha256: None,
                byte_len: Some(37),
            },
            transcript,
            report,
        );
        snapshot.source_scan_bytes = Some(37);
        snapshot.coverage_runtime.turns = Some(Vec::new());
        apply_pi_worker_frame(&mut snapshot, &None::<Instant>);
        assert_eq!(
            snapshot.partial_reason,
            Some(CaptureSnapshotPartialReason::SourceReadError)
        );
        assert!(snapshot.transcript_redacted.is_none());
        assert_eq!(snapshot.redaction_report.match_count(), 0);
        assert!(snapshot.source_scan_bytes.is_none());
        assert!(snapshot.coverage_runtime.turns.is_none());
    }

    #[test]
    fn cov2_preview_preserves_order_and_fails_closed_on_budget() {
        let native = serde_json::json!({"info":{"id":"ses_order"},"messages":[
            {"id":"u","type":"user","text":"go"},
            {"id":"a","type":"assistant","content":[
                {"id":"b","type":"text","text":"before"},
                {"id":"r","type":"reasoning","text":"visible thought"},
                {"id":"t","type":"tool","name":"read","state":{
                    "status":"completed","input":{},"content":[{"type":"text","text":"ok"}]}},
                {"id":"z","type":"text","text":"after"}]},
            {"id":"i","type":"idle","outcome":"succeeded"}]});
        let classic = serde_json::json!({"info":{"id":"ses_order"},"messages":[
            {"info":{"id":"u","role":"user"},"parts":[{"id":"up","type":"text","text":"go"}]},
            {"info":{"id":"a","role":"assistant"},"parts":[
                {"id":"b","type":"text","text":"before"},
                {"id":"r","type":"reasoning","text":"visible thought"},
                {"id":"t","type":"tool","tool":"read","state":{
                    "status":"completed","input":{},"output":"ok"}},
                {"id":"z","type":"text","text":"after"}]}]});
        for source in [native.clone(), classic] {
            let raw = serde_json::to_vec(&source).expect("synthetic ordered export");
            let (_, preview) = CaptureSnapshotService::preview_redacted_coverage_v2(
                "opencode",
                raw.clone(),
                CaptureSnapshotPolicy::default(),
            )
            .expect("ordered pure-memory preview");
            assert_eq!(preview["partial"], false);
            assert_eq!(preview["turns"][0]["coverage_schema_version"], 2);
            let records = preview["turns"][0]["projection"]["records"]
                .as_array()
                .expect("actual records");
            let roles: Vec<_> = records
                .iter()
                .map(|row| row["type"].as_str().expect("record tag"))
                .collect();
            assert_eq!(
                roles,
                [
                    "user",
                    "assistant",
                    "reasoning",
                    "tool_call",
                    "tool_result",
                    "assistant"
                ]
            );
            assert_eq!(records[1]["text"], "before");
            assert_eq!(records[2]["text"], "visible thought");
            assert_eq!(records[5]["text"], "after");
            assert!(matches!(
                CaptureSnapshotService::preview_redacted_coverage_v2(
                    "opencode",
                    raw.clone(),
                    CaptureSnapshotPolicy::with_max_bytes(None, raw.len() as u64 - 1),
                ),
                Err(CaptureSnapshotPartialReason::SourceOversize)
            ));
            assert!(matches!(
                CaptureSnapshotService::preview_redacted_coverage_v2(
                    "opencode",
                    raw,
                    CaptureSnapshotPolicy::with_deadline(Some(
                        Instant::now() - Duration::from_secs(1)
                    )),
                ),
                Err(CaptureSnapshotPartialReason::DeadlineExceeded)
            ));
        }
        let mut unsettled = native;
        unsettled["messages"]
            .as_array_mut()
            .expect("messages")
            .pop();
        let (_, preview) = CaptureSnapshotService::preview_redacted_coverage_v2(
            "opencode",
            serde_json::to_vec(&unsettled).expect("unsettled export"),
            CaptureSnapshotPolicy::default(),
        )
        .expect("unsettled pure-memory preview");
        assert_eq!(
            preview["partial"], true,
            "visible thought cannot fabricate idle"
        );
        assert_eq!(
            preview["turns"][0]["projection"]["completeness"],
            "incomplete"
        );
    }

    #[test]
    fn cov2_visible_text_redacted_before_digest_and_projection() {
        use sha2::{Digest, Sha256};

        use crate::internal::ai::observed_agents::coverage::{
            encode_coverage_turn, safe_turn_projection_for_schema,
        };
        const KEY: &str = "sk-ant-EXAMPLE0000000000000000";
        const EXPECTED: &str = r#"[{"role":"user","text":"hi"},{"provider":"opencode","role":"reasoning","source_kind":"reasoning","text":"before <REDACTED:anthropic-api-key> after"},{"role":"assistant","text":"hello"}]"#;
        let source =
            serde_json::to_string(&serde_json::json!({"info":{"id":"ses_cov10"},"messages":[
            {"id":"u","type":"user","text":"hi"},
            {"id":"a","type":"assistant","content":[
                {"id":"p","type":"reasoning","text":format!("before {KEY} after")},
                {"id":"t","type":"text","text":"hello"}]},
            {"id":"i","type":"idle","outcome":"succeeded"}]}))
            .expect("synthetic export");
        for raw in [source.clone(), source.replace("sk-ant-", "sk-\\u0061nt-")] {
            let prepared = prepare_redacted_coverage_preview(
                "opencode",
                raw.clone().into_bytes(),
                CaptureSnapshotPolicy::default(),
            )
            .expect("redacted memory preparation");
            assert!(!prepared.partial);
            assert_eq!(prepared.turns.len(), 1);
            assert_eq!(
                prepared.turns[0].completeness,
                crate::internal::ai::observed_agents::Completeness::Complete
            );
            let encoded = encode_coverage_turn(&prepared.turns[0]).expect("redacted typed encoder");
            assert_eq!(encoded.schema_version.as_i64(), 2);
            assert_eq!(encoded.canonical_bytes, EXPECTED.as_bytes());
            assert_eq!(
                encoded.digest_hex,
                hex::encode(Sha256::digest(EXPECTED.as_bytes()))
            );
            let projection = safe_turn_projection_for_schema(2, "opencode", &prepared.turns[0])
                .expect("safe v2 projection");
            assert!(!projection.to_string().contains(KEY));
            let ordinary: serde_json::Value = serde_json::from_slice(prepared.transcript.bytes())
                .expect("ordinary source still valid JSON");
            assert_eq!(
                ordinary["messages"][1]["content"][0]["text"],
                "before <REDACTED:anthropic-api-key> after"
            );
            assert!(!ordinary.to_string().contains(KEY));
            assert!(prepared.report.match_count() >= 1);
            let (bytes, preview) = CaptureSnapshotService::preview_redacted_coverage_v2(
                "opencode",
                raw.into_bytes(),
                CaptureSnapshotPolicy::default(),
            )
            .expect("public safe preview");
            assert_eq!(preview["schema_version"], 1);
            assert_eq!(preview["partial"], false);
            assert_eq!(preview["turns"][0]["coverage_schema_version"], 2);
            assert_eq!(
                preview["turns"][0]["digest"],
                hex::encode(Sha256::digest(EXPECTED.as_bytes()))
            );
            assert_eq!(bytes.bytes(), prepared.transcript.bytes());
        }
    }

    use std::{
        path::{Path, PathBuf},
        time::Duration,
    };

    use serial_test::serial;

    use super::*;
    use crate::utils::json::normalize_json_value;

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
        // Generic snapshot contract: this plain text is not an OpenCode export.
        let bytes = b"redacted source commitment fixture".to_vec();
        let snapshot = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("claude_code", "session-marker", &bytes),
            bytes,
        };
        let mut snapshot = CaptureSnapshotService::capture_authorized(
            snapshot,
            "claude_code",
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
        // Generic digest contract over JSON without provider-specific shape.
        let secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB";
        let raw_bytes = format!("{{\"token\":\"{secret}\"}}\n").into_bytes();
        let raw_digest = digest_hex(&raw_bytes);
        let source = TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("claude_code", "libra-session", &raw_bytes),
            bytes: raw_bytes,
        };

        let snapshot = CaptureSnapshotService::capture_authorized(
            source,
            "claude_code",
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
            &ClaudeCodeObservedAgent::new(),
            &AuthorizedReasoningSourceContext::ordinary(
                "claude_code",
                "libra-session",
                AuthorizedReasoningSourceOrigin::LegacyOrdinary,
            ),
            CaptureSnapshotPolicy::default(),
            &None::<Instant>,
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
        // Generic identity contract over JSON without provider-specific shape.
        let first_secret = "ghp_abcdefghijklmnopqrstuvwxyz0123456789AB";
        let second_secret = "ghp_ZYXWVUTSRQPONMLKJIHGFEDCBA9876543210CD";
        let first_raw = format!("{{\"token\":\"{first_secret}\"}}\n").into_bytes();
        let second_raw = format!("{{\"token\":\"{second_secret}\"}}\n").into_bytes();
        assert_ne!(first_raw, second_raw);

        let capture_export = |bytes: Vec<u8>| {
            let auth = ExportAuthorized::issue("claude_code", "libra-session", &bytes);
            CaptureSnapshotService::capture_authorized(
                TranscriptSource::Bytes { auth, bytes },
                "claude_code",
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
            auth: ExportAuthorized::issue("claude_code", "libra-session", &bytes),
            bytes,
        };

        let snapshot = CaptureSnapshotService::capture_authorized(
            source,
            AgentKind::ClaudeCode.as_db_str(),
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
    struct ReasoningTestAdapter {
        enabled: bool,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl ObservedAgent for ReasoningTestAdapter {
        fn provider_kind(&self) -> AgentKind {
            AgentKind::ClaudeCode
        }
        fn provider_name(&self) -> &'static str {
            "claude-code"
        }
        fn read_transcript(&self, _: &AgentSessionCtx) -> anyhow::Result<Option<Vec<u8>>> {
            panic!("reasoning fixture must never reopen a provider source")
        }
        fn protected_dirs(&self) -> &'static [&'static str] {
            &[]
        }
        fn as_reasoning_source_preprocessor(
            &self,
        ) -> Option<&dyn crate::internal::ai::observed_agents::reasoning::ReasoningSourcePreprocessor>
        {
            self.enabled.then_some(self)
        }
    }

    impl crate::internal::ai::observed_agents::reasoning::ReasoningSourcePreprocessor
        for ReasoningTestAdapter
    {
        fn prepare_authorized_source(
            &self,
            mut source: Vec<u8>,
        ) -> Result<
            crate::internal::ai::observed_agents::reasoning::PreparedReasoningSource,
            crate::internal::ai::observed_agents::reasoning::ReasoningFieldError,
        > {
            use crate::internal::ai::observed_agents::reasoning::PreparedReasoningSource;
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let candidate = b"OG16_CANDIDATE_PRIVATE_CANARY";
            if let Some(index) = source
                .windows(candidate.len())
                .position(|window| window == candidate)
            {
                source.drain(index..index + candidate.len());
                Ok(PreparedReasoningSource::unproven_candidates(source))
            } else {
                Ok(PreparedReasoningSource::without_candidates(source))
            }
        }
    }

    fn reasoning_fixture_snapshot(enabled: bool, bytes: Vec<u8>) -> CaptureSnapshot {
        let auth = ExportAuthorized::issue("claude_code", "capture-synthetic", &bytes);
        let adapter = ReasoningTestAdapter {
            enabled,
            calls: Default::default(),
        };
        CaptureSnapshotService::capture_authorized_with_reasoning_context(
            TranscriptSource::Bytes { bytes, auth },
            &adapter,
            AuthorizedReasoningSourceContext::authorized_export(
                "claude_code",
                "capture-synthetic",
                "source-synthetic",
            ),
            CaptureSnapshotPolicy::default(),
        )
    }

    #[test]
    fn reasoning_none_snapshot_bytes_unchanged() {
        let bytes = b"ordinary transcript has no candidate\n".to_vec();
        for enabled in [false, true] {
            let snapshot = reasoning_fixture_snapshot(enabled, bytes.clone());
            assert_eq!(
                snapshot.completeness(),
                CaptureSnapshotCompleteness::Complete
            );
            assert_eq!(
                snapshot
                    .try_into_ordinary()
                    .expect("ordinary owning conversion")
                    .expect("transcript")
                    .bytes(),
                &bytes
            );
        }
    }

    #[test]
    fn reasoning_candidate_removed_before_redactor() {
        let bytes = b"ordinary OG16_CANDIDATE_PRIVATE_CANARY text".to_vec();
        let snapshot = reasoning_fixture_snapshot(true, bytes);
        let projection = snapshot.safe_projection();
        assert_eq!(projection.redaction_bytes_scanned, b"ordinary  text".len());
        assert_eq!(
            snapshot
                .transcript()
                .expect("sanitized ordinary component")
                .bytes(),
            b"ordinary  text"
        );
        assert_eq!(
            projection.partial_reason,
            Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
        );
        let rejected = snapshot
            .try_into_ordinary()
            .expect_err("zero-artifact partial cannot become ordinary");
        assert!(!format!("{rejected:?}").contains("OG16_CANDIDATE_PRIVATE_CANARY"));
    }

    #[test]
    fn reasoning_carrier_source_bytes_are_bounded() {
        let bytes = b"OG16_CANDIDATE_PRIVATE_CANARY".to_vec();
        let auth = ExportAuthorized::issue("claude_code", "capture-synthetic", &bytes);
        let adapter = ReasoningTestAdapter {
            enabled: true,
            calls: Default::default(),
        };
        let snapshot = CaptureSnapshotService::capture_authorized_with_reasoning_context(
            TranscriptSource::Bytes { bytes, auth },
            &adapter,
            AuthorizedReasoningSourceContext::authorized_export(
                "claude_code",
                "capture-synthetic",
                "source-synthetic",
            ),
            CaptureSnapshotPolicy::with_max_bytes(None, 1),
        );
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceOversize)
        );
        assert_eq!(adapter.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(snapshot.transcript().is_none());
    }

    #[tokio::test]
    async fn reasoning_enabled_claude_deadline_rejects_before_source_resolution() {
        let adapter = ReasoningTestAdapter {
            enabled: true,
            calls: Default::default(),
        };
        let snapshot = CaptureSnapshotService::capture_live_until(
            &adapter,
            &context(None),
            CaptureSnapshotPolicy::with_deadline(Some(Instant::now())),
        )
        .await;
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
        );
        assert_eq!(adapter.calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        assert!(snapshot.try_into_ordinary().is_err());
    }
    #[test]
    fn reasoning_visible_text_is_typed_redacted() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
        let secret = "AKIAABCDEFGHIJKLMNOP";
        let bytes = format!("{{\"thinking\":\"visible {secret}\"}}").into_bytes();
        let snapshot = carrier_test_snapshot(&fixture::STRIP, bytes);
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        assert_eq!(snapshot.redaction_report().match_count(), 1);
        let typed = snapshot.try_into_ordinary().unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(typed.bytes()).unwrap();
        assert!(value["thinking"].as_str().unwrap().starts_with("visible "));
        assert!(!String::from_utf8_lossy(typed.bytes()).contains(secret));
        use crate::internal::ai::observed_agents::{
            coverage::{
                Completeness, NormalizedTurn, SemanticRecord, redact_turns_with_report,
                safe_turn_projection,
            },
            reasoning::{
                ReasoningProvider, ReasoningSourceKind, classify_provider_visible_reasoning,
            },
        };
        let (record, text) = classify_provider_visible_reasoning(
            ReasoningProvider::ClaudeCode,
            ReasoningSourceKind::Thinking,
            value["thinking"].as_str().unwrap().to_owned(),
        );
        assert_eq!(
            record.availability(),
            crate::internal::ai::observed_agents::reasoning::ReasoningAvailability::ProviderVisible
        );
        let mut turn = NormalizedTurn {
            logical_turn_key: "visible-turn".to_owned(),
            ordinal: 0,
            completeness: Completeness::Complete,
            started_at: None,
            ended_at: None,
            records: vec![SemanticRecord::Reasoning {
                provider: ReasoningProvider::ClaudeCode,
                source_kind: Some(ReasoningSourceKind::Thinking),
                text,
            }],
        };
        let report = redact_turns_with_report(std::slice::from_mut(&mut turn));
        assert_eq!(
            report.match_count(),
            0,
            "the source snapshot already redacted the visible field"
        );
        let projection = safe_turn_projection("claude_code", &turn);
        assert_eq!(projection["records"][0]["type"], "reasoning");
        assert!(!projection.to_string().contains(secret));
    }

    #[tokio::test]
    #[serial(env)]
    async fn reasoning_missing_proof_preserves_partial() {
        use crate::internal::ai::{
            capture::live_checkpoint::{
                attach_subagent_snapshot_metadata, snapshot_subagent_sources_for_extraction,
            },
            observed_agents::{
                live_capture::scoped_test_snapshot_adapter,
                reasoning::carrier_test_support as fixture,
            },
        };
        crate::internal::ai::capture::live_checkpoint::tests::assert_no_deadline_reasoning_handoff(
        )
        .await;
        let bytes = [b"ordinary ".as_slice(), fixture::CANARY].concat();
        for adapter in [&fixture::STRIP, &fixture::MALFORMED] {
            let snapshot = carrier_test_snapshot(adapter, bytes.clone());
            assert_eq!(
                snapshot.partial_reason(),
                Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
            );
            assert!(snapshot.try_into_ordinary().is_err());
            let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, adapter);
            let public = CaptureSnapshotService::capture_authorized(
                TranscriptSource::Bytes {
                    auth: ExportAuthorized::issue("claude_code", fixture::CAPTURE, &bytes),
                    bytes: bytes.clone(),
                },
                "claude_code",
                fixture::CAPTURE,
                Default::default(),
            );
            assert_eq!(
                public.partial_reason(),
                Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
            );
            assert!(!public.reasoning.is_ordinary());
            assert!(!matches!(
                public.reasoning,
                ReasoningSourceDisposition::NativeProven(_)
            ));
            assert!(public.try_into_ordinary().is_err());
            let child = crate::internal::ai::subagent_content::DiscoveredSubagentContent::fixture(
                "claude_code",
                "synthetic-child-key",
                &bytes,
                None,
            );
            let snapshots = snapshot_subagent_sources_for_extraction(
                vec![child],
                fixture::CAPTURE,
                Default::default(),
            );
            assert!(snapshots.transcripts.is_empty());
            assert_eq!(snapshots.partial_source_count(), 1);
            let mut metadata = serde_json::json!({});
            attach_subagent_snapshot_metadata(&mut metadata, &snapshots);
            assert_eq!(
                metadata["subagent_snapshot"]["partial_reasons"],
                serde_json::json!(["source_authorization_mismatch"])
            );
            assert_eq!(metadata["subagent_snapshot"]["partial_source_count"], 1);
            assert_eq!(metadata["subagent_snapshot"]["complete_source_count"], 0);
            assert!(
                !metadata
                    .to_string()
                    .contains(std::str::from_utf8(fixture::CANARY).unwrap())
            );
        }
        let snapshot = carrier_test_snapshot(&fixture::NONE, b"old ordinary".to_vec());
        assert!(snapshot.try_into_ordinary().is_ok());
        // A valid native test-only witness cannot supply a missing source
        // association. The public legacy wrapper retains an owning partial.
        let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::NATIVE);
        let snapshot = CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue("claude_code", fixture::CAPTURE, &bytes),
                bytes,
            },
            "claude_code",
            fixture::CAPTURE,
            Default::default(),
        );
        assert!(matches!(
            snapshot.reasoning,
            ReasoningSourceDisposition::Unmapped
        ));
        assert!(snapshot.try_into_ordinary().is_err());
    }

    #[test]
    fn reasoning_carrier_artifact_bytes_are_bounded() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
        let snapshot = carrier_test_snapshot(&fixture::OVERSIZE, b"ordinary".to_vec());
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceOversize)
        );
        assert!(snapshot.transcript().is_none());
        assert!(snapshot.try_into_ordinary().is_err());
        let control = carrier_test_snapshot(&fixture::NATIVE, b"ordinary".to_vec());
        assert!(matches!(
            control.reasoning,
            ReasoningSourceDisposition::NativeProven(_)
        ));
    }

    #[test]
    fn reasoning_carrier_artifact_count_is_bounded() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
        let snapshot = carrier_test_snapshot(&fixture::TOO_MANY, b"ordinary".to_vec());
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
        );
        assert!(snapshot.transcript().is_none());
        assert!(snapshot.try_into_ordinary().is_err());
    }

    fn og26_snapshot(bytes: Vec<u8>, policy: CaptureSnapshotPolicy) -> CaptureSnapshot {
        use crate::internal::ai::observed_agents::{ExportAuthorized, agent_for};

        let capture = "capture__og26_fixture";
        let source = "ses_og26_fixture";
        CaptureSnapshotService::capture_authorized_with_reasoning_context(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue("opencode", capture, &bytes),
                bytes,
            },
            agent_for(AgentKind::OpenCode),
            AuthorizedReasoningSourceContext::authorized_export("opencode", capture, source),
            policy,
        )
    }

    #[test]
    fn cov2_runtime_partial_classifier_keeps_legacy_consumer() {
        let base = serde_json::json!({"info":{"id":"ses_og26_fixture"},"messages":[
            {"id":"u1","type":"user","text":"safe request"},
            {"id":"a1","type":"assistant","content":[
                {"id":"r1","type":"reasoning","text":"safe thought"},
                {"id":"t1","type":"text","text":"safe reply"}
            ]},
            {"id":"i1","type":"idle","outcome":"succeeded"}
        ]});
        for name in [
            "empty reasoning",
            "whitespace reasoning",
            "missing part id",
            "wrong part session",
            "wrong part message",
            "missing assistant id",
        ] {
            let mut document = base.clone();
            match name {
                "empty reasoning" => document["messages"][1]["content"][0]["text"] = "".into(),
                "whitespace reasoning" => {
                    document["messages"][1]["content"][0]["text"] = " \t\n".into();
                }
                "missing part id" => {
                    document["messages"][1]["content"][0]
                        .as_object_mut()
                        .unwrap()
                        .remove("id");
                }
                "wrong part session" => {
                    document["messages"][1]["content"][0]["sessionID"] = "another".into();
                }
                "wrong part message" => {
                    document["messages"][1]["content"][0]["messageID"] = "another".into();
                }
                "missing assistant id" => {
                    document["messages"][1]
                        .as_object_mut()
                        .unwrap()
                        .remove("id");
                }
                _ => unreachable!("closed fixture matrix"),
            }
            let source = serde_json::to_vec(&normalize_json_value(document.clone())).unwrap();
            // The old scrubber removes these auxiliary associations without
            // demoting its Ordinary consumer. Pin those exact ordinary bytes.
            let field = match name {
                "wrong part session" => Some("sessionID"),
                "wrong part message" => Some("messageID"),
                _ => None,
            };
            if let Some(field) = field {
                document["messages"][1]["content"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove(field);
            }
            let legacy_source =
                serde_json::to_vec(&normalize_json_value(document.clone())).unwrap();
            let (legacy, legacy_report) = Redactor::new_default().redact(&legacy_source);
            let mut snapshot = og26_snapshot(source, CaptureSnapshotPolicy::default());
            assert!(
                matches!(snapshot.reasoning, ReasoningSourceDisposition::Ordinary),
                "{name}"
            );
            assert_eq!(
                snapshot.completeness(),
                CaptureSnapshotCompleteness::Complete,
                "{name}"
            );
            assert!(!snapshot.rejects_capture_source_consumer(), "{name}");
            assert!(!snapshot.has_verified_ordinary_partial_permit(), "{name}");
            assert_eq!(snapshot.transcript().unwrap(), &legacy, "{name}");
            assert_eq!(snapshot.redaction_report(), &legacy_report, "{name}");
            let runtime = snapshot.take_coverage_runtime();
            assert!(runtime.turns.is_none(), "{name}");
            assert!(runtime.classifier_rejected, "{name}");
            assert!(!runtime.source_partial, "{name}");
            assert!(snapshot.try_into_ordinary().is_ok(), "{name}");
        }
    }

    #[test]
    fn cov2_runtime_partial_classifier_keeps_unproven_source_routes() {
        let base = serde_json::json!({"info":{"id":"ses_og26_fixture"},"messages":[
            {"id":"u1","type":"user","text":"safe request"},
            {"id":"a1","type":"assistant","content":[
                {"id":"r1","type":"reasoning","text":"safe thought"},
                {"id":"t1","type":"text","text":"safe reply"}
            ]}
        ]});
        // These are already Unproven in the old scrubber, unlike missing IDs
        // and empty visible text. Rejection must never promote them to V1.
        for name in [
            "duplicate part id",
            "duplicate assistant id",
            "missing content sibling",
            "classic invalid tokens",
        ] {
            let mut document = base.clone();
            match name {
                "duplicate part id" => document["messages"][1]["content"][1]["id"] = "r1".into(),
                "duplicate assistant id" => document["messages"].as_array_mut().unwrap().push(
                    serde_json::json!({
                        "id":"a1","type":"assistant","content":[{"type":"text","text":"sibling"}]
                    }),
                ),
                "missing content sibling" => {
                    document["messages"]
                        .as_array_mut()
                        .unwrap()
                        .push(serde_json::json!({
                            "id":"a2","type":"assistant"
                        }))
                }
                "classic invalid tokens" => {
                    document["messages"] = serde_json::json!([
                        {"info":{"id":"u1","role":"user"},"parts":[{"type":"text","text":"safe request"}]},
                        {"info":{"id":"a1","role":"assistant","tokens":{"input":"PRIVATE_TOKEN_CANARY"}},"parts":[
                            {"id":"r1","type":"reasoning","text":"safe thought"},
                            {"id":"t1","type":"text","text":"safe reply"}
                        ]}
                    ])
                }
                _ => unreachable!("closed fixture matrix"),
            }
            let source = serde_json::to_vec(&normalize_json_value(document.clone())).unwrap();
            let mut snapshot = og26_snapshot(source, CaptureSnapshotPolicy::default());
            assert!(
                matches!(snapshot.reasoning, ReasoningSourceDisposition::Unproven),
                "{name}"
            );
            assert_eq!(
                snapshot.completeness(),
                CaptureSnapshotCompleteness::Partial,
                "{name}"
            );
            assert!(snapshot.rejects_capture_source_consumer(), "{name}");
            let has_permit = name != "classic invalid tokens";
            assert_eq!(
                snapshot.has_verified_ordinary_partial_permit(),
                has_permit,
                "{name}"
            );
            assert!(
                !String::from_utf8_lossy(snapshot.transcript().unwrap().bytes())
                    .contains("PRIVATE_TOKEN_CANARY"),
                "{name}"
            );
            let runtime = snapshot.take_coverage_runtime();
            assert!(!runtime.classifier_rejected, "{name}");
            assert!(runtime.source_partial, "{name}");
            assert_eq!(
                snapshot.has_verified_ordinary_partial_permit(),
                has_permit,
                "{name}"
            );
            assert!(snapshot.try_into_ordinary().is_err(), "{name}");
        }
    }

    #[test]
    fn cov2_runtime_partial_classifier_decoded_match_keeps_legacy_bytes() {
        use crate::internal::ai::observed_agents::coverage::Completeness;

        // The first raw pass cannot match the escaped prefix. Reject only the
        // typed companion: do not re-encode this legacy source or claim to fix
        // the pre-existing escaped-secret redaction contract.
        let source = br#"{"info":{"id":"ses_og26_fixture"},"messages":[{"id":"u1","type":"user","text":"before \u0073k-ant-EXAMPLE0000000000000000 after"},{"id":"a1","type":"assistant","content":[{"id":"r1","type":"reasoning","text":"safe thought"},{"id":"t1","type":"text","text":"safe reply"}]},{"id":"i1","type":"idle","outcome":"succeeded"}]}"#;
        let preview = prepare_redacted_coverage_preview(
            "opencode",
            source.to_vec(),
            CaptureSnapshotPolicy::default(),
        )
        .expect("data-only preview remains partial");
        assert!(preview.partial);
        assert!(
            preview
                .turns
                .iter()
                .all(|turn| turn.completeness == Completeness::Incomplete)
        );

        let (legacy, legacy_report) = Redactor::new_default().redact(source);
        assert_eq!(
            legacy_report.match_count(),
            0,
            "the extra match is decoded-only"
        );
        let mut snapshot = og26_snapshot(source.to_vec(), CaptureSnapshotPolicy::default());
        assert!(matches!(
            snapshot.reasoning,
            ReasoningSourceDisposition::Ordinary
        ));
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        assert!(!snapshot.rejects_capture_source_consumer());
        assert_eq!(snapshot.transcript().unwrap(), &legacy);
        assert_eq!(snapshot.redaction_report(), &legacy_report);
        let expected = digest_hex(legacy.bytes());
        assert_eq!(
            snapshot
                .safe_projection()
                .source
                .unwrap()
                .digest_sha256
                .as_deref(),
            Some(expected.as_str())
        );
        let runtime = snapshot.take_coverage_runtime();
        assert!(runtime.turns.is_none());
        assert!(runtime.classifier_rejected);
        assert!(!runtime.source_partial);
        assert!(snapshot.try_into_ordinary().is_ok());
    }

    #[test]
    fn cov2_runtime_post_redaction_structure_keeps_legacy_capture() {
        use crate::internal::ai::observed_agents::coverage::parse_canon_value;

        let sources = [
            // The raw rule swallows syntax through the next whitespace.
            (false, br#"{"info":{"id":"ses_og26_fixture"},"messages":[{"id":"u1","type":"user","text":"postgres://user:pass@host/db"},{"id":"a1","type":"assistant","content":[{"id":"r1","type":"reasoning","text":"safe thought"}]}]}"#.as_slice()),
            // This layout remains valid JSON but loses the reasoning part's
            // ordinal: the URI's text part absorbs its adjacent sibling.
            (true, br#"{"info":{"id":"ses_og26_fixture"},"messages":[{"id":"u1","type":"user","text":"safe request"},{"id":"a1","type":"assistant","content":[{"id":"t1","type":"text","text":"postgres://user:pass@host/db"},{"id":"r1","type":"reasoning","text":"safe thought"}]}]}"#.as_slice()),
        ];
        for (still_json, source) in sources {
            let (legacy, legacy_report) = Redactor::new_default().redact(source);
            assert!(
                legacy_report
                    .matches
                    .iter()
                    .any(|hit| hit.rule_id == "credential-uri")
            );
            assert_eq!(parse_canon_value(legacy.bytes()).is_ok(), still_json);
            assert!(
                prepare_redacted_coverage_preview(
                    "opencode",
                    source.to_vec(),
                    CaptureSnapshotPolicy::default()
                )
                .is_err(),
                "preview keeps its existing structural-error policy"
            );

            let mut snapshot = og26_snapshot(source.to_vec(), CaptureSnapshotPolicy::default());
            assert!(matches!(
                snapshot.reasoning,
                ReasoningSourceDisposition::Ordinary
            ));
            assert_eq!(
                snapshot.completeness(),
                CaptureSnapshotCompleteness::Complete
            );
            assert!(!snapshot.rejects_capture_source_consumer());
            assert_eq!(snapshot.transcript().unwrap(), &legacy);
            assert_eq!(snapshot.redaction_report(), &legacy_report);
            let expected = digest_hex(legacy.bytes());
            assert_eq!(
                snapshot
                    .safe_projection()
                    .source
                    .unwrap()
                    .digest_sha256
                    .as_deref(),
                Some(expected.as_str())
            );
            let runtime = snapshot.take_coverage_runtime();
            assert!(runtime.turns.is_none());
            assert!(runtime.classifier_rejected);
            assert!(!runtime.source_partial);
            assert!(snapshot.try_into_ordinary().is_ok());
        }
    }

    #[test]
    fn cov2_runtime_post_redaction_shifted_reasoning_identity_is_rejected() {
        use crate::internal::ai::observed_agents::{
            ExportAuthorized, agent_for, coverage::parse_canon_value,
        };

        // A real raw-rule span merges two parts into one still-valid object.
        // This is a coordinate-shift fixture, not a completed false-V2 claim.
        let source = br#"{"info":{"id":"s"},"messages":[{"type":"assistant","id":"m","content":[{"text":"postgres://u:p@h","id":"a","type":"reasoning"},{"text":" B","id":"b","type":"reasoning"}]}]}"#;
        assert_eq!(source.len(), 172);
        let (legacy, legacy_report) = Redactor::new_default().redact(source);
        assert_eq!(legacy_report.matches.len(), 1);
        let hit = &legacy_report.matches[0];
        assert_eq!(
            (hit.rule_id.as_str(), hit.start, hit.end),
            ("credential-uri", 80, 136)
        );
        let document = parse_canon_value(legacy.bytes()).expect("the shifted result still parses");
        let parts = document.get("messages").unwrap().as_array().unwrap()[0]
            .get("content")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(parts.len(), 1);
        assert_eq!(parts[0].get("id").unwrap().as_str(), Some("b"));
        assert_eq!(parts[0].get("type").unwrap().as_str(), Some("reasoning"));
        assert!(
            prepare_redacted_coverage_preview(
                "opencode",
                source.to_vec(),
                CaptureSnapshotPolicy::default()
            )
            .is_err()
        );

        let mut snapshot = CaptureSnapshotService::capture_authorized_with_reasoning_context(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue("opencode", "capture", source),
                bytes: source.to_vec(),
            },
            agent_for(AgentKind::OpenCode),
            AuthorizedReasoningSourceContext::authorized_export("opencode", "capture", "s"),
            CaptureSnapshotPolicy::default(),
        );
        assert!(matches!(
            snapshot.reasoning,
            ReasoningSourceDisposition::Ordinary
        ));
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        assert_eq!(snapshot.transcript().unwrap(), &legacy);
        assert_eq!(snapshot.redaction_report(), &legacy_report);
        let runtime = snapshot.take_coverage_runtime();
        assert!(runtime.turns.is_none());
        assert!(runtime.classifier_rejected);
        assert!(!runtime.source_partial);
        assert!(snapshot.try_into_ordinary().is_ok());
    }

    #[test]
    fn cov2_runtime_post_redaction_binding_changes_reject_typed() {
        use crate::internal::ai::observed_agents::{ExportAuthorized, agent_for};

        let secret_id = "sk-ant-EXAMPLE0000000000000000";
        for changed in ["session", "message", "part"] {
            let source_session = if changed == "session" {
                secret_id
            } else {
                "ses_og26_fixture"
            };
            let document = serde_json::json!({"info":{"id":source_session},"messages":[
                {"id":"u1","type":"user","text":"safe request"},
                {"id":if changed == "message" { secret_id } else { "a1" },"type":"assistant","content":[
                    {"id":if changed == "part" { secret_id } else { "r1" },"type":"reasoning","text":"safe thought"}
                ]}
            ]});
            let source = serde_json::to_vec(&normalize_json_value(document.clone())).unwrap();
            let (legacy, legacy_report) = Redactor::new_default().redact(&source);
            assert!(legacy_report.match_count() > 0, "{changed}");
            let preview = prepare_redacted_coverage_preview(
                "opencode",
                source.clone(),
                CaptureSnapshotPolicy::default(),
            )
            .expect("preview keeps its existing redacted-coordinate behavior");
            assert!(!preview.partial, "{changed}");

            let mut snapshot = CaptureSnapshotService::capture_authorized_with_reasoning_context(
                TranscriptSource::Bytes {
                    auth: ExportAuthorized::issue("opencode", "capture__og26_fixture", &source),
                    bytes: source,
                },
                agent_for(AgentKind::OpenCode),
                AuthorizedReasoningSourceContext::authorized_export(
                    "opencode",
                    "capture__og26_fixture",
                    source_session,
                ),
                CaptureSnapshotPolicy::default(),
            );
            assert!(
                matches!(snapshot.reasoning, ReasoningSourceDisposition::Ordinary),
                "{changed}"
            );
            assert_eq!(
                snapshot.completeness(),
                CaptureSnapshotCompleteness::Complete,
                "{changed}"
            );
            assert_eq!(snapshot.transcript().unwrap(), &legacy, "{changed}");
            assert_eq!(snapshot.redaction_report(), &legacy_report, "{changed}");
            let runtime = snapshot.take_coverage_runtime();
            assert!(runtime.turns.is_none(), "{changed}");
            assert!(runtime.classifier_rejected, "{changed}");
            assert!(!runtime.source_partial, "{changed}");
            assert!(snapshot.try_into_ordinary().is_ok(), "{changed}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn cov2_runtime_partial_classifier_keeps_candidate_and_partial_permit() {
        use crate::internal::ai::observed_agents::reasoning::assert_og26_candidate_for_test;

        let receipt = r#"{"receipt":"unverified","nonce":3}"#;
        let source = format!(
            r#"{{"info":{{"id":"ses_og26_fixture"}},"messages":[{{"id":"msg_og26","type":"assistant","content":[{{"id":"npart_0123456789abcdef0123456789abcdef","type":"reasoning","text":"","state":{{"signature":"sig-\u754c","redactedData":"red-\u00e9","reasoningEncryptedContent":"enc-\uD83D\uDE80"}},"originProof":{receipt}}}]}}]}}"#
        );
        let mut candidate = og26_snapshot(source.into_bytes(), CaptureSnapshotPolicy::default());
        assert!(matches!(
            candidate.reasoning,
            ReasoningSourceDisposition::OpenCodeCandidate(_)
        ));
        assert_og26_candidate_for_test(&candidate.reasoning, receipt.as_bytes());
        assert_eq!(
            candidate.completeness(),
            CaptureSnapshotCompleteness::Partial
        );
        assert!(candidate.rejects_capture_source_consumer());
        assert!(!candidate.has_verified_ordinary_partial_permit());
        let runtime = candidate.take_coverage_runtime();
        assert!(runtime.turns.is_none());
        assert!(!runtime.classifier_rejected);
        assert!(runtime.source_partial);
        assert!(candidate.try_into_ordinary().is_err());

        let mut partial = verified_partial_fixture_snapshot(
            "capture__og26_fixture",
            "ses_og26_fixture",
            Path::new("/fixture"),
        );
        assert!(matches!(
            partial.reasoning,
            ReasoningSourceDisposition::Unproven
        ));
        assert_eq!(partial.completeness(), CaptureSnapshotCompleteness::Partial);
        assert!(partial.rejects_capture_source_consumer());
        assert!(partial.has_verified_ordinary_partial_permit());
        let runtime = partial.take_coverage_runtime();
        assert!(runtime.turns.is_none());
        assert!(!runtime.classifier_rejected);
        assert!(runtime.source_partial);
        assert!(
            partial.has_verified_ordinary_partial_permit(),
            "consuming typed data must retain the sealed permit"
        );
        assert!(partial.try_into_ordinary().is_err());
    }

    #[test]
    fn cov2_runtime_source_transport_redacts_before_digest_and_projection() {
        use crate::internal::ai::observed_agents::coverage::{
            CoverageSchemaVersion, encode_coverage_turn, safe_turn_projection_for_schema,
            select_coverage_schema,
        };
        let secret = "sk-ant-EXAMPLE0000000000000000";
        let source = br#"{"info":{"id":"ses_og26_fixture"},"messages":[{"id":"u1","type":"user","text":"safe request"},{"id":"a1","type":"assistant","content":[{"id":"r1","type":"reasoning","text":"thought sk-ant-EXAMPLE000000000000000\u0030"},{"id":"t1","type":"text","text":"safe reply"}]}]}"#;
        let mut snapshot = og26_snapshot(source.to_vec(), CaptureSnapshotPolicy::default());
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        let transcript = snapshot.transcript().expect("authorized redacted source");
        assert!(!String::from_utf8_lossy(transcript.bytes()).contains(secret));
        let expected = digest_hex(transcript.bytes());
        assert_eq!(
            snapshot
                .safe_projection()
                .source
                .expect("source projection")
                .digest_sha256
                .as_deref(),
            Some(expected.as_str())
        );
        assert_eq!(
            snapshot.safe_projection().redaction_bytes_scanned,
            source.len()
        );
        assert!(
            snapshot.redaction_report().bytes_scanned > source.len(),
            "the detailed report counts the extra decoded/typed redaction passes"
        );
        let runtime = snapshot.take_coverage_runtime();
        assert_eq!(
            snapshot.safe_projection().redaction_bytes_scanned,
            source.len(),
            "consuming the companion must not change the immutable scan boundary"
        );
        assert!(!runtime.classifier_rejected);
        assert!(!runtime.source_partial);
        let turns = runtime.turns.expect("private redacted typed turns");
        assert!(
            turns
                .iter()
                .any(|turn| select_coverage_schema(turn) == CoverageSchemaVersion::V2)
        );
        for turn in &turns {
            let canonical = encode_coverage_turn(turn).expect("redacted typed canonical");
            assert!(!String::from_utf8_lossy(&canonical.canonical_bytes).contains(secret));
            let projection = safe_turn_projection_for_schema(2, "opencode", turn)
                .expect("safe versioned projection");
            assert!(!projection.to_string().contains(secret));
        }
        let consumed = snapshot.take_coverage_runtime();
        assert!(consumed.turns.is_none());
        assert!(!consumed.classifier_rejected);
        assert!(!consumed.source_partial);
    }

    #[test]
    fn cov2_runtime_legacy_rejection_marker_and_non_opencode_defaults() {
        use crate::internal::ai::observed_agents::{ExportAuthorized, agent_for};
        let source_session = "s".repeat(1025);
        let bytes = serde_json::to_vec(&serde_json::json!({
            "info":{"id":source_session},
            "messages":[{"id":"u1","type":"user","text":"safe request"}]
        }))
        .expect("legacy-only export");
        let mut snapshot = CaptureSnapshotService::capture_authorized_with_reasoning_context(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue("opencode", "capture", &bytes),
                bytes: bytes.clone(),
            },
            agent_for(AgentKind::OpenCode),
            AuthorizedReasoningSourceContext::authorized_export(
                "opencode",
                "capture",
                &source_session,
            ),
            CaptureSnapshotPolicy::default(),
        );
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        assert_eq!(
            snapshot.transcript().expect("same ordinary source").bytes(),
            bytes
        );
        let mut runtime = snapshot.take_coverage_runtime();
        assert!(runtime.classifier_rejected);
        assert!(runtime.turns.take().is_none());
        assert!(
            runtime.classifier_rejected,
            "taking typed data cannot clear the rejection barrier"
        );
        assert!(!runtime.source_partial);
        // Legacy ordinary text does not acquire typed-coordinate prerequisites.
        let ordinary = br#"{"info":{"id":"ses_og26_fixture"},"messages":[{"type":"assistant","content":[{"type":"text","text":"safe response"}]}]}"#;
        let mut ordinary_snapshot =
            og26_snapshot(ordinary.to_vec(), CaptureSnapshotPolicy::default());
        assert_eq!(
            ordinary_snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        assert_eq!(
            ordinary_snapshot
                .transcript()
                .expect("unchanged ordinary bytes")
                .bytes(),
            ordinary
        );
        let ordinary_runtime = ordinary_snapshot.take_coverage_runtime();
        assert!(ordinary_runtime.turns.is_none());
        assert!(!ordinary_runtime.classifier_rejected);
        assert!(!ordinary_runtime.source_partial);
        let mut plain = CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue("claude_code", "capture", b"safe ordinary source"),
                bytes: b"safe ordinary source".to_vec(),
            },
            "claude_code",
            "capture",
            CaptureSnapshotPolicy::default(),
        );
        assert_eq!(
            plain.transcript().expect("ordinary source").bytes(),
            b"safe ordinary source"
        );
        let runtime = plain.take_coverage_runtime();
        assert!(runtime.turns.is_none());
        assert!(!runtime.classifier_rejected);
        assert!(!runtime.source_partial);
    }

    #[test]
    fn og26_preprocessor_rejections_wipe_and_hide_raw_source() {
        use crate::internal::ai::observed_agents::{ExportAuthorized, agent_for};

        const MARKER: &[u8] = b"OG26_PREPARSER_PRIVATE_canary";
        let source = |bytes: &[u8], session: &str| TranscriptSource::Bytes {
            auth: ExportAuthorized::issue("opencode", session, bytes),
            bytes: bytes.to_vec(),
        };
        let mut probe = source(MARKER, "capture_og26");
        wipe_in_memory_source(&mut probe);
        let TranscriptSource::Bytes { bytes, .. } = probe else {
            panic!("test source must remain an owned export")
        };
        assert!(bytes.is_empty(), "zeroize clears the owned export buffer");

        let capture = "capture_og26";
        let provider = "ses_og26_fixture";
        let adapter = agent_for(AgentKind::OpenCode);
        let expired = Instant::now()
            .checked_sub(Duration::from_millis(1))
            .expect("test clock has a past instant");
        let cases = [
            (
                "unknown provider",
                CaptureSnapshotService::capture_authorized(
                    source(MARKER, capture),
                    "unknown-provider",
                    capture,
                    CaptureSnapshotPolicy::default(),
                ),
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
            ),
            (
                "provider mismatch",
                CaptureSnapshotService::capture_authorized_with_reasoning_context(
                    source(MARKER, capture),
                    adapter,
                    AuthorizedReasoningSourceContext::authorized_export(
                        "claude_code",
                        capture,
                        provider,
                    ),
                    CaptureSnapshotPolicy::default(),
                ),
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
            ),
            (
                "expired deadline",
                CaptureSnapshotService::capture_authorized_with_reasoning_context(
                    source(MARKER, capture),
                    adapter,
                    AuthorizedReasoningSourceContext::authorized_export(
                        "opencode", capture, provider,
                    ),
                    CaptureSnapshotPolicy::with_max_bytes(Some(expired), 1024),
                ),
                CaptureSnapshotPartialReason::DeadlineExceeded,
            ),
            (
                "source oversize",
                CaptureSnapshotService::capture_authorized_with_reasoning_context(
                    source(MARKER, capture),
                    adapter,
                    AuthorizedReasoningSourceContext::authorized_export(
                        "opencode", capture, provider,
                    ),
                    CaptureSnapshotPolicy::with_max_bytes(None, 1),
                ),
                CaptureSnapshotPartialReason::SourceOversize,
            ),
            (
                "authorization mismatch",
                CaptureSnapshotService::capture_authorized_with_reasoning_context(
                    source(MARKER, "another_capture"),
                    adapter,
                    AuthorizedReasoningSourceContext::authorized_export(
                        "opencode", capture, provider,
                    ),
                    CaptureSnapshotPolicy::default(),
                ),
                CaptureSnapshotPartialReason::SourceAuthorizationMismatch,
            ),
            (
                "empty export",
                CaptureSnapshotService::capture_authorized_with_reasoning_context(
                    source(&[], capture),
                    adapter,
                    AuthorizedReasoningSourceContext::authorized_export(
                        "opencode", capture, provider,
                    ),
                    CaptureSnapshotPolicy::default(),
                ),
                CaptureSnapshotPartialReason::SourceEmpty,
            ),
        ];
        for (name, snapshot, reason) in cases {
            assert_eq!(snapshot.partial_reason(), Some(reason), "{name}");
            assert_eq!(
                snapshot.completeness(),
                CaptureSnapshotCompleteness::Partial,
                "{name}"
            );
            assert!(snapshot.transcript().is_none(), "{name}");
            assert!(
                !format!("{snapshot:?}").contains("OG26_PREPARSER_PRIVATE"),
                "{name}"
            );
        }
    }

    #[test]
    fn og26_opencode_candidate_is_removed_before_redactor() {
        use crate::internal::ai::observed_agents::reasoning::{
            ReasoningSourceDisposition, assert_og26_candidate_for_test,
        };

        let receipt = r#"{ "receipt" : "r\u0065ceipt", "nonce" : 3 }"#;
        let source = format!(
            r#"{{"info":{{"id":"ses_og26_fixture"}},"messages":[{{"id":"msg_og26","type":"assistant","content":[{{"id":"npart_0123456789abcdef0123456789abcdef","type":"reasoning","text":"safe visible","state":{{"signature":"sig-\u754c","redactedData":"red-\u00e9","reasoningEncryptedContent":"enc-\uD83D\uDE80"}},"originProof":{receipt}}}]}}]}}"#
        );
        let snapshot = og26_snapshot(source.into_bytes(), CaptureSnapshotPolicy::default());
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Partial
        );
        assert!(snapshot.rejects_capture_source_consumer());
        assert!(matches!(
            snapshot.reasoning,
            ReasoningSourceDisposition::OpenCodeCandidate(_)
        ));
        assert_og26_candidate_for_test(&snapshot.reasoning, receipt.as_bytes());
        let ordinary = snapshot
            .transcript()
            .expect("a safe redacted copy remains in the private snapshot")
            .bytes();
        let visible = std::str::from_utf8(ordinary).expect("JSON remains UTF-8");
        assert!(visible.contains("messages"));
        for required in [
            "safe visible",
            "sig-",
            "red-",
            "enc-",
            "receipt",
            "originProof",
            "signature",
        ] {
            assert!(
                visible.contains(required),
                "ordinary output dropped unproven {required}"
            );
        }
        assert!(!format!("{snapshot:?}").contains("sig-"));
        assert!(snapshot.try_into_ordinary().is_err());
    }

    #[test]
    fn og26_candidate_budget_and_partial_are_closed() {
        use crate::internal::ai::observed_agents::{
            opencode_reasoning::prepare_authorized_source, reasoning::ReasoningFieldError,
        };

        let mut dense = Vec::with_capacity(2 * 65_537 + 2);
        dense.push(b'[');
        for ordinal in 0..=65_536 {
            if ordinal > 0 {
                dense.push(b',');
            }
            dense.push(b'0');
        }
        dense.push(b']');
        assert!(matches!(
            prepare_authorized_source(dense.clone()),
            Err(ReasoningFieldError::SourceTooLarge)
        ));
        let snapshot = og26_snapshot(dense, CaptureSnapshotPolicy::default());
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Partial
        );
        assert!(snapshot.transcript().is_none());
        assert_eq!(
            snapshot.partial_reason(),
            Some(CaptureSnapshotPartialReason::SourceOversize)
        );

        // The lexical budget refuses even a malformed tail before JSON tree materialization.
        let mut invalid_after_budget = Vec::with_capacity(2 * 65_537 + 12);
        invalid_after_budget.push(b'[');
        for ordinal in 0..=65_536 {
            if ordinal > 0 {
                invalid_after_budget.push(b',');
            }
            invalid_after_budget.push(b'0');
        }
        invalid_after_budget.extend_from_slice(b",NOT_JSON]");
        assert!(matches!(
            prepare_authorized_source(invalid_after_budget),
            Err(ReasoningFieldError::SourceTooLarge)
        ));

        let deep = format!("{}0{}", "[".repeat(70), "]".repeat(70));
        assert!(matches!(
            prepare_authorized_source(deep.into_bytes()),
            Err(ReasoningFieldError::SourceTooLarge)
        ));
        let duplicate = br#"{"info":{"id":"ses_og26_fixture"},"info":{},"messages":[]}"#;
        assert!(matches!(
            prepare_authorized_source(duplicate.to_vec()),
            Err(ReasoningFieldError::MalformedSource)
        ));
        let oversized_receipt = format!(
            r#"{{"info":{{"id":"ses_og26_fixture"}},"messages":[{{"id":"msg_og26","type":"assistant","content":[{{"id":"npart_0123456789abcdef0123456789abcdef","type":"reasoning","text":"safe","state":{{"signature":"private"}},"originProof":{{"padding":"{}"}}}}]}}]}}"#,
            "x".repeat(8192)
        );
        assert!(matches!(
            prepare_authorized_source(oversized_receipt.into_bytes()),
            Err(ReasoningFieldError::SourceTooLarge)
        ));

        let expired = og26_snapshot(
            br#"{"info":{"id":"ses_og26_fixture"},"messages":[]}"#.to_vec(),
            CaptureSnapshotPolicy::with_deadline(
                Instant::now().checked_sub(Duration::from_secs(1)),
            ),
        );
        assert_eq!(
            expired.partial_reason(),
            Some(CaptureSnapshotPartialReason::DeadlineExceeded)
        );
        assert!(expired.transcript().is_none());
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn verified_partial_snapshot_requires_authorization_mapping_and_redaction() {
        let (dir, storage, conn, scope) = verified_partial_key_fixture().await;
        let capture = "opencode__partial-test";
        let source = "ses_partial_test";
        let snapshot = verified_partial_fixture_snapshot(capture, source, dir.path());
        use crate::internal::ai::observed_agents::{ExportAuthorized, agent_for};
        let ordinary = snapshot.transcript().expect("safe source").bytes().to_vec();
        let authorization_mismatch =
            CaptureSnapshotService::capture_authorized_with_reasoning_context(
                TranscriptSource::Bytes {
                    auth: ExportAuthorized::issue(
                        "opencode",
                        capture,
                        b"different authorized preimage",
                    ),
                    bytes: ordinary.clone(),
                },
                agent_for(AgentKind::OpenCode),
                AuthorizedReasoningSourceContext::authorized_export("opencode", capture, source),
                CaptureSnapshotPolicy::default(),
            );
        assert!(!authorization_mismatch.has_verified_ordinary_partial_permit());
        assert!(authorization_mismatch.transcript().is_none());
        let raw = serde_json::to_vec(&serde_json::json!({"info":{"id":source},"messages":[
            {"id":"u","type":"user","text":"safe"},
            {"id":"a","type":"assistant","content":[{"type":"reasoning","text":"private","state":{"reasoningField":"private"}},{"type":"text","text":"safe"}]}
        ]})).unwrap();
        let mapping_mismatch = CaptureSnapshotService::capture_authorized_with_reasoning_context(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue("opencode", capture, &raw),
                bytes: raw.clone(),
            },
            agent_for(AgentKind::OpenCode),
            AuthorizedReasoningSourceContext::authorized_export(
                "opencode",
                capture,
                "wrong-provider-session",
            ),
            CaptureSnapshotPolicy::default(),
        );
        assert!(!mapping_mismatch.has_verified_ordinary_partial_permit());
        assert!(mapping_mismatch.transcript().is_none());
        let legacy = CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes {
                auth: ExportAuthorized::issue("opencode", capture, &raw),
                bytes: raw,
            },
            "opencode",
            capture,
            CaptureSnapshotPolicy::default(),
        );
        assert!(!legacy.has_verified_ordinary_partial_permit());
        let mut wrong_kind = verified_partial_fixture_snapshot(capture, source, dir.path());
        wrong_kind.source.as_mut().unwrap().kind = CaptureSnapshotSourceKind::ProviderFile;
        assert!(!wrong_kind.has_verified_ordinary_partial_permit());
        assert!(snapshot.has_verified_ordinary_partial_permit());
        assert!(snapshot.rejects_capture_source_consumer());
        let copy = snapshot.transcript().expect("safe partial source").bytes();
        for canary in [
            "PRIVATE_ASSOCIATED_REASONING_CANARY",
            "PRIVATE_STATE_CANARY",
        ] {
            assert!(
                String::from_utf8_lossy(copy).contains(canary),
                "ordinary partial transcript dropped {canary}"
            );
        }
        assert!(
            verified_partial_fixture_snapshot(capture, source, dir.path())
                .try_into_ordinary()
                .is_err()
        );
        let carrier = snapshot
            .try_into_verified_ordinary_partial_in_scope_until(
                &conn,
                &scope,
                &storage,
                dir.path(),
                Instant::now() + std::time::Duration::from_secs(10),
            )
            .await
            .unwrap_or_else(|_| panic!("current scope accepts sealed ordinary partial"));
        assert_eq!(
            carrier.projection().completeness,
            CaptureSnapshotCompleteness::Partial
        );
        assert!(carrier.projection().has_durable_source_commitment());
        assert!(carrier.binding.matches_capture(capture, "opencode"));
        assert!(!carrier.binding.matches_capture("different", "opencode"));
        assert!(!carrier.binding.matches_capture(capture, "codex"));
        let mut wrong_scope = scope.clone();
        wrong_scope.repo_id = "foreign".to_owned();
        assert!(!carrier.binding.matches_scope(Some(&wrong_scope)));
        for (candidate_scope, candidate_root, deadline) in [
            (
                &wrong_scope,
                dir.path(),
                Instant::now() + std::time::Duration::from_secs(10),
            ),
            (&scope, dir.path(), Instant::now()),
        ] {
            let rejected = verified_partial_fixture_snapshot(capture, source, dir.path())
                .try_into_verified_ordinary_partial_in_scope_until(
                    &conn,
                    candidate_scope,
                    &storage,
                    candidate_root,
                    deadline,
                )
                .await
                .err()
                .expect("invalid scope/deadline rejected");
            assert!(!rejected.has_verified_ordinary_partial_permit());
            assert!(rejected.transcript().is_none());
        }
        let foreign = tempfile::tempdir().expect("foreign scope root");
        let foreign_storage = foreign.path().join(".libra");
        std::fs::create_dir(&foreign_storage).expect("foreign storage");
        for (candidate_storage, candidate_root) in [
            (storage.as_path(), foreign.path()),
            (foreign_storage.as_path(), dir.path()),
        ] {
            let rejected = verified_partial_fixture_snapshot(capture, source, dir.path())
                .try_into_verified_ordinary_partial_in_scope_until(
                    &conn,
                    &scope,
                    candidate_storage,
                    candidate_root,
                    Instant::now() + std::time::Duration::from_secs(10),
                )
                .await
                .err()
                .expect("foreign storage/root rejected");
            assert!(!rejected.has_verified_ordinary_partial_permit());
            assert!(rejected.transcript().is_none());
        }
        let mut missing_source = verified_partial_fixture_snapshot(capture, source, dir.path());
        missing_source.source = None;
        let rejected = missing_source
            .try_into_verified_ordinary_partial_in_scope_until(
                &conn,
                &scope,
                &storage,
                dir.path(),
                Instant::now() + std::time::Duration::from_secs(10),
            )
            .await
            .err()
            .expect("missing source cannot mint a carrier");
        assert!(!rejected.has_verified_ordinary_partial_permit());
        assert!(rejected.transcript().is_none());
        let mut forged_commitment = verified_partial_fixture_snapshot(capture, source, dir.path());
        forged_commitment.source.as_mut().unwrap().digest_sha256 =
            Some(format!("source/hmac-v2/{}", "00".repeat(32)));
        assert!(
            forged_commitment
                .safe_projection()
                .has_durable_source_commitment(),
            "syntactically valid HMAC is still not current-scope authorization"
        );
        let rejected = forged_commitment
            .try_into_verified_ordinary_partial_in_scope_until(
                &conn,
                &scope,
                &storage,
                dir.path(),
                Instant::now() + std::time::Duration::from_secs(10),
            )
            .await
            .err()
            .expect("caller HMAC cannot replace fresh current-scope derivation");
        assert!(!rejected.has_verified_ordinary_partial_permit());
        assert!(rejected.transcript().is_none());
        let mut corrupted = verified_partial_fixture_snapshot(capture, source, dir.path());
        corrupted
            .source
            .as_mut()
            .expect("source projection")
            .digest_sha256 = Some(format!("sha256:{}", "00".repeat(32)));
        let rejected = corrupted
            .try_into_verified_ordinary_partial_in_scope_until(
                &conn,
                &scope,
                &storage,
                dir.path(),
                Instant::now() + std::time::Duration::from_secs(10),
            )
            .await
            .err()
            .expect("digest mismatch cannot be rebound");
        assert!(rejected.transcript().is_none());
        let mut downgraded = verified_partial_fixture_snapshot(capture, source, dir.path());
        downgraded.downgrade_for_commitment_failure(CaptureSnapshotPartialReason::SourceReadError);
        assert!(!downgraded.has_verified_ordinary_partial_permit());
        assert!(downgraded.transcript().is_none());
    }
}
