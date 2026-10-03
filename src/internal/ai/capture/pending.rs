//! Repository-private, authenticated recovery artifacts. No provider locator
//! or raw source is a recovery authority; only a complete sealed payload may
//! enter this codec. Headers are small indexed discovery/GC records and chunks
//! are read only for one explicitly selected artifact.

use std::{path::Path, time::Instant};

use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use sea_orm::{ConnectionTrait, DatabaseTransaction, Statement};
use serde::{Deserialize, Serialize};

use crate::internal::{
    ai::{
        capture::{
            catalog::{CaptureCatalogError, PendingSessionContext},
            checkpoint::{AuthenticatedPendingEnvelope, CheckpointRedactedPayload},
            pending_identity::{self, PreparedPendingAlias},
            pending_payload::{PendingPayloadProjection, PortablePendingCoverage},
        },
        capture_scope::CaptureScope,
        coverage_gate::{LiveClaimCommitPlan, ReservedTurnClaim},
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
                && coverage.parent_commit == binding.parent_commit,
            "capture recovery identity changed; retry through the provider"
        );
        let portable_coverage = PortablePendingCoverage::from_live(coverage, &identity)?;
        check_sidecar_capacity(payload, serde_json::to_vec(&portable_coverage)?.len())?;
        validate_payload_binding(payload, &binding)?;
        let projection =
            PendingPayloadProjection::seal(conn, storage, root, payload, &identity, deadline)
                .await?;
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
    pending_candidates_inner(txn, repo_id, None).await
}

pub(crate) async fn pending_candidates_for_scope(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
) -> Result<PendingCandidateBatch> {
    pending_candidates_inner(txn, &scope.repo_id, Some(scope)).await
}

async fn pending_candidates_inner(
    txn: &DatabaseTransaction,
    repo_id: &str,
    scope: Option<&CaptureScope>,
) -> Result<PendingCandidateBatch> {
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
          {ELIGIBLE_HEADERS} {scope_filter} ORDER BY p.key LIMIT 5"),
        selection_values,
    )).await.context("cannot read capture recovery candidates; run `libra agent doctor`")?;
    let window_full = rows.len() == 5;
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
                unassigned |= !text.is_some_and(|text| PendingHeader::decode(text).is_ok())
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
mod tests {
    #[cfg(unix)]
    use std::{path::PathBuf, time::Duration};

    #[cfg(unix)]
    use sea_orm::DatabaseConnection;
    #[cfg(unix)]
    use tempfile::TempDir;
    use uuid::Uuid;

    use super::*;

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
                redact(&serde_json::to_vec_pretty(&metadata).unwrap()),
                redact(&events),
                redact(&serde_json::to_vec_pretty(&report).unwrap()),
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
        replay_resolves_alias_after_vacuum_and_source_removal_inner(false, false, false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn exhausted_artifact_allows_one_audited_replay_without_rewriting_receipt() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, false, false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn attempt_capped_artifact_allows_one_audited_replay_without_rewriting_receipt() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, true, false).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial_test::serial(cwd)]
    async fn doctor_replays_expired_authenticated_artifact_once() {
        replay_resolves_alias_after_vacuum_and_source_removal_inner(true, false, true).await;
    }

    #[cfg(unix)]
    async fn replay_resolves_alias_after_vacuum_and_source_removal_inner(
        manual: bool,
        exhaust_attempts: bool,
        through_doctor: bool,
    ) {
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
        let (artifact, _) = f
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
        let binding = artifact.binding().clone();
        let alias = artifact.identity.alias().to_owned();
        f.persist(&artifact).await;

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
        let executable = std::env::current_exe().unwrap();
        let helper = executable
            .parent()
            .and_then(std::path::Path::parent)
            .unwrap()
            .join("libra");
        assert!(helper.is_file(), "built Libra helper is unavailable");
        // The proof is that one audited replay completes. The full lib suite
        // can starve the checkpoint write until a 10 s budget elapses before
        // the write begins, so the harness budget is wider than that floor.
        let deadline = crate::internal::ai::capture_scope::CaptureCommitDeadline::from_budget(
            Duration::from_secs(60),
        )
        .unwrap();
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
        let storage = ClientStorage::init(objects.clone());
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
                Arc::new(LocalStorage::new(objects.clone())),
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
                Arc::new(LocalStorage::new(objects.clone())),
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
}
