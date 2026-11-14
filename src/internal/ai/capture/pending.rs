//! Repository-private, authenticated recovery artifacts. No provider locator
//! or raw source is a recovery authority; only a complete sealed payload may
//! enter this codec. Headers are small indexed discovery/GC records and chunks
//! are read only for one explicitly selected artifact.

use std::{
    io::{self, Write},
    path::Path,
    time::Instant,
};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use sea_orm::{ConnectionTrait, DatabaseTransaction, Statement};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

#[cfg(test)]
use super::snapshot;
use crate::internal::{
    ai::{
        capture::{
            catalog::{CaptureCatalogError, PendingSessionContext},
            checkpoint::{
                AuthenticatedPendingEnvelope, CheckpointRedactedPayload, PreparedPendingNativeSeal,
                ReasoningCoveragePlan,
            },
            key,
            pending_identity::{self, PreparedPendingAlias},
            pending_payload::{
                PendingPayloadProjection, PortablePendingCoverage,
                VersionedPendingPayloadProjection, VersionedPendingReasoningReplay,
                VersionedPortablePendingCoverage, check_versioned_tuple_wire_capacity,
                preflight_versioned_tuples,
            },
        },
        capture_scope::CaptureScope,
        coverage_gate::{
            LiveClaimCommitPlan, NativeVersionedReasoningClaimCommitPlan, ReservedTurnClaim,
            VersionedLiveClaimCommitPlan, VersionedReservedTurnClaim,
        },
        observed_agents::{
            coverage::CoverageSchemaVersion, parse_canon_value, reasoning::PendingIntegrityToken,
        },
    },
    metadata::{MetadataKv, MetadataScope, MetadataValueType},
};

pub(crate) const MAX_ARTIFACTS: usize = 16;
pub(crate) const MAX_HEADER_BYTES: usize = 8 * 1024;
pub(crate) const MAX_ENVELOPE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TRANSCRIPT_BYTES: usize = 24 * 1024 * 1024;
const MAX_SIDECAR_BYTES: usize = 4 * 1024 * 1024;
const CHUNK_BYTES: usize = 512 * 1024;
const MAX_CHUNKS: usize = 128;
const CHUNK_TEXT_BYTES: usize = CHUNK_BYTES.div_ceil(3) * 4;
const VERSION: u8 = 1;
const VERSIONED_ENVELOPE_VERSION: u8 = 2;

// Compiled pre-v2 payload/header/envelope decoder and original recovery
// dispatch. Kept separate from the live reader so v2 cannot pass an oracle
// accidentally updated with the production decoder.
#[cfg(test)]
#[allow(dead_code)]
pub(crate) mod frozen_pre_v2 {
    include!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/agent_transcripts/reasoning/pending_v1_decoder.rs"
    ));
}

#[cfg(all(test, unix))]
pub(crate) async fn assert_frozen_pre_v2_route_rejects_v2() {
    tests::assert_frozen_old_route_rejects_v2().await;
}
const HEADER_SCOPES: [MetadataScope; 2] = [
    MetadataScope::AgentCapturePending,
    MetadataScope::AgentCaptureQuarantine,
];

/// Closed opaque bindings produced by the catalog's current terminal fence.
/// These fields contain no native provider identity or path.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingBinding {
    pub(crate) scope: CaptureScope,
    pub(crate) session_id: String,
    pub(crate) checkpoint_id: String,
    pub(crate) event_id: String,
    pub(crate) action_key: String,
    pub(crate) receipt_key: String,
    pub(crate) marker_generation: String,
    pub(crate) source_commitment: String,
    pub(crate) reserved_revision: i64,
    pub(crate) original_deadline_millis: Option<i64>,
    pub(crate) deferrable: bool,
    pub(crate) first_attempt_millis: i64,
    pub(crate) parent_commit: Option<String>,
    /// Explicit absence proof, not a missing field silently defaulting to None.
    pub(crate) parent_unborn: bool,
}

impl PendingBinding {
    fn validate(&self) -> Result<()> {
        ensure!(
            uuid::Uuid::parse_str(&self.session_id).is_ok_and(|id| id.get_version_num() == 4
                && id.get_variant() == uuid::Variant::RFC4122
                && id.to_string() == self.session_id),
            "invalid capture artifact alias; run `libra agent doctor`"
        );
        for id in [&self.checkpoint_id, &self.event_id] {
            ensure!(
                uuid::Uuid::parse_str(id).is_ok_and(|parsed| parsed.to_string() == *id),
                "invalid capture artifact identity; run `libra agent doctor`"
            );
        }
        for value in [
            &self.scope.repo_id,
            &self.action_key,
            &self.receipt_key,
            &self.marker_generation,
            &self.source_commitment,
        ] {
            ensure!(
                !value.is_empty()
                    && value.len() <= 512
                    && !value.chars().any(|c| c.is_whitespace() || c.is_control()),
                "invalid capture artifact binding; run `libra agent doctor`"
            );
        }
        ensure!(
            self.scope.worktree_id.len() <= 512
                && self
                    .scope
                    .workspace_id
                    .as_ref()
                    .is_none_or(|id| !id.is_empty() && id.len() <= 512)
                && self.scope.workspace_id.is_some() == self.scope.workspace_fence.is_some()
                && self.scope.workspace_fence.is_none_or(|fence| fence > 0)
                && self.reserved_revision > 0,
            "invalid capture artifact scope; run `libra agent doctor`"
        );
        ensure!(
            self.parent_commit.as_deref().is_none_or(valid_oid)
                && self.parent_unborn == self.parent_commit.is_none(),
            "invalid capture artifact parent proof; destructive maintenance stopped"
        );
        ensure!(
            self.source_commitment
                .strip_prefix("source/hmac-v2/")
                .is_some_and(|digest| digest.len() == 64 && is_lower_hex(digest)),
            "capture artifact requires a keyed source commitment; run `libra agent doctor`"
        );
        Ok(())
    }
}

fn is_lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn valid_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && is_lower_hex(value)
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingHeader {
    version: u8,
    pub(crate) binding: PendingBinding,
    mac: String,
    envelope_bytes: usize,
    chunks: usize,
    pub(crate) manual_attempted: bool,
}

impl PendingHeader {
    pub(crate) fn decode(text: &str) -> Result<Self> {
        ensure!(
            text.len() <= MAX_HEADER_BYTES,
            "capture recovery header exceeds its safe limit; run `libra agent doctor`"
        );
        let header: Self = serde_json::from_str(text).map_err(|_| {
            anyhow::anyhow!("invalid capture recovery header; run `libra agent doctor`")
        })?;
        header.binding.validate()?;
        ensure!(
            header.version == VERSION
                && header.envelope_bytes > 0
                && header.envelope_bytes <= MAX_ENVELOPE_BYTES
                && header.chunks == header.envelope_bytes.div_ceil(CHUNK_BYTES)
                && header.chunks <= MAX_CHUNKS
                && header
                    .mac
                    .strip_prefix("pending-envelope/hmac-v1/")
                    .is_some_and(|digest| digest.len() == 64 && is_lower_hex(digest))
                && serde_json::to_string(&header)? == text,
            "invalid capture recovery header; run `libra agent doctor`"
        );
        Ok(header)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingEnvelope {
    version: u8,
    binding: PendingBinding,
    payload: PendingPayloadProjection,
    coverage: PortablePendingCoverage,
}

/// Outer-v2 is a separate wire schema. It leaves `PendingEnvelope` and its
/// historical v1 serialization path completely unchanged.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)] // COV2-24 seals and COV2-22 persists/loads this codec.
pub(crate) struct VersionedPendingEnvelope {
    pub(crate) version: u8,
    pub(crate) binding: PendingBinding,
    pub(crate) payload: VersionedPendingPayloadProjection,
    pub(crate) coverage: VersionedPortablePendingCoverage,
}

impl VersionedPendingEnvelope {
    fn validate(&self) -> Result<()> {
        ensure!(
            self.version == VERSIONED_ENVELOPE_VERSION,
            "unsupported capture recovery envelope version; run `libra agent doctor`"
        );
        self.binding.validate()?;
        self.payload.validate()?;
        self.payload.matches_binding(&self.binding)?;
        self.coverage.validate()?;
        ensure!(
            self.payload.identity_commit_associations.len() == self.coverage.claims.len(),
            "capture recovery claim and identity tuple changed; run `libra agent doctor`"
        );
        for (association, claim) in self
            .payload
            .identity_commit_associations
            .iter()
            .zip(&self.coverage.claims)
        {
            ensure!(
                association.coverage_schema_version == claim.coverage_schema_version
                    && association.logical_turn_key == claim.logical_turn_key,
                "capture recovery claim and identity tuple changed; run `libra agent doctor`"
            );
        }
        check_versioned_tuple_wire_capacity(&self.payload, &self.coverage)?;
        Ok(())
    }
}

struct BoundedEnvelopeCounter {
    len: usize,
}

impl Write for BoundedEnvelopeCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.len = self
            .len
            .checked_add(bytes.len())
            .filter(|size| *size <= MAX_ENVELOPE_BYTES)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "capture recovery envelope capacity",
                )
            })?;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct BoundedEnvelopeWriter {
    bytes: Vec<u8>,
}

impl Write for BoundedEnvelopeWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let remaining = MAX_ENVELOPE_BYTES.saturating_sub(self.bytes.len());
        if bytes.len() > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "capture recovery envelope capacity",
            ));
        }
        self.bytes.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct ExactEnvelopeWriter<'a> {
    original: &'a [u8],
    offset: usize,
}

impl Write for ExactEnvelopeWriter<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let end = self.offset.checked_add(bytes.len()).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "capture recovery envelope overflow",
            )
        })?;
        if self.original.get(self.offset..end) != Some(bytes) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "capture recovery envelope is not canonical",
            ));
        }
        self.offset = end;
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Pure bounded encoding; this does not sign, seal, persist or confer native
/// source authority. The v1 envelope and existing MAC domain stay untouched.
#[allow(dead_code)] // COV2-24 supplies the checked native seal caller.
pub(crate) fn encode_versioned_envelope(
    binding: PendingBinding,
    payload: VersionedPendingPayloadProjection,
    coverage: VersionedPortablePendingCoverage,
) -> Result<Vec<u8>> {
    let envelope = VersionedPendingEnvelope {
        version: VERSIONED_ENVELOPE_VERSION,
        binding,
        payload,
        coverage,
    };
    envelope.validate()?;
    let mut counter = BoundedEnvelopeCounter { len: 0 };
    serde_json::to_writer(&mut counter, &envelope).map_err(|_| {
        anyhow::anyhow!(
            "capture recovery envelope exceeds its safe capacity; run `libra agent doctor`"
        )
    })?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(counter.len).map_err(|_| {
        anyhow::anyhow!(
            "cannot reserve bounded capture recovery envelope; retry through the provider"
        )
    })?;
    let mut writer = BoundedEnvelopeWriter { bytes };
    serde_json::to_writer(&mut writer, &envelope).map_err(|_| {
        anyhow::anyhow!(
            "capture recovery envelope exceeds its safe capacity; run `libra agent doctor`"
        )
    })?;
    ensure!(
        writer.bytes.len() == counter.len,
        "capture recovery envelope changed during encoding; retry through the provider"
    );
    validate_envelope_shape(&writer.bytes)?;
    Ok(writer.bytes)
}

/// The witness is issued only after verification of the original exact-byte
/// envelope MAC. Parsing an unauthenticated DTO cannot call this decoder.
#[allow(dead_code)] // COV2-22 wires the authenticated reader.
pub(crate) fn decode_authenticated_versioned_envelope(
    authentication: &AuthenticatedPendingEnvelope<'_>,
    expected: &PendingBinding,
) -> Result<VersionedPendingEnvelope> {
    ensure!(
        authentication.scope() == &expected.scope,
        "capture recovery authentication scope changed; run `libra agent doctor`"
    );
    let bytes = authentication.bytes();
    validate_envelope_shape(bytes)?;
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct RawVersionedEnvelope<'a> {
        version: u8,
        #[serde(borrow)]
        binding: &'a RawValue,
        #[serde(borrow)]
        payload: &'a RawValue,
        #[serde(borrow)]
        coverage: &'a RawValue,
    }
    let raw: RawVersionedEnvelope<'_> = serde_json::from_slice(bytes).map_err(|_| {
        anyhow::anyhow!("invalid authenticated capture recovery envelope; run `libra agent doctor`")
    })?;
    ensure!(
        raw.version == VERSIONED_ENVELOPE_VERSION,
        "unsupported capture recovery envelope version; run `libra agent doctor`"
    );
    preflight_versioned_tuples(raw.payload, raw.coverage)?;
    // Detect duplicate nested JSON keys before typed deserialization can
    // collapse them into a map. This runs only after entry/wire preflight.
    parse_canon_value(bytes).map_err(|_| {
        anyhow::anyhow!("invalid authenticated capture recovery envelope; run `libra agent doctor`")
    })?;
    let envelope: VersionedPendingEnvelope = serde_json::from_slice(bytes).map_err(|_| {
        anyhow::anyhow!("invalid authenticated capture recovery envelope; run `libra agent doctor`")
    })?;
    ensure!(
        envelope.binding == *expected
            && serde_json::to_vec(&envelope.binding).map_err(|_| {
                anyhow::anyhow!("invalid capture recovery binding; run `libra agent doctor`")
            })? == raw.binding.get().as_bytes(),
        "capture recovery binding changed; run `libra agent doctor`"
    );
    envelope.validate()?;
    let mut exact = ExactEnvelopeWriter {
        original: bytes,
        offset: 0,
    };
    serde_json::to_writer(&mut exact, &envelope).map_err(|_| {
        anyhow::anyhow!("noncanonical capture recovery envelope; run `libra agent doctor`")
    })?;
    ensure!(
        exact.offset == bytes.len(),
        "noncanonical capture recovery envelope; run `libra agent doctor`"
    );
    Ok(envelope)
}

pub(crate) struct VerifiedPendingPayload {
    payload: CheckpointRedactedPayload,
    coverage: LiveClaimCommitPlan,
    header: PendingHeader,
    context: PendingSessionContext,
}

impl VerifiedPendingPayload {
    pub(crate) fn payload(&self) -> &CheckpointRedactedPayload {
        &self.payload
    }

    pub(crate) fn coverage(&self) -> &LiveClaimCommitPlan {
        &self.coverage
    }

    pub(crate) fn binding(&self) -> &PendingBinding {
        &self.header.binding
    }

    pub(crate) fn header(&self) -> &PendingHeader {
        &self.header
    }

    pub(crate) fn context(&self) -> &PendingSessionContext {
        &self.context
    }
}

/// A checked outer-v2 load remains a separate carrier until COV2-08 consumes
/// its typed deferred source. No native proof or checkpoint ACK is issued.
#[allow(dead_code)] // COV2-08 wires this private recovery carrier.
pub(crate) struct VerifiedVersionedPendingPayload {
    payload: CheckpointRedactedPayload,
    coverage: VersionedLiveClaimCommitPlan,
    reasoning: VersionedPendingReasoningReplay,
    header: PendingHeader,
    context: PendingSessionContext,
}

#[allow(dead_code)] // COV2-08 wires this private recovery carrier.
impl VerifiedVersionedPendingPayload {
    pub(crate) fn payload(&self) -> &CheckpointRedactedPayload {
        &self.payload
    }

    pub(crate) fn coverage(&self) -> &VersionedLiveClaimCommitPlan {
        &self.coverage
    }

    pub(crate) fn reasoning(&self) -> &VersionedPendingReasoningReplay {
        &self.reasoning
    }

    pub(crate) fn header(&self) -> &PendingHeader {
        &self.header
    }

    pub(crate) fn context(&self) -> &PendingSessionContext {
        &self.context
    }

    /// Transfer the entire local-integrity carrier to the COV2-08 replay owner.
    /// Its deferred source must remain attached until lower native validation.
    pub(crate) fn into_replay_parts(
        self,
    ) -> (
        CheckpointRedactedPayload,
        VersionedLiveClaimCommitPlan,
        VersionedPendingReasoningReplay,
        PendingHeader,
        PendingSessionContext,
    ) {
        (
            self.payload,
            self.coverage,
            self.reasoning,
            self.header,
            self.context,
        )
    }
}

/// Only the authenticated loader in this module can pair its exact decoded
/// projection with the MAC witness and the key-owner token from those bytes.
#[allow(dead_code)] // COV2-08 consumes the loader without widening provenance.
pub(crate) struct VersionedPendingRehydrateMaterial<'a> {
    projection: &'a VersionedPendingPayloadProjection,
    authentication: &'a AuthenticatedPendingEnvelope<'a>,
    integrity: &'a PendingIntegrityToken,
}

impl<'a> VersionedPendingRehydrateMaterial<'a> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        &'a VersionedPendingPayloadProjection,
        &'a AuthenticatedPendingEnvelope<'a>,
        &'a PendingIntegrityToken,
    ) {
        (self.projection, self.authentication, self.integrity)
    }
}

/// Only the authenticated loader can transfer the exact projection, original
/// bytes/MAC and key-owner token into the deferred replay carrier.
#[allow(dead_code)] // COV2-08 consumes the loader without widening provenance.
pub(crate) struct VersionedPendingReplayMaterial {
    projection: VersionedPendingPayloadProjection,
    original_envelope: Vec<u8>,
    original_mac: String,
    integrity: PendingIntegrityToken,
}

impl VersionedPendingReplayMaterial {
    pub(crate) fn into_parts(
        self,
    ) -> (
        VersionedPendingPayloadProjection,
        Vec<u8>,
        String,
        PendingIntegrityToken,
    ) {
        (
            self.projection,
            self.original_envelope,
            self.original_mac,
            self.integrity,
        )
    }
}

/// The constructor is the only production path from payload to artifact.
/// No Debug implementation may inadvertently log the authenticated content.
pub(crate) struct SealedPendingArtifact {
    header: PendingHeader,
    bytes: Vec<u8>,
    identity: PreparedPendingAlias,
    coverage_owner: String,
    coverage_claims: Vec<ReservedTurnClaim>,
}

pub(crate) struct PendingSealRequest<'a> {
    pub(crate) binding: PendingBinding,
    pub(crate) identity: PreparedPendingAlias,
    pub(crate) payload: &'a CheckpointRedactedPayload,
    pub(crate) coverage: &'a LiveClaimCommitPlan,
    pub(crate) reasoning: Option<&'a ReasoningCoveragePlan>,
}

/// A closed native-v2 artifact. Construction alone writes no rows or ACK;
/// persistence retains both claim versions in one caller-owned transaction.
#[cfg_attr(not(all(test, unix)), allow(dead_code))]
pub(crate) struct SealedVersionedPendingArtifact {
    header: PendingHeader,
    bytes: Vec<u8>,
    identity: PreparedPendingAlias,
    coverage_owner: String,
    coverage_claims: Vec<VersionedReservedTurnClaim>,
}

/// The caller supplies one payload and its sealed source-derived companion.
/// It cannot pass an independently prepared borrow or an issuer label.
#[cfg_attr(not(all(test, unix)), allow(dead_code))]
pub(crate) struct VersionedPendingSealRequest<'a> {
    pub(crate) binding: PendingBinding,
    pub(crate) identity: PreparedPendingAlias,
    pub(crate) payload: &'a CheckpointRedactedPayload,
    pub(crate) companion: &'a NativeVersionedReasoningClaimCommitPlan,
}

/// Only this module can pair the payload with the borrow returned by its
/// checked native gate; the inner projection cannot recombine source parts.
#[cfg_attr(not(all(test, unix)), allow(dead_code))]
pub(crate) struct VersionedPendingSealMaterial<'a> {
    payload: &'a CheckpointRedactedPayload,
    prepared: PreparedPendingNativeSeal<'a>,
    companion: &'a NativeVersionedReasoningClaimCommitPlan,
}

#[cfg_attr(not(all(test, unix)), allow(dead_code))]
impl<'a> VersionedPendingSealMaterial<'a> {
    pub(crate) fn into_parts(
        self,
    ) -> (
        &'a CheckpointRedactedPayload,
        PreparedPendingNativeSeal<'a>,
        &'a NativeVersionedReasoningClaimCommitPlan,
    ) {
        (self.payload, self.prepared, self.companion)
    }
}

#[cfg_attr(not(all(test, unix)), allow(dead_code))]
impl SealedVersionedPendingArtifact {
    pub(crate) async fn seal<C: ConnectionTrait>(
        conn: &C,
        storage: &Path,
        root: &Path,
        request: VersionedPendingSealRequest<'_>,
        deadline: Instant,
    ) -> Result<Self> {
        let VersionedPendingSealRequest {
            binding,
            identity,
            payload,
            companion,
        } = request;
        check_deadline(deadline)?;
        binding.validate()?;
        let versioned = companion.versioned();
        let ordinary = versioned.ordinary();
        ensure!(
            payload.is_exact_complete_snapshot()
                && payload
                    .snapshot()
                    .source
                    .as_ref()
                    .and_then(|source| source.digest_sha256.as_deref())
                    == Some(binding.source_commitment.as_str()),
            "capture recovery requires a complete sealed source snapshot; retry through the provider"
        );
        ensure!(
            binding.session_id == identity.alias()
                && binding.scope == *identity.context().scope()
                && ordinary.session_id == identity.context().session_id()
                && ordinary.checkpoint_id == binding.checkpoint_id
                && ordinary.parent_commit == binding.parent_commit
                && ordinary.capture_scope.as_ref() == Some(&binding.scope),
            "capture recovery identity changed; retry through the provider"
        );
        let portable = VersionedPortablePendingCoverage::from_live(versioned, &identity)?;
        check_sidecar_capacity(payload, serde_json::to_vec(&portable)?.len())?;
        validate_payload_binding(payload, &binding)?;
        let prepared = payload.prepare_pending_native_versioned_seal(
            &binding.scope,
            companion,
            identity.context().agent_kind(),
        )?;
        let projection = PendingPayloadProjection::seal_bound_versioned_reasoning(
            conn,
            storage,
            root,
            &identity,
            VersionedPendingSealMaterial {
                payload,
                prepared,
                companion,
            },
            deadline,
        )
        .await?;
        let bytes = encode_versioned_envelope(binding.clone(), projection, portable)?;
        check_deadline(deadline)?;
        // This is the historical local MAC domain over the exact outer bytes,
        // not a native issuer proof or a persistence authorization.
        let mac = binding
            .scope
            .sign_pending_envelope_until(conn, storage, root, &bytes, deadline)
            .await
            .map_err(|error| {
                error.context(
                    "cannot authenticate capture recovery artifact; run `libra agent doctor`",
                )
            })?;
        let header = PendingHeader {
            version: VERSION,
            binding,
            mac,
            envelope_bytes: bytes.len(),
            chunks: bytes.len().div_ceil(CHUNK_BYTES),
            manual_attempted: false,
        };
        ensure!(
            serde_json::to_vec(&header)?.len() <= MAX_HEADER_BYTES,
            "capture recovery header exceeds its capacity; retry through the provider"
        );
        Ok(Self {
            header,
            bytes,
            identity,
            coverage_owner: ordinary.owner.clone(),
            coverage_claims: versioned
                .claims()
                .map(
                    |(coverage_schema_version, claim)| VersionedReservedTurnClaim {
                        coverage_schema_version,
                        claim: claim.clone(),
                    },
                )
                .collect(),
        })
    }

    pub(crate) fn binding(&self) -> &PendingBinding {
        &self.header.binding
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Native outer-v2 persistence uses one caller-owned writer transaction.
    /// The caller must roll the whole transaction back on every error.
    pub(crate) async fn persist(&self, txn: &DatabaseTransaction, deadline: Instant) -> Result<()> {
        check_deadline(deadline)?;
        let binding = self.binding();
        binding.validate()?;
        validate_envelope_shape(&self.bytes)?;
        #[derive(Deserialize)]
        struct RawPersistEnvelope<'a> {
            #[serde(borrow)]
            payload: &'a RawValue,
            #[serde(borrow)]
            coverage: &'a RawValue,
        }
        let raw: RawPersistEnvelope<'_> = serde_json::from_slice(&self.bytes).map_err(|_| {
            anyhow::anyhow!("invalid sealed capture recovery artifact; run `libra agent doctor`")
        })?;
        // Repeat entry/wire bounds before allocating typed vectors even though
        // the closed production sealer has already enforced them.
        preflight_versioned_tuples(raw.payload, raw.coverage)?;
        let envelope: VersionedPendingEnvelope =
            serde_json::from_slice(&self.bytes).map_err(|_| {
                anyhow::anyhow!(
                    "invalid sealed capture recovery artifact; run `libra agent doctor`"
                )
            })?;
        envelope.validate()?;
        let mut exact = ExactEnvelopeWriter {
            original: &self.bytes,
            offset: 0,
        };
        serde_json::to_writer(&mut exact, &envelope).map_err(|_| {
            anyhow::anyhow!("sealed capture recovery artifact changed; run `libra agent doctor`")
        })?;
        ensure!(
            exact.offset == self.bytes.len()
                && envelope.binding == *binding
                && self.header.envelope_bytes == self.bytes.len()
                && self.header.chunks == self.bytes.len().div_ceil(CHUNK_BYTES)
                && envelope.coverage.owner == self.coverage_owner
                && envelope.payload.reasoning_source.is_some()
                && self
                    .coverage_claims
                    .iter()
                    .any(|claim| { claim.coverage_schema_version == CoverageSchemaVersion::V2 }),
            "versioned capture recovery requires a sealed v2 claim; run `libra agent doctor`"
        );
        // The sealed type's normal constructor already enforces this. Repeat
        // the native bare-key guard at persistence before any SQLite mutation,
        // verification or lease retention; OG-29 owns version-keyed aliases.
        let mut bare_keys = std::collections::HashSet::with_capacity(
            envelope.payload.identity_commit_associations.len(),
        );
        ensure!(
            envelope
                .payload
                .identity_commit_associations
                .iter()
                .all(|association| { bare_keys.insert(association.logical_turn_key.as_str()) }),
            "native capture recovery cannot persist cross-version turn aliases yet; run `libra agent doctor`"
        );
        drop(bare_keys);
        drop(envelope.payload);
        let plan = envelope.coverage.into_live(
            &self.identity,
            &binding.checkpoint_id,
            binding.parent_commit.clone(),
            binding.first_attempt_millis,
        )?;
        ensure!(
            plan.ordinary().session_id == self.identity.context().session_id()
                && plan.ordinary().owner == self.coverage_owner
                && self.coverage_claims.len() == plan.claims().count()
                && self.coverage_claims.iter().zip(plan.claims()).all(
                    |(given, (version, original))| {
                        given.coverage_schema_version == version && given.claim == *original
                    }
                ),
            "sealed capture recovery claim tuple changed; run `libra agent doctor`"
        );
        check_deadline(deadline)?;
        // All retained leases, chunks, header and alias use this one caller
        // transaction. Any Err requires a full caller rollback.
        txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
            .await
            .context("cannot lock capture recovery storage; run `libra agent doctor`")?;
        pending_identity::assert_private_repo(txn, &binding.scope.repo_id).await?;
        crate::internal::ai::coverage_gate::verify_versioned_reserved_live_claims_with_conn(
            txn,
            &binding.scope,
            &plan,
        )
        .await?;
        crate::internal::ai::coverage_gate::retain_versioned_reserved_live_claims_for_artifact_with_conn(
            txn,
            &binding.scope,
            &plan,
        )
        .await?;
        let headers =
            headers_for_repo(txn, &binding.scope.repo_id, MAX_ARTIFACTS as u64 + 1).await?;
        ensure!(
            headers.len() <= MAX_ARTIFACTS,
            "capture recovery capacity is invalid; run `libra agent doctor`"
        );
        let mut existing = None;
        for entry in &headers {
            let header = decode_entry(entry)?;
            if entry.key == binding.checkpoint_id {
                ensure!(
                    existing.is_none(),
                    "duplicate capture recovery headers; run `libra agent doctor`"
                );
                existing = Some(header);
            }
        }
        if let Some(existing) = existing {
            ensure!(
                existing.binding == *binding
                    && existing.mac == self.header.mac
                    && existing.envelope_bytes == self.header.envelope_bytes
                    && existing.chunks == self.header.chunks,
                "capture recovery artifact conflicts with retained evidence; run `libra agent doctor`"
            );
            // An idempotent delivery never replaces quarantine/manual state
            // or rewrites payload rows. Corrupt chunks remain repair-required.
            let retained = load_chunks(txn, &existing).await?;
            ensure!(
                retained == self.bytes,
                "capture recovery chunks conflict with retained evidence; run `libra agent doctor`"
            );
            self.identity
                .publish_for_artifact(txn, &binding.checkpoint_id)
                .await?;
            check_deadline(deadline)?;
            return Ok(());
        }
        ensure!(
            headers.len() < MAX_ARTIFACTS,
            "capture recovery capacity is full; repair or explicitly erase a retained session"
        );
        let range = chunk_range(&binding.checkpoint_id);
        ensure!(
            MetadataKv::list_bounded_with_conn(
                txn,
                &[MetadataScope::AgentCapturePendingChunk],
                Some(&binding.scope.repo_id),
                Some((&range.0, &range.1)),
                1,
                CHUNK_TEXT_BYTES
            )
            .await?
            .is_empty(),
            "orphaned capture recovery chunks require inspection; run `libra agent doctor`"
        );
        for (index, chunk) in self.bytes.chunks(CHUNK_BYTES).enumerate() {
            check_deadline(deadline)?;
            MetadataKv::set_with_conn(
                txn,
                MetadataScope::AgentCapturePendingChunk,
                &binding.scope.repo_id,
                &chunk_key(&binding.checkpoint_id, index),
                &STANDARD.encode(chunk),
                MetadataValueType::Binary,
            )
            .await?;
        }
        MetadataKv::set_with_conn(
            txn,
            MetadataScope::AgentCapturePending,
            &binding.scope.repo_id,
            &binding.checkpoint_id,
            &serde_json::to_string(&self.header)?,
            MetadataValueType::Text,
        )
        .await?;
        self.identity
            .publish_for_artifact(txn, &binding.checkpoint_id)
            .await?;
        check_deadline(deadline)?;
        Ok(())
    }
}

impl SealedPendingArtifact {
    pub(crate) async fn seal<C: ConnectionTrait>(
        conn: &C,
        storage: &Path,
        root: &Path,
        request: PendingSealRequest<'_>,
        deadline: Instant,
    ) -> Result<Self> {
        let PendingSealRequest {
            binding,
            identity,
            payload,
            coverage,
            reasoning,
        } = request;
        check_deadline(deadline)?;
        binding.validate()?;
        ensure!(
            payload.is_exact_complete_snapshot()
                && payload
                    .snapshot()
                    .source
                    .as_ref()
                    .and_then(|source| source.digest_sha256.as_deref())
                    == Some(binding.source_commitment.as_str()),
            "capture recovery requires a complete sealed source snapshot; retry through the provider"
        );
        ensure!(
            binding.session_id == identity.alias()
                && binding.scope == *identity.context().scope()
                && coverage.checkpoint_id == binding.checkpoint_id
                && coverage.parent_commit == binding.parent_commit
                && coverage.capture_scope.as_ref() == Some(&binding.scope),
            "capture recovery identity changed; retry through the provider"
        );
        let portable_coverage = PortablePendingCoverage::from_live(coverage, &identity)?;
        check_sidecar_capacity(payload, serde_json::to_vec(&portable_coverage)?.len())?;
        validate_payload_binding(payload, &binding)?;
        let projection = if let Some(reasoning) = reasoning {
            let prepared = payload.prepare_pending_native_seal(
                &binding.scope,
                coverage,
                reasoning,
                identity.context().agent_kind(),
            )?;
            PendingPayloadProjection::seal_bound_reasoning(
                conn, storage, root, payload, &identity, prepared, deadline,
            )
            .await?
        } else {
            PendingPayloadProjection::seal(conn, storage, root, payload, &identity, deadline)
                .await?
        };
        let bytes = encode_envelope(binding.clone(), projection, portable_coverage)?;
        check_deadline(deadline)?;
        let mac = binding
            .scope
            .sign_pending_envelope_until(conn, storage, root, &bytes, deadline)
            .await
            .map_err(|error| {
                error.context(
                    "cannot authenticate capture recovery artifact; run `libra agent doctor`",
                )
            })?;
        let header = PendingHeader {
            version: VERSION,
            binding,
            mac,
            envelope_bytes: bytes.len(),
            chunks: bytes.len().div_ceil(CHUNK_BYTES),
            manual_attempted: false,
        };
        ensure!(
            serde_json::to_vec(&header)?.len() <= MAX_HEADER_BYTES,
            "capture recovery header exceeds its capacity; retry through the provider"
        );
        Ok(Self {
            header,
            bytes,
            identity,
            coverage_owner: coverage.owner.clone(),
            coverage_claims: coverage.claims.clone(),
        })
    }

    pub(crate) fn binding(&self) -> &PendingBinding {
        &self.header.binding
    }

    /// Caller must have verified catalog and coverage fences in this same
    /// SQLite writer transaction. No pool wrapper can split that atomicity.
    pub(crate) async fn persist(&self, txn: &DatabaseTransaction, deadline: Instant) -> Result<()> {
        check_deadline(deadline)?;
        let binding = self.binding();
        // Acquire the writer lock before capacity/discovery reads. All
        // artifact rows and the authenticated alias commit together.
        txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
            .await
            .context("cannot lock capture recovery storage; run `libra agent doctor`")?;
        pending_identity::assert_private_repo(txn, &binding.scope.repo_id).await?;
        crate::internal::ai::coverage_gate::verify_reserved_live_claims_with_conn(
            txn,
            &binding.scope,
            self.identity.context().session_id(),
            &self.coverage_owner,
            &self.coverage_claims,
        )
        .await?;
        // From this commit on the artifact, not the hook process, owns the
        // claims: an ordinary 60 s lease expiry must not let a redelivery,
        // resumed writer, or import fence them out. Any later failure in
        // this transaction rolls the retention back with the artifact rows.
        crate::internal::ai::coverage_gate::retain_reserved_live_claims_for_artifact_with_conn(
            txn,
            self.identity.context().session_id(),
            &self.coverage_owner,
            &self.coverage_claims,
        )
        .await?;
        let headers =
            headers_for_repo(txn, &binding.scope.repo_id, MAX_ARTIFACTS as u64 + 1).await?;
        ensure!(
            headers.len() <= MAX_ARTIFACTS,
            "capture recovery capacity is invalid; run `libra agent doctor`"
        );
        let mut existing = None;
        for entry in &headers {
            let header = decode_entry(entry)?;
            if entry.key == binding.checkpoint_id {
                ensure!(
                    existing.is_none(),
                    "duplicate capture recovery headers; run `libra agent doctor`"
                );
                existing = Some(header);
            }
        }
        if let Some(existing) = existing {
            ensure!(
                existing.binding == *binding
                    && existing.mac == self.header.mac
                    && existing.envelope_bytes == self.header.envelope_bytes
                    && existing.chunks == self.header.chunks,
                "capture recovery artifact conflicts with retained evidence; run `libra agent doctor`"
            );
            // An idempotent delivery never replaces quarantine/manual state
            // or rewrites payload rows. Corrupt chunks remain repair-required.
            let retained = load_chunks(txn, &existing).await?;
            ensure!(
                retained == self.bytes,
                "capture recovery chunks conflict with retained evidence; run `libra agent doctor`"
            );
            self.identity
                .publish_for_artifact(txn, &binding.checkpoint_id)
                .await?;
            check_deadline(deadline)?;
            return Ok(());
        }
        ensure!(
            headers.len() < MAX_ARTIFACTS,
            "capture recovery capacity is full; repair or explicitly erase a retained session"
        );
        let range = chunk_range(&binding.checkpoint_id);
        ensure!(
            MetadataKv::list_bounded_with_conn(
                txn,
                &[MetadataScope::AgentCapturePendingChunk],
                Some(&binding.scope.repo_id),
                Some((&range.0, &range.1)),
                1,
                CHUNK_TEXT_BYTES
            )
            .await?
            .is_empty(),
            "orphaned capture recovery chunks require inspection; run `libra agent doctor`"
        );
        for (index, chunk) in self.bytes.chunks(CHUNK_BYTES).enumerate() {
            check_deadline(deadline)?;
            MetadataKv::set_with_conn(
                txn,
                MetadataScope::AgentCapturePendingChunk,
                &binding.scope.repo_id,
                &chunk_key(&binding.checkpoint_id, index),
                &STANDARD.encode(chunk),
                MetadataValueType::Binary,
            )
            .await?;
        }
        MetadataKv::set_with_conn(
            txn,
            MetadataScope::AgentCapturePending,
            &binding.scope.repo_id,
            &binding.checkpoint_id,
            &serde_json::to_string(&self.header)?,
            MetadataValueType::Text,
        )
        .await?;
        self.identity
            .publish_for_artifact(txn, &binding.checkpoint_id)
            .await?;
        check_deadline(deadline)?;
        Ok(())
    }
}

fn encode_envelope(
    binding: PendingBinding,
    payload: PendingPayloadProjection,
    coverage: PortablePendingCoverage,
) -> Result<Vec<u8>> {
    coverage.validate()?;
    // The payload owner's closed codec validates nested sidecars and depth.
    payload.encode()?;
    let envelope = PendingEnvelope {
        version: VERSION,
        binding,
        payload,
        coverage,
    };
    let bytes = serde_json::to_vec(&envelope).map_err(|_| {
        anyhow::anyhow!("cannot encode capture recovery artifact; retry through the provider")
    })?;
    validate_envelope_shape(&bytes)?;
    Ok(bytes)
}

/// Bound nesting before typed allocation; exact re-encoding below rejects
/// duplicate fields and noncanonical representations, including nested JSON.
fn validate_envelope_shape(bytes: &[u8]) -> Result<()> {
    ensure!(
        !bytes.is_empty() && bytes.len() <= MAX_ENVELOPE_BYTES,
        "capture recovery envelope exceeds its safe capacity; run `libra agent doctor`"
    );
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    ensure!(
                        depth <= 66,
                        "capture recovery nesting exceeds its safe limit; run `libra agent doctor`"
                    );
                }
                b'}' | b']' => {
                    depth = depth
                        .checked_sub(1)
                        .context("invalid capture recovery envelope; run `libra agent doctor`")?;
                }
                _ => {}
            }
        }
    }
    ensure!(
        !quoted && depth == 0,
        "invalid capture recovery envelope; run `libra agent doctor`"
    );
    Ok(())
}

fn check_sidecar_capacity(
    payload: &CheckpointRedactedPayload,
    coverage_bytes: usize,
) -> Result<()> {
    ensure!(
        payload.transcript().len() <= MAX_TRANSCRIPT_BYTES,
        "capture recovery transcript exceeds its safe capacity; run `libra agent doctor`"
    );
    let size = serde_json::to_vec(payload.snapshot())?
        .len()
        .checked_add(coverage_bytes)
        .and_then(|size| size.checked_add(payload.metadata_json().len()))
        .and_then(|size| size.checked_add(payload.lifecycle_events_jsonl().len()))
        .and_then(|size| size.checked_add(payload.redaction_report_json().len()));
    ensure!(
        size.is_some_and(|size| size <= MAX_SIDECAR_BYTES),
        "capture recovery sidecars exceed their safe capacity; run `libra agent doctor`"
    );
    Ok(())
}

fn validate_payload_binding(
    payload: &CheckpointRedactedPayload,
    binding: &PendingBinding,
) -> Result<()> {
    let metadata: serde_json::Value = serde_json::from_slice(payload.metadata_json().bytes())
        .map_err(|_| {
            anyhow::anyhow!("invalid capture recovery metadata; run `libra agent doctor`")
        })?;
    ensure!(
        metadata
            .get("checkpoint_id")
            .and_then(serde_json::Value::as_str)
            == Some(binding.checkpoint_id.as_str()),
        "capture recovery checkpoint identity changed; run `libra agent doctor`"
    );
    let mut matching_event = false;
    for line in payload
        .lifecycle_events_jsonl()
        .bytes()
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
    {
        let event: serde_json::Value = serde_json::from_slice(line).map_err(|_| {
            anyhow::anyhow!("invalid capture recovery lifecycle; run `libra agent doctor`")
        })?;
        matching_event |= event.get("event_id").and_then(serde_json::Value::as_str)
            == Some(binding.event_id.as_str());
    }
    ensure!(
        matching_event,
        "capture recovery event identity changed; run `libra agent doctor`"
    );
    Ok(())
}

pub(crate) async fn load_verified_payload<C: ConnectionTrait>(
    conn: &C,
    storage: &Path,
    root: &Path,
    identity: &PreparedPendingAlias,
    expected: &PendingBinding,
    header: &PendingHeader,
    deadline: Instant,
) -> Result<VerifiedPendingPayload> {
    check_deadline(deadline)?;
    expected.validate()?;
    ensure!(
        &header.binding == expected
            && expected.session_id == identity.alias()
            && expected.scope == *identity.context().scope(),
        "capture recovery binding changed; run `libra agent doctor`"
    );
    let bytes = load_chunks(conn, header).await?;
    let authentication = AuthenticatedPendingEnvelope::verify(
        conn,
        storage,
        root,
        &expected.scope,
        &bytes,
        &header.mac,
        deadline,
    )
    .await?;
    check_deadline(deadline)?;
    validate_envelope_shape(&bytes)?;
    let envelope: PendingEnvelope = serde_json::from_slice(&bytes).map_err(|_| {
        anyhow::anyhow!("invalid authenticated capture recovery envelope; run `libra agent doctor`")
    })?;
    ensure!(
        envelope.version == VERSION
            && envelope.binding == *expected
            && serde_json::to_vec(&envelope)? == bytes,
        "invalid authenticated capture recovery binding; run `libra agent doctor`"
    );
    envelope.coverage.validate()?;
    let coverage_bytes = serde_json::to_vec(&envelope.coverage)?.len();
    let reasoning_integrity = if envelope.payload.is_reasoning_v2() {
        Some(
            key::authenticate_pending_reasoning_original_in_scope_until(
                conn,
                &expected.scope,
                storage,
                root,
                &bytes,
                &header.mac,
                deadline,
            )
            .await?,
        )
    } else {
        None
    };
    let payload = if reasoning_integrity.is_some() {
        envelope
            .payload
            .rehydrate_v2_deferred(&authentication, conn, storage, root, identity, deadline)
            .await?
    } else {
        envelope
            .payload
            .rehydrate(&authentication, conn, storage, root, identity, deadline)
            .await?
    };
    check_sidecar_capacity(&payload, coverage_bytes)?;
    validate_payload_binding(&payload, expected)?;
    ensure!(
        payload.is_exact_complete_snapshot()
            && payload
                .snapshot()
                .source
                .as_ref()
                .and_then(|source| source.digest_sha256.as_deref())
                == Some(expected.source_commitment.as_str()),
        "capture recovery source commitment changed; run `libra agent doctor`"
    );
    check_deadline(deadline)?;
    let payload = if let Some(integrity) = reasoning_integrity {
        payload.with_deferred_pending_reasoning(envelope.payload.into_deferred_reasoning(
            bytes,
            header.mac.clone(),
            integrity,
        )?)?
    } else {
        payload
    };
    Ok(VerifiedPendingPayload {
        payload,
        coverage: envelope.coverage.into_live(
            identity,
            &expected.checkpoint_id,
            expected.parent_commit.clone(),
            chrono::Utc::now().timestamp_millis(),
        )?,
        header: header.clone(),
        context: identity.context().clone(),
    })
}

/// Read an explicitly selected committed outer-v2 artifact. This returns a
/// local-integrity carrier only; COV2-08 owns native source replay and ACKs.
#[allow(dead_code)] // COV2-08 connects this separate authenticated recovery path.
pub(crate) async fn load_verified_versioned_payload<C: ConnectionTrait>(
    conn: &C,
    storage: &Path,
    root: &Path,
    identity: &PreparedPendingAlias,
    expected: &PendingBinding,
    header: &PendingHeader,
    deadline: Instant,
) -> Result<VerifiedVersionedPendingPayload> {
    check_deadline(deadline)?;
    expected.validate()?;
    ensure!(
        &header.binding == expected
            && expected.session_id == identity.alias()
            && expected.scope == *identity.context().scope(),
        "capture recovery binding changed; run `libra agent doctor`"
    );
    let bytes = load_chunks(conn, header).await?;
    let authentication = AuthenticatedPendingEnvelope::verify(
        conn,
        storage,
        root,
        &expected.scope,
        &bytes,
        &header.mac,
        deadline,
    )
    .await?;
    let envelope = decode_authenticated_versioned_envelope(&authentication, expected)?;
    ensure!(
        envelope.payload.reasoning_source.is_some(),
        "versioned capture recovery requires retained source evidence; run `libra agent doctor`"
    );
    let integrity = key::authenticate_pending_versioned_reasoning_members_in_scope_until(
        conn,
        key::PendingVersionedReasoningAuthentication {
            scope: &expected.scope,
            storage_path: storage,
            authorized_root: root,
            original_envelope: &bytes,
            original_mac: &header.mac,
            expected_binding: expected,
            candidate: &envelope,
            deadline,
        },
    )
    .await?;
    let coverage_bytes = serde_json::to_vec(&envelope.coverage)?.len();
    let payload = VersionedPendingPayloadProjection::rehydrate_authenticated_deferred(
        VersionedPendingRehydrateMaterial {
            projection: &envelope.payload,
            authentication: &authentication,
            integrity: &integrity,
        },
        conn,
        storage,
        root,
        identity,
        deadline,
    )
    .await?;
    check_sidecar_capacity(&payload, coverage_bytes)?;
    validate_payload_binding(&payload, expected)?;
    ensure!(
        payload.is_exact_complete_snapshot()
            && payload
                .snapshot()
                .source
                .as_ref()
                .and_then(|source| source.digest_sha256.as_deref())
                == Some(expected.source_commitment.as_str()),
        "capture recovery source commitment changed; run `libra agent doctor`"
    );
    let coverage = envelope.coverage.into_live(
        identity,
        &expected.checkpoint_id,
        expected.parent_commit.clone(),
        chrono::Utc::now().timestamp_millis(),
    )?;
    check_deadline(deadline)?;
    let reasoning = VersionedPendingPayloadProjection::into_deferred_reasoning(
        VersionedPendingReplayMaterial {
            projection: envelope.payload,
            original_envelope: bytes,
            original_mac: header.mac.clone(),
            integrity,
        },
    )?;
    check_deadline(deadline)?;
    Ok(VerifiedVersionedPendingPayload {
        payload,
        coverage,
        reasoning,
        header: header.clone(),
        context: identity.context().clone(),
    })
}

pub(crate) fn retryable_load_failure(error: &anyhow::Error, deadline: Instant) -> bool {
    Instant::now() >= deadline
        || error.chain().any(|cause| {
            cause.is::<sea_orm::DbErr>()
                || cause
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(pending_identity::transient_io_failure)
                || cause
                    .downcast_ref::<crate::internal::workspace::WorkspaceError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            crate::internal::workspace::WorkspaceError::ReadFailed(_)
                                | crate::internal::workspace::WorkspaceError::LeaseLost { .. }
                        )
                    })
                || cause
                    .downcast_ref::<CaptureCatalogError>()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            CaptureCatalogError::Database
                                | CaptureCatalogError::TransactionStart
                                | CaptureCatalogError::CommitFailed
                                | CaptureCatalogError::DeadlineExceeded
                                | CaptureCatalogError::SchemaUnavailable
                                | CaptureCatalogError::WorkspaceLeaseRejected
                        )
                    })
                || cause
                    .downcast_ref::<
                        crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError,
                    >()
                    .is_some_and(|error| {
                        matches!(
                            error,
                            crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
                                | crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
                        )
                    })
        })
}

fn decode_part(text: &str, cap: usize) -> Result<Vec<u8>> {
    ensure!(
        text.len() <= cap.div_ceil(3) * 4,
        "capture recovery field exceeds its safe capacity; run `libra agent doctor`"
    );
    let bytes = STANDARD.decode(text).map_err(|_| {
        anyhow::anyhow!("invalid capture recovery encoding; run `libra agent doctor`")
    })?;
    ensure!(
        bytes.len() <= cap && STANDARD.encode(&bytes) == text,
        "invalid capture recovery encoding; run `libra agent doctor`"
    );
    Ok(bytes)
}

fn check_deadline(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(anyhow::Error::new(CaptureCatalogError::DeadlineExceeded)
            .context("capture recovery budget elapsed; retry with `libra agent doctor --repair`"));
    }
    Ok(())
}

fn chunk_key(checkpoint: &str, index: usize) -> String {
    format!("{checkpoint}:{index:03}")
}
fn chunk_range(checkpoint: &str) -> (String, String) {
    (format!("{checkpoint}:"), format!("{checkpoint};"))
}

pub(crate) async fn headers_for_repo<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    limit: u64,
) -> Result<Vec<crate::internal::metadata::MetadataEntry>> {
    MetadataKv::list_bounded_with_conn(
        conn,
        &HEADER_SCOPES,
        Some(repo_id),
        None,
        limit,
        MAX_HEADER_BYTES,
    )
    .await
}

pub(crate) fn decode_entry(
    entry: &crate::internal::metadata::MetadataEntry,
) -> Result<PendingHeader> {
    ensure!(
        entry.value_type == "text",
        "invalid capture recovery header type; run `libra agent doctor`"
    );
    let header = PendingHeader::decode(&entry.value)?;
    ensure!(
        header.binding.scope.repo_id == entry.target && header.binding.checkpoint_id == entry.key,
        "capture recovery header identity changed; run `libra agent doctor`"
    );
    Ok(header)
}

/// Discovery never grants replay authority. The executor must resolve the
/// alias and verify MAC, receipt, scope and coverage fences before use.
pub(crate) struct PendingCandidateBatch {
    pub(crate) headers: Vec<PendingHeader>,
    pub(crate) quarantined: usize,
    pub(crate) conflicting_namespaces: bool,
    pub(crate) window_full: bool,
}

// A duplicate namespace key cannot be moved without overwriting evidence.
// Exclude it from the automatic queue, but retain both rows for cold doctor
// diagnostics. Otherwise five such rows could starve every later artifact.
const ELIGIBLE_HEADERS: &str = "FROM metadata_kv AS p
    WHERE p.scope = 'agent_capture_pending' AND p.target = ?
      AND NOT EXISTS (SELECT 1 FROM metadata_kv AS q
        WHERE q.scope = 'agent_capture_quarantine'
          AND q.target = p.target AND q.key = p.key)";

async fn validate_queue_repo<C: ConnectionTrait>(conn: &C, repo_id: &str) -> Result<()> {
    let current = crate::internal::workspace::RepoIdentity::resolve(conn)
        .await
        .context(
            "cannot establish capture recovery repository; restore repository configuration",
        )?;
    ensure!(
        current.as_str() == repo_id,
        "capture recovery repository changed; run `libra agent doctor`"
    );
    Ok(())
}

/// Parent-side indexed hint: no header, chunk, alias or key hydration. A hint
/// is not a validity/eligibility decision; that belongs to the bounded child.
pub(crate) async fn has_pending_hint<C: ConnectionTrait>(
    conn: &C,
    scope: &CaptureScope,
) -> Result<bool> {
    validate_queue_repo(conn, &scope.repo_id).await?;
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM metadata_kv AS p
             WHERE p.scope = 'agent_capture_pending' AND p.target = ?
               AND NOT EXISTS (SELECT 1 FROM metadata_kv AS q
                 WHERE q.scope = 'agent_capture_quarantine'
                   AND q.target = p.target AND q.key = p.key)
               AND CASE WHEN p.value_type = 'text' AND typeof(p.value) = 'text'
                      AND length(CAST(p.value AS BLOB)) <= 8192
                      THEN CASE WHEN json_valid(p.value)
                        AND json_type(p.value, '$.manual_attempted') IS NOT 'true'
                        AND json_type(p.value, '$.binding.scope.repo_id') = 'text'
                        AND json_type(p.value, '$.binding.scope.worktree_id') = 'text'
                        AND (json_type(p.value, '$.binding.scope.workspace_id') = 'text'
                          OR json_type(p.value, '$.binding.scope.workspace_id') = 'null')
                        AND (json_type(p.value, '$.binding.scope.workspace_fence') = 'integer'
                          OR json_type(p.value, '$.binding.scope.workspace_fence') = 'null')
                        AND json_extract(p.value, '$.binding.scope.repo_id') IS ?
                        AND json_extract(p.value, '$.binding.scope.worktree_id') IS ?
                        AND json_extract(p.value, '$.binding.scope.workspace_id') IS ?
                        AND json_extract(p.value, '$.binding.scope.workspace_fence') IS ?
                        THEN 1 ELSE 0 END
                      ELSE 0 END = 1
             LIMIT 1",
            [
                scope.repo_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .context("cannot inspect capture recovery queue; run `libra agent doctor`")?
        .is_some())
}

/// Select exactly one bounded batch under the SQLite writer lock. Malformed
/// rows are moved by SQL, without hydrating oversized keys/values. Reverse
/// order keeps selection offsets stable while removing invalid candidates;
/// those offsets never escape this locked transaction or identify a session.
/// Commit this discovery transaction before processing individual candidates;
/// a later per-candidate failure must not roll back these quarantine moves.
#[cfg(test)]
pub(crate) async fn pending_candidates(
    txn: &DatabaseTransaction,
    repo_id: &str,
) -> Result<PendingCandidateBatch> {
    pending_candidates_inner(txn, repo_id, None, 5).await
}

pub(crate) async fn pending_candidates_for_scope(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
) -> Result<PendingCandidateBatch> {
    // A child-pending header can remain indefinitely. See every bounded
    // same-scope candidate, while the caller still limits actual attempts.
    pending_candidates_inner(txn, &scope.repo_id, Some(scope), MAX_ARTIFACTS).await
}

async fn pending_candidates_inner(
    txn: &DatabaseTransaction,
    repo_id: &str,
    scope: Option<&CaptureScope>,
    selection_limit: usize,
) -> Result<PendingCandidateBatch> {
    ensure!(
        (1..=MAX_ARTIFACTS).contains(&selection_limit),
        "capture recovery selection limit is invalid; rerun libra agent doctor"
    );
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .context("cannot lock capture recovery queue; run `libra agent doctor`")?;
    validate_queue_repo(txn, repo_id).await?;
    // LIMIT on the final output alone would not bound the correlated queue
    // predicate's work after out-of-band over-capacity corruption.
    let capacity = txn.query_all_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT 1 FROM metadata_kv WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
          AND target = ? LIMIT 17",
        [repo_id.into()],
    )).await.context("cannot inspect capture recovery capacity; run `libra agent doctor`")?;
    ensure!(
        capacity.len() <= MAX_ARTIFACTS,
        "capture recovery capacity exceeds its safe limit; run `libra agent doctor`"
    );
    let scope_filter = if scope.is_some() {
        "AND (typeof(p.value) <> 'text' OR NOT json_valid(CAST(p.value AS TEXT))
          OR CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
            THEN json_type(CAST(p.value AS TEXT), '$.binding.scope.worktree_id') END IS NOT 'text'
          OR CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
            THEN json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_id') END IS NOT 'text'
            AND CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
              THEN json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_id') END IS NOT 'null'
          OR CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
            THEN json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_fence') END IS NOT 'integer'
            AND CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
              THEN json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_fence') END IS NOT 'null'
          OR (CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
            THEN json_extract(CAST(p.value AS TEXT), '$.binding.scope.worktree_id') END IS ?
          AND CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
            THEN json_extract(CAST(p.value AS TEXT), '$.binding.scope.workspace_id') END IS ?
          AND CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
            THEN json_extract(CAST(p.value AS TEXT), '$.binding.scope.workspace_fence') END IS ?))"
    } else {
        ""
    };
    let mut selection_values = vec![repo_id.into()];
    let scope_valid_expr = if scope.is_some() {
        "CASE WHEN typeof(p.value) = 'text' AND json_valid(CAST(p.value AS TEXT))
          AND json_type(CAST(p.value AS TEXT), '$.binding.scope.worktree_id') = 'text'
          AND (json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_id') = 'text'
            OR json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_id') = 'null')
          AND (json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_fence') = 'integer'
            OR json_type(CAST(p.value AS TEXT), '$.binding.scope.workspace_fence') = 'null')
          THEN 1 ELSE 0 END"
    } else {
        "1"
    };
    if let Some(scope) = scope {
        selection_values.push(scope.worktree_id.clone().into());
        selection_values.push(scope.workspace_id.clone().into());
        selection_values.push(scope.workspace_fence.into());
    }
    let rows = txn.query_all_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        format!("SELECT
          CASE WHEN typeof(p.key) = 'text' AND length(CAST(p.key AS BLOB)) <= 36 THEN CAST(p.key AS BLOB) ELSE NULL END AS bounded_key,
          CASE WHEN p.value_type = 'text' AND typeof(p.value) = 'text'
            AND length(CAST(p.value AS BLOB)) <= 8192 THEN CAST(p.value AS BLOB) ELSE NULL END AS bounded_value,
          {scope_valid_expr} AS scope_valid
          {ELIGIBLE_HEADERS} {scope_filter} ORDER BY p.key LIMIT {selection_limit}"),
        selection_values,
    )).await.context("cannot read capture recovery candidates; run `libra agent doctor`")?;
    // Conservatively preserve deferred handling if a candidate disappears
    // while another full snapshot is installed concurrently.
    let window_full = rows.len() == selection_limit;
    let mut headers = Vec::with_capacity(rows.len());
    let mut invalid = Vec::new();
    for (index, row) in rows.into_iter().enumerate() {
        let key: Option<Vec<u8>> = row
            .try_get_by("bounded_key")
            .context("cannot decode capture recovery candidate; run `libra agent doctor`")?;
        let text: Option<Vec<u8>> = row
            .try_get_by("bounded_value")
            .context("cannot decode capture recovery candidate; run `libra agent doctor`")?;
        let scope_valid: i64 = row
            .try_get_by("scope_valid")
            .context("cannot decode capture recovery candidate scope")?;
        if scope_valid != 1 {
            invalid.push(index);
            continue;
        }
        let header = text
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| PendingHeader::decode(text).ok());
        match header {
            Some(header)
                if key.as_deref() == Some(header.binding.checkpoint_id.as_bytes())
                    && header.binding.scope.repo_id == repo_id
                    && scope.is_none_or(|expected| header.binding.scope == *expected)
                    && !header.manual_attempted =>
            {
                headers.push(header)
            }
            _ => invalid.push(index),
        }
    }
    for index in invalid.iter().rev() {
        let mut invalid_values = vec![repo_id.into(), repo_id.into()];
        if let Some(scope) = scope {
            invalid_values.push(scope.worktree_id.clone().into());
            invalid_values.push(scope.workspace_id.clone().into());
            invalid_values.push(scope.workspace_fence.into());
        }
        invalid_values.push((*index as i64).into());
        let changed = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                format!(
                    "UPDATE metadata_kv SET scope = 'agent_capture_quarantine'
              WHERE scope = 'agent_capture_pending' AND target = ? AND key =
                (SELECT p.key {ELIGIBLE_HEADERS} {scope_filter} ORDER BY p.key LIMIT 1 OFFSET ?)"
                ),
                invalid_values,
            ))
            .await
            .context("cannot retain invalid capture recovery evidence; run `libra agent doctor`")?;
        ensure!(
            changed.rows_affected() == 1,
            "capture recovery queue changed; retry `libra agent doctor`"
        );
    }
    let conflicting_namespaces = txn.query_one_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT 1 FROM metadata_kv AS p WHERE p.scope = 'agent_capture_pending' AND p.target = ?
          AND EXISTS (SELECT 1 FROM metadata_kv AS q WHERE q.scope = 'agent_capture_quarantine'
            AND q.target = p.target AND q.key = p.key) LIMIT 1",
        [repo_id.into()],
    )).await.context("cannot inspect conflicting capture evidence; run `libra agent doctor`")?.is_some();
    Ok(PendingCandidateBatch {
        headers,
        quarantined: invalid.len(),
        conflicting_namespaces,
        window_full,
    })
}

pub(crate) async fn current_header_namespace(
    txn: &DatabaseTransaction,
    expected: &PendingHeader,
) -> Result<MetadataScope> {
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .context("cannot lock capture recovery header; run `libra agent doctor`")?;
    validate_queue_repo(txn, &expected.binding.scope.repo_id).await?;
    let encoded = serde_json::to_string(expected)?;
    PendingHeader::decode(&encoded)?;
    let rows = txn
        .query_all_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT scope, CASE WHEN value_type = 'text' AND typeof(value) = 'text'
          AND length(CAST(value AS BLOB)) <= 8192 THEN CAST(value AS BLOB) ELSE NULL END AS bounded_value
         FROM metadata_kv WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
          AND target = ? AND key = ? LIMIT 2",
            [
                expected.binding.scope.repo_id.clone().into(),
                expected.binding.checkpoint_id.clone().into(),
            ],
        ))
        .await
        .context("cannot inspect capture recovery header; run `libra agent doctor`")?;
    ensure!(
        rows.len() == 1,
        "capture recovery header is missing or conflicting; run `libra agent doctor`"
    );
    let text: Option<Vec<u8>> = rows[0]
        .try_get_by("bounded_value")
        .context("cannot decode capture recovery header; run `libra agent doctor`")?;
    ensure!(
        text.as_deref() == Some(encoded.as_bytes()),
        "capture recovery header changed; retry `libra agent doctor`"
    );
    let scope: String = rows[0]
        .try_get_by("scope")
        .context("cannot decode capture recovery namespace; run `libra agent doctor`")?;
    match scope.as_str() {
        "agent_capture_pending" => Ok(MetadataScope::AgentCapturePending),
        "agent_capture_quarantine" => Ok(MetadataScope::AgentCaptureQuarantine),
        _ => anyhow::bail!("invalid capture recovery namespace; run `libra agent doctor`"),
    }
}

/// Read at most one bounded header for a doctor-selected deterministic
/// checkpoint. Duplicate namespace rows, malformed UTF-8/JSON, and oversized
/// values remain visible as an error for this artifact only; callers can
/// continue the rest of the bounded recovery batch.
pub(crate) async fn header_for_checkpoint<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    checkpoint_id: &str,
) -> Result<Option<(PendingHeader, MetadataScope)>> {
    validate_queue_repo(conn, repo_id).await?;
    let rows = conn
        .query_all_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT scope, value_type,
                    CASE WHEN typeof(value) = 'text' AND value_type = 'text'
                      AND length(CAST(value AS BLOB)) <= 8192
                      THEN CAST(value AS BLOB) ELSE NULL END AS bounded_value
             FROM metadata_kv
             WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
               AND target = ? AND key = ? LIMIT 2",
            [repo_id.into(), checkpoint_id.into()],
        ))
        .await
        .context("cannot inspect capture recovery header; run `libra agent doctor`")?;
    if rows.is_empty() {
        return Ok(None);
    }
    ensure!(
        rows.len() == 1,
        "capture recovery header is missing or conflicting; run `libra agent doctor`"
    );
    let scope: String = rows[0]
        .try_get_by("scope")
        .context("cannot decode capture recovery namespace; run `libra agent doctor`")?;
    let value_type: String = rows[0]
        .try_get_by("value_type")
        .context("cannot decode capture recovery header type; run `libra agent doctor`")?;
    let value: Option<Vec<u8>> = rows[0]
        .try_get_by("bounded_value")
        .context("cannot decode capture recovery header; run `libra agent doctor`")?;
    ensure!(
        value_type == "text",
        "invalid capture recovery header type; run `libra agent doctor`"
    );
    let text = std::str::from_utf8(value.as_deref().ok_or_else(|| {
        anyhow::anyhow!("oversized capture recovery header; run `libra agent doctor`")
    })?)
    .context("invalid capture recovery header encoding; run `libra agent doctor`")?;
    let header = PendingHeader::decode(text)?;
    ensure!(
        header.binding.checkpoint_id == checkpoint_id && header.binding.scope.repo_id == repo_id,
        "capture recovery header binding changed; run `libra agent doctor`"
    );
    let scope = match scope.as_str() {
        "agent_capture_pending" => MetadataScope::AgentCapturePending,
        "agent_capture_quarantine" => MetadataScope::AgentCaptureQuarantine,
        _ => anyhow::bail!("invalid capture recovery namespace; run `libra agent doctor`"),
    };
    Ok(Some((header, scope)))
}

/// Artifact-only quarantine: preserve exact bytes, alias and original receipt.
/// This does not quarantine the session or increment its ledger revision.
#[cfg(test)]
pub(crate) async fn quarantine_header(
    txn: &DatabaseTransaction,
    expected: &PendingHeader,
) -> Result<()> {
    let scope = current_header_namespace(txn, expected).await?;
    if scope == MetadataScope::AgentCaptureQuarantine {
        return Ok(());
    }
    let result = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE metadata_kv SET scope = 'agent_capture_quarantine'
         WHERE scope = 'agent_capture_pending' AND target = ? AND key = ?",
            [
                expected.binding.scope.repo_id.clone().into(),
                expected.binding.checkpoint_id.clone().into(),
            ],
        ))
        .await
        .context("cannot quarantine capture recovery evidence; run `libra agent doctor`")?;
    ensure!(
        result.rows_affected() == 1,
        "capture recovery header changed; retry `libra agent doctor`"
    );
    Ok(())
}

/// Codec CAS only, not an authorization capability. The executor must check
/// all current authority fences and write its audit in THIS transaction before
/// committing. Precommit authority/audit failures roll this bit back; after
/// commit it is consumed even if replay fails. Policy/counters are intact.
pub(crate) async fn claim_manual_attempt(
    txn: &DatabaseTransaction,
    expected: &PendingHeader,
) -> Result<PendingHeader> {
    ensure!(
        !expected.manual_attempted,
        "capture recovery manual attempt was already used; provide a newly authorized event or explicitly erase the session"
    );
    let scope = current_header_namespace(txn, expected).await?;
    ensure!(
        scope == MetadataScope::AgentCaptureQuarantine,
        "capture recovery requires artifact quarantine before a manual claim; run `libra agent doctor --repair`"
    );
    let mut claimed = expected.clone();
    claimed.manual_attempted = true;
    MetadataKv::set_with_conn(
        txn,
        scope,
        &claimed.binding.scope.repo_id,
        &claimed.binding.checkpoint_id,
        &serde_json::to_string(&claimed)?,
        MetadataValueType::Text,
    )
    .await?;
    Ok(claimed)
}

async fn load_chunks<C: ConnectionTrait>(conn: &C, header: &PendingHeader) -> Result<Vec<u8>> {
    let range = chunk_range(&header.binding.checkpoint_id);
    let entries = MetadataKv::list_bounded_with_conn(
        conn,
        &[MetadataScope::AgentCapturePendingChunk],
        Some(&header.binding.scope.repo_id),
        Some((&range.0, &range.1)),
        MAX_CHUNKS as u64 + 1,
        CHUNK_TEXT_BYTES,
    )
    .await?;
    ensure!(
        entries.len() == header.chunks,
        "capture recovery chunks are missing or excessive; run `libra agent doctor`"
    );
    let mut bytes = Vec::with_capacity(header.envelope_bytes);
    for (index, entry) in entries.iter().enumerate() {
        ensure!(
            entry.key == chunk_key(&header.binding.checkpoint_id, index)
                && entry.value_type == "binary",
            "capture recovery chunk ordering is invalid; run `libra agent doctor`"
        );
        let chunk = decode_part(&entry.value, CHUNK_BYTES)?;
        let expected = if index + 1 == header.chunks {
            header.envelope_bytes - index * CHUNK_BYTES
        } else {
            CHUNK_BYTES
        };
        ensure!(
            chunk.len() == expected,
            "capture recovery chunk length is invalid; run `libra agent doctor`"
        );
        bytes.extend_from_slice(&chunk);
    }
    ensure!(
        bytes.len() == header.envelope_bytes,
        "capture recovery envelope length is invalid; run `libra agent doctor`"
    );
    Ok(bytes)
}

/// GC deliberately reads only small headers. No chunk hydration, MAC/key I/O,
/// or generic JSON OID discovery is needed for conservative parent roots.
pub(crate) async fn gc_parent_roots<C: ConnectionTrait>(conn: &C) -> Result<Vec<String>> {
    let entries = MetadataKv::list_bounded_with_conn(
        conn,
        &HEADER_SCOPES,
        None,
        None,
        MAX_ARTIFACTS as u64 + 1,
        MAX_HEADER_BYTES,
    )
    .await?;
    ensure!(
        entries.len() <= MAX_ARTIFACTS,
        "capture recovery capacity exceeds its safe limit; destructive maintenance stopped"
    );
    let mut roots = Vec::new();
    for entry in entries {
        if let Some(parent) = decode_entry(&entry)?.binding.parent_commit {
            roots.push(parent);
        }
    }
    Ok(roots)
}

pub(crate) async fn has_header_for_checkpoint<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    checkpoint: &str,
) -> Result<bool> {
    ensure!(
        uuid::Uuid::parse_str(checkpoint).is_ok_and(|id| id.to_string() == checkpoint),
        "invalid capture artifact cleanup identity; run `libra agent doctor`"
    );
    Ok(conn.query_one_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "SELECT 1 FROM metadata_kv WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
          AND target = ? AND key = ? LIMIT 1", [repo_id.into(), checkpoint.into()],
    )).await.context("cannot inspect capture recovery header; run `libra agent doctor`")?.is_some())
}

/// Native-redelivery ownership probe: an authenticated artifact (pending or
/// quarantined) is retained for this stable terminal checkpoint, and its
/// checkpoint row has not been published yet. One indexed point lookup; it
/// hydrates no header, chunk, alias or key. Once the checkpoint row exists a
/// redelivery may still finish the receipt through the durable-replay path.
pub(crate) async fn retains_unpublished_artifact_for_checkpoint<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    checkpoint: &str,
) -> Result<bool> {
    ensure!(
        uuid::Uuid::parse_str(checkpoint).is_ok_and(|id| id.to_string() == checkpoint),
        "invalid capture artifact identity; run `libra agent doctor`"
    );
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM metadata_kv AS p
              WHERE p.scope IN ('agent_capture_pending', 'agent_capture_quarantine')
                AND p.target = ? AND p.key = ?
                AND NOT EXISTS (SELECT 1 FROM agent_checkpoint AS c
                  WHERE c.checkpoint_id = p.key)
              LIMIT 1",
            [repo_id.into(), checkpoint.into()],
        ))
        .await
        .context("cannot inspect retained capture recovery evidence; run `libra agent doctor`")?
        .is_some())
}

pub(crate) async fn has_pending_header_for_checkpoint<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    checkpoint: &str,
) -> Result<bool> {
    ensure!(
        uuid::Uuid::parse_str(checkpoint).is_ok_and(|id| id.to_string() == checkpoint),
        "invalid capture artifact cleanup identity; run `libra agent doctor`"
    );
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM metadata_kv WHERE scope = 'agent_capture_pending'
              AND target = ? AND key = ?
              AND CASE WHEN value_type = 'text' AND typeof(value) = 'text'
                    AND length(CAST(value AS BLOB)) <= 8192
                    THEN CASE WHEN json_valid(value)
                               AND json_type(value, '$.manual_attempted') = 'true'
                              THEN 0 ELSE 1 END
                    ELSE 1 END = 1
              AND NOT EXISTS (SELECT 1 FROM metadata_kv AS q
                WHERE q.scope = 'agent_capture_quarantine'
                  AND q.target = metadata_kv.target AND q.key = metadata_kv.key)
              LIMIT 1",
            [repo_id.into(), checkpoint.into()],
        ))
        .await
        .context("cannot inspect pending capture recovery header; run `libra agent doctor`")?
        .is_some())
}

pub(crate) async fn has_manual_attempted_header_for_checkpoint<C: ConnectionTrait>(
    conn: &C,
    repo_id: &str,
    checkpoint: &str,
) -> Result<bool> {
    ensure!(
        uuid::Uuid::parse_str(checkpoint).is_ok_and(|id| id.to_string() == checkpoint),
        "invalid capture artifact cleanup identity; run `libra agent doctor`"
    );
    Ok(conn
        .query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT 1 FROM metadata_kv WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
              AND target = ? AND key = ?
              AND CASE WHEN value_type = 'text' AND typeof(value) = 'text'
                    AND length(CAST(value AS BLOB)) <= 8192
                    THEN CASE WHEN json_valid(value)
                               AND json_type(value, '$.manual_attempted') = 'true'
                              THEN 1 ELSE 0 END
                    ELSE 0 END = 1 LIMIT 1",
            [repo_id.into(), checkpoint.into()],
        ))
        .await
        .context("cannot inspect capture recovery attempt state; run `libra agent doctor`")?
        .is_some())
}

/// Artifact-only policy routing for a catalog-fenced receipt. Even malformed
/// evidence is retained: no decoding/MAC/source/key access or receipt mutation.
/// A namespace collision preserves both rows rather than overwriting either.
pub(crate) async fn quarantine_checkpoint_if_present(
    txn: &DatabaseTransaction,
    repo_id: &str,
    checkpoint: &str,
) -> Result<bool> {
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .context("cannot lock capture recovery quarantine; run `libra agent doctor`")?;
    if !has_header_for_checkpoint(txn, repo_id, checkpoint).await? {
        return Ok(false);
    }
    validate_queue_repo(txn, repo_id).await?;
    txn.execute_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "UPDATE metadata_kv SET scope = 'agent_capture_quarantine'
         WHERE scope = 'agent_capture_pending' AND target = ? AND key = ?
           AND NOT EXISTS (SELECT 1 FROM metadata_kv WHERE scope = 'agent_capture_quarantine'
             AND target = ? AND key = ?)",
        [
            repo_id.into(),
            checkpoint.into(),
            repo_id.into(),
            checkpoint.into(),
        ],
    ))
    .await
    .context("cannot retain exhausted capture artifact; run `libra agent doctor`")?;
    Ok(true)
}

/// Delete only headers attributed to caller-proven local owner aliases.
/// Keep chunks if any unknown/foreign same-key header survives. The returned
/// flag reports retained evidence, never broadens deletion authorization.
pub(crate) async fn remove_artifact(
    txn: &DatabaseTransaction,
    repo_id: &str,
    checkpoint: &str,
    owned_aliases: &[String],
) -> Result<bool> {
    ensure!(
        uuid::Uuid::parse_str(checkpoint).is_ok_and(|id| id.to_string() == checkpoint),
        "invalid capture artifact cleanup identity; run `libra agent doctor`"
    );
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .context("cannot lock capture recovery cleanup; run `libra agent doctor`")?;
    let rows = txn.query_all_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT scope, CASE WHEN typeof(value) = 'text' AND length(CAST(value AS BLOB)) <= 8192 THEN CAST(value AS BLOB) ELSE NULL END AS bounded_value
         FROM metadata_kv WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
           AND target = ? AND key = ? LIMIT 2",
        [repo_id.into(), checkpoint.into()],
    )).await.context("cannot read capture cleanup ownership; run `libra agent doctor`")?;
    let mut aliases = std::collections::HashSet::new();
    let mut retained = false;
    for row in rows {
        let text: Option<Vec<u8>> = row
            .try_get_by("bounded_value")
            .context("cannot decode capture cleanup ownership; run `libra agent doctor`")?;
        let alias = text
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .and_then(|text| pending_identity::header_alias_attribution(text, checkpoint));
        match alias {
            Some(alias) if owned_aliases.contains(&alias) => {
                let scope: String = row
                    .try_get_by("scope")
                    .context("cannot decode capture cleanup namespace; run `libra agent doctor`")?;
                let scope = match scope.as_str() {
                    "agent_capture_pending" => MetadataScope::AgentCapturePending,
                    "agent_capture_quarantine" => MetadataScope::AgentCaptureQuarantine,
                    _ => {
                        anyhow::bail!("invalid capture cleanup namespace; run `libra agent doctor`")
                    }
                };
                MetadataKv::unset_with_conn(txn, scope, repo_id, checkpoint).await?;
                aliases.insert(alias);
            }
            _ => retained = true,
        }
    }
    let range = chunk_range(checkpoint);
    if !retained && !aliases.is_empty() {
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "DELETE FROM metadata_kv WHERE scope = ? AND target = ? AND key >= ? AND key < ?",
            [
                MetadataScope::AgentCapturePendingChunk.as_str().into(),
                repo_id.into(),
                range.0.into(),
                range.1.into(),
            ],
        ))
        .await
        .context("cannot remove completed capture recovery chunks; run `libra agent doctor`")?;
    } else if aliases.is_empty() {
        // A missing header does not establish ownership of orphaned chunks.
        retained |= txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 FROM metadata_kv WHERE scope = 'agent_capture_pending_chunk'
              AND target = ? AND key >= ? AND key < ? LIMIT 1",
                [repo_id.into(), range.0.into(), range.1.into()],
            ))
            .await
            .context("cannot inspect retained capture chunks; run `libra agent doctor`")?
            .is_some();
    }
    for alias in aliases {
        pending_identity::remove_alias_if_unreferenced(txn, repo_id, &alias).await?;
    }
    Ok(retained)
}

/// Erasure is called in the existing tombstone-fenced catalog transaction.
/// Local association ownership, not MAC/receipt/source availability, grants
/// deletion. Unattributable evidence stays private and consumes capacity.
/// The return value requests a content-free lost-capacity diagnostic AFTER
/// the caller commits; it never expands the public erasure outcome schema.
pub(crate) async fn erase_session_artifacts(
    txn: &DatabaseTransaction,
    session_id: &str,
) -> Result<bool> {
    txn.execute_unprepared("UPDATE metadata_kv SET updated_at = updated_at WHERE 0")
        .await
        .context("cannot lock private capture erasure; run `libra agent doctor`")?;
    // Legacy repositories without any artifact need no new repo identity.
    let any = txn.query_one_raw(Statement::from_string(txn.get_database_backend(),
        "SELECT 1 FROM metadata_kv WHERE scope IN ('agent_capture_session_alias',
         'agent_capture_pending', 'agent_capture_quarantine', 'agent_capture_pending_chunk') LIMIT 1".to_string(),
    )).await.context("cannot inspect private capture erasure; run `libra agent doctor`")?;
    if any.is_none() {
        return Ok(false);
    }
    let repo = crate::internal::workspace::RepoIdentity::resolve(txn)
        .await
        .context(
            "cannot establish private capture erasure ownership; restore repository configuration",
        )?;
    let scope = CaptureScope {
        repo_id: repo.as_str().into(),
        worktree_id: String::new(),
        workspace_id: None,
        workspace_fence: None,
    };
    // Inspect only the alias owner's bounded codec. Unknown incarnation is
    // not deletion authority for a known association from any incarnation.
    let legacy = pending_identity::aliases_for_erasure(txn, &scope, session_id, None).await?;
    let known_association = !legacy.aliases().is_empty() || legacy.has_other_incarnation();
    let ownership_remedy = "capture erasure cannot establish the session incarnation; restore consistent session/alias catalog metadata before retrying";
    let row = txn.query_one_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT CASE WHEN length(CAST(metadata_json AS BLOB)) <= 1048576 THEN metadata_json ELSE NULL END AS bounded_value
         FROM agent_session WHERE session_id = ? LIMIT 1",
        [session_id.into()],
    )).await.context("cannot read erased session incarnation; run `libra agent doctor`")?;
    let Some(row) = row else {
        ensure!(!known_association, ownership_remedy);
        return Ok(legacy.has_unassigned());
    };
    let metadata: Option<String> = row
        .try_get_by("bounded_value")
        .context("cannot decode erased session incarnation; run `libra agent doctor`")?;
    use crate::internal::ai::observed_agents::coverage::CanonValue;
    let Some(CanonValue::Object(metadata)) = metadata.as_deref().and_then(|text| {
        crate::internal::ai::observed_agents::parse_canon_value(text.as_bytes()).ok()
    }) else {
        ensure!(!known_association, ownership_remedy);
        return Ok(legacy.has_unassigned());
    };
    let incarnation = match metadata.get("capture_incarnation") {
        None | Some(CanonValue::Null) => None,
        Some(CanonValue::Str(value)) if value.len() == 32 && is_lower_hex(value) => {
            Some(value.as_str())
        }
        _ => {
            ensure!(!known_association, ownership_remedy);
            return Ok(legacy.has_unassigned());
        }
    };
    let owned = pending_identity::aliases_for_erasure(txn, &scope, session_id, incarnation).await?;
    ensure!(!owned.has_other_incarnation(), ownership_remedy);
    let rows = txn.query_all_raw(Statement::from_sql_and_values(
        txn.get_database_backend(),
        "SELECT CASE WHEN typeof(key) = 'text' AND length(CAST(key AS BLOB)) <= 36 THEN CAST(key AS BLOB) ELSE NULL END AS bounded_key,
           CASE WHEN typeof(value) = 'text' AND length(CAST(value AS BLOB)) <= 8192 THEN CAST(value AS BLOB) ELSE NULL END AS bounded_value
         FROM metadata_kv WHERE scope IN ('agent_capture_pending', 'agent_capture_quarantine')
           AND target = ? ORDER BY key LIMIT 17",
        [scope.repo_id.clone().into()],
    )).await.context("cannot inspect capture artifact deletion ownership; run `libra agent doctor`")?;
    let mut unassigned = owned.has_unassigned() || rows.len() > MAX_ARTIFACTS;
    for row in rows {
        let checkpoint: Option<Vec<u8>> = row
            .try_get_by("bounded_key")
            .context("cannot decode capture deletion ownership; run `libra agent doctor`")?;
        let text: Option<Vec<u8>> = row
            .try_get_by("bounded_value")
            .context("cannot decode capture deletion ownership; run `libra agent doctor`")?;
        let checkpoint = checkpoint
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        let text = text
            .as_deref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok());
        let attribution = checkpoint
            .zip(text)
            .and_then(|(key, text)| pending_identity::header_alias_attribution(text, key));
        match (checkpoint, attribution) {
            (Some(checkpoint), Some(alias)) if owned.aliases().contains(&alias) => {
                unassigned |=
                    remove_artifact(txn, &scope.repo_id, checkpoint, owned.aliases()).await?;
            }
            (_, None) => unassigned = true,
            // Foreign evidence is retained, but a corrupt or orphaned header
            // keeps blocking persists and destructive GC: report lost capacity.
            (_, Some(alias)) => {
                unassigned |= text.is_none_or(|text| PendingHeader::decode(text).is_err())
                    || !alias_registered(txn, &scope.repo_id, &alias).await?;
            }
        }
    }
    pending_identity::erase_attributed_aliases(txn, &owned).await?;
    Ok(unassigned)
}

/// One indexed existence probe; the association body is never hydrated.
async fn alias_registered(txn: &DatabaseTransaction, repo_id: &str, alias: &str) -> Result<bool> {
    Ok(txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM metadata_kv WHERE scope = 'agent_capture_session_alias'
              AND target = ? AND key = ? LIMIT 1",
            [repo_id.into(), alias.into()],
        ))
        .await
        .context("cannot inspect capture deletion ownership; run `libra agent doctor`")?
        .is_some())
}

#[cfg(test)]
pub(crate) mod tests {
    #[cfg(unix)]
    use std::{path::PathBuf, time::Duration};

    #[cfg(unix)]
    use sea_orm::DatabaseConnection;
    #[cfg(unix)]
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;
    #[cfg(unix)]
    use crate::utils::json::normalize_json_value;

    #[test]
    fn replay_deadline_guard_preserves_typed_catalog_error() {
        let error = check_deadline(Instant::now() - std::time::Duration::from_millis(1))
            .expect_err("elapsed deadline must fail closed");
        assert!(error.chain().any(|cause| {
            cause.downcast_ref::<CaptureCatalogError>()
                == Some(&CaptureCatalogError::DeadlineExceeded)
        }));
    }

    #[test]
    fn wrapped_capture_scope_database_failures_are_retryable() {
        let deadline = Instant::now() + std::time::Duration::from_secs(1);
        let repository =
            anyhow::Error::new(crate::internal::workspace::WorkspaceError::ReadFailed(
                "repository identity unavailable".to_owned(),
            ));
        let typed_repository =
            anyhow::Error::new(crate::internal::workspace::WorkspaceError::ReadFailed(
                "database unavailable".to_owned(),
            ));
        let workspace = anyhow::Error::new(crate::internal::workspace::WorkspaceError::ReadFailed(
            "workspace record unavailable".to_owned(),
        ));
        let expired_lease = anyhow::anyhow!(CaptureCatalogError::WorkspaceLeaseRejected);
        let ownership_conflict =
            anyhow::anyhow!("capture catalog scope or workspace lease is not writable");
        let catalog_database = anyhow::anyhow!(CaptureCatalogError::Database);
        let corrupt = anyhow::anyhow!("invalid capture envelope authentication tag");
        let transient_key_io = anyhow::Error::new(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "key unavailable",
        ));
        let permanent_key_mode =
            anyhow::anyhow!("repository-private agent capture key must have permissions 0600");
        assert!(retryable_load_failure(&repository, deadline));
        assert!(retryable_load_failure(&typed_repository, deadline));
        assert!(retryable_load_failure(&workspace, deadline));
        assert!(retryable_load_failure(&expired_lease, deadline));
        assert!(retryable_load_failure(&catalog_database, deadline));
        assert!(retryable_load_failure(&transient_key_io, deadline));
        assert!(!retryable_load_failure(&ownership_conflict, deadline));
        assert!(!retryable_load_failure(&permanent_key_mode, deadline));
        assert!(!retryable_load_failure(&corrupt, deadline));
    }
    #[cfg(unix)]
    use crate::internal::{
        ai::{
            capture::{
                catalog::{CaptureCatalogPort, resolve_pending_session_context},
                key,
                pending_identity::PendingSessionAlias,
                snapshot::CaptureSnapshotService,
            },
            hooks::lifecycle::{
                CanonicalEventContext, LifecycleEvent, LifecycleEventKind, LifecycleIdentityScheme,
                lifecycle_event_canonical_json_with_identity,
            },
            observed_agents::{
                ExportAuthorized, TranscriptSource, coverage::Completeness, redaction::Redactor,
            },
        },
        config::ConfigKv,
        db,
    };

    const PK: &str = "claude__native-session";
    #[cfg(unix)]
    const NATIVE: &str = "native-session";

    #[cfg(unix)]
    struct Fixture {
        root: FixtureRoot,
        storage: PathBuf,
        db: DatabaseConnection,
        scope: CaptureScope,
    }

    /// A fresh tempdir, except in an isolated child process whose cwd-bound
    /// object GC loader needs the repository at the parent-owned child cwd.
    #[cfg(unix)]
    enum FixtureRoot {
        Temp(TempDir),
        ChildCwd(PathBuf),
    }

    #[cfg(unix)]
    impl FixtureRoot {
        fn path(&self) -> &Path {
            match self {
                Self::Temp(root) => root.path(),
                Self::ChildCwd(root) => root,
            }
        }
    }

    #[cfg(unix)]
    struct TerminalArtifactBinding {
        event_id: Uuid,
        checkpoint_id: String,
        action_key: String,
        receipt_key: String,
        marker_generation: String,
        parent_commit: Option<String>,
        reserved_revision: i64,
    }

    #[cfg(unix)]
    impl Fixture {
        async fn new() -> Self {
            Self::new_at(FixtureRoot::Temp(tempfile::tempdir().unwrap())).await
        }

        async fn new_at(root: FixtureRoot) -> Self {
            let storage = root.path().join(".libra");
            std::fs::create_dir_all(storage.join("objects")).unwrap();
            let db = db::create_database(storage.join("libra.db").to_str().unwrap())
                .await
                .unwrap();
            ConfigKv::set_with_conn(&db, "libra.repoid", "opaque-repository", false)
                .await
                .unwrap();
            key::load_capture_dedup_secret(&storage).unwrap();
            let scope = CaptureScope::resolve(&db, root.path()).await.unwrap();
            db.execute_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
                 state, working_dir, metadata_json, started_at, last_event_at,
                 sync_revision, repo_id, worktree_id, scope_state)
                 VALUES (?, 'claude_code', ?, 'active', ?, '{}', 1, 1, 1, ?, '', 'scoped')",
                [
                    PK.into(),
                    NATIVE.into(),
                    root.path().to_string_lossy().into_owned().into(),
                    scope.repo_id.clone().into(),
                ],
            ))
            .await
            .unwrap();
            db.execute_raw(Statement::from_sql_and_values(
                db.get_database_backend(),
                "INSERT INTO agent_coverage_claim (
                 session_id, logical_turn_key, coverage_schema_version, coverage_digest,
                 completeness, revision, state, owner, fence_token, source_channel, created_at, updated_at)
                 VALUES (?, 'turn-1', 1, ?, 'complete', 0, 'reserved_live', 'owned-live-reservation', 1, 'live', 1, 1)",
                [PK.into(), "d".repeat(64).into()],
            )).await.unwrap();
            Self {
                root,
                storage,
                db,
                scope,
            }
        }

        async fn identity(&self, alias: Option<&str>) -> PreparedPendingAlias {
            let existing = match alias {
                Some(alias) => Some(
                    pending_identity::lookup(&self.db, &self.scope.repo_id, alias)
                        .await
                        .unwrap()
                        .unwrap(),
                ),
                None => None,
            };
            let txn = db::begin_write_transaction(&self.db).await.unwrap();
            let context = if let Some(record) = &existing {
                record
                    .resolve(
                        &txn,
                        &self.scope,
                        &self.storage,
                        self.root.path(),
                        deadline(),
                    )
                    .await
                    .unwrap()
            } else {
                resolve_pending_session_context(&txn, &self.scope, PK)
                    .await
                    .unwrap()
            };
            txn.commit().await.unwrap();
            PendingSessionAlias::prepare(
                &self.db,
                &context,
                existing,
                &self.storage,
                self.root.path(),
                deadline(),
            )
            .await
            .unwrap()
        }

        async fn artifact(
            &self,
            alias: Option<&str>,
        ) -> (SealedPendingArtifact, CheckpointRedactedPayload) {
            self.artifact_with_terminal_binding(alias, None).await
        }

        async fn artifact_with_terminal_binding(
            &self,
            alias: Option<&str>,
            terminal: Option<TerminalArtifactBinding>,
        ) -> (SealedPendingArtifact, CheckpointRedactedPayload) {
            let identity = self.identity(alias).await;
            // Test-only trusted-export producer reads a disposable source
            // before sealing; replay must not retain or reopen this locator.
            let source = self.root.path().join("provider-source.jsonl");
            std::fs::write(&source, b"safe useful transcript AKIAABCDEFGHIJKLMNOP").unwrap();
            let bytes = std::fs::read(source).unwrap();
            let auth = ExportAuthorized::issue("claude_code", NATIVE, &bytes);
            let mut snapshot = CaptureSnapshotService::capture_authorized(
                TranscriptSource::Bytes { bytes, auth },
                "claude_code",
                NATIVE,
                Default::default(),
            );
            let source_mac = key::derive_snapshot_content_commitment_in_scope_until(
                &self.db,
                &self.scope,
                &self.storage,
                self.root.path(),
                &snapshot.redacted_digest_preimage().unwrap(),
                deadline(),
            )
            .await
            .unwrap();
            assert!(snapshot.bind_source_commitment(source_mac.clone()));
            let mut binding = binding(
                identity.alias(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                &self.scope,
                source_mac,
            );
            if let Some(terminal) = terminal {
                binding.event_id = terminal.event_id.to_string();
                binding.checkpoint_id = terminal.checkpoint_id;
                binding.action_key = terminal.action_key;
                binding.receipt_key = terminal.receipt_key;
                binding.marker_generation = terminal.marker_generation;
                binding.parent_commit = terminal.parent_commit;
                binding.parent_unborn = binding.parent_commit.is_none();
                binding.reserved_revision = terminal.reserved_revision;
                // This artifact represents a real catalog finalizer created
                // without a deadline; the generic fixture defaults to an
                // expired deadline for one-shot repair tests.
                binding.original_deadline_millis = None;
            }
            let report = serde_json::to_value(snapshot.redaction_report()).unwrap();
            let metadata = serde_json::json!({
                "schema_version":2, "checkpoint_id":binding.checkpoint_id, "session_id":PK,
                "provider_session_id":NATIVE, "working_dir":identity.context().working_dir(),
                "agent_kind":"claude_code", "scope":"committed", "model":{"name":"test"},
                "created_at":1, "redaction_report":report, "transcript_snapshot":snapshot.safe_projection(),
                "extraction":{"edited_files":["src/test.rs"],"prompt":"useful content"}
            });
            let event = lifecycle_event_canonical_json_with_identity(
                &LifecycleEvent {
                    kind: LifecycleEventKind::SessionEnd,
                    session_id: NATIVE.into(),
                    session_ref: Some("excluded provider source locator".into()),
                    prompt: None,
                    model: None,
                    source: None,
                    tool_name: None,
                    tool_input: Some(
                        serde_json::json!({"cwd":"nested user data","session_id":"user data"}),
                    ),
                    tool_response: None,
                    assistant_message: None,
                    timestamp: chrono::DateTime::from_timestamp(1, 0).unwrap(),
                },
                &CanonicalEventContext {
                    agent_kind: "claude_code",
                    session_id: PK,
                    provider_session_id: NATIVE,
                    identity_scheme: LifecycleIdentityScheme::NativeReplayHmacV2,
                    provenance: serde_json::json!({"channel":"hook","hook_event_name":"SessionEnd"}),
                },
                Uuid::parse_str(&binding.event_id).unwrap(),
                false,
            );
            let mut events = serde_json::to_vec(&event).unwrap();
            events.push(b'\n');
            let payload = CheckpointRedactedPayload::from_snapshot(
                snapshot,
                redact(&serde_json::to_vec_pretty(&normalize_json_value(metadata)).unwrap()),
                redact(&events),
                redact(&serde_json::to_vec_pretty(&normalize_json_value(report)).unwrap()),
            )
            .unwrap();
            let coverage = LiveClaimCommitPlan {
                source_channel: "live",
                session_id: PK.into(),
                checkpoint_id: binding.checkpoint_id.clone(),
                owner: "owned-live-reservation".into(),
                parent_commit: binding.parent_commit.clone(),
                created_at: 1,
                now_ms: 1,
                claims: vec![ReservedTurnClaim {
                    logical_turn_key: "turn-1".into(),
                    coverage_digest: "d".repeat(64),
                    completeness: Completeness::Complete,
                    fence_token: 1,
                    next_revision: 1,
                }],
                import_session: None,
                import_identity: None,
                capture_scope: Some(self.scope.clone()),
            };
            let artifact = SealedPendingArtifact::seal(
                &self.db,
                &self.storage,
                self.root.path(),
                PendingSealRequest {
                    binding,
                    identity,
                    payload: &payload,
                    coverage: &coverage,
                    reasoning: None,
                },
                deadline(),
            )
            .await
            .unwrap();
            (artifact, payload)
        }

        async fn persist(&self, artifact: &SealedPendingArtifact) {
            let txn = db::begin_write_transaction(&self.db).await.unwrap();
            artifact.persist(&txn, deadline()).await.unwrap();
            txn.commit().await.unwrap();
        }

        async fn count(&self, scope: MetadataScope) -> usize {
            MetadataKv::list_bounded_with_conn(
                &self.db,
                &[scope],
                Some(&self.scope.repo_id),
                None,
                129,
                CHUNK_TEXT_BYTES,
            )
            .await
            .unwrap()
            .len()
        }
    }

    #[cfg(unix)]
    struct VersionedNativeSealFixture {
        f: Fixture,
        binding: PendingBinding,
        identity: PreparedPendingAlias,
        payload: CheckpointRedactedPayload,
        ordinary_payload: CheckpointRedactedPayload,
        companion: NativeVersionedReasoningClaimCommitPlan,
    }

    #[cfg(unix)]
    fn native_pending_payload_from_ordinary(
        ordinary: &CheckpointRedactedPayload,
        fields: Vec<(u64, Vec<u8>)>,
    ) -> CheckpointRedactedPayload {
        use crate::internal::ai::observed_agents::{
            RedactedBytes,
            reasoning::{ReasoningSourceDisposition, synthetic_native_batch_for_turns},
        };

        CheckpointRedactedPayload::from_redacted_capture_with_reasoning(
            RedactedBytes::new_unchecked(ordinary.transcript().bytes().to_vec()),
            Some(ordinary.snapshot().clone()),
            RedactedBytes::new_unchecked(ordinary.metadata_json().bytes().to_vec()),
            RedactedBytes::new_unchecked(ordinary.lifecycle_events_jsonl().bytes().to_vec()),
            RedactedBytes::new_unchecked(ordinary.redaction_report_json().bytes().to_vec()),
            ReasoningSourceDisposition::NativeProven(synthetic_native_batch_for_turns(
                PK, NATIVE, fields,
            )),
        )
        .expect("construct a complete redacted synthetic native payload")
    }

    #[cfg(unix)]
    async fn versioned_companion_for_turns(
        f: &Fixture,
        payload: &CheckpointRedactedPayload,
        binding: &PendingBinding,
        turns: &[crate::internal::ai::observed_agents::NormalizedTurn],
    ) -> NativeVersionedReasoningClaimCommitPlan {
        use crate::internal::ai::{
            capture::checkpoint::{
                ReasoningIdentityPreparation, prepare_native_versioned_reasoning_coverage_plan,
            },
            coverage_gate::{VersionedLiveClaimCommitPlan, VersionedReservedTurnClaim},
            observed_agents::coverage::{encode_coverage_turn_for_schema, select_coverage_schema},
        };

        let source = prepare_native_versioned_reasoning_coverage_plan(
            payload,
            ReasoningIdentityPreparation {
                conn: &f.db,
                scope: &f.scope,
                storage_path: &f.storage,
                authorized_root: f.root.path(),
                session_id: PK,
                provider: "claude_code",
                turns,
                deadline: deadline(),
            },
        )
        .await
        .expect("derive synthetic native identities from the sealed payload");
        let versioned = turns
            .iter()
            .map(|turn| {
                let version = select_coverage_schema(turn);
                let digest = encode_coverage_turn_for_schema(version.as_i64(), turn)
                    .expect("encode synthetic normalized turn")
                    .digest_hex;
                VersionedReservedTurnClaim::new(
                    version.as_i64(),
                    ReservedTurnClaim {
                        logical_turn_key: turn.logical_turn_key.clone(),
                        coverage_digest: digest,
                        completeness: turn.completeness,
                        fence_token: 1,
                        next_revision: 1,
                    },
                )
                .expect("use a supported coverage version")
            })
            .collect::<Vec<_>>();
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: PK.to_owned(),
            checkpoint_id: binding.checkpoint_id.clone(),
            owner: "owned-live-reservation".to_owned(),
            parent_commit: binding.parent_commit.clone(),
            created_at: 1,
            now_ms: 1,
            claims: versioned.iter().map(|claim| claim.claim.clone()).collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(f.scope.clone()),
        };
        let plan = VersionedLiveClaimCommitPlan::new(ordinary, &versioned)
            .expect("bind exact versioned synthetic claims");
        NativeVersionedReasoningClaimCommitPlan::new(plan, &source)
            .expect("seal the source-derived synthetic companion")
    }

    #[cfg(unix)]
    async fn versioned_native_seal_fixture() -> VersionedNativeSealFixture {
        use crate::internal::ai::coverage_gate::tests::cov2_native_v2_turn;

        let f = Fixture::new().await;
        let (ordinary, ordinary_payload) = f.artifact(None).await;
        let binding = ordinary.header.binding;
        let identity = ordinary.identity;
        let payload = native_pending_payload_from_ordinary(
            &ordinary_payload,
            vec![(0, b"synthetic native pending-v2 proof".to_vec())],
        );
        let turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "synthetic visible typed reasoning",
            Completeness::Complete,
        )];
        let companion = versioned_companion_for_turns(&f, &payload, &binding, &turns).await;
        VersionedNativeSealFixture {
            f,
            binding,
            identity,
            payload,
            ordinary_payload,
            companion,
        }
    }

    /// Compare the actual repository rows and ref tip without exposing any
    /// synthetic source bytes in failed assertions.
    #[cfg(unix)]
    async fn versioned_seal_state(f: &Fixture) -> (Vec<String>, Vec<String>, i64, Option<String>) {
        use sha2::{Digest, Sha256};

        let metadata =
            f.db.query_all_raw(Statement::from_string(
                f.db.get_database_backend(),
                "SELECT json_object('scope',scope,'target',target,'key',key,'value',hex(value), \
                 'type',value_type,'created',created_at,'updated',updated_at) AS row \
                 FROM metadata_kv ORDER BY scope,target,key"
                    .to_owned(),
            ))
            .await
            .expect("read pending, alias, marker and receipt rows")
            .into_iter()
            .map(|row| {
                let value: String = row.try_get_by("row").expect("read metadata row");
                hex::encode(Sha256::digest(value.as_bytes()))
            })
            .collect();
        let claims = f
            .db
            .query_all_raw(Statement::from_string(
                f.db.get_database_backend(),
                "SELECT json_object('session',session_id,'key',logical_turn_key, \
                 'version',coverage_schema_version,'digest',coverage_digest, \
                 'completeness',completeness,'revision',revision,'state',state, \
                 'attempt',attempt_checkpoint_id,'owner',owner,'lease',lease_expires_at, \
                 'fence',fence_token,'checkpoint',checkpoint_id,'traces',traces_commit, \
                 'channel',source_channel,'created',created_at,'updated',updated_at) AS row \
                 FROM agent_coverage_claim ORDER BY session_id,coverage_schema_version,logical_turn_key"
                    .to_owned(),
            ))
            .await
            .expect("read full coverage lease rows")
            .into_iter()
            .map(|row| {
                let value: String = row.try_get_by("row").expect("read coverage row");
                hex::encode(Sha256::digest(value.as_bytes()))
            })
            .collect();
        let checkpoints =
            f.db.query_one_raw(Statement::from_string(
                f.db.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_checkpoint".to_owned(),
            ))
            .await
            .expect("count catalog checkpoints")
            .expect("receive checkpoint count")
            .try_get_by("n")
            .expect("decode checkpoint count");
        let traces_ref = std::fs::read_to_string(f.storage.join("refs/libra/traces")).ok();
        (metadata, claims, checkpoints, traces_ref)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_versioned_native_seal_consumes_checked_borrow() {
        use crate::internal::ai::{
            coverage_gate::tests::{cov2_native_v2_turn, turn},
            observed_agents::{RedactedBytes, coverage::CoverageSchemaVersion},
        };

        let fixture = versioned_native_seal_fixture().await;
        let VersionedNativeSealFixture {
            f,
            binding,
            identity,
            payload,
            companion,
            ..
        } = fixture;
        let before = versioned_seal_state(&f).await;
        let artifact = SealedVersionedPendingArtifact::seal(
            &f.db,
            &f.storage,
            f.root.path(),
            VersionedPendingSealRequest {
                binding: binding.clone(),
                identity,
                payload: &payload,
                companion: &companion,
            },
            deadline(),
        )
        .await
        .expect("the same sealed source and companion must seal in memory");
        assert_eq!(versioned_seal_state(&f).await, before);
        let envelope: VersionedPendingEnvelope = serde_json::from_slice(artifact.bytes()).unwrap();
        envelope.validate().unwrap();
        let (version, claim) = companion.versioned().claims().next().unwrap();
        let expected = companion
            .reasoning()
            .turn(version, &claim.logical_turn_key)
            .unwrap();
        let association = &envelope.payload.identity_commit_associations[0];
        assert_eq!(association.coverage_schema_version, version.as_i64());
        assert_eq!(association.logical_turn_key, claim.logical_turn_key);
        assert_eq!(association.slot, expected.slot());
        assert_eq!(association.descriptor_mac, expected.descriptor_mac());
        assert_eq!(
            envelope.coverage.claims[0].coverage_schema_version,
            version.as_i64()
        );
        assert_eq!(
            envelope.coverage.claims[0].logical_turn_key,
            claim.logical_turn_key
        );
        assert!(envelope.payload.reasoning_source.is_some());

        // A locally re-authenticated wire edit still cannot swap identity
        // descriptor domains or the corresponding versioned claim tuple.
        let mut wrong_domain: serde_json::Value = serde_json::from_slice(artifact.bytes()).unwrap();
        wrong_domain["payload"]["identity_commit_associations"][0]["descriptor_mac"] =
            serde_json::json!(format!("reasoning-identity/hmac-v1/{}", "a".repeat(64)));
        let wrong_domain: VersionedPendingEnvelope = serde_json::from_value(wrong_domain).unwrap();
        assert!(wrong_domain.validate().is_err());
        let mut wrong_version: serde_json::Value =
            serde_json::from_slice(artifact.bytes()).unwrap();
        wrong_version["coverage"]["claims"][0]["coverage_schema_version"] =
            serde_json::json!(CoverageSchemaVersion::V1.as_i64());
        let wrong_version: VersionedPendingEnvelope =
            serde_json::from_value(wrong_version).unwrap();
        assert!(wrong_version.validate().is_err());

        let changed = versioned_native_seal_fixture().await;
        let other_source = native_pending_payload_from_ordinary(
            &changed.ordinary_payload,
            vec![(0, b"different synthetic native source".to_vec())],
        );
        assert!(
            SealedVersionedPendingArtifact::seal(
                &changed.f.db,
                &changed.f.storage,
                changed.f.root.path(),
                VersionedPendingSealRequest {
                    binding: changed.binding,
                    identity: changed.identity,
                    payload: &other_source,
                    companion: &changed.companion,
                },
                deadline(),
            )
            .await
            .is_err(),
            "another NativeProven payload cannot borrow the companion"
        );

        let ordinary = versioned_native_seal_fixture().await;
        assert!(
            SealedVersionedPendingArtifact::seal(
                &ordinary.f.db,
                &ordinary.f.storage,
                ordinary.f.root.path(),
                VersionedPendingSealRequest {
                    binding: ordinary.binding,
                    identity: ordinary.identity,
                    payload: &ordinary.ordinary_payload,
                    companion: &ordinary.companion,
                },
                deadline(),
            )
            .await
            .is_err(),
            "ordinary content cannot enter the native sealer"
        );

        let partial = versioned_native_seal_fixture().await;
        let partial_payload = CheckpointRedactedPayload::from_redacted_capture(
            RedactedBytes::new_unchecked(partial.ordinary_payload.transcript().bytes().to_vec()),
            None,
            RedactedBytes::new_unchecked(partial.ordinary_payload.metadata_json().bytes().to_vec()),
            RedactedBytes::new_unchecked(
                partial
                    .ordinary_payload
                    .lifecycle_events_jsonl()
                    .bytes()
                    .to_vec(),
            ),
            RedactedBytes::new_unchecked(
                partial
                    .ordinary_payload
                    .redaction_report_json()
                    .bytes()
                    .to_vec(),
            ),
        )
        .unwrap();
        assert!(
            SealedVersionedPendingArtifact::seal(
                &partial.f.db,
                &partial.f.storage,
                partial.f.root.path(),
                VersionedPendingSealRequest {
                    binding: partial.binding,
                    identity: partial.identity,
                    payload: &partial_payload,
                    companion: &partial.companion,
                },
                deadline(),
            )
            .await
            .is_err(),
            "partial source cannot become a native pending artifact"
        );

        let shared = versioned_native_seal_fixture().await;
        let same_key_turns = [
            turn("synthetic-turn-0", "ordinary v1", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-0", "visible v2", Completeness::Complete),
        ];
        let same_key = versioned_companion_for_turns(
            &shared.f,
            &shared.payload,
            &shared.binding,
            &same_key_turns,
        )
        .await;
        assert!(
            SealedVersionedPendingArtifact::seal(
                &shared.f.db,
                &shared.f.storage,
                shared.f.root.path(),
                VersionedPendingSealRequest {
                    binding: shared.binding,
                    identity: shared.identity,
                    payload: &shared.payload,
                    companion: &same_key,
                },
                deadline(),
            )
            .await
            .is_err(),
            "native cross-version bare-key aliases remain closed until OG-29"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_versioned_native_seal_binding_and_zero_write() {
        use crate::internal::ai::coverage_gate::{
            VersionedLiveClaimCommitPlan, VersionedReservedTurnClaim, tests::cov2_native_v2_turn,
        };

        let valid = versioned_native_seal_fixture().await;
        let before = versioned_seal_state(&valid.f).await;
        let sealed = SealedVersionedPendingArtifact::seal(
            &valid.f.db,
            &valid.f.storage,
            valid.f.root.path(),
            VersionedPendingSealRequest {
                binding: valid.binding,
                identity: valid.identity,
                payload: &valid.payload,
                companion: &valid.companion,
            },
            deadline(),
        )
        .await
        .expect("a valid source-bound request seals only in RAM");
        assert!(!sealed.bytes().is_empty());
        assert_eq!(versioned_seal_state(&valid.f).await, before);

        for mutation in [
            "source_commitment",
            "session_alias",
            "scope",
            "checkpoint",
            "parent",
            "provider",
            "companion_checkpoint",
        ] {
            let mut fixture = versioned_native_seal_fixture().await;
            let mut alternate_companion = None;
            match mutation {
                "source_commitment" => {
                    fixture.binding.source_commitment =
                        format!("source/hmac-v2/{}", "f".repeat(64));
                }
                "session_alias" => fixture.binding.session_id = Uuid::new_v4().to_string(),
                "scope" => fixture.binding.scope.worktree_id = "wrong-worktree".to_owned(),
                "checkpoint" => fixture.binding.checkpoint_id = Uuid::new_v4().to_string(),
                "parent" => {
                    fixture.binding.parent_commit = None;
                    fixture.binding.parent_unborn = true;
                }
                "provider" => {
                    fixture
                        .f
                        .db
                        .execute_raw(Statement::from_sql_and_values(
                            fixture.f.db.get_database_backend(),
                            "UPDATE agent_session SET agent_kind = 'codex' WHERE session_id = ?",
                            [PK.into()],
                        ))
                        .await
                        .unwrap();
                    fixture.identity = fixture.f.identity(None).await;
                    fixture.binding.session_id = fixture.identity.alias().to_owned();
                }
                "companion_checkpoint" => {
                    let mut wrong = fixture.binding.clone();
                    wrong.checkpoint_id = Uuid::new_v4().to_string();
                    let turns = [cov2_native_v2_turn(
                        "synthetic-turn-0",
                        "synthetic visible typed reasoning",
                        Completeness::Complete,
                    )];
                    alternate_companion = Some(
                        versioned_companion_for_turns(&fixture.f, &fixture.payload, &wrong, &turns)
                            .await,
                    );
                }
                _ => unreachable!("fixed mutation matrix"),
            }
            let before = versioned_seal_state(&fixture.f).await;
            let error = SealedVersionedPendingArtifact::seal(
                &fixture.f.db,
                &fixture.f.storage,
                fixture.f.root.path(),
                VersionedPendingSealRequest {
                    binding: fixture.binding,
                    identity: fixture.identity,
                    payload: &fixture.payload,
                    companion: alternate_companion.as_ref().unwrap_or(&fixture.companion),
                },
                deadline(),
            )
            .await;
            assert!(
                error.is_err(),
                "{mutation} must not produce a sealed artifact"
            );
            assert_eq!(
                versioned_seal_state(&fixture.f).await,
                before,
                "{mutation} must not alter artifact, lease, catalog or ref state"
            );
        }

        // A claim edited after source derivation cannot become a sealed
        // companion and therefore cannot reach the production seal entrance.
        let source_bound = versioned_native_seal_fixture().await;
        let mut changed = source_bound.companion.versioned().ordinary().clone();
        changed.claims[0].coverage_digest = "f".repeat(64);
        let sidecar = [VersionedReservedTurnClaim::new(2, changed.claims[0].clone()).unwrap()];
        let changed = VersionedLiveClaimCommitPlan::new(changed, &sidecar).unwrap();
        let before = versioned_seal_state(&source_bound.f).await;
        assert!(
            NativeVersionedReasoningClaimCommitPlan::new(
                changed,
                source_bound.companion.reasoning(),
            )
            .is_err()
        );
        assert_eq!(versioned_seal_state(&source_bound.f).await, before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_versioned_native_seal_authenticated_outer_roundtrip() {
        let fixture = versioned_native_seal_fixture().await;
        let VersionedNativeSealFixture {
            f,
            binding,
            identity,
            payload,
            companion,
            ..
        } = fixture;
        let artifact = SealedVersionedPendingArtifact::seal(
            &f.db,
            &f.storage,
            f.root.path(),
            VersionedPendingSealRequest {
                binding: binding.clone(),
                identity,
                payload: &payload,
                companion: &companion,
            },
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(artifact.header.version, VERSION);
        assert!(artifact.binding() == &binding);
        assert_eq!(artifact.header.envelope_bytes, artifact.bytes().len());
        assert_eq!(artifact.identity.alias(), binding.session_id);
        assert_eq!(
            artifact.coverage_owner,
            companion.versioned().ordinary().owner
        );
        assert_eq!(artifact.coverage_claims.len(), 1);
        PendingHeader::decode(&serde_json::to_string(&artifact.header).unwrap()).unwrap();
        let exact_mac = binding
            .scope
            .sign_pending_envelope_until(
                &f.db,
                &f.storage,
                f.root.path(),
                artifact.bytes(),
                deadline(),
            )
            .await
            .unwrap();
        assert_eq!(artifact.header.mac, exact_mac);
        let authenticated = AuthenticatedPendingEnvelope::verify(
            &f.db,
            &f.storage,
            f.root.path(),
            &binding.scope,
            artifact.bytes(),
            &artifact.header.mac,
            deadline(),
        )
        .await
        .unwrap();
        let decoded = decode_authenticated_versioned_envelope(&authenticated, &binding).unwrap();
        assert_eq!(decoded.version, VERSIONED_ENVELOPE_VERSION);
        assert!(decoded.binding == binding);
        assert_eq!(decoded.coverage.claims.len(), 1);
        assert_eq!(decoded.payload.identity_commit_associations.len(), 1);
        let claim = &decoded.coverage.claims[0];
        let association = &decoded.payload.identity_commit_associations[0];
        assert_eq!(
            claim.coverage_schema_version,
            association.coverage_schema_version
        );
        assert_eq!(claim.logical_turn_key, association.logical_turn_key);
        assert!(decoded.payload.reasoning_source.is_some());
        assert!(
            serde_json::to_value(&decoded.payload.redacted)
                .unwrap()
                .get("transcript")
                .is_some(),
            "authenticated v2 keeps the redacted ordinary projection"
        );

        let mut tampered = artifact.bytes().to_vec();
        tampered[10] ^= 1;
        assert!(
            AuthenticatedPendingEnvelope::verify(
                &f.db,
                &f.storage,
                f.root.path(),
                &binding.scope,
                &tampered,
                &artifact.header.mac,
                deadline(),
            )
            .await
            .is_err(),
            "a modified outer byte cannot receive an authentication witness"
        );
        // Re-entering the pure COV2-04 codec from the actual sealed fields
        // must reproduce exactly the MACed bytes, including tuple order.
        let rebuilt =
            encode_versioned_envelope(decoded.binding, decoded.payload, decoded.coverage).unwrap();
        assert_eq!(rebuilt, artifact.bytes());
    }

    #[cfg(unix)]
    async fn cov2_22_persisted_state(f: &Fixture) -> (Vec<(String, Vec<String>)>, Option<Vec<u8>>) {
        use sha2::{Digest, Sha256};

        let mut tables = Vec::new();
        for table in [
            "metadata_kv",
            "agent_coverage_claim",
            "agent_coverage_revision",
            "agent_checkpoint",
            "agent_session",
            "reference",
            "reflog",
        ] {
            let columns =
                f.db.query_all_raw(Statement::from_string(
                    f.db.get_database_backend(),
                    format!("PRAGMA table_info(\"{table}\")"),
                ))
                .await
                .unwrap();
            assert!(!columns.is_empty(), "snapshot table must exist");
            let expressions = columns
                .iter()
                .map(|column| {
                    let name: String = column.try_get_by("name").unwrap();
                    let quoted = format!("\"{}\"", name.replace('"', "\"\""));
                    format!("json_array(typeof({quoted}),hex(CAST({quoted} AS BLOB)))")
                })
                .collect::<Vec<_>>()
                .join(",");
            let rows =
                f.db.query_all_raw(Statement::from_string(
                    f.db.get_database_backend(),
                    format!(
                        "SELECT json_array({expressions}) AS row FROM \"{table}\" ORDER BY row"
                    ),
                ))
                .await
                .unwrap()
                .into_iter()
                .map(|row| {
                    let bytes: String = row.try_get_by("row").unwrap();
                    hex::encode(Sha256::digest(bytes.as_bytes()))
                })
                .collect();
            tables.push((table.to_owned(), rows));
        }
        (
            tables,
            std::fs::read(f.storage.join("refs/libra/traces")).ok(),
        )
    }

    #[cfg(unix)]
    async fn cov2_22_real_artifact() -> (Fixture, SealedVersionedPendingArtifact) {
        use crate::internal::ai::coverage_gate::{
            reserve_turn_claims_by_version_with_capture_scope,
            tests::{cov2_native_v2_turn, turn},
        };
        let f = Fixture::new().await;
        let (ordinary, ordinary_payload) = f.artifact(None).await;
        let binding = ordinary.header.binding;
        let identity = ordinary.identity;
        let payload = native_pending_payload_from_ordinary(
            &ordinary_payload,
            vec![
                (0, b"synthetic v1 native source".to_vec()),
                (1, b"synthetic v2 native source".to_vec()),
            ],
        );
        let first = turn(
            "synthetic-turn-0",
            "ordinary v1 content",
            Completeness::Complete,
        );
        let mut second = cov2_native_v2_turn(
            "synthetic-turn-1",
            "synthetic visible v2 reasoning",
            Completeness::Complete,
        );
        second.ordinal = 1;
        let turns = [first, second];
        let versions = [1, 2];
        let reservation = reserve_turn_claims_by_version_with_capture_scope(
            &f.db,
            &f.scope,
            PK,
            &turns,
            &versions,
            "owned-live-reservation",
            1,
            "live",
        )
        .await
        .expect("reserve actual versioned rows through the production seam");
        assert_eq!(reservation.reserved.len(), 2);
        let companion = versioned_companion_for_turns(&f, &payload, &binding, &turns).await;
        assert_eq!(
            reservation.reserved,
            companion
                .versioned()
                .claims()
                .map(|(coverage_schema_version, claim)| {
                    VersionedReservedTurnClaim {
                        coverage_schema_version,
                        claim: claim.clone(),
                    }
                })
                .collect::<Vec<_>>()
        );
        let artifact = SealedVersionedPendingArtifact::seal(
            &f.db,
            &f.storage,
            f.root.path(),
            VersionedPendingSealRequest {
                binding,
                identity,
                payload: &payload,
                companion: &companion,
            },
            deadline(),
        )
        .await
        .expect("seal one real production artifact from the same source");
        (f, artifact)
    }

    #[cfg(unix)]
    async fn cov2_22_commit_artifact(f: &Fixture, artifact: &SealedVersionedPendingArtifact) {
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        artifact.persist(&txn, deadline()).await.unwrap();
        txn.commit().await.unwrap();
    }

    #[cfg(unix)]
    async fn cov2_22_scope_count(
        conn: &impl ConnectionTrait,
        f: &Fixture,
        scope: MetadataScope,
    ) -> i64 {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM metadata_kv WHERE target = ? AND scope = ?",
                [f.scope.repo_id.clone().into(), scope.as_str().into()],
            ))
            .await
            .unwrap()
            .unwrap();
        row.try_get_by("n").unwrap()
    }

    #[cfg(unix)]
    async fn cov2_22_assert_no_ack(conn: &impl ConnectionTrait) {
        for table in [
            "agent_checkpoint",
            "agent_coverage_revision",
            "reference",
            "reflog",
        ] {
            let row = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    format!("SELECT COUNT(*) AS n FROM \"{table}\""),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                row.try_get_by::<i64, _>("n").unwrap(),
                0,
                "no publication in {table}"
            );
        }
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [PK.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by::<String, _>("metadata_json").unwrap(), "{}");
    }

    #[cfg(unix)]
    async fn cov2_22_assert_retained(conn: &impl ConnectionTrait, count: i64) {
        let row = conn.query_one_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE session_id = ? AND lease_expires_at = ?",
            [PK.into(), crate::internal::ai::coverage_gate::ARTIFACT_RETAINED_LEASE_MS.into()],
        )).await.unwrap().unwrap();
        assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), count);
        if count == 2 {
            let rows = conn.query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT coverage_schema_version, logical_turn_key FROM agent_coverage_claim
                 WHERE session_id = ? AND lease_expires_at = ? ORDER BY coverage_schema_version, logical_turn_key LIMIT 3",
                [PK.into(), crate::internal::ai::coverage_gate::ARTIFACT_RETAINED_LEASE_MS.into()],
            )).await.unwrap();
            let actual = rows
                .iter()
                .map(|row| {
                    (
                        row.try_get_by::<i64, _>("coverage_schema_version").unwrap(),
                        row.try_get_by::<String, _>("logical_turn_key").unwrap(),
                    )
                })
                .collect::<Vec<_>>();
            assert_eq!(
                actual,
                [
                    (1, "synthetic-turn-0".to_owned()),
                    (2, "synthetic-turn-1".to_owned())
                ]
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_real_v2_persist_and_frozen_old_reader_reject() {
        let (f, artifact) = cov2_22_real_artifact().await;
        cov2_22_commit_artifact(&f, &artifact).await;
        cov2_22_assert_retained(&f.db, 2).await;
        cov2_22_assert_no_ack(&f.db).await;
        assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 1);
        assert_eq!(f.count(MetadataScope::AgentCaptureSessionAlias).await, 1);
        assert_eq!(
            load_chunks(&f.db, &artifact.header).await.unwrap(),
            artifact.bytes()
        );
        let header =
            header_for_checkpoint(&f.db, &f.scope.repo_id, &artifact.binding().checkpoint_id)
                .await
                .unwrap()
                .unwrap()
                .0;
        assert_eq!(header.mac, artifact.header.mac);
        let identity = f.identity(Some(artifact.identity.alias())).await;
        let before = cov2_22_persisted_state(&f).await;
        let committed = load_chunks(&f.db, &header).await.unwrap();
        AuthenticatedPendingEnvelope::verify(
            &f.db,
            &f.storage,
            f.root.path(),
            &f.scope,
            &committed,
            &header.mac,
            deadline(),
        )
        .await
        .expect("the original committed v2 MAC authenticates before old-reader rejection");
        let error = frozen_pre_v2::load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            artifact.binding(),
            &header,
            deadline(),
        )
        .await
        .err()
        .expect("compiled pre-v2 reader must reject actual committed outer-v2");
        assert!(
            error
                .to_string()
                .contains("invalid authenticated capture recovery envelope")
        );
        assert_eq!(cov2_22_persisted_state(&f).await, before);
        assert_eq!(load_chunks(&f.db, &header).await.unwrap(), artifact.bytes());
        cov2_22_assert_retained(&f.db, 2).await;
        // Idempotent delivery must not reset quarantine/manual policy or
        // rewrite the original chunks/alias/leases after their first commit.
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        quarantine_header(&txn, &header).await.unwrap();
        let manual = claim_manual_attempt(&txn, &header).await.unwrap();
        assert!(manual.manual_attempted);
        txn.commit().await.unwrap();
        let retained = cov2_22_persisted_state(&f).await;
        cov2_22_commit_artifact(&f, &artifact).await;
        assert_eq!(cov2_22_persisted_state(&f).await, retained);
        let (retained_header, namespace) =
            header_for_checkpoint(&f.db, &f.scope.repo_id, &artifact.binding().checkpoint_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(namespace, MetadataScope::AgentCaptureQuarantine);
        assert!(retained_header.manual_attempted);
        cov2_22_assert_no_ack(&f.db).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_real_v2_artifact_new_reader_roundtrip() {
        let (f, artifact) = cov2_22_real_artifact().await;
        cov2_22_commit_artifact(&f, &artifact).await;
        let identity = f.identity(Some(artifact.identity.alias())).await;
        let (committed_header, namespace) =
            header_for_checkpoint(&f.db, &f.scope.repo_id, &artifact.binding().checkpoint_id)
                .await
                .unwrap()
                .unwrap();
        assert_eq!(namespace, MetadataScope::AgentCapturePending);
        assert_eq!(committed_header.mac, artifact.header.mac);
        let verified = load_verified_versioned_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            artifact.binding(),
            &committed_header,
            deadline(),
        )
        .await
        .expect("new reader must load the same committed production artifact");
        assert_eq!(
            verified
                .coverage()
                .claims()
                .map(|(version, claim)| { (version.as_i64(), claim.logical_turn_key.as_str()) })
                .collect::<Vec<_>>(),
            [(1, "synthetic-turn-0"), (2, "synthetic-turn-1")]
        );
        assert_eq!(
            verified
                .reasoning()
                .associations()
                .iter()
                .map(|entry| {
                    (
                        entry.coverage_schema_version,
                        entry.logical_turn_key.as_str(),
                    )
                })
                .collect::<Vec<_>>(),
            [(1, "synthetic-turn-0"), (2, "synthetic-turn-1")]
        );
        assert!(
            verified
                .coverage()
                .claims()
                .zip(&artifact.coverage_claims)
                .all(
                    |((version, original), sealed)| version == sealed.coverage_schema_version
                        && *original == sealed.claim
                )
        );
        assert_eq!(verified.reasoning().original_envelope(), artifact.bytes());
        assert_eq!(verified.reasoning().original_mac(), committed_header.mac);
        assert!(verified.payload().is_exact_complete_snapshot());
        let candidate = serde_json::from_slice(artifact.bytes()).unwrap();
        let token = cov2_25_authenticate_candidate(
            &f,
            artifact.bytes(),
            &artifact.header.mac,
            artifact.binding(),
            &candidate,
            f.root.path(),
        )
        .await
        .unwrap();
        assert!(verified.reasoning().matches_integrity(&token));
        let before = cov2_22_persisted_state(&f).await;
        let mut wrong_header = artifact.header.clone();
        wrong_header
            .mac
            .replace_range(wrong_header.mac.len() - 1.., "0");
        if wrong_header.mac == artifact.header.mac {
            wrong_header
                .mac
                .replace_range(wrong_header.mac.len() - 1.., "1");
        }
        assert!(
            load_verified_versioned_payload(
                &f.db,
                &f.storage,
                f.root.path(),
                &identity,
                artifact.binding(),
                &wrong_header,
                deadline(),
            )
            .await
            .is_err()
        );
        let foreign = Fixture::new().await;
        assert!(
            load_verified_versioned_payload(
                &f.db,
                &f.storage,
                foreign.root.path(),
                &identity,
                artifact.binding(),
                &artifact.header,
                deadline(),
            )
            .await
            .is_err(),
            "foreign authorized root returns no verified payload"
        );
        assert!(
            load_verified_versioned_payload(
                &f.db,
                &foreign.storage,
                f.root.path(),
                &identity,
                artifact.binding(),
                &artifact.header,
                deadline(),
            )
            .await
            .is_err(),
            "foreign storage returns no verified payload"
        );
        let mut changed_binding = artifact.binding().clone();
        changed_binding.scope.repo_id = "foreign-repository".into();
        let mut changed_header = artifact.header.clone();
        changed_header.binding = changed_binding.clone();
        assert!(
            load_verified_versioned_payload(
                &f.db,
                &f.storage,
                f.root.path(),
                &identity,
                &changed_binding,
                &changed_header,
                deadline(),
            )
            .await
            .is_err(),
            "foreign scope returns no verified payload"
        );
        // Tamper the stored original in a disposable transaction while keeping
        // its committed header MAC. This negative never substitutes for the
        // real persist/commit positive above.
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        let mut changed = artifact.bytes().to_vec();
        let target = changed
            .windows(b"owned-live-reservation".len())
            .position(|bytes| bytes == b"owned-live-reservation")
            .unwrap();
        changed[target] = b'x';
        assert_eq!(changed.len(), artifact.bytes().len());
        MetadataKv::set_with_conn(
            &txn,
            MetadataScope::AgentCapturePendingChunk,
            &f.scope.repo_id,
            &chunk_key(&artifact.binding().checkpoint_id, 0),
            &STANDARD.encode(&changed),
            MetadataValueType::Binary,
        )
        .await
        .unwrap();
        assert!(
            load_verified_versioned_payload(
                &txn,
                &f.storage,
                f.root.path(),
                &identity,
                artifact.binding(),
                &artifact.header,
                deadline(),
            )
            .await
            .is_err()
        );
        txn.rollback().await.unwrap();
        assert_eq!(cov2_22_persisted_state(&f).await, before);
        let (absent_bytes, absent_mac, absent_candidate) =
            cov2_25_absent_source_codec_fixture(&f, &artifact).await;
        let absent_token = cov2_25_authenticate_candidate(
            &f,
            &absent_bytes,
            &absent_mac,
            &absent_candidate.binding,
            &absent_candidate,
            f.root.path(),
        )
        .await
        .expect("codec absence still permits the existing integrity-only sentinel");
        cov2_25_assert_token_has_five_original_digests(
            &absent_token,
            &absent_bytes,
            Some(artifact.bytes()),
        );
        let mut absent_header = artifact.header.clone();
        absent_header.mac = absent_mac;
        absent_header.envelope_bytes = absent_bytes.len();
        absent_header.chunks = absent_bytes.len().div_ceil(CHUNK_BYTES);
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        MetadataKv::set_with_conn(
            &txn,
            MetadataScope::AgentCapturePendingChunk,
            &f.scope.repo_id,
            &chunk_key(&artifact.binding().checkpoint_id, 0),
            &STANDARD.encode(&absent_bytes),
            MetadataValueType::Binary,
        )
        .await
        .unwrap();
        let error = load_verified_versioned_payload(
            &txn,
            &f.storage,
            f.root.path(),
            &identity,
            artifact.binding(),
            &absent_header,
            deadline(),
        )
        .await
        .err()
        .expect("native loader must reject codec-only absent source");
        assert!(
            error
                .to_string()
                .contains("requires retained source evidence")
        );
        txn.rollback().await.unwrap();
        assert_eq!(cov2_22_persisted_state(&f).await, before);
        cov2_22_assert_no_ack(&f.db).await;
        let (payload, coverage, reasoning, header, context) = verified.into_replay_parts();
        assert!(payload.is_exact_complete_snapshot());
        assert_eq!(coverage.claims().count(), 2);
        assert_eq!(reasoning.original_envelope(), artifact.bytes());
        assert_eq!(header.mac, artifact.header.mac);
        assert_eq!(context.session_id(), PK);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_real_v2_artifact_rejects_all_v1_plan() {
        let (f, mut artifact) = cov2_22_real_artifact().await;
        // The real native source factory correctly rejects an all-v1 plan.
        // This test-only adversarial mutation starts from a real sealed mixed
        // artifact and bypasses that source gate solely to exercise persist's
        // independent pre-SQL all-v1 defense. It is not a valid native producer
        // or a Source A positive, even though its canonical outer MAC is valid.
        let mut envelope: VersionedPendingEnvelope =
            serde_json::from_slice(artifact.bytes()).unwrap();
        let association = &mut envelope.payload.identity_commit_associations[1];
        assert_eq!(association.coverage_schema_version, 2);
        association.coverage_schema_version = 1;
        for mac in [&mut association.slot, &mut association.descriptor_mac] {
            let suffix = mac
                .strip_prefix("reasoning-identity/hmac-v2/")
                .expect("the original second identity uses the v2 syntax domain");
            *mac = format!("reasoning-identity/hmac-v1/{suffix}");
        }
        envelope.coverage.claims[1].coverage_schema_version = 1;
        artifact.coverage_claims[1].coverage_schema_version = CoverageSchemaVersion::V1;
        artifact.bytes =
            encode_versioned_envelope(envelope.binding, envelope.payload, envelope.coverage)
                .unwrap();
        artifact.header.envelope_bytes = artifact.bytes.len();
        artifact.header.chunks = artifact.bytes.len().div_ceil(CHUNK_BYTES);
        artifact.header.mac = f
            .scope
            .sign_pending_envelope_until(
                &f.db,
                &f.storage,
                f.root.path(),
                &artifact.bytes,
                deadline(),
            )
            .await
            .unwrap();
        assert!(
            artifact
                .coverage_claims
                .iter()
                .all(|entry| entry.coverage_schema_version == CoverageSchemaVersion::V1)
        );
        let before = cov2_22_persisted_state(&f).await;
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        // Missing tables make even the writer-lock statement or claim verify
        // observable as DbErr, so only the pure persist guard can succeed.
        txn.execute_unprepared("DROP TABLE metadata_kv")
            .await
            .unwrap();
        txn.execute_unprepared("DROP TABLE agent_coverage_claim")
            .await
            .unwrap();
        let error = artifact.persist(&txn, deadline()).await.unwrap_err();
        assert!(error.to_string().contains("requires a sealed v2 claim"));
        assert!(!error.chain().any(|cause| cause.is::<sea_orm::DbErr>()));
        txn.rollback().await.unwrap();
        assert_eq!(cov2_22_persisted_state(&f).await, before);
        cov2_22_assert_retained(&f.db, 0).await;
        cov2_22_assert_no_ack(&f.db).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_real_v2_artifact_later_sql_fault_rolls_back() {
        for scope in [
            "agent_capture_pending_chunk",
            "agent_capture_pending",
            "agent_capture_session_alias",
        ] {
            let (f, artifact) = cov2_22_real_artifact().await;
            let before = cov2_22_persisted_state(&f).await;
            f.db.execute_unprepared(&format!(
                "CREATE TRIGGER cov2_22_late_fault BEFORE INSERT ON metadata_kv
                 WHEN NEW.scope = '{scope}' AND
                 (SELECT COUNT(*) FROM agent_coverage_claim WHERE session_id = 'claude__native-session'
                    AND lease_expires_at = 9223372036854775807) = 2
                 BEGIN SELECT RAISE(ABORT, 'synthetic later artifact writer fault'); END"
            )).await.unwrap();
            let txn = db::begin_write_transaction(&f.db).await.unwrap();
            let error = artifact.persist(&txn, deadline()).await.unwrap_err();
            assert!(error.chain().any(|cause| cause.is::<sea_orm::DbErr>()));
            cov2_22_assert_retained(&txn, 2).await;
            // The fault must occur at the selected writer stage, after both
            // retentions; later stage tests observe the earlier real writes.
            let expected_chunks = i64::from(scope != "agent_capture_pending_chunk");
            let expected_header = i64::from(scope == "agent_capture_session_alias");
            assert_eq!(
                cov2_22_scope_count(&txn, &f, MetadataScope::AgentCapturePendingChunk).await,
                expected_chunks
            );
            assert_eq!(
                cov2_22_scope_count(&txn, &f, MetadataScope::AgentCapturePending).await,
                expected_header
            );
            assert_eq!(
                cov2_22_scope_count(&txn, &f, MetadataScope::AgentCaptureSessionAlias).await,
                0
            );
            cov2_22_assert_no_ack(&txn).await;
            txn.rollback().await.unwrap();
            assert_eq!(
                cov2_22_persisted_state(&f).await,
                before,
                "{scope} rollback must restore every row/ref"
            );
            cov2_22_assert_retained(&f.db, 0).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_sealed_native_same_key_persist_defense_fails_closed() {
        let (f, mut artifact) = cov2_22_real_artifact().await;
        // Test-only adversarial construction bypasses the normal COV2-23
        // source borrow rejection solely to reach persistence's second guard.
        let mut envelope: VersionedPendingEnvelope =
            serde_json::from_slice(artifact.bytes()).unwrap();
        envelope.payload.identity_commit_associations[1].logical_turn_key =
            "synthetic-turn-0".into();
        envelope.coverage.claims[1].logical_turn_key = "synthetic-turn-0".into();
        artifact.coverage_claims[1].claim.logical_turn_key = "synthetic-turn-0".into();
        envelope.validate().unwrap();
        artifact.bytes =
            encode_versioned_envelope(envelope.binding, envelope.payload, envelope.coverage)
                .unwrap();
        artifact.header.envelope_bytes = artifact.bytes.len();
        artifact.header.chunks = artifact.bytes.len().div_ceil(CHUNK_BYTES);
        artifact.header.mac = f
            .scope
            .sign_pending_envelope_until(
                &f.db,
                &f.storage,
                f.root.path(),
                &artifact.bytes,
                deadline(),
            )
            .await
            .unwrap();
        let before = cov2_22_persisted_state(&f).await;
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        // If persistence touches its first writer-lock statement, verify or
        // retain before the pure guard, these missing tables produce DbErr.
        // Rollback restores the test's DDL as well as any caller mutations.
        txn.execute_unprepared("DROP TABLE metadata_kv")
            .await
            .unwrap();
        txn.execute_unprepared("DROP TABLE agent_coverage_claim")
            .await
            .unwrap();
        let error = artifact.persist(&txn, deadline()).await.unwrap_err();
        assert!(
            error
                .to_string()
                .contains("cannot persist cross-version turn aliases yet")
        );
        assert!(!error.chain().any(|cause| cause.is::<sea_orm::DbErr>()));
        txn.rollback().await.unwrap();
        assert_eq!(cov2_22_persisted_state(&f).await, before);
        cov2_22_assert_retained(&f.db, 0).await;
        cov2_22_assert_no_ack(&f.db).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_v1_pending_persisted_bytes_and_mac_frozen() {
        // Frozen only after the complete 7efcf868d49a5183e7f0851a89abebee91357155
        // v1 production writer actually sealed, persisted, committed and loaded
        // these raw storage rows under its original locked dependencies. Only
        // cfg(test) sampling was added to that independent historical tree.
        // Reference receipt SHA256: 1bb278f923d0f5e3ab83a1deba498b056218553cd12878eac676f0f39a18ed11.
        // Original envelope SHA256: bf87589384983e2ad5ae74f3773f8459c9a20c56e4736775eb3129fe52a005ef.
        // Public synthetic key bytes 0..31; never derive expected bytes/MACs
        // from the current serializer, loader, signer or roundtrip. This is
        // historical-source compatibility evidence, not a released binary or
        // real provider-native Source A. Rejected Python-only KATs stay in the
        // private failure record and are not accepted reference artifacts.
        const HISTORICAL_SOURCE_7EFCF_BYTES: &[u8] = br#"{"version":1,"binding":{"scope":{"repo_id":"opaque-repository","worktree_id":"","workspace_id":null,"workspace_fence":null},"session_id":"00000000-0000-4000-8000-000000000001","checkpoint_id":"00000000-0000-4000-8000-000000000002","event_id":"00000000-0000-4000-8000-000000000003","action_key":"action-1","receipt_key":"receipt-1","marker_generation":"generation-1","source_commitment":"source/hmac-v2/66ec8ea49c85476434b3dda0ca28738cc02e3d82b1cc9573413ee423e44a088e","reserved_revision":1,"original_deadline_millis":null,"deferrable":false,"first_attempt_millis":1,"parent_commit":null,"parent_unborn":true},"payload":{"version":1,"alias":"00000000-0000-4000-8000-000000000001","snapshot":{"completeness":"complete","partial_reason":null,"source":{"kind":"trusted_export","identity":"not_retained:v1","digest_sha256":"source/hmac-v2/66ec8ea49c85476434b3dda0ca28738cc02e3d82b1cc9573413ee423e44a088e","byte_len":1},"transcript_redacted_bytes":1,"redaction_match_count":0,"redaction_bytes_scanned":1,"redaction_bytes_redacted":0},"transcript":"eA==","metadata":{"agent_kind":"claude_code","checkpoint_id":"00000000-0000-4000-8000-000000000002","created_at":1,"extraction":{},"model":{},"redaction_report":{"bytes_redacted":0,"bytes_scanned":1,"matches":[]},"schema_version":2,"scope":"committed"},"lifecycle":[{"agent_kind":"claude_code","event_id":"00000000-0000-4000-8000-000000000003","identity_scheme":"native_replay_hmac_v2","kind":"session_end","partial":false,"provenance":{},"schema_version":2,"source":null,"timestamp":"1970-01-01T00:00:01Z"}],"redaction_report":"eyJieXRlc19yZWRhY3RlZCI6MCwiYnl0ZXNfc2Nhbm5lZCI6MSwibWF0Y2hlcyI6W119","metadata_mac":"pending-envelope/hmac-v1/ab0bd2a456d062e3c0f91e4b8d3be1814c9ff26482680fc17e89c3bc9ca18be0","lifecycle_mac":"pending-envelope/hmac-v1/ec60fa7223a85b8c464869fb91467a11c0fa44cc8040eed5e9e53aed284d6aec"},"coverage":{"owner":"owned-live-reservation","created_at":1,"claims":[{"logical_turn_key":"turn-1","coverage_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","completeness":"complete","fence_token":1,"next_revision":1}]}}"#;
        const HISTORICAL_SOURCE_7EFCF_HEADER: &str = r#"{"version":1,"binding":{"scope":{"repo_id":"opaque-repository","worktree_id":"","workspace_id":null,"workspace_fence":null},"session_id":"00000000-0000-4000-8000-000000000001","checkpoint_id":"00000000-0000-4000-8000-000000000002","event_id":"00000000-0000-4000-8000-000000000003","action_key":"action-1","receipt_key":"receipt-1","marker_generation":"generation-1","source_commitment":"source/hmac-v2/66ec8ea49c85476434b3dda0ca28738cc02e3d82b1cc9573413ee423e44a088e","reserved_revision":1,"original_deadline_millis":null,"deferrable":false,"first_attempt_millis":1,"parent_commit":null,"parent_unborn":true},"mac":"pending-envelope/hmac-v1/70f5d7b8cb511d2ea2d9f964b44ba3a404be80f9e6467f22c7e873f093367c8a","envelope_bytes":2104,"chunks":1,"manual_attempted":false}"#;
        const HISTORICAL_SOURCE_7EFCF_MAC: &str = "pending-envelope/hmac-v1/70f5d7b8cb511d2ea2d9f964b44ba3a404be80f9e6467f22c7e873f093367c8a";
        const HISTORICAL_SOURCE_7EFCF_ALIAS: &str = r#"{"body":{"version":1,"alias":"00000000-0000-4000-8000-000000000001","session_id":"claude__native-session","repo_id":"opaque-repository","worktree_id":"","workspace_id":null,"capture_incarnation":null},"mac":"pending-alias/hmac-v1/091353a81935e0694b52f4cf99250b7f44ef857b35bd709bdfa4eadfcc28cc87"}"#;
        const HISTORICAL_SOURCE_7EFCF_METADATA: &[u8] = br#"{
  "agent_kind": "claude_code",
  "checkpoint_id": "00000000-0000-4000-8000-000000000002",
  "created_at": 1,
  "extraction": {},
  "model": {},
  "provider_session_id": "native-session",
  "redaction_report": {
    "bytes_redacted": 0,
    "bytes_scanned": 1,
    "matches": []
  },
  "schema_version": 2,
  "scope": "committed",
  "session_id": "claude__native-session",
  "working_dir": "/frozen/workspace"
}"#;
        const HISTORICAL_SOURCE_7EFCF_LIFECYCLE: &[u8] = br#"{"agent_kind":"claude_code","event_id":"00000000-0000-4000-8000-000000000003","identity_scheme":"native_replay_hmac_v2","kind":"session_end","partial":false,"provenance":{},"provider_session_id":"native-session","schema_version":2,"session_id":"claude__native-session","source":null,"timestamp":"1970-01-01T00:00:01Z"}
"#;
        const HISTORICAL_SOURCE_7EFCF_REPORT: &[u8] =
            br#"{"bytes_redacted":0,"bytes_scanned":1,"matches":[]}"#;
        let f = Fixture::new().await;
        let fixed_key: [u8; 32] = std::array::from_fn(|index| index as u8);
        std::fs::write(
            f.storage.join("private/agent-capture-dedup-v1.key"),
            fixed_key,
        )
        .unwrap();
        f.db.execute_unprepared("UPDATE agent_session SET working_dir = '/frozen/workspace' WHERE session_id = 'claude__native-session'").await.unwrap();
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        let context = resolve_pending_session_context(&txn, &f.scope, PK)
            .await
            .unwrap();
        txn.commit().await.unwrap();
        // Verify the actual historical committed alias record; its original
        // MAC is consumed unchanged, never produced by the current test signer.
        let record: PendingSessionAlias =
            serde_json::from_str(HISTORICAL_SOURCE_7EFCF_ALIAS).unwrap();
        let identity = PendingSessionAlias::prepare(
            &f.db,
            &context,
            Some(record),
            &f.storage,
            f.root.path(),
            deadline(),
        )
        .await
        .unwrap();
        let bytes = b"x".to_vec();
        let auth = ExportAuthorized::issue("claude_code", NATIVE, &bytes);
        let mut snapshot = CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes { bytes, auth },
            "claude_code",
            NATIVE,
            Default::default(),
        );
        let source = key::derive_snapshot_content_commitment_in_scope_until(
            &f.db,
            &f.scope,
            &f.storage,
            f.root.path(),
            &snapshot.redacted_digest_preimage().unwrap(),
            deadline(),
        )
        .await
        .unwrap();
        assert!(snapshot.bind_source_commitment(source.clone()));
        let mut bound = binding(
            identity.alias(),
            Uuid::parse_str("00000000-0000-4000-8000-000000000002").unwrap(),
            Uuid::parse_str("00000000-0000-4000-8000-000000000003").unwrap(),
            &f.scope,
            source,
        );
        bound.original_deadline_millis = None;
        bound.deferrable = false;
        bound.parent_commit = None;
        bound.parent_unborn = true;
        let report = serde_json::to_value(snapshot.redaction_report()).unwrap();
        let metadata = serde_json::json!({
            "schema_version":2,"checkpoint_id":bound.checkpoint_id,"session_id":PK,"provider_session_id":NATIVE,
            "working_dir":"/frozen/workspace","agent_kind":"claude_code","scope":"committed",
            "model":{},"created_at":1,"redaction_report":report,"extraction":{},
        });
        let event = serde_json::json!({
            "schema_version":2,"event_id":bound.event_id,"identity_scheme":"native_replay_hmac_v2",
            "kind":"session_end","agent_kind":"claude_code","timestamp":"1970-01-01T00:00:01Z",
            "source":null,"partial":false,"provenance":{},"session_id":PK,"provider_session_id":NATIVE,
        });
        let mut lifecycle = serde_json::to_vec(&normalize_json_value(event)).unwrap();
        lifecycle.push(b'\n');
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot,
            redact(&serde_json::to_vec_pretty(&normalize_json_value(metadata)).unwrap()),
            redact(&lifecycle),
            redact(&serde_json::to_vec(&normalize_json_value(report)).unwrap()),
        )
        .unwrap();
        let coverage = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: PK.into(),
            checkpoint_id: bound.checkpoint_id.clone(),
            owner: "owned-live-reservation".into(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: vec![ReservedTurnClaim {
                logical_turn_key: "turn-1".into(),
                coverage_digest: "d".repeat(64),
                completeness: Completeness::Complete,
                fence_token: 1,
                next_revision: 1,
            }],
            import_session: None,
            import_identity: None,
            capture_scope: Some(f.scope.clone()),
        };
        #[derive(Deserialize)]
        struct OriginalGoldMembers<'a> {
            #[serde(borrow)]
            payload: &'a RawValue,
            #[serde(borrow)]
            coverage: &'a RawValue,
        }
        let gold: OriginalGoldMembers<'_> =
            serde_json::from_slice(HISTORICAL_SOURCE_7EFCF_BYTES).unwrap();
        let old_projection = frozen_pre_v2::PendingPayloadProjection::seal(
            &f.db,
            &f.storage,
            f.root.path(),
            &payload,
            &identity,
            deadline(),
        )
        .await
        .expect("execute the immutable pre-v2 inner producer");
        let old_encoded = old_projection.encode().unwrap();
        assert_eq!(old_encoded, gold.payload.get().as_bytes());
        let old_coverage =
            frozen_pre_v2::PortablePendingCoverage::from_live(&coverage, &identity).unwrap();
        assert_eq!(
            serde_json::to_vec(&old_coverage).unwrap(),
            gold.coverage.get().as_bytes()
        );
        let expected_sidecars = [
            HISTORICAL_SOURCE_7EFCF_METADATA,
            HISTORICAL_SOURCE_7EFCF_LIFECYCLE,
            HISTORICAL_SOURCE_7EFCF_REPORT,
        ];
        assert_eq!(payload.metadata_json().bytes(), expected_sidecars[0]);
        assert_eq!(
            payload.lifecycle_events_jsonl().bytes(),
            expected_sidecars[1]
        );
        assert_eq!(
            payload.redaction_report_json().bytes(),
            expected_sidecars[2]
        );
        let artifact = SealedPendingArtifact::seal(
            &f.db,
            &f.storage,
            f.root.path(),
            PendingSealRequest {
                binding: bound,
                identity,
                payload: &payload,
                coverage: &coverage,
                reasoning: None,
            },
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(artifact.bytes.as_slice(), HISTORICAL_SOURCE_7EFCF_BYTES);
        assert_eq!(artifact.header.mac, HISTORICAL_SOURCE_7EFCF_MAC);
        assert_eq!(
            serde_json::to_string(&artifact.header).unwrap(),
            HISTORICAL_SOURCE_7EFCF_HEADER
        );
        f.persist(&artifact).await;
        let stored = MetadataKv::get_with_conn(
            &f.db,
            MetadataScope::AgentCapturePending,
            &f.scope.repo_id,
            &artifact.binding().checkpoint_id,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(stored.value_type, "text");
        assert_eq!(
            stored.value.as_bytes(),
            HISTORICAL_SOURCE_7EFCF_HEADER.as_bytes()
        );
        let range = chunk_range(&artifact.binding().checkpoint_id);
        let chunks = MetadataKv::list_bounded_with_conn(
            &f.db,
            &[MetadataScope::AgentCapturePendingChunk],
            Some(&f.scope.repo_id),
            Some((&range.0, &range.1)),
            MAX_CHUNKS as u64 + 1,
            CHUNK_TEXT_BYTES,
        )
        .await
        .unwrap();
        assert_eq!(chunks.len(), 1);
        assert_eq!(
            chunks[0].key,
            chunk_key(&artifact.binding().checkpoint_id, 0)
        );
        assert_eq!(chunks[0].value_type, "binary");
        assert_eq!(
            chunks[0].value,
            STANDARD.encode(HISTORICAL_SOURCE_7EFCF_BYTES)
        );
        assert_eq!(
            STANDARD.decode(&chunks[0].value).unwrap(),
            HISTORICAL_SOURCE_7EFCF_BYTES
        );
        let alias = MetadataKv::get_with_conn(
            &f.db,
            MetadataScope::AgentCaptureSessionAlias,
            &f.scope.repo_id,
            artifact.identity.alias(),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(alias.value_type, "text");
        assert_eq!(alias.value, HISTORICAL_SOURCE_7EFCF_ALIAS);
        let header = decode_entry(&stored).unwrap();
        assert_eq!(
            load_chunks(&f.db, &header).await.unwrap(),
            HISTORICAL_SOURCE_7EFCF_BYTES
        );
        let bound = artifact.binding().clone();
        let identity = f.identity(Some(artifact.identity.alias())).await;
        drop(payload);
        drop(artifact);
        let before = cov2_22_persisted_state(&f).await;
        let loaded = load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            &bound,
            &header,
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(loaded.payload().transcript().bytes(), b"x");
        assert_eq!(loaded.header().mac, HISTORICAL_SOURCE_7EFCF_MAC);
        let old_loaded = frozen_pre_v2::load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            &bound,
            &header,
            deadline(),
        )
        .await
        .expect("immutable old reader accepts original committed gold bytes/MAC");
        for restored in [loaded.payload(), old_loaded.payload()] {
            assert_eq!(restored.transcript().bytes(), b"x");
            assert_eq!(restored.metadata_json().bytes(), expected_sidecars[0]);
            assert_eq!(
                restored.lifecycle_events_jsonl().bytes(),
                expected_sidecars[1]
            );
            assert_eq!(
                restored.redaction_report_json().bytes(),
                expected_sidecars[2]
            );
        }
        for plan in [loaded.coverage(), old_loaded.coverage()] {
            assert_eq!(plan.claims, coverage.claims);
            assert_eq!(plan.owner, coverage.owner);
            assert_eq!(plan.session_id, coverage.session_id);
            assert_eq!(plan.capture_scope, coverage.capture_scope);
            assert_eq!(plan.checkpoint_id, coverage.checkpoint_id);
            assert_eq!(plan.parent_commit, coverage.parent_commit);
            assert_eq!(plan.created_at, coverage.created_at);
            assert_eq!(plan.source_channel, "live");
            assert!(plan.import_identity.is_none() && plan.import_session.is_none());
        }
        assert_eq!(cov2_22_persisted_state(&f).await, before);
        cov2_22_assert_no_ack(&f.db).await;
    }

    #[cfg(unix)]
    async fn cov2_25_seal_two_turns_in_repo(
        f: &Fixture,
        proof_tag: &str,
    ) -> SealedVersionedPendingArtifact {
        use crate::internal::ai::coverage_gate::tests::cov2_native_v2_turn;

        let (ordinary, ordinary_payload) = f.artifact(None).await;
        let binding = ordinary.header.binding;
        let identity = ordinary.identity;
        let payload = native_pending_payload_from_ordinary(
            &ordinary_payload,
            vec![
                (
                    0,
                    format!("synthetic {proof_tag} native proof for first key").into_bytes(),
                ),
                (
                    1,
                    format!("synthetic {proof_tag} native proof for second key").into_bytes(),
                ),
            ],
        );
        let turns = [
            cov2_native_v2_turn(
                "synthetic-turn-0",
                &format!("synthetic {proof_tag} visible reasoning for first key"),
                Completeness::Complete,
            ),
            cov2_native_v2_turn(
                "synthetic-turn-1",
                &format!("synthetic {proof_tag} visible reasoning for second key"),
                Completeness::Complete,
            ),
        ];
        let companion = versioned_companion_for_turns(f, &payload, &binding, &turns).await;
        SealedVersionedPendingArtifact::seal(
            &f.db,
            &f.storage,
            f.root.path(),
            VersionedPendingSealRequest {
                binding,
                identity,
                payload: &payload,
                companion: &companion,
            },
            deadline(),
        )
        .await
        .expect("production v2 sealer must preserve both distinct native turns")
    }

    #[cfg(unix)]
    async fn cov2_25_sealed_fixture() -> (Fixture, SealedVersionedPendingArtifact) {
        let f = Fixture::new().await;
        let artifact = cov2_25_seal_two_turns_in_repo(&f, "first").await;
        (f, artifact)
    }

    #[cfg(unix)]
    async fn cov2_25_authenticate_candidate(
        f: &Fixture,
        bytes: &[u8],
        mac: &str,
        expected_binding: &PendingBinding,
        candidate: &VersionedPendingEnvelope,
        root: &Path,
    ) -> Result<crate::internal::ai::observed_agents::reasoning::PendingIntegrityToken> {
        key::authenticate_pending_versioned_reasoning_members_in_scope_until(
            &f.db,
            key::PendingVersionedReasoningAuthentication {
                scope: &f.scope,
                storage_path: &f.storage,
                authorized_root: root,
                original_envelope: bytes,
                original_mac: mac,
                expected_binding,
                candidate,
                deadline: deadline(),
            },
        )
        .await
    }

    #[cfg(unix)]
    async fn cov2_25_absent_source_codec_fixture(
        f: &Fixture,
        artifact: &SealedVersionedPendingArtifact,
    ) -> (Vec<u8>, String, VersionedPendingEnvelope) {
        // This is a codec/key-owner integrity fixture, not a native seal or
        // evidence that a production source can omit its reasoning member.
        let mut codec: VersionedPendingEnvelope = serde_json::from_slice(artifact.bytes()).unwrap();
        codec.payload.reasoning_source = None;
        let bytes = encode_versioned_envelope(codec.binding, codec.payload, codec.coverage)
            .expect("encode bounded canonical absent-source v2 bytes");
        let candidate: VersionedPendingEnvelope = serde_json::from_slice(&bytes).unwrap();
        assert!(candidate.payload.reasoning_source.is_none());
        let mac = f
            .scope
            .sign_pending_envelope_until(&f.db, &f.storage, f.root.path(), &bytes, deadline())
            .await
            .expect("sign absent-source codec bytes with the existing repository key");
        (bytes, mac, candidate)
    }

    #[cfg(unix)]
    fn cov2_25_assert_token_has_five_original_digests(
        token: &crate::internal::ai::observed_agents::reasoning::PendingIntegrityToken,
        bytes: &[u8],
        some_source_envelope: Option<&[u8]>,
    ) {
        use sha2::{Digest, Sha256};

        #[derive(Deserialize)]
        struct RawOuter<'a> {
            #[serde(borrow)]
            payload: &'a RawValue,
            #[serde(borrow)]
            coverage: &'a RawValue,
        }
        #[derive(Deserialize)]
        struct RawPayload<'a> {
            #[serde(borrow)]
            redacted: &'a RawValue,
            #[serde(borrow)]
            reasoning_source: Option<&'a RawValue>,
            #[serde(borrow)]
            identity_commit_associations: &'a RawValue,
        }
        fn digest(bytes: &[u8]) -> [u8; 32] {
            Sha256::digest(bytes).into()
        }

        let outer: RawOuter<'_> = serde_json::from_slice(bytes).unwrap();
        let payload: RawPayload<'_> = serde_json::from_str(outer.payload.get()).unwrap();
        let source_digest = payload.reasoning_source.map_or_else(
            || digest(b"libra/pending-v2/no-reasoning-source\0"),
            |source| digest(source.get().as_bytes()),
        );
        let originals = [
            digest(bytes),
            digest(payload.redacted.get().as_bytes()),
            source_digest,
            digest(payload.identity_commit_associations.get().as_bytes()),
            digest(outer.coverage.get().as_bytes()),
        ];
        let expected =
            crate::internal::ai::observed_agents::reasoning::PendingIntegrityToken::from_authenticated_members(
                originals[0], originals[1], originals[2], originals[3], originals[4],
            );
        assert!(token.matches_stamp(&expected.into_stamp()));
        if payload.reasoning_source.is_none() {
            let some_outer: RawOuter<'_> = serde_json::from_slice(
                some_source_envelope.expect("absent-source case needs an actual sealed source"),
            )
            .unwrap();
            let some_payload: RawPayload<'_> =
                serde_json::from_str(some_outer.payload.get()).unwrap();
            let actual_source_digest = digest(
                some_payload
                    .reasoning_source
                    .expect("production-sealed source must be present")
                    .get()
                    .as_bytes(),
            );
            assert_ne!(source_digest, actual_source_digest);
            let real_source_stamp =
                crate::internal::ai::observed_agents::reasoning::PendingIntegrityToken::from_authenticated_members(
                    originals[0], originals[1], actual_source_digest, originals[3], originals[4],
                )
                .into_stamp();
            assert!(
                !token.matches_stamp(&real_source_stamp),
                "absent-source integrity sentinel must not authorize a real source"
            );
        }
        for changed in 0..originals.len() {
            let mut altered = originals;
            altered[changed][0] ^= 1;
            let wrong =
                crate::internal::ai::observed_agents::reasoning::PendingIntegrityToken::from_authenticated_members(
                    altered[0], altered[1], altered[2], altered[3], altered[4],
                );
            assert!(
                !token.matches_stamp(&wrong.into_stamp()),
                "token digest {changed} must come from the original raw member"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_pending_v2_original_members_authenticate_exact_tuple() {
        let (f, artifact) = cov2_25_sealed_fixture().await;
        let key_path = f.storage.join("private/agent-capture-dedup-v1.key");
        let key_before = std::fs::read(&key_path).unwrap();
        let candidate: VersionedPendingEnvelope = serde_json::from_slice(artifact.bytes()).unwrap();
        let token = cov2_25_authenticate_candidate(
            &f,
            artifact.bytes(),
            &artifact.header.mac,
            artifact.binding(),
            &candidate,
            f.root.path(),
        )
        .await
        .expect("original production-sealed v2 members must authenticate");
        cov2_25_assert_token_has_five_original_digests(&token, artifact.bytes(), None);
        let associations =
            serde_json::to_vec(&candidate.payload.identity_commit_associations).unwrap();
        assert!(token.matches_associations(&associations));
        assert_eq!(candidate.payload.identity_commit_associations.len(), 2);
        assert_eq!(candidate.coverage.claims.len(), 2);
        assert_eq!(
            candidate
                .payload
                .identity_commit_associations
                .iter()
                .map(|entry| entry.logical_turn_key.as_str())
                .collect::<Vec<_>>(),
            ["synthetic-turn-0", "synthetic-turn-1"]
        );
        assert_eq!(
            candidate
                .payload
                .identity_commit_associations
                .iter()
                .map(|entry| (
                    entry.coverage_schema_version,
                    entry.logical_turn_key.as_str()
                ))
                .collect::<Vec<_>>(),
            candidate
                .coverage
                .claims
                .iter()
                .map(|entry| (
                    entry.coverage_schema_version,
                    entry.logical_turn_key.as_str()
                ))
                .collect::<Vec<_>>()
        );
        let second = cov2_25_seal_two_turns_in_repo(&f, "second").await;
        assert_ne!(artifact.bytes(), second.bytes());
        let second_candidate: VersionedPendingEnvelope =
            serde_json::from_slice(second.bytes()).unwrap();
        let second_token = cov2_25_authenticate_candidate(
            &f,
            second.bytes(),
            &second.header.mac,
            second.binding(),
            &second_candidate,
            f.root.path(),
        )
        .await
        .expect("second production-sealed envelope under the same key must authenticate");
        cov2_25_assert_token_has_five_original_digests(&second_token, second.bytes(), None);

        // The optional absence case uses the bounded codec and the same
        // already initialized repository key. It proves full raw-member
        // integrity only; the native production sealer above emits Some.
        let (absent_bytes, absent_mac, absent_candidate) =
            cov2_25_absent_source_codec_fixture(&f, &artifact).await;
        let absent_token = cov2_25_authenticate_candidate(
            &f,
            &absent_bytes,
            &absent_mac,
            &absent_candidate.binding,
            &absent_candidate,
            f.root.path(),
        )
        .await
        .expect("canonical absent-source members must receive local integrity only");
        cov2_25_assert_token_has_five_original_digests(
            &absent_token,
            &absent_bytes,
            Some(artifact.bytes()),
        );
        assert!(absent_token.matches_associations(
            &serde_json::to_vec(&absent_candidate.payload.identity_commit_associations).unwrap()
        ));
        assert_eq!(std::fs::read(&key_path).unwrap(), key_before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_pending_v2_original_members_reject_substitution_or_bad_mac() {
        let (f, artifact) = cov2_25_sealed_fixture().await;
        let key_path = f.storage.join("private/agent-capture-dedup-v1.key");
        let key_before = std::fs::read(&key_path).unwrap();
        let original: serde_json::Value = serde_json::from_slice(artifact.bytes()).unwrap();
        let candidate: VersionedPendingEnvelope = serde_json::from_slice(artifact.bytes()).unwrap();
        let mut changed_mac = artifact.header.mac.clone();
        changed_mac.replace_range(changed_mac.len() - 1.., "0");
        if changed_mac == artifact.header.mac {
            changed_mac.replace_range(changed_mac.len() - 1.., "1");
        }
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &changed_mac,
                artifact.binding(),
                &candidate,
                f.root.path(),
            )
            .await
            .is_err()
        );
        let (absent_bytes, absent_mac, absent_candidate) =
            cov2_25_absent_source_codec_fixture(&f, &artifact).await;
        assert!(key::cov2_versioned_absent_source_members_match_for_test(
            &absent_bytes,
            &absent_candidate
        ));
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                &absent_bytes,
                &absent_mac,
                &absent_candidate.binding,
                &candidate,
                f.root.path(),
            )
            .await
            .is_err(),
            "a caller Some source must not match authenticated absent source bytes"
        );
        let mut bad_absent_mac = absent_mac.clone();
        bad_absent_mac.replace_range(bad_absent_mac.len() - 1.., "0");
        if bad_absent_mac == absent_mac {
            bad_absent_mac.replace_range(bad_absent_mac.len() - 1.., "1");
        }
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                &absent_bytes,
                &bad_absent_mac,
                &absent_candidate.binding,
                &absent_candidate,
                f.root.path(),
            )
            .await
            .is_err(),
            "absent source cannot bypass the whole-envelope MAC"
        );
        let with_null_source = String::from_utf8(absent_bytes.clone()).unwrap().replacen(
            "\"identity_commit_associations\":",
            "\"reasoning_source\":null,\"identity_commit_associations\":",
            1,
        );
        assert_ne!(with_null_source.as_bytes(), absent_bytes);
        assert!(serde_json::from_str::<VersionedPendingEnvelope>(&with_null_source).is_ok());
        assert!(!key::cov2_versioned_absent_source_members_match_for_test(
            with_null_source.as_bytes(),
            &absent_candidate
        ));
        let null_mac = f
            .scope
            .sign_pending_envelope_until(
                &f.db,
                &f.storage,
                f.root.path(),
                with_null_source.as_bytes(),
                deadline(),
            )
            .await
            .unwrap();
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                with_null_source.as_bytes(),
                &null_mac,
                &absent_candidate.binding,
                &absent_candidate,
                f.root.path(),
            )
            .await
            .is_err(),
            "even validly MACed explicit null is not canonical source omission"
        );
        let wrong_domain = artifact.header.mac.replacen(
            "pending-envelope/hmac-v1/",
            "pending-envelope/hmac-v2/",
            1,
        );
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &wrong_domain,
                artifact.binding(),
                &candidate,
                f.root.path(),
            )
            .await
            .is_err()
        );
        let mut wrong_scope = artifact.binding().clone();
        wrong_scope.scope.repo_id.push_str("-foreign");
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &artifact.header.mac,
                &wrong_scope,
                &candidate,
                f.root.path(),
            )
            .await
            .is_err()
        );
        let mut foreign_request_scope = f.scope.clone();
        foreign_request_scope.repo_id.push_str("-foreign");
        assert!(
            key::authenticate_pending_versioned_reasoning_members_in_scope_until(
                &f.db,
                key::PendingVersionedReasoningAuthentication {
                    scope: &foreign_request_scope,
                    storage_path: &f.storage,
                    authorized_root: f.root.path(),
                    original_envelope: artifact.bytes(),
                    original_mac: &artifact.header.mac,
                    expected_binding: artifact.binding(),
                    candidate: &candidate,
                    deadline: deadline(),
                },
            )
            .await
            .is_err(),
            "a foreign requested capture scope must not authenticate the original envelope"
        );
        // A child path still resolves to this same repository and scope.
        // Use a separate initialized repository to test a foreign root.
        let foreign_repo = Fixture::new().await;
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &artifact.header.mac,
                artifact.binding(),
                &candidate,
                foreign_repo.root.path(),
            )
            .await
            .is_err()
        );
        assert!(
            key::authenticate_pending_versioned_reasoning_members_in_scope_until(
                &f.db,
                key::PendingVersionedReasoningAuthentication {
                    scope: &f.scope,
                    storage_path: &foreign_repo.storage,
                    authorized_root: f.root.path(),
                    original_envelope: artifact.bytes(),
                    original_mac: &artifact.header.mac,
                    expected_binding: artifact.binding(),
                    candidate: &candidate,
                    deadline: deadline(),
                },
            )
            .await
            .is_err(),
            "a foreign storage path must not authenticate under the original root"
        );
        for (path, substitute) in [
            (
                "/binding/first_attempt_millis",
                serde_json::json!(123_456_789),
            ),
            (
                "/payload/redacted/redaction_report",
                serde_json::json!("changed report"),
            ),
            (
                "/payload/reasoning_source/capture_session",
                serde_json::json!("foreign"),
            ),
            (
                "/payload/identity_commit_associations/0/logical_turn_key",
                serde_json::json!("foreign-turn"),
            ),
            ("/coverage/owner", serde_json::json!("foreign-owner")),
            (
                "/coverage/claims/0/coverage_schema_version",
                serde_json::json!(1),
            ),
        ] {
            let mut changed = original.clone();
            *changed
                .pointer_mut(path)
                .expect("fixture member must exist") = substitute;
            let changed: VersionedPendingEnvelope = serde_json::from_value(changed).unwrap();
            assert!(
                cov2_25_authenticate_candidate(
                    &f,
                    artifact.bytes(),
                    &artifact.header.mac,
                    artifact.binding(),
                    &changed,
                    f.root.path(),
                )
                .await
                .is_err(),
                "changed member must not receive an integrity token: {path}"
            );
        }
        let mut reordered: VersionedPendingEnvelope =
            serde_json::from_slice(artifact.bytes()).unwrap();
        reordered.payload.identity_commit_associations.swap(0, 1);
        reordered.coverage.claims.swap(0, 1);
        reordered.validate().unwrap();
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &artifact.header.mac,
                artifact.binding(),
                &reordered,
                f.root.path(),
            )
            .await
            .is_err(),
            "a paired, internally valid tuple reorder must not match the original order"
        );
        let other_artifact = cov2_25_seal_two_turns_in_repo(&f, "other").await;
        assert_ne!(artifact.bytes(), other_artifact.bytes());
        let other_candidate: VersionedPendingEnvelope =
            serde_json::from_slice(other_artifact.bytes()).unwrap();
        cov2_25_authenticate_candidate(
            &f,
            other_artifact.bytes(),
            &other_artifact.header.mac,
            other_artifact.binding(),
            &other_candidate,
            f.root.path(),
        )
        .await
        .expect("the donor must itself be valid under the same repository key");
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &artifact.header.mac,
                artifact.binding(),
                &other_candidate,
                f.root.path(),
            )
            .await
            .is_err(),
            "a DTO from another valid envelope must not be spliced in"
        );
        let other_value: serde_json::Value =
            serde_json::from_slice(other_artifact.bytes()).unwrap();
        assert_ne!(
            original["payload"]["reasoning_source"],
            other_value["payload"]["reasoning_source"]
        );
        assert_ne!(original["coverage"], other_value["coverage"]);
        let mut spliced = original.clone();
        spliced["payload"]["reasoning_source"] = other_value["payload"]["reasoning_source"].clone();
        spliced["coverage"] = other_value["coverage"].clone();
        let spliced: VersionedPendingEnvelope = serde_json::from_value(spliced).unwrap();
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &artifact.header.mac,
                artifact.binding(),
                &spliced,
                f.root.path(),
            )
            .await
            .is_err(),
            "originally valid same-key source/coverage members cannot cross envelopes"
        );
        let mut absent_source = original.clone();
        absent_source["payload"]
            .as_object_mut()
            .unwrap()
            .remove("reasoning_source");
        let absent_source: VersionedPendingEnvelope =
            serde_json::from_value(absent_source).unwrap();
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &artifact.header.mac,
                artifact.binding(),
                &absent_source,
                f.root.path(),
            )
            .await
            .is_err(),
            "dropping the original source member must not receive a token"
        );
        let mut noncanonical = artifact.bytes().to_vec();
        noncanonical.insert(1, b' ');
        let noncanonical_mac = f
            .scope
            .sign_pending_envelope_until(
                &f.db,
                &f.storage,
                f.root.path(),
                &noncanonical,
                deadline(),
            )
            .await
            .unwrap();
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                &noncanonical,
                &noncanonical_mac,
                artifact.binding(),
                &candidate,
                f.root.path(),
            )
            .await
            .is_err()
        );
        let wrong_version = String::from_utf8(artifact.bytes().to_vec())
            .unwrap()
            .replacen("\"version\":2", "\"version\":3", 1);
        assert_ne!(wrong_version.as_bytes(), artifact.bytes());
        let wrong_version_mac = f
            .scope
            .sign_pending_envelope_until(
                &f.db,
                &f.storage,
                f.root.path(),
                wrong_version.as_bytes(),
                deadline(),
            )
            .await
            .unwrap();
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                wrong_version.as_bytes(),
                &wrong_version_mac,
                artifact.binding(),
                &candidate,
                f.root.path(),
            )
            .await
            .is_err()
        );
        // Authenticate these adversarial originals with the existing key so
        // rejection proves structural validation after MAC verification too.
        let original_text = String::from_utf8(artifact.bytes().to_vec()).unwrap();
        let redacted_text = serde_json::to_string(&candidate.payload.redacted).unwrap();
        let adversarial = [
            (
                "unknown outer field",
                original_text.replacen(
                    "\"version\":2,",
                    "\"version\":2,\"unknown_outer\":true,",
                    1,
                ),
            ),
            (
                "unknown payload field",
                original_text.replacen(
                    "\"payload\":{",
                    "\"payload\":{\"unknown_payload\":true,",
                    1,
                ),
            ),
            (
                "duplicate outer field",
                original_text.replacen("\"version\":2,", "\"version\":2,\"version\":2,", 1),
            ),
            (
                "duplicate payload field",
                original_text.replacen(
                    "\"redacted\":",
                    &format!("\"redacted\":{redacted_text},\"redacted\":"),
                    1,
                ),
            ),
            (
                "malformed authenticated JSON",
                original_text[..original_text.len() - 1].to_owned(),
            ),
        ];
        for (label, bytes) in adversarial {
            assert_ne!(
                bytes.as_bytes(),
                artifact.bytes(),
                "{label} must change the input"
            );
            let mac = f
                .scope
                .sign_pending_envelope_until(
                    &f.db,
                    &f.storage,
                    f.root.path(),
                    bytes.as_bytes(),
                    deadline(),
                )
                .await
                .unwrap();
            assert!(
                cov2_25_authenticate_candidate(
                    &f,
                    bytes.as_bytes(),
                    &mac,
                    artifact.binding(),
                    &candidate,
                    f.root.path(),
                )
                .await
                .is_err(),
                "{label} must fail after whole-envelope MAC authentication"
            );
        }
        let oversized = vec![b'{'; MAX_ENVELOPE_BYTES + 1];
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                &oversized,
                &artifact.header.mac,
                artifact.binding(),
                &candidate,
                f.root.path(),
            )
            .await
            .is_err()
        );
        assert!(
            key::authenticate_pending_versioned_reasoning_members_in_scope_until(
                &f.db,
                key::PendingVersionedReasoningAuthentication {
                    scope: &f.scope,
                    storage_path: &f.storage,
                    authorized_root: f.root.path(),
                    original_envelope: artifact.bytes(),
                    original_mac: &artifact.header.mac,
                    expected_binding: artifact.binding(),
                    candidate: &candidate,
                    deadline: Instant::now() - Duration::from_millis(1),
                },
            )
            .await
            .is_err(),
            "expired verification cannot issue an integrity token"
        );
        assert_eq!(std::fs::read(&key_path).unwrap(), key_before);
        std::fs::remove_file(&key_path).unwrap();
        assert!(
            cov2_25_authenticate_candidate(
                &f,
                artifact.bytes(),
                &artifact.header.mac,
                artifact.binding(),
                &candidate,
                f.root.path(),
            )
            .await
            .is_err()
        );
        assert!(!key_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_pending_v2_member_auth_preserves_v1() {
        let (f, artifact) = cov2_25_sealed_fixture().await;
        let candidate: VersionedPendingEnvelope = serde_json::from_slice(artifact.bytes()).unwrap();
        assert!(
            key::authenticate_pending_reasoning_original_in_scope_until(
                &f.db,
                &f.scope,
                &f.storage,
                f.root.path(),
                artifact.bytes(),
                &artifact.header.mac,
                deadline(),
            )
            .await
            .is_err(),
            "the unchanged v1 member API must reject outer-v2"
        );
        let v1_fixture = Fixture::new().await;
        let (v1, _) = v1_fixture.artifact(None).await;
        assert!(
            cov2_25_authenticate_candidate(
                &v1_fixture,
                &v1.bytes,
                &v1.header.mac,
                &v1.header.binding,
                &candidate,
                v1_fixture.root.path(),
            )
            .await
            .is_err(),
            "the v2 member API must reject outer-v1"
        );
    }

    fn binding(
        alias: &str,
        checkpoint: Uuid,
        event: Uuid,
        scope: &CaptureScope,
        source: String,
    ) -> PendingBinding {
        PendingBinding {
            scope: scope.clone(),
            session_id: alias.into(),
            checkpoint_id: checkpoint.to_string(),
            event_id: event.to_string(),
            action_key: "action-1".into(),
            receipt_key: "receipt-1".into(),
            marker_generation: "generation-1".into(),
            source_commitment: source,
            reserved_revision: 1,
            original_deadline_millis: Some(1),
            deferrable: true,
            first_attempt_millis: 1,
            parent_commit: Some("b".repeat(40)),
            parent_unborn: false,
        }
    }

    fn header() -> PendingHeader {
        let scope = CaptureScope {
            repo_id: "opaque-repository".into(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        PendingHeader {
            version: VERSION,
            binding: binding(
                &Uuid::new_v4().to_string(),
                Uuid::new_v4(),
                Uuid::new_v4(),
                &scope,
                format!("source/hmac-v2/{}", "a".repeat(64)),
            ),
            mac: format!("pending-envelope/hmac-v1/{}", "c".repeat(64)),
            envelope_bytes: 2,
            chunks: 1,
            manual_attempted: false,
        }
    }

    #[cfg(unix)]
    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_v1_pending_bytes_and_mac_frozen() {
        use sha2::{Digest, Sha256};

        let frozen = include_bytes!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/fixtures/agent_transcripts/reasoning/pending_v1_decoder.rs"
        ));
        // Hash updated when the frozen decoder gained explicit key
        // canonicalization for the GPUI-forced serde_json/preserve_order feature.
        assert_eq!(
            hex::encode(Sha256::digest(frozen)),
            "e8578fce3419fdb7a70b69242f9c3220767897e3fda9ddc1f61e6a9d014a450a",
            "the pre-v2 decoder oracle must remain byte-for-byte frozen"
        );
        let fixture = Fixture::new().await;
        let (artifact, _) = fixture.artifact(None).await;
        let envelope: PendingEnvelope = serde_json::from_slice(&artifact.bytes).unwrap();
        assert_eq!(envelope.version, VERSION);
        let binding = serde_json::to_vec(&envelope.binding).unwrap();
        let payload = serde_json::to_vec(&envelope.payload).unwrap();
        let coverage = serde_json::to_vec(&envelope.coverage).unwrap();
        let frozen_v1_layout = [
            b"{\"version\":1,\"binding\":".as_slice(),
            binding.as_slice(),
            b",\"payload\":".as_slice(),
            payload.as_slice(),
            b",\"coverage\":".as_slice(),
            coverage.as_slice(),
            b"}".as_slice(),
        ]
        .concat();
        assert_eq!(artifact.bytes, frozen_v1_layout);
        frozen_pre_v2::decode_payload_v1(&payload).unwrap();
        let header_text = serde_json::to_string(&artifact.header).unwrap();
        frozen_pre_v2::decode_header_v1(&header_text).unwrap();
        let mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.db,
                &fixture.storage,
                fixture.root.path(),
                &artifact.bytes,
                deadline(),
            )
            .await
            .unwrap();
        assert_eq!(mac, artifact.header.mac);
        fixture
            .scope
            .verify_pending_envelope_until(
                &fixture.db,
                &fixture.storage,
                fixture.root.path(),
                &artifact.bytes,
                &artifact.header.mac,
                deadline(),
            )
            .await
            .unwrap();
        // Frozen v1 wire order from dcc468bd31c88a7e8d8c0d2957d6e80b65908990
        // pending.rs/pending_payload.rs. The fixed key exercises the historical
        // pending HMAC domain; the literal MAC pins these exact bytes.
        // Sidecar tags are format-valid placeholders: only outer bytes and MAC are pinned.
        const FROZEN_V1_BYTES: &[u8] = concat!(
            r#"{"version":1,"binding":{"scope":{"repo_id":"opaque-repository","worktree_id":"","workspace_id":null,"workspace_fence":null},"session_id":"00000000-0000-4000-8000-000000000001","checkpoint_id":"00000000-0000-4000-8000-000000000002","event_id":"00000000-0000-4000-8000-000000000003","action_key":"action-1","receipt_key":"receipt-1","marker_generation":"generation-1","#,
            r#""source_commitment":"source/hmac-v2/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","reserved_revision":1,"original_deadline_millis":null,"deferrable":false,"first_attempt_millis":1,"parent_commit":null,"parent_unborn":true},"payload":{"version":1,"alias":"00000000-0000-4000-8000-000000000001","snapshot":{"completeness":"complete","partial_reason":null,"#,
            r#""source":{"kind":"trusted_export","identity":"not_retained:v1","digest_sha256":"source/hmac-v2/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","byte_len":1},"transcript_redacted_bytes":1,"redaction_match_count":0,"redaction_bytes_scanned":1,"redaction_bytes_redacted":0},"transcript":"eA==","metadata":{"agent_kind":"claude_code","#,
            r#""checkpoint_id":"00000000-0000-4000-8000-000000000002","created_at":1,"extraction":{},"model":{},"redaction_report":{"bytes_redacted":0,"bytes_scanned":1,"matches":[]},"schema_version":2,"scope":"committed"},"lifecycle":[{"agent_kind":"claude_code","event_id":"00000000-0000-4000-8000-000000000003","identity_scheme":"native_replay_hmac_v2","kind":"session_end","partial":false,"provenance":{},"#,
            r#""schema_version":2,"source":null,"timestamp":"1970-01-01T00:00:01Z"}],"redaction_report":"eyJieXRlc19yZWRhY3RlZCI6MCwiYnl0ZXNfc2Nhbm5lZCI6MSwibWF0Y2hlcyI6W119","metadata_mac":"pending-envelope/hmac-v1/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","lifecycle_mac":"pending-envelope/hmac-v1/cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc"},"#,
            r#""coverage":{"owner":"owned-live-reservation","created_at":1,"claims":[{"logical_turn_key":"turn-1","coverage_digest":"dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd","completeness":"complete","fence_token":1,"next_revision":1}]}}"#,
        ).as_bytes();
        const FROZEN_V1_MAC: &str = "pending-envelope/hmac-v1/a164845ca8c6b45df9a777f1a80f77f67243b47b1f933bf8e270c4eff8501bbd";
        let frozen_envelope: PendingEnvelope = serde_json::from_slice(FROZEN_V1_BYTES).unwrap();
        let frozen_payload = serde_json::to_vec(&frozen_envelope.payload).unwrap();
        frozen_pre_v2::decode_payload_v1(&frozen_payload).unwrap();
        let encoded_v1 = encode_envelope(
            frozen_envelope.binding,
            frozen_envelope.payload,
            frozen_envelope.coverage,
        )
        .unwrap();
        assert_eq!(
            encoded_v1.as_slice(),
            FROZEN_V1_BYTES,
            "v1 encoder changed the complete frozen pending envelope",
        );
        assert_eq!(
            hex::encode(Sha256::digest(FROZEN_V1_BYTES)),
            "61d071a8ed03aa1bde199ccace8af992c98eaa89e23bf15c7abe646108b8b4ba",
        );
        let key_path = fixture
            .storage
            .join(key::CAPTURE_DEDUP_SECRET_DIR)
            .join(key::CAPTURE_DEDUP_SECRET_FILE);
        let fixed_key: [u8; 32] = std::array::from_fn(|index| index as u8);
        std::fs::write(&key_path, fixed_key).unwrap();
        let known_mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.db,
                &fixture.storage,
                fixture.root.path(),
                FROZEN_V1_BYTES,
                deadline(),
            )
            .await
            .unwrap();
        assert_eq!(known_mac, FROZEN_V1_MAC);
        fixture
            .scope
            .verify_pending_envelope_until(
                &fixture.db,
                &fixture.storage,
                fixture.root.path(),
                FROZEN_V1_BYTES,
                FROZEN_V1_MAC,
                deadline(),
            )
            .await
            .unwrap();
    }
    #[cfg(unix)]
    fn redact(bytes: &[u8]) -> crate::internal::ai::observed_agents::RedactedBytes {
        Redactor::new_default().redact(bytes).0
    }

    #[test]
    fn header_rejects_unknown_duplicate_and_oversized_fields() {
        let text = serde_json::to_string(&header()).unwrap();
        assert!(PendingHeader::decode(&text).is_ok());
        assert!(PendingHeader::decode(&format!("{{\"version\":1,{}", &text[1..])).is_err());
        assert!(
            PendingHeader::decode(&text.replacen("{", "{\"path\":\"canary-locator\",", 1)).is_err()
        );
        assert!(PendingHeader::decode(&"x".repeat(MAX_HEADER_BYTES + 1)).is_err());
        assert!(
            PendingHeader::decode(
                &text.replace("\"parent_unborn\":false", "\"parent_unborn\":true")
            )
            .is_err()
        );
    }

    #[test]
    fn raw_provider_session_identity_cannot_masquerade_as_opaque_binding() {
        let mut binding = header().binding;
        for id in [
            PK.to_string(),
            Uuid::nil().to_string(),
            Uuid::new_v4().to_string().to_uppercase(),
        ] {
            binding.session_id = id;
            assert!(binding.validate().is_err());
        }
    }

    #[test]
    fn chunk_encoding_is_bounded_and_canonical() {
        assert_eq!(decode_part("YQ==", 1).unwrap(), b"a");
        for text in ["YR==", "YQ", "YQ==\n", "canary invalid"] {
            let error = decode_part(text, 10).unwrap_err().to_string();
            assert!(!error.contains(text));
        }
        assert!(decode_part("YWI=", 1).is_err());
        assert!(decode_part(&"A".repeat(CHUNK_TEXT_BYTES + 4), CHUNK_BYTES).is_err());
        const { assert!(CHUNK_TEXT_BYTES < crate::internal::metadata::MAX_VALUE_LEN) };
        assert_eq!(MAX_ENVELOPE_BYTES.div_ceil(CHUNK_BYTES), MAX_CHUNKS);
    }

    // Queue codec tests need neither a source/key nor session authorization;
    // these ports deliberately cannot create a replay payload or catalog fence.
    async fn queue_fixture() -> (tempfile::TempDir, sea_orm::DatabaseConnection) {
        let dir = tempfile::tempdir().unwrap();
        let conn =
            crate::internal::db::create_database(dir.path().join("queue.db").to_str().unwrap())
                .await
                .unwrap();
        crate::internal::config::ConfigKv::set_with_conn(
            &conn,
            "libra.repoid",
            "opaque-repository",
            false,
        )
        .await
        .unwrap();
        (dir, conn)
    }

    fn queue_scope(repo_id: &str) -> CaptureScope {
        CaptureScope {
            repo_id: repo_id.to_owned(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        }
    }

    fn queue_header(index: usize) -> PendingHeader {
        let mut h = header();
        h.binding.checkpoint_id = format!("00000000-0000-4000-8000-{index:012}");
        h
    }

    async fn put_queue_header<C: ConnectionTrait>(
        conn: &C,
        scope: MetadataScope,
        h: &PendingHeader,
    ) {
        MetadataKv::set_with_conn(
            conn,
            scope,
            &h.binding.scope.repo_id,
            &h.binding.checkpoint_id,
            &serde_json::to_string(h).unwrap(),
            MetadataValueType::Text,
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn exhausted_checkpoint_routing_is_keyless_atomic_and_collision_safe() {
        use sea_orm::TransactionTrait;

        let (_dir, conn) = queue_fixture().await;
        let h = queue_header(1);
        let checkpoint = &h.binding.checkpoint_id;
        // No artifact is legacy policy routing, not a new identity/key gate.
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        assert!(
            !quarantine_checkpoint_if_present(&txn, "legacy-repo", checkpoint)
                .await
                .unwrap()
        );
        txn.commit().await.unwrap();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO metadata_kv(scope,target,key,value,value_type,created_at,updated_at)
             VALUES('agent_capture_pending',?,?,CAST(X'FF' AS TEXT),'text','created','updated')",
            ["opaque-repository".into(), checkpoint.clone().into()],
        ))
        .await
        .unwrap();
        let rows = || async {
            conn.query_all_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                "SELECT scope,hex(CAST(value AS BLOB)) AS v,typeof(value) AS t,value_type,created_at,updated_at
                 FROM metadata_kv WHERE target=? AND key=? ORDER BY scope",
                ["opaque-repository".into(), checkpoint.clone().into()],
            )).await.unwrap().into_iter().map(|row| {
                ["scope", "v", "t", "value_type", "created_at", "updated_at"]
                    .map(|field| row.try_get_by::<String, _>(field).unwrap())
            }).collect::<Vec<_>>()
        };
        let original = rows().await;
        let txn = conn.begin().await.unwrap();
        assert!(
            quarantine_checkpoint_if_present(&txn, "opaque-repository", checkpoint)
                .await
                .unwrap()
        );
        txn.rollback().await.unwrap();
        assert_eq!(rows().await, original);
        let txn = conn.begin().await.unwrap();
        assert!(
            quarantine_checkpoint_if_present(&txn, "opaque-repository", checkpoint)
                .await
                .unwrap()
        );
        txn.commit().await.unwrap();
        let mut expected = original.clone();
        expected[0][0] = "agent_capture_quarantine".into();
        assert_eq!(
            rows().await,
            expected,
            "only scope can change for malformed evidence"
        );
        let txn = conn.begin().await.unwrap();
        assert!(
            quarantine_checkpoint_if_present(&txn, "opaque-repository", checkpoint)
                .await
                .unwrap()
        );
        txn.commit().await.unwrap();
        assert_eq!(
            rows().await,
            expected,
            "already quarantined routing is idempotent"
        );
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &h).await;
        let collision = rows().await;
        let txn = conn.begin().await.unwrap();
        assert!(
            quarantine_checkpoint_if_present(&txn, "opaque-repository", checkpoint)
                .await
                .unwrap()
        );
        txn.commit().await.unwrap();
        assert_eq!(
            rows().await,
            collision,
            "neither namespace may overwrite the other"
        );
    }

    #[tokio::test]
    async fn pending_queue_is_bounded_and_bad_first_rows_do_not_starve() {
        let (_dir, conn) = queue_fixture().await;
        assert!(
            !has_pending_hint(&conn, &queue_scope("opaque-repository"))
                .await
                .unwrap()
        );
        assert!(
            has_pending_hint(&conn, &queue_scope("foreign-repository"))
                .await
                .is_err()
        );
        // Corrupt out-of-band SQL can exceed the metadata setter's key/value
        // bounds. The queue must preserve it without allocating those fields.
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO metadata_kv(scope,target,key,value,value_type,created_at,updated_at)
             VALUES('agent_capture_pending','opaque-repository',?,?, 'text','original-created','original-updated')",
            [
                "!".repeat(1024 * 1024).into(),
                "canary-private-bad-value".repeat(65536).into(),
            ],
        ))
        .await
        .unwrap();
        for i in 1..=4 {
            let mut h = queue_header(i);
            let text = match i {
                1 => "canary-private-malformed".to_owned(),
                2 => "x".repeat(MAX_HEADER_BYTES + 1),
                3 => {
                    h.binding.scope.repo_id = "foreign-repository".into();
                    serde_json::to_string(&h).unwrap()
                }
                _ => {
                    h.binding.checkpoint_id = Uuid::new_v4().to_string();
                    serde_json::to_string(&h).unwrap()
                }
            };
            MetadataKv::set_with_conn(
                &conn,
                MetadataScope::AgentCapturePending,
                "opaque-repository",
                &queue_header(i).binding.checkpoint_id,
                &text,
                MetadataValueType::Text,
            )
            .await
            .unwrap();
        }
        for i in 5..=10 {
            put_queue_header(&conn, MetadataScope::AgentCapturePending, &queue_header(i)).await;
        }
        assert!(
            has_pending_hint(&conn, &queue_scope("opaque-repository"))
                .await
                .unwrap()
        );
        let mut other_worktree = queue_scope("opaque-repository");
        other_worktree.worktree_id = "linked-worktree".into();
        assert!(!has_pending_hint(&conn, &other_worktree).await.unwrap());
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert!(batch.headers.is_empty());
        assert_eq!(batch.quarantined, 5);
        assert!(!batch.conflicting_namespaces);
        // A failed executor transaction does not silently consume evidence.
        txn.rollback().await.unwrap();
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.quarantined, 5);
        txn.commit().await.unwrap();
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.headers.len(), 5);
        assert_eq!(batch.quarantined, 0);
        assert_eq!(
            batch
                .headers
                .iter()
                .map(|h| h.binding.checkpoint_id.clone())
                .collect::<Vec<_>>(),
            (5..=9)
                .map(|i| queue_header(i).binding.checkpoint_id)
                .collect::<Vec<_>>()
        );
        txn.commit().await.unwrap();
        let retained = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT count(*) AS n, max(length(CAST(key AS BLOB))) AS k,
              max(length(CAST(value AS BLOB))) AS v FROM metadata_kv
              WHERE scope = 'agent_capture_quarantine'"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(retained.try_get_by::<i64, _>("n").unwrap(), 5);
        assert_eq!(retained.try_get_by::<i64, _>("k").unwrap(), 1024 * 1024);
        assert_eq!(
            retained.try_get_by::<i64, _>("v").unwrap(),
            "canary-private-bad-value".len() as i64 * 65536
        );
    }

    #[cfg(feature = "test-live-agent")]
    #[tokio::test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    async fn codex_child_recover_scoped_queue_exposes_sixteenth_without_mutating_headers() {
        let (_dir, conn) = queue_fixture().await;
        let scope = queue_scope("opaque-repository");
        for index in 1..=MAX_ARTIFACTS {
            put_queue_header(
                &conn,
                MetadataScope::AgentCapturePending,
                &queue_header(index),
            )
            .await;
        }
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let before = txn.query_all_raw(Statement::from_string(
            txn.get_database_backend(),
            "SELECT key,value FROM metadata_kv WHERE scope='agent_capture_pending' ORDER BY key".to_owned(),
        )).await.unwrap().into_iter().map(|row| (
            row.try_get_by::<String,_>("key").unwrap(),
            row.try_get_by::<String,_>("value").unwrap(),
        )).collect::<Vec<_>>();
        let batch = pending_candidates_for_scope(&txn, &scope).await.unwrap();
        assert_eq!(batch.quarantined, 0);
        assert_eq!(
            batch.headers.len(),
            MAX_ARTIFACTS,
            "long-lived child-pending heads must not hide the last bounded candidate"
        );
        assert_eq!(
            batch.headers.last().unwrap().binding.checkpoint_id,
            queue_header(MAX_ARTIFACTS).binding.checkpoint_id
        );
        assert!(
            batch.window_full,
            "full snapshots retain conservative race handling"
        );
        txn.commit().await.unwrap();
        let after = conn.query_all_raw(Statement::from_string(
            conn.get_database_backend(),
            "SELECT key,value FROM metadata_kv WHERE scope='agent_capture_pending' ORDER BY key".to_owned(),
        )).await.unwrap().into_iter().map(|row| (
            row.try_get_by::<String,_>("key").unwrap(),
            row.try_get_by::<String,_>("value").unwrap(),
        )).collect::<Vec<_>>();
        assert_eq!(
            after, before,
            "selection is not replay or quarantine authority"
        );
    }

    #[tokio::test]
    async fn scoped_worker_queue_excludes_other_worktrees_without_mutating_them() {
        let (_dir, conn) = queue_fixture().await;
        let current = queue_header(1).binding.scope;
        let mut foreign_headers = Vec::new();
        for i in 1..=5 {
            let mut foreign = queue_header(i);
            if i == 1 {
                // Same worktree, different workspace ownership must be just
                // as isolated as a different linked worktree.
                foreign.binding.scope.workspace_id = Some("other-workspace".into());
                foreign.binding.scope.workspace_fence = Some(9);
            } else {
                foreign.binding.scope.worktree_id = "other-worktree".to_string();
            }
            put_queue_header(&conn, MetadataScope::AgentCapturePending, &foreign).await;
            foreign_headers.push(foreign);
        }
        let current_header = queue_header(6);
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &current_header).await;

        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates_for_scope(&txn, &current).await.unwrap();
        assert_eq!(
            batch.headers.len(),
            1,
            "quarantined rows: {}",
            batch.quarantined
        );
        assert_eq!(
            batch.headers[0].binding.checkpoint_id,
            current_header.binding.checkpoint_id
        );
        assert_eq!(batch.quarantined, 0);
        txn.commit().await.unwrap();

        let pending_rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT key FROM metadata_kv WHERE scope = 'agent_capture_pending' AND target = ?",
                ["opaque-repository".into()],
            ))
            .await
            .unwrap();
        let pending_keys = pending_rows
            .iter()
            .map(|row| row.try_get_by::<String, _>("key").unwrap())
            .collect::<Vec<_>>();
        assert_eq!(pending_keys.len(), 6);
        for foreign in foreign_headers {
            assert!(pending_keys.contains(&foreign.binding.checkpoint_id));
        }
        assert!(pending_keys.contains(&current_header.binding.checkpoint_id));
    }

    #[tokio::test]
    async fn scoped_worker_quarantines_valid_json_with_incomplete_scope_identity() {
        let (_dir, conn) = queue_fixture().await;
        let scope = queue_header(1).binding.scope;
        for (index, field) in [
            (1, "worktree_id"),
            (2, "workspace_id"),
            (3, "workspace_fence"),
        ] {
            let header = queue_header(index);
            let mut value = serde_json::to_value(&header).unwrap();
            value
                .pointer_mut("/binding/scope")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .remove(field);
            MetadataKv::set_with_conn(
                &conn,
                MetadataScope::AgentCapturePending,
                "opaque-repository",
                &header.binding.checkpoint_id,
                &serde_json::to_string(&value).unwrap(),
                MetadataValueType::Text,
            )
            .await
            .unwrap();
        }
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates_for_scope(&txn, &scope).await.unwrap();
        assert!(batch.headers.is_empty());
        assert_eq!(batch.quarantined, 3);
        txn.commit().await.unwrap();
        let quarantined = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT count(*) AS n FROM metadata_kv WHERE target = ? AND scope = 'agent_capture_quarantine'",
                ["opaque-repository".into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(quarantined.try_get_by::<i64, _>("n").unwrap(), 3);
    }

    #[tokio::test]
    async fn pending_queue_namespace_collision_retains_both_without_starving() {
        let (_dir, conn) = queue_fixture().await;
        for i in 1..=5 {
            put_queue_header(&conn, MetadataScope::AgentCapturePending, &queue_header(i)).await;
            put_queue_header(
                &conn,
                MetadataScope::AgentCaptureQuarantine,
                &queue_header(i),
            )
            .await;
        }
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &queue_header(6)).await;
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert!(batch.conflicting_namespaces);
        assert_eq!(batch.headers.len(), 1);
        assert_eq!(
            batch.headers[0].binding.checkpoint_id,
            queue_header(6).binding.checkpoint_id
        );
        assert_eq!(batch.quarantined, 0);
        assert!(quarantine_header(&txn, &queue_header(1)).await.is_err());
        assert!(claim_manual_attempt(&txn, &queue_header(1)).await.is_err());
        txn.commit().await.unwrap();
        assert_eq!(
            headers_for_repo(&conn, "opaque-repository", 17)
                .await
                .unwrap()
                .len(),
            11
        );
    }

    #[tokio::test]
    async fn quarantined_header_is_present_but_not_automatic_pending_work() {
        let (_dir, conn) = queue_fixture().await;
        let header = queue_header(1);
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &header).await;
        assert!(
            has_pending_header_for_checkpoint(
                &conn,
                &header.binding.scope.repo_id,
                &header.binding.checkpoint_id,
            )
            .await
            .unwrap()
        );
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        quarantine_header(&txn, &header).await.unwrap();
        txn.commit().await.unwrap();
        assert!(
            has_header_for_checkpoint(
                &conn,
                &header.binding.scope.repo_id,
                &header.binding.checkpoint_id,
            )
            .await
            .unwrap()
        );
        assert!(
            !has_pending_header_for_checkpoint(
                &conn,
                &header.binding.scope.repo_id,
                &header.binding.checkpoint_id,
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn one_shot_manual_header_is_not_pending_queue_work() {
        let (_dir, conn) = queue_fixture().await;
        let header = queue_header(1);
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &header).await;
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        quarantine_header(&txn, &header).await.unwrap();
        let claimed = claim_manual_attempt(&txn, &header).await.unwrap();
        txn.commit().await.unwrap();
        assert!(claimed.manual_attempted);
        assert!(
            has_manual_attempted_header_for_checkpoint(
                &conn,
                &header.binding.scope.repo_id,
                &header.binding.checkpoint_id,
            )
            .await
            .unwrap()
        );
        assert!(
            !has_pending_header_for_checkpoint(
                &conn,
                &header.binding.scope.repo_id,
                &header.binding.checkpoint_id,
            )
            .await
            .unwrap()
        );
    }

    #[tokio::test]
    async fn pending_queue_mixed_invalid_rows_preserve_valid_order_and_sql_types() {
        let (_dir, conn) = queue_fixture().await;
        for i in 1..=6 {
            put_queue_header(&conn, MetadataScope::AgentCapturePending, &queue_header(i)).await;
        }
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "UPDATE metadata_kv SET value = X'707269766174652d626c6f62' WHERE key IN
             ('00000000-0000-4000-8000-000000000002','00000000-0000-4000-8000-000000000004')"
                .to_owned(),
        ))
        .await
        .unwrap();
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.quarantined, 2);
        assert_eq!(
            batch
                .headers
                .iter()
                .map(|h| h.binding.checkpoint_id.clone())
                .collect::<Vec<_>>(),
            [1, 3, 5].map(|i| queue_header(i).binding.checkpoint_id)
        );
        txn.commit().await.unwrap();
        let retained = conn
            .query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT typeof(value) AS t, hex(value) AS v FROM metadata_kv
             WHERE scope = 'agent_capture_quarantine' ORDER BY key"
                    .to_owned(),
            ))
            .await
            .unwrap();
        assert_eq!(retained.len(), 2);
        for row in retained {
            assert_eq!(row.try_get_by::<String, _>("t").unwrap(), "blob");
            assert_eq!(
                row.try_get_by::<String, _>("v").unwrap(),
                "707269766174652D626C6F62"
            );
        }
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.headers.len(), 4);
        assert_eq!(batch.quarantined, 0);
        txn.commit().await.unwrap();
    }

    #[tokio::test]
    async fn pending_hint_ignores_manual_and_namespace_conflict_headers() {
        let (_dir, conn) = queue_fixture().await;
        let mut consumed = queue_header(101);
        consumed.manual_attempted = true;
        let conflicting = queue_header(102);
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &consumed).await;
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &conflicting).await;
        put_queue_header(&conn, MetadataScope::AgentCaptureQuarantine, &conflicting).await;
        assert!(
            !has_pending_hint(&conn, &queue_scope("opaque-repository"))
                .await
                .unwrap(),
            "consumed or namespace-conflicted headers cannot launch useful worker work"
        );
    }

    #[tokio::test]
    async fn pending_queue_over_capacity_is_bounded_fail_closed() {
        let (_dir, conn) = queue_fixture().await;
        for i in 1..=17 {
            put_queue_header(&conn, MetadataScope::AgentCapturePending, &queue_header(i)).await;
        }
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let err = match pending_candidates(&txn, "opaque-repository").await {
            Ok(_) => panic!("over-capacity queue must fail closed"),
            Err(err) => err,
        };
        assert_eq!(
            err.to_string(),
            "capture recovery capacity exceeds its safe limit; run `libra agent doctor`"
        );
        txn.rollback().await.unwrap();
        assert_eq!(
            headers_for_repo(&conn, "opaque-repository", 17)
                .await
                .unwrap()
                .len(),
            17
        );
    }

    #[tokio::test]
    async fn pending_queue_invalid_utf8_and_types_are_quarantined_without_hydration() {
        let (_dir, conn) = queue_fixture().await;
        conn.execute_unprepared(
            "INSERT INTO metadata_kv(scope,target,key,value,value_type,created_at,updated_at) VALUES
             ('agent_capture_pending','opaque-repository',CAST(X'21FF' AS TEXT),'bad','text','original-created','original-updated'),
             ('agent_capture_pending','opaque-repository','!value',CAST(X'FF' AS TEXT),'text','original-created','original-updated'),
             ('agent_capture_pending','opaque-repository',X'626C6F622D6B6579','bad','text','original-created','original-updated'),
             ('agent_capture_pending','opaque-repository','!blob',X'FF','text','original-created','original-updated'),
             ('agent_capture_pending','opaque-repository','!type','bad',7,'original-created','original-updated')"
        ).await.unwrap();
        let snapshot_sql = "SELECT hex(CAST(key AS BLOB)) AS k, hex(CAST(value AS BLOB)) AS v,
           typeof(key) AS kt, typeof(value) AS vt, value_type, created_at, updated_at
           FROM metadata_kv WHERE scope = ? ORDER BY key";
        async fn snapshot(
            conn: &sea_orm::DatabaseConnection,
            sql: &str,
            scope: &str,
        ) -> Vec<Vec<String>> {
            conn.query_all_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                sql,
                [scope.into()],
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                [
                    "k",
                    "v",
                    "kt",
                    "vt",
                    "value_type",
                    "created_at",
                    "updated_at",
                ]
                .into_iter()
                .map(|field| row.try_get_by::<String, _>(field).unwrap())
                .collect()
            })
            .collect()
        }
        let before = snapshot(&conn, snapshot_sql, "agent_capture_pending").await;
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.quarantined, 5);
        assert!(batch.headers.is_empty());
        txn.commit().await.unwrap();
        assert_eq!(
            snapshot(&conn, snapshot_sql, "agent_capture_quarantine").await,
            before
        );
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &queue_header(6)).await;
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.headers.len(), 1);
        assert_eq!(
            batch.headers[0].binding.checkpoint_id,
            queue_header(6).binding.checkpoint_id
        );
        assert_eq!(batch.quarantined, 0);
        txn.commit().await.unwrap();
    }

    #[tokio::test]
    async fn pending_queue_consumed_manual_headers_never_return_to_auto_candidates() {
        let (_dir, conn) = queue_fixture().await;
        for i in 1..=6 {
            let mut h = queue_header(i);
            // Simulates out-of-band reclassification of a consumed header.
            h.manual_attempted = i <= 5;
            put_queue_header(&conn, MetadataScope::AgentCapturePending, &h).await;
        }
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.quarantined, 5);
        assert!(batch.headers.is_empty());
        txn.commit().await.unwrap();
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let batch = pending_candidates(&txn, "opaque-repository").await.unwrap();
        assert_eq!(batch.headers.len(), 1);
        assert_eq!(
            batch.headers[0].binding.checkpoint_id,
            queue_header(6).binding.checkpoint_id
        );
        txn.commit().await.unwrap();
    }

    #[tokio::test]
    async fn pending_queue_header_moves_and_manual_claim_are_atomic_one_shot() {
        let (_dir, conn) = queue_fixture().await;
        let h = queue_header(1);
        put_queue_header(&conn, MetadataScope::AgentCapturePending, &h).await;
        MetadataKv::set_with_conn(
            &conn,
            MetadataScope::AgentCapturePendingChunk,
            "opaque-repository",
            &chunk_key(&h.binding.checkpoint_id, 0),
            "untouched-chunk-canary",
            MetadataValueType::Binary,
        )
        .await
        .unwrap();
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        assert!(claim_manual_attempt(&txn, &h).await.is_err());
        quarantine_header(&txn, &h).await.unwrap();
        quarantine_header(&txn, &h).await.unwrap();
        let claimed = claim_manual_attempt(&txn, &h).await.unwrap();
        assert!(claimed.manual_attempted);
        assert_eq!(
            serde_json::to_value(&claimed.binding).unwrap(),
            serde_json::to_value(&h.binding).unwrap()
        );
        assert!(claim_manual_attempt(&txn, &claimed).await.is_err());
        assert!(claim_manual_attempt(&txn, &h).await.is_err());
        txn.rollback().await.unwrap();
        assert!(
            has_pending_hint(&conn, &queue_scope("opaque-repository"))
                .await
                .unwrap()
        );
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        let mut stale = h.clone();
        stale.binding.marker_generation = "stale-generation".into();
        assert!(quarantine_header(&txn, &stale).await.is_err());
        assert!(claim_manual_attempt(&txn, &stale).await.is_err());
        quarantine_header(&txn, &h).await.unwrap();
        let claimed = claim_manual_attempt(&txn, &h).await.unwrap();
        txn.commit().await.unwrap();
        assert!(
            !has_pending_hint(&conn, &queue_scope("opaque-repository"))
                .await
                .unwrap()
        );
        let rows = headers_for_repo(&conn, "opaque-repository", 17)
            .await
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].scope, "agent_capture_quarantine");
        assert_eq!(rows[0].value, serde_json::to_string(&claimed).unwrap());
        let chunk = MetadataKv::get_with_conn(
            &conn,
            MetadataScope::AgentCapturePendingChunk,
            "opaque-repository",
            &chunk_key(&h.binding.checkpoint_id, 0),
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(chunk.value, "untouched-chunk-canary");
        let txn = crate::internal::db::begin_write_transaction(&conn)
            .await
            .unwrap();
        assert!(claim_manual_attempt(&txn, &h).await.is_err());
        txn.rollback().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_artifact_contains_only_redacted_content() {
        let f = Fixture::new().await;
        let (artifact, payload) = f.artifact(None).await;
        let body: serde_json::Value = serde_json::from_slice(&artifact.bytes).unwrap();
        let transcript = STANDARD
            .decode(body["payload"]["transcript"].as_str().unwrap())
            .unwrap();
        assert_eq!(transcript, payload.transcript().bytes());
        assert!(!String::from_utf8_lossy(&transcript).contains("AKIAABCDEFGHIJKLMNOP"));
        for canary in [
            PK,
            NATIVE,
            f.root.path().to_str().unwrap(),
            "excluded provider source locator",
        ] {
            assert!(!String::from_utf8_lossy(&artifact.bytes).contains(canary));
        }
        assert_eq!(
            body["payload"]["lifecycle"][0]["tool_input"]["cwd"],
            "nested user data"
        );
        assert_eq!(
            body["payload"]["lifecycle"][0]["tool_input"]["session_id"],
            "user data"
        );
        assert!(body["coverage"].get("session_id").is_none());
        assert_eq!(body["binding"]["session_id"], artifact.identity.alias());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sidecar_capacity_is_checked_before_envelope_allocation() {
        let f = Fixture::new().await;
        let (_, payload) = f.artifact(None).await;
        assert!(check_sidecar_capacity(&payload, MAX_SIDECAR_BYTES).is_err());
        assert!(validate_envelope_shape(&[b'x'; 0]).is_err());
        assert!(validate_envelope_shape(&vec![b'x'; MAX_ENVELOPE_BYTES + 1]).is_err());
        let deep = format!("{}0{}", "[".repeat(67), "]".repeat(67));
        assert!(validate_envelope_shape(deep.as_bytes()).is_err());
        assert!(validate_envelope_shape(br#"{"text":"[\\\"}]"}"#).is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn artifact_transaction_rolls_back_and_idempotency_preserves_manual_state() {
        let f = Fixture::new().await;
        let (artifact, _) = f.artifact(None).await;
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        artifact.persist(&txn, deadline()).await.unwrap();
        txn.rollback().await.unwrap();
        for scope in [
            MetadataScope::AgentCapturePending,
            MetadataScope::AgentCapturePendingChunk,
            MetadataScope::AgentCaptureSessionAlias,
        ] {
            assert_eq!(f.count(scope).await, 0);
        }
        f.persist(&artifact).await;
        let mut header = artifact.header.clone();
        header.manual_attempted = true;
        MetadataKv::set_with_conn(
            &f.db,
            MetadataScope::AgentCapturePending,
            &f.scope.repo_id,
            &header.binding.checkpoint_id,
            &serde_json::to_string(&header).unwrap(),
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        f.persist(&artifact).await;
        let entries = headers_for_repo(&f.db, &f.scope.repo_id, 17).await.unwrap();
        assert_eq!(entries.len(), 1);
        assert!(decode_entry(&entries[0]).unwrap().manual_attempted);
        assert_eq!(f.count(MetadataScope::AgentCaptureSessionAlias).await, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_artifact_round_trip_without_provider_source() {
        let f = Fixture::new().await;
        let provider_source = f.root.path().join("provider-source.jsonl");
        let (artifact, original) = f.artifact(None).await;
        f.persist(&artifact).await;
        let alias = artifact.identity.alias().to_owned();
        let binding = artifact.binding().clone();
        let header = artifact.header.clone();
        drop(artifact);
        std::fs::remove_file(provider_source).unwrap();
        f.db.execute_unprepared("VACUUM").await.unwrap();
        let identity = f.identity(Some(&alias)).await;
        let verified = load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            &binding,
            &header,
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(verified.coverage.session_id, PK);
        assert_ne!(verified.coverage.session_id, alias);
        assert_eq!(verified.coverage.checkpoint_id, binding.checkpoint_id);
        assert_eq!(verified.coverage.claims[0].fence_token, 1);
        assert_eq!(
            verified.payload.transcript().bytes(),
            original.transcript().bytes()
        );
        assert_eq!(
            verified.payload.metadata_json().bytes(),
            original.metadata_json().bytes()
        );
        assert_eq!(
            verified.payload.lifecycle_events_jsonl().bytes(),
            original.lifecycle_events_jsonl().bytes()
        );
        assert_eq!(
            verified.payload.redaction_report_json().bytes(),
            original.redaction_report_json().bytes()
        );
        crate::internal::ai::coverage_gate::verify_reserved_live_claims_with_conn(
            &f.db,
            &f.scope,
            PK,
            &verified.coverage.owner,
            &verified.coverage.claims,
        )
        .await
        .unwrap();

        let mut bad = header.clone();
        bad.mac = format!("pending-envelope/hmac-v1/{}", "0".repeat(64));
        assert!(
            load_verified_payload(
                &f.db,
                &f.storage,
                f.root.path(),
                &identity,
                &binding,
                &bad,
                deadline()
            )
            .await
            .is_err()
        );
        MetadataKv::set_with_conn(
            &f.db,
            MetadataScope::AgentCapturePendingChunk,
            &f.scope.repo_id,
            &chunk_key(&binding.checkpoint_id, 0),
            &STANDARD.encode(b"canary-unauthenticated"),
            MetadataValueType::Binary,
        )
        .await
        .unwrap();
        let error = load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            &binding,
            &header,
            deadline(),
        )
        .await
        .err()
        .unwrap()
        .to_string();
        assert!(!error.contains("canary-unauthenticated"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn replay_resolves_alias_after_vacuum_and_source_removal() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(false, false, false, false)
            .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exhausted_artifact_allows_one_audited_replay_without_rewriting_receipt() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, false, false, false)
            .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attempt_capped_artifact_allows_one_audited_replay_without_rewriting_receipt() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, true, false, false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn doctor_replays_expired_authenticated_artifact_once() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, false, true, false).await;
    }

    #[cfg(unix)]
    async fn replay_resolves_alias_after_vacuum_and_source_removal_inner(
        manual: bool,
        exhaust_attempts: bool,
        through_doctor: bool,
        reasoning_v2: bool,
    ) {
        async fn claim_snapshot(f: &Fixture) -> String {
            f.db.query_one_raw(Statement::from_sql_and_values(
                f.db.get_database_backend(),
                "SELECT json_object('state',state,'revision',revision,'owner',owner,
                    'fence_token',fence_token,'lease_expires_at',lease_expires_at,
                    'attempt_checkpoint_id',attempt_checkpoint_id,'checkpoint_id',checkpoint_id,
                    'traces_commit',traces_commit) AS snapshot
                 FROM agent_coverage_claim WHERE session_id = ? AND logical_turn_key = 'turn-1'",
                [PK.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by::<String, _>("snapshot")
            .unwrap()
        }

        let f = Fixture::new().await;
        let event_id = Uuid::new_v4();
        let receipt_key = format!("capture-dedup-v2:{}", "a".repeat(64));
        let action = crate::internal::ai::capture::catalog::CaptureCatalogAction::from_ingress(
            event_id,
            Some(&receipt_key),
            LifecycleEventKind::SessionEnd,
        )
        .unwrap();
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                event_id,
                crate::internal::ai::capture::state::CheckpointWrite::Committed,
            );
        let marker_generation = Uuid::new_v4().to_string();
        let (mut artifact, _) = f
            .artifact_with_terminal_binding(
                None,
                Some(TerminalArtifactBinding {
                    event_id,
                    checkpoint_id: checkpoint_id.clone(),
                    action_key: action.action_key().to_string(),
                    receipt_key: receipt_key.clone(),
                    marker_generation: marker_generation.clone(),
                    parent_commit: None,
                    reserved_revision: 2,
                }),
            )
            .await;
        if reasoning_v2 {
            let bytes = synthetic_reasoning_v2_bytes(&artifact);
            artifact.header.mac = f
                .scope
                .sign_pending_envelope_until(&f.db, &f.storage, f.root.path(), &bytes, deadline())
                .await
                .unwrap();
            artifact.header.envelope_bytes = bytes.len();
            artifact.header.chunks = bytes.len().div_ceil(CHUNK_BYTES);
            artifact.bytes = bytes;
        }
        let binding = artifact.binding().clone();
        let alias = artifact.identity.alias().to_owned();
        f.persist(&artifact).await;
        let expected_v2_bytes = if reasoning_v2 {
            let error = load_verified_payload(
                &f.db,
                &f.storage,
                f.root.path(),
                &artifact.identity,
                &binding,
                &artifact.header,
                deadline(),
            )
            .await
            .err()
            .expect("signed terminal-bound v2 must fail at the current reader");
            assert!(
                error
                    .to_string()
                    .contains("capture recovery membership authentication failed")
            );
            Some(artifact.bytes.clone())
        } else {
            None
        };

        let active = crate::internal::ai::capture::state::DurableCaptureState {
            phase: crate::internal::ai::capture::state::CapturePhase::Active,
            stopped_at: None,
            sync_revision: 1,
        };
        let action_plan = crate::internal::ai::capture::state::reduce_lifecycle(
            crate::internal::ai::capture::state::LifecycleReducerInput {
                current: Some(active),
                event_kind: LifecycleEventKind::SessionEnd,
                event_id,
                occurred_at: 1_700_000_000,
                deadline: None,
            },
        )
        .unwrap();
        let mutation = crate::internal::ai::capture::catalog::CaptureCatalogMutation::from_reducer(
            Some(active),
            &action_plan,
            1_700_000_000,
        )
        .unwrap();
        let session = crate::internal::ai::capture::catalog::CaptureCatalogSession::new(
            PK,
            "claude_code",
            NATIVE,
            f.root.path().to_string_lossy(),
        )
        .unwrap();
        let apply = crate::internal::ai::capture::catalog::CaptureCatalogApplyRequest::new(
            f.scope.clone(),
            session,
            action.clone(),
            mutation,
        )
        .unwrap();
        let catalog = crate::internal::ai::capture::catalog::CaptureCatalogStore::new(f.db.clone());
        catalog.apply(&apply).await.unwrap();
        let policy = crate::internal::ai::capture::finalizer::CaptureFinalizePolicy::new(
            None,
            crate::internal::ai::capture::finalizer::CaptureFinalizeMode::Deferrable,
            action.action_key(),
        )
        .unwrap();
        let finalizer =
            crate::internal::ai::capture::catalog::CaptureCatalogFinalizeRequest::from_apply(
                &apply,
                policy,
                marker_generation.clone(),
                Some(binding.source_commitment.clone()),
                1,
                crate::internal::ai::capture::finalizer::FinalizeCheckpointProgress::NotStarted,
            )
            .unwrap();
        assert!(matches!(
            catalog.finalize(&finalizer).await.unwrap(),
            crate::internal::ai::capture::catalog::CaptureCatalogFinalizeResult::Pending { .. }
        ));
        if exhaust_attempts {
            for attempt in 2..=crate::internal::ai::capture::finalizer::MAX_FINALIZE_ATTEMPTS + 1 {
                let retry =
                    crate::internal::ai::capture::catalog::CaptureCatalogFinalizeRequest::from_apply(
                        &apply,
                        crate::internal::ai::capture::finalizer::CaptureFinalizePolicy::new(
                            None,
                            crate::internal::ai::capture::finalizer::CaptureFinalizeMode::Deferrable,
                            action.action_key(),
                        )
                        .unwrap(),
                        binding.marker_generation.clone(),
                        Some(binding.source_commitment.clone()),
                        i64::from(attempt),
                        crate::internal::ai::capture::finalizer::FinalizeCheckpointProgress::NotStarted,
                    )
                    .unwrap();
                let result = catalog.finalize(&retry).await.unwrap();
                if attempt == crate::internal::ai::capture::finalizer::MAX_FINALIZE_ATTEMPTS + 1 {
                    assert!(matches!(
                        result,
                        crate::internal::ai::capture::catalog::CaptureCatalogFinalizeResult::Quarantined { .. }
                    ));
                } else {
                    assert!(matches!(
                        result,
                        crate::internal::ai::capture::catalog::CaptureCatalogFinalizeResult::Pending { .. }
                    ));
                }
            }
        }

        let now_millis = if exhaust_attempts {
            1 + i64::from(crate::internal::ai::capture::finalizer::MAX_FINALIZE_ATTEMPTS)
        } else if manual {
            1 + crate::internal::ai::capture::finalizer::MAX_FINALIZE_WINDOW_MILLIS + 1
        } else {
            2
        };
        let recovery = catalog
            .pending_finalizer_recoveries_for_doctor(now_millis)
            .await
            .unwrap()
            .recoveries
            .into_iter()
            .find(|recovery| recovery.checkpoint_id() == checkpoint_id)
            .unwrap();
        assert_eq!(recovery.budget_exhausted(), manual);
        assert!(recovery.artifact_present());
        let before_repair =
            f.db.query_one_raw(Statement::from_sql_and_values(
                f.db.get_database_backend(),
                "SELECT metadata_json, sync_revision FROM agent_session WHERE session_id = ?",
                [PK.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        let before_metadata = before_repair
            .try_get_by::<String, _>("metadata_json")
            .unwrap();
        let before_revision = before_repair.try_get_by::<i64, _>("sync_revision").unwrap();
        if manual && !through_doctor {
            assert_eq!(
                catalog
                    .quarantine_exhausted_pending_finalizer(&recovery, now_millis)
                    .await
                    .unwrap(),
                crate::internal::ai::capture::catalog::CaptureCatalogFinalizerRecoveryResult::Quarantined
            );
            let after_quarantine =
                f.db.query_one_raw(Statement::from_sql_and_values(
                    f.db.get_database_backend(),
                    "SELECT metadata_json, sync_revision FROM agent_session WHERE session_id = ?",
                    [PK.into()],
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(
                after_quarantine
                    .try_get_by::<String, _>("metadata_json")
                    .unwrap(),
                before_metadata
            );
            assert_eq!(
                after_quarantine
                    .try_get_by::<i64, _>("sync_revision")
                    .unwrap(),
                before_revision
            );
            assert_eq!(
                crate::internal::ai::capture::pending::header_for_checkpoint(
                    &f.db,
                    &f.scope.repo_id,
                    &checkpoint_id,
                )
                .await
                .unwrap()
                .unwrap()
                .1,
                MetadataScope::AgentCaptureQuarantine
            );
        }
        drop(artifact);

        std::fs::remove_file(f.root.path().join("provider-source.jsonl")).unwrap();
        f.db.execute_unprepared("VACUUM").await.unwrap();
        let helper = crate::utils::test::real_libra_helper_binary();
        assert!(helper.is_file(), "built Libra helper is unavailable");
        // The proof is that one audited replay completes. The full lib suite
        // can starve the checkpoint write until a 10 s budget elapses before
        // the write begins, so the harness budget is wider than that floor.
        let deadline = crate::internal::ai::capture_scope::CaptureCommitDeadline::from_budget(
            Duration::from_secs(60),
        )
        .unwrap();
        let retained_v2 = if reasoning_v2 {
            let header = header_for_checkpoint(&f.db, &f.scope.repo_id, &checkpoint_id)
                .await
                .unwrap()
                .expect("signed v2 recovery artifact remains indexed");
            assert!(!header.0.manual_attempted);
            let chunks = load_chunks(&f.db, &header.0).await.unwrap();
            assert_eq!(Some(chunks.as_slice()), expected_v2_bytes.as_deref());
            let pending_count = f.count(MetadataScope::AgentCapturePending).await;
            let quarantine_count = f.count(MetadataScope::AgentCaptureQuarantine).await;
            assert_eq!(pending_count, usize::from(!manual || through_doctor));
            assert_eq!(quarantine_count, usize::from(manual && !through_doctor));
            Some((
                chunks,
                header,
                f.count(MetadataScope::AgentCaptureSessionAlias).await,
                claim_snapshot(&f).await,
                pending_count,
                quarantine_count,
            ))
        } else {
            None
        };
        if through_doctor {
            let _cwd = crate::utils::test::ChangeDirGuard::new(f.root.path());
            let report = crate::internal::ai::authorized_read::with_test_helper_program(
                helper,
                crate::command::agent::scan_checkpoint_store_with_replay_budget_for_test(
                    &f.db,
                    true,
                    true,
                    Duration::from_secs(30),
                ),
            )
            .await
            .unwrap();
            let expired_finding = report["findings"]
                .as_array()
                .and_then(|findings| {
                    findings
                        .iter()
                        .find(|finding| finding["inconsistency_type"] == "expired_inflight_marker")
                })
                .expect("doctor classifies the authenticated expired finalizer");
            if let Some((before_chunks, before_header, before_aliases, before_claim, _, _)) =
                retained_v2.as_ref()
            {
                assert_eq!(expired_finding["repaired"], false);
                assert_eq!(expired_finding["manual_required"], true);
                let detail = expired_finding["detail"]
                    .as_str()
                    .expect("doctor finding has operator-facing detail");
                assert!(detail.contains(
                    "authenticated artifact replay was refused or failed closed; manual recovery is required"
                ));
                assert!(!detail.contains("OG17_PRIVATE_PENDING_V2_CANARY"));
                let after_header = header_for_checkpoint(&f.db, &f.scope.repo_id, &checkpoint_id)
                    .await
                    .unwrap()
                    .expect("doctor must retain signed v2 header");
                assert!(
                    after_header.0 == before_header.0,
                    "doctor changed signed v2 header"
                );
                assert_eq!(after_header.1, MetadataScope::AgentCaptureQuarantine);
                assert!(!after_header.0.manual_attempted);
                assert_eq!(
                    load_chunks(&f.db, &after_header.0).await.unwrap(),
                    *before_chunks
                );
                assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 0);
                assert_eq!(f.count(MetadataScope::AgentCaptureQuarantine).await, 1);
                assert_eq!(
                    f.count(MetadataScope::AgentCaptureSessionAlias).await,
                    *before_aliases
                );
                assert_eq!(&claim_snapshot(&f).await, before_claim);
                let after_session = f
                    .db
                    .query_one_raw(Statement::from_sql_and_values(
                        f.db.get_database_backend(),
                        "SELECT metadata_json, sync_revision, state FROM agent_session WHERE session_id = ?",
                        [PK.into()],
                    ))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    after_session
                        .try_get_by::<String, _>("metadata_json")
                        .unwrap(),
                    before_metadata
                );
                assert_eq!(
                    after_session.try_get_by::<i64, _>("sync_revision").unwrap(),
                    before_revision
                );
                assert_eq!(
                    after_session.try_get_by::<String, _>("state").unwrap(),
                    "active"
                );
                return;
            }
            assert!(
                expired_finding["repaired"].as_bool().unwrap_or(false),
                "doctor did not repair the expired authenticated finalizer: {expired_finding}"
            );
        } else {
            let result = crate::internal::ai::authorized_read::with_test_helper_program(
                helper,
                crate::internal::ai::capture::recovery::replay_for_doctor(
                    crate::internal::ai::capture::recovery::DoctorReplayRequest {
                        conn: &f.db,
                        recovery: &recovery,
                        storage: &f.storage,
                        root: f.root.path(),
                        repo_path: &f.storage,
                        now_millis,
                        deadline,
                        manual,
                    },
                ),
            )
            .await
            .unwrap();
            if let Some((
                before_chunks,
                before_header,
                before_aliases,
                before_claim,
                pending_count,
                quarantine_count,
            )) = retained_v2
            {
                assert_eq!(
                    result,
                    crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::ManualRequired,
                    "inconsistent synthetic v2 membership remains closed"
                );
                let after_header = header_for_checkpoint(&f.db, &f.scope.repo_id, &checkpoint_id)
                    .await
                    .unwrap()
                    .expect("driver must retain signed v2 header");
                assert!(
                    after_header == before_header,
                    "driver changed signed v2 header"
                );
                assert!(!after_header.0.manual_attempted);
                assert_eq!(
                    load_chunks(&f.db, &after_header.0).await.unwrap(),
                    before_chunks
                );
                assert_eq!(
                    f.count(MetadataScope::AgentCaptureSessionAlias).await,
                    before_aliases
                );
                assert_eq!(
                    f.count(MetadataScope::AgentCapturePending).await,
                    pending_count
                );
                assert_eq!(
                    f.count(MetadataScope::AgentCaptureQuarantine).await,
                    quarantine_count
                );
                assert_eq!(claim_snapshot(&f).await, before_claim);
                let after_session = f
                    .db
                    .query_one_raw(Statement::from_sql_and_values(
                        f.db.get_database_backend(),
                        "SELECT metadata_json, sync_revision, state FROM agent_session WHERE session_id = ?",
                        [PK.into()],
                    ))
                    .await
                    .unwrap()
                    .unwrap();
                assert_eq!(
                    after_session
                        .try_get_by::<String, _>("metadata_json")
                        .unwrap(),
                    before_metadata,
                    "driver must retain terminal receipt ledger"
                );
                assert_eq!(
                    after_session.try_get_by::<i64, _>("sync_revision").unwrap(),
                    before_revision
                );
                assert_eq!(
                    after_session.try_get_by::<String, _>("state").unwrap(),
                    "active"
                );
                return;
            }
            assert_eq!(
                result,
                crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::Completed
            );
        }
        let row =
            f.db.query_one_raw(Statement::from_sql_and_values(
                f.db.get_database_backend(),
                "SELECT state, stopped_at, sync_revision FROM agent_session WHERE session_id = ?",
                [PK.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by::<String, _>("state").unwrap(), "stopped");
        assert_eq!(
            row.try_get_by::<Option<i64>, _>("stopped_at").unwrap(),
            Some(1_700_000_000)
        );
        assert_eq!(row.try_get_by::<i64, _>("sync_revision").unwrap(), 3);
        assert!(
            crate::internal::ai::capture::pending::header_for_checkpoint(
                &f.db,
                &f.scope.repo_id,
                &checkpoint_id,
            )
            .await
            .unwrap()
            .is_none(),
            "successful strict completion removes the owned recovery artifact"
        );
        assert_ne!(alias, PK);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_reads_reject_corrupt_oversized_header_without_echo() {
        let f = Fixture::new().await;
        let canary = format!("canary-{}", "x".repeat(MAX_HEADER_BYTES));
        MetadataKv::set_with_conn(
            &f.db,
            MetadataScope::AgentCapturePending,
            &f.scope.repo_id,
            "bad",
            &canary,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let error = gc_parent_roots(&f.db).await.unwrap_err().to_string();
        assert!(!error.contains("canary-"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_snapshot_survives_marker_expiry_and_gc() {
        use std::{collections::HashSet, sync::Arc};

        use git_internal::{
            hash::HashKind,
            internal::object::{
                ObjectTrait,
                blob::Blob,
                commit::Commit,
                signature::Signature,
                tree::{Tree, TreeItem, TreeItemMode},
            },
        };

        use crate::{
            internal::ai::{
                history::{
                    HistoryManager, list_all_traces_inflight_markers,
                    list_live_traces_inflight_markers,
                },
                traces::{TracesInflightMarker, write_traces_inflight_marker},
            },
            utils::{client_storage::ClientStorage, storage::local::LocalStorage},
        };

        const CHILD: &str = "LIBRA_ACF10_MARKER_GC_FIXTURE_CHILD";
        const COMPLETE: &str = "ACF10_MARKER_GC_CHILD_ASSERTIONS_COMPLETE";
        if std::env::var(CHILD).as_deref() != Ok("1") {
            // Object GC loads commits/trees from the process cwd. Run this
            // fixture in an isolated child instead of moving the shared test
            // process's cwd, environment, or hash-kind state.
            let root = tempfile::Builder::new()
                .prefix("libra-acf10-marker-gc-")
                .tempdir()
                .unwrap();
            let mut command = tokio::process::Command::new(std::env::current_exe().unwrap());
            command
                .env_clear()
                .args([
                    "--exact",
                    "internal::ai::capture::pending::tests::pending_snapshot_survives_marker_expiry_and_gc",
                    "--nocapture",
                ])
                .current_dir(root.path())
                .env(CHILD, "1")
                .env("LIBRA_TEST", "1")
                .env("TMPDIR", root.path())
                .env("TMP", root.path())
                .env("TEMP", root.path())
                .env(
                    "LIBRA_CONFIG_GLOBAL_DB",
                    root.path().join("global-config.db"),
                )
                .env(
                    "LIBRA_CONFIG_SYSTEM_DB",
                    root.path().join("system-config.db"),
                )
                .kill_on_drop(true);
            // Keep cargo's enlarged test stack; debug async fixtures need it.
            if let Some(stack) = std::env::var_os("RUST_MIN_STACK") {
                command.env("RUST_MIN_STACK", stack);
            }
            let output = tokio::time::timeout(Duration::from_secs(120), command.output())
                .await
                .expect("isolated marker/GC fixture must finish within its test timeout")
                .unwrap();
            assert!(
                output.status.success(),
                "isolated marker/GC fixture failed:\n{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            assert!(
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .any(|line| line == COMPLETE),
                "a successful child exit without the final assertion receipt is not GC evidence"
            );
            return;
        }

        let root = std::env::current_dir().unwrap();
        assert!(
            root.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("libra-acf10-marker-gc-")
        );
        let f = Fixture::new_at(FixtureRoot::ChildCwd(root)).await;
        let objects = f.storage.join("objects");
        let storage = ClientStorage::try_init(objects.clone()).expect("storage");
        let blob =
            Blob::from_content_with_kind(HashKind::Sha1, "parent graph held by recovery").unwrap();
        let canary =
            Blob::from_content_with_kind(HashKind::Sha1, "unrooted object stays collectable")
                .unwrap();
        let tree = Tree::from_tree_items_with_kind(
            HashKind::Sha1,
            vec![TreeItem::new(
                TreeItemMode::Blob,
                blob.id,
                "parent.txt".into(),
            )],
        )
        .unwrap();
        let author =
            Signature::from_data(b"author t <t@example.com> 1000000000 +0000".to_vec()).unwrap();
        let committer =
            Signature::from_data(b"committer t <t@example.com> 1000000000 +0000".to_vec()).unwrap();
        let parent = Commit::new_with_kind(
            HashKind::Sha1,
            author,
            committer,
            tree.id,
            vec![],
            "recovery parent",
        )
        .unwrap();
        for blob in [&blob, &canary] {
            storage
                .put(&blob.id, &blob.to_data().unwrap(), blob.get_type())
                .unwrap();
        }
        storage
            .put(&tree.id, &tree.to_data().unwrap(), tree.get_type())
            .unwrap();
        storage
            .put(&parent.id, &parent.to_data().unwrap(), parent.get_type())
            .unwrap();

        // A real writer marker for this exact artifact checkpoint, already
        // past its TTL, with the generation the artifact binding retains.
        let checkpoint_id = Uuid::new_v4().to_string();
        let marker = TracesInflightMarker::new(PK, &checkpoint_id, 1);
        let (artifact, _) = f
            .artifact_with_terminal_binding(
                None,
                Some(TerminalArtifactBinding {
                    event_id: Uuid::new_v4(),
                    checkpoint_id: checkpoint_id.clone(),
                    action_key: "action-1".into(),
                    receipt_key: "receipt-1".into(),
                    marker_generation: marker.generation.clone().unwrap(),
                    parent_commit: Some(parent.id.to_string()),
                    reserved_revision: 1,
                }),
            )
            .await;
        f.persist(&artifact).await;
        async fn private_rows(f: &Fixture) -> Vec<Vec<String>> {
            f.db.query_all_raw(Statement::from_string(
                f.db.get_database_backend(),
                "SELECT scope, target, key, hex(CAST(value AS BLOB)) AS v, value_type,
                   created_at, updated_at FROM metadata_kv WHERE scope IN (
                   'agent_capture_pending', 'agent_capture_quarantine',
                   'agent_capture_pending_chunk', 'agent_capture_session_alias')
                 ORDER BY scope, key"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                [
                    "scope",
                    "target",
                    "key",
                    "v",
                    "value_type",
                    "created_at",
                    "updated_at",
                ]
                .into_iter()
                .map(|field| row.try_get_by::<String, _>(field).unwrap())
                .collect()
            })
            .collect()
        }
        let before = private_rows(&f).await;
        assert_eq!(
            before.len(),
            2 + artifact.header.chunks,
            "header, its chunks and the alias"
        );
        write_traces_inflight_marker(&f.db, &marker).await.unwrap();
        let now = chrono::Utc::now().timestamp_millis();
        assert!(
            list_live_traces_inflight_markers(&f.db, now)
                .await
                .unwrap()
                .is_empty(),
            "the marker is expired, so it no longer protects the attempt"
        );
        assert_eq!(
            list_all_traces_inflight_markers(&f.db).await.unwrap().len(),
            1
        );

        // Production doctor expired-marker retirement.
        let history = HistoryManager::new_with_ref(
            Arc::new(ClientStorage::from_test_storage(
                Arc::new(LocalStorage::try_new(objects.clone()).expect("storage")),
                objects,
            )),
            f.storage.clone(),
            Arc::new(f.db.clone()),
            "libra/traces",
        );
        assert!(
            history
                .repair_expired_traces_inflight_marker_for_test(PK, &checkpoint_id, now)
                .await
                .unwrap(),
            "doctor must retire the expired marker"
        );
        assert!(
            list_all_traces_inflight_markers(&f.db)
                .await
                .unwrap()
                .is_empty()
        );
        assert_eq!(private_rows(&f).await, before);

        // Production object-GC root collection: with no refs, the artifact's
        // parent proof alone keeps that graph alive; the canary is collectable.
        let reachable =
            crate::command::maintenance::collect_reachable_objects_with_conn(&storage, &f.db)
                .await
                .unwrap();
        assert_eq!(reachable, HashSet::from([parent.id, tree.id, blob.id]));
        assert!(!reachable.contains(&canary.id));
        assert_eq!(private_rows(&f).await, before);
        assert_eq!(
            gc_parent_roots(&f.db).await.unwrap(),
            vec![parent.id.to_string()]
        );
        assert_eq!(
            load_chunks(&f.db, &artifact.header).await.unwrap(),
            artifact.bytes
        );
        println!("\n{COMPLETE}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn retained_quarantine_counts_towards_capacity_and_gc_fails_closed() {
        let f = Fixture::new().await;
        let (artifact, _) = f.artifact(None).await;
        for _ in 0..MAX_ARTIFACTS {
            let mut header = artifact.header.clone();
            header.binding.checkpoint_id = Uuid::new_v4().to_string();
            MetadataKv::set_with_conn(
                &f.db,
                MetadataScope::AgentCaptureQuarantine,
                &f.scope.repo_id,
                &header.binding.checkpoint_id,
                &serde_json::to_string(&header).unwrap(),
                MetadataValueType::Text,
            )
            .await
            .unwrap();
        }
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        assert!(artifact.persist(&txn, deadline()).await.is_err());
        txn.rollback().await.unwrap();
        assert_eq!(gc_parent_roots(&f.db).await.unwrap().len(), MAX_ARTIFACTS);
        let header = artifact.header.clone();
        MetadataKv::set_with_conn(
            &f.db,
            MetadataScope::AgentCaptureQuarantine,
            &f.scope.repo_id,
            &header.binding.checkpoint_id,
            &serde_json::to_string(&header).unwrap(),
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        assert!(gc_parent_roots(&f.db).await.is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn completion_cleans_only_unreferenced_alias_in_the_same_transaction() {
        let f = Fixture::new().await;
        let (first, _) = f.artifact(None).await;
        f.persist(&first).await;
        let (second, _) = f.artifact(Some(first.identity.alias())).await;
        f.persist(&second).await;
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        remove_artifact(
            &txn,
            &f.scope.repo_id,
            &first.binding().checkpoint_id,
            &[first.identity.alias().to_owned()],
        )
        .await
        .unwrap();
        txn.rollback().await.unwrap();
        assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 2);
        assert_eq!(f.count(MetadataScope::AgentCaptureSessionAlias).await, 1);
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        remove_artifact(
            &txn,
            &f.scope.repo_id,
            &first.binding().checkpoint_id,
            &[first.identity.alias().to_owned()],
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        assert_eq!(f.count(MetadataScope::AgentCaptureSessionAlias).await, 1);
        assert_eq!(
            load_chunks(&f.db, &second.header).await.unwrap(),
            second.bytes
        );
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        remove_artifact(
            &txn,
            &f.scope.repo_id,
            &second.binding().checkpoint_id,
            &[second.identity.alias().to_owned()],
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        for scope in [
            MetadataScope::AgentCapturePending,
            MetadataScope::AgentCapturePendingChunk,
            MetadataScope::AgentCaptureSessionAlias,
        ] {
            assert_eq!(f.count(scope).await, 0);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn malformed_utf8_foreign_evidence_does_not_block_completion_or_keyless_erase() {
        let f = Fixture::new().await;
        let (artifact, _) = f.artifact(None).await;
        f.persist(&artifact).await;
        f.db.execute_unprepared(
            "INSERT INTO metadata_kv(scope,target,key,value,value_type,created_at,updated_at) VALUES
             ('agent_capture_quarantine','opaque-repository',CAST(X'21FF' AS TEXT),'bad','text','original-created','original-updated'),
             ('agent_capture_quarantine','opaque-repository','foreign-header',CAST(X'FF' AS TEXT),'text','original-created','original-updated'),
             ('agent_capture_session_alias','opaque-repository','foreign-registry',CAST(X'FF' AS TEXT),'text','original-created','original-updated'),
             ('agent_capture_session_alias','opaque-repository',X'7265676973747279','bad',CAST(X'FF' AS TEXT),'original-created','original-updated')"
        ).await.unwrap();
        async fn foreign_snapshot(conn: &sea_orm::DatabaseConnection) -> Vec<Vec<String>> {
            conn.query_all_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT scope, hex(CAST(key AS BLOB)) AS k, hex(CAST(value AS BLOB)) AS v,
                  typeof(key) AS kt, typeof(value) AS vt, hex(CAST(value_type AS BLOB)) AS t,
                  created_at, updated_at FROM metadata_kv
                  WHERE key = CAST(X'21FF' AS TEXT) OR key IN ('foreign-header','foreign-registry')
                     OR key = X'7265676973747279' ORDER BY scope,key"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .into_iter()
            .map(|row| {
                [
                    "scope",
                    "k",
                    "v",
                    "kt",
                    "vt",
                    "t",
                    "created_at",
                    "updated_at",
                ]
                .into_iter()
                .map(|field| row.try_get_by::<String, _>(field).unwrap())
                .collect()
            })
            .collect()
        }
        let before = foreign_snapshot(&f.db).await;
        assert_eq!(before.len(), 4);
        let key_path = f
            .storage
            .join(key::CAPTURE_DEDUP_SECRET_DIR)
            .join(key::CAPTURE_DEDUP_SECRET_FILE);
        std::fs::remove_file(&key_path).unwrap();
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        remove_artifact(
            &txn,
            &f.scope.repo_id,
            &artifact.binding().checkpoint_id,
            &[artifact.identity.alias().to_owned()],
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        assert!(
            pending_identity::lookup(&f.db, &f.scope.repo_id, artifact.identity.alias())
                .await
                .unwrap()
                .is_some(),
            "completion conservatively retains alias when foreign ownership is unreadable"
        );
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        assert!(
            erase_session_artifacts(&txn, PK).await.unwrap(),
            "unassigned data requests a content-free postcommit note"
        );
        txn.commit().await.unwrap();
        assert!(
            pending_identity::lookup(&f.db, &f.scope.repo_id, artifact.identity.alias())
                .await
                .unwrap()
                .is_none()
        );
        assert_eq!(foreign_snapshot(&f.db).await, before);
        assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 0);
        assert_eq!(f.count(MetadataScope::AgentCapturePendingChunk).await, 0);
        assert!(!key_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn same_checkpoint_collision_never_deletes_unknown_or_foreign_evidence() {
        for foreign in [false, true] {
            for erase_directly in [false, true] {
                let f = Fixture::new().await;
                let (artifact, _) = f.artifact(None).await;
                f.persist(&artifact).await;
                let foreign_identity = if foreign {
                    const OTHER_PK: &str = "codex__foreign-session";
                    f.db.execute_raw(Statement::from_sql_and_values(f.db.get_database_backend(),
                        "INSERT INTO agent_session(session_id,agent_kind,provider_session_id,state,working_dir,
                          metadata_json,started_at,last_event_at,sync_revision,repo_id,worktree_id,scope_state)
                         VALUES(?, 'codex', 'foreign-session','active',?,'{}',1,1,1,?,'','scoped')",
                        [OTHER_PK.into(), f.root.path().to_string_lossy().into_owned().into(), f.scope.repo_id.clone().into()]))
                        .await.unwrap();
                    let txn = db::begin_write_transaction(&f.db).await.unwrap();
                    let context = resolve_pending_session_context(&txn, &f.scope, OTHER_PK)
                        .await
                        .unwrap();
                    txn.commit().await.unwrap();
                    let identity = PendingSessionAlias::prepare(
                        &f.db,
                        &context,
                        None,
                        &f.storage,
                        f.root.path(),
                        deadline(),
                    )
                    .await
                    .unwrap();
                    let mut h = artifact.header.clone();
                    h.binding.session_id = identity.alias().to_owned();
                    h.binding.checkpoint_id = Uuid::new_v4().to_string();
                    let txn = db::begin_write_transaction(&f.db).await.unwrap();
                    put_queue_header(&txn, MetadataScope::AgentCaptureQuarantine, &h).await;
                    identity
                        .publish_for_artifact(&txn, &h.binding.checkpoint_id)
                        .await
                        .unwrap();
                    txn.commit().await.unwrap();
                    // Out-of-band collision after both owners published valid
                    // associations. It grants no authority to replay B's data.
                    MetadataKv::unset_with_conn(
                        &f.db,
                        MetadataScope::AgentCaptureQuarantine,
                        &f.scope.repo_id,
                        &h.binding.checkpoint_id,
                    )
                    .await
                    .unwrap();
                    h.binding.checkpoint_id = artifact.binding().checkpoint_id.clone();
                    put_queue_header(&f.db, MetadataScope::AgentCaptureQuarantine, &h).await;
                    Some(identity)
                } else {
                    f.db.execute_raw(Statement::from_sql_and_values(f.db.get_database_backend(),
                        "INSERT INTO metadata_kv(scope,target,key,value,value_type,created_at,updated_at)
                         VALUES('agent_capture_quarantine',?,?,CAST(X'FF' AS TEXT),'text','original-created','original-updated')",
                        [f.scope.repo_id.clone().into(), artifact.binding().checkpoint_id.clone().into()]))
                        .await.unwrap();
                    None
                };
                async fn collision_snapshot(f: &Fixture, checkpoint: &str) -> Vec<String> {
                    let row = f.db.query_one_raw(Statement::from_sql_and_values(f.db.get_database_backend(),
                        "SELECT hex(CAST(value AS BLOB)) AS v, typeof(value) AS t, value_type, created_at,updated_at
                         FROM metadata_kv WHERE scope='agent_capture_quarantine' AND target=? AND key=?",
                        [f.scope.repo_id.clone().into(), checkpoint.into()])).await.unwrap().unwrap();
                    ["v", "t", "value_type", "created_at", "updated_at"]
                        .into_iter()
                        .map(|field| row.try_get_by::<String, _>(field).unwrap())
                        .collect()
                }
                let before = collision_snapshot(&f, &artifact.binding().checkpoint_id).await;
                let foreign_before = if let Some(identity) = &foreign_identity {
                    Some(
                        pending_identity::lookup(&f.db, &f.scope.repo_id, identity.alias())
                            .await
                            .unwrap()
                            .unwrap()
                            .encode()
                            .unwrap(),
                    )
                } else {
                    None
                };
                let key_path = f
                    .storage
                    .join(key::CAPTURE_DEDUP_SECRET_DIR)
                    .join(key::CAPTURE_DEDUP_SECRET_FILE);
                std::fs::remove_file(&key_path).unwrap();
                let txn = db::begin_write_transaction(&f.db).await.unwrap();
                if erase_directly {
                    assert!(erase_session_artifacts(&txn, PK).await.unwrap());
                } else {
                    assert!(
                        remove_artifact(
                            &txn,
                            &f.scope.repo_id,
                            &artifact.binding().checkpoint_id,
                            &[artifact.identity.alias().to_owned()]
                        )
                        .await
                        .unwrap()
                    );
                }
                txn.commit().await.unwrap();
                assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 0);
                assert_eq!(
                    load_chunks(&f.db, &artifact.header).await.unwrap(),
                    artifact.bytes
                );
                assert_eq!(
                    collision_snapshot(&f, &artifact.binding().checkpoint_id).await,
                    before
                );
                if let Some(identity) = &foreign_identity {
                    assert_eq!(
                        pending_identity::lookup(&f.db, &f.scope.repo_id, identity.alias())
                            .await
                            .unwrap()
                            .unwrap()
                            .encode()
                            .unwrap(),
                        foreign_before.unwrap()
                    );
                }
                let own_retained =
                    pending_identity::lookup(&f.db, &f.scope.repo_id, artifact.identity.alias())
                        .await
                        .unwrap()
                        .is_some();
                assert_eq!(
                    own_retained,
                    !erase_directly && !foreign,
                    "unknown ownership retains completion alias; explicit erase retires its own association only"
                );
                assert!(!key_path.exists());
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn erase_is_atomic_and_preserves_other_session_evidence() {
        const OTHER_PK: &str = "codex__foreign-session";
        // (foreign header decodes, foreign alias has a registry row). Only
        // well-formed, registered foreign evidence is retained silently; a
        // corrupt or orphaned foreign header still blocks persists and GC,
        // so erase reports its lost capacity without deleting it.
        for (decodable, registered) in [(true, true), (true, false), (false, true), (false, false)]
        {
            let f = Fixture::new().await;
            let (artifact, _) = f.artifact(None).await;
            f.persist(&artifact).await;
            // An independently attributable foreign header is never deletion
            // authority for this session, even if the body/MAC is unusable.
            let mut foreign = artifact.header.clone();
            foreign.binding.checkpoint_id = Uuid::new_v4().to_string();
            if registered {
                f.db.execute_raw(Statement::from_sql_and_values(f.db.get_database_backend(),
                    "INSERT INTO agent_session(session_id,agent_kind,provider_session_id,state,working_dir,
                      metadata_json,started_at,last_event_at,sync_revision,repo_id,worktree_id,scope_state)
                     VALUES(?, 'codex', 'foreign-session','active',?,'{}',1,1,1,?,'','scoped')",
                    [OTHER_PK.into(), f.root.path().to_string_lossy().into_owned().into(), f.scope.repo_id.clone().into()]))
                    .await.unwrap();
                let txn = db::begin_write_transaction(&f.db).await.unwrap();
                let context = resolve_pending_session_context(&txn, &f.scope, OTHER_PK)
                    .await
                    .unwrap();
                txn.commit().await.unwrap();
                let identity = PendingSessionAlias::prepare(
                    &f.db,
                    &context,
                    None,
                    &f.storage,
                    f.root.path(),
                    deadline(),
                )
                .await
                .unwrap();
                foreign.binding.session_id = identity.alias().to_owned();
                let txn = db::begin_write_transaction(&f.db).await.unwrap();
                put_queue_header(&txn, MetadataScope::AgentCaptureQuarantine, &foreign).await;
                identity
                    .publish_for_artifact(&txn, &foreign.binding.checkpoint_id)
                    .await
                    .unwrap();
                txn.commit().await.unwrap();
            } else {
                foreign.binding.session_id = Uuid::new_v4().to_string();
            }
            if !decodable {
                foreign.mac = "corrupt foreign MAC".into();
            }
            put_queue_header(&f.db, MetadataScope::AgentCaptureQuarantine, &foreign).await;
            assert_eq!(
                PendingHeader::decode(&serde_json::to_string(&foreign).unwrap()).is_ok(),
                decodable
            );
            // Ownership is independently readable even if other header fields
            // are corrupt. Neither key nor receipt ledger is needed to erase.
            let mut owned = artifact.header.clone();
            owned.mac = "corrupt owned MAC".into();
            put_queue_header(&f.db, MetadataScope::AgentCapturePending, &owned).await;
            let key_path = f
                .storage
                .join(key::CAPTURE_DEDUP_SECRET_DIR)
                .join(key::CAPTURE_DEDUP_SECRET_FILE);
            std::fs::remove_file(&key_path).unwrap();
            let lost_capacity = !(decodable && registered);
            let aliases = 1 + usize::from(registered);
            let txn = db::begin_write_transaction(&f.db).await.unwrap();
            assert_eq!(
                erase_session_artifacts(&txn, PK).await.unwrap(),
                lost_capacity,
                "decodable={decodable} registered={registered}"
            );
            txn.rollback().await.unwrap();
            assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 1);
            assert_eq!(
                f.count(MetadataScope::AgentCaptureSessionAlias).await,
                aliases
            );
            let txn = db::begin_write_transaction(&f.db).await.unwrap();
            assert_eq!(
                erase_session_artifacts(&txn, PK).await.unwrap(),
                lost_capacity
            );
            txn.commit().await.unwrap();
            assert!(!key_path.exists(), "keyless erase must not recreate a key");
            assert_eq!(
                f.count(MetadataScope::AgentCaptureSessionAlias).await,
                aliases - 1,
                "only this session's association is retired"
            );
            assert_eq!(f.count(MetadataScope::AgentCapturePendingChunk).await, 0);
            let entries = headers_for_repo(&f.db, &f.scope.repo_id, 17).await.unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].key, foreign.binding.checkpoint_id);
            assert_eq!(
                entries[0].value,
                serde_json::to_string(&foreign).unwrap(),
                "foreign evidence is retained byte-for-byte"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn unassigned_evidence_does_not_block_attributed_erase() {
        let f = Fixture::new().await;
        let (artifact, _) = f.artifact(None).await;
        f.persist(&artifact).await;
        let corrupt_checkpoint = Uuid::new_v4().to_string();
        let corrupt = format!("canary-unassigned-{}", "x".repeat(MAX_HEADER_BYTES));
        MetadataKv::set_with_conn(
            &f.db,
            MetadataScope::AgentCaptureQuarantine,
            &f.scope.repo_id,
            &corrupt_checkpoint,
            &corrupt,
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        assert!(
            erase_session_artifacts(&txn, PK).await.unwrap(),
            "retain and report lost capacity"
        );
        txn.commit().await.unwrap();
        assert_eq!(f.count(MetadataScope::AgentCaptureSessionAlias).await, 0);
        assert_eq!(f.count(MetadataScope::AgentCapturePendingChunk).await, 0);
        let retained = MetadataKv::get_with_conn(
            &f.db,
            MetadataScope::AgentCaptureQuarantine,
            &f.scope.repo_id,
            &corrupt_checkpoint,
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(retained.value, corrupt);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn history_erase_rejects_unreadable_or_mismatched_owned_incarnation() {
        use std::sync::Arc;

        use crate::{
            internal::ai::history::HistoryManager,
            utils::{client_storage::ClientStorage, storage::local::LocalStorage},
        };
        for metadata in [
            "canary-invalid-metadata".to_string(),
            "x".repeat(1024 * 1024 + 1),
            r#"{"capture_incarnation":"canary-invalid-incarnation"}"#.to_string(),
            format!(r#"{{"capture_incarnation":"{}"}}"#, "a".repeat(32)),
        ] {
            let f = Fixture::new().await;
            let (artifact, _) = f.artifact(None).await;
            f.persist(&artifact).await;
            f.db.execute_raw(Statement::from_sql_and_values(
                f.db.get_database_backend(),
                "UPDATE agent_session SET metadata_json = ? WHERE session_id = ?",
                [metadata.into(), PK.into()],
            ))
            .await
            .unwrap();
            let objects = f.storage.join("objects");
            let storage = Arc::new(ClientStorage::from_test_storage(
                Arc::new(LocalStorage::try_new(objects.clone()).expect("storage")),
                objects,
            ));
            let history = HistoryManager::new_with_ref(
                storage,
                f.storage.clone(),
                Arc::new(f.db.clone()),
                "libra/traces",
            );
            let error = history.erase_session_local(PK).await.err().unwrap();
            assert!(
                format!("{error:#}").contains("restore consistent session/alias catalog metadata")
            );
            assert!(!format!("{error:#}").contains("canary-"));
            let session =
                f.db.query_one_raw(Statement::from_sql_and_values(
                    f.db.get_database_backend(),
                    "SELECT 1 FROM agent_session WHERE session_id = ?",
                    [PK.into()],
                ))
                .await
                .unwrap();
            assert!(
                session.is_some(),
                "a refused erase must not delete the session row"
            );
            assert_eq!(f.count(MetadataScope::AgentCaptureSessionAlias).await, 1);
            assert_eq!(
                load_chunks(&f.db, &artifact.header).await.unwrap(),
                artifact.bytes
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn missing_session_ownership_fails_closed_but_unrelated_reerase_is_quiet() {
        let f = Fixture::new().await;
        let (artifact, _) = f.artifact(None).await;
        f.persist(&artifact).await;
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        assert!(
            !erase_session_artifacts(&txn, "already-erased-unrelated-session")
                .await
                .unwrap()
        );
        txn.rollback().await.unwrap();
        f.db.execute_raw(Statement::from_sql_and_values(
            f.db.get_database_backend(),
            "DELETE FROM agent_session WHERE session_id = ?",
            [PK.into()],
        ))
        .await
        .unwrap();
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        assert!(erase_session_artifacts(&txn, PK).await.is_err());
        txn.rollback().await.unwrap();
        assert_eq!(f.count(MetadataScope::AgentCaptureSessionAlias).await, 1);
        assert_eq!(
            load_chunks(&f.db, &artifact.header).await.unwrap(),
            artifact.bytes
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn lost_coverage_fence_cannot_publish_artifact_or_alias() {
        let f = Fixture::new().await;
        let (artifact, _) = f.artifact(None).await;
        f.db.execute_unprepared("UPDATE agent_coverage_claim SET fence_token = 2")
            .await
            .unwrap();
        let txn = db::begin_write_transaction(&f.db).await.unwrap();
        assert!(artifact.persist(&txn, deadline()).await.is_err());
        txn.rollback().await.unwrap();
        for scope in [
            MetadataScope::AgentCapturePending,
            MetadataScope::AgentCapturePendingChunk,
            MetadataScope::AgentCaptureSessionAlias,
        ] {
            assert_eq!(f.count(scope).await, 0);
        }
    }

    #[cfg(unix)]
    fn synthetic_reasoning_v2_bytes(artifact: &SealedPendingArtifact) -> Vec<u8> {
        use crate::internal::ai::{
            capture::pending_payload::PendingReasoningIdentityAssociation,
            observed_agents::reasoning::{
                VerifiedCaptureBinding, synthetic_native_batch_for_turns,
            },
        };

        let PendingEnvelope {
            binding,
            payload,
            coverage,
            ..
        } = serde_json::from_slice(&artifact.bytes).unwrap();
        let native = synthetic_native_batch_for_turns(
            PK,
            NATIVE,
            vec![(0, b"OG17_PRIVATE_PENDING_V2_CANARY".to_vec())],
        );
        let native_binding =
            VerifiedCaptureBinding::from_validated_capture_context(&native, PK, "claude_code")
                .unwrap();
        let mac = format!("reasoning-identity/hmac-v1/{}", "a".repeat(64));
        let association: PendingReasoningIdentityAssociation =
            serde_json::from_value(serde_json::json!({
                "logical_turn_key": "synthetic-turn-0",
                "slot": mac,
                "descriptor_mac": format!("reasoning-identity/hmac-v1/{}", "b".repeat(64)),
                "disposition": "verified_nonempty"
            }))
            .unwrap();
        let payload = payload
            .with_synthetic_reasoning(&native, &native_binding, vec![association])
            .unwrap();
        encode_envelope(binding, payload, coverage).unwrap()
    }

    #[cfg(unix)]
    async fn actual_native_reasoning_artifact(
        f: &Fixture,
    ) -> (SealedPendingArtifact, CheckpointRedactedPayload) {
        actual_native_reasoning_artifact_with_terminal(f, None, false).await
    }

    #[cfg(unix)]
    async fn actual_native_reasoning_artifact_with_terminal(
        f: &Fixture,
        terminal: Option<TerminalArtifactBinding>,
        covered_prefix: bool,
    ) -> (SealedPendingArtifact, CheckpointRedactedPayload) {
        use crate::internal::ai::{
            capture::checkpoint::{ReasoningIdentityPreparation, prepare_reasoning_coverage_plan},
            observed_agents::{
                NormalizedTurn, RedactedBytes,
                reasoning::{ReasoningSourceDisposition, synthetic_native_batch_for_turns},
            },
        };

        let (ordinary, ordinary_payload) = f.artifact_with_terminal_binding(None, terminal).await;
        let PendingEnvelope {
            binding,
            coverage: portable,
            ..
        } = serde_json::from_slice(&ordinary.bytes).unwrap();
        let mut coverage = portable
            .into_live(
                &ordinary.identity,
                &binding.checkpoint_id,
                binding.parent_commit.clone(),
                1,
            )
            .unwrap();
        let reserved_key = if covered_prefix {
            "synthetic-turn-1"
        } else {
            "synthetic-turn-0"
        };
        coverage.claims[0].logical_turn_key = reserved_key.to_owned();
        f.db.execute_raw(Statement::from_sql_and_values(
            f.db.get_database_backend(),
            "UPDATE agent_coverage_claim SET logical_turn_key = ? \
             WHERE session_id = 'claude__native-session' AND logical_turn_key = 'turn-1'",
            [reserved_key.into()],
        ))
        .await
        .unwrap();
        if covered_prefix {
            let mut ordinary = coverage.claims[0].clone();
            ordinary.logical_turn_key = "synthetic-turn-2".to_owned();
            ordinary.fence_token = 2;
            coverage.claims.push(ordinary);
            f.db.execute_unprepared(
                "INSERT INTO agent_coverage_claim \
                 (session_id,logical_turn_key,coverage_schema_version,coverage_digest, \
                 completeness,revision,state,owner,fence_token,source_channel,created_at,updated_at) \
                 VALUES ('claude__native-session','synthetic-turn-2',1, \
                 'dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd', \
                 'complete',0,'reserved_live','owned-live-reservation',2,'live',1,1)",
            )
            .await
            .unwrap();
            f.db.execute_unprepared(
                "INSERT INTO agent_coverage_claim \
                 (session_id,logical_turn_key,coverage_schema_version,coverage_digest, \
                 completeness,revision,state,checkpoint_id,traces_commit, \
                 source_channel,created_at,updated_at) \
                 VALUES ('claude__native-session','synthetic-turn-0',1, \
                 'eeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeeee', \
                 'complete',1,'catalog_committed','covered-prefix-checkpoint', \
                 'covered-prefix-commit','live',1,1)",
            )
            .await
            .unwrap();
        }
        let native = synthetic_native_batch_for_turns(
            PK,
            NATIVE,
            if covered_prefix {
                vec![
                    (0, b"OG21_COVERED_PREFIX_NATIVE_CANARY".to_vec()),
                    (1, b"OG21_PRIVATE_PENDING_NATIVE_CANARY".to_vec()),
                ]
            } else {
                vec![(0, b"OG21_PRIVATE_PENDING_NATIVE_CANARY".to_vec())]
            },
        );
        let payload = CheckpointRedactedPayload::from_redacted_capture_with_reasoning(
            RedactedBytes::new_unchecked(ordinary_payload.transcript().bytes().to_vec()),
            Some(ordinary_payload.snapshot().clone()),
            RedactedBytes::new_unchecked(ordinary_payload.metadata_json().bytes().to_vec()),
            RedactedBytes::new_unchecked(
                ordinary_payload.lifecycle_events_jsonl().bytes().to_vec(),
            ),
            RedactedBytes::new_unchecked(ordinary_payload.redaction_report_json().bytes().to_vec()),
            ReasoningSourceDisposition::NativeProven(native),
        )
        .unwrap();
        let turns = (0..if covered_prefix { 3 } else { 1 })
            .map(|ordinal| NormalizedTurn {
                logical_turn_key: format!("synthetic-turn-{ordinal}"),
                ordinal,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: Vec::new(),
            })
            .collect::<Vec<_>>();
        if covered_prefix {
            let digest = turns[1].digest_hex();
            for claim in &mut coverage.claims {
                claim.coverage_digest = digest.clone();
            }
            f.db.execute_raw(Statement::from_sql_and_values(
                f.db.get_database_backend(),
                "UPDATE agent_coverage_claim SET coverage_digest = ? WHERE session_id = ?",
                [digest.into(), PK.into()],
            ))
            .await
            .unwrap();
        }
        let identity = prepare_reasoning_coverage_plan(
            &payload,
            ReasoningIdentityPreparation {
                conn: &f.db,
                scope: &f.scope,
                storage_path: &f.storage,
                authorized_root: f.root.path(),
                session_id: PK,
                provider: "claude_code",
                turns: &turns,
                deadline: deadline(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let artifact = SealedPendingArtifact::seal(
            &f.db,
            &f.storage,
            f.root.path(),
            PendingSealRequest {
                binding,
                identity: ordinary.identity,
                payload: &payload,
                coverage: &coverage,
                reasoning: Some(&identity),
            },
            deadline(),
        )
        .await
        .unwrap();
        (artifact, payload)
    }

    #[cfg(unix)]
    struct NativeTerminalFixture {
        f: Fixture,
        artifact: SealedPendingArtifact,
        source_payload: CheckpointRedactedPayload,
        recovery: crate::internal::ai::capture::catalog::CaptureCatalogFinalizerRecovery,
    }

    #[cfg(unix)]
    impl NativeTerminalFixture {
        async fn new() -> Self {
            Self::new_with_covered_prefix(false).await
        }

        async fn new_with_covered_prefix(covered_prefix: bool) -> Self {
            use crate::internal::ai::capture::{
                catalog::{
                    CaptureCatalogAction, CaptureCatalogApplyRequest,
                    CaptureCatalogFinalizeRequest, CaptureCatalogFinalizeResult,
                    CaptureCatalogMutation, CaptureCatalogSession, CaptureCatalogStore,
                },
                finalizer::{
                    CaptureFinalizeMode, CaptureFinalizePolicy, FinalizeCheckpointProgress,
                },
                state::{
                    CapturePhase, CheckpointWrite, DurableCaptureState, LifecycleReducerInput,
                    reduce_lifecycle,
                },
            };

            let f = Fixture::new().await;
            let event_id = Uuid::new_v4();
            let receipt_key = format!("capture-dedup-v2:{}", "a".repeat(64));
            let action = CaptureCatalogAction::from_ingress(
                event_id,
                Some(&receipt_key),
                LifecycleEventKind::SessionEnd,
            )
            .unwrap();
            let checkpoint_id =
                crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                    event_id,
                    CheckpointWrite::Committed,
                );
            let marker_generation = Uuid::new_v4().to_string();
            let (artifact, source_payload) = actual_native_reasoning_artifact_with_terminal(
                &f,
                Some(TerminalArtifactBinding {
                    event_id,
                    checkpoint_id: checkpoint_id.clone(),
                    action_key: action.action_key().to_owned(),
                    receipt_key,
                    marker_generation: marker_generation.clone(),
                    parent_commit: None,
                    reserved_revision: 2,
                }),
                covered_prefix,
            )
            .await;
            f.persist(&artifact).await;
            let active = DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            };
            let action_plan = reduce_lifecycle(LifecycleReducerInput {
                current: Some(active),
                event_kind: LifecycleEventKind::SessionEnd,
                event_id,
                occurred_at: 1_700_000_000,
                deadline: None,
            })
            .unwrap();
            let mutation =
                CaptureCatalogMutation::from_reducer(Some(active), &action_plan, 1_700_000_000)
                    .unwrap();
            let session = CaptureCatalogSession::new(
                PK,
                "claude_code",
                NATIVE,
                f.root.path().to_string_lossy(),
            )
            .unwrap();
            let apply =
                CaptureCatalogApplyRequest::new(f.scope.clone(), session, action.clone(), mutation)
                    .unwrap();
            let catalog = CaptureCatalogStore::new(f.db.clone());
            catalog.apply(&apply).await.unwrap();
            let policy = CaptureFinalizePolicy::new(
                None,
                CaptureFinalizeMode::Deferrable,
                action.action_key(),
            )
            .unwrap();
            let finalizer = CaptureCatalogFinalizeRequest::from_apply(
                &apply,
                policy,
                marker_generation,
                Some(artifact.binding().source_commitment.clone()),
                1,
                FinalizeCheckpointProgress::NotStarted,
            )
            .unwrap();
            assert!(matches!(
                catalog.finalize(&finalizer).await.unwrap(),
                CaptureCatalogFinalizeResult::Pending { .. }
            ));
            let recovery = catalog
                .pending_finalizer_recoveries_for_doctor(2)
                .await
                .unwrap()
                .recoveries
                .into_iter()
                .find(|recovery| recovery.checkpoint_id() == checkpoint_id)
                .unwrap();
            assert!(recovery.artifact_present());
            Self {
                f,
                artifact,
                source_payload,
                recovery,
            }
        }

        async fn claim_snapshot(&self) -> String {
            self.f
                .db
                .query_one_raw(Statement::from_sql_and_values(
                    self.f.db.get_database_backend(),
                    "SELECT json_object('state',state,'revision',revision,'owner',owner, \
                     'fence_token',fence_token,'attempt_checkpoint_id',attempt_checkpoint_id, \
                     'checkpoint_id',checkpoint_id,'traces_commit',traces_commit) AS snapshot \
                     FROM agent_coverage_claim WHERE session_id = ? AND logical_turn_key = 'synthetic-turn-0'",
                    [PK.into()],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by::<String, _>("snapshot")
                .unwrap()
        }

        async fn pending_bytes(&self) -> Vec<u8> {
            load_chunks(&self.f.db, &self.artifact.header)
                .await
                .unwrap()
        }

        async fn replay_result(
            &self,
        ) -> anyhow::Result<crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome>
        {
            use crate::internal::ai::{
                capture::recovery::{DoctorReplayRequest, replay_for_doctor},
                capture_scope::CaptureCommitDeadline,
            };
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(60)).unwrap();
            let helper = crate::utils::test::real_libra_helper_binary();
            assert!(helper.is_file(), "built Libra helper is unavailable");
            crate::internal::ai::authorized_read::with_test_helper_program(
                helper,
                replay_for_doctor(DoctorReplayRequest {
                    conn: &self.f.db,
                    recovery: &self.recovery,
                    storage: &self.f.storage,
                    root: self.f.root.path(),
                    repo_path: &self.f.storage,
                    now_millis: 2,
                    deadline,
                    manual: false,
                }),
            )
            .await
        }

        async fn replay(&self) -> crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome {
            self.replay_result().await.unwrap()
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_replay_commits_original_identity_atomically() {
        let f = NativeTerminalFixture::new().await;
        assert_eq!(
            f.replay().await,
            crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::Completed,
        );
        assert_eq!(f.f.count(MetadataScope::AgentCapturePending).await, 0);
        let identity = MetadataKv::get_with_conn(
            &f.f.db,
            MetadataScope::AgentReasoningIdentity,
            PK,
            crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
        )
        .await
        .unwrap();
        assert!(
            identity.is_some(),
            "replay must commit the source-derived identity"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_store_failure_preserves_pending() {
        use crate::internal::ai::capture::checkpoint::test_support::{
            CheckpointFaultPoint, armed_fault, fail_once_at,
        };

        let f = NativeTerminalFixture::new().await;
        fail_once_at(CheckpointFaultPoint::AfterMarkerRegistration);
        assert!(f.replay_result().await.is_err());
        assert_eq!(armed_fault(), None, "fault must reach the actual store");
        assert_eq!(f.f.count(MetadataScope::AgentCapturePending).await, 1);
        assert_eq!(f.pending_bytes().await, f.artifact.bytes);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_store_failure_preserves_claims() {
        use crate::internal::ai::capture::checkpoint::test_support::{
            CheckpointFaultPoint, armed_fault, fail_once_at,
        };

        let f = NativeTerminalFixture::new().await;
        let before = f.claim_snapshot().await;
        fail_once_at(CheckpointFaultPoint::AfterObjectWrite);
        assert!(f.replay_result().await.is_err());
        assert_eq!(armed_fault(), None, "fault must reach object write");
        let after = f.claim_snapshot().await;
        assert_eq!(after, before, "failed append must retain original claim");
        assert_eq!(f.f.count(MetadataScope::AgentCapturePending).await, 1);
        assert_eq!(f.pending_bytes().await, f.artifact.bytes);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_replay_identity_transaction_failure_rolls_back() {
        use crate::internal::ai::capture::checkpoint::test_support::{
            CheckpointFaultPoint, armed_fault, fail_once_at,
        };

        let f = NativeTerminalFixture::new().await;
        let before = f.claim_snapshot().await;
        fail_once_at(CheckpointFaultPoint::CompanionTransaction);
        assert!(f.replay_result().await.is_err());
        assert_eq!(
            armed_fault(),
            None,
            "fault must reach companion transaction"
        );
        assert_eq!(f.claim_snapshot().await, before);
        assert_eq!(f.f.count(MetadataScope::AgentCapturePending).await, 1);
        assert_eq!(f.pending_bytes().await, f.artifact.bytes);
        assert!(
            MetadataKv::get_with_conn(
                &f.f.db,
                MetadataScope::AgentReasoningIdentity,
                PK,
                crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
            )
            .await
            .unwrap()
            .is_none(),
            "identity must roll back with the failed ref transaction"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_replay_is_session_end_source_free() {
        let f = NativeTerminalFixture::new().await;
        let source = f.f.root.path().join("provider-source.jsonl");
        if source.exists() {
            std::fs::remove_file(source).unwrap();
        }
        f.f.db.execute_unprepared("VACUUM").await.unwrap();
        assert_eq!(
            f.replay().await,
            crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::Completed,
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_forward_recovery_preserves_evidence() {
        use crate::internal::ai::capture::checkpoint::test_support::{
            CheckpointFaultPoint, armed_fault, fail_once_at,
        };

        let f = NativeTerminalFixture::new().await;
        fail_once_at(CheckpointFaultPoint::AfterMarkerRegistration);
        assert!(f.replay_result().await.is_err());
        assert_eq!(armed_fault(), None);
        assert_eq!(f.pending_bytes().await, f.artifact.bytes);
        let catalog =
            crate::internal::ai::capture::catalog::CaptureCatalogStore::new(f.f.db.clone());
        let recovery = catalog
            .pending_finalizer_recoveries_for_doctor(3)
            .await
            .unwrap()
            .recoveries
            .into_iter()
            .find(|recovery| recovery.checkpoint_id() == f.recovery.checkpoint_id())
            .unwrap();
        let retry = NativeTerminalFixture { recovery, ..f };
        assert_eq!(
            retry.replay().await,
            crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::Completed,
        );
        assert_eq!(retry.f.count(MetadataScope::AgentCapturePending).await, 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_completion_is_atomic() {
        let f = NativeTerminalFixture::new().await;
        assert_eq!(f.f.count(MetadataScope::AgentCapturePending).await, 1);
        assert_eq!(f.f.count(MetadataScope::AgentCapturePendingChunk).await, 1);
        assert_eq!(f.f.count(MetadataScope::AgentCaptureSessionAlias).await, 1);
        assert_eq!(
            f.replay().await,
            crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::Completed,
        );
        for scope in [
            MetadataScope::AgentCapturePending,
            MetadataScope::AgentCapturePendingChunk,
            MetadataScope::AgentCaptureSessionAlias,
        ] {
            assert_eq!(f.f.count(scope).await, 0, "terminal cleanup must be atomic");
        }
        assert!(f.claim_snapshot().await.contains("catalog_committed"));
        assert!(
            MetadataKv::get_with_conn(
                &f.f.db,
                MetadataScope::AgentReasoningIdentity,
                PK,
                crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
            )
            .await
            .unwrap()
            .is_some()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_erasure_is_atomic() {
        let f = NativeTerminalFixture::new().await;
        let txn = db::begin_write_transaction(&f.f.db).await.unwrap();
        assert!(!erase_session_artifacts(&txn, PK).await.unwrap());
        txn.rollback().await.unwrap();
        assert_eq!(f.f.count(MetadataScope::AgentCapturePending).await, 1);
        assert_eq!(f.pending_bytes().await, f.artifact.bytes);
        let txn = db::begin_write_transaction(&f.f.db).await.unwrap();
        assert!(!erase_session_artifacts(&txn, PK).await.unwrap());
        txn.commit().await.unwrap();
        for scope in [
            MetadataScope::AgentCapturePending,
            MetadataScope::AgentCapturePendingChunk,
            MetadataScope::AgentCaptureSessionAlias,
        ] {
            assert_eq!(f.f.count(scope).await, 0);
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_retains_current_fences() {
        use crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome;

        for fence in [
            "scope",
            "alias",
            "incarnation",
            "receipt",
            "generation",
            "coverage",
            "tombstone",
        ] {
            let f = NativeTerminalFixture::new().await;
            let before = f.pending_bytes().await;
            match fence {
                "scope" => {
                    ConfigKv::set_with_conn(&f.f.db, "libra.repoid", "different-repository", false)
                        .await
                        .unwrap();
                }
                "alias" => {
                    f.f.db
                        .execute_unprepared(
                            "DELETE FROM metadata_kv WHERE scope = 'agent_capture_session_alias'",
                        )
                        .await
                        .unwrap();
                }
                "incarnation" => {
                    f.f.db
                        .execute_unprepared(
                            "UPDATE agent_session SET metadata_json = json_set(metadata_json, \
                         '$.capture_incarnation', 'ffffffffffffffffffffffffffffffff') \
                         WHERE session_id = 'claude__native-session'",
                        )
                        .await
                        .unwrap();
                }
                "receipt" => {
                    f.f.db
                        .execute_unprepared(
                            "UPDATE agent_session SET metadata_json = json_remove(metadata_json, \
                         '$.capture_catalog_receipts_v1') \
                         WHERE session_id = 'claude__native-session'",
                        )
                        .await
                        .unwrap();
                }
                "generation" => {
                    f.f.db
                        .execute_unprepared(
                            "UPDATE agent_session SET metadata_json = json_set(metadata_json, \
                         '$.capture_catalog_receipts_v1.entries[0].finalizer.marker_generation', \
                         'different-generation') WHERE session_id = 'claude__native-session'",
                        )
                        .await
                        .unwrap();
                }
                "coverage" => {
                    f.f.db
                        .execute_unprepared(
                            "UPDATE agent_coverage_claim SET fence_token = fence_token + 1 \
                         WHERE session_id = 'claude__native-session'",
                        )
                        .await
                        .unwrap();
                }
                "tombstone" => {
                    f.f.db
                        .execute_unprepared(
                            "INSERT INTO agent_import_tombstone \
                         (tombstone_id,agent_kind,provider_session_id,erased_session_id,erased_at) \
                         VALUES ('og21-tombstone','claude_code','native-session', \
                         'claude__native-session',1)",
                        )
                        .await
                        .unwrap();
                }
                _ => unreachable!(),
            }
            let result = f.replay_result().await;
            assert!(
                !matches!(result, Ok(ArtifactRecoveryOutcome::Completed)),
                "{fence} fence admitted a stale native replay"
            );
            assert_eq!(f.pending_bytes().await, before, "{fence} lost evidence");
            assert!(
                MetadataKv::get_with_conn(
                    &f.f.db,
                    MetadataScope::AgentReasoningIdentity,
                    PK,
                    crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
                )
                .await
                .unwrap()
                .is_none(),
                "{fence} wrote reasoning identity"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_authenticated_pending_rejects_inconsistent_identity_claim_associations() {
        for invalid in ["duplicate", "missing", "orphan", "wrong_order"] {
            let f = Fixture::new().await;
            let (mut artifact, _) = actual_native_reasoning_artifact(&f).await;
            let mut value: serde_json::Value = serde_json::from_slice(&artifact.bytes).unwrap();
            if invalid == "duplicate" || invalid == "wrong_order" {
                let mut second_claim = value["coverage"]["claims"][0].clone();
                second_claim["logical_turn_key"] = "synthetic-turn-1".into();
                second_claim["fence_token"] = 2.into();
                value["coverage"]["claims"]
                    .as_array_mut()
                    .unwrap()
                    .push(second_claim);
                let mut second_association =
                    value["payload"]["identity_commit_associations"][0].clone();
                if invalid == "wrong_order" {
                    second_association["logical_turn_key"] = "synthetic-turn-0".into();
                    value["payload"]["identity_commit_associations"][0]["logical_turn_key"] =
                        "synthetic-turn-1".into();
                }
                value["payload"]["identity_commit_associations"]
                    .as_array_mut()
                    .unwrap()
                    .push(second_association);
            } else if invalid == "missing" {
                value["payload"]["identity_commit_associations"]
                    .as_array_mut()
                    .unwrap()
                    .clear();
            } else {
                value["payload"]["identity_commit_associations"][0]["logical_turn_key"] =
                    "orphan-turn".into();
            }
            let malformed: PendingEnvelope = serde_json::from_value(value).unwrap();
            let bytes = serde_json::to_vec(&malformed).unwrap();
            artifact.header.mac = f
                .scope
                .sign_pending_envelope_until(&f.db, &f.storage, f.root.path(), &bytes, deadline())
                .await
                .unwrap();
            artifact.header.envelope_bytes = bytes.len();
            artifact.header.chunks = bytes.len().div_ceil(CHUNK_BYTES);
            artifact.bytes = bytes;
            f.persist(&artifact).await;
            let identity = f.identity(Some(artifact.identity.alias())).await;
            let error = load_verified_payload(
                &f.db,
                &f.storage,
                f.root.path(),
                &identity,
                artifact.binding(),
                &artifact.header,
                deadline(),
            )
            .await
            .err()
            .expect("inconsistent signed associations must fail authenticated load");
            assert!(
                format!("{error:#}").contains("membership"),
                "{invalid} must fail at membership before native restoration: {error:#}"
            );
            assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 1);
            assert_eq!(f.count(MetadataScope::AgentReasoningIdentity).await, 0);
        }
    }

    #[cfg(unix)]
    pub(crate) async fn assert_pending_late_restore_requires_terminal_claim_authority() {
        use crate::internal::ai::{
            capture::checkpoint::{CheckpointStore, CheckpointWriteRequest, TracesCheckpointStore},
            traces::CheckpointScope,
        };

        let f = NativeTerminalFixture::new().await;
        let identity = f.f.identity(Some(f.artifact.identity.alias())).await;
        let verified = load_verified_payload(
            &f.f.db,
            &f.f.storage,
            f.f.root.path(),
            &identity,
            f.artifact.binding(),
            &f.artifact.header,
            deadline(),
        )
        .await
        .unwrap();
        let store = TracesCheckpointStore::new(
            &f.f.db,
            &f.f.storage,
            PK,
            &f.artifact.binding().checkpoint_id,
            &[],
        )
        .unwrap();
        let request = || {
            CheckpointWriteRequest::new(
                &f.artifact.binding().action_key,
                &f.artifact.binding().checkpoint_id,
                PK,
                "claude_code",
                f.artifact.binding().parent_commit.as_deref(),
                CheckpointScope::Committed,
                store.marker_generation(),
                None,
                verified.payload(),
                None,
                Some(
                    crate::internal::ai::capture_scope::CaptureCommitDeadline::from_budget(
                        Duration::from_secs(60),
                    )
                    .unwrap(),
                ),
            )
            .unwrap()
        };
        assert!(
            store.write(request()).await.is_err(),
            "omitted pending coverage must fail before marker registration"
        );
        assert!(
            MetadataKv::get_with_conn(
                &f.f.db,
                MetadataScope::AgentTracesInflight,
                PK,
                &f.artifact.binding().checkpoint_id,
            )
            .await
            .unwrap()
            .is_none()
        );
        assert!(
            store
                .write(
                    request()
                        .with_pending_coverage(verified.coverage(), &f.f.storage, f.f.root.path(),)
                        .unwrap(),
                )
                .await
                .is_err(),
            "ordinary store must not mint current native authority"
        );
        assert_eq!(f.pending_bytes().await, f.artifact.bytes);
        assert_eq!(
            f.replay().await,
            crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::Completed,
            "catalog-elected terminal claim authority permits the same artifact"
        );
    }

    #[cfg(unix)]
    pub(crate) async fn assert_actual_borrowed_recovery_reauthenticates_each_attempt() {
        use crate::internal::ai::capture::{
            checkpoint::test_support::{CheckpointFaultPoint, armed_fault, fail_once_at},
            recovery::ArtifactRecoveryOutcome,
        };

        let f = NativeTerminalFixture::new().await;
        let original = f.pending_bytes().await;
        fail_once_at(CheckpointFaultPoint::AfterMarkerRegistration);
        assert!(f.replay_result().await.is_err());
        assert_eq!(armed_fault(), None);
        assert_eq!(f.pending_bytes().await, original);
        let catalog =
            crate::internal::ai::capture::catalog::CaptureCatalogStore::new(f.f.db.clone());
        let recovery = catalog
            .pending_finalizer_recoveries_for_doctor(3)
            .await
            .unwrap()
            .recoveries
            .into_iter()
            .find(|item| item.checkpoint_id() == f.recovery.checkpoint_id())
            .unwrap();
        let retry = NativeTerminalFixture { recovery, ..f };
        retry
            .f
            .db
            .execute_unprepared(
                "UPDATE agent_coverage_claim SET fence_token = fence_token + 1 \
                 WHERE session_id = 'claude__native-session'",
            )
            .await
            .unwrap();
        let result = retry.replay_result().await;
        assert!(
            !matches!(result, Ok(ArtifactRecoveryOutcome::Completed)),
            "changed current claim must reject the next borrowed recovery attempt"
        );
        assert_eq!(retry.pending_bytes().await, original);
        assert!(
            MetadataKv::get_with_conn(
                &retry.f.db,
                MetadataScope::AgentReasoningIdentity,
                PK,
                crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
            )
            .await
            .unwrap()
            .is_none()
        );
    }

    #[cfg(unix)]
    pub(crate) async fn assert_covered_prefix_keeps_reserved_identity_subset() {
        use crate::internal::ai::{
            capture::checkpoint::{
                ReasoningIdentityPreparation, ReasoningIdentityReadContext,
                prepare_reasoning_coverage_plan, prepare_reasoning_coverage_plan_from_disposition,
            },
            coverage_gate::{
                ReasoningCoverageInput, reserve_turn_claims_with_reasoning_no_deadline,
            },
            observed_agents::{
                NormalizedTurn,
                reasoning::{ReasoningSourceDisposition, synthetic_native_batch_for_turns},
            },
        };

        let f = NativeTerminalFixture::new_with_covered_prefix(true).await;
        let turns = (0..3)
            .map(|ordinal| NormalizedTurn {
                logical_turn_key: format!("synthetic-turn-{ordinal}"),
                ordinal,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: Vec::new(),
            })
            .collect::<Vec<_>>();
        let direct = prepare_reasoning_coverage_plan(
            &f.source_payload,
            ReasoningIdentityPreparation {
                conn: &f.f.db,
                scope: &f.f.scope,
                storage_path: &f.f.storage,
                authorized_root: f.f.root.path(),
                session_id: PK,
                provider: "claude_code",
                turns: &turns,
                deadline: deadline(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let original: serde_json::Value = serde_json::from_slice(&f.artifact.bytes).unwrap();
        let associations = original["payload"]["identity_commit_associations"]
            .as_array()
            .unwrap();
        assert_eq!(associations.len(), 2);
        assert_eq!(associations[0]["logical_turn_key"], "synthetic-turn-1");
        assert_eq!(associations[1]["logical_turn_key"], "synthetic-turn-2");
        assert_eq!(associations[0]["disposition"], "verified_nonempty");
        assert_eq!(associations[1]["disposition"], "verified_empty");
        for (index, key) in ["synthetic-turn-1", "synthetic-turn-2"].iter().enumerate() {
            assert_eq!(
                associations[index]["slot"],
                direct.turn(key).unwrap().slot()
            );
            assert_eq!(
                associations[index]["descriptor_mac"],
                direct.turn(key).unwrap().descriptor_mac()
            );
        }
        let identity = f.f.identity(Some(f.artifact.identity.alias())).await;
        let verified = load_verified_payload(
            &f.f.db,
            &f.f.storage,
            f.f.root.path(),
            &identity,
            f.artifact.binding(),
            &f.artifact.header,
            deadline(),
        )
        .await
        .unwrap();
        assert_eq!(verified.coverage().claims.len(), 2);
        assert!(verified.payload().deferred_pending_reasoning().is_some());
        assert_eq!(
            f.replay().await,
            crate::internal::ai::capture::recovery::ArtifactRecoveryOutcome::Completed,
        );
        let retained = MetadataKv::get_with_conn(
            &f.f.db,
            MetadataScope::AgentReasoningIdentity,
            PK,
            crate::internal::metadata::ReasoningIdentitySessionMap::KEY,
        )
        .await
        .unwrap()
        .unwrap();
        let map: serde_json::Value = serde_json::from_str(&retained.value).unwrap();
        let records = map["records"].as_object().unwrap();
        assert_eq!(records.len(), 2, "covered prefix must not be recommitted");
        for (index, key) in ["synthetic-turn-1", "synthetic-turn-2"].iter().enumerate() {
            let slot = associations[index]["slot"].as_str().unwrap();
            let record = &records[slot];
            assert_eq!(record["body"]["context"]["logical_turn_key"], slot);
            assert_ne!(slot, *key, "private identity must use a keyed turn slot");
            assert_eq!(
                record["body"]["kind"],
                if index == 0 { "nonempty" } else { "empty" }
            );
        }
        let turn2 = &turns[2..3];
        let reader = ReasoningIdentityReadContext::new(
            &f.f.scope,
            PK,
            &f.f.storage,
            f.f.root.path(),
            deadline(),
        );
        let ordinary = reserve_turn_claims_with_reasoning_no_deadline(
            &f.f.db,
            &f.f.scope,
            PK,
            turn2,
            "ordinary-v1-owner",
            3,
            "live",
            ReasoningCoverageInput::ordinary(&reader),
        )
        .await
        .unwrap();
        assert_eq!(ordinary.skipped_incomplete_reasoning, 1);
        assert!(ordinary.ordinary.reserved.is_empty());
        let changed = ReasoningSourceDisposition::NativeProven(synthetic_native_batch_for_turns(
            PK,
            NATIVE,
            vec![(2, b"late-candidate-after-empty".to_vec())],
        ));
        let candidate = prepare_reasoning_coverage_plan_from_disposition(
            &changed,
            ReasoningIdentityPreparation {
                conn: &f.f.db,
                scope: &f.f.scope,
                storage_path: &f.f.storage,
                authorized_root: f.f.root.path(),
                session_id: PK,
                provider: "claude_code",
                turns: turn2,
                deadline: deadline(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let changed = reserve_turn_claims_with_reasoning_no_deadline(
            &f.f.db,
            &f.f.scope,
            PK,
            turn2,
            "late-candidate-owner",
            4,
            "live",
            ReasoningCoverageInput::native(&candidate),
        )
        .await
        .unwrap();
        assert_eq!(changed.ordinary.conflicted, 1);
        assert!(changed.ordinary.reserved.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_producer_and_reader_activate_together() {
        let f = Fixture::new().await;
        let (artifact, _) = actual_native_reasoning_artifact(&f).await;
        let encoded: serde_json::Value = serde_json::from_slice(&artifact.bytes).unwrap();
        assert_eq!(encoded["version"], 1);
        assert_eq!(encoded["payload"]["version"], 2);
        f.persist(&artifact).await;
        let identity = f.identity(Some(artifact.identity.alias())).await;
        let verified = load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            artifact.binding(),
            &artifact.header,
            deadline(),
        )
        .await
        .unwrap();
        assert!(verified.payload().deferred_pending_reasoning().is_some());
        assert_eq!(
            verified.coverage().claims[0].logical_turn_key,
            "synthetic-turn-0"
        );
        assert!(
            !verified
                .payload()
                .transcript()
                .bytes()
                .windows(b"OG21_PRIVATE_PENDING_NATIVE_CANARY".len())
                .any(|window| window == b"OG21_PRIVATE_PENDING_NATIVE_CANARY")
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_original_envelope_ownership_is_bounded() {
        let f = Fixture::new().await;
        let (artifact, _) = actual_native_reasoning_artifact(&f).await;
        f.persist(&artifact).await;
        let identity = f.identity(Some(artifact.identity.alias())).await;
        let verified = load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            artifact.binding(),
            &artifact.header,
            deadline(),
        )
        .await
        .unwrap();
        let replay = verified.payload().deferred_pending_reasoning().unwrap();
        assert_eq!(replay.original_envelope(), artifact.bytes);
        assert!(replay.original_envelope().len() <= MAX_ENVELOPE_BYTES);
    }

    #[cfg(unix)]
    async fn synthetic_reasoning_v2_envelope(f: &Fixture) -> (SealedPendingArtifact, Vec<u8>) {
        let (artifact, _) = f.artifact(None).await;
        let bytes = synthetic_reasoning_v2_bytes(&artifact);
        (artifact, bytes)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_v2_total_bytes_are_bounded() {
        let f = Fixture::new().await;
        let (_, bytes) = synthetic_reasoning_v2_envelope(&f).await;
        assert!(validate_envelope_shape(&bytes).is_ok());
        assert!(bytes.len() <= MAX_ENVELOPE_BYTES);
        let mut oversized = bytes;
        oversized.resize(MAX_ENVELOPE_BYTES + 1, b' ');
        assert!(validate_envelope_shape(&oversized).is_err());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_v2_chunk_count_is_bounded() {
        let f = Fixture::new().await;
        let (_, bytes) = synthetic_reasoning_v2_envelope(&f).await;
        assert!(bytes.len().div_ceil(CHUNK_BYTES) <= MAX_CHUNKS);
        assert_eq!(MAX_ENVELOPE_BYTES.div_ceil(CHUNK_BYTES), MAX_CHUNKS);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_v2_chunk_bytes_are_bounded() {
        let f = Fixture::new().await;
        let (_, bytes) = synthetic_reasoning_v2_envelope(&f).await;
        let mut restored = Vec::new();
        for chunk in bytes.chunks(CHUNK_BYTES) {
            assert!(chunk.len() <= CHUNK_BYTES);
            let encoded = STANDARD.encode(chunk);
            restored.extend_from_slice(&decode_part(&encoded, CHUNK_BYTES).unwrap());
        }
        assert_eq!(restored, bytes);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_codec_rejection_preserves_retained_evidence() {
        let f = Fixture::new().await;
        let (artifact, bytes) = synthetic_reasoning_v2_envelope(&f).await;
        f.persist(&artifact).await;
        let before = load_chunks(&f.db, &artifact.header).await.unwrap();
        let mut malformed = bytes;
        malformed.extend_from_slice(b"trailing bytes");
        assert!(serde_json::from_slice::<PendingEnvelope>(&malformed).is_err());
        assert_eq!(load_chunks(&f.db, &artifact.header).await.unwrap(), before);
        assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 1);
        // The real terminal-bound recovery driver must also leave a signed
        // unsupported v2 artifact, receipt and coverage claim untouched.
        replay_resolves_alias_after_vacuum_and_source_removal_inner(false, false, false, true)
            .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_v2_keeps_outer_v1_header_and_envelope() {
        let f = Fixture::new().await;
        let (mut artifact, bytes) = synthetic_reasoning_v2_envelope(&f).await;
        let ordinary_envelope: serde_json::Value = serde_json::from_slice(&artifact.bytes).unwrap();
        artifact.header.mac = f
            .scope
            .sign_pending_envelope_until(&f.db, &f.storage, f.root.path(), &bytes, deadline())
            .await
            .unwrap();
        artifact.header.envelope_bytes = bytes.len();
        artifact.header.chunks = bytes.len().div_ceil(CHUNK_BYTES);
        artifact.bytes = bytes.clone();
        f.persist(&artifact).await;
        let stored_header = header_for_checkpoint(
            &f.db,
            &f.scope.repo_id,
            &artifact.header.binding.checkpoint_id,
        )
        .await
        .unwrap()
        .expect("v2 artifact header must be present")
        .0;
        let envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(stored_header.version, VERSION);
        assert_eq!(stored_header.mac, artifact.header.mac);
        assert_eq!(stored_header.envelope_bytes, bytes.len());
        assert_eq!(stored_header.chunks, bytes.len().div_ceil(CHUNK_BYTES));
        assert_eq!(envelope["version"].as_u64(), Some(VERSION as u64));
        assert_eq!(envelope["payload"]["version"].as_u64(), Some(2));
        assert_eq!(
            envelope["coverage"], ordinary_envelope["coverage"],
            "v2 payload must preserve the ordinary outer coverage proof"
        );
        let coverage: PortablePendingCoverage =
            serde_json::from_value(envelope["coverage"].clone()).unwrap();
        coverage.validate().unwrap();
    }

    #[cfg(unix)]
    pub(crate) async fn assert_frozen_old_route_rejects_v2() {
        let f = Fixture::new().await;
        let (mut artifact, bytes) = synthetic_reasoning_v2_envelope(&f).await;
        let mac = f
            .scope
            .sign_pending_envelope_until(&f.db, &f.storage, f.root.path(), &bytes, deadline())
            .await
            .unwrap();
        artifact.header.mac = mac;
        artifact.header.envelope_bytes = bytes.len();
        artifact.header.chunks = bytes.len().div_ceil(CHUNK_BYTES);
        artifact.bytes = bytes.clone();
        f.persist(&artifact).await;
        let stored_header = MetadataKv::get_with_conn(
            &f.db,
            MetadataScope::AgentCapturePending,
            &f.scope.repo_id,
            &artifact.header.binding.checkpoint_id,
        )
        .await
        .unwrap()
        .expect("persisted pre-v2-compatible header");
        assert_eq!(
            stored_header.value,
            serde_json::to_string(&artifact.header).unwrap()
        );
        frozen_pre_v2::decode_header_v1(&stored_header.value).unwrap();
        let alias = artifact.identity.alias().to_owned();
        let identity = f.identity(Some(&alias)).await;
        let error = frozen_pre_v2::load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            artifact.binding(),
            &artifact.header,
            deadline(),
        )
        .await
        .err()
        .expect("pre-v2 route must reject a signed v2 pending artifact");
        assert!(
            error
                .to_string()
                .contains("invalid authenticated capture recovery envelope")
        );
        assert_eq!(load_chunks(&f.db, &artifact.header).await.unwrap(), bytes);
        assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_v2_old_recovery_route_fails_closed() {
        assert_frozen_old_route_rejects_v2().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_codec_forward_recovery_preserves_evidence() {
        let f = Fixture::new().await;
        let (mut artifact, bytes) = synthetic_reasoning_v2_envelope(&f).await;
        let mac = f
            .scope
            .sign_pending_envelope_until(&f.db, &f.storage, f.root.path(), &bytes, deadline())
            .await
            .unwrap();
        artifact.header.mac = mac;
        artifact.header.envelope_bytes = bytes.len();
        artifact.header.chunks = bytes.len().div_ceil(CHUNK_BYTES);
        artifact.bytes = bytes.clone();
        f.persist(&artifact).await;
        let alias = artifact.identity.alias().to_owned();
        let identity = f.identity(Some(&alias)).await;
        let error = load_verified_payload(
            &f.db,
            &f.storage,
            f.root.path(),
            &identity,
            artifact.binding(),
            &artifact.header,
            deadline(),
        )
        .await
        .err()
        .expect("v2 replay remains closed until OG-21 pairs the authenticated reader");
        assert!(
            error
                .to_string()
                .contains("capture recovery membership authentication failed")
        );
        assert_eq!(load_chunks(&f.db, &artifact.header).await.unwrap(), bytes);
        assert_eq!(f.count(MetadataScope::AgentCapturePending).await, 1);

        let legacy = Fixture::new().await;
        let (artifact, _) = legacy.artifact(None).await;
        legacy.persist(&artifact).await;
        let alias = artifact.identity.alias().to_owned();
        let identity = legacy.identity(Some(&alias)).await;
        assert!(
            load_verified_payload(
                &legacy.db,
                &legacy.storage,
                legacy.root.path(),
                &identity,
                artifact.binding(),
                &artifact.header,
                deadline(),
            )
            .await
            .is_ok()
        );
        // The quarantined manual route cannot consume its one-shot attempt
        // before OG-21's authenticated v2 reader is available.
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, false, false, true).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn reasoning_pending_doctor_retains_unsupported_v2() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, false, true, true).await;
    }
}
