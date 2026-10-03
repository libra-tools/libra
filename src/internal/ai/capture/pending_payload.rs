//! Portable redacted payloads. Control identity is restored only in RAM from
//! a verified private alias, never from a durable native locator or a path.

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
        let bytes = serde_json::to_vec(self).map_err(|_| failure())?;
        ensure!(bytes.len() <= MAX_ENVELOPE, REMEDY);
        preflight(&bytes, MAX_ENVELOPE, MAX_DEPTH)?;
        Ok(bytes)
    }

    #[cfg(test)]
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
        let metadata = serde_json::to_vec_pretty(&metadata).map_err(|_| failure())?;
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
            let line = serde_json::to_vec(&event).map_err(|_| failure())?;
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

#[cfg(test)]
#[cfg(unix)]
mod tests {
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
                redact(&serde_json::to_vec_pretty(&metadata).unwrap()),
                redact(&jsonl),
                redact(&serde_json::to_vec_pretty(&report).unwrap()),
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
}
