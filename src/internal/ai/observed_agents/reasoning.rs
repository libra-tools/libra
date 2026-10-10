//! Provider-neutral reasoning states and an opaque ciphertext type.
//!
//! This module validates a declared Claude field shape after source
//! authorization. Provider adapters must establish identity from pinned source
//! evidence; entropy and base64-like contents never prove ciphertext.

use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, Deserializer, MapAccess, SeqAccess, Visitor},
};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::transcript_source::TRANSCRIPT_READ_HARD_CAP_BYTES;

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
pub struct ProviderVisibleText {
    inner: String,
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
    pub(crate) fn from_classification(record: &ReasoningRecord, text: String) -> Option<Self> {
        if record.availability() != ReasoningAvailability::ProviderVisible {
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
        self.inner = String::from_utf8_lossy(bytes.as_ref()).into_owned();
        report
    }
}

/// Errors do not include source field names, values, or raw provider JSON.
#[derive(Debug, Error, Eq, PartialEq)]
pub enum ReasoningFieldError {
    #[error("reasoning source exceeds the 16 MiB transcript read limit; use a bounded export")]
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
        },
    })
}

/// Verified provider ciphertext, never a redacted transcript or JSON value.
///
/// RG-01 exposes no conversion from arbitrary bytes. Construction happens
/// only inside this module after [`verify_claude_encrypted_field`] checks a
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
}

impl OpaqueEncryptedBytes {
    /// Used only for isolated contract tests until verified provider wiring.
    /// `pub(crate)` so sibling lib test modules (history.rs RG-02 artifact
    /// tests) can construct verified-looking ciphertext for the writer.
    #[cfg(test)]
    pub(crate) fn from_verified_test_field(data: Vec<u8>) -> Self {
        Self { data }
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
    use std::time::Instant;

    use super::*;
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
                "reasoning source exceeds the 16 MiB transcript read limit; use a bounded export",
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
