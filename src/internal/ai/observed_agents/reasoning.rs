//! Provider-neutral reasoning states and an opaque ciphertext type.
//!
//! This module validates a declared Claude field shape after source
//! authorization. Provider adapters must establish identity from pinned source
//! evidence; entropy and base64-like contents never prove ciphertext.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    time::Instant,
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use ring::{
    hmac,
    rand::{SecureRandom, SystemRandom},
};
use serde::{
    Deserialize, Serialize,
    de::{self, Deserializer, MapAccess, SeqAccess, Visitor},
};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use thiserror::Error;
use zeroize::{Zeroize, Zeroizing};

use super::{
    coverage::{CanonValue, Completeness, NormalizedTurn, SemanticRecord, parse_canon_value},
    native_origin::codec::{FieldName, RECEIPT_BYTE_CAP},
    transcript_source::TRANSCRIPT_READ_HARD_CAP_BYTES,
};

/// Optional provider port at the authorized, bounded source boundary.
/// Implementations only separate an owned buffer. They cannot open a source,
/// access a store, or construct native authority from a provider label.
pub trait ReasoningSourcePreprocessor: Send + Sync {
    fn prepare_authorized_source(
        &self,
        source: Vec<u8>,
    ) -> Result<PreparedReasoningSource, ReasoningFieldError>;
}

/// A separated ordinary buffer and a sealed, owning disposition. No public raw
/// accessor, serialization, cloning or content-bearing Debug is available.
pub struct PreparedReasoningSource {
    ordinary: Zeroizing<Vec<u8>>,
    disposition: ReasoningSourceDisposition,
    ordinary_partial_permit: Option<OpenCodeOrdinaryPartialPermit>,
    coverage_fragment: Option<ReasoningCoverageFragment>,
    classifier_rejected: bool,
}

/// Data-only owning classifier output. The preview consumer destroys the
/// prepared source's authority before using the fragment.
pub(crate) enum OpenCodeCoverageClassification {
    Parsed(Box<OpenCodeCoverageSource>),
    Partial(ReasoningFieldError),
}

pub(crate) struct OpenCodeCoverageSource {
    pub(crate) source_session: Zeroizing<String>,
    pub(crate) prepared: PreparedReasoningSource,
    pub(crate) visible_parts: Vec<OpenCodeVisibleReasoningPart>,
    /// An independently readable part cannot clear unsupported content.
    pub(crate) partial: bool,
}

/// Owning raw classifier result, sealed within the provider source boundary.
/// It carries data only: no source, capture or native-origin authority. All
/// coordinate strings and visible payload strings wipe when dropped.
pub(crate) struct ReasoningCoverageFragment {
    pub(super) contains_reasoning: bool,
    pub(super) source_session: Zeroizing<String>,
    pub(super) visible_parts: Vec<OpenCodeVisibleReasoningPart>,
    pub(super) partial: bool,
}

pub(crate) struct OpenCodeVisibleReasoningPart {
    pub(crate) source_message: Zeroizing<String>,
    pub(crate) message_ordinal: usize,
    pub(crate) source_part: Zeroizing<String>,
    pub(crate) part_ordinal: usize,
    pub(crate) record: SemanticRecord,
}

/// Sole attachment site is the OpenCode adapter's bounded original-buffer
/// classifier. This adds no constructor method or native authority.
pub(super) fn attach_reasoning_coverage(
    mut prepared: PreparedReasoningSource,
    classification: Result<ReasoningCoverageFragment, ReasoningFieldError>,
) -> PreparedReasoningSource {
    match classification {
        Ok(fragment) => {
            // Candidates retain their separate sealed owner. Raw readable
            // coordinates must not survive along that unverified route.
            if !matches!(
                prepared.disposition,
                ReasoningSourceDisposition::OpenCodeCandidate(_)
            ) {
                prepared.coverage_fragment = Some(fragment);
            }
        }
        Err(_) => prepared.classifier_rejected = true,
    }
    prepared
}

/// The authorized snapshot boundary consumes this data once, independently of
/// disposition and storage-only permits. Original session identity is checked
/// before typed redaction can transform its spelling.
pub(crate) fn take_authorized_reasoning_coverage(
    prepared: &mut PreparedReasoningSource,
    has_verified_source_mapping: bool,
    provider: &str,
    source_session: Option<&str>,
) -> Result<(Option<ReasoningCoverageFragment>, bool, bool), ReasoningFieldError> {
    let rejected = prepared.classifier_rejected;
    let partial = !prepared.disposition.has_capture_consumer();
    let Some(mut fragment) = prepared.coverage_fragment.take() else {
        return Ok((None, rejected, partial));
    };
    // A stricter typed classifier cannot demote an existing ordinary/native
    // consumer. Destroy its incomplete companion and retain the legacy source
    // decision; the caller's V2-history barrier owns rejection of the attempt.
    if !partial && fragment.partial && fragment.contains_reasoning {
        prepared.classifier_rejected = true;
        return Ok((None, true, false));
    }
    let partial = partial || (fragment.partial && fragment.contains_reasoning);
    // An ordinary export without a readable field retains its legacy bytes
    // and does not require a new identity prerequisite.
    if fragment.visible_parts.is_empty() {
        return Ok((None, rejected, partial));
    }
    if !has_verified_source_mapping
        || provider != "opencode"
        || source_session != Some(fragment.source_session.as_str())
    {
        return Err(ReasoningFieldError::UnsupportedShape);
    }
    fragment.partial = partial;
    Ok((Some(fragment), rejected, partial))
}

/// Storage-only eligibility from the strict OpenCode scrubber. It cannot
/// authorize native bytes, serialize across a helper, or be cloned.
pub(crate) struct OpenCodeOrdinaryPartialPermit {
    source_session: String,
    capture_session: String,
}

impl OpenCodeOrdinaryPartialPermit {
    pub(crate) fn capture_session(&self) -> &str {
        &self.capture_session
    }
    pub(crate) fn source_session(&self) -> &str {
        &self.source_session
    }
}

/// Conservative, read-only shape check for the inert coverage classifier.
/// A free query keeps the carrier's closed constructor-method set unchanged;
/// this boolean cannot construct a native proof or an ordinary permit.
pub(super) fn prepared_source_is_ordinary_projection(source: &PreparedReasoningSource) -> bool {
    source.disposition.is_ordinary()
}

/// Consume only the separated ordinary projection for an untrusted preview.
/// Dropping the owning carrier destroys all candidate/native/permit authority;
/// the returned boolean can only make a preview partial, never authorize it.
pub(super) fn discard_prepared_authority_for_coverage_preview(
    mut source: PreparedReasoningSource,
) -> (Vec<u8>, bool) {
    let partial = !source.disposition.is_ordinary();
    let ordinary = std::mem::take(&mut *source.ordinary);
    drop(source);
    (ordinary, partial)
}

impl PreparedReasoningSource {
    /// The selected, reviewed detector found no reasoning candidate.
    pub fn without_candidates(ordinary: Vec<u8>) -> Self {
        Self {
            ordinary: Zeroizing::new(ordinary),
            disposition: ReasoningSourceDisposition::Ordinary,
            ordinary_partial_permit: None,
            coverage_fragment: None,
            classifier_rejected: false,
        }
    }

    /// The detector removed candidates whose native provenance is absent.
    /// The safe result remains nonordinary even when it has no artifact.
    pub fn unproven_candidates(ordinary: Vec<u8>) -> Self {
        Self {
            ordinary: Zeroizing::new(ordinary),
            disposition: ReasoningSourceDisposition::Unproven,
            ordinary_partial_permit: None,
            coverage_fragment: None,
            classifier_rejected: false,
        }
    }

    /// Sole storage-only producer: the strict OpenCode parser has erased all
    /// unsupported reasoning and ruled out every original originProof key.
    pub(super) fn with_verified_opencode_partial(
        ordinary: Vec<u8>,
        source_session: String,
    ) -> Self {
        Self {
            ordinary: Zeroizing::new(ordinary),
            disposition: ReasoningSourceDisposition::Unproven,
            ordinary_partial_permit: Some(OpenCodeOrdinaryPartialPermit {
                source_session,
                capture_session: String::new(),
            }),
            coverage_fragment: None,
            classifier_rejected: false,
        }
    }

    /// The OpenCode parser has moved a bounded, unverified candidate out of
    /// the ordinary copy. This is only private input for a later verifier;
    /// constructing it grants no native or capture authority.
    pub(super) fn with_opencode_candidates(
        ordinary: Vec<u8>,
        candidates: SealedOpenCodeCandidates,
    ) -> Self {
        Self {
            ordinary: Zeroizing::new(ordinary),
            disposition: ReasoningSourceDisposition::OpenCodeCandidate(candidates),
            ordinary_partial_permit: None,
            coverage_fragment: None,
            classifier_rejected: false,
        }
    }
}

/// A single export part's source coordinates and original decoded JSON
/// strings. All payload-bearing allocations wipe on drop. This type has no
/// Clone, Serialize, Debug or public raw accessor.
pub(super) struct OpenCodeCandidatePart {
    source_message: Zeroizing<String>,
    message_ordinal: usize,
    source_part: Zeroizing<String>,
    part_ordinal: usize,
    receipt: Zeroizing<Vec<u8>>,
    fields: Vec<(FieldName, Zeroizing<String>)>,
    private_bytes: usize,
}

/// Maximum parser nodes and source ordinals admitted before materializing an
/// OpenCode JSON tree. The preprocessor applies this bound before parsing.
pub(super) const OPENCODE_CANON_MAX_NODES: usize = 65_536;

impl OpenCodeCandidatePart {
    /// Checks only bounded syntax and ownership, never receipt authority.
    pub(super) fn checked(
        source_message: Zeroizing<String>,
        message_ordinal: usize,
        source_part: Zeroizing<String>,
        part_ordinal: usize,
        receipt: Zeroizing<Vec<u8>>,
        fields: Vec<(FieldName, Zeroizing<String>)>,
        source_len: usize,
    ) -> Result<Self, ReasoningFieldError> {
        let part_suffix = source_part
            .strip_prefix("npart_")
            .ok_or(ReasoningFieldError::UnsupportedShape)?;
        if source_message.is_empty()
            || source_message.len() > 1024
            || part_suffix.len() != 32
            || !part_suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || receipt.is_empty()
            || receipt.len() > RECEIPT_BYTE_CAP
            || fields.is_empty()
            || fields.len() > 3
        {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        let mut private_bytes = receipt.len();
        let mut previous = None;
        for (name, value) in &fields {
            if value.is_empty() || previous.is_some_and(|last| last >= *name) {
                return Err(ReasoningFieldError::UnsupportedShape);
            }
            private_bytes = private_bytes
                .checked_add(value.len())
                .filter(|total| *total <= source_len)
                .ok_or(ReasoningFieldError::SourceTooLarge)?;
            previous = Some(*name);
        }
        Ok(Self {
            source_message,
            message_ordinal,
            source_part,
            part_ordinal,
            receipt,
            fields,
            private_bytes,
        })
    }
}

/// Session-scoped, non-authoritative private carrier. Even a syntactically
/// correct receipt stays Unproven until a future writer-transaction verifier.
pub(crate) struct SealedOpenCodeCandidates {
    source_session: Zeroizing<String>,
    source_len: usize,
    private_bytes: usize,
    parts: Vec<OpenCodeCandidatePart>,
}

impl SealedOpenCodeCandidates {
    pub(super) fn checked(
        source_session: Zeroizing<String>,
        source_len: usize,
    ) -> Result<Self, ReasoningFieldError> {
        if source_session.is_empty()
            || source_session.len() > 1024
            || source_len == 0
            || source_len as u64 > TRANSCRIPT_READ_HARD_CAP_BYTES
        {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        Ok(Self {
            source_session,
            source_len,
            private_bytes: 0,
            parts: Vec::new(),
        })
    }

    pub(super) fn append(
        &mut self,
        part: OpenCodeCandidatePart,
    ) -> Result<(), ReasoningFieldError> {
        if self.parts.len() >= crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES
            || part.message_ordinal >= OPENCODE_CANON_MAX_NODES
            || part.part_ordinal >= OPENCODE_CANON_MAX_NODES
            || part.source_message.is_empty()
            || part.source_part.is_empty()
            || part.receipt.is_empty()
            || part.fields.is_empty()
        {
            return Err(ReasoningFieldError::SourceTooLarge);
        }
        if self.parts.iter().any(|existing| {
            existing.source_part.as_str() == part.source_part.as_str()
                || (existing.message_ordinal == part.message_ordinal
                    && existing.part_ordinal == part.part_ordinal)
                || (existing.source_message.as_str() == part.source_message.as_str()
                    && existing.message_ordinal != part.message_ordinal)
        }) {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        let actual_bytes = part
            .fields
            .iter()
            .try_fold(part.receipt.len(), |total, (_, value)| {
                total.checked_add(value.len())
            });
        if actual_bytes != Some(part.private_bytes) {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        let private_bytes = self
            .private_bytes
            .checked_add(part.private_bytes)
            .filter(|total| *total <= self.source_len)
            .ok_or(ReasoningFieldError::SourceTooLarge)?;
        self.parts
            .try_reserve(1)
            .map_err(|_| ReasoningFieldError::SourceTooLarge)?;
        self.parts.push(part);
        self.private_bytes = private_bytes;
        Ok(())
    }

    pub(super) fn is_empty(&self) -> bool {
        self.parts.is_empty()
    }
}

#[cfg(test)]
pub(crate) fn assert_og26_candidate_for_test(
    disposition: &ReasoningSourceDisposition,
    receipt: &[u8],
) {
    let ReasoningSourceDisposition::OpenCodeCandidate(candidates) = disposition else {
        panic!("the unverified candidate must remain privately owned")
    };
    assert_eq!(candidates.source_session.as_str(), "ses_og26_fixture");
    assert_eq!(candidates.parts.len(), 1);
    let part = &candidates.parts[0];
    assert_eq!(part.source_message.as_str(), "msg_og26");
    assert_eq!(part.message_ordinal, 0);
    assert_eq!(
        part.source_part.as_str(),
        "npart_0123456789abcdef0123456789abcdef"
    );
    assert_eq!(part.part_ordinal, 0);
    assert_eq!(part.receipt.as_slice(), receipt);
    assert_eq!(
        part.fields
            .iter()
            .map(|(name, value)| (*name, value.as_str()))
            .collect::<Vec<_>>(),
        [
            (FieldName::Signature, "sig-界"),
            (FieldName::RedactedData, "red-é"),
            (FieldName::ReasoningEncryptedContent, "enc-🚀"),
        ]
    );
}

/// Payload-free state readable by the narrow capture adapters. This is an
/// internal state, not a new public metadata enum or a native proof.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReasoningSourceState {
    Ordinary,
    Unproven,
    Unmapped,
    HelperUnsupported,
    NativeProven,
}

/// Owned reasoning authority travels with the source, never through a DTO.
/// Even a zero-artifact partial must not become ordinary complete content.
pub(crate) enum ReasoningSourceDisposition {
    Ordinary,
    Unproven,
    Unmapped,
    HelperUnsupported,
    OpenCodeCandidate(SealedOpenCodeCandidates),
    #[allow(dead_code)] // OG-05 alone may admit a production native issuer.
    NativeProven(NativeProvenReasoning),
}

impl ReasoningSourceDisposition {
    pub(crate) fn from_prepared_authorized_source(
        mut prepared: PreparedReasoningSource,
        has_verified_source_mapping: bool,
        max_source_bytes: u64,
        provider: &str,
        capture_session: &str,
        source_session: Option<&str>,
    ) -> Result<(Vec<u8>, Self), ReasoningFieldError> {
        if prepared.ordinary.len() as u64 > max_source_bytes {
            return Err(ReasoningFieldError::SourceTooLarge);
        }
        let mut disposition = prepared.disposition;
        if !has_verified_source_mapping && !disposition.is_ordinary() {
            disposition = Self::Unmapped;
        } else if let Self::OpenCodeCandidate(candidates) = &disposition {
            if provider != "opencode"
                || source_session != Some(candidates.source_session.as_str())
                || candidates.is_empty()
            {
                return Err(ReasoningFieldError::UnsupportedShape);
            }
        } else if let Self::NativeProven(batch) = &disposition {
            let kind = match provider {
                "claude_code" => ReasoningProvider::ClaudeCode,
                "codex" => ReasoningProvider::Codex,
                _ => return Err(ReasoningFieldError::UnsupportedShape),
            };
            batch.validate(capture_session, kind)?;
            if source_session != Some(batch.source_session.as_str()) {
                return Err(ReasoningFieldError::UnsupportedShape);
            }
        }
        Ok((std::mem::take(&mut *prepared.ordinary), disposition))
    }

    /// Preserve the parser's owning storage permit only after the existing
    /// source authorization mapping agrees with its original native info.id.
    pub(crate) fn from_prepared_authorized_source_with_partial_permit(
        mut prepared: PreparedReasoningSource,
        has_verified_source_mapping: bool,
        max_source_bytes: u64,
        provider: &str,
        capture_session: &str,
        source_session: Option<&str>,
    ) -> Result<(Vec<u8>, Self, Option<OpenCodeOrdinaryPartialPermit>), ReasoningFieldError> {
        let permit = prepared.ordinary_partial_permit.take();
        let (ordinary, disposition) = Self::from_prepared_authorized_source(
            prepared,
            has_verified_source_mapping,
            max_source_bytes,
            provider,
            capture_session,
            source_session,
        )?;
        let permit = match permit {
            Some(mut permit)
                if matches!(disposition, Self::Unproven) && has_verified_source_mapping =>
            {
                if provider != "opencode"
                    || capture_session.is_empty()
                    || source_session != Some(permit.source_session.as_str())
                {
                    return Err(ReasoningFieldError::UnsupportedShape);
                }
                permit.capture_session = capture_session.to_owned();
                Some(permit)
            }
            _ => None,
        };
        Ok((ordinary, disposition, permit))
    }

    pub(crate) fn helper_unsupported() -> Self {
        Self::HelperUnsupported
    }

    pub(crate) fn is_ordinary(&self) -> bool {
        matches!(self, Self::Ordinary)
    }

    /// Whether the sealed source state has a downstream capture consumer.
    /// `NativeProven` is admitted only through the typed identity/claim path;
    /// all other nonordinary states remain fail-closed.
    pub(crate) fn has_capture_consumer(&self) -> bool {
        matches!(self, Self::Ordinary | Self::NativeProven(_))
    }

    pub(crate) fn state(&self) -> ReasoningSourceState {
        match self {
            Self::Ordinary => ReasoningSourceState::Ordinary,
            Self::Unproven => ReasoningSourceState::Unproven,
            Self::Unmapped => ReasoningSourceState::Unmapped,
            Self::HelperUnsupported => ReasoningSourceState::HelperUnsupported,
            Self::OpenCodeCandidate(_) => ReasoningSourceState::Unproven,
            Self::NativeProven(_) => ReasoningSourceState::NativeProven,
        }
    }
}

/// No production native issuer is admitted by this preparatory carrier card.
/// OG-05 may add the independently verified OpenCode issuer after native GO.
/// An opaque RG classification never constructs this sealed witness.
#[allow(dead_code)] // Admission remains closed until OG-05 native GO.
enum NativeReasoningIssuer {
    Unregistered,
    #[cfg(test)]
    SyntheticOnly,
    #[cfg(test)]
    SyntheticUnknown,
}

pub(crate) struct NativeReasoningProof {
    issuer: NativeReasoningIssuer,
    provider: ReasoningProvider,
    source_session: String,
    message: u64,
    part: u64,
    field: ReasoningSourceKind,
    original_wire_sha256: [u8; 32],
    logical_turn: String,
}

/// Non-Clone batch: artifact bytes and their witnesses have exactly one owner.
/// Its constructor remains inside this module's fixed source validator.
pub(crate) struct NativeProvenReasoning {
    capture_session: String,
    source_session: String,
    artifacts: Vec<crate::internal::ai::traces::ReasoningArtifactParam>,
    proofs: Vec<NativeReasoningProof>,
}

/// Shared pending source/claim key budget; capture's portable claim codec
/// aliases this bound so an item cannot be valid only on one side.
pub(crate) const MAX_REASONING_TURN_KEY_BYTES: usize = 512;

/// A fresh, process-local import response key. It is consumed by seal or
/// verify; the only wire copy is the explicitly redacted helper control type.
pub(crate) struct ImportTransportKey([u8; 32]);

impl ImportTransportKey {
    pub(crate) fn random() -> Result<Self, ReasoningTransportError> {
        let mut bytes = [0_u8; 32];
        if SystemRandom::new().fill(&mut bytes).is_err() || bytes == [0; 32] {
            bytes.zeroize();
            return Err(ReasoningTransportError::KeyUnavailable);
        }
        Ok(Self(bytes))
    }

    pub(crate) fn encode_for_helper(&self) -> EncodedTransportKey {
        EncodedTransportKey(self.0)
    }
}

impl Drop for ImportTransportKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The only serializable key representation, confined to the bounded private
/// helper control frame. Its Debug implementation never reveals key bytes.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
pub(crate) struct EncodedTransportKey([u8; 32]);

impl EncodedTransportKey {
    pub(crate) fn into_key(self) -> Result<ImportTransportKey, ReasoningTransportError> {
        if self.0 == [0; 32] {
            return Err(ReasoningTransportError::InvalidControl);
        }
        Ok(ImportTransportKey(self.0))
    }
}

impl fmt::Debug for EncodedTransportKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("EncodedTransportKey([redacted])")
    }
}

impl Drop for EncodedTransportKey {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// Parent and helper independently derive these expected bindings from their
/// authorized request and source projection. None is itself native authority.
pub(crate) struct ImportTransportContext {
    challenge: [u8; 32],
    request_digest: [u8; 32],
    source_digest: [u8; 32],
    scope_digest: [u8; 32],
    projection_digest: [u8; 32],
    provider: ReasoningProvider,
    capture_session: String,
    source_session: String,
}

impl ImportTransportContext {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        challenge: [u8; 32],
        request_digest: [u8; 32],
        source_digest: [u8; 32],
        scope_digest: [u8; 32],
        projection_digest: [u8; 32],
        provider: ReasoningProvider,
        capture_session: String,
        source_session: String,
    ) -> Result<Self, ReasoningTransportError> {
        if capture_session.is_empty()
            || source_session.is_empty()
            || capture_session.len() > 4096
            || source_session.len() > 4096
        {
            return Err(ReasoningTransportError::InvalidControl);
        }
        Ok(Self {
            challenge,
            request_digest,
            source_digest,
            scope_digest,
            projection_digest,
            provider,
            capture_session,
            source_session,
        })
    }

    fn digest(&self) -> [u8; 32] {
        let mut digest = Sha256::new();
        digest.update(b"libra/import-reasoning-context/v1\0");
        for bytes in [
            self.challenge.as_slice(),
            self.request_digest.as_slice(),
            self.source_digest.as_slice(),
            self.scope_digest.as_slice(),
            self.projection_digest.as_slice(),
            match self.provider {
                ReasoningProvider::ClaudeCode => b"claude_code".as_slice(),
                ReasoningProvider::Codex => b"codex".as_slice(),
                ReasoningProvider::OpenCode => b"opencode".as_slice(),
            },
            self.capture_session.as_bytes(),
            self.source_session.as_bytes(),
        ] {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        digest.finalize().into()
    }
}

#[derive(Debug, Error)]
pub(crate) enum ReasoningTransportError {
    #[error("cannot create a private import transport key; retry the import")]
    KeyUnavailable,
    #[error("invalid private import transport control; retry the import")]
    InvalidControl,
    #[error("private import reasoning transport was rejected; retry the import")]
    Rejected,
    #[error("private import native reasoning proof was rejected; retry the import")]
    NativeProofRejected,
}

/// A closed, untrusted wire value. Only `verify_import_transport` may turn it
/// into owned native reasoning, after authenticating its complete collection.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ImportTransportDto {
    version: u8,
    context_sha256: [u8; 32],
    items: Vec<ImportTransportItem>,
    collection_sha256: [u8; 32],
    mac: [u8; 32],
}

impl fmt::Debug for ImportTransportDto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportTransportDto")
            .field("version", &self.version)
            .field("item_count", &self.items.len())
            .finish_non_exhaustive()
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportTransportItem {
    locator: String,
    provider: ReasoningProvider,
    source_kind: ReasoningSourceKind,
    bytes_base64: String,
    proof: ImportTransportProof,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportTransportProof {
    issuer: ImportTransportIssuer,
    provider: ReasoningProvider,
    source_session: String,
    message: u64,
    part: u64,
    field: ReasoningSourceKind,
    original_wire_sha256: [u8; 32],
    logical_turn: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ImportTransportIssuer {
    Unregistered,
    #[cfg(test)]
    SyntheticOnly,
    #[cfg(test)]
    SyntheticUnknown,
}

/// A capture association validated by the shared checkpoint adapter. It does
/// not mint a native witness and cannot be serialized into an IPC shortcut.
pub(crate) struct VerifiedCaptureBinding {
    capture_session: String,
    source_session: String,
    provider: ReasoningProvider,
}

/// A pending writer may borrow an already proven source only after the
/// checkpoint adapter has matched the current source, scope and claim plan.
/// This metadata is not a second issuer of native provider evidence.
pub(crate) struct VerifiedPendingBinding {
    capture: VerifiedCaptureBinding,
}

impl VerifiedPendingBinding {
    pub(crate) fn from_validated_pending_context(
        batch: &NativeProvenReasoning,
        capture_session: &str,
        provider: &str,
    ) -> Result<Self, ReasoningFieldError> {
        Ok(Self {
            capture: VerifiedCaptureBinding::from_validated_capture_context(
                batch,
                capture_session,
                provider,
            )?,
        })
    }
}

/// The catalog and marker adapter issues this only after checking the current
/// terminal attempt. A DTO or envelope MAC alone cannot create it.
pub(crate) struct VerifiedCurrentBinding {
    capture_session: String,
    provider: ReasoningProvider,
}

impl VerifiedCurrentBinding {
    pub(crate) fn from_validated_current_context(
        capture_session: &str,
        provider: &str,
    ) -> Result<Self, ReasoningFieldError> {
        let provider = match provider {
            "claude_code" => ReasoningProvider::ClaudeCode,
            "codex" => ReasoningProvider::Codex,
            _ => return Err(ReasoningFieldError::UnsupportedShape),
        };
        if capture_session.is_empty() || capture_session.len() > 4096 {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        Ok(Self {
            capture_session: capture_session.to_owned(),
            provider,
        })
    }
}

/// Integrity of the original private envelope and its exact members. This
/// token has no native-source authority, cannot be cloned or serialized, and
/// is consumed by one late restoration attempt.
pub(crate) struct PendingIntegrityToken {
    envelope_sha256: [u8; 32],
    payload_sha256: [u8; 32],
    source_sha256: [u8; 32],
    associations_sha256: [u8; 32],
    coverage_sha256: [u8; 32],
}

/// Retained equality data, not an authorization token. Each late attempt
/// must reacquire the repository key and compare a fresh integrity token.
pub(crate) struct PendingIntegrityStamp {
    envelope_sha256: [u8; 32],
    payload_sha256: [u8; 32],
    source_sha256: [u8; 32],
    associations_sha256: [u8; 32],
    coverage_sha256: [u8; 32],
}

impl PendingIntegrityToken {
    /// Production caller is closed to capture/key.rs by the architecture gate.
    pub(crate) fn from_authenticated_members(
        envelope_sha256: [u8; 32],
        payload_sha256: [u8; 32],
        source_sha256: [u8; 32],
        associations_sha256: [u8; 32],
        coverage_sha256: [u8; 32],
    ) -> Self {
        Self {
            envelope_sha256,
            payload_sha256,
            source_sha256,
            associations_sha256,
            coverage_sha256,
        }
    }

    pub(crate) fn into_stamp(self) -> PendingIntegrityStamp {
        PendingIntegrityStamp {
            envelope_sha256: self.envelope_sha256,
            payload_sha256: self.payload_sha256,
            source_sha256: self.source_sha256,
            associations_sha256: self.associations_sha256,
            coverage_sha256: self.coverage_sha256,
        }
    }

    pub(crate) fn matches_stamp(&self, stamp: &PendingIntegrityStamp) -> bool {
        self.envelope_sha256 == stamp.envelope_sha256
            && self.payload_sha256 == stamp.payload_sha256
            && self.source_sha256 == stamp.source_sha256
            && self.associations_sha256 == stamp.associations_sha256
            && self.coverage_sha256 == stamp.coverage_sha256
    }

    pub(crate) fn matches_associations(&self, canonical: &[u8]) -> bool {
        <[u8; 32]>::from(Sha256::digest(canonical)) == self.associations_sha256
    }
}

impl VerifiedCaptureBinding {
    pub(crate) fn from_validated_capture_context(
        batch: &NativeProvenReasoning,
        capture_session: &str,
        provider: &str,
    ) -> Result<Self, ReasoningFieldError> {
        let provider = match provider {
            "claude_code" => ReasoningProvider::ClaudeCode,
            "codex" => ReasoningProvider::Codex,
            _ => return Err(ReasoningFieldError::UnsupportedShape),
        };
        batch.validate(capture_session, provider)?;
        Ok(Self {
            capture_session: capture_session.to_owned(),
            source_session: batch.source_session.clone(),
            provider,
        })
    }
}

/// Metadata-only borrowed identity inputs. The native bytes are not present
/// in this view, and no constructor or mutable accessor is exposed.
pub(crate) struct BoundIdentityInputRef<'a> {
    original_wire_sha256: &'a [u8; 32],
    logical_turn: &'a str,
    source_binding_sha256: [u8; 32],
    issuer_tag: &'static str,
}

#[allow(dead_code)] // OG-19/20 consume this metadata-only preparatory view.
impl<'a> BoundIdentityInputRef<'a> {
    fn from_bound_item(proof: &'a NativeReasoningProof) -> Self {
        let mut source_binding = Sha256::new();
        source_binding.update(b"libra/reasoning-source-binding/v1\0");
        for field in [
            b"claude_code".as_slice(),
            proof.source_session.as_bytes(),
            &proof.message.to_le_bytes(),
            &proof.part.to_le_bytes(),
            match proof.field {
                ReasoningSourceKind::Signature => b"signature".as_slice(),
                ReasoningSourceKind::RedactedThinkingData => b"data".as_slice(),
                _ => b"unsupported".as_slice(),
            },
        ] {
            source_binding.update((field.len() as u64).to_le_bytes());
            source_binding.update(field);
        }
        Self {
            original_wire_sha256: &proof.original_wire_sha256,
            logical_turn: &proof.logical_turn,
            source_binding_sha256: source_binding.finalize().into(),
            issuer_tag: match proof.issuer {
                NativeReasoningIssuer::Unregistered => "unregistered",
                #[cfg(test)]
                NativeReasoningIssuer::SyntheticOnly => "synthetic-test-only-v1",
                #[cfg(test)]
                NativeReasoningIssuer::SyntheticUnknown => "unknown-test-only",
            },
        }
    }

    pub(crate) fn original_wire_sha256(&self) -> &[u8; 32] {
        self.original_wire_sha256
    }

    pub(crate) fn logical_turn(&self) -> &str {
        self.logical_turn
    }

    pub(crate) fn source_binding_sha256(&self) -> &[u8; 32] {
        &self.source_binding_sha256
    }

    pub(crate) fn issuer_tag(&self) -> &'static str {
        self.issuer_tag
    }
}

pub(crate) fn borrow_native_identity_bindings<'a>(
    batch: &'a NativeProvenReasoning,
    binding: &VerifiedCaptureBinding,
) -> Result<Vec<BoundIdentityInputRef<'a>>, ReasoningFieldError> {
    batch.validate(&binding.capture_session, binding.provider)?;
    if batch.source_session != binding.source_session {
        return Err(ReasoningFieldError::UnsupportedShape);
    }
    Ok(batch
        .proofs
        .iter()
        .map(BoundIdentityInputRef::from_bound_item)
        .collect())
}

impl NativeProvenReasoning {
    /// Source identity for a private helper context; this does not expose
    /// proof bytes or grant a caller authority to mint another native batch.
    pub(crate) fn transport_source_session(&self) -> &str {
        &self.source_session
    }

    pub(crate) fn pending_logical_turns(&self) -> impl Iterator<Item = &str> {
        self.proofs.iter().map(|proof| proof.logical_turn.as_str())
    }

    fn validate(
        &self,
        capture_session: &str,
        provider: ReasoningProvider,
    ) -> Result<(), ReasoningFieldError> {
        use crate::internal::ai::traces::{
            REASONING_ARTIFACT_MAX_ENTRIES, REASONING_ARTIFACT_TOTAL_MAX_BYTES,
        };
        if capture_session.is_empty()
            || self.capture_session != capture_session
            || self.capture_session != format!("claude__{}", self.source_session)
            || self.source_session.is_empty()
            || self.artifacts.is_empty()
            || self.artifacts.len() != self.proofs.len()
            || self.artifacts.len() > REASONING_ARTIFACT_MAX_ENTRIES
        {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        let mut total = 0_u64;
        let mut locators = BTreeSet::new();
        for (artifact, proof) in self.artifacts.iter().zip(&self.proofs) {
            let issuer_registered = match proof.issuer {
                NativeReasoningIssuer::Unregistered => false,
                #[cfg(test)]
                NativeReasoningIssuer::SyntheticOnly => {
                    proof.logical_turn == format!("synthetic-turn-{}", proof.message)
                }
                #[cfg(test)]
                NativeReasoningIssuer::SyntheticUnknown => false,
            };
            let field = match proof.field {
                ReasoningSourceKind::Signature => "signature",
                ReasoningSourceKind::RedactedThinkingData => "data",
                _ => return Err(ReasoningFieldError::UnsupportedShape),
            };
            // OG-16's synthetic positive uses the already admitted Claude
            // locator. No production OpenCode locator or issuer is added.
            let locator = format!(
                "claude_code:msg={}/part={}/metadata={field}",
                proof.message, proof.part
            );
            if !issuer_registered
                || provider != ReasoningProvider::ClaudeCode
                || proof.provider != provider
                || artifact.provider != provider
                || proof.source_session != self.source_session
                || proof.logical_turn.is_empty()
                || proof.logical_turn.len() > MAX_REASONING_TURN_KEY_BYTES
                || proof.logical_turn.chars().any(char::is_control)
                || artifact.bytes.is_empty()
                || artifact.source_kind != Some(proof.field)
                || artifact.bytes.verified_source() != (provider, proof.field)
                || artifact.locator != locator
                || !locators.insert(&artifact.locator)
                || !artifact_locator_is_valid(provider, Some(proof.field), &artifact.locator)
                || artifact.availability != ReasoningAvailability::OpaqueArchived
                || artifact.decrypt_capability != "none"
                || proof.original_wire_sha256
                    != <[u8; 32]>::from(Sha256::digest(artifact.bytes.as_bytes()))
            {
                return Err(ReasoningFieldError::UnsupportedShape);
            }
            total = total
                .checked_add(artifact.bytes.len() as u64)
                .ok_or(ReasoningFieldError::SourceTooLarge)?;
            if total > REASONING_ARTIFACT_TOTAL_MAX_BYTES {
                return Err(ReasoningFieldError::SourceTooLarge);
            }
        }
        Ok(())
    }

    pub(crate) fn artifact_params<'a>(
        &'a self,
        binding: &VerifiedCaptureBinding,
    ) -> Result<&'a [crate::internal::ai::traces::ReasoningArtifactParam], ReasoningFieldError>
    {
        self.validate(&binding.capture_session, binding.provider)?;
        if self.source_session != binding.source_session {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        Ok(&self.artifacts)
    }
}

/// A closed pending-wire projection of a previously validated native batch.
/// Deserializing this value never creates `NativeProvenReasoning` or opaque
/// authority. OG-21 must authenticate the complete envelope and membership
/// before asking this module to restore the original fixed-source proof.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingReasoningSourceDto {
    capture_session: String,
    source_session: String,
    provider: ReasoningProvider,
    items: Vec<ImportTransportItem>,
}

impl fmt::Debug for PendingReasoningSourceDto {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PendingReasoningSourceDto")
            .field("item_count", &self.items.len())
            .finish_non_exhaustive()
    }
}

impl PendingReasoningSourceDto {
    pub(crate) fn logical_turns(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .map(|item| item.proof.logical_turn.as_str())
    }

    #[allow(dead_code)] // OG-21 pairs this codec with the authenticated producer.
    pub(crate) fn from_bound_native(
        batch: &NativeProvenReasoning,
        binding: &VerifiedCaptureBinding,
    ) -> Result<Self, ReasoningFieldError> {
        batch.validate(&binding.capture_session, binding.provider)?;
        if batch.source_session != binding.source_session {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        if batch.artifacts.len() != batch.proofs.len() {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        let items = batch
            .artifacts
            .iter()
            .zip(&batch.proofs)
            .map(|(artifact, proof)| {
                let source_kind = artifact
                    .source_kind
                    .ok_or(ReasoningFieldError::UnsupportedShape)?;
                let issuer = match &proof.issuer {
                    NativeReasoningIssuer::Unregistered => ImportTransportIssuer::Unregistered,
                    #[cfg(test)]
                    NativeReasoningIssuer::SyntheticOnly => ImportTransportIssuer::SyntheticOnly,
                    #[cfg(test)]
                    NativeReasoningIssuer::SyntheticUnknown => {
                        ImportTransportIssuer::SyntheticUnknown
                    }
                };
                Ok(ImportTransportItem {
                    locator: artifact.locator.clone(),
                    provider: artifact.provider,
                    source_kind,
                    bytes_base64: BASE64_STANDARD.encode(artifact.bytes.as_bytes()),
                    proof: ImportTransportProof {
                        issuer,
                        provider: proof.provider,
                        source_session: proof.source_session.clone(),
                        message: proof.message,
                        part: proof.part,
                        field: proof.field,
                        original_wire_sha256: proof.original_wire_sha256,
                        logical_turn: proof.logical_turn.clone(),
                    },
                })
            })
            .collect::<Result<Vec<_>, ReasoningFieldError>>()?;
        let projection = Self {
            capture_session: batch.capture_session.clone(),
            source_session: batch.source_session.clone(),
            provider: binding.provider,
            items,
        };
        projection.validate_shape()?;
        Ok(projection)
    }

    /// Validate wire shape and exact bytes without elevating source authority.
    /// The original fixed issuer remains mandatory on the later restore path.
    pub(crate) fn validate_shape(&self) -> Result<(), ReasoningFieldError> {
        use crate::internal::ai::traces::{
            REASONING_ARTIFACT_MAX_ENTRIES, REASONING_ARTIFACT_TOTAL_MAX_BYTES,
        };

        if self.capture_session.is_empty()
            || self.capture_session.len() > 4096
            || self.source_session.is_empty()
            || self.source_session.len() > 4096
            || self.capture_session != format!("claude__{}", self.source_session)
            || self.provider != ReasoningProvider::ClaudeCode
            || self.items.is_empty()
            || self.items.len() > REASONING_ARTIFACT_MAX_ENTRIES
        {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        // Reject an over-budget transport before any field is decoded. The
        // remaining-byte check below also bounds one late field when earlier
        // fields have already consumed most of the raw artifact budget.
        let max_encoded =
            max_import_transport_base64_bytes(REASONING_ARTIFACT_TOTAL_MAX_BYTES, self.items.len())
                .map_err(|_| ReasoningFieldError::SourceTooLarge)?;
        let encoded_total = self.items.iter().try_fold(0_u64, |sum, item| {
            sum.checked_add(
                u64::try_from(item.bytes_base64.len())
                    .map_err(|_| ReasoningFieldError::SourceTooLarge)?,
            )
            .ok_or(ReasoningFieldError::SourceTooLarge)
        })?;
        if encoded_total > max_encoded {
            return Err(ReasoningFieldError::SourceTooLarge);
        }
        let mut total = 0_u64;
        let mut locators = BTreeSet::new();
        for item in &self.items {
            let field = match item.proof.field {
                ReasoningSourceKind::Signature => "signature",
                ReasoningSourceKind::RedactedThinkingData => "data",
                _ => return Err(ReasoningFieldError::UnsupportedShape),
            };
            let expected_locator = format!(
                "claude_code:msg={}/part={}/metadata={field}",
                item.proof.message, item.proof.part
            );
            if item.provider != self.provider
                || item.proof.provider != self.provider
                || item.source_kind != item.proof.field
                || item.proof.source_session != self.source_session
                || item.proof.logical_turn.is_empty()
                || item.proof.logical_turn.len() > MAX_REASONING_TURN_KEY_BYTES
                || item.proof.logical_turn.chars().any(char::is_control)
                || item.locator != expected_locator
                || !locators.insert(&item.locator)
                || !artifact_locator_is_valid(self.provider, Some(item.source_kind), &item.locator)
            {
                return Err(ReasoningFieldError::UnsupportedShape);
            }
            let remaining = REASONING_ARTIFACT_TOTAL_MAX_BYTES
                .checked_sub(total)
                .ok_or(ReasoningFieldError::SourceTooLarge)?;
            let bytes = decode_pending_base64_bounded(&item.bytes_base64, remaining)?;
            if bytes.is_empty()
                || <[u8; 32]>::from(Sha256::digest(&bytes)) != item.proof.original_wire_sha256
            {
                return Err(ReasoningFieldError::UnsupportedShape);
            }
            total = total
                .checked_add(bytes.len() as u64)
                .ok_or(ReasoningFieldError::SourceTooLarge)?;
            if total > REASONING_ARTIFACT_TOTAL_MAX_BYTES {
                return Err(ReasoningFieldError::SourceTooLarge);
            }
        }
        Ok(())
    }
}

/// Seal only an immutable borrow of an already bound native witness. The
/// resulting DTO remains untrusted when it is decoded on a later process.
pub(crate) fn seal_pending(
    native: &NativeProvenReasoning,
    binding: &VerifiedPendingBinding,
) -> Result<PendingReasoningSourceDto, ReasoningFieldError> {
    PendingReasoningSourceDto::from_bound_native(native, &binding.capture)
}

struct Sha256Writer(Sha256);

impl std::io::Write for Sha256Writer {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Restore only after the key owner reverified the original canonical
/// envelope and the checkpoint adapter checked the current terminal attempt.
/// The MAC proves integrity, while the fixed issuer validator below decides
/// whether the original source was actually native-proven.
pub(crate) fn restore_pending(
    dto: &PendingReasoningSourceDto,
    integrity: PendingIntegrityToken,
    current: &VerifiedCurrentBinding,
) -> Result<NativeProvenReasoning, ReasoningFieldError> {
    let mut writer = Sha256Writer(Sha256::new());
    serde_json::to_writer(&mut writer, dto).map_err(|_| ReasoningFieldError::UnsupportedShape)?;
    if <[u8; 32]>::from(writer.0.finalize()) != integrity.source_sha256
        || dto.capture_session != current.capture_session
        || dto.provider != current.provider
    {
        return Err(ReasoningFieldError::UnsupportedShape);
    }
    dto.validate_shape()?;

    use crate::internal::ai::traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES;
    let mut remaining = REASONING_ARTIFACT_TOTAL_MAX_BYTES;
    let mut artifacts = Vec::with_capacity(dto.items.len());
    let mut proofs = Vec::with_capacity(dto.items.len());
    for item in &dto.items {
        let bytes = decode_pending_base64_bounded(&item.bytes_base64, remaining)?;
        remaining = remaining
            .checked_sub(bytes.len() as u64)
            .ok_or(ReasoningFieldError::SourceTooLarge)?;
        artifacts.push(crate::internal::ai::traces::ReasoningArtifactParam {
            locator: item.locator.clone(),
            provider: item.provider,
            source_kind: Some(item.source_kind),
            availability: ReasoningAvailability::OpaqueArchived,
            decrypt_capability: "none".to_owned(),
            bytes: OpaqueEncryptedBytes {
                data: bytes,
                provider: item.provider,
                source_kind: item.source_kind,
            },
        });
        proofs.push(NativeReasoningProof {
            issuer: match item.proof.issuer {
                ImportTransportIssuer::Unregistered => NativeReasoningIssuer::Unregistered,
                #[cfg(test)]
                ImportTransportIssuer::SyntheticOnly => NativeReasoningIssuer::SyntheticOnly,
                #[cfg(test)]
                ImportTransportIssuer::SyntheticUnknown => NativeReasoningIssuer::SyntheticUnknown,
            },
            provider: item.proof.provider,
            source_session: item.proof.source_session.clone(),
            message: item.proof.message,
            part: item.proof.part,
            field: item.proof.field,
            original_wire_sha256: item.proof.original_wire_sha256,
            logical_turn: item.proof.logical_turn.clone(),
        });
    }
    let native = NativeProvenReasoning {
        capture_session: dto.capture_session.clone(),
        source_session: dto.source_session.clone(),
        artifacts,
        proofs,
    };
    native.validate(&current.capture_session, current.provider)?;
    Ok(native)
}

impl NativeProvenReasoning {
    /// Covered historical source turns may be needed to verify the original
    /// collection, but only this attempt's reserved claims become new objects.
    pub(crate) fn into_pending_claim_artifacts(
        self,
        current: &VerifiedCurrentBinding,
        claim_keys: &BTreeSet<String>,
    ) -> Result<Vec<crate::internal::ai::traces::ReasoningArtifactParam>, ReasoningFieldError> {
        self.validate(&current.capture_session, current.provider)?;
        Ok(self
            .artifacts
            .into_iter()
            .zip(self.proofs)
            .filter_map(|(artifact, proof)| {
                claim_keys.contains(&proof.logical_turn).then_some(artifact)
            })
            .collect())
    }
}

fn decode_pending_base64_bounded(
    encoded: &str,
    remaining_raw_bytes: u64,
) -> Result<Vec<u8>, ReasoningFieldError> {
    let max_encoded = max_import_transport_base64_bytes(remaining_raw_bytes, 1)
        .map_err(|_| ReasoningFieldError::SourceTooLarge)?;
    if u64::try_from(encoded.len()).map_err(|_| ReasoningFieldError::SourceTooLarge)? > max_encoded
    {
        return Err(ReasoningFieldError::SourceTooLarge);
    }
    let bytes = BASE64_STANDARD
        .decode(encoded.as_bytes())
        .map_err(|_| ReasoningFieldError::UnsupportedShape)?;
    if u64::try_from(bytes.len()).map_err(|_| ReasoningFieldError::SourceTooLarge)?
        > remaining_raw_bytes
    {
        return Err(ReasoningFieldError::SourceTooLarge);
    }
    if BASE64_STANDARD.encode(&bytes) != encoded {
        return Err(ReasoningFieldError::UnsupportedShape);
    }
    Ok(bytes)
}

fn max_import_transport_base64_bytes(
    raw_cap: u64,
    item_count: usize,
) -> Result<u64, ReasoningTransportError> {
    if item_count == 0 {
        return Err(ReasoningTransportError::Rejected);
    }
    raw_cap
        .checked_add(2)
        .and_then(|size| size.checked_div(3))
        .and_then(|size| size.checked_mul(4))
        // Base64 padding is charged for each item, not once for the whole
        // collection. This conservative margin admits a cap-sized raw batch
        // split among short fields while keeping the encoded total bounded.
        .and_then(|size| size.checked_add(4 * (item_count as u64 - 1)))
        .ok_or(ReasoningTransportError::Rejected)
}

fn import_transport_collection_digest(
    items: &[ImportTransportItem],
) -> Result<[u8; 32], ReasoningTransportError> {
    use crate::internal::ai::traces::{
        REASONING_ARTIFACT_MAX_ENTRIES, REASONING_ARTIFACT_TOTAL_MAX_BYTES,
    };

    if items.is_empty() || items.len() > REASONING_ARTIFACT_MAX_ENTRIES {
        return Err(ReasoningTransportError::Rejected);
    }
    let mut encoded_bytes = 0_u64;
    let max_base64_bytes =
        max_import_transport_base64_bytes(REASONING_ARTIFACT_TOTAL_MAX_BYTES, items.len())?;
    for item in items {
        let item_bytes = u64::try_from(item.bytes_base64.len())
            .map_err(|_| ReasoningTransportError::Rejected)?;
        encoded_bytes = encoded_bytes
            .checked_add(item_bytes)
            .ok_or(ReasoningTransportError::Rejected)?;
        if encoded_bytes > max_base64_bytes
            || item.locator.len() > 4096
            || item.proof.source_session.len() > 4096
            || item.proof.logical_turn.len() > 4096
        {
            return Err(ReasoningTransportError::Rejected);
        }
    }
    // Struct field order and array order are fixed. No map or floating-point
    // value can introduce a second representation of this collection.
    let canonical = serde_json::to_vec(items).map_err(|_| ReasoningTransportError::Rejected)?;
    Ok(Sha256::digest(canonical).into())
}

fn import_transport_mac(
    key: &ImportTransportKey,
    context_sha256: &[u8; 32],
    collection_sha256: &[u8; 32],
    item_count: usize,
) -> Result<[u8; 32], ReasoningTransportError> {
    let count = u64::try_from(item_count).map_err(|_| ReasoningTransportError::Rejected)?;
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, &key.0);
    let mut mac = hmac::Context::with_key(&signing_key);
    mac.update(b"libra/import-reasoning-transport/v1\0");
    mac.update(context_sha256);
    mac.update(collection_sha256);
    mac.update(&count.to_le_bytes());
    let tag = mac.sign();
    tag.as_ref()
        .try_into()
        .map_err(|_| ReasoningTransportError::Rejected)
}

/// Seal only a fixed-source native witness. A deserialized projection or
/// arbitrary provider/kind label cannot call this with a native batch.
pub(crate) fn seal_import_transport(
    key: ImportTransportKey,
    context: &ImportTransportContext,
    native: NativeProvenReasoning,
) -> Result<ImportTransportDto, ReasoningTransportError> {
    native
        .validate(&context.capture_session, context.provider)
        .map_err(|_| ReasoningTransportError::Rejected)?;
    if native.source_session != context.source_session {
        return Err(ReasoningTransportError::Rejected);
    }
    let items = native
        .artifacts
        .into_iter()
        .zip(native.proofs)
        .map(
            |(artifact, proof)| -> Result<ImportTransportItem, ReasoningTransportError> {
                let source_kind = artifact
                    .source_kind
                    .ok_or(ReasoningTransportError::Rejected)?;
                Ok(ImportTransportItem {
                    locator: artifact.locator,
                    provider: artifact.provider,
                    source_kind,
                    bytes_base64: BASE64_STANDARD.encode(artifact.bytes.as_bytes()),
                    proof: ImportTransportProof {
                        issuer: match proof.issuer {
                            NativeReasoningIssuer::Unregistered => {
                                ImportTransportIssuer::Unregistered
                            }
                            #[cfg(test)]
                            NativeReasoningIssuer::SyntheticOnly => {
                                ImportTransportIssuer::SyntheticOnly
                            }
                            #[cfg(test)]
                            NativeReasoningIssuer::SyntheticUnknown => {
                                ImportTransportIssuer::SyntheticUnknown
                            }
                        },
                        provider: proof.provider,
                        source_session: proof.source_session,
                        message: proof.message,
                        part: proof.part,
                        field: proof.field,
                        original_wire_sha256: proof.original_wire_sha256,
                        logical_turn: proof.logical_turn,
                    },
                })
            },
        )
        .collect::<Result<Vec<_>, _>>()?;
    let collection_sha256 = import_transport_collection_digest(&items)?;
    let context_sha256 = context.digest();
    let mac = import_transport_mac(&key, &context_sha256, &collection_sha256, items.len())?;
    Ok(ImportTransportDto {
        version: 1,
        context_sha256,
        items,
        collection_sha256,
        mac,
    })
}

/// Test-only malformed frame with a valid transport MAC. This exercises the
/// parent's native-proof rejection after transport authentication without
/// introducing a production DTO-to-native issuer.
#[cfg(test)]
pub(crate) fn invalidate_import_transport_native_proof_for_test(
    mut dto: ImportTransportDto,
    child_key: ImportTransportKey,
    context: &ImportTransportContext,
) -> Result<ImportTransportDto, ReasoningTransportError> {
    if dto.version != 1 || dto.context_sha256 != context.digest() {
        return Err(ReasoningTransportError::Rejected);
    }
    let first = dto
        .items
        .first_mut()
        .ok_or(ReasoningTransportError::Rejected)?;
    first.proof.issuer = ImportTransportIssuer::Unregistered;
    dto.collection_sha256 = import_transport_collection_digest(&dto.items)?;
    dto.mac = import_transport_mac(
        &child_key,
        &dto.context_sha256,
        &dto.collection_sha256,
        dto.items.len(),
    )?;
    Ok(dto)
}

/// Consume the parent's one-use key before rebuilding any private native
/// value. The MAC authenticates transport only; native authority is still
/// checked by the fixed issuer/coordinate validator below.
pub(crate) fn verify_import_transport(
    key: ImportTransportKey,
    context: ImportTransportContext,
    dto: ImportTransportDto,
) -> Result<NativeProvenReasoning, ReasoningTransportError> {
    if dto.version != 1 || dto.context_sha256 != context.digest() {
        return Err(ReasoningTransportError::Rejected);
    }
    let collection_sha256 = import_transport_collection_digest(&dto.items)?;
    if dto.collection_sha256 != collection_sha256 {
        return Err(ReasoningTransportError::Rejected);
    }
    let signing_key = hmac::Key::new(hmac::HMAC_SHA256, &key.0);
    // `ring::hmac::verify` performs the tag comparison in constant time.
    // Keep the expected tag check in the same domain rather than reflecting
    // a helper-provided value into an error or log.
    let mut verification_body = Vec::with_capacity(32 + 32 + 8 + 36);
    verification_body.extend_from_slice(b"libra/import-reasoning-transport/v1\0");
    verification_body.extend_from_slice(&dto.context_sha256);
    verification_body.extend_from_slice(&collection_sha256);
    verification_body.extend_from_slice(&(dto.items.len() as u64).to_le_bytes());
    hmac::verify(&signing_key, &verification_body, &dto.mac)
        .map_err(|_| ReasoningTransportError::Rejected)?;
    use crate::internal::ai::traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES;
    let mut total = 0_u64;
    let mut artifacts = Vec::with_capacity(dto.items.len());
    let mut proofs = Vec::with_capacity(dto.items.len());
    for item in dto.items {
        let bytes = BASE64_STANDARD
            .decode(item.bytes_base64.as_bytes())
            .map_err(|_| ReasoningTransportError::Rejected)?;
        total = total
            .checked_add(bytes.len() as u64)
            .ok_or(ReasoningTransportError::Rejected)?;
        if total > REASONING_ARTIFACT_TOTAL_MAX_BYTES {
            return Err(ReasoningTransportError::Rejected);
        }
        artifacts.push(crate::internal::ai::traces::ReasoningArtifactParam {
            locator: item.locator,
            provider: item.provider,
            source_kind: Some(item.source_kind),
            availability: ReasoningAvailability::OpaqueArchived,
            decrypt_capability: "none".to_owned(),
            bytes: OpaqueEncryptedBytes {
                data: bytes,
                provider: item.provider,
                source_kind: item.source_kind,
            },
        });
        proofs.push(NativeReasoningProof {
            issuer: match item.proof.issuer {
                ImportTransportIssuer::Unregistered => NativeReasoningIssuer::Unregistered,
                #[cfg(test)]
                ImportTransportIssuer::SyntheticOnly => NativeReasoningIssuer::SyntheticOnly,
                #[cfg(test)]
                ImportTransportIssuer::SyntheticUnknown => NativeReasoningIssuer::SyntheticUnknown,
            },
            provider: item.proof.provider,
            source_session: item.proof.source_session,
            message: item.proof.message,
            part: item.proof.part,
            field: item.proof.field,
            original_wire_sha256: item.proof.original_wire_sha256,
            logical_turn: item.proof.logical_turn,
        });
    }
    let native = NativeProvenReasoning {
        capture_session: context.capture_session.clone(),
        source_session: context.source_session.clone(),
        artifacts,
        proofs,
    };
    native
        .validate(&context.capture_session, context.provider)
        .map_err(|_| ReasoningTransportError::NativeProofRejected)?;
    Ok(native)
}

/// A verified one-to-one mapping of all native items into the authorized
/// import turn set. The checkpoint adapter alone may construct this after it
/// checks the durable source and canonical capture identity.
pub(crate) struct VerifiedImportTurnMapping {
    capture_session: String,
    source_session: String,
    provider: ReasoningProvider,
    turns: BTreeSet<String>,
}

impl VerifiedImportTurnMapping {
    pub(crate) fn from_validated_import_mapping(
        batch: &NativeProvenReasoning,
        capture_session: &str,
        provider: &str,
        source_session: &str,
        logical_turn_keys: &[&str],
    ) -> Result<Self, ReasoningFieldError> {
        let provider = match provider {
            "claude_code" => ReasoningProvider::ClaudeCode,
            "codex" => ReasoningProvider::Codex,
            "opencode" => ReasoningProvider::OpenCode,
            _ => return Err(ReasoningFieldError::UnsupportedShape),
        };
        batch.validate(capture_session, provider)?;
        if source_session.is_empty() || batch.source_session != source_session {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        let turns = logical_turn_keys
            .iter()
            .map(|turn| (*turn).to_owned())
            .collect::<BTreeSet<_>>();
        if turns.len() != logical_turn_keys.len()
            || turns.iter().any(String::is_empty)
            || batch
                .proofs
                .iter()
                .any(|proof| !turns.contains(&proof.logical_turn))
        {
            return Err(ReasoningFieldError::UnsupportedShape);
        }
        Ok(Self {
            capture_session: capture_session.to_owned(),
            source_session: source_session.to_owned(),
            provider,
            turns,
        })
    }
}

/// Move each ciphertext and its original proof into exactly one per-turn
/// batch. No ciphertext/proof clone, native issuer, or DTO constructor exists
/// in this ownership transform.
pub(crate) fn partition_import_reasoning(
    native: NativeProvenReasoning,
    mapping: &VerifiedImportTurnMapping,
) -> Result<BTreeMap<String, NativeProvenReasoning>, ReasoningFieldError> {
    native.validate(&mapping.capture_session, mapping.provider)?;
    if native.source_session != mapping.source_session
        || native
            .proofs
            .iter()
            .any(|proof| !mapping.turns.contains(&proof.logical_turn))
    {
        return Err(ReasoningFieldError::UnsupportedShape);
    }
    let mut partitioned = BTreeMap::<String, NativeProvenReasoning>::new();
    for (artifact, proof) in native.artifacts.into_iter().zip(native.proofs) {
        let batch = partitioned
            .entry(proof.logical_turn.clone())
            .or_insert_with(|| NativeProvenReasoning {
                capture_session: mapping.capture_session.clone(),
                source_session: mapping.source_session.clone(),
                artifacts: Vec::new(),
                proofs: Vec::new(),
            });
        batch.artifacts.push(artifact);
        batch.proofs.push(proof);
    }
    for batch in partitioned.values() {
        batch.validate(&mapping.capture_session, mapping.provider)?;
    }
    Ok(partitioned)
}

#[cfg(test)]
pub(crate) fn synthetic_native_preparation(
    ordinary: Vec<u8>,
    capture_session: &str,
    source_session: &str,
    fields: Vec<Vec<u8>>,
) -> PreparedReasoningSource {
    let fields = fields.into_iter().map(|bytes| (0, bytes)).collect();
    PreparedReasoningSource {
        ordinary: Zeroizing::new(ordinary),
        disposition: ReasoningSourceDisposition::NativeProven(synthetic_native_batch_for_turns(
            capture_session,
            source_session,
            fields,
        )),
        ordinary_partial_permit: None,
        coverage_fragment: None,
        classifier_rejected: false,
    }
}

/// Test-only native proof fixture whose message coordinate selects the
/// synthetic logical turn. Production issuers remain closed.
#[cfg(test)]
pub(crate) fn synthetic_native_batch_for_turns(
    capture_session: &str,
    source_session: &str,
    fields: Vec<(u64, Vec<u8>)>,
) -> NativeProvenReasoning {
    let mut artifacts = Vec::with_capacity(fields.len());
    let mut proofs = Vec::with_capacity(fields.len());
    for (part, (message, data)) in fields.into_iter().enumerate() {
        let digest = Sha256::digest(&data).into();
        artifacts.push(crate::internal::ai::traces::ReasoningArtifactParam {
            locator: format!("claude_code:msg={message}/part={part}/metadata=signature"),
            provider: ReasoningProvider::ClaudeCode,
            source_kind: Some(ReasoningSourceKind::Signature),
            availability: ReasoningAvailability::OpaqueArchived,
            decrypt_capability: "none".to_owned(),
            bytes: OpaqueEncryptedBytes {
                data,
                provider: ReasoningProvider::ClaudeCode,
                source_kind: ReasoningSourceKind::Signature,
            },
        });
        proofs.push(NativeReasoningProof {
            issuer: NativeReasoningIssuer::SyntheticOnly,
            provider: ReasoningProvider::ClaudeCode,
            source_session: source_session.to_owned(),
            message,
            part: part as u64,
            field: ReasoningSourceKind::Signature,
            original_wire_sha256: digest,
            logical_turn: format!("synthetic-turn-{message}"),
        });
    }
    NativeProvenReasoning {
        capture_session: capture_session.to_owned(),
        source_session: source_session.to_owned(),
        artifacts,
        proofs,
    }
}

#[cfg(test)]
pub(crate) mod carrier_test_support {
    use super::*;
    use crate::internal::ai::observed_agents::{AgentKind, AgentSessionCtx, ObservedAgent};
    pub(crate) const CANARY: &[u8] = b"OG16_CANDIDATE_PRIVATE_CANARY";
    pub(crate) const SOURCE: &str = "og16-native-source";
    pub(crate) const CAPTURE: &str = "claude__og16-native-source";
    #[derive(Clone, Copy)]
    pub(crate) enum Mode {
        None,
        Strip,
        Native,
        Malformed,
        ArtifactOversize,
        TooMany,
        Unmapped,
        Helper,
    }
    pub(crate) struct Adapter(pub(crate) Mode);
    pub(crate) static NONE: Adapter = Adapter(Mode::None);
    pub(crate) static STRIP: Adapter = Adapter(Mode::Strip);
    pub(crate) static NATIVE: Adapter = Adapter(Mode::Native);
    pub(crate) static MALFORMED: Adapter = Adapter(Mode::Malformed);
    pub(crate) static OVERSIZE: Adapter = Adapter(Mode::ArtifactOversize);
    pub(crate) static TOO_MANY: Adapter = Adapter(Mode::TooMany);
    pub(crate) static UNMAPPED: Adapter = Adapter(Mode::Unmapped);
    pub(crate) static HELPER: Adapter = Adapter(Mode::Helper);
    impl ObservedAgent for Adapter {
        fn provider_kind(&self) -> AgentKind {
            AgentKind::ClaudeCode
        }
        fn provider_name(&self) -> &'static str {
            "claude-code"
        }
        fn read_transcript(&self, _: &AgentSessionCtx) -> anyhow::Result<Option<Vec<u8>>> {
            panic!("the source preparation port must not reopen a source")
        }
        fn protected_dirs(&self) -> &'static [&'static str] {
            &[".claude"]
        }
        fn as_reasoning_source_preprocessor(&self) -> Option<&dyn ReasoningSourcePreprocessor> {
            (!matches!(self.0, Mode::None)).then_some(self)
        }
    }
    impl ReasoningSourcePreprocessor for Adapter {
        fn prepare_authorized_source(
            &self,
            mut ordinary: Vec<u8>,
        ) -> Result<PreparedReasoningSource, ReasoningFieldError> {
            let candidate = ordinary.windows(CANARY.len()).any(|w| w == CANARY);
            while let Some(index) = ordinary.windows(CANARY.len()).position(|w| w == CANARY) {
                ordinary.drain(index..index + CANARY.len());
            }
            match self.0 {
                Mode::None => panic!("disabled accessor must not prepare"),
                Mode::Strip if candidate => {
                    Ok(PreparedReasoningSource::unproven_candidates(ordinary))
                }
                Mode::Strip => Ok(PreparedReasoningSource::without_candidates(ordinary)),
                Mode::Malformed => Err(ReasoningFieldError::UnsupportedShape),
                Mode::Unmapped => Ok(PreparedReasoningSource {
                    ordinary: Zeroizing::new(ordinary),
                    disposition: ReasoningSourceDisposition::Unmapped,
                    ordinary_partial_permit: None,
                    coverage_fragment: None,
                    classifier_rejected: false,
                }),
                Mode::Helper => Ok(PreparedReasoningSource {
                    ordinary: Zeroizing::new(ordinary),
                    disposition: ReasoningSourceDisposition::HelperUnsupported,
                    ordinary_partial_permit: None,
                    coverage_fragment: None,
                    classifier_rejected: false,
                }),
                Mode::Native => Ok(synthetic_native_preparation(
                    ordinary,
                    CAPTURE,
                    SOURCE,
                    vec![CANARY.to_vec(), b"second-opaque-wire".to_vec()],
                )),
                Mode::ArtifactOversize => Ok(synthetic_native_preparation(
                    ordinary,
                    CAPTURE,
                    SOURCE,
                    vec![vec![
                        b'x';
                        crate::internal::ai::traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES
                            as usize
                            + 1
                    ]],
                )),
                Mode::TooMany => Ok(synthetic_native_preparation(
                    ordinary,
                    CAPTURE,
                    SOURCE,
                    vec![
                        vec![b'x'];
                        crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES + 1
                    ],
                )),
            }
        }
    }
    pub(crate) fn tamper_carrier_locator(disposition: &mut ReasoningSourceDisposition) {
        let ReasoningSourceDisposition::NativeProven(batch) = disposition else {
            panic!("synthetic native fixture")
        };
        batch.artifacts[0].locator = "claude_code:msg=0/part=100/metadata=signature".to_owned();
    }
}

/// Availability describes what the source proved, not whether capture failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningAvailability {
    ProviderVisible,
    EncryptedUnavailable,
    OpaqueArchived,
    NotPresent,
    UnsupportedShape,
}

/// Only registered provider tags can appear in safe reasoning metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningProvider {
    ClaudeCode,
    Codex,
    #[serde(rename = "opencode")]
    OpenCode,
}

/// A future provider's explicitly authorized, checkpoint-scoped capability.
/// Implementations must verify that their authorization covers checkpoint_id
/// and the ciphertext's provider. This registry currently grants none; callers
/// must retain encrypted_unavailable rather than infer a plaintext fallback.
/// No client key, config, environment or network discovery is performed here.
pub trait ReasoningDecryptor: Send + Sync {
    fn decrypt(
        &self,
        checkpoint_id: &str,
        ciphertext: &OpaqueEncryptedBytes,
    ) -> Result<ProviderVisibleText, ReasoningFieldError>;
}

/// No officially documented and authorized decryptor is implemented for any
/// observed provider. The capability is deliberately unavailable by default.
pub fn reasoning_decryptor(
    _provider: ReasoningProvider,
) -> Option<&'static dyn ReasoningDecryptor> {
    None
}

/// Metadata tags; no source text, paths, keys or original bytes are retained.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningSourceKind {
    Reasoning,
    Thinking,
    Summary,
    Signature,
    RedactedThinkingData,
    EncryptedContent,
}

/// Safe artifact locators contain only provider coordinates and a registered
/// source field. They are metadata, never a free-form provider string.
pub(crate) fn artifact_locator_is_valid(
    provider: ReasoningProvider,
    source_kind: Option<ReasoningSourceKind>,
    locator: &str,
) -> bool {
    fn ordinal(value: &str) -> bool {
        !value.is_empty() && value.len() <= 20 && value.bytes().all(|b| b.is_ascii_digit())
    }
    if locator.len() > 4096 {
        return false;
    }
    match (provider, source_kind) {
        (ReasoningProvider::ClaudeCode, Some(kind)) => {
            let field = match kind {
                ReasoningSourceKind::Signature => "signature",
                ReasoningSourceKind::RedactedThinkingData => "data",
                _ => return false,
            };
            let Some(rest) = locator.strip_prefix("claude_code:msg=") else {
                return false;
            };
            let Some((message, rest)) = rest.split_once("/part=") else {
                return false;
            };
            let Some((part, metadata)) = rest.split_once("/metadata=") else {
                return false;
            };
            ordinal(message) && ordinal(part) && metadata == field
        }
        (ReasoningProvider::Codex, Some(ReasoningSourceKind::EncryptedContent)) => {
            let Some(rest) = locator.strip_prefix("codex:ordinal=") else {
                return false;
            };
            if let Some((record, history)) = rest.split_once("/replacement_history[") {
                ordinal(record) && history.strip_suffix(']').is_some_and(ordinal)
            } else {
                ordinal(rest)
            }
        }
        // OpenCode has no source-proven opaque field at the current pin.
        _ => false,
    }
}

/// Diagnostic codes never contain a field path or source payload.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningWarning {
    UnsupportedShape,
}

/// A metadata-only classification. Persisting content is not part of RG-01.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ReasoningRecord {
    provider: ReasoningProvider,
    #[serde(skip_serializing_if = "Option::is_none")]
    source_kind: Option<ReasoningSourceKind>,
    availability: ReasoningAvailability,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<ReasoningWarning>,
}

impl ReasoningRecord {
    /// The classification remains available to callers without exposing data.
    pub fn availability(&self) -> ReasoningAvailability {
        self.availability
    }

    pub fn source_kind(&self) -> Option<ReasoningSourceKind> {
        self.source_kind
    }

    /// Unknown reasoning-like structure produces a payload-free warning.
    pub fn warning(&self) -> Option<ReasoningWarning> {
        self.warning
    }
}

/// Only the provider adapter can establish whether a block is reasoning-like.
/// Unrecognized, high-entropy fields do not acquire that status by inspection.
pub enum UnrecognizedReasoningField {
    Other,
    ReasoningLike,
}

/// Classify an undeclared source field without examining or retaining its bytes.
/// Declared provider fields are handled only after source-shape verification.
pub fn classify_unrecognized_field(
    provider: ReasoningProvider,
    field: UnrecognizedReasoningField,
    _raw_value: &[u8],
) -> ReasoningRecord {
    let (availability, warning) = match field {
        UnrecognizedReasoningField::Other => (ReasoningAvailability::NotPresent, None),
        UnrecognizedReasoningField::ReasoningLike => (
            ReasoningAvailability::UnsupportedShape,
            Some(ReasoningWarning::UnsupportedShape),
        ),
    };
    ReasoningRecord {
        provider,
        source_kind: None,
        availability,
        warning,
    }
}

/// Classify a provider-supported, readable reasoning field: the record is
/// marked `ProviderVisible` and the text is returned behind the gated
/// [`ProviderVisibleText`] type so only the typed-redaction path can read it.
/// The source text is stored as-is (no decode/re-encode); redaction and
/// projection run before any persistence.
///
/// `pub(crate)` (Codex re-review P2): arbitrary external callers must not be
/// able to mint readable reasoning; adapters live in-crate and the type gate
/// (private `inner`, fail-closed `Serialize`) keeps the variant unforgeable
/// from outside the crate.
#[allow(dead_code)] // RG-04 adapters (claude/opencode classifier wiring) consume this entry.
pub(crate) fn classify_provider_visible_reasoning(
    provider: ReasoningProvider,
    source_kind: ReasoningSourceKind,
    text: String,
) -> (ReasoningRecord, ProviderVisibleText) {
    let record = ReasoningRecord {
        provider,
        source_kind: Some(source_kind),
        availability: ReasoningAvailability::ProviderVisible,
        warning: None,
    };
    let text = ProviderVisibleText::from_classification(&record, text);
    match text {
        Some(text) => (record, text),
        // INVARIANT: the record was constructed two lines above with
        // `availability: ReasoningAvailability::ProviderVisible`, so
        // `from_classification` cannot return None for it.
        None => unreachable!("record was just constructed with ProviderVisible availability"),
    }
}

/// Provider-visible reasoning text, constructible only through the
/// classification path that proved `ReasoningAvailability::ProviderVisible`.
///
/// The inner string is private and the type has no `From<String>`/`Display`
/// impl: external code cannot mint provider-visible reasoning by struct
/// literal or conversion (RG-04 compile_fail gate). Projection and redaction
/// read it via [`ProviderVisibleText::as_str`].
///
/// # Construction gate (RG-04 compile_fail)
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::coverage::SemanticRecord;
/// use libra::internal::ai::observed_agents::reasoning::{
///     ProviderVisibleText, ReasoningProvider, ReasoningSourceKind,
/// };
/// // The `inner` field is private and there is no public constructor or
/// // `From<String>` impl, so this struct literal cannot compile:
/// let text = ProviderVisibleText {
///     inner: "minted reasoning".to_string(),
/// };
/// let _ = SemanticRecord::Reasoning {
///     provider: ReasoningProvider::ClaudeCode,
///     source_kind: Some(ReasoningSourceKind::Reasoning),
///     text,
/// };
/// ```
#[derive(Clone, Eq, PartialEq)]
// The payload wipes on drop and before typed redaction replaces it. This is
// an in-process allocation guarantee, not a guarantee about allocator copies.
pub struct ProviderVisibleText {
    inner: String,
}

impl Drop for ProviderVisibleText {
    fn drop(&mut self) {
        self.inner.zeroize();
    }
}

impl fmt::Debug for ProviderVisibleText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Codex re-review P1: `{:?}` must never leak the (possibly
        // unredacted) inner reasoning text into logs or diagnostics.
        f.debug_struct("ProviderVisibleText")
            .field("len", &self.inner.len())
            .field("inner", &"<redacted>".to_string())
            .finish()
    }
}

impl Serialize for ProviderVisibleText {
    fn serialize<S: serde::Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
        // Codex re-review P1: serialization is deliberately unsupported. The
        // derived SemanticRecord::Serialize would otherwise provide a
        // pre-redaction persistence bypass; the sanctioned outputs are the
        // canonical writer and safe_turn_projection, which read `as_str()`
        // only after typed redaction has run.
        Err(serde::ser::Error::custom(
            "provider-visible reasoning text must be redacted first; use the canonical/projection writers",
        ))
    }
}

impl<'de> Deserialize<'de> for ProviderVisibleText {
    fn deserialize<D: Deserializer<'de>>(_deserializer: D) -> Result<Self, D::Error> {
        // Fail closed: provider-visible reasoning text can only be minted by
        // the classification path. Deserializing one from persisted JSON
        // would bypass the gate.
        Err(de::Error::custom(
            "provider-visible reasoning text cannot be deserialized; use the classification path",
        ))
    }
}

impl ProviderVisibleText {
    /// Constructible only from the classification path: the record must
    /// already carry `ReasoningAvailability::ProviderVisible`. Returns `None`
    /// when the availability is not `ProviderVisible` — the gate holds in
    /// release builds too (Codex re-review P1, not just a debug assert).
    #[allow(dead_code)] // RG-04 adapters consume this gated constructor.
    pub(crate) fn from_classification(record: &ReasoningRecord, mut text: String) -> Option<Self> {
        if record.availability() != ReasoningAvailability::ProviderVisible {
            text.zeroize();
            return None;
        }
        Some(Self { inner: text })
    }

    pub fn as_str(&self) -> &str {
        &self.inner
    }

    /// In-crate trusted path for the typed-redaction step: redaction
    /// rewrites the text in place through the shared Redactor and returns its
    /// report. The inner string can only ever be redacted, never replaced
    /// with arbitrary text (Codex re-review P1).
    pub(crate) fn redact_with(
        &mut self,
        redactor: &super::redaction::Redactor,
    ) -> super::redaction::RedactionReport {
        let (bytes, report) = redactor.redact(self.inner.as_bytes());
        self.inner.zeroize();
        self.inner = String::from_utf8_lossy(bytes.as_ref()).into_owned();
        report
    }
}

/// Errors do not include source field names, values, or raw provider JSON.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ReasoningFieldError {
    #[error("reasoning source exceeds its 16 MiB byte or structural budget; use a smaller export")]
    SourceTooLarge,
    #[error("reasoning source is not a valid provider record; check the pinned provider export")]
    MalformedSource,
    #[error("reasoning field has an unsupported shape; update the pinned provider contract")]
    UnsupportedShape,
}

/// A verified in-memory source field, not an archived or decrypted artifact.
#[derive(Debug)]
pub struct VerifiedOpaqueReasoning {
    record: ReasoningRecord,
    bytes: OpaqueEncryptedBytes,
}

impl VerifiedOpaqueReasoning {
    pub fn record(&self) -> ReasoningRecord {
        self.record
    }

    pub fn into_opaque(self) -> OpaqueEncryptedBytes {
        self.bytes
    }
}

#[allow(dead_code)]
struct UniqueJsonKeys;

impl<'de> Deserialize<'de> for UniqueJsonKeys {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct UniqueVisitor;

        impl<'de> Visitor<'de> for UniqueVisitor {
            type Value = UniqueJsonKeys;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a JSON value with unique object keys")
            }

            fn visit_bool<E: de::Error>(self, _value: bool) -> Result<Self::Value, E> {
                Ok(UniqueJsonKeys)
            }

            fn visit_i64<E: de::Error>(self, _value: i64) -> Result<Self::Value, E> {
                Ok(UniqueJsonKeys)
            }

            fn visit_u64<E: de::Error>(self, _value: u64) -> Result<Self::Value, E> {
                Ok(UniqueJsonKeys)
            }

            fn visit_f64<E: de::Error>(self, _value: f64) -> Result<Self::Value, E> {
                Ok(UniqueJsonKeys)
            }

            fn visit_str<E: de::Error>(self, _value: &str) -> Result<Self::Value, E> {
                Ok(UniqueJsonKeys)
            }

            fn visit_unit<E: de::Error>(self) -> Result<Self::Value, E> {
                Ok(UniqueJsonKeys)
            }

            fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
                while seq.next_element::<UniqueJsonKeys>()?.is_some() {}
                Ok(UniqueJsonKeys)
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut keys = BTreeSet::new();
                while let Some(key) = map.next_key::<String>()? {
                    if !keys.insert(key) {
                        return Err(de::Error::custom("duplicate JSON key in reasoning source"));
                    }
                    map.next_value::<UniqueJsonKeys>()?;
                }
                Ok(UniqueJsonKeys)
            }
        }

        deserializer.deserialize_any(UniqueVisitor)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
// RG-02 wiring consumes this item; see verify_claude_encrypted_field.
#[allow(dead_code)]
struct ClaudeAssistantRecord<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    message: ClaudeAssistantMessage<'a>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
// RG-02 wiring consumes this item; see verify_claude_encrypted_field.
#[allow(dead_code)]
struct ClaudeAssistantMessage<'a> {
    role: &'a str,
    #[serde(borrow)]
    content: Vec<&'a RawValue>,
}

#[derive(Deserialize)]
// RG-02 wiring consumes this item; see verify_claude_encrypted_field.
#[allow(dead_code)]
struct ClaudeBlockTag<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
// RG-02 wiring consumes this item; see verify_claude_encrypted_field.
#[allow(dead_code)]
struct ClaudeThinkingBlock<'a> {
    #[serde(rename = "type")]
    _kind: &'a str,
    #[serde(rename = "thinking", borrow)]
    thinking: &'a RawValue,
    #[serde(borrow)]
    signature: &'a RawValue,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
// RG-02 wiring consumes this item; see verify_claude_encrypted_field.
#[allow(dead_code)]
struct ClaudeRedactedThinkingBlock<'a> {
    #[serde(rename = "type")]
    _kind: &'a str,
    #[serde(borrow)]
    data: &'a RawValue,
}

#[allow(dead_code)] // RG-02 wiring consumes this helper; see verify_claude_encrypted_field.
fn json_string_field_bytes(raw: &RawValue) -> Option<&[u8]> {
    let bytes = raw.get().as_bytes();
    if bytes.len() < 2 || bytes.first() != Some(&b'"') || bytes.last() != Some(&b'"') {
        return None;
    }
    Some(&bytes[1..bytes.len() - 1])
}

/// One ordinary Claude attached field recovered from original JSONL bytes.
///
/// `value` is the decoded JSON string. `availability` stays `unproven`:
/// this projection is not a native artifact and is not lexical ciphertext.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeAttachedField {
    pub provider: &'static str,
    pub session_id: String,
    pub turn_key: String,
    pub line_ordinal: u32,
    pub record_ordinal: u32,
    pub content_index: u32,
    pub field: &'static str,
    pub value: String,
    pub availability: &'static str,
}

/// Indexed view of ordinary Claude thinking attachments.
///
/// The durable carrier is the original JSONL. This view does not re-serialize
/// the line and does not construct `OpaqueEncryptedBytes`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ClaudeAttachedProjection {
    pub fields: Vec<ClaudeAttachedField>,
    pub readable: Vec<String>,
    pub duplicate_rejected: u32,
    /// The file leaf was not the authorized session, so no field is associated.
    pub refused: bool,
}

impl ClaudeAttachedProjection {
    /// A non-empty thinking string or a non-empty attached field.
    pub fn data_obtained(&self) -> bool {
        self.readable.iter().any(|text| !text.is_empty())
            || self.fields.iter().any(|field| !field.value.is_empty())
    }

    /// Data was obtained and at least one thinking string is non-empty.
    pub fn readable_obtained(&self) -> bool {
        self.data_obtained() && self.readable.iter().any(|text| !text.is_empty())
    }
}

const CLAUDE_ATTACHED_PROVIDER: &str = "claude_code";
const CLAUDE_ATTACHED_AVAILABILITY: &str = "unproven";

/// Project signature and redacted `data` from original Claude JSONL bytes.
///
/// Extra outer keys stay on the source. Empty strings are not payloads.
/// Duplicate-key lines contribute no fields and increment `duplicate_rejected`.
pub fn project_claude_attached_fields(source: &[u8]) -> ClaudeAttachedProjection {
    project_claude_attached_fields_until(source, None).unwrap_or_default()
}

/// Same reader as [`project_claude_attached_fields`]. `None` means the deadline
/// elapsed before the scan finished. No retained bytes are deleted.
pub fn project_claude_attached_fields_until(
    source: &[u8],
    deadline: Option<Instant>,
) -> Option<ClaudeAttachedProjection> {
    project_claude_attached_fields_for(source, "", deadline)
}

/// Associate fields only when `file_name` is the JSONL leaf for `session_id`.
/// A mismatch returns no fields. Calling it again still refuses.
pub fn authorize_claude_attached_fields(
    file_name: &str,
    session_id: &str,
    source: &[u8],
) -> ClaudeAttachedProjection {
    if !super::coverage::claude_jsonl_session_matches(file_name, session_id) {
        return ClaudeAttachedProjection {
            refused: true,
            ..ClaudeAttachedProjection::default()
        };
    }
    project_claude_attached_fields_for(source, session_id, None).unwrap_or_default()
}

fn project_claude_attached_fields_for(
    source: &[u8],
    session_id: &str,
    deadline: Option<Instant>,
) -> Option<ClaudeAttachedProjection> {
    let mut projection = ClaudeAttachedProjection::default();
    let mut line_ordinal: u32 = 0;
    for line in source.split(|byte| *byte == b'\n') {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            return None;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let entry = match parse_canon_value(line) {
            Ok(value) => value,
            Err(error) => {
                if error.to_string().contains("duplicate") {
                    projection.duplicate_rejected = projection.duplicate_rejected.saturating_add(1);
                }
                line_ordinal = line_ordinal.saturating_add(1);
                continue;
            }
        };
        let turn_key = entry
            .get("uuid")
            .and_then(CanonValue::as_str)
            .filter(|uuid| !uuid.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| line_ordinal.to_string());
        if let Some(CanonValue::Array(blocks)) = entry
            .get("message")
            .and_then(|message| message.get("content"))
        {
            for (record_ordinal, block) in blocks.iter().enumerate() {
                absorb_claude_attached_block(
                    &mut projection,
                    session_id,
                    &turn_key,
                    line_ordinal,
                    u32::try_from(record_ordinal).unwrap_or(u32::MAX),
                    block,
                );
            }
        }
        line_ordinal = line_ordinal.saturating_add(1);
    }
    Some(projection)
}

fn non_empty_string(block: &CanonValue, key: &str) -> Option<String> {
    match block.get(key) {
        Some(CanonValue::Str(text)) if !text.is_empty() => Some(text.clone()),
        _ => None,
    }
}

fn absorb_claude_attached_block(
    projection: &mut ClaudeAttachedProjection,
    session_id: &str,
    turn_key: &str,
    line_ordinal: u32,
    record_ordinal: u32,
    block: &CanonValue,
) {
    let Some(kind) = block.get("type").and_then(CanonValue::as_str) else {
        return;
    };
    match kind {
        "thinking" => {
            if let Some(text) = non_empty_string(block, "thinking") {
                projection.readable.push(text);
            }
            if let Some(value) = non_empty_string(block, "signature") {
                push_attached_field(
                    projection,
                    session_id,
                    turn_key,
                    line_ordinal,
                    record_ordinal,
                    "signature",
                    value,
                );
            }
        }
        "redacted_thinking" => {
            if let Some(value) = non_empty_string(block, "data") {
                push_attached_field(
                    projection,
                    session_id,
                    turn_key,
                    line_ordinal,
                    record_ordinal,
                    "data",
                    value,
                );
            }
        }
        _ => {}
    }
}

fn push_attached_field(
    projection: &mut ClaudeAttachedProjection,
    session_id: &str,
    turn_key: &str,
    line_ordinal: u32,
    record_ordinal: u32,
    field: &'static str,
    value: String,
) {
    let content_index = u32::try_from(projection.fields.len()).unwrap_or(u32::MAX);
    projection.fields.push(ClaudeAttachedField {
        provider: CLAUDE_ATTACHED_PROVIDER,
        session_id: session_id.to_owned(),
        turn_key: turn_key.to_owned(),
        line_ordinal,
        record_ordinal,
        content_index,
        field,
        value,
        availability: CLAUDE_ATTACHED_AVAILABILITY,
    });
}

/// Run the ordinary attached-field reader. A duplicate-key line that did not
/// already poison a turn marks the tail incomplete. Retained bytes stay put.
pub(crate) fn note_claude_attached_duplicates(turns: &mut [NormalizedTurn], source: &[u8]) {
    if project_claude_attached_fields(source).duplicate_rejected > 0 {
        poison_unmarked_claude_tail(turns);
    }
}

/// Deadline-bound form of [`note_claude_attached_duplicates`].
pub(crate) fn note_claude_attached_duplicates_until(
    turns: &mut [NormalizedTurn],
    source: &[u8],
    deadline: Instant,
) -> Option<()> {
    let projection = project_claude_attached_fields_until(source, Some(deadline))?;
    if projection.duplicate_rejected > 0 {
        poison_unmarked_claude_tail(turns);
    }
    Some(())
}

fn poison_unmarked_claude_tail(turns: &mut [NormalizedTurn]) {
    if turns
        .iter()
        .all(|turn| turn.completeness == Completeness::Complete)
        && let Some(turn) = turns.last_mut()
    {
        turn.completeness = Completeness::Incomplete;
    }
}

/// Verify a single encrypted field in an authorized Claude assistant JSONL record.
///
/// The caller must first authorize and pin the source through the existing
/// provider-root/import path. This function verifies the official content
/// block shape; it does not authenticate a caller-supplied JSON document.
/// This type-contract verifier accepts the minimal synthetic envelope only;
/// real session metadata envelopes belong to the provider adapter contract.
/// Unknown or wrong-typed reasoning blocks fail closed without logging data.
/// No OpenCode field is registered: the pinned 2.0.24 `state` is an open record, not
/// proof of ciphertext. A selected non-reasoning block is also rejected;
/// callers classify ordinary text/tool blocks before invoking this verifier.
///
/// Visibility is `pub(crate)` (Codex re-review P1): the un-forgeable byte-type
/// guarantee requires that opaque construction is reachable only through the
/// authorized pipeline, not from arbitrary external callers synthesizing a
/// matching record.
#[allow(dead_code)] // RG-02 wiring consumes the verifier; see the structs above.
pub(crate) fn verify_claude_encrypted_field(
    source: &[u8],
    content_index: usize,
) -> Result<VerifiedOpaqueReasoning, ReasoningFieldError> {
    if u64::try_from(source.len()).map_or(true, |len| len > TRANSCRIPT_READ_HARD_CAP_BYTES) {
        return Err(ReasoningFieldError::SourceTooLarge);
    }
    serde_json::from_slice::<UniqueJsonKeys>(source)
        .map_err(|_| ReasoningFieldError::MalformedSource)?;
    let envelope: ClaudeAssistantRecord<'_> =
        serde_json::from_slice(source).map_err(|_| ReasoningFieldError::MalformedSource)?;
    if envelope.kind != "assistant" || envelope.message.role != "assistant" {
        return Err(ReasoningFieldError::UnsupportedShape);
    }
    let block = envelope
        .message
        .content
        .get(content_index)
        .ok_or(ReasoningFieldError::UnsupportedShape)?;
    let tag: ClaudeBlockTag<'_> =
        serde_json::from_str(block.get()).map_err(|_| ReasoningFieldError::UnsupportedShape)?;
    let (kind, raw) = match tag.kind {
        "thinking" => {
            let thinking: ClaudeThinkingBlock<'_> = serde_json::from_str(block.get())
                .map_err(|_| ReasoningFieldError::UnsupportedShape)?;
            if json_string_field_bytes(thinking.thinking).is_none() {
                return Err(ReasoningFieldError::UnsupportedShape);
            }
            (ReasoningSourceKind::Signature, thinking.signature)
        }
        "redacted_thinking" => {
            let redacted: ClaudeRedactedThinkingBlock<'_> = serde_json::from_str(block.get())
                .map_err(|_| ReasoningFieldError::UnsupportedShape)?;
            (ReasoningSourceKind::RedactedThinkingData, redacted.data)
        }
        _ => return Err(ReasoningFieldError::UnsupportedShape),
    };
    let data = json_string_field_bytes(raw).ok_or(ReasoningFieldError::UnsupportedShape)?;
    Ok(VerifiedOpaqueReasoning {
        record: ReasoningRecord {
            provider: ReasoningProvider::ClaudeCode,
            source_kind: Some(kind),
            availability: ReasoningAvailability::EncryptedUnavailable,
            warning: None,
        },
        bytes: OpaqueEncryptedBytes {
            data: data.to_vec(),
            provider: ReasoningProvider::ClaudeCode,
            source_kind: kind,
        },
    })
}

/// Verified provider ciphertext, never a redacted transcript or JSON value.
///
/// RG-01 exposes no conversion from arbitrary bytes. Construction happens
/// only inside this module after `verify_claude_encrypted_field` checks a
/// provider-declared source field. RG-02 will add its artifact sink separately.
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::reasoning::OpaqueEncryptedBytes;
/// let _: OpaqueEncryptedBytes = Vec::<u8>::new().into();
/// ```
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::reasoning::OpaqueEncryptedBytes;
/// let _: OpaqueEncryptedBytes = (&b"ciphertext"[..]).into();
/// ```
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::reasoning::OpaqueEncryptedBytes;
/// let payload: OpaqueEncryptedBytes = unreachable!();
/// let _ = serde_json::to_vec(&payload);
/// ```
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::reasoning::OpaqueEncryptedBytes;
/// let payload: OpaqueEncryptedBytes = unreachable!();
/// let _ = format!("{payload}");
/// ```
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::{RedactedBytes, reasoning::OpaqueEncryptedBytes};
/// let payload: OpaqueEncryptedBytes = unreachable!();
/// let _: RedactedBytes = payload.into();
/// ```
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::{coverage_digest_hex, reasoning::OpaqueEncryptedBytes};
/// let payload: OpaqueEncryptedBytes = unreachable!();
/// let _ = coverage_digest_hex(&[payload]);
/// ```
///
/// ```compile_fail
/// use libra::internal::ai::observed_agents::{Redactor, reasoning::OpaqueEncryptedBytes};
/// let payload: OpaqueEncryptedBytes = unreachable!();
/// let _ = Redactor::new_default().redact(&payload);
/// ```
#[derive(Clone)]
pub struct OpaqueEncryptedBytes {
    data: Vec<u8>,
    provider: ReasoningProvider,
    source_kind: ReasoningSourceKind,
}

impl OpaqueEncryptedBytes {
    /// Used only for isolated contract tests until verified provider wiring.
    /// `pub(crate)` so sibling lib test modules (history.rs RG-02 artifact
    /// tests) can construct verified-looking ciphertext for the writer.
    #[cfg(test)]
    pub(crate) fn from_verified_test_field(data: Vec<u8>) -> Self {
        Self {
            data,
            provider: ReasoningProvider::ClaudeCode,
            source_kind: ReasoningSourceKind::Signature,
        }
    }

    pub(crate) fn verified_source(&self) -> (ReasoningProvider, ReasoningSourceKind) {
        (self.provider, self.source_kind)
    }

    pub fn len(&self) -> usize {
        self.data.len()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Hash the original wire bytes without decoding or serializing them.
    pub fn sha256_hex(&self) -> String {
        hex::encode(Sha256::digest(&self.data))
    }

    /// Byte-exact artifact view for the RG-02 checkpoint writer. `pub(crate)`
    /// so only the in-crate artifact sink (never an external caller) can
    /// obtain the raw ciphertext bytes for a single object write.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.data
    }
}

impl fmt::Debug for OpaqueEncryptedBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpaqueEncryptedBytes")
            .field("byte_len", &self.data.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    fn transport_context() -> ImportTransportContext {
        ImportTransportContext::new(
            [1; 32],
            [2; 32],
            [3; 32],
            [4; 32],
            [5; 32],
            ReasoningProvider::ClaudeCode,
            carrier_test_support::CAPTURE.to_owned(),
            carrier_test_support::SOURCE.to_owned(),
        )
        .expect("valid synthetic context")
    }

    fn transport_native() -> NativeProvenReasoning {
        let prepared = synthetic_native_preparation(
            b"ordinary".to_vec(),
            carrier_test_support::CAPTURE,
            carrier_test_support::SOURCE,
            vec![b"private-wire-0".to_vec(), b"private-wire-1".to_vec()],
        );
        let ReasoningSourceDisposition::NativeProven(native) = prepared.disposition else {
            panic!("synthetic fixture must have a native witness")
        };
        native
    }

    #[test]
    fn reasoning_transport_key_is_single_use() {
        let parent_key = ImportTransportKey::random().expect("OS randomness");
        let encoded = parent_key.encode_for_helper();
        assert!(!format!("{encoded:?}").contains(&hex::encode(encoded.0)));
        let wire = serde_json::to_vec(&encoded).expect("encode private helper control");
        let decoded: EncodedTransportKey =
            serde_json::from_slice(&wire).expect("decode private helper control");
        let child_key = decoded.into_key().expect("valid helper key");
        let context = transport_context();
        let dto = seal_import_transport(child_key, &context, transport_native())
            .expect("synthetic native seal");
        let restored = verify_import_transport(parent_key, context, dto)
            .expect("authenticated native restore");
        assert_eq!(restored.artifacts.len(), 2);
        assert_eq!(restored.proofs.len(), 2);
        assert_eq!(restored.artifacts[0].bytes.as_bytes(), b"private-wire-0");
        assert_eq!(restored.artifacts[1].bytes.as_bytes(), b"private-wire-1");
    }

    #[test]
    fn reasoning_transport_rejects_bound_mutation_and_signed_missing_proof() {
        let context = transport_context();
        let dto = seal_import_transport(ImportTransportKey([7; 32]), &context, transport_native())
            .expect("synthetic native seal");
        let wrong_context = ImportTransportContext::new(
            [1; 32],
            [2; 32],
            [9; 32],
            [4; 32],
            [5; 32],
            ReasoningProvider::ClaudeCode,
            carrier_test_support::CAPTURE.to_owned(),
            carrier_test_support::SOURCE.to_owned(),
        )
        .expect("valid changed context");
        let frame = serde_json::to_vec(&dto).expect("encode transport");
        let copied: ImportTransportDto = serde_json::from_slice(&frame).expect("decode transport");
        assert!(matches!(
            verify_import_transport(ImportTransportKey([7; 32]), wrong_context, copied),
            Err(ReasoningTransportError::Rejected)
        ));

        let missing_proof: ImportTransportDto =
            serde_json::from_slice(&frame).expect("decode transport");
        let missing_proof = invalidate_import_transport_native_proof_for_test(
            missing_proof,
            ImportTransportKey([7; 32]),
            &context,
        )
        .expect("test-only malformed signed frame");
        assert!(!missing_proof.items.is_empty());
        assert!(matches!(
            verify_import_transport(ImportTransportKey([7; 32]), context, missing_proof),
            Err(ReasoningTransportError::NativeProofRejected)
        ));
    }

    #[test]
    fn reasoning_transport_base64_budget_accounts_for_per_item_padding() {
        // Three fields totaling six bytes encode as 4 + 4 + 8 bytes. A
        // whole-collection padding estimate would incorrectly allow only 8.
        let allowed = max_import_transport_base64_bytes(6, 3).expect("bounded frame");
        let actual = [1_usize, 1, 4]
            .into_iter()
            .map(|len| BASE64_STANDARD.encode(vec![0; len]).len() as u64)
            .sum::<u64>();
        assert!(actual <= allowed);
        assert!(actual > max_import_transport_base64_bytes(6, 1).expect("one field"));
    }

    #[test]
    fn reasoning_pending_base64_rejects_over_budget_before_decode() {
        let over_budget_and_invalid = "!".repeat(9);
        assert!(matches!(
            decode_pending_base64_bounded(&over_budget_and_invalid, 3),
            Err(ReasoningFieldError::SourceTooLarge)
        ));
        let encoded = BASE64_STANDARD.encode(b"four");
        assert!(matches!(
            decode_pending_base64_bounded(&encoded, 3),
            Err(ReasoningFieldError::SourceTooLarge)
        ));
        assert_eq!(decode_pending_base64_bounded(&encoded, 4).unwrap(), b"four");
    }

    #[test]
    fn reasoning_pending_source_key_and_empty_field_are_rejected() {
        let empty_native = synthetic_native_batch_for_turns(
            carrier_test_support::CAPTURE,
            carrier_test_support::SOURCE,
            vec![(0, Vec::new())],
        );
        assert!(matches!(
            VerifiedCaptureBinding::from_validated_capture_context(
                &empty_native,
                carrier_test_support::CAPTURE,
                "claude_code"
            ),
            Err(ReasoningFieldError::UnsupportedShape)
        ));
        let native = synthetic_native_batch_for_turns(
            carrier_test_support::CAPTURE,
            carrier_test_support::SOURCE,
            vec![(0, b"private-wire".to_vec())],
        );
        let binding = VerifiedCaptureBinding::from_validated_capture_context(
            &native,
            carrier_test_support::CAPTURE,
            "claude_code",
        )
        .unwrap();
        let mut dto = PendingReasoningSourceDto::from_bound_native(&native, &binding).unwrap();
        dto.items[0].proof.logical_turn = "control\nkey".into();
        assert!(matches!(
            dto.validate_shape(),
            Err(ReasoningFieldError::UnsupportedShape)
        ));
        dto.items[0].proof.logical_turn = "k".repeat(MAX_REASONING_TURN_KEY_BYTES + 1);
        assert!(matches!(
            dto.validate_shape(),
            Err(ReasoningFieldError::UnsupportedShape)
        ));
        dto.items[0].proof.logical_turn = "synthetic-turn-0".into();
        dto.items[0].bytes_base64.clear();
        dto.items[0].proof.original_wire_sha256 = Sha256::digest([]).into();
        assert!(matches!(
            dto.validate_shape(),
            Err(ReasoningFieldError::UnsupportedShape)
        ));
    }

    #[test]
    fn reasoning_transport_error_display_is_stable() {
        assert_eq!(
            ReasoningTransportError::KeyUnavailable.to_string(),
            "cannot create a private import transport key; retry the import"
        );
        assert_eq!(
            ReasoningTransportError::InvalidControl.to_string(),
            "invalid private import transport control; retry the import"
        );
        assert_eq!(
            ReasoningTransportError::Rejected.to_string(),
            "private import reasoning transport was rejected; retry the import"
        );
        assert_eq!(
            ReasoningTransportError::NativeProofRejected.to_string(),
            "private import native reasoning proof was rejected; retry the import"
        );
    }

    #[test]
    fn reasoning_import_partition_moves_each_existing_item_once() {
        let native = synthetic_native_batch_for_turns(
            carrier_test_support::CAPTURE,
            carrier_test_support::SOURCE,
            vec![
                (0, b"private-wire-0".to_vec()),
                (1, b"private-wire-1".to_vec()),
                (0, b"private-wire-2".to_vec()),
            ],
        );
        let mapping = VerifiedImportTurnMapping::from_validated_import_mapping(
            &native,
            carrier_test_support::CAPTURE,
            "claude_code",
            carrier_test_support::SOURCE,
            &["synthetic-turn-0", "synthetic-turn-1", "ordinary-turn"],
        )
        .expect("authorized synthetic turn map");
        let partitioned = partition_import_reasoning(native, &mapping).expect("valid partition");
        assert_eq!(partitioned.len(), 2);
        let batch = partitioned
            .get("synthetic-turn-0")
            .expect("candidate-bearing turn");
        assert_eq!(batch.artifacts.len(), 2);
        assert_eq!(batch.proofs.len(), 2);
        assert_eq!(batch.artifacts[0].bytes.as_bytes(), b"private-wire-0");
        assert_eq!(batch.artifacts[1].bytes.as_bytes(), b"private-wire-2");
        let other = partitioned
            .get("synthetic-turn-1")
            .expect("second candidate-bearing turn");
        assert_eq!(other.artifacts.len(), 1);
        assert_eq!(other.artifacts[0].bytes.as_bytes(), b"private-wire-1");
    }

    #[test]
    fn native_reasoning_proof_rejects_each_bound_dimension() {
        fn batch() -> NativeProvenReasoning {
            let prepared = synthetic_native_preparation(
                b"ordinary".to_vec(),
                carrier_test_support::CAPTURE,
                carrier_test_support::SOURCE,
                vec![b"opaque-wire".to_vec()],
            );
            let ReasoningSourceDisposition::NativeProven(batch) = prepared.disposition else {
                panic!("native fixture")
            };
            batch
        }
        let good = batch();
        good.validate(carrier_test_support::CAPTURE, ReasoningProvider::ClaudeCode)
            .expect("synthetic control");
        for dimension in 0..9 {
            let mut bad = batch();
            match dimension {
                0 => bad.proofs[0].issuer = NativeReasoningIssuer::SyntheticUnknown,
                1 => bad.proofs[0].provider = ReasoningProvider::Codex,
                2 => bad.proofs[0].source_session.push_str("wrong"),
                3 => bad.proofs[0].message += 1,
                4 => bad.proofs[0].part += 1,
                5 => bad.proofs[0].field = ReasoningSourceKind::RedactedThinkingData,
                6 => bad.proofs[0].original_wire_sha256[0] ^= 1,
                7 => bad.proofs[0].logical_turn.push_str("wrong"),
                8 => bad.capture_session.push_str("wrong"),
                _ => unreachable!(),
            }
            assert!(
                bad.validate(carrier_test_support::CAPTURE, ReasoningProvider::ClaudeCode)
                    .is_err(),
                "dimension {dimension}"
            );
        }
        assert!(
            good.validate("different-capture", ReasoningProvider::ClaudeCode)
                .is_err()
        );
        assert!(
            good.validate(carrier_test_support::CAPTURE, ReasoningProvider::OpenCode)
                .is_err()
        );
    }

    use std::time::Instant;

    use super::*;

    #[test]
    fn cov2_runtime_partial_classifier_preserves_existing_native_owner() {
        // Reuse the pre-existing synthetic carrier fixture; this test adds no
        // native issuer and does not claim a real provider-origin positive.
        let mut prepared = carrier_test_support::NATIVE
            .prepare_authorized_source(b"ordinary".to_vec())
            .expect("existing synthetic native carrier");
        prepared.coverage_fragment = Some(ReasoningCoverageFragment {
            contains_reasoning: true,
            source_session: Zeroizing::new(carrier_test_support::SOURCE.to_owned()),
            visible_parts: Vec::new(),
            partial: true,
        });
        let (fragment, rejected, partial) = take_authorized_reasoning_coverage(
            &mut prepared,
            true,
            "claude_code",
            Some(carrier_test_support::SOURCE),
        )
        .expect("a rejected data-only companion does not consume native authority");
        assert!(fragment.is_none());
        assert!(rejected);
        assert!(!partial);
        assert!(prepared.classifier_rejected);
        let (ordinary, disposition, permit) =
            ReasoningSourceDisposition::from_prepared_authorized_source_with_partial_permit(
                prepared,
                true,
                TRANSCRIPT_READ_HARD_CAP_BYTES,
                "claude_code",
                carrier_test_support::CAPTURE,
                Some(carrier_test_support::SOURCE),
            )
            .expect("the original native binding remains valid");
        assert_eq!(ordinary, b"ordinary");
        assert!(matches!(
            disposition,
            ReasoningSourceDisposition::NativeProven(_)
        ));
        assert!(permit.is_none());
    }

    #[test]
    fn og26_unverified_candidate_cannot_become_native() {
        fn part(index: usize, source_len: usize) -> OpenCodeCandidatePart {
            OpenCodeCandidatePart::checked(
                Zeroizing::new(format!("msg_{index}")),
                index,
                Zeroizing::new(format!("npart_{index:032x}")),
                0,
                Zeroizing::new(b"{\"untrusted\":true}".to_vec()),
                vec![(
                    FieldName::Signature,
                    Zeroizing::new("synthetic-private-value".to_owned()),
                )],
                source_len,
            )
            .expect("bounded synthetic candidate")
        }

        fn prepared() -> PreparedReasoningSource {
            let source_len = 16 * 1024;
            let mut candidates = SealedOpenCodeCandidates::checked(
                Zeroizing::new("source_session".to_owned()),
                source_len,
            )
            .expect("bounded source session");
            candidates
                .append(part(1, source_len))
                .expect("one bounded part");
            PreparedReasoningSource::with_opencode_candidates(b"safe ordinary".to_vec(), candidates)
        }

        let (_, disposition) = ReasoningSourceDisposition::from_prepared_authorized_source(
            prepared(),
            true,
            16 * 1024,
            "opencode",
            "capture_session",
            Some("source_session"),
        )
        .expect("unverified candidate remains contained");
        assert!(matches!(
            disposition.state(),
            ReasoningSourceState::Unproven
        ));
        assert!(!disposition.is_ordinary());
        assert!(!disposition.has_capture_consumer());
        let ReasoningSourceDisposition::OpenCodeCandidate(candidates) = &disposition else {
            panic!("candidate must remain privately sealed")
        };
        assert_eq!(candidates.parts[0].message_ordinal, 1);
        assert_eq!(candidates.parts[0].part_ordinal, 0);
        assert_eq!(candidates.parts[0].source_message.as_str(), "msg_1");
        assert_eq!(
            candidates.parts[0].source_part.as_str(),
            format!("npart_{:032x}", 1)
        );
        assert_eq!(
            candidates.parts[0].receipt.as_slice(),
            b"{\"untrusted\":true}"
        );
        assert_eq!(
            candidates.parts[0].fields[0].1.as_bytes(),
            b"synthetic-private-value"
        );

        let (_, unmapped) = ReasoningSourceDisposition::from_prepared_authorized_source(
            prepared(),
            false,
            16 * 1024,
            "opencode",
            "capture_session",
            Some("source_session"),
        )
        .expect("missing mapping is a safe partial");
        assert!(matches!(unmapped.state(), ReasoningSourceState::Unmapped));
        for (provider, source_session) in [
            ("claude_code", Some("source_session")),
            ("codex", Some("source_session")),
            ("unknown", Some("source_session")),
            ("opencode", Some("wrong_session")),
            ("opencode", None),
        ] {
            assert!(
                ReasoningSourceDisposition::from_prepared_authorized_source(
                    prepared(),
                    true,
                    16 * 1024,
                    provider,
                    "capture_session",
                    source_session,
                )
                .is_err(),
                "{provider} must not receive a candidate"
            );
        }

        let source_len = 64 * 1024;
        let mut overfull = SealedOpenCodeCandidates::checked(
            Zeroizing::new("source_session".to_owned()),
            source_len,
        )
        .expect("bounded source session");
        for index in 0..crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES {
            overfull
                .append(part(index, source_len))
                .expect("within bounded candidate count");
        }
        assert!(overfull.append(part(512, source_len)).is_err());
        assert!(
            OpenCodeCandidatePart::checked(
                Zeroizing::new("msg".to_owned()),
                0,
                Zeroizing::new("npart_invalid".to_owned()),
                0,
                Zeroizing::new(b"{}".to_vec()),
                vec![(FieldName::Signature, Zeroizing::new("x".to_owned()))],
                source_len,
            )
            .is_err()
        );
    }

    #[test]
    fn decryptor_registry_is_none_for_all_providers() {
        for provider in [
            ReasoningProvider::Codex,
            ReasoningProvider::ClaudeCode,
            ReasoningProvider::OpenCode,
        ] {
            assert!(reasoning_decryptor(provider).is_none());
            let state = if reasoning_decryptor(provider).is_none() {
                ReasoningAvailability::EncryptedUnavailable
            } else {
                panic!("no provider decryptor is registered");
            };
            assert_eq!(state, ReasoningAvailability::EncryptedUnavailable);
        }
    }

    #[test]
    fn contract_serde_snapshot() {
        let statuses = [
            ReasoningAvailability::ProviderVisible,
            ReasoningAvailability::EncryptedUnavailable,
            ReasoningAvailability::OpaqueArchived,
            ReasoningAvailability::NotPresent,
            ReasoningAvailability::UnsupportedShape,
        ];
        let absent = classify_unrecognized_field(
            ReasoningProvider::OpenCode,
            UnrecognizedReasoningField::Other,
            b"unrecognized",
        );
        let unknown = classify_unrecognized_field(
            ReasoningProvider::OpenCode,
            UnrecognizedReasoningField::ReasoningLike,
            b"unsupported",
        );
        let encrypted = ReasoningRecord {
            provider: ReasoningProvider::ClaudeCode,
            source_kind: Some(ReasoningSourceKind::Signature),
            availability: ReasoningAvailability::EncryptedUnavailable,
            warning: None,
        };
        // RG-04 additive freeze: the canonical bytes of a synthetic
        // SemanticRecord::Reasoning record are pinned alongside the metadata
        // contract. The variant's serde serialization is deliberately
        // fail-closed (ProviderVisibleText::serialize always errors), so the
        // canonical writer is the frozen persistence face.
        let (_, reasoning_text) = classify_provider_visible_reasoning(
            ReasoningProvider::OpenCode,
            ReasoningSourceKind::Reasoning,
            "synthetic".to_string(),
        );
        let reasoning_record =
            crate::internal::ai::observed_agents::coverage::SemanticRecord::Reasoning {
                provider: ReasoningProvider::OpenCode,
                source_kind: Some(ReasoningSourceKind::Reasoning),
                text: reasoning_text,
            };
        let reasoning_canonical = String::from_utf8(
            crate::internal::ai::observed_agents::coverage::canonical_turn_bytes(
                std::slice::from_ref(&reasoning_record),
            ),
        )
        .expect("canonical bytes are UTF-8");
        let projected = crate::internal::ai::observed_agents::coverage::safe_turn_projection(
            "opencode",
            &crate::internal::ai::observed_agents::coverage::NormalizedTurn {
                logical_turn_key: "fixture-turn".to_string(),
                ordinal: 0,
                completeness:
                    crate::internal::ai::observed_agents::coverage::Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: vec![reasoning_record],
            },
        );
        let actual = serde_json::to_string_pretty(&serde_json::json!({
            "availability": statuses,
            "records": [absent, unknown, encrypted],
            "semantic_reasoning_canonical": reasoning_canonical,
            "semantic_reasoning_projection": projected,
            "semantic_reasoning_note": "RG-04 additive freeze: canonical bytes of a synthetic Reasoning record (write_canonical) and the full safe_turn_projection output; provider_visible text is redacted before any persistence and serde serialization of the variant is fail-closed.",
        }))
        .expect("fixed contract types serialize");
        assert_eq!(
            actual.trim(),
            include_str!("../../../../tests/fixtures/agent_transcripts/reasoning/contract.snap")
                .trim()
        );
    }

    #[test]
    fn undeclared_high_entropy_is_not_present() {
        for candidate in [
            "dGhpcy1sb29rcy1saWtlLWNpcGhlcnRleHQ=",
            "83fde0f5e4c1208fddc5591f95befd2d9f45ca37",
        ] {
            let classified = classify_unrecognized_field(
                ReasoningProvider::OpenCode,
                UnrecognizedReasoningField::Other,
                candidate.as_bytes(),
            );
            assert_eq!(classified.availability(), ReasoningAvailability::NotPresent);
            assert_eq!(classified.warning(), None);
        }
    }

    #[test]
    fn debug_output_has_no_payload() {
        let payload = OpaqueEncryptedBytes::from_verified_test_field(
            b"CANARY_PRIVATE_CIPHERTEXT_4d5c".to_vec(),
        );
        let debug = format!("{payload:?}");
        assert!(!debug.contains("CANARY_PRIVATE_CIPHERTEXT_4d5c"));
        assert!(!debug.contains("43414e415259"));
        assert_eq!(payload.len(), 30);
        assert!(!payload.is_empty());
    }

    #[test]
    fn unsupported_shape_yields_warning() {
        let classified = classify_unrecognized_field(
            ReasoningProvider::OpenCode,
            UnrecognizedReasoningField::ReasoningLike,
            b"CANARY_PRIVATE_REASONING",
        );
        assert_eq!(
            classified.availability(),
            ReasoningAvailability::UnsupportedShape
        );
        assert_eq!(
            classified.warning(),
            Some(ReasoningWarning::UnsupportedShape)
        );
        let diagnostic = serde_json::to_string(&classified).expect("fixed diagnostic");
        assert!(!diagnostic.contains("payload"));
        assert!(!diagnostic.contains("field_path"));
    }

    #[test]
    fn verified_claude_json_string_bytes_exact() {
        let source = br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"","signature":"A\u0042\\C"},{"type":"redacted_thinking","data":"\u0041/raw=="}]}}"#;
        let signature = verify_claude_encrypted_field(source, 0)
            .expect("declared encrypted thinking signature");
        assert_eq!(
            signature.record().availability(),
            ReasoningAvailability::EncryptedUnavailable
        );
        assert_eq!(
            signature.record().source_kind(),
            Some(ReasoningSourceKind::Signature)
        );
        assert_eq!(signature.into_opaque().data, br"A\u0042\\C");

        let redacted = verify_claude_encrypted_field(source, 1)
            .expect("declared encrypted redacted thinking data");
        assert_eq!(
            redacted.record().source_kind(),
            Some(ReasoningSourceKind::RedactedThinkingData)
        );
        assert_eq!(
            serde_json::to_value(redacted.record())
                .expect("safe metadata")
                .get("source_kind"),
            Some(&serde_json::json!("redacted_thinking_data"))
        );
        assert_eq!(redacted.into_opaque().data, br"\u0041/raw==");
    }

    #[test]
    fn unverified_field_rejected_without_payload() {
        for source in [
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":123}]}}"#[..],
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":"CANARY_PRIVATE_REASONING","signature":"again"}]}}"#[..],
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":"CANARY_PRIVATE_REASONING","extra":"value"}]}}"#[..],
            &br#"{"type":"assistant","debug":{"x":1,"x":2},"message":{"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":"CANARY_PRIVATE_REASONING"}]}}"#[..],
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"thinking","thinking":"x","signature":"CANARY_PRIVATE_REASONING"}]} ,"debug":{"a":1,"\u0061":2}}"#[..],
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"future_thinking","encrypted":"CANARY_PRIVATE_REASONING"}]}}"#[..],
            &br#"{"type":"assistant","content":[{"type":"reasoning","text":"x","state":{"anthropic":{"signature":"CANARY_PRIVATE_REASONING"}}}]}"#[..],
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"CANARY_PRIVATE_REASONING","signature":"fake"}]}}"#[..],
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"ordinary text"}]}}"#[..],
            &br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"tool_use","id":"call_1","name":"Read","input":{}}]}}"#[..],
        ] {
            let error = verify_claude_encrypted_field(source, 0).expect_err("unverified field");
            assert!(!format!("{error:?}").contains("CANARY_PRIVATE_REASONING"));
        }
        let oversized = vec![b'x'; TRANSCRIPT_READ_HARD_CAP_BYTES as usize + 1];
        assert_eq!(
            verify_claude_encrypted_field(&oversized, 0).expect_err("bounded source"),
            ReasoningFieldError::SourceTooLarge
        );
    }

    #[test]
    fn readable_text_cannot_bypass_classification_or_serde() {
        let canary = "sk-ant-EXAMPLE0000000000000000";
        let (_, text) = classify_provider_visible_reasoning(
            ReasoningProvider::OpenCode,
            ReasoningSourceKind::Reasoning,
            canary.to_string(),
        );
        assert_eq!(text.as_str(), canary);
        let record = crate::internal::ai::observed_agents::coverage::SemanticRecord::Reasoning {
            provider: ReasoningProvider::OpenCode,
            source_kind: Some(ReasoningSourceKind::Reasoning),
            text,
        };
        let error = serde_json::to_value(&record).expect_err("unredacted serde must fail closed");
        assert!(!error.to_string().contains(canary));
        assert!(serde_json::from_str::<ProviderVisibleText>("\"synthetic\"").is_err());
        for availability in [
            ReasoningAvailability::EncryptedUnavailable,
            ReasoningAvailability::OpaqueArchived,
            ReasoningAvailability::NotPresent,
            ReasoningAvailability::UnsupportedShape,
        ] {
            let classification = ReasoningRecord {
                provider: ReasoningProvider::OpenCode,
                source_kind: None,
                availability,
                warning: None,
            };
            assert!(
                ProviderVisibleText::from_classification(&classification, canary.to_string())
                    .is_none()
            );
        }
    }

    #[test]
    fn duplicate_keys_in_unselected_block_are_rejected() {
        let source = br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first","text":"second"},{"type":"thinking","thinking":"","signature":"CANARY_PRIVATE_REASONING"}]}}"#;
        let error = verify_claude_encrypted_field(source, 1)
            .expect_err("duplicate key in an unselected sibling must fail closed");
        assert_eq!(error, ReasoningFieldError::MalformedSource);
        assert!(!format!("{error:?}").contains("CANARY_PRIVATE_REASONING"));
        let unique = br#"{"type":"assistant","message":{"role":"assistant","content":[{"type":"text","text":"first"},{"type":"thinking","thinking":"","signature":"CANARY_PRIVATE_REASONING"}]}}"#;
        assert!(verify_claude_encrypted_field(unique, 1).is_ok());
    }

    #[test]
    fn reasoning_field_error_display_is_stable() {
        assert_eq!(TRANSCRIPT_READ_HARD_CAP_BYTES, 16 * 1024 * 1024);
        for (error, expected) in [
            (
                ReasoningFieldError::SourceTooLarge,
                "reasoning source exceeds its 16 MiB byte or structural budget; use a smaller export",
            ),
            (
                ReasoningFieldError::MalformedSource,
                "reasoning source is not a valid provider record; check the pinned provider export",
            ),
            (
                ReasoningFieldError::UnsupportedShape,
                "reasoning field has an unsupported shape; update the pinned provider contract",
            ),
        ] {
            assert_eq!(error.to_string(), expected);
        }
    }

    #[test]
    #[ignore = "run explicitly in release mode on the acceptance machine"]
    fn verified_source_16_mib_budget() {
        let mut source = b"{\"type\":\"assistant\",\"message\":{\"role\":\"assistant\",\"content\":[{\"type\":\"redacted_thinking\",\"data\":\"".to_vec();
        let suffix = b"\"}]}}";
        let fill =
            (TRANSCRIPT_READ_HARD_CAP_BYTES as usize).saturating_sub(source.len() + suffix.len());
        assert!(fill > 0);
        source.extend(std::iter::repeat_n(b'A', fill));
        source.extend_from_slice(suffix);
        assert_eq!(source.len(), TRANSCRIPT_READ_HARD_CAP_BYTES as usize);
        let started = Instant::now();
        let ciphertext = verify_claude_encrypted_field(&source, 0)
            .expect("synthetic pinned encrypted field")
            .into_opaque();
        std::hint::black_box(ciphertext.sha256_hex());
        let elapsed = started.elapsed();
        eprintln!("RG-01 verified-source + SHA-256 16 MiB: {elapsed:?}");
        #[cfg(not(debug_assertions))]
        assert!(
            elapsed <= std::time::Duration::from_millis(50),
            "RG-01 16 MiB source/hash budget exceeded: {elapsed:?}"
        );
    }

    #[test]
    fn opaque_digest_uses_original_bytes() {
        let payload = OpaqueEncryptedBytes::from_verified_test_field(b"abc".to_vec());
        assert_eq!(
            payload.sha256_hex(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn opaque_digest_stays_out_of_metadata() {
        let payload = OpaqueEncryptedBytes::from_verified_test_field(b"private bytes".to_vec());
        let digest = payload.sha256_hex();
        let metadata = classify_unrecognized_field(
            ReasoningProvider::OpenCode,
            UnrecognizedReasoningField::Other,
            &payload.data,
        );
        let json = serde_json::to_string(&metadata).expect("safe metadata");
        assert!(!json.contains(&digest));
        assert!(!json.contains("private bytes"));
    }
}
