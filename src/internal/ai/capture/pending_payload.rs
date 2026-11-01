//! Portable redacted payloads. Ordinary control identity is restored only in
//! RAM from a verified private alias, never from a durable native locator or
//! path. Closed v2 source proof also stores capture/source session identifiers
//! in the signed local artifact; those strings do not confer native authority.

use std::{
    collections::{HashMap, HashSet},
    io::{self, Write},
    path::Path,
    time::Instant,
};

use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use sea_orm::ConnectionTrait;
use serde::{
    Deserialize, Deserializer, Serialize,
    de::{IgnoredAny, SeqAccess, Visitor},
};
use serde_json::{Map, Value, value::RawValue};
use uuid::Uuid;

#[cfg(test)]
#[cfg(unix)]
use crate::internal::ai::observed_agents::reasoning::VerifiedCaptureBinding;
use crate::{
    internal::{
        ai::{
            capture::{
                checkpoint::{
                    AuthenticatedPendingEnvelope, CheckpointRedactedPayload,
                    NativeVersionedReasoningCoveragePlan, PreparedPendingNativeSeal,
                    ReasoningCoveragePlan, ReasoningTurnIdentity,
                },
                pending::{
                    PendingBinding, VersionedPendingRehydrateMaterial,
                    VersionedPendingReplayMaterial, VersionedPendingSealMaterial,
                },
                pending_identity::PreparedPendingAlias,
                snapshot::CaptureSnapshotProjection,
            },
            coverage_gate::{
                LiveClaimCommitPlan, ReservedTurnClaim, VersionedLiveClaimCommitPlan,
                VersionedReservedTurnClaim,
            },
            observed_agents::{
                Completeness, MAX_REDACTION_MATCH_SAMPLES, RedactedBytes, Redactor,
                coverage::CoverageSchemaVersion,
                parse_canon_value,
                reasoning::{
                    MAX_REASONING_TURN_KEY_BYTES, NativeProvenReasoning, PendingIntegrityStamp,
                    PendingIntegrityToken, PendingReasoningSourceDto, VerifiedPendingBinding,
                    seal_pending,
                },
            },
        },
        metadata::ReasoningIdentityKind,
    },
    utils::json::normalize_json_value,
};

const VERSION: u8 = 1;
const REASONING_VERSION: u8 = 2;
const MAX_TRANSCRIPT: usize = 24 * 1024 * 1024;
const MAX_SIDECARS: usize = 4 * 1024 * 1024;
const MAX_PORTABLE_CLAIMS: usize = MAX_SIDECARS / 64;
const MAX_PORTABLE_TURN_KEY_BYTES: usize = MAX_REASONING_TURN_KEY_BYTES;
/// The private map's existing cap bounds a versioned pending tuple before
/// allocating its typed claim/association vectors. The combined JSON arrays
/// also have the existing 1 MiB private-map wire budget.
pub(crate) const MAX_VERSIONED_PENDING_ENTRIES: usize =
    crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_ENTRIES;
pub(crate) const MAX_VERSIONED_PENDING_TUPLE_BYTES: usize =
    crate::internal::metadata::REASONING_IDENTITY_SESSION_MAP_MAX_BYTES;
// Keep the existing outer v1 coverage limit fixed if the inner reasoning
// source's key bound changes in a later revision.
const _: () = assert!(MAX_PORTABLE_TURN_KEY_BYTES == 512);
const MAX_ENVELOPE: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_EVENTS: usize = MAX_SIDECARS / 128;
const REMEDY: &str = "capture recovery payload cannot be trusted; run `libra agent doctor`";
const PENDING_V2_REMEDY: &str = "capture recovery contains pending reasoning v2 that this Libra build cannot replay; keep the artifact and upgrade to a release with v2 recovery support";
const METADATA_FIELDS: &[&str] = &[
    "schema_version",
    "checkpoint_id",
    "agent_kind",
    "scope",
    "model",
    "redaction_report",
    "created_at",
    "extraction",
    "transcript_snapshot",
    "usage_attribution",
];
const EVENT_FIELDS: &[&str] = &[
    "schema_version",
    "event_id",
    "identity_scheme",
    "kind",
    "agent_kind",
    "timestamp",
    "source",
    "partial",
    "provenance",
    "prompt",
    "model",
    "tool_name",
    "tool_input",
    "tool_response",
    "assistant_message",
];

/// Closed, private wire data, not a checkpoint persistence capability.
/// Even successfully decoded projections cannot construct RedactedBytes until
/// their exact membership in an authenticated envelope has been checked.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingPayloadProjection {
    version: u8,
    alias: String,
    snapshot: CaptureSnapshotProjection,
    transcript: String,
    metadata: Value,
    lifecycle: Vec<Value>,
    redaction_report: String,
    metadata_mac: String,
    lifecycle_mac: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_source: Option<PendingReasoningSourceDto>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    identity_commit_associations: Option<Vec<PendingReasoningIdentityAssociation>>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PendingReasoningIdentityAssociation {
    logical_turn_key: String,
    slot: String,
    descriptor_mac: String,
    disposition: PendingReasoningIdentityDisposition,
}

/// Only this value retains the original bounded canonical envelope after
/// early rehydration. Its DTO remains untrusted until a fresh late key check,
/// current terminal claim fence, and lower fixed-source proof validation.
pub(crate) struct PendingReasoningReplay {
    original_envelope: Vec<u8>,
    original_mac: String,
    source: PendingReasoningSourceDto,
    associations: Vec<PendingReasoningIdentityAssociation>,
    integrity: PendingIntegrityStamp,
}

impl PendingReasoningReplay {
    pub(crate) fn original_envelope(&self) -> &[u8] {
        &self.original_envelope
    }

    pub(crate) fn original_mac(&self) -> &str {
        &self.original_mac
    }

    pub(crate) fn source(&self) -> &PendingReasoningSourceDto {
        &self.source
    }

    pub(crate) fn matches_integrity(&self, token: &PendingIntegrityToken) -> bool {
        token.matches_stamp(&self.integrity)
            && serde_json::to_vec(&self.associations)
                .is_ok_and(|encoded| token.matches_associations(&encoded))
    }

    pub(crate) fn verify_recomputed_associations(
        &self,
        coverage: &LiveClaimCommitPlan,
        reasoning: &ReasoningCoveragePlan,
    ) -> Result<()> {
        ensure!(self.associations.len() == coverage.claims.len(), REMEDY);
        for (association, claim) in self.associations.iter().zip(&coverage.claims) {
            let turn = reasoning
                .turn(&claim.logical_turn_key)
                .ok_or_else(failure)?;
            let expected = PendingReasoningIdentityAssociation::from_prepared_turn(
                &claim.logical_turn_key,
                turn,
            );
            ensure!(
                association.logical_turn_key == expected.logical_turn_key
                    && association.slot == expected.slot
                    && association.descriptor_mac == expected.descriptor_mac
                    && association.disposition == expected.disposition,
                REMEDY
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum PendingReasoningIdentityDisposition {
    VerifiedEmpty,
    VerifiedNonempty,
}

impl PendingReasoningIdentityAssociation {
    #[allow(dead_code)] // OG-21 wires the actual claim plan into v2 producer.
    pub(crate) fn from_prepared_turn(key: &str, turn: &ReasoningTurnIdentity) -> Self {
        Self {
            logical_turn_key: key.to_owned(),
            slot: turn.slot().to_owned(),
            descriptor_mac: turn.descriptor_mac().to_owned(),
            disposition: match turn.kind() {
                ReasoningIdentityKind::Empty => PendingReasoningIdentityDisposition::VerifiedEmpty,
                ReasoningIdentityKind::Nonempty => {
                    PendingReasoningIdentityDisposition::VerifiedNonempty
                }
            },
        }
    }
}

/// A pure wire projection. It contains neither a native-origin capability
/// nor permission to persist an artifact. COV2-24 supplies the checked source
/// and identity tuple before this value can be used by a production sealer.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedPendingPayloadProjection {
    pub(crate) redacted: PendingPayloadProjection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reasoning_source: Option<PendingReasoningSourceDto>,
    pub(crate) identity_commit_associations: Vec<VersionedPendingIdentityAssociation>,
}

/// A negative-authority barrier binding the exact rehydrated redacted value
/// to its original authenticated outer-v2 bytes. It is not a native issuer.
/// Only authenticated rehydration below can construct it.
pub(crate) struct VersionedPendingPayloadRequirement {
    original_envelope_sha256: [u8; 32],
    redacted_payload_sha256: [u8; 32],
}
impl VersionedPendingPayloadRequirement {
    fn from_authenticated_payload(
        authentication: &AuthenticatedPendingEnvelope<'_>,
        payload: &CheckpointRedactedPayload,
    ) -> Result<Self> {
        use sha2::{Digest, Sha256};
        Ok(Self {
            original_envelope_sha256: Sha256::digest(authentication.bytes()).into(),
            redacted_payload_sha256: Self::payload_tag(payload)?,
        })
    }
    fn payload_tag(payload: &CheckpointRedactedPayload) -> Result<[u8; 32]> {
        use sha2::{Digest, Sha256};
        let mut digest = Sha256::new();
        digest.update(b"libra/versioned-pending-redacted/v1\0");
        let snapshot = serde_json::to_vec(payload.snapshot()).map_err(|_| failure())?;
        for bytes in [
            snapshot.as_slice(),
            payload.transcript().bytes(),
            payload.metadata_json().bytes(),
            payload.lifecycle_events_jsonl().bytes(),
            payload.redaction_report_json().bytes(),
        ] {
            digest.update((bytes.len() as u64).to_le_bytes());
            digest.update(bytes);
        }
        Ok(digest.finalize().into())
    }
    pub(crate) fn matches_payload(&self, payload: &CheckpointRedactedPayload) -> bool {
        Self::payload_tag(payload).is_ok_and(|tag| tag == self.redacted_payload_sha256)
    }
    pub(crate) fn matches_original(&self, bytes: &[u8]) -> bool {
        use sha2::{Digest, Sha256};
        <[u8; 32]>::from(Sha256::digest(bytes)) == self.original_envelope_sha256
    }
}

/// Original authenticated outer-v2 bytes retained with their ordered,
/// explicitly versioned associations. This owns DTOs, not native authority.
#[allow(dead_code)] // COV2-08 consumes this carrier and freshly reauthenticates it.
pub(crate) struct VersionedPendingReasoningReplay {
    original_envelope: Vec<u8>,
    original_mac: String,
    source: PendingReasoningSourceDto,
    associations: Vec<VersionedPendingIdentityAssociation>,
    integrity: PendingIntegrityStamp,
}

#[allow(dead_code)] // COV2-08 consumes the typed deferred data.
impl VersionedPendingReasoningReplay {
    pub(crate) fn original_envelope(&self) -> &[u8] {
        &self.original_envelope
    }
    pub(crate) fn original_mac(&self) -> &str {
        &self.original_mac
    }
    pub(crate) fn source(&self) -> &PendingReasoningSourceDto {
        &self.source
    }
    pub(crate) fn associations(&self) -> &[VersionedPendingIdentityAssociation] {
        &self.associations
    }
    pub(crate) fn matches_integrity(&self, token: &PendingIntegrityToken) -> bool {
        token.matches_stamp(&self.integrity)
            && serde_json::to_vec(&self.associations)
                .is_ok_and(|encoded| token.matches_associations(&encoded))
    }
    pub(crate) fn verify_recomputed_associations(
        &self,
        coverage: &VersionedLiveClaimCommitPlan,
        reasoning: &NativeVersionedReasoningCoveragePlan,
    ) -> Result<()> {
        ensure!(
            self.associations.len() == coverage.ordinary().claims.len(),
            REMEDY
        );
        for (association, (version, claim)) in self.associations.iter().zip(coverage.claims()) {
            let turn = reasoning
                .turn(version, &claim.logical_turn_key)
                .ok_or_else(failure)?;
            let expected = VersionedPendingIdentityAssociation::from_prepared_turn(
                version,
                &claim.logical_turn_key,
                turn,
            );
            ensure!(
                association.coverage_schema_version == expected.coverage_schema_version
                    && association.logical_turn_key == expected.logical_turn_key
                    && association.slot == expected.slot
                    && association.descriptor_mac == expected.descriptor_mac
                    && association.disposition == expected.disposition,
                REMEDY
            );
        }
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedPendingIdentityAssociation {
    pub(crate) coverage_schema_version: i64,
    pub(crate) logical_turn_key: String,
    pub(crate) slot: String,
    pub(crate) descriptor_mac: String,
    pub(crate) disposition: PendingReasoningIdentityDisposition,
}

impl VersionedPendingIdentityAssociation {
    #[allow(dead_code)] // COV2-24 consumes the checked COV2-23 borrow.
    pub(crate) fn from_prepared_turn(
        version: CoverageSchemaVersion,
        key: &str,
        turn: &ReasoningTurnIdentity,
    ) -> Self {
        Self {
            coverage_schema_version: version.as_i64(),
            logical_turn_key: key.to_owned(),
            slot: turn.slot().to_owned(),
            descriptor_mac: turn.descriptor_mac().to_owned(),
            disposition: match turn.kind() {
                ReasoningIdentityKind::Empty => PendingReasoningIdentityDisposition::VerifiedEmpty,
                ReasoningIdentityKind::Nonempty => {
                    PendingReasoningIdentityDisposition::VerifiedNonempty
                }
            },
        }
    }

    fn validate(&self) -> Result<()> {
        let version =
            CoverageSchemaVersion::try_from(self.coverage_schema_version).map_err(|_| failure())?;
        let prefix = match version {
            CoverageSchemaVersion::V1 => "reasoning-identity/hmac-v1/",
            CoverageSchemaVersion::V2 => "reasoning-identity/hmac-v2/",
        };
        ensure!(
            !self.logical_turn_key.is_empty()
                && self.logical_turn_key.len() <= MAX_PORTABLE_TURN_KEY_BYTES
                && !self.logical_turn_key.chars().any(char::is_control)
                && [self.slot.as_str(), self.descriptor_mac.as_str()]
                    .into_iter()
                    .all(|value| value
                        .strip_prefix(prefix)
                        .is_some_and(|digest| digest.len() == 64 && lower_hex(digest))),
            REMEDY
        );
        Ok(())
    }
}

impl VersionedPendingPayloadProjection {
    /// The pending owner supplies a projection bound to its original bytes
    /// and exact key-owner token. Source DTOs remain deferred and untrusted.
    #[allow(dead_code)] // COV2-08 wires recovery through the v2 loader.
    pub(crate) async fn rehydrate_authenticated_deferred<C: ConnectionTrait>(
        material: VersionedPendingRehydrateMaterial<'_>,
        conn: &C,
        storage: &Path,
        root: &Path,
        identity: &PreparedPendingAlias,
        deadline: Instant,
    ) -> Result<CheckpointRedactedPayload> {
        let (projection, authentication, integrity) = material.into_parts();
        projection.validate()?;
        check_budget(deadline)?;
        ensure!(
            projection.reasoning_source.is_some()
                && projection.redacted.alias == identity.alias()
                && authentication.scope() == identity.context().scope()
                && integrity.matches_associations(
                    &serde_json::to_vec(&projection.identity_commit_associations)
                        .map_err(|_| failure())?
                ),
            REMEDY
        );
        #[derive(Deserialize)]
        struct RawOuter<'a> {
            version: u8,
            #[serde(borrow)]
            payload: &'a RawValue,
        }
        #[derive(Deserialize)]
        struct RawProjection<'a> {
            #[serde(borrow)]
            redacted: &'a RawValue,
        }
        let outer: RawOuter<'_> =
            serde_json::from_slice(authentication.bytes()).map_err(|_| failure())?;
        ensure!(outer.version == 2, REMEDY);
        let raw: RawProjection<'_> =
            serde_json::from_str(outer.payload.get()).map_err(|_| failure())?;
        // Compare only this bounded redacted component; the pending key owner
        // has already authenticated the full canonical envelope and tuple.
        struct OriginalMemberWriter<'a> {
            original: &'a [u8],
            offset: usize,
        }
        impl Write for OriginalMemberWriter<'_> {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                let end = self.offset.checked_add(bytes.len()).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "capture member length overflow")
                })?;
                if self.original.get(self.offset..end) != Some(bytes) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "capture member differs",
                    ));
                }
                self.offset = end;
                Ok(bytes.len())
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let mut exact = OriginalMemberWriter {
            original: raw.redacted.get().as_bytes(),
            offset: 0,
        };
        serde_json::to_writer(&mut exact, &projection.redacted).map_err(|_| failure())?;
        ensure!(exact.offset == exact.original.len(), REMEDY);
        let payload = projection
            .redacted
            .rehydrate_authenticated_components(conn, storage, root, identity, deadline)
            .await?;
        let requirement = VersionedPendingPayloadRequirement::from_authenticated_payload(
            authentication,
            &payload,
        )?;
        payload
            .with_versioned_pending_requirement(requirement)
            .map_err(Into::into)
    }

    #[allow(dead_code)] // COV2-08 consumes the original bytes on every replay attempt.
    pub(crate) fn into_deferred_reasoning(
        material: VersionedPendingReplayMaterial,
    ) -> Result<VersionedPendingReasoningReplay> {
        let (projection, original_envelope, original_mac, integrity) = material.into_parts();
        projection.validate()?;
        ensure!(original_envelope.len() <= MAX_ENVELOPE, REMEDY);
        ensure!(
            integrity.matches_associations(
                &serde_json::to_vec(&projection.identity_commit_associations)
                    .map_err(|_| failure())?
            ),
            REMEDY
        );
        Ok(VersionedPendingReasoningReplay {
            original_envelope,
            original_mac,
            source: projection.reasoning_source.ok_or_else(failure)?,
            associations: projection.identity_commit_associations,
            integrity: integrity.into_stamp(),
        })
    }

    #[allow(dead_code)] // COV2-24 connects the checked native seal to this codec.
    pub(crate) fn new_codec(
        redacted: PendingPayloadProjection,
        reasoning_source: Option<PendingReasoningSourceDto>,
        identity_commit_associations: Vec<VersionedPendingIdentityAssociation>,
    ) -> Result<Self> {
        let result = Self {
            redacted,
            reasoning_source,
            identity_commit_associations,
        };
        result.validate()?;
        Ok(result)
    }

    /// Canonical inner bytes for the pure codec only. The size is counted
    /// before reserving the encoded buffer; no native or storage capability
    /// is issued by a successful encoding.
    #[allow(dead_code)] // COV2-24 consumes this inner encoder.
    pub(crate) fn encode_canonical(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut tuple_counter = BoundedTupleCounter {
            remaining: MAX_VERSIONED_PENDING_TUPLE_BYTES,
        };
        serde_json::to_writer(&mut tuple_counter, &self.identity_commit_associations)
            .map_err(|_| failure())?;
        let mut counter = BoundedTupleCounter {
            remaining: MAX_ENVELOPE,
        };
        serde_json::to_writer(&mut counter, self).map_err(|_| failure())?;
        let measured = MAX_ENVELOPE - counter.remaining;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(measured).map_err(|_| failure())?;
        serde_json::to_writer(&mut bytes, self).map_err(|_| failure())?;
        ensure!(bytes.len() == measured, REMEDY);
        preflight(&bytes, MAX_ENVELOPE, MAX_DEPTH)?;
        Ok(bytes)
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            self.redacted.version == VERSION
                && self.redacted.reasoning_source.is_none()
                && self.redacted.identity_commit_associations.is_none(),
            REMEDY
        );
        self.redacted.validate()?;
        if let Some(source) = &self.reasoning_source {
            source.validate_shape().map_err(|_| failure())?;
        }
        ensure!(
            !self.identity_commit_associations.is_empty()
                && self.identity_commit_associations.len() <= MAX_VERSIONED_PENDING_ENTRIES,
            REMEDY
        );
        let mut seen = HashSet::with_capacity(self.identity_commit_associations.len());
        for association in &self.identity_commit_associations {
            association.validate()?;
            ensure!(
                seen.insert((
                    association.coverage_schema_version,
                    association.logical_turn_key.as_str(),
                )),
                REMEDY
            );
        }
        Ok(())
    }

    pub(crate) fn matches_binding(&self, binding: &PendingBinding) -> Result<()> {
        ensure!(
            self.redacted.alias == binding.session_id
                && self
                    .redacted
                    .snapshot
                    .source
                    .as_ref()
                    .and_then(|source| source.digest_sha256.as_deref())
                    == Some(binding.source_commitment.as_str())
                && self
                    .redacted
                    .metadata
                    .get("checkpoint_id")
                    .and_then(Value::as_str)
                    == Some(binding.checkpoint_id.as_str())
                && self.redacted.lifecycle.iter().any(|event| {
                    event.get("event_id").and_then(Value::as_str) == Some(binding.event_id.as_str())
                }),
            REMEDY
        );
        Ok(())
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedPortableClaim {
    pub(crate) coverage_schema_version: i64,
    pub(crate) logical_turn_key: String,
    pub(crate) coverage_digest: String,
    pub(crate) completeness: Completeness,
    pub(crate) fence_token: i64,
    pub(crate) next_revision: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct VersionedPortablePendingCoverage {
    pub(crate) owner: String,
    pub(crate) created_at: i64,
    pub(crate) claims: Vec<VersionedPortableClaim>,
}

impl VersionedPortablePendingCoverage {
    #[allow(dead_code)] // COV2-24 connects the versioned reservation plan.
    pub(crate) fn from_live(
        plan: &VersionedLiveClaimCommitPlan,
        identity: &PreparedPendingAlias,
    ) -> Result<Self> {
        let ordinary = plan.ordinary();
        ensure!(
            !ordinary.claims.is_empty() && ordinary.claims.len() <= MAX_VERSIONED_PENDING_ENTRIES,
            REMEDY
        );
        ensure!(
            ordinary.source_channel == "live"
                && ordinary.import_identity.is_none()
                && ordinary.import_session.is_none()
                && ordinary.session_id == identity.context().session_id()
                && ordinary.capture_scope.as_ref() == Some(identity.context().scope()),
            REMEDY
        );
        let result = Self {
            owner: ordinary.owner.clone(),
            created_at: ordinary.created_at,
            claims: plan
                .claims()
                .map(|(version, claim)| VersionedPortableClaim {
                    coverage_schema_version: version.as_i64(),
                    logical_turn_key: claim.logical_turn_key.clone(),
                    coverage_digest: claim.coverage_digest.clone(),
                    completeness: claim.completeness,
                    fence_token: claim.fence_token,
                    next_revision: claim.next_revision,
                })
                .collect(),
        };
        result.validate()?;
        Ok(result)
    }

    #[allow(dead_code)] // COV2-22 consumes this memory-only replay plan.
    pub(crate) fn into_live(
        self,
        identity: &PreparedPendingAlias,
        checkpoint_id: &str,
        parent_commit: Option<String>,
        now_ms: i64,
    ) -> Result<VersionedLiveClaimCommitPlan> {
        self.validate()?;
        ensure!(canonical_uuid(checkpoint_id) && now_ms >= 0, REMEDY);
        let mut bound = Vec::with_capacity(self.claims.len());
        let mut ordinary = Vec::with_capacity(self.claims.len());
        for claim in self.claims {
            let version = claim.coverage_schema_version;
            let unversioned = ReservedTurnClaim {
                logical_turn_key: claim.logical_turn_key,
                coverage_digest: claim.coverage_digest,
                completeness: claim.completeness,
                fence_token: claim.fence_token,
                next_revision: claim.next_revision,
            };
            bound.push(VersionedReservedTurnClaim::new(
                version,
                unversioned.clone(),
            )?);
            ordinary.push(unversioned);
        }
        let plan = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: identity.context().session_id().into(),
            checkpoint_id: checkpoint_id.into(),
            owner: self.owner,
            parent_commit,
            created_at: self.created_at,
            now_ms,
            claims: ordinary,
            import_session: None,
            import_identity: None,
            capture_scope: Some(identity.context().scope().clone()),
        };
        VersionedLiveClaimCommitPlan::new(plan, &bound).map_err(|_| failure())
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.owner.is_empty()
                && self.owner.len() <= 512
                && !self.owner.chars().any(char::is_control)
                && self.created_at >= 0
                && !self.claims.is_empty()
                && self.claims.len() <= MAX_VERSIONED_PENDING_ENTRIES,
            REMEDY
        );
        let mut seen = HashSet::with_capacity(self.claims.len());
        for claim in &self.claims {
            CoverageSchemaVersion::try_from(claim.coverage_schema_version)
                .map_err(|_| failure())?;
            ensure!(
                !claim.logical_turn_key.is_empty()
                    && claim.logical_turn_key.len() <= MAX_PORTABLE_TURN_KEY_BYTES
                    && !claim.logical_turn_key.chars().any(char::is_control)
                    && claim.coverage_digest.len() == 64
                    && lower_hex(&claim.coverage_digest)
                    && claim.fence_token > 0
                    && claim.next_revision > 0
                    && seen.insert((
                        claim.coverage_schema_version,
                        claim.logical_turn_key.as_str()
                    )),
                REMEDY
            );
        }
        Ok(())
    }
}

struct BoundedArrayCount(usize);

impl<'de> Deserialize<'de> for BoundedArrayCount {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct Counter;
        impl<'de> Visitor<'de> for Counter {
            type Value = BoundedArrayCount;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a bounded pending tuple array")
            }

            fn visit_seq<A>(self, mut sequence: A) -> std::result::Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut count = 0;
                while let Some(_ignored) = sequence.next_element::<IgnoredAny>()? {
                    count += 1;
                    if count > MAX_VERSIONED_PENDING_ENTRIES {
                        return Err(serde::de::Error::custom("too many pending tuples"));
                    }
                }
                Ok(BoundedArrayCount(count))
            }
        }
        deserializer.deserialize_seq(Counter)
    }
}

/// Borrow raw array slices before typed `Vec` decoding. A hostile, correctly
/// MACed local artifact can still be malformed; count and wire bytes must be
/// checked before reserving claim/association storage.
pub(crate) fn preflight_versioned_tuples(
    payload_raw: &RawValue,
    coverage_raw: &RawValue,
) -> Result<()> {
    #[derive(Deserialize)]
    struct RawPayload<'a> {
        #[serde(borrow)]
        identity_commit_associations: &'a RawValue,
    }
    #[derive(Deserialize)]
    struct RawCoverage<'a> {
        #[serde(borrow)]
        claims: &'a RawValue,
    }
    let payload: RawPayload<'_> = serde_json::from_str(payload_raw.get()).map_err(|_| failure())?;
    let coverage: RawCoverage<'_> =
        serde_json::from_str(coverage_raw.get()).map_err(|_| failure())?;
    let associations = payload.identity_commit_associations.get();
    let claims = coverage.claims.get();
    ensure!(
        associations
            .len()
            .checked_add(claims.len())
            .is_some_and(|bytes| bytes <= MAX_VERSIONED_PENDING_TUPLE_BYTES),
        REMEDY
    );
    let associations_count: BoundedArrayCount =
        serde_json::from_str(associations).map_err(|_| failure())?;
    let claims_count: BoundedArrayCount = serde_json::from_str(claims).map_err(|_| failure())?;
    ensure!(
        associations_count.0 > 0 && associations_count.0 == claims_count.0,
        REMEDY
    );
    Ok(())
}

struct BoundedTupleCounter {
    remaining: usize,
}

impl Write for BoundedTupleCounter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "pending tuple capacity",
            ));
        }
        self.remaining -= bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Measure both tuple arrays without allocating their encoded bytes.
pub(crate) fn check_versioned_tuple_wire_capacity(
    payload: &VersionedPendingPayloadProjection,
    coverage: &VersionedPortablePendingCoverage,
) -> Result<()> {
    let mut counter = BoundedTupleCounter {
        remaining: MAX_VERSIONED_PENDING_TUPLE_BYTES,
    };
    serde_json::to_writer(&mut counter, &payload.identity_commit_associations)
        .map_err(|_| failure())?;
    serde_json::to_writer(&mut counter, &coverage.claims).map_err(|_| failure())?;
    Ok(())
}

impl PendingPayloadProjection {
    /// Project only a complete sealed payload. Reconstruction uses the same
    /// pretty metadata and whole JSONL buffer redaction as the live producer.
    /// Sidecar MACs are local keyed equality commitments, not source authority;
    /// they prevent current catalog/redactor changes from silently rewriting
    /// identity. The artifact owner still signs the complete outer envelope.
    pub(crate) async fn seal<C: ConnectionTrait>(
        conn: &C,
        storage: &Path,
        root: &Path,
        payload: &CheckpointRedactedPayload,
        identity: &PreparedPendingAlias,
        deadline: Instant,
    ) -> Result<Self> {
        payload.authorize_ordinary_pending_payload()?;
        Self::seal_base(conn, storage, root, payload, identity, deadline).await
    }

    pub(crate) async fn seal_bound_reasoning<C: ConnectionTrait>(
        conn: &C,
        storage: &Path,
        root: &Path,
        payload: &CheckpointRedactedPayload,
        identity: &PreparedPendingAlias,
        prepared: PreparedPendingNativeSeal<'_>,
        deadline: Instant,
    ) -> Result<Self> {
        Self::seal_base(conn, storage, root, payload, identity, deadline)
            .await?
            .with_bound_reasoning(prepared.batch, &prepared.binding, prepared.associations)
    }

    /// Only the pending artifact sealer supplies this payload-owned borrow and
    /// its source-derived companion. Bare logical keys cannot supply a version.
    #[cfg_attr(not(all(test, unix)), allow(dead_code))]
    pub(crate) async fn seal_bound_versioned_reasoning<C: ConnectionTrait>(
        conn: &C,
        storage: &Path,
        root: &Path,
        identity: &PreparedPendingAlias,
        material: VersionedPendingSealMaterial<'_>,
        deadline: Instant,
    ) -> Result<VersionedPendingPayloadProjection> {
        let (payload, prepared, companion) = material.into_parts();
        let claims = companion.versioned();
        let count = claims.claims().count();
        ensure!(
            count > 0
                && count <= crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES
                && prepared.associations.len() == count,
            REMEDY
        );
        let mut associations = Vec::with_capacity(count);
        for ((version, claim), (key, turn)) in claims.claims().zip(prepared.associations) {
            let expected = companion
                .reasoning()
                .turn(version, &claim.logical_turn_key)
                .ok_or_else(failure)?;
            ensure!(
                key == claim.logical_turn_key
                    && turn.slot() == expected.slot()
                    && turn.descriptor_mac() == expected.descriptor_mac()
                    && turn.kind() == expected.kind(),
                REMEDY
            );
            associations.push(VersionedPendingIdentityAssociation::from_prepared_turn(
                version, &key, &turn,
            ));
        }
        let source = seal_pending(prepared.batch, &prepared.binding).map_err(|_| failure())?;
        let redacted = Self::seal_base(conn, storage, root, payload, identity, deadline).await?;
        VersionedPendingPayloadProjection::new_codec(redacted, Some(source), associations)
    }

    async fn seal_base<C: ConnectionTrait>(
        conn: &C,
        storage: &Path,
        root: &Path,
        payload: &CheckpointRedactedPayload,
        identity: &PreparedPendingAlias,
        deadline: Instant,
    ) -> Result<Self> {
        ensure!(
            payload.is_exact_complete_snapshot() && payload.transcript().len() <= MAX_TRANSCRIPT,
            REMEDY
        );
        check_budget(deadline)?;
        let original = [
            payload.metadata_json().bytes(),
            payload.lifecycle_events_jsonl().bytes(),
            payload.redaction_report_json().bytes(),
        ];
        ensure!(sidecar_size(original)? <= MAX_SIDECARS, REMEDY);
        let mut metadata = json(payload.metadata_json().bytes(), MAX_SIDECARS)?;
        let fields = metadata.as_object_mut().ok_or_else(failure)?;
        remove_controls(
            fields,
            &["session_id", "provider_session_id", "working_dir"],
        )?;
        let metadata = normalize_json_value(metadata);
        let lifecycle_bytes = payload.lifecycle_events_jsonl().bytes();
        ensure!(
            !lifecycle_bytes.is_empty() && lifecycle_bytes.ends_with(b"\n"),
            REMEDY
        );
        let mut lifecycle = Vec::new();
        for line in lifecycle_bytes[..lifecycle_bytes.len() - 1].split(|byte| *byte == b'\n') {
            ensure!(!line.is_empty() && lifecycle.len() < MAX_EVENTS, REMEDY);
            let mut value = json(line, MAX_SIDECARS)?;
            remove_controls(
                value.as_object_mut().ok_or_else(failure)?,
                &["session_id", "provider_session_id"],
            )?;
            let value = normalize_json_value(value);
            lifecycle.push(value);
        }
        validate_report(&json(
            payload.redaction_report_json().bytes(),
            MAX_SIDECARS,
        )?)?;
        let mut projected = Self {
            version: VERSION,
            alias: identity.alias().to_owned(),
            snapshot: payload.snapshot().clone(),
            transcript: STANDARD.encode(payload.transcript().bytes()),
            metadata,
            lifecycle,
            redaction_report: STANDARD.encode(payload.redaction_report_json().bytes()),
            metadata_mac: String::new(),
            lifecycle_mac: String::new(),
            reasoning_source: None,
            identity_commit_associations: None,
        };
        projected.validate_data()?;
        let (metadata, lifecycle) = projected.rebuild(identity)?;
        ensure!(
            metadata.bytes() == original[0] && lifecycle.bytes() == original[1],
            REMEDY
        );
        for (kind, bytes, target) in [
            ("metadata", metadata.bytes(), &mut projected.metadata_mac),
            ("lifecycle", lifecycle.bytes(), &mut projected.lifecycle_mac),
        ] {
            *target = identity
                .context()
                .scope()
                .sign_pending_envelope_until(
                    conn,
                    storage,
                    root,
                    &sidecar_commitment_body(kind, bytes),
                    deadline,
                )
                .await
                .map_err(|error| untrusted_unless_retryable(error, deadline))?;
        }
        projected.validate()?;
        check_budget(deadline)?;
        Ok(projected)
    }

    /// Closed v2 codec entry. A deserialized DTO cannot call this constructor.
    fn with_bound_reasoning(
        self,
        batch: &NativeProvenReasoning,
        binding: &VerifiedPendingBinding,
        associations: Vec<(String, ReasoningTurnIdentity)>,
    ) -> Result<Self> {
        let associations = associations
            .iter()
            .map(|(key, turn)| PendingReasoningIdentityAssociation::from_prepared_turn(key, turn))
            .collect();
        self.with_reasoning_codec_parts(
            seal_pending(batch, binding).map_err(|_| failure())?,
            associations,
        )
    }

    #[cfg(test)]
    #[cfg(unix)]
    pub(crate) fn with_synthetic_reasoning(
        self,
        batch: &NativeProvenReasoning,
        binding: &VerifiedCaptureBinding,
        associations: Vec<PendingReasoningIdentityAssociation>,
    ) -> Result<Self> {
        self.with_reasoning_codec_parts(
            PendingReasoningSourceDto::from_bound_native(batch, binding).map_err(|_| failure())?,
            associations,
        )
    }

    fn with_reasoning_codec_parts(
        mut self,
        source: PendingReasoningSourceDto,
        associations: Vec<PendingReasoningIdentityAssociation>,
    ) -> Result<Self> {
        ensure!(
            self.version == VERSION && self.reasoning_source.is_none(),
            REMEDY
        );
        self.version = REASONING_VERSION;
        self.reasoning_source = Some(source);
        self.identity_commit_associations = Some(associations);
        self.validate()?;
        Ok(self)
    }

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(|_| failure())?;
        ensure!(bytes.len() <= MAX_ENVELOPE, REMEDY);
        preflight(&bytes, MAX_ENVELOPE, MAX_DEPTH)?;
        Ok(bytes)
    }

    #[cfg(test)]
    #[cfg_attr(windows, allow(dead_code))]
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        preflight(bytes, MAX_ENVELOPE, MAX_DEPTH)?;
        parse_canon_value(bytes).map_err(|_| failure())?;
        let value: Self = serde_json::from_slice(bytes).map_err(|_| failure())?;
        ensure!(value.encode()? == bytes, REMEDY);
        Ok(value)
    }

    /// ACF-10 supplies the outer MAC witness after reading chunks and validating
    /// its closed binding; ACF-12 must revalidate current receipt/coverage fences
    /// before publishing. The signed envelope must contain THIS exact `payload`
    /// component, not merely some authenticated unrelated bytes.
    pub(crate) async fn rehydrate<C: ConnectionTrait>(
        &self,
        authentication: &AuthenticatedPendingEnvelope<'_>,
        conn: &C,
        storage: &Path,
        root: &Path,
        identity: &PreparedPendingAlias,
        deadline: Instant,
    ) -> Result<CheckpointRedactedPayload> {
        // OG-21 must restore the authenticated native proof together with
        // ordinary payload. Silently projecting only redacted content would
        // discard the pending reasoning collection.
        ensure!(self.version != REASONING_VERSION, PENDING_V2_REMEDY);
        self.rehydrate_ordinary_components(authentication, conn, storage, root, identity, deadline)
            .await
    }

    pub(crate) async fn rehydrate_v2_deferred<C: ConnectionTrait>(
        &self,
        authentication: &AuthenticatedPendingEnvelope<'_>,
        conn: &C,
        storage: &Path,
        root: &Path,
        identity: &PreparedPendingAlias,
        deadline: Instant,
    ) -> Result<CheckpointRedactedPayload> {
        ensure!(self.version == REASONING_VERSION, REMEDY);
        self.rehydrate_ordinary_components(authentication, conn, storage, root, identity, deadline)
            .await
    }

    async fn rehydrate_ordinary_components<C: ConnectionTrait>(
        &self,
        authentication: &AuthenticatedPendingEnvelope<'_>,
        conn: &C,
        storage: &Path,
        root: &Path,
        identity: &PreparedPendingAlias,
        deadline: Instant,
    ) -> Result<CheckpointRedactedPayload> {
        self.validate()?;
        check_budget(deadline)?;
        ensure!(
            self.alias == identity.alias() && authentication.scope() == identity.context().scope(),
            REMEDY
        );
        verify_payload_membership(authentication.bytes(), self)?;
        self.rehydrate_authenticated_components(conn, storage, root, identity, deadline)
            .await
    }

    /// Private common byte restoration after the appropriate outer member
    /// gate. Neither call path restores a native-origin capability.
    async fn rehydrate_authenticated_components<C: ConnectionTrait>(
        &self,
        conn: &C,
        storage: &Path,
        root: &Path,
        identity: &PreparedPendingAlias,
        deadline: Instant,
    ) -> Result<CheckpointRedactedPayload> {
        check_budget(deadline)?;
        let (metadata, lifecycle) = self.rebuild(identity)?;
        for (kind, bytes, mac) in [
            ("metadata", metadata.bytes(), &self.metadata_mac),
            ("lifecycle", lifecycle.bytes(), &self.lifecycle_mac),
        ] {
            identity
                .context()
                .scope()
                .verify_pending_envelope_until(
                    conn,
                    storage,
                    root,
                    &sidecar_commitment_body(kind, bytes),
                    mac,
                    deadline,
                )
                .await
                .map_err(|error| untrusted_unless_retryable(error, deadline))?;
        }
        let transcript = decoded(&self.transcript, MAX_TRANSCRIPT)?;
        let report = decoded(&self.redaction_report, MAX_SIDECARS)?;
        ensure!(
            sidecar_size([metadata.bytes(), lifecycle.bytes(), &report])? <= MAX_SIDECARS,
            REMEDY
        );
        let payload = CheckpointRedactedPayload::from_redacted_capture(
            RedactedBytes::new_unchecked(transcript),
            Some(self.snapshot.clone()),
            metadata,
            lifecycle,
            RedactedBytes::new_unchecked(report),
        )
        .map_err(|_| failure())?;
        ensure!(payload.is_exact_complete_snapshot(), REMEDY);
        check_budget(deadline)?;
        Ok(payload)
    }

    pub(crate) fn is_reasoning_v2(&self) -> bool {
        self.version == REASONING_VERSION
    }

    pub(crate) fn into_deferred_reasoning(
        self,
        original_envelope: Vec<u8>,
        original_mac: String,
        integrity: PendingIntegrityToken,
    ) -> Result<PendingReasoningReplay> {
        ensure!(
            self.version == REASONING_VERSION && original_envelope.len() <= MAX_ENVELOPE,
            REMEDY
        );
        Ok(PendingReasoningReplay {
            original_envelope,
            original_mac,
            source: self.reasoning_source.ok_or_else(failure)?,
            associations: self.identity_commit_associations.ok_or_else(failure)?,
            integrity: integrity.into_stamp(),
        })
    }

    fn rebuild(&self, identity: &PreparedPendingAlias) -> Result<(RedactedBytes, RedactedBytes)> {
        ensure!(self.alias == identity.alias(), REMEDY);
        let context = identity.context();
        let mut metadata = self.metadata.clone();
        let fields = metadata.as_object_mut().ok_or_else(failure)?;
        fields.insert(
            "session_id".into(),
            Value::String(context.session_id().into()),
        );
        fields.insert(
            "provider_session_id".into(),
            Value::String(context.provider_session_id().into()),
        );
        fields.insert(
            "working_dir".into(),
            Value::String(context.working_dir().into()),
        );
        let metadata =
            serde_json::to_vec_pretty(&normalize_json_value(metadata)).map_err(|_| failure())?;
        ensure!(metadata.len() <= MAX_SIDECARS, REMEDY);
        let mut lifecycle = Vec::new();
        for event in &self.lifecycle {
            let mut event = event.clone();
            let fields = event.as_object_mut().ok_or_else(failure)?;
            fields.insert(
                "session_id".into(),
                Value::String(context.session_id().into()),
            );
            fields.insert(
                "provider_session_id".into(),
                Value::String(context.provider_session_id().into()),
            );
            let line = serde_json::to_vec(&normalize_json_value(event)).map_err(|_| failure())?;
            ensure!(
                lifecycle
                    .len()
                    .checked_add(line.len())
                    .and_then(|n| n.checked_add(1))
                    .is_some_and(|n| n <= MAX_SIDECARS),
                REMEDY
            );
            lifecycle.extend_from_slice(&line);
            lifecycle.push(b'\n');
        }
        let redactor = Redactor::new_default();
        Ok((redactor.redact(&metadata).0, redactor.redact(&lifecycle).0))
    }

    fn validate_data(&self) -> Result<()> {
        ensure!(
            matches!(self.version, VERSION | REASONING_VERSION) && canonical_alias(&self.alias),
            REMEDY
        );
        match self.version {
            VERSION => ensure!(
                self.reasoning_source.is_none() && self.identity_commit_associations.is_none(),
                REMEDY
            ),
            REASONING_VERSION => {
                let source = self.reasoning_source.as_ref().ok_or_else(failure)?;
                source.validate_shape().map_err(|_| failure())?;
                let associations = self
                    .identity_commit_associations
                    .as_ref()
                    .ok_or_else(failure)?;
                ensure!(
                    !associations.is_empty() && associations.len() <= MAX_PORTABLE_CLAIMS,
                    REMEDY
                );
                let source_turns: HashSet<&str> = source.logical_turns().collect();
                let mut dispositions = HashMap::with_capacity(associations.len());
                for association in associations {
                    ensure!(
                        !association.logical_turn_key.is_empty()
                            && association.logical_turn_key.len() <= MAX_PORTABLE_TURN_KEY_BYTES
                            && !association.logical_turn_key.chars().any(char::is_control)
                            && dispositions
                                .insert(
                                    association.logical_turn_key.as_str(),
                                    association.disposition,
                                )
                                .is_none()
                            && valid_reasoning_identity_mac(&association.slot)
                            && valid_reasoning_identity_mac(&association.descriptor_mac),
                        REMEDY
                    );
                }
                for (key, disposition) in dispositions {
                    ensure!(
                        source_turns.contains(key)
                            == (disposition
                                == PendingReasoningIdentityDisposition::VerifiedNonempty),
                        REMEDY
                    );
                }
            }
            _ => return Err(failure()),
        }
        let transcript = decoded(&self.transcript, MAX_TRANSCRIPT)?;
        ensure!(
            self.snapshot.has_durable_source_commitment()
                && self.snapshot.transcript_redacted_bytes == transcript.len()
                && self.snapshot.completeness
                    == super::snapshot::CaptureSnapshotCompleteness::Complete
                && self.snapshot.partial_reason.is_none()
                && self
                    .snapshot
                    .source
                    .as_ref()
                    .is_some_and(|s| s.identity == "not_retained:v1"),
            REMEDY
        );
        let fields = closed_object(&self.metadata, METADATA_FIELDS)?;
        for key in &METADATA_FIELDS[..8] {
            ensure!(fields.contains_key(*key), REMEDY);
        }
        ensure!(
            fields["schema_version"].as_u64() == Some(2)
                && fields["scope"].as_str() == Some("committed")
                && fields["created_at"].as_i64().is_some_and(|n| n >= 0)
                && valid_string(&fields["agent_kind"], 96)
                && fields["checkpoint_id"].as_str().is_some_and(canonical_uuid),
            REMEDY
        );
        validate_report(&fields["redaction_report"])?;
        if let Some(snapshot) = fields.get("transcript_snapshot") {
            ensure!(
                *snapshot == serde_json::to_value(&self.snapshot).map_err(|_| failure())?,
                REMEDY
            );
        }
        ensure!(
            !self.lifecycle.is_empty() && self.lifecycle.len() <= MAX_EVENTS,
            REMEDY
        );
        for event in &self.lifecycle {
            let fields = closed_object(event, EVENT_FIELDS)?;
            for key in &EVENT_FIELDS[..9] {
                ensure!(fields.contains_key(*key), REMEDY);
            }
            ensure!(
                fields["schema_version"].as_u64() == Some(2)
                    && fields["event_id"].as_str().is_some_and(canonical_uuid)
                    && valid_string(&fields["identity_scheme"], 96)
                    && valid_string(&fields["kind"], 96)
                    && valid_string(&fields["agent_kind"], 96)
                    && valid_string(&fields["timestamp"], 128)
                    && fields["partial"].is_boolean(),
                REMEDY
            );
        }
        let report = decoded(&self.redaction_report, MAX_SIDECARS)?;
        validate_report(&json(&report, MAX_SIDECARS)?)?;
        let metadata = serde_json::to_vec(&self.metadata).map_err(|_| failure())?;
        let lifecycle = serde_json::to_vec(&self.lifecycle).map_err(|_| failure())?;
        let snapshot = serde_json::to_vec(&self.snapshot).map_err(|_| failure())?;
        ensure!(
            sidecar_size([metadata.as_slice(), lifecycle.as_slice(), report.as_slice()])?
                .checked_add(snapshot.len())
                .is_some_and(|n| n <= MAX_SIDECARS),
            REMEDY
        );
        for bytes in [&metadata, &lifecycle, &snapshot] {
            preflight(bytes, MAX_SIDECARS, MAX_DEPTH)?;
        }
        Ok(())
    }

    fn validate(&self) -> Result<()> {
        self.validate_data()?;
        ensure!(
            valid_mac(&self.metadata_mac) && valid_mac(&self.lifecycle_mac),
            REMEDY
        );
        Ok(())
    }
}

/// Semantic coverage proof only. A native catalog PK is never serialized here.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PortablePendingCoverage {
    owner: String,
    created_at: i64,
    claims: Vec<PortableClaim>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PortableClaim {
    logical_turn_key: String,
    coverage_digest: String,
    completeness: Completeness,
    fence_token: i64,
    next_revision: i64,
}

impl PortablePendingCoverage {
    pub(crate) fn from_live(
        plan: &LiveClaimCommitPlan,
        identity: &PreparedPendingAlias,
    ) -> Result<Self> {
        ensure!(
            plan.source_channel == "live"
                && plan.import_identity.is_none()
                && plan.import_session.is_none()
                && plan.session_id == identity.context().session_id()
                && plan.capture_scope.as_ref() == Some(identity.context().scope()),
            REMEDY
        );
        ensure!(
            plan.owner.len() <= 512 && plan.claims.len() <= MAX_PORTABLE_CLAIMS,
            REMEDY
        );
        for claim in &plan.claims {
            ensure!(
                claim.logical_turn_key.len() <= MAX_PORTABLE_TURN_KEY_BYTES
                    && claim.coverage_digest.len() == 64,
                REMEDY
            );
        }
        let proof = Self {
            owner: plan.owner.clone(),
            created_at: plan.created_at,
            claims: plan
                .claims
                .iter()
                .map(|claim| PortableClaim {
                    logical_turn_key: claim.logical_turn_key.clone(),
                    coverage_digest: claim.coverage_digest.clone(),
                    completeness: claim.completeness,
                    fence_token: claim.fence_token,
                    next_revision: claim.next_revision,
                })
                .collect(),
        };
        proof.validate()?;
        Ok(proof)
    }

    pub(crate) fn into_live(
        self,
        identity: &PreparedPendingAlias,
        checkpoint_id: &str,
        parent_commit: Option<String>,
        now_ms: i64,
    ) -> Result<LiveClaimCommitPlan> {
        self.validate()?;
        ensure!(canonical_uuid(checkpoint_id) && now_ms >= 0, REMEDY);
        Ok(LiveClaimCommitPlan {
            source_channel: "live",
            session_id: identity.context().session_id().into(),
            checkpoint_id: checkpoint_id.into(),
            owner: self.owner,
            parent_commit,
            created_at: self.created_at,
            now_ms,
            claims: self
                .claims
                .into_iter()
                .map(|claim| ReservedTurnClaim {
                    logical_turn_key: claim.logical_turn_key,
                    coverage_digest: claim.coverage_digest,
                    completeness: claim.completeness,
                    fence_token: claim.fence_token,
                    next_revision: claim.next_revision,
                })
                .collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(identity.context().scope().clone()),
        })
    }

    pub(crate) fn validate(&self) -> Result<()> {
        ensure!(
            !self.owner.is_empty()
                && self.owner.len() <= 512
                && !self.owner.chars().any(char::is_control)
                && self.created_at >= 0
                && self.claims.len() <= MAX_PORTABLE_CLAIMS,
            REMEDY
        );
        let mut keys = HashSet::new();
        for claim in &self.claims {
            ensure!(
                !claim.logical_turn_key.is_empty()
                    && claim.logical_turn_key.len() <= MAX_PORTABLE_TURN_KEY_BYTES
                    && !claim.logical_turn_key.chars().any(char::is_control)
                    && claim.coverage_digest.len() == 64
                    && lower_hex(&claim.coverage_digest)
                    && claim.fence_token > 0
                    && claim.next_revision > 0
                    && keys.insert(&claim.logical_turn_key),
                REMEDY
            );
        }
        Ok(())
    }
}

fn failure() -> anyhow::Error {
    anyhow::anyhow!(REMEDY)
}
/// A sidecar equality-tag mismatch is the projection's fixed, content-free
/// trust failure. Typed transient causes (database, lease, deadline, retryable
/// I/O) keep their chain so the recovery classifier can defer, not quarantine.
fn untrusted_unless_retryable(error: anyhow::Error, deadline: Instant) -> anyhow::Error {
    if crate::internal::ai::capture::pending::retryable_load_failure(&error, deadline) {
        error
    } else {
        failure()
    }
}
fn verify_payload_membership(bytes: &[u8], payload: &PendingPayloadProjection) -> Result<()> {
    preflight(bytes, MAX_ENVELOPE, MAX_DEPTH + 2)?;
    // CanonValue detects duplicate keys, but its numeric comparison can lose
    // u64 precision. Compare canonical JSON bytes with serde's exact integers.
    parse_canon_value(bytes).map_err(|_| failure())?;
    let envelope: Value = serde_json::from_slice(bytes).map_err(|_| failure())?;
    let component = envelope.get("payload").ok_or_else(failure)?;
    // Use Value's active map policy on both sides rather than mixing struct
    // declaration order with object-map serialization.
    let expected = serde_json::to_value(payload).map_err(|_| failure())?;
    ensure!(
        serde_json::to_vec(component).map_err(|_| failure())?
            == serde_json::to_vec(&expected).map_err(|_| failure())?,
        REMEDY
    );
    Ok(())
}
fn check_budget(deadline: Instant) -> Result<()> {
    if Instant::now() >= deadline {
        return Err(anyhow::Error::new(
            crate::internal::ai::capture::catalog::CaptureCatalogError::DeadlineExceeded,
        )
        .context(REMEDY));
    }
    Ok(())
}
fn canonical_uuid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok_and(|u| u.to_string() == s)
}
fn canonical_alias(s: &str) -> bool {
    Uuid::parse_str(s).is_ok_and(|u| {
        u.to_string() == s && u.get_version_num() == 4 && u.get_variant() == uuid::Variant::RFC4122
    })
}
fn lower_hex(s: &str) -> bool {
    s.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn valid_mac(s: &str) -> bool {
    s.strip_prefix("pending-envelope/hmac-v1/")
        .is_some_and(|s| s.len() == 64 && lower_hex(s))
}
fn valid_reasoning_identity_mac(s: &str) -> bool {
    s.strip_prefix("reasoning-identity/hmac-v1/")
        .is_some_and(|digest| digest.len() == 64 && lower_hex(digest))
}
fn valid_string(value: &Value, cap: usize) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.is_empty() && s.len() <= cap && !s.chars().any(char::is_control))
}

fn sidecar_commitment_body(kind: &str, bytes: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(bytes.len() + 64);
    body.extend_from_slice(b"libra-pending-payload-sidecar-v1\0");
    body.extend_from_slice(kind.as_bytes());
    body.push(0);
    body.extend_from_slice(bytes);
    body
}
fn sidecar_size<const N: usize>(parts: [&[u8]; N]) -> Result<usize> {
    parts.iter().try_fold(0usize, |n, part| {
        n.checked_add(part.len()).ok_or_else(failure)
    })
}
fn decoded(text: &str, cap: usize) -> Result<Vec<u8>> {
    ensure!(text.len() <= cap.div_ceil(3) * 4, REMEDY);
    let bytes = STANDARD.decode(text).map_err(|_| failure())?;
    ensure!(
        bytes.len() <= cap && STANDARD.encode(&bytes) == text,
        REMEDY
    );
    Ok(bytes)
}
fn json(bytes: &[u8], cap: usize) -> Result<Value> {
    preflight(bytes, cap, MAX_DEPTH)?;
    parse_canon_value(bytes).map_err(|_| failure())?;
    serde_json::from_slice(bytes).map_err(|_| failure())
}
/// Check depth/bytes before allocating a JSON tree. The actual parser checks
/// delimiter syntax and duplicate keys; this scanner only provides bounds.
fn preflight(bytes: &[u8], cap: usize, depth_cap: usize) -> Result<()> {
    ensure!(!bytes.is_empty() && bytes.len() <= cap, REMEDY);
    let (mut depth, mut quoted, mut escaped) = (0usize, false, false);
    for byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if *byte == b'\\' {
                escaped = true;
            } else if *byte == b'"' {
                quoted = false;
            }
        } else {
            match *byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    ensure!(depth <= depth_cap, REMEDY);
                }
                b'}' | b']' => {
                    ensure!(depth > 0, REMEDY);
                    depth -= 1;
                }
                _ => (),
            }
        }
    }
    ensure!(depth == 0 && !quoted, REMEDY);
    Ok(())
}
fn remove_controls(fields: &mut Map<String, Value>, keys: &[&str]) -> Result<()> {
    for key in keys {
        ensure!(fields.remove(*key).is_some_and(|v| v.is_string()), REMEDY);
    }
    Ok(())
}
fn closed_object<'a>(value: &'a Value, allowed: &[&str]) -> Result<&'a Map<String, Value>> {
    let fields = value.as_object().ok_or_else(failure)?;
    ensure!(fields.keys().all(|k| allowed.contains(&k.as_str())), REMEDY);
    Ok(fields)
}
fn validate_report(value: &Value) -> Result<()> {
    let fields = closed_object(
        value,
        &[
            "matches",
            "bytes_scanned",
            "bytes_redacted",
            "dropped_matches",
        ],
    )?;
    let scanned = fields
        .get("bytes_scanned")
        .and_then(Value::as_u64)
        .ok_or_else(failure)?;
    let redacted = fields
        .get("bytes_redacted")
        .and_then(Value::as_u64)
        .ok_or_else(failure)?;
    ensure!(redacted <= scanned, REMEDY);
    if let Some(dropped) = fields.get("dropped_matches") {
        ensure!(dropped.as_u64().is_some(), REMEDY);
    }
    let matches = fields
        .get("matches")
        .and_then(Value::as_array)
        .ok_or_else(failure)?;
    ensure!(matches.len() <= MAX_REDACTION_MATCH_SAMPLES, REMEDY);
    for item in matches {
        let fields = closed_object(item, &["rule_id", "start", "end"])?;
        ensure!(
            fields.get("rule_id").is_some_and(|v| valid_string(v, 128)),
            REMEDY
        );
        let start = fields
            .get("start")
            .and_then(Value::as_u64)
            .ok_or_else(failure)?;
        let end = fields
            .get("end")
            .and_then(Value::as_u64)
            .ok_or_else(failure)?;
        ensure!(start <= end && end <= scanned, REMEDY);
    }
    Ok(())
}

#[cfg(all(test, unix))]
pub(crate) async fn assert_reasoning_pending_seal_rejected(payload: &CheckpointRedactedPayload) {
    tests::assert_reasoning_seal_rejected(payload).await;
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    pub(super) async fn assert_reasoning_seal_rejected(payload: &CheckpointRedactedPayload) {
        let fixture = Fixture::new().await;
        fixture.seed("og16-pending", "og16-pending-native").await;
        let identity = fixture.identity("og16-pending", None).await;
        let before = walkdir::WalkDir::new(fixture.root.path())
            .into_iter()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .collect::<std::collections::BTreeSet<_>>();
        let error = PendingPayloadProjection::seal(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            payload,
            &identity,
            deadline(),
        )
        .await
        .err()
        .expect("ordinary V1 seal cannot encode a nonordinary carrier");
        assert!(format!("{error:#}").contains("reasoning pending consumer"));
        let after = walkdir::WalkDir::new(fixture.root.path())
            .into_iter()
            .map(|e| e.unwrap())
            .filter(|e| e.file_type().is_file())
            .map(|e| e.path().to_path_buf())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            before, after,
            "rejected seal must not create any envelope or MAC file"
        );
        assert!(
            payload.authorize_ordinary_pending_payload().is_err(),
            "rejection keeps the original borrowed payload"
        );
    }

    use std::{path::PathBuf, time::Duration};

    use sea_orm::{DatabaseConnection, Statement};
    use tempfile::TempDir;

    use super::*;
    #[test]
    fn elapsed_projection_budget_preserves_typed_catalog_error() {
        let error = check_budget(Instant::now() - Duration::from_millis(1))
            .expect_err("expired payload budget must fail");
        assert!(error.chain().any(|cause| {
            cause.downcast_ref::<crate::internal::ai::capture::catalog::CaptureCatalogError>()
                == Some(
                    &crate::internal::ai::capture::catalog::CaptureCatalogError::DeadlineExceeded,
                )
        }));
    }
    use crate::internal::{
        ai::{
            capture::{
                catalog::resolve_pending_session_context,
                key,
                pending_identity::{self, PendingSessionAlias},
                snapshot::CaptureSnapshotService,
            },
            capture_scope::CaptureScope,
            observed_agents::{ExportAuthorized, TranscriptSource},
        },
        config::ConfigKv,
        db,
        metadata::{MetadataKv, MetadataScope, MetadataValueType},
    };

    struct Fixture {
        root: TempDir,
        storage: PathBuf,
        conn: DatabaseConnection,
        scope: CaptureScope,
    }

    impl Fixture {
        async fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let storage = root.path().join(".libra");
            std::fs::create_dir_all(storage.join("objects")).unwrap();
            let conn = db::create_database(storage.join("libra.db").to_str().unwrap())
                .await
                .unwrap();
            ConfigKv::set_with_conn(&conn, "libra.repoid", "portable-test-repo", false)
                .await
                .unwrap();
            key::load_capture_dedup_secret(&storage).unwrap();
            let scope = CaptureScope::resolve(&conn, root.path()).await.unwrap();
            let fixture = Self {
                root,
                storage,
                conn,
                scope,
            };
            fixture
                .seed("claude__native-session", "native-session")
                .await;
            fixture
        }

        async fn seed(&self, pk: &str, native: &str) {
            self.conn
                .execute_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
                 state, working_dir, metadata_json, started_at, last_event_at,
                 sync_revision, repo_id, worktree_id, scope_state)
                 VALUES (?, 'claude_code', ?, 'active', ?, '{}', 1, 1, 1, ?, '', 'scoped')",
                    [
                        pk.into(),
                        native.into(),
                        self.root.path().to_string_lossy().into_owned().into(),
                        self.scope.repo_id.clone().into(),
                    ],
                ))
                .await
                .unwrap();
        }

        async fn identity(&self, pk: &str, alias: Option<&str>) -> PreparedPendingAlias {
            let txn = db::begin_write_transaction(&self.conn).await.unwrap();
            let context = resolve_pending_session_context(&txn, &self.scope, pk)
                .await
                .unwrap();
            txn.commit().await.unwrap();
            let existing = match alias {
                Some(alias) => pending_identity::lookup(&self.conn, &self.scope.repo_id, alias)
                    .await
                    .unwrap(),
                None => None,
            };
            PendingSessionAlias::prepare(
                &self.conn,
                &context,
                existing,
                &self.storage,
                self.root.path(),
                deadline(),
            )
            .await
            .unwrap()
        }

        /// An actual MAC-authenticated registry fixture, not a public seed API
        /// and not evidence of ACF-10 atomic artifact publication.
        async fn retain_alias(&self, identity: &PreparedPendingAlias) {
            #[derive(Serialize)]
            struct Body<'a> {
                version: u8,
                alias: &'a str,
                session_id: &'a str,
                repo_id: &'a str,
                worktree_id: &'a str,
                workspace_id: Option<&'a str>,
                capture_incarnation: Option<&'a str>,
            }
            #[derive(Serialize)]
            struct Record<'a> {
                body: Body<'a>,
                mac: String,
            }
            let context = identity.context();
            let body = Body {
                version: 1,
                alias: identity.alias(),
                session_id: context.session_id(),
                repo_id: &context.scope().repo_id,
                worktree_id: &context.scope().worktree_id,
                workspace_id: context.scope().workspace_id.as_deref(),
                capture_incarnation: context.incarnation(),
            };
            let bytes = serde_json::to_vec(&body).unwrap();
            let mac = key::authenticate_pending_alias_in_scope_until(
                &self.conn,
                &self.scope,
                &self.storage,
                self.root.path(),
                &bytes,
                None,
                deadline(),
            )
            .await
            .unwrap();
            let text = serde_json::to_string(&Record { body, mac }).unwrap();
            MetadataKv::set_with_conn(
                &self.conn,
                MetadataScope::AgentCaptureSessionAlias,
                &self.scope.repo_id,
                identity.alias(),
                &text,
                MetadataValueType::Text,
            )
            .await
            .unwrap();
        }

        async fn payload(&self, identity: &PreparedPendingAlias) -> CheckpointRedactedPayload {
            let bytes =
                b"redacted capture includes a key AKIAABCDEFGHIJKLMNOP and useful tool data"
                    .to_vec();
            let auth = ExportAuthorized::issue(
                "claude_code",
                identity.context().provider_session_id(),
                &bytes,
            );
            let mut snapshot = CaptureSnapshotService::capture_authorized(
                TranscriptSource::Bytes { bytes, auth },
                "claude_code",
                identity.context().provider_session_id(),
                Default::default(),
            );
            let source_mac = crate::internal::ai::capture::key::derive_snapshot_content_commitment_in_scope_until(
                &self.conn,
                &self.scope,
                &self.storage,
                self.root.path(),
                &snapshot.redacted_digest_preimage().unwrap(),
                deadline(),
            )
            .await
            .unwrap();
            assert!(snapshot.bind_source_commitment(source_mac));
            let context = identity.context();
            let report = serde_json::to_value(snapshot.redaction_report()).unwrap();
            let metadata = serde_json::json!({
                "schema_version": 2, "checkpoint_id": Uuid::new_v4().to_string(),
                "session_id": context.session_id(), "provider_session_id": context.provider_session_id(),
                "working_dir": context.working_dir(), "agent_kind": "claude_code", "scope": "committed",
                "model": {"name":"test"}, "created_at": 1, "redaction_report": report,
                "transcript_snapshot": snapshot.safe_projection(),
                "extraction": {"edited_files":["src/test.rs"], "prompt":"ordinary user content"},
            });
            use crate::internal::ai::hooks::lifecycle::{
                CanonicalEventContext, LifecycleEvent, LifecycleEventKind, LifecycleIdentityScheme,
                lifecycle_event_canonical_json_with_identity,
            };
            let event = LifecycleEvent {
                kind: LifecycleEventKind::SessionEnd,
                session_id: context.provider_session_id().into(),
                session_ref: Some("excluded native source locator".into()),
                prompt: None,
                model: None,
                source: None,
                tool_name: None,
                tool_input: Some(
                    serde_json::json!({"cwd":"nested cwd data", "working_dir":"nested directory data", "session_id":"user data"}),
                ),
                tool_response: Some(serde_json::json!({"assistant_message":"already safe"})),
                assistant_message: None,
                timestamp: chrono::DateTime::from_timestamp(1, 0).unwrap(),
            };
            let event = lifecycle_event_canonical_json_with_identity(
                &event,
                &CanonicalEventContext {
                    agent_kind: "claude_code",
                    session_id: context.session_id(),
                    provider_session_id: context.provider_session_id(),
                    identity_scheme: LifecycleIdentityScheme::NativeReplayHmacV2,
                    provenance: serde_json::json!({"channel":"hook", "hook_event_name":"SessionEnd"}),
                },
                Uuid::new_v4(),
                false,
            );
            let mut jsonl = serde_json::to_vec(&event).unwrap();
            jsonl.push(b'\n');
            let mut second_event = event.clone();
            second_event["event_id"] = Value::String(Uuid::new_v4().to_string());
            second_event["tool_response"] =
                serde_json::json!({"result":"second event AKIAABCDEFGHIJKLMNOP"});
            jsonl.extend_from_slice(&serde_json::to_vec(&second_event).unwrap());
            jsonl.push(b'\n');
            CheckpointRedactedPayload::from_snapshot(
                snapshot,
                redact(&serde_json::to_vec_pretty(&normalize_json_value(metadata)).unwrap()),
                redact(&jsonl),
                redact(&serde_json::to_vec_pretty(&normalize_json_value(report)).unwrap()),
            )
            .unwrap()
        }

        async fn seal(
            &self,
            payload: &CheckpointRedactedPayload,
            identity: &PreparedPendingAlias,
        ) -> PendingPayloadProjection {
            PendingPayloadProjection::seal(
                &self.conn,
                &self.storage,
                self.root.path(),
                payload,
                identity,
                deadline(),
            )
            .await
            .unwrap()
        }

        async fn signed(&self, projection: &PendingPayloadProjection) -> (Vec<u8>, String) {
            let bytes =
                serde_json::to_vec(&serde_json::json!({"payload": projection, "version": 1}))
                    .unwrap();
            let mac = self
                .scope
                .sign_pending_envelope_until(
                    &self.conn,
                    &self.storage,
                    self.root.path(),
                    &bytes,
                    deadline(),
                )
                .await
                .unwrap();
            (bytes, mac)
        }

        async fn rehydrate(
            &self,
            projection: &PendingPayloadProjection,
            identity: &PreparedPendingAlias,
        ) -> Result<CheckpointRedactedPayload> {
            let (bytes, mac) = self.signed(projection).await;
            let proof = AuthenticatedPendingEnvelope::verify(
                &self.conn,
                &self.storage,
                self.root.path(),
                &self.scope,
                &bytes,
                &mac,
                deadline(),
            )
            .await?;
            projection
                .rehydrate(
                    &proof,
                    &self.conn,
                    &self.storage,
                    self.root.path(),
                    identity,
                    deadline(),
                )
                .await
        }
    }

    fn deadline() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }
    fn redact(bytes: &[u8]) -> RedactedBytes {
        Redactor::new_default().redact(bytes).0
    }
    fn invalid_message<T>(result: Result<T>) {
        assert_eq!(
            result.err().expect("reject unsafe payload").to_string(),
            REMEDY
        );
    }

    fn changed(
        payload: &CheckpointRedactedPayload,
        metadata: Vec<u8>,
        events: Vec<u8>,
        report: Vec<u8>,
    ) -> CheckpointRedactedPayload {
        CheckpointRedactedPayload::from_redacted_capture(
            redact(payload.transcript().bytes()),
            Some(payload.snapshot().clone()),
            redact(&metadata),
            redact(&events),
            redact(&report),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn projection_contains_no_native_control_identity() {
        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        let projection = fixture.seal(&payload, &identity).await;
        let text = String::from_utf8(projection.encode().unwrap()).unwrap();
        for private in [
            identity.context().session_id(),
            identity.context().provider_session_id(),
            identity.context().working_dir(),
        ] {
            assert!(
                !text.contains(private),
                "native control values are local-only"
            );
        }
        assert!(projection.metadata.get("session_id").is_none());
        assert!(projection.lifecycle[0].get("provider_session_id").is_none());
        for key in [
            "cwd",
            "session_ref",
            "transcript_path",
            "transcript_locator",
            "identity_wrapper",
        ] {
            let mut metadata: Value =
                serde_json::from_slice(payload.metadata_json().bytes()).unwrap();
            metadata[key] = serde_json::json!({"session_id":"native-session"});
            let altered = changed(
                &payload,
                serde_json::to_vec_pretty(&metadata).unwrap(),
                payload.lifecycle_events_jsonl().bytes().to_vec(),
                payload.redaction_report_json().bytes().to_vec(),
            );
            invalid_message(
                PendingPayloadProjection::seal(
                    &fixture.conn,
                    &fixture.storage,
                    fixture.root.path(),
                    &altered,
                    &identity,
                    deadline(),
                )
                .await,
            );
        }
        // The closed E3 field set applies to every top-level lifecycle field:
        // a native locator or wrapper object cannot ride along in an event.
        let events = payload
            .lifecycle_events_jsonl()
            .bytes()
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(events.len(), 2);
        let with_events = |events: &[Value]| {
            let mut jsonl = Vec::new();
            for event in events {
                jsonl.extend_from_slice(&serde_json::to_vec(event).unwrap());
                jsonl.push(b'\n');
            }
            changed(
                &payload,
                payload.metadata_json().bytes().to_vec(),
                jsonl,
                payload.redaction_report_json().bytes().to_vec(),
            )
        };
        // Positive control: the unmodified re-encoded events still seal, so
        // each rejection below is caused by the injected top-level field.
        PendingPayloadProjection::seal(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            &with_events(&events),
            &identity,
            deadline(),
        )
        .await
        .unwrap();
        for (index, key) in [
            "cwd",
            "session_ref",
            "transcript_path",
            "transcript_locator",
            "identity_wrapper",
        ]
        .into_iter()
        .enumerate()
        {
            let mut altered = events.clone();
            altered[index % events.len()][key] = Value::String("native locator".into());
            invalid_message(
                PendingPayloadProjection::seal(
                    &fixture.conn,
                    &fixture.storage,
                    fixture.root.path(),
                    &with_events(&altered),
                    &identity,
                    deadline(),
                )
                .await,
            );
        }
        let mut wrapped = events.clone();
        let inner = wrapped[0].clone();
        wrapped[0] = serde_json::json!({
            "session_id": inner["session_id"],
            "provider_session_id": inner["provider_session_id"],
            "wrapper": inner,
        });
        invalid_message(
            PendingPayloadProjection::seal(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &with_events(&wrapped),
                &identity,
                deadline(),
            )
            .await,
        );
        let metadata = payload.metadata_json().bytes().to_vec();
        let renamed = String::from_utf8(metadata)
            .unwrap()
            .replace("\"session_id\"", "\"native_session_id\"");
        let altered = changed(
            &payload,
            renamed.into_bytes(),
            payload.lifecycle_events_jsonl().bytes().to_vec(),
            payload.redaction_report_json().bytes().to_vec(),
        );
        invalid_message(
            PendingPayloadProjection::seal(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &altered,
                &identity,
                deadline(),
            )
            .await,
        );
    }

    #[tokio::test]
    async fn transcript_and_snapshot_are_preserved_exactly() {
        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        let projected = fixture.seal(&payload, &identity).await;
        assert_eq!(
            decoded(&projected.transcript, MAX_TRANSCRIPT).unwrap(),
            payload.transcript().bytes()
        );
        assert_eq!(projected.snapshot, *payload.snapshot());
        assert_eq!(
            projected.lifecycle[0]["tool_input"]["cwd"],
            "nested cwd data"
        );
        let restored = fixture.rehydrate(&projected, &identity).await.unwrap();
        assert_eq!(restored.transcript().bytes(), payload.transcript().bytes());
        assert_eq!(restored.snapshot(), payload.snapshot());
        assert_eq!(
            restored.lifecycle_events_jsonl().bytes(),
            payload.lifecycle_events_jsonl().bytes()
        );
    }

    #[tokio::test]
    async fn unverified_or_invalid_projection_cannot_rehydrate() {
        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        let projection = fixture.seal(&payload, &identity).await;
        let bytes = projection.encode().unwrap();
        assert!(PendingPayloadProjection::decode(&bytes).is_ok());
        let text = String::from_utf8(bytes).unwrap();
        for invalid in [
            text.replacen("\"version\":1", "\"version\":1,\"version\":1", 1),
            text.replacen("{", "{\"locator\":\"secret\",", 1),
            format!(" {text}"),
            text.replace("\"version\":1", "\"version\":2"),
        ] {
            invalid_message(PendingPayloadProjection::decode(invalid.as_bytes()));
        }
        assert!(preflight(&vec![b' '; MAX_ENVELOPE + 1], MAX_ENVELOPE, MAX_DEPTH).is_err());
        let nested = format!(
            "{}0{}",
            "[".repeat(MAX_DEPTH + 1),
            "]".repeat(MAX_DEPTH + 1)
        );
        invalid_message(json(nested.as_bytes(), MAX_SIDECARS));
        for report in [b"{\"matches\":[],\"bytes_scanned\":0,\"bytes_scanned\":0,\"bytes_redacted\":0}".as_slice(),
            b"{\"matches\":[],\"bytes_scanned\":0,\"bytes_redacted\":0,\"cwd\":\"secret\"}",
            b"{\"matches\":[{\"rule_id\":\"rule\",\"start\":0,\"end\":1,\"session_id\":\"secret\"}],\"bytes_scanned\":1,\"bytes_redacted\":0}",
            b"{\"matches\":[],\"bytes_scanned\":-1,\"bytes_redacted\":0}"] {
            invalid_message(json(report, MAX_SIDECARS).and_then(|value| validate_report(&value)));
        }
        let (body, mac) = fixture.signed(&projection).await;
        // A valid sidecar MAC must never be usable as an envelope witness.
        let sidecar_body = sidecar_commitment_body("metadata", payload.metadata_json().bytes());
        fixture
            .scope
            .verify_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &sidecar_body,
                &projection.metadata_mac,
                deadline(),
            )
            .await
            .unwrap();
        assert!(
            AuthenticatedPendingEnvelope::verify(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &fixture.scope,
                &sidecar_body,
                &projection.metadata_mac,
                deadline(),
            )
            .await
            .is_err()
        );
        let mut integer_projection =
            PendingPayloadProjection::decode(&projection.encode().unwrap()).unwrap();
        integer_projection.metadata["extraction"]["large_integer"] = Value::from(u64::MAX - 1);
        let mut integer_body = serde_json::json!({"payload": &integer_projection, "version": 1});
        verify_payload_membership(
            &serde_json::to_vec(&integer_body).unwrap(),
            &integer_projection,
        )
        .unwrap();
        integer_body["payload"]["metadata"]["extraction"]["large_integer"] = Value::from(u64::MAX);
        invalid_message(verify_payload_membership(
            &serde_json::to_vec(&integer_body).unwrap(),
            &integer_projection,
        ));
        assert!(
            AuthenticatedPendingEnvelope::verify(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &fixture.scope,
                &body,
                &mac.replace("hmac-v1", "hmac-v2"),
                deadline()
            )
            .await
            .is_err()
        );
        let unrelated = b"{\"payload\":{},\"version\":1}";
        let tag = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                unrelated,
                deadline(),
            )
            .await
            .unwrap();
        let proof = AuthenticatedPendingEnvelope::verify(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            &fixture.scope,
            unrelated,
            &tag,
            deadline(),
        )
        .await
        .unwrap();
        invalid_message(
            projection
                .rehydrate(
                    &proof,
                    &fixture.conn,
                    &fixture.storage,
                    fixture.root.path(),
                    &identity,
                    deadline(),
                )
                .await,
        );
    }

    #[tokio::test]
    async fn resolved_context_mismatch_is_content_free() {
        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        let projection = fixture.seal(&payload, &identity).await;
        fixture.seed("claude__other", "other").await;
        let other = fixture.identity("claude__other", None).await;
        invalid_message(fixture.rehydrate(&projection, &other).await);
        fixture.retain_alias(&identity).await;
        fixture.conn.execute_unprepared("UPDATE agent_session SET provider_session_id = 'changed-native' WHERE session_id = 'claude__native-session'").await.unwrap();
        let changed = fixture
            .identity("claude__native-session", Some(identity.alias()))
            .await;
        assert_eq!(changed.alias(), identity.alias());
        invalid_message(fixture.rehydrate(&projection, &changed).await);
        fixture.conn.execute_unprepared("UPDATE agent_session SET provider_session_id = 'native-session', working_dir = '/other-working-dir' WHERE session_id = 'claude__native-session'").await.unwrap();
        let moved = fixture
            .identity("claude__native-session", Some(identity.alias()))
            .await;
        invalid_message(fixture.rehydrate(&projection, &moved).await);
        let mut scope = fixture.scope.clone();
        scope.repo_id = "foreign".into();
        let (body, tag) = fixture.signed(&projection).await;
        assert!(
            AuthenticatedPendingEnvelope::verify(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &scope,
                &body,
                &tag,
                deadline()
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn sidecar_signing_keeps_typed_transient_causes() {
        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        // Sidecar signing is the first database read in seal. A transient
        // repository-identity read failure must stay typed and retryable,
        // exactly like the rehydrate mapping, not collapse into the remedy.
        fixture
            .conn
            .execute_unprepared("ALTER TABLE config_kv RENAME TO config_kv_unavailable")
            .await
            .unwrap();
        let transient = PendingPayloadProjection::seal(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            &payload,
            &identity,
            deadline(),
        )
        .await
        .err()
        .expect("an unavailable repository identity cannot sign sidecars");
        assert_ne!(transient.to_string(), REMEDY);
        assert!(
            crate::internal::ai::capture::pending::retryable_load_failure(&transient, deadline()),
            "typed transient signing causes must remain retryable"
        );
        fixture
            .conn
            .execute_unprepared("ALTER TABLE config_kv_unavailable RENAME TO config_kv")
            .await
            .unwrap();
        fixture.seal(&payload, &identity).await;
        // A lost key is permanent damage: the fixed content-free remedy.
        std::fs::remove_file(
            fixture
                .storage
                .join(key::CAPTURE_DEDUP_SECRET_DIR)
                .join(key::CAPTURE_DEDUP_SECRET_FILE),
        )
        .unwrap();
        invalid_message(
            PendingPayloadProjection::seal(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &payload,
                &identity,
                deadline(),
            )
            .await,
        );
    }

    #[tokio::test]
    async fn coverage_bridge_preserves_original_catalog_claim_identity() {
        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let checkpoint = Uuid::new_v4().to_string();
        let mut original = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: identity.context().session_id().into(),
            checkpoint_id: checkpoint.clone(),
            owner: "owner-token".into(),
            parent_commit: Some("a".repeat(40)),
            created_at: 1,
            now_ms: 2,
            claims: vec![ReservedTurnClaim {
                logical_turn_key: "turn:semantic-key".into(),
                coverage_digest: "a".repeat(64),
                completeness: Completeness::Complete,
                fence_token: 4,
                next_revision: 8,
            }],
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let proof = PortablePendingCoverage::from_live(&original, &identity).unwrap();
        let bytes = serde_json::to_vec(&proof).unwrap();
        assert!(
            !String::from_utf8(bytes.clone())
                .unwrap()
                .contains(identity.context().session_id())
        );
        let proof: PortablePendingCoverage = serde_json::from_slice(&bytes).unwrap();
        let restored = proof
            .into_live(&identity, &checkpoint, original.parent_commit.clone(), 3)
            .unwrap();
        assert_eq!(restored.session_id, original.session_id);
        assert_ne!(restored.session_id, identity.alias());
        assert_eq!(restored.owner, original.owner);
        assert_eq!(restored.capture_scope, original.capture_scope);
        assert_eq!(
            restored.claims[0].logical_turn_key,
            original.claims[0].logical_turn_key
        );
        assert_eq!(restored.claims[0].fence_token, 4);
        assert_eq!(restored.claims[0].next_revision, 8);
        assert_eq!(
            restored.claims[0].coverage_digest,
            original.claims[0].coverage_digest
        );
        original.session_id = identity.alias().into();
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.session_id = identity.context().session_id().into();
        original.source_channel = "import";
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.source_channel = "live";
        original.capture_scope = None;
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.capture_scope = Some(fixture.scope.clone());
        original.import_identity = Some(crate::internal::ai::coverage_gate::ImportIdentityCommit {
            identity_id: "foreign-import".into(),
            observed_digest: "a".repeat(64),
            owner: "owner".into(),
            fence_token: 1,
            next_ordinal: 1,
            final_turn: true,
        });
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.import_identity = None;
        original.claims.push(original.claims[0].clone());
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.claims.pop();
        original.claims[0].fence_token = 0;
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.claims[0].fence_token = 4;
        original.claims[0].next_revision = 0;
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.claims[0].next_revision = 8;
        original.owner = "x".repeat(513);
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.owner = "owner".into();
        original.created_at = -1;
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.created_at = 1;
        original.capture_scope.as_mut().unwrap().repo_id = "foreign-repo".into();
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        original.capture_scope = Some(fixture.scope.clone());
        for key in ["".to_owned(), "control\nkey".into(), "x".repeat(513)] {
            original.claims[0].logical_turn_key = key;
            invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        }
        original.claims[0].logical_turn_key = "turn:semantic-key".into();
        for digest in ["z".repeat(64), "a".repeat(63)] {
            original.claims[0].coverage_digest = digest;
            invalid_message(PortablePendingCoverage::from_live(&original, &identity));
        }
        original.claims[0].coverage_digest = "a".repeat(64);
        let valid_proof = PortablePendingCoverage::from_live(&original, &identity).unwrap();
        invalid_message(valid_proof.into_live(&identity, "not-a-checkpoint", None, 3));
        original.claims = vec![original.claims[0].clone(); MAX_SIDECARS / 64 + 1];
        invalid_message(PortablePendingCoverage::from_live(&original, &identity));
    }

    #[tokio::test]
    async fn rehydrated_sidecars_match_normal_checkpoint_payload() {
        let fixture = Fixture::new().await;
        fixture.conn.execute_unprepared("UPDATE agent_session SET provider_session_id = 'AKIAABCDEFGHIJKLMNOP', working_dir = '/workspace/AKIAABCDEFGHIJKLMNOP'").await.unwrap();
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        let projection = fixture.seal(&payload, &identity).await;
        let restored = fixture.rehydrate(&projection, &identity).await.unwrap();
        for (before, after) in [
            (payload.metadata_json(), restored.metadata_json()),
            (
                payload.lifecycle_events_jsonl(),
                restored.lifecycle_events_jsonl(),
            ),
            (
                payload.redaction_report_json(),
                restored.redaction_report_json(),
            ),
        ] {
            assert_eq!(before.bytes(), after.bytes());
            assert!(!String::from_utf8_lossy(after.bytes()).contains("AKIAABCDEFGHIJKLMNOP"));
        }
        assert_eq!(projection.lifecycle.len(), 2);
        // A future rule that spans two E3 lines must change the whole-buffer
        // commitment, rather than silently publishing newly redacted bytes.
        let changed_redactor =
            Redactor::with_rules(vec![crate::internal::ai::observed_agents::RedactionRule {
                id: "changed-cross-line-policy",
                regex: regex::bytes::Regex::new("\\}\\n\\{").unwrap(),
                replacement: "[REDACTED]",
            }]);
        let changed_lifecycle = changed_redactor
            .redact(payload.lifecycle_events_jsonl().bytes())
            .0;
        assert_ne!(
            changed_lifecycle.bytes(),
            payload.lifecycle_events_jsonl().bytes()
        );
        assert!(
            fixture
                .scope
                .verify_pending_envelope_until(
                    &fixture.conn,
                    &fixture.storage,
                    fixture.root.path(),
                    &sidecar_commitment_body("lifecycle", changed_lifecycle.bytes()),
                    &projection.lifecycle_mac,
                    deadline(),
                )
                .await
                .is_err()
        );
        let metadata: Value = serde_json::from_slice(payload.metadata_json().bytes()).unwrap();
        let differently_formatted = changed(
            &payload,
            serde_json::to_vec(&metadata).unwrap(),
            payload.lifecycle_events_jsonl().bytes().to_vec(),
            payload.redaction_report_json().bytes().to_vec(),
        );
        invalid_message(
            PendingPayloadProjection::seal(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &differently_formatted,
                &identity,
                deadline(),
            )
            .await,
        );
        let mut swapped = PendingPayloadProjection::decode(&projection.encode().unwrap()).unwrap();
        std::mem::swap(&mut swapped.metadata_mac, &mut swapped.lifecycle_mac);
        invalid_message(fixture.rehydrate(&swapped, &identity).await);
        let preserved_report = decoded(&projection.redaction_report, MAX_SIDECARS).unwrap();
        assert_eq!(preserved_report, payload.redaction_report_json().bytes());
    }

    fn synthetic_association(
        key: &str,
        disposition: PendingReasoningIdentityDisposition,
    ) -> PendingReasoningIdentityAssociation {
        PendingReasoningIdentityAssociation {
            logical_turn_key: key.to_owned(),
            slot: format!("reasoning-identity/hmac-v1/{}", "a".repeat(64)),
            descriptor_mac: format!("reasoning-identity/hmac-v1/{}", "b".repeat(64)),
            disposition,
        }
    }

    async fn synthetic_v2_projection() -> (
        Fixture,
        PreparedPendingAlias,
        PendingPayloadProjection,
        PendingPayloadProjection,
    ) {
        use crate::internal::ai::observed_agents::reasoning::synthetic_native_batch_for_turns;

        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        let v1 = fixture.seal(&payload, &identity).await;
        let native = synthetic_native_batch_for_turns(
            "claude__native-session",
            "native-session",
            vec![
                (0, b"first-private-field".to_vec()),
                (1, b"second-private-field".to_vec()),
            ],
        );
        let binding = VerifiedCaptureBinding::from_validated_capture_context(
            &native,
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        let v2 = fixture
            .seal(&payload, &identity)
            .await
            .with_synthetic_reasoning(
                &native,
                &binding,
                vec![
                    synthetic_association(
                        "synthetic-turn-0",
                        PendingReasoningIdentityDisposition::VerifiedNonempty,
                    ),
                    synthetic_association(
                        "ordinary-turn",
                        PendingReasoningIdentityDisposition::VerifiedEmpty,
                    ),
                    synthetic_association(
                        "synthetic-turn-1",
                        PendingReasoningIdentityDisposition::VerifiedNonempty,
                    ),
                ],
            )
            .unwrap();
        (fixture, identity, v1, v2)
    }

    fn synthetic_versioned_codec(
        fixture: &Fixture,
        identity: &PreparedPendingAlias,
        v1: PendingPayloadProjection,
        v2: PendingPayloadProjection,
    ) -> (
        PendingBinding,
        VersionedPendingPayloadProjection,
        VersionedPortablePendingCoverage,
    ) {
        let binding = PendingBinding {
            scope: fixture.scope.clone(),
            session_id: identity.alias().to_owned(),
            checkpoint_id: v1.metadata["checkpoint_id"].as_str().unwrap().to_owned(),
            event_id: v1.lifecycle[0]["event_id"].as_str().unwrap().to_owned(),
            action_key: "cov2-codec-action".into(),
            receipt_key: "cov2-codec-receipt".into(),
            marker_generation: "cov2-codec-generation".into(),
            source_commitment: v1
                .snapshot
                .source
                .as_ref()
                .unwrap()
                .digest_sha256
                .as_ref()
                .unwrap()
                .clone(),
            reserved_revision: 1,
            original_deadline_millis: None,
            deferrable: false,
            first_attempt_millis: 1,
            parent_commit: None,
            parent_unborn: true,
        };
        let associations = [1, 2]
            .into_iter()
            .map(|version| VersionedPendingIdentityAssociation {
                coverage_schema_version: version,
                logical_turn_key: "synthetic-turn-0".into(),
                slot: format!("reasoning-identity/hmac-v{version}/{}", "a".repeat(64)),
                descriptor_mac: format!("reasoning-identity/hmac-v{version}/{}", "b".repeat(64)),
                disposition: PendingReasoningIdentityDisposition::VerifiedNonempty,
            })
            .collect();
        let payload =
            VersionedPendingPayloadProjection::new_codec(v1, v2.reasoning_source, associations)
                .unwrap();
        let coverage = VersionedPortablePendingCoverage {
            owner: "cov2-codec-owner".into(),
            created_at: 1,
            claims: [1, 2]
                .into_iter()
                .map(|version| VersionedPortableClaim {
                    coverage_schema_version: version,
                    logical_turn_key: "synthetic-turn-0".into(),
                    coverage_digest: "c".repeat(64),
                    completeness: Completeness::Complete,
                    fence_token: version,
                    next_revision: 1,
                })
                .collect(),
        };
        (binding, payload, coverage)
    }

    #[tokio::test]
    async fn cov2_cross_version_same_key_authenticated_codec_roundtrip() {
        let (fixture, identity, v1, v2) = synthetic_v2_projection().await;
        let (binding, payload, coverage) = synthetic_versioned_codec(&fixture, &identity, v1, v2);
        let bytes = crate::internal::ai::capture::pending::encode_versioned_envelope(
            binding.clone(),
            payload,
            coverage,
        )
        .unwrap();
        let mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &bytes,
                deadline(),
            )
            .await
            .unwrap();
        let authentication = AuthenticatedPendingEnvelope::verify(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            &fixture.scope,
            &bytes,
            &mac,
            deadline(),
        )
        .await
        .unwrap();
        let decoded =
            crate::internal::ai::capture::pending::decode_authenticated_versioned_envelope(
                &authentication,
                &binding,
            )
            .unwrap();
        assert_eq!(decoded.version, 2);
        assert!(decoded.binding == binding);
        assert!(decoded.payload.reasoning_source.is_some());
        let inner = decoded.payload.encode_canonical().unwrap();
        let inner_json: Value = serde_json::from_slice(&inner).unwrap();
        assert_eq!(
            inner_json["identity_commit_associations"][0]["coverage_schema_version"],
            1
        );
        assert_eq!(
            inner_json["identity_commit_associations"][1]["coverage_schema_version"],
            2
        );
        assert_eq!(serde_json::to_vec(&decoded.payload).unwrap(), inner);
        assert_eq!(
            decoded
                .coverage
                .claims
                .iter()
                .map(|claim| (
                    claim.coverage_schema_version,
                    claim.logical_turn_key.as_str()
                ))
                .collect::<Vec<_>>(),
            [(1, "synthetic-turn-0"), (2, "synthetic-turn-0")]
        );
        assert_eq!(
            decoded
                .payload
                .identity_commit_associations
                .iter()
                .map(|identity| (
                    identity.coverage_schema_version,
                    identity.logical_turn_key.as_str()
                ))
                .collect::<Vec<_>>(),
            [(1, "synthetic-turn-0"), (2, "synthetic-turn-0")]
        );
        assert_eq!(
            serde_json::to_vec(&decoded).unwrap(),
            bytes,
            "the authenticated decoder must retain the exact ordered sealed tuple"
        );
        let mut wrong_binding = binding.clone();
        wrong_binding.checkpoint_id = Uuid::new_v4().to_string();
        assert!(
            crate::internal::ai::capture::pending::decode_authenticated_versioned_envelope(
                &authentication,
                &wrong_binding,
            )
            .is_err()
        );
    }

    #[tokio::test]
    async fn cov2_same_version_duplicate_and_unknown_reject() {
        let (fixture, identity, v1, v2) = synthetic_v2_projection().await;
        let (binding, payload, coverage) = synthetic_versioned_codec(&fixture, &identity, v1, v2);
        let bytes = crate::internal::ai::capture::pending::encode_versioned_envelope(
            binding, payload, coverage,
        )
        .unwrap();
        let mut reordered: crate::internal::ai::capture::pending::VersionedPendingEnvelope =
            serde_json::from_slice(&bytes).unwrap();
        reordered.payload.identity_commit_associations.swap(0, 1);
        let error = crate::internal::ai::capture::pending::encode_versioned_envelope(
            reordered.binding,
            reordered.payload,
            reordered.coverage,
        )
        .expect_err("claim and identity ordering must remain paired");
        assert!(
            error
                .to_string()
                .contains("claim and identity tuple changed")
        );
        for version in [1, 0, 3] {
            let mut changed: crate::internal::ai::capture::pending::VersionedPendingEnvelope =
                serde_json::from_slice(&bytes).unwrap();
            changed.coverage.claims[1].coverage_schema_version = version;
            changed.payload.identity_commit_associations[1].coverage_schema_version = version;
            if version == 1 {
                changed.payload.identity_commit_associations[1].slot =
                    format!("reasoning-identity/hmac-v1/{}", "a".repeat(64));
                changed.payload.identity_commit_associations[1].descriptor_mac =
                    format!("reasoning-identity/hmac-v1/{}", "b".repeat(64));
            }
            let error = crate::internal::ai::capture::pending::encode_versioned_envelope(
                changed.binding,
                changed.payload,
                changed.coverage,
            )
            .expect_err("same-version duplicate and unsupported versions fail closed");
            assert!(
                error
                    .to_string()
                    .contains("capture recovery payload cannot be trusted")
            );
        }
    }

    #[tokio::test]
    async fn cov2_versioned_producer_claim_count_guard_precedes_projection() {
        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let template = ReservedTurnClaim {
            logical_turn_key: String::new(),
            coverage_digest: "a".repeat(64),
            completeness: Completeness::Complete,
            fence_token: 1,
            next_revision: 1,
        };
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: identity.context().session_id().into(),
            checkpoint_id: Uuid::new_v4().to_string(),
            owner: "cov2-count-owner".into(),
            parent_commit: None,
            created_at: 1,
            now_ms: 2,
            claims: (0..MAX_VERSIONED_PENDING_ENTRIES)
                .map(|index| ReservedTurnClaim {
                    logical_turn_key: format!("turn-{index:04}"),
                    ..template.clone()
                })
                .collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let mut bound = ordinary
            .claims
            .iter()
            .cloned()
            .map(|claim| VersionedReservedTurnClaim::new(1, claim).unwrap())
            .collect::<Vec<_>>();
        let at_cap = VersionedLiveClaimCommitPlan::new(ordinary.clone(), &bound).unwrap();
        assert_eq!(
            VersionedPortablePendingCoverage::from_live(&at_cap, &identity)
                .unwrap()
                .claims
                .len(),
            MAX_VERSIONED_PENDING_ENTRIES
        );

        let mut excessive = ordinary;
        let extra = ReservedTurnClaim {
            logical_turn_key: "turn-over-cap".into(),
            ..template
        };
        excessive.claims.push(extra.clone());
        bound.push(VersionedReservedTurnClaim::new(1, extra).unwrap());
        let over_cap = VersionedLiveClaimCommitPlan::new(excessive, &bound).unwrap();
        assert_eq!(
            over_cap.ordinary().claims.len(),
            MAX_VERSIONED_PENDING_ENTRIES + 1
        );
        let files_before = walkdir::WalkDir::new(fixture.root.path())
            .into_iter()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().to_path_buf())
            .collect::<std::collections::BTreeSet<_>>();
        let error = VersionedPortablePendingCoverage::from_live(&over_cap, &identity)
            .err()
            .expect("4097 live claims must not produce a portable projection");
        assert_eq!(error.to_string(), REMEDY);
        let files_after = walkdir::WalkDir::new(fixture.root.path())
            .into_iter()
            .map(|entry| entry.unwrap())
            .filter(|entry| entry.file_type().is_file())
            .map(|entry| entry.path().to_path_buf())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            files_before, files_after,
            "rejection must not create an envelope"
        );
    }

    #[tokio::test]
    async fn cov2_pending_v2_mac_and_bounds_fail_closed() {
        let (fixture, identity, v1, v2) = synthetic_v2_projection().await;
        let (binding, payload, coverage) = synthetic_versioned_codec(&fixture, &identity, v1, v2);
        let bytes = crate::internal::ai::capture::pending::encode_versioned_envelope(
            binding.clone(),
            payload,
            coverage,
        )
        .unwrap();
        let mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &bytes,
                deadline(),
            )
            .await
            .unwrap();
        let mut tampered = bytes.clone();
        let position = tampered.iter().position(|byte| *byte == b'c').unwrap();
        tampered[position] = b'd';
        assert!(
            AuthenticatedPendingEnvelope::verify(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &fixture.scope,
                &tampered,
                &mac,
                deadline(),
            )
            .await
            .is_err()
        );

        // Tiny malformed arrays exercise the count guard independently of
        // the 1 MiB wire guard, before either typed Vec is allocated.
        let mut count_attack: Value = serde_json::from_slice(&bytes).unwrap();
        count_attack["payload"]["identity_commit_associations"] =
            Value::Array(vec![Value::Null; MAX_VERSIONED_PENDING_ENTRIES + 1]);
        count_attack["coverage"]["claims"] =
            Value::Array(vec![Value::Null; MAX_VERSIONED_PENDING_ENTRIES + 1]);
        let count_bytes = serde_json::to_vec(&count_attack).unwrap();
        assert!(count_bytes.len() < MAX_VERSIONED_PENDING_TUPLE_BYTES);
        let count_mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &count_bytes,
                deadline(),
            )
            .await
            .unwrap();
        let count_auth = AuthenticatedPendingEnvelope::verify(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            &fixture.scope,
            &count_bytes,
            &count_mac,
            deadline(),
        )
        .await
        .unwrap();
        assert!(
            crate::internal::ai::capture::pending::decode_authenticated_versioned_envelope(
                &count_auth,
                &binding,
            )
            .is_err()
        );

        let mut too_many: crate::internal::ai::capture::pending::VersionedPendingEnvelope =
            serde_json::from_slice(&bytes).unwrap();
        let first_claim = too_many.coverage.claims[0].clone();
        let first_identity = too_many.payload.identity_commit_associations[0].clone();
        for index in 2..=MAX_VERSIONED_PENDING_ENTRIES {
            let key = format!("overflow-{index}");
            let mut claim = first_claim.clone();
            claim.logical_turn_key = key.clone();
            let mut association = first_identity.clone();
            association.logical_turn_key = key;
            too_many.coverage.claims.push(claim);
            too_many
                .payload
                .identity_commit_associations
                .push(association);
        }
        assert_eq!(
            too_many.coverage.claims.len(),
            MAX_VERSIONED_PENDING_ENTRIES + 1
        );
        assert!(
            crate::internal::ai::capture::pending::encode_versioned_envelope(
                too_many.binding.clone(),
                too_many.payload,
                too_many.coverage,
            )
            .is_err()
        );

        let mut oversized: crate::internal::ai::capture::pending::VersionedPendingEnvelope =
            serde_json::from_slice(&bytes).unwrap();
        let first_claim = oversized.coverage.claims[0].clone();
        let first_identity = oversized.payload.identity_commit_associations[0].clone();
        for index in 2..MAX_VERSIONED_PENDING_ENTRIES {
            let key = format!("{index:04}{}", "k".repeat(508));
            let mut claim = first_claim.clone();
            claim.logical_turn_key = key.clone();
            let mut association = first_identity.clone();
            association.logical_turn_key = key;
            oversized.coverage.claims.push(claim);
            oversized
                .payload
                .identity_commit_associations
                .push(association);
        }
        assert_eq!(
            oversized.coverage.claims.len(),
            MAX_VERSIONED_PENDING_ENTRIES
        );
        let wire_bytes = serde_json::to_vec(&oversized).unwrap();
        assert!(wire_bytes.len() < crate::internal::ai::capture::pending::MAX_ENVELOPE_BYTES);
        let wire_mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &wire_bytes,
                deadline(),
            )
            .await
            .unwrap();
        let wire_auth = AuthenticatedPendingEnvelope::verify(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            &fixture.scope,
            &wire_bytes,
            &wire_mac,
            deadline(),
        )
        .await
        .unwrap();
        assert!(
            crate::internal::ai::capture::pending::decode_authenticated_versioned_envelope(
                &wire_auth, &binding,
            )
            .is_err()
        );
        assert!(
            crate::internal::ai::capture::pending::encode_versioned_envelope(
                oversized.binding,
                oversized.payload,
                oversized.coverage,
            )
            .is_err()
        );
    }

    fn v2_original_envelope(projection: &PendingPayloadProjection) -> (Vec<u8>, Vec<u8>) {
        let claims = projection
            .identity_commit_associations
            .as_ref()
            .unwrap()
            .iter()
            .map(|association| serde_json::json!({"logical_turn_key":association.logical_turn_key}))
            .collect::<Vec<_>>();
        let coverage = serde_json::to_vec(&serde_json::json!({"claims":claims})).unwrap();
        let payload = projection.encode().unwrap();
        let envelope = [
            b"{\"version\":1,\"binding\":{},\"payload\":".as_slice(),
            payload.as_slice(),
            b",\"coverage\":".as_slice(),
            coverage.as_slice(),
            b"}".as_slice(),
        ]
        .concat();
        (envelope, coverage)
    }

    async fn signed_v2_projection() -> (Fixture, PendingPayloadProjection, Vec<u8>, String) {
        let (fixture, _, _, projection) = synthetic_v2_projection().await;
        let (envelope, _) = v2_original_envelope(&projection);
        let mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &envelope,
                deadline(),
            )
            .await
            .unwrap();
        (fixture, projection, envelope, mac)
    }

    async fn authenticate_v2(
        fixture: &Fixture,
        envelope: &[u8],
        mac: &str,
    ) -> PendingIntegrityToken {
        key::authenticate_pending_reasoning_original_in_scope_until(
            &fixture.conn,
            &fixture.scope,
            &fixture.storage,
            fixture.root.path(),
            envelope,
            mac,
            deadline(),
        )
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn reasoning_pending_restore_requires_whole_mac() {
        let (fixture, _, envelope, mac) = signed_v2_projection().await;
        assert!(
            key::authenticate_pending_reasoning_original_in_scope_until(
                &fixture.conn,
                &fixture.scope,
                &fixture.storage,
                fixture.root.path(),
                &envelope,
                &mac,
                deadline(),
            )
            .await
            .is_ok()
        );
        let mut changed = envelope.clone();
        let pos = changed.iter().position(|byte| *byte == b'1').unwrap();
        changed[pos] = b'2';
        assert!(
            key::authenticate_pending_reasoning_original_in_scope_until(
                &fixture.conn,
                &fixture.scope,
                &fixture.storage,
                fixture.root.path(),
                &changed,
                &mac,
                deadline(),
            )
            .await
            .is_err()
        );
    }

    #[tokio::test]
    async fn reasoning_pending_restore_requires_membership() {
        let (fixture, projection, envelope, mac) = signed_v2_projection().await;
        let payload = projection.encode().unwrap();
        let (_, coverage) = v2_original_envelope(&projection);
        let mut other_payload = payload.clone();
        other_payload.push(b' ');
        let result = key::authenticate_pending_reasoning_members_in_scope_until(
            &fixture.conn,
            key::PendingReasoningAuthentication {
                scope: &fixture.scope,
                storage_path: &fixture.storage,
                authorized_root: fixture.root.path(),
                original_envelope: &envelope,
                original_mac: &mac,
                members: key::PendingReasoningMembers {
                    binding: b"{}",
                    payload: &other_payload,
                    coverage: &coverage,
                },
                deadline: deadline(),
            },
        )
        .await;
        assert!(
            result.is_err(),
            "a valid MAC cannot authenticate a foreign payload member"
        );
        let token = authenticate_v2(&fixture, &envelope, &mac).await;
        let associations =
            serde_json::to_vec(projection.identity_commit_associations.as_ref().unwrap()).unwrap();
        assert!(token.matches_associations(&associations));
    }

    #[tokio::test]
    async fn reasoning_pending_restore_preserves_source_record() {
        use crate::internal::ai::observed_agents::reasoning::{
            VerifiedCurrentBinding, restore_pending,
        };
        let (fixture, projection, envelope, mac) = signed_v2_projection().await;
        let token = authenticate_v2(&fixture, &envelope, &mac).await;
        let current = VerifiedCurrentBinding::from_validated_current_context(
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        let native = restore_pending(
            projection.reasoning_source.as_ref().unwrap(),
            token,
            &current,
        )
        .unwrap();
        assert_eq!(native.transport_source_session(), "native-session");
    }

    #[tokio::test]
    async fn reasoning_pending_restore_preserves_locator() {
        use crate::internal::ai::observed_agents::reasoning::{
            VerifiedCurrentBinding, restore_pending,
        };
        let (fixture, projection, envelope, mac) = signed_v2_projection().await;
        let token = authenticate_v2(&fixture, &envelope, &mac).await;
        let current = VerifiedCurrentBinding::from_validated_current_context(
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        let native = restore_pending(
            projection.reasoning_source.as_ref().unwrap(),
            token,
            &current,
        )
        .unwrap();
        let binding = VerifiedCaptureBinding::from_validated_capture_context(
            &native,
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        let artifacts = native.artifact_params(&binding).unwrap();
        assert_eq!(
            artifacts[0].locator,
            "claude_code:msg=0/part=0/metadata=signature"
        );
        assert_eq!(
            artifacts[1].locator,
            "claude_code:msg=1/part=1/metadata=signature"
        );
    }

    #[tokio::test]
    async fn reasoning_pending_restore_preserves_exact_bytes() {
        use crate::internal::ai::observed_agents::reasoning::{
            VerifiedCurrentBinding, restore_pending,
        };
        let (fixture, projection, envelope, mac) = signed_v2_projection().await;
        let token = authenticate_v2(&fixture, &envelope, &mac).await;
        let current = VerifiedCurrentBinding::from_validated_current_context(
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        let native = restore_pending(
            projection.reasoning_source.as_ref().unwrap(),
            token,
            &current,
        )
        .unwrap();
        let binding = VerifiedCaptureBinding::from_validated_capture_context(
            &native,
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        let artifacts = native.artifact_params(&binding).unwrap();
        assert_eq!(artifacts[0].bytes.as_bytes(), b"first-private-field");
        assert_eq!(artifacts[1].bytes.as_bytes(), b"second-private-field");
    }

    #[tokio::test]
    async fn reasoning_pending_cannot_upgrade_native_proof() {
        use crate::internal::ai::observed_agents::reasoning::{
            VerifiedCurrentBinding, restore_pending,
        };
        let (fixture, _, _, _) = signed_v2_projection().await;
        let (_, _, _, good) = synthetic_v2_projection().await;
        let mut value = serde_json::to_value(&good).unwrap();
        value["reasoning_source"]["items"][0]["proof"]["issuer"] = Value::from("unregistered");
        let changed: PendingPayloadProjection = serde_json::from_value(value).unwrap();
        let (envelope, _) = v2_original_envelope(&changed);
        let mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &envelope,
                deadline(),
            )
            .await
            .unwrap();
        let token = authenticate_v2(&fixture, &envelope, &mac).await;
        let current = VerifiedCurrentBinding::from_validated_current_context(
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        assert!(
            restore_pending(changed.reasoning_source.as_ref().unwrap(), token, &current).is_err()
        );
    }

    #[tokio::test]
    async fn reasoning_pending_early_rehydrate_cannot_restore_opaque() {
        let (fixture, identity, _, projection) = synthetic_v2_projection().await;
        let (envelope, _) = v2_original_envelope(&projection);
        let mac = fixture
            .scope
            .sign_pending_envelope_until(
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &envelope,
                deadline(),
            )
            .await
            .unwrap();
        let authentication = AuthenticatedPendingEnvelope::verify(
            &fixture.conn,
            &fixture.storage,
            fixture.root.path(),
            &fixture.scope,
            &envelope,
            &mac,
            deadline(),
        )
        .await
        .unwrap();
        let early = projection
            .rehydrate_v2_deferred(
                &authentication,
                &fixture.conn,
                &fixture.storage,
                fixture.root.path(),
                &identity,
                deadline(),
            )
            .await
            .unwrap();
        assert!(early.authorize_ordinary_pending_payload().is_ok());
        assert!(early.deferred_pending_reasoning().is_none());
        assert!(
            !early
                .transcript()
                .bytes()
                .windows(b"first-private-field".len())
                .any(|window| window == b"first-private-field")
        );
    }

    #[tokio::test]
    async fn reasoning_pending_empty_v1_bytes_unchanged() {
        let (_, _, v1, _) = synthetic_v2_projection().await;
        let bytes = v1.encode().unwrap();
        crate::internal::ai::capture::pending::frozen_pre_v2::decode_payload_v1(&bytes).unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("reasoning_source"));
        assert!(!String::from_utf8_lossy(&bytes).contains("identity_commit_associations"));
        assert_eq!(
            PendingPayloadProjection::decode(&bytes)
                .unwrap()
                .encode()
                .unwrap(),
            bytes
        );
    }

    #[tokio::test]
    async fn reasoning_pending_legacy_v1_is_readable() {
        let (fixture, identity, v1, _) = synthetic_v2_projection().await;
        let bytes = v1.encode().unwrap();
        let decoded = PendingPayloadProjection::decode(&bytes).unwrap();
        assert_eq!(decoded.version, VERSION);
        fixture.rehydrate(&decoded, &identity).await.unwrap();
    }

    #[tokio::test]
    async fn reasoning_pending_v2_schema_is_closed() {
        let (_, _, _, v2) = synthetic_v2_projection().await;
        let bytes = v2.encode().unwrap();
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["unknown_outer"] = Value::Bool(true);
        assert!(serde_json::from_value::<PendingPayloadProjection>(value).is_err());
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["reasoning_source"]["items"][0]["unknown_item"] = Value::Bool(true);
        assert!(serde_json::from_value::<PendingPayloadProjection>(value).is_err());
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["reasoning_source"]["unknown_source_field"] = Value::Bool(true);
        assert!(serde_json::from_value::<PendingPayloadProjection>(value).is_err());
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["identity_commit_associations"][0]["unknown_association_field"] = Value::Bool(true);
        assert!(serde_json::from_value::<PendingPayloadProjection>(value).is_err());
        let mut value: Value = serde_json::from_slice(&bytes).unwrap();
        value["version"] = Value::from(3);
        assert!(
            serde_json::from_value::<PendingPayloadProjection>(value.clone())
                .unwrap()
                .encode()
                .is_err()
        );
        value["version"] = Value::from(1);
        assert!(
            serde_json::from_value::<PendingPayloadProjection>(value)
                .unwrap()
                .encode()
                .is_err()
        );

        let (fixture, identity, _, v2) = synthetic_v2_projection().await;
        let mut unknown = v2;
        unknown.version = 3;
        let error = match fixture.rehydrate(&unknown, &identity).await {
            Ok(_) => panic!("unknown pending payload version must be rejected"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), REMEDY);
    }

    #[tokio::test]
    async fn reasoning_pending_v2_source_record_roundtrip() {
        let (_, _, _, v2) = synthetic_v2_projection().await;
        let bytes = v2.encode().unwrap();
        let decoded = PendingPayloadProjection::decode(&bytes).unwrap();
        let original: Value = serde_json::to_value(&v2.reasoning_source).unwrap();
        let restored: Value = serde_json::to_value(&decoded.reasoning_source).unwrap();
        assert_eq!(original, restored);
        assert_eq!(decoded.version, REASONING_VERSION);
    }

    #[tokio::test]
    async fn reasoning_pending_v2_locator_roundtrip() {
        let (_, _, _, v2) = synthetic_v2_projection().await;
        let decoded = PendingPayloadProjection::decode(&v2.encode().unwrap()).unwrap();
        let value = serde_json::to_value(decoded.reasoning_source).unwrap();
        assert_eq!(
            value["items"][0]["locator"],
            "claude_code:msg=0/part=0/metadata=signature"
        );
        assert_eq!(
            value["items"][1]["locator"],
            "claude_code:msg=1/part=1/metadata=signature"
        );
    }

    #[tokio::test]
    async fn reasoning_pending_v2_digest_roundtrip() {
        use sha2::{Digest, Sha256};
        let (_, _, _, v2) = synthetic_v2_projection().await;
        let decoded = PendingPayloadProjection::decode(&v2.encode().unwrap()).unwrap();
        let value = serde_json::to_value(decoded.reasoning_source).unwrap();
        let expected: [u8; 32] = Sha256::digest(b"first-private-field").into();
        assert_eq!(
            value["items"][0]["proof"]["original_wire_sha256"],
            serde_json::json!(expected)
        );
    }

    #[tokio::test]
    async fn reasoning_pending_v2_payload_roundtrip() {
        let (_, _, _, v2) = synthetic_v2_projection().await;
        let decoded = PendingPayloadProjection::decode(&v2.encode().unwrap()).unwrap();
        let value = serde_json::to_value(decoded.reasoning_source).unwrap();
        let bytes = STANDARD
            .decode(value["items"][0]["bytes_base64"].as_str().unwrap())
            .unwrap();
        assert_eq!(bytes, b"first-private-field");
    }

    #[tokio::test]
    async fn reasoning_pending_v2_order_roundtrip() {
        let (_, _, _, v2) = synthetic_v2_projection().await;
        let decoded = PendingPayloadProjection::decode(&v2.encode().unwrap()).unwrap();
        let value = serde_json::to_value(decoded.reasoning_source).unwrap();
        assert_eq!(value["items"][0]["proof"]["message"], 0);
        assert_eq!(value["items"][1]["proof"]["message"], 1);
        let keys: Vec<_> = decoded
            .identity_commit_associations
            .unwrap()
            .into_iter()
            .map(|entry| entry.logical_turn_key)
            .collect();
        assert_eq!(
            keys,
            ["synthetic-turn-0", "ordinary-turn", "synthetic-turn-1"]
        );
    }

    #[tokio::test]
    async fn reasoning_pending_v2_old_reader_fails_closed() {
        let (fixture, identity, _, v2) = synthetic_v2_projection().await;
        let bytes = v2.encode().unwrap();
        assert!(
            crate::internal::ai::capture::pending::frozen_pre_v2::decode_payload_v1(&bytes)
                .is_err()
        );
        let error = match fixture.rehydrate(&v2, &identity).await {
            Ok(_) => panic!("v2 rehydrate must remain disabled"),
            Err(error) => error,
        };
        assert_eq!(error.to_string(), PENDING_V2_REMEDY);
        assert_eq!(
            PendingPayloadProjection::decode(&bytes)
                .unwrap()
                .encode()
                .unwrap(),
            bytes
        );
        crate::internal::ai::capture::pending::assert_frozen_pre_v2_route_rejects_v2().await;
    }

    #[tokio::test]
    async fn reasoning_pending_identity_roundtrip_keeps_ordinary_claim_proof() {
        let (_, _, v1, v2) = synthetic_v2_projection().await;
        assert_eq!(v1.snapshot, v2.snapshot);
        assert_eq!(v1.metadata, v2.metadata);
        assert_eq!(v1.lifecycle, v2.lifecycle);
        let decoded = PendingPayloadProjection::decode(&v2.encode().unwrap()).unwrap();
        let entries = decoded.identity_commit_associations.unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1].logical_turn_key, "ordinary-turn");
        assert_eq!(
            entries[1].disposition,
            PendingReasoningIdentityDisposition::VerifiedEmpty
        );
    }

    #[tokio::test]
    async fn reasoning_pending_typed_sealer_uses_authenticated_turn_identities() {
        use crate::internal::ai::{
            capture::checkpoint::{
                ReasoningIdentityPreparation, prepare_reasoning_coverage_plan_from_disposition,
            },
            observed_agents::{
                NormalizedTurn,
                reasoning::{ReasoningSourceDisposition, synthetic_native_batch_for_turns},
            },
        };

        let fixture = Fixture::new().await;
        let identity = fixture.identity("claude__native-session", None).await;
        let payload = fixture.payload(&identity).await;
        let native = synthetic_native_batch_for_turns(
            "claude__native-session",
            "native-session",
            vec![(0, b"private-native-source".to_vec())],
        );
        let disposition = ReasoningSourceDisposition::NativeProven(native);
        let turns = ["synthetic-turn-0", "ordinary-turn"]
            .into_iter()
            .enumerate()
            .map(|(ordinal, key)| NormalizedTurn {
                logical_turn_key: key.to_owned(),
                ordinal,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: Vec::new(),
            })
            .collect::<Vec<_>>();
        let plan = prepare_reasoning_coverage_plan_from_disposition(
            &disposition,
            ReasoningIdentityPreparation {
                conn: &fixture.conn,
                scope: &fixture.scope,
                storage_path: &fixture.storage,
                authorized_root: fixture.root.path(),
                session_id: "claude__native-session",
                provider: "claude_code",
                turns: &turns,
                deadline: deadline(),
            },
        )
        .await
        .unwrap()
        .unwrap();
        let ReasoningSourceDisposition::NativeProven(native) = &disposition else {
            panic!("synthetic native disposition must remain typed")
        };
        let binding = VerifiedPendingBinding::from_validated_pending_context(
            native,
            "claude__native-session",
            "claude_code",
        )
        .unwrap();
        let projected = fixture
            .seal(&payload, &identity)
            .await
            .with_bound_reasoning(
                native,
                &binding,
                turns
                    .iter()
                    .map(|turn| {
                        (
                            turn.logical_turn_key.clone(),
                            plan.turn(&turn.logical_turn_key).unwrap().clone(),
                        )
                    })
                    .collect(),
            )
            .unwrap();
        let decoded = PendingPayloadProjection::decode(&projected.encode().unwrap()).unwrap();
        let associations = decoded.identity_commit_associations.unwrap();
        assert_eq!(associations.len(), 2);
        assert_eq!(
            associations[0].disposition,
            PendingReasoningIdentityDisposition::VerifiedNonempty
        );
        assert_eq!(
            associations[1].disposition,
            PendingReasoningIdentityDisposition::VerifiedEmpty
        );
        assert_eq!(
            associations[0].slot,
            plan.turn("synthetic-turn-0").unwrap().slot()
        );
        assert_eq!(
            associations[1].descriptor_mac,
            plan.turn("ordinary-turn").unwrap().descriptor_mac()
        );
    }

    #[tokio::test]
    async fn reasoning_pending_v2_identity_association_shape_is_unique() {
        let (_, _, _, v2) = synthetic_v2_projection().await;
        let valid: Value = serde_json::from_slice(&v2.encode().unwrap()).unwrap();
        // The batch may carry a previously covered source prefix. Its
        // authenticated membership is checked by the OG-21 reader, not by
        // the current claim association list.
        let mut prefix = valid.clone();
        let mut covered = prefix["reasoning_source"]["items"][0].clone();
        covered["locator"] = Value::from("claude_code:msg=99/part=99/metadata=signature");
        covered["proof"]["message"] = Value::from(99);
        covered["proof"]["part"] = Value::from(99);
        covered["proof"]["logical_turn"] = Value::from("covered-prefix");
        prefix["reasoning_source"]["items"]
            .as_array_mut()
            .unwrap()
            .push(covered);
        let projected: PendingPayloadProjection = serde_json::from_value(prefix).unwrap();
        projected.encode().unwrap();

        // An association claiming nonempty reasoning must have source bytes
        // for that key, while an empty claim must not carry source bytes.
        let mut missing_source = valid.clone();
        missing_source["identity_commit_associations"][1]["disposition"] =
            Value::from("verified_nonempty");
        let projected: PendingPayloadProjection = serde_json::from_value(missing_source).unwrap();
        assert!(projected.encode().is_err());
        let mut false_empty = valid.clone();
        false_empty["identity_commit_associations"][0]["disposition"] =
            Value::from("verified_empty");
        let projected: PendingPayloadProjection = serde_json::from_value(false_empty).unwrap();
        assert!(projected.encode().is_err());

        let mut value: Value = serde_json::from_slice(&v2.encode().unwrap()).unwrap();
        let duplicate = value["identity_commit_associations"][0].clone();
        value["identity_commit_associations"]
            .as_array_mut()
            .unwrap()
            .push(duplicate);
        let decoded: PendingPayloadProjection = serde_json::from_value(value).unwrap();
        assert!(decoded.encode().is_err());

        // The commit plan may contain ordinary turns beyond the source
        // artifact count; it shares the outer coverage claim bound instead.
        let mut value: Value = serde_json::from_slice(&v2.encode().unwrap()).unwrap();
        let ordinary = value["identity_commit_associations"][1].clone();
        let associations = value["identity_commit_associations"]
            .as_array_mut()
            .unwrap();
        for index in 0..=crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES {
            let mut entry = ordinary.clone();
            entry["logical_turn_key"] = Value::from(format!("ordinary-turn-{index}"));
            associations.push(entry);
        }
        assert!(associations.len() > crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES);
        assert!(associations.len() <= MAX_PORTABLE_CLAIMS);
        let decoded: PendingPayloadProjection = serde_json::from_value(value).unwrap();
        decoded.encode().unwrap();

        // The native source collection retains its separate 512-item bound.
        let mut source_value: Value = serde_json::to_value(v2.reasoning_source).unwrap();
        let items = source_value["items"].as_array_mut().unwrap();
        let seed = items[0].clone();
        for index in items.len()..crate::internal::ai::traces::REASONING_ARTIFACT_MAX_ENTRIES {
            let mut item = seed.clone();
            item["locator"] = Value::from(format!(
                "claude_code:msg={index}/part={index}/metadata=signature"
            ));
            item["proof"]["message"] = Value::from(index as u64);
            item["proof"]["part"] = Value::from(index as u64);
            item["proof"]["logical_turn"] = Value::from(format!("synthetic-turn-{index}"));
            items.push(item);
        }
        let source: PendingReasoningSourceDto =
            serde_json::from_value(source_value.clone()).unwrap();
        source.validate_shape().unwrap();
        let mut overflow = seed;
        overflow["locator"] = Value::from("claude_code:msg=512/part=512/metadata=signature");
        overflow["proof"]["message"] = Value::from(512);
        overflow["proof"]["part"] = Value::from(512);
        overflow["proof"]["logical_turn"] = Value::from("synthetic-turn-512");
        source_value["items"].as_array_mut().unwrap().push(overflow);
        let source: PendingReasoningSourceDto = serde_json::from_value(source_value).unwrap();
        assert!(source.validate_shape().is_err());
    }
}
