// Frozen pre-v2 production decoder/schema and recovery route from Libra commit
// 7efcf868d49a5183e7f0851a89abebee91357155,
// src/internal/ai/capture/pending_payload.rs lines 1..674.
// Original whole-file SHA-256: 87f2e8879831dae5fa83e3ffac3e916e23159dfb6a08f33a865bd1ae9f5d93fb.
// Keep this oracle unchanged when implementing payload v2.
//
// The GPUI dependency set forces `serde_json/preserve_order`, so the frozen
// decoder now explicitly normalizes object keys before serializing. That
// reproduces the original BTreeMap ordering this oracle was generated with;
// the hash pin in pending.rs was updated for the explicit canonicalization.
// Portable redacted payloads. Control identity is restored only in RAM from
// a verified private alias, never from a durable native locator or a path.

use std::{collections::HashSet, path::Path, time::Instant};

use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use sea_orm::ConnectionTrait;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use uuid::Uuid;

use crate::internal::ai::{
    capture::{
        checkpoint::{AuthenticatedPendingEnvelope, CheckpointRedactedPayload},
        pending_identity::PreparedPendingAlias,
        snapshot::CaptureSnapshotProjection,
    },
    coverage_gate::{LiveClaimCommitPlan, ReservedTurnClaim},
    observed_agents::{
        Completeness, MAX_REDACTION_MATCH_SAMPLES, RedactedBytes, Redactor, parse_canon_value,
    },
};

const VERSION: u8 = 1;
const MAX_TRANSCRIPT: usize = 24 * 1024 * 1024;
const MAX_SIDECARS: usize = 4 * 1024 * 1024;
const MAX_ENVELOPE: usize = 64 * 1024 * 1024;
const MAX_DEPTH: usize = 64;
const MAX_EVENTS: usize = MAX_SIDECARS / 128;
const REMEDY: &str = "capture recovery payload cannot be trusted; run `libra agent doctor`";
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
        let metadata = crate::utils::json::normalize_json_value(metadata);
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
            let value = crate::utils::json::normalize_json_value(value);
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

    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        self.validate()?;
        let mut value = serde_json::to_value(self).map_err(|_| failure())?;
        if let Some(metadata) = value.get_mut("metadata") {
            *metadata = crate::utils::json::normalize_json_value(metadata.take());
        }
        if let Some(Value::Array(events)) = value.get_mut("lifecycle") {
            for event in events {
                *event = crate::utils::json::normalize_json_value(event.take());
            }
        }
        let bytes = serde_json::to_vec(&value).map_err(|_| failure())?;
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
        self.validate()?;
        check_budget(deadline)?;
        ensure!(
            self.alias == identity.alias() && authentication.scope() == identity.context().scope(),
            REMEDY
        );
        verify_payload_membership(authentication.bytes(), self)?;
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
            serde_json::to_vec_pretty(&crate::utils::json::normalize_json_value(metadata))
                .map_err(|_| failure())?;
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
            let line = serde_json::to_vec(&crate::utils::json::normalize_json_value(event))
                .map_err(|_| failure())?;
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
            self.version == VERSION && canonical_alias(&self.alias),
            REMEDY
        );
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
            plan.owner.len() <= 512 && plan.claims.len() <= MAX_SIDECARS / 64,
            REMEDY
        );
        for claim in &plan.claims {
            ensure!(
                claim.logical_turn_key.len() <= 512 && claim.coverage_digest.len() == 64,
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
                && self.claims.len() <= MAX_SIDECARS / 64,
            REMEDY
        );
        let mut keys = HashSet::new();
        for claim in &self.claims {
            ensure!(
                !claim.logical_turn_key.is_empty()
                    && claim.logical_turn_key.len() <= 512
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


use anyhow::Context;
use super::{
    PendingBinding, PendingHeader, VerifiedPendingPayload, check_deadline,
    check_sidecar_capacity, is_lower_hex, load_chunks, validate_payload_binding,
};

// These constants and the immediately following functions are frozen from
// src/internal/ai/capture/pending.rs at 7efcf868. The header name alone is
// changed to avoid collision with the live header used by load_chunks.
const MAX_HEADER_BYTES: usize = 8 * 1024;
const MAX_ENVELOPE_BYTES: usize = 64 * 1024 * 1024;
const CHUNK_BYTES: usize = 512 * 1024;
const MAX_CHUNKS: usize = 128;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct FrozenPendingHeader {
    version: u8,
    pub(crate) binding: PendingBinding,
    mac: String,
    envelope_bytes: usize,
    chunks: usize,
    pub(crate) manual_attempted: bool,
}

impl FrozenPendingHeader {
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
    let payload = envelope
        .payload
        .rehydrate(&authentication, conn, storage, root, identity, deadline)
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
    check_deadline(deadline)?;
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

pub(crate) fn decode_payload_v1(bytes: &[u8]) -> Result<()> {
    PendingPayloadProjection::decode(bytes).map(|_| ())
}

pub(crate) fn decode_header_v1(text: &str) -> Result<()> {
    FrozenPendingHeader::decode(text).map(|_| ())
}
