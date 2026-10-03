//! Scope-bound durable catalog mutations for external-agent capture.
//!
//! This module is the only capture-layer owner of the `agent_session` state
//! transition and its bounded replay receipt ledger.  It deliberately does
//! **not** create `agent_checkpoint` rows: a checkpoint is not durable until
//! the ref/object transaction in the later checkpoint store commits, and an
//! optimistic placeholder would be a false success record.  Instead, a
//! checkpoint-producing lifecycle action is first recorded as a `pending`
//! opaque receipt.  The checkpoint facade marks that exact receipt `complete`
//! only after its transaction succeeds.  A retry can therefore resume a
//! pending checkpoint without applying the lifecycle mutation a second time.
//!
//! Receipt data is stored in the existing `agent_session.metadata_json`
//! object, under a private versioned key.  It contains only keyed-HMAC replay
//! commitments, UUID-derived action identifiers, lifecycle action tags, and
//! timestamps.  In particular it never stores a transcript path, raw source
//! bytes, provider payload, prompt, or tool content.

#[cfg(test)]
use std::time::Duration;
use std::{
    collections::{HashMap, HashSet, VecDeque},
    future::Future,
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::Instant,
};

use async_trait::async_trait;
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::internal::{
    ai::{
        authorized_read::{
            CAPTURE_REDACTION_REPORT_RULE_PASS_CAP, SOURCE_IDENTITY_NOT_RETAINED,
            capture_redacted_output_cap,
        },
        capture::{
            finalizer::{
                CaptureFinalizeMode, CaptureFinalizePolicy, FinalizeCheckpointProgress,
                FinalizeDecision, FinalizeDecisionInput, FinalizePendingStage,
                FinalizeQuarantineReason, MAX_FINALIZE_ATTEMPTS, MAX_FINALIZE_WINDOW_MILLIS,
                PendingFinalizeReceipt, decide_finalization,
            },
            snapshot::{
                CaptureSnapshotCompleteness, CaptureSnapshotProjection, CaptureSnapshotSourceKind,
            },
            state::{
                CapturePhase, CheckpointWrite, DurableCaptureState, LifecycleActionPlan,
                StoppedAtMutation,
            },
        },
        capture_scope::{
            CaptureCommitDeadline, CaptureFinalCommitAuthorizationError, CaptureScope,
            authorize_final_capture_commit,
        },
        hooks::LifecycleEventKind,
        observed_agents::{
            MAX_REDACTION_MATCH_SAMPLES, TRANSCRIPT_READ_HARD_CAP_BYTES, parse_canon_value,
            redaction::{RedactionMatch, RedactionReport, is_durable_default_rule_id},
        },
    },
    db,
};

/// Private metadata field.  This is intentionally versioned independently of
/// the public `agent_session` wire because it is recovery machinery, not
/// provider metadata.
const RECEIPT_LEDGER_FIELD: &str = "capture_catalog_receipts_v1";
const RECEIPT_LEDGER_VERSION: u8 = 1;
const FINALIZER_RECEIPT_VERSION: u8 = 1;
const UNBOUND_FINALIZER_MARKER_PREFIX: &str = "capture-finalizer-unbound-v1:";
/// A bounded receipt ring prevents a high-rate provider from growing
/// `metadata_json` without limit. Pending entries are never evicted: losing
/// one would make a failed checkpoint unrecoverable.
pub(crate) const MAX_CAPTURE_RECEIPTS: usize = 128;

/// v1 is retained solely to read receipts written by the pre-ACF helper
/// protocol. New ingress always writes v2, whose HMAC input is the fixed
/// ingress SHA-256 commitment rather than a raw native identity.
const RECEIPT_PREFIX_V1: &str = "capture-dedup-v1:";
const RECEIPT_PREFIX_V2: &str = "capture-dedup-v2:";
const RECEIPT_HEX_BYTES: usize = 64;
/// A locally allocated action receipt is used only to defer a terminal write
/// when ingress could not prove a provider-native delivery identity. It is a
/// UUID action key, never provider data, and lets finalizer/doctor recover
/// the pending terminal operation without publishing `stopped` early.
const ACTION_RECEIPT_PREFIX: &str = "capture-action-v1:";
const MAX_SESSION_ID_BYTES: usize = 1_024;
const MAX_AGENT_KIND_BYTES: usize = 96;
const MAX_PROVIDER_SESSION_ID_BYTES: usize = 1_024;
const MAX_WORKING_DIR_BYTES: usize = 4_096;
const DOCTOR_FINALIZER_PAGE_SIZE: usize = 32;
const MAX_DOCTOR_FINALIZER_SESSIONS: usize = 512;
const MAX_DOCTOR_FINALIZER_RECOVERIES: usize = 128;
const MAX_DOCTOR_RECEIPT_METADATA_BYTES: usize = 1_048_576;
const MAX_IMPORT_SOURCE_FIELD_BYTES: usize = 4_096;
const IMPORT_SOURCE_HMAC_V2_PREFIX: &str = "source/hmac-v2/";
const SOURCE_COMMITMENT_HEX_BYTES: usize = 64;
/// These fields collectively identify an import-owned metadata projection.
/// A V2 import may adopt an otherwise normal live-capture row, but it must
/// never mistake a partial, legacy, or malformed import record for one.
const IMPORT_OWNERSHIP_METADATA_FIELDS: &[&str] = &[
    "repository_identity",
    "source_kind",
    "source_id",
    "source_fingerprint",
    "import_source_schema_version",
    "import_provisional",
    "imported",
    "transcript_snapshot",
];

/// The durable snapshot comes from one source-level redaction pass.  Its
/// output may be larger than the authorized source when a short secret is
/// replaced with a descriptive placeholder, so this is deliberately the
/// shared 1.5x helper bound rather than the raw-source cap.
fn snapshot_redacted_output_cap() -> u64 {
    capture_redacted_output_cap(TRANSCRIPT_READ_HARD_CAP_BYTES)
}

/// Redaction metrics are cumulative across ordered passes; they are bounded
/// by the helper's fixed pass margin, not by the first input buffer length.
fn redaction_metric_cap(output_cap: u64) -> u64 {
    output_cap.saturating_mul(CAPTURE_REDACTION_REPORT_RULE_PASS_CAP as u64)
}

fn redaction_match_count_cap(metric_cap: u64) -> u64 {
    metric_cap.saturating_add(MAX_REDACTION_MATCH_SAMPLES as u64)
}

/// An import report merges the source snapshot pass with the typed-field
/// pass. A typed field can itself expand while redacted, so coordinates use a
/// second shared 1.5x bound. The flat durable report intentionally does not
/// claim that a match offset belongs to its aggregate `bytes_scanned` total.
fn import_redaction_coordinate_cap() -> u64 {
    capture_redacted_output_cap(snapshot_redacted_output_cap())
}

fn import_redaction_scanned_cap() -> u64 {
    import_redaction_coordinate_cap().saturating_mul(2)
}

fn import_redaction_metric_cap() -> u64 {
    redaction_metric_cap(import_redaction_coordinate_cap()).saturating_mul(2)
}

fn import_redaction_dropped_match_cap() -> u64 {
    redaction_match_count_cap(import_redaction_metric_cap())
}

fn catalog_metric_u64(value: usize) -> Result<u64, CaptureCatalogError> {
    u64::try_from(value).map_err(|_| CaptureCatalogError::InvalidRequest)
}

// A deterministic test-only pause immediately before final SQLite
// authorization. The synchronous pause proves that the SQLite deadline/fence
// authorization, rather than task-cancellation timing, owns rollback.
#[cfg(test)]
tokio::task_local! {
    static TEST_CATALOG_FINAL_FENCE_DELAY: Option<Duration>;
}

// A deterministic pause after read-only preflight and immediately before a
// catalog DML gate. It proves deadline expiry is observed before a mutation
// can be dispatched, rather than relying on cancellation of an in-flight
// SQLite future.
#[cfg(test)]
tokio::task_local! {
    static TEST_CATALOG_BEFORE_DML_DELAY: Option<Duration>;
}

#[cfg(test)]
async fn with_catalog_final_fence_delay<F>(delay: Duration, future: F) -> F::Output
where
    F: Future,
{
    TEST_CATALOG_FINAL_FENCE_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
async fn with_catalog_before_dml_delay<F>(delay: Duration, future: F) -> F::Output
where
    F: Future,
{
    TEST_CATALOG_BEFORE_DML_DELAY
        .scope(Some(delay), future)
        .await
}

#[cfg(test)]
fn catalog_test_delay_after_final_fence() {
    if let Ok(Some(delay)) = TEST_CATALOG_FINAL_FENCE_DELAY.try_with(|configured| *configured) {
        std::thread::sleep(delay);
    }
}

#[cfg(test)]
fn catalog_test_delay_before_dml() {
    if let Ok(Some(delay)) = TEST_CATALOG_BEFORE_DML_DELAY.try_with(|configured| *configured) {
        std::thread::sleep(delay);
    }
}

#[cfg(not(test))]
fn catalog_test_delay_after_final_fence() {}

#[cfg(not(test))]
fn catalog_test_delay_before_dml() {}

/// A keyed, opaque delivery identity produced at capture ingress.
///
/// The constructor accepts the legacy persisted v1 spelling and current v2
/// ingress HMAC spelling only. This makes it impossible for a future caller
/// to accidentally persist a raw provider event ID or an unkeyed content
/// hash in the receipt ledger while retaining recovery of already-stored v1
/// receipts.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub(crate) struct OpaqueCaptureReceiptKey(String);

impl OpaqueCaptureReceiptKey {
    pub(crate) fn parse(value: impl Into<String>) -> Result<Self, CaptureCatalogError> {
        let value = value.into();
        let Some(hex) = value
            .strip_prefix(RECEIPT_PREFIX_V1)
            .or_else(|| value.strip_prefix(RECEIPT_PREFIX_V2))
        else {
            return Err(CaptureCatalogError::InvalidReceiptKey);
        };
        if hex.len() != RECEIPT_HEX_BYTES
            || !hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(CaptureCatalogError::InvalidReceiptKey);
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// The action identity emitted by the pure lifecycle reducer.
///
/// It is derived exclusively from the canonical UUID, so this type has no
/// string parser that might accept arbitrary provider content.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CaptureActionKey(String);

impl CaptureActionKey {
    fn for_event(event_id: Uuid) -> Self {
        Self(format!("capture-lifecycle-v1:{event_id}"))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

/// Stable identity for one canonical lifecycle action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogAction {
    event_id: Uuid,
    action_key: CaptureActionKey,
    receipt_key: Option<OpaqueCaptureReceiptKey>,
    /// Only an explicit lifecycle kind can bypass the cross-provider owner
    /// fence. Unknown/manual catalog actions stay fenced by default.
    lifecycle_kind: Option<LifecycleEventKind>,
}

impl CaptureCatalogAction {
    /// Build the action identity expected by `reduce_lifecycle`.
    pub(crate) fn lifecycle(event_id: Uuid, receipt_key: Option<OpaqueCaptureReceiptKey>) -> Self {
        Self {
            event_id,
            action_key: CaptureActionKey::for_event(event_id),
            receipt_key,
            lifecycle_kind: None,
        }
    }

    /// Convert the ingress HMAC string at the typed catalog boundary. This
    /// prevents runtime callers from treating arbitrary text as a receipt.
    pub(crate) fn from_ingress(
        event_id: Uuid,
        dedup_key: Option<&str>,
        lifecycle_kind: LifecycleEventKind,
    ) -> Result<Self, CaptureCatalogError> {
        let receipt_key = dedup_key.map(OpaqueCaptureReceiptKey::parse).transpose()?;
        Ok(Self {
            event_id,
            action_key: CaptureActionKey::for_event(event_id),
            receipt_key,
            lifecycle_kind: Some(lifecycle_kind),
        })
    }

    pub(crate) fn action_key(&self) -> &str {
        self.action_key.as_str()
    }

    pub(crate) fn event_id(&self) -> Uuid {
        self.event_id
    }

    /// Preserve the lifecycle ownership classification when a live terminal
    /// replay atomically adopts the identity stored in a local pending
    /// receipt. The stored receipt deliberately contains only durable action
    /// identity, not provider adapter policy.
    fn with_lifecycle_kind(mut self, lifecycle_kind: LifecycleEventKind) -> Self {
        self.lifecycle_kind = Some(lifecycle_kind);
        self
    }

    /// SessionStart and TurnStart intentionally coexist across providers so
    /// each adapter can establish benign metadata. Every other lifecycle
    /// event, including state-only ToolUse and Compaction, must elect the
    /// rowid-first provider owner before it can mutate durable state.
    fn owner_claim_exempt(&self) -> bool {
        matches!(
            self.lifecycle_kind,
            Some(LifecycleEventKind::SessionStart | LifecycleEventKind::TurnStart)
        )
    }

    /// Return the durable receipt key for an action. Normal lifecycle events
    /// are recorded only when ingress supplied an HMAC delivery identity; a
    /// terminal action must always get a locally recoverable pending receipt
    /// so it cannot publish a false terminal state before checkpoint commit.
    fn receipt_storage_key(&self, defer_terminal: bool) -> Option<String> {
        self.receipt_key
            .as_ref()
            .map(|key| key.as_str().to_string())
            .or_else(|| defer_terminal.then(|| format!("{ACTION_RECEIPT_PREFIX}{}", self.event_id)))
    }

    fn completion_receipt_storage_key(&self) -> String {
        match &self.receipt_key {
            Some(key) => key.as_str().to_string(),
            None => format!("{ACTION_RECEIPT_PREFIX}{}", self.event_id),
        }
    }
}

/// Stable `agent_session` identity plus the already-verified worktree path
/// required by the legacy catalog schema.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogSession {
    session_id: String,
    agent_kind: String,
    provider_session_id: String,
    working_dir: String,
}

impl CaptureCatalogSession {
    pub(crate) fn new(
        session_id: impl Into<String>,
        agent_kind: impl Into<String>,
        provider_session_id: impl Into<String>,
        working_dir: impl Into<String>,
    ) -> Result<Self, CaptureCatalogError> {
        let session = Self {
            session_id: session_id.into(),
            agent_kind: agent_kind.into(),
            provider_session_id: provider_session_id.into(),
            working_dir: working_dir.into(),
        };
        session.validate()?;
        Ok(session)
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session_id
    }

    pub(crate) fn provider_session_id(&self) -> &str {
        &self.provider_session_id
    }

    fn validate(&self) -> Result<(), CaptureCatalogError> {
        validate_catalog_text(&self.session_id, MAX_SESSION_ID_BYTES)?;
        validate_catalog_text(&self.agent_kind, MAX_AGENT_KIND_BYTES)?;
        validate_catalog_text(&self.provider_session_id, MAX_PROVIDER_SESSION_ID_BYTES)?;
        validate_catalog_text(&self.working_dir, MAX_WORKING_DIR_BYTES)?;
        Ok(())
    }
}

/// Safe metadata that the catalog is permitted to merge. This deliberately
/// has no arbitrary JSON or source locator field: callers can retain the
/// existing redaction report shape and concurrent-session bit without giving
/// this store a path, transcript, prompt, or provider payload channel.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CaptureCatalogMetadataPatch {
    concurrent_active: bool,
    redaction_report: Option<CaptureCatalogRedactionReport>,
}

impl CaptureCatalogMetadataPatch {
    pub(crate) fn new(
        concurrent_active: bool,
        redaction_report: Option<CaptureCatalogRedactionReport>,
    ) -> Self {
        Self {
            concurrent_active,
            redaction_report,
        }
    }
}

/// Typed, redaction-only catalog report. The retained rule identifiers and
/// offsets match the existing `agent_session.redaction_report` contract and
/// cannot carry the matched source text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogRedactionReport {
    matches: Vec<RedactionMatch>,
    dropped_matches: usize,
    bytes_scanned: usize,
    bytes_redacted: usize,
    import_pipeline: bool,
}

impl CaptureCatalogRedactionReport {
    pub(crate) fn from_report(report: &RedactionReport) -> Self {
        Self {
            matches: report.matches.clone(),
            dropped_matches: report.dropped_matches,
            bytes_scanned: report.bytes_scanned,
            bytes_redacted: report.bytes_redacted,
            import_pipeline: false,
        }
    }

    /// Convert the importer's already-redacted report into the catalog's
    /// typed shape. The parser is closed over the established safe fields so
    /// a generic JSON value cannot become a source-content metadata channel.
    pub(crate) fn from_import_value(
        value: &serde_json::Value,
    ) -> Result<Self, CaptureCatalogError> {
        let Some(object) = value.as_object() else {
            return Err(CaptureCatalogError::InvalidRequest);
        };
        if object.get("pipeline").and_then(serde_json::Value::as_str) != Some("typed_allowlist")
            || object
                .get("raw_persisted")
                .and_then(serde_json::Value::as_bool)
                != Some(false)
            || object
                .get("snapshot_redaction")
                .is_some_and(|value| value.as_bool() != Some(true))
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let matches = object
            .get("matches")
            .and_then(serde_json::Value::as_array)
            .ok_or(CaptureCatalogError::InvalidRequest)?
            .iter()
            .map(|value| {
                let object = value
                    .as_object()
                    .ok_or(CaptureCatalogError::InvalidRequest)?;
                if object.len() != 3 {
                    return Err(CaptureCatalogError::InvalidRequest);
                }
                let rule_id = object
                    .get("rule_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or(CaptureCatalogError::InvalidRequest)?
                    .to_string();
                validate_catalog_text(&rule_id, 256)?;
                let start = object
                    .get("start")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or(CaptureCatalogError::InvalidRequest)?;
                let end = object
                    .get("end")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or(CaptureCatalogError::InvalidRequest)?;
                if end < start {
                    return Err(CaptureCatalogError::InvalidRequest);
                }
                Ok(RedactionMatch {
                    rule_id,
                    start,
                    end,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        if matches.len() > crate::internal::ai::observed_agents::MAX_REDACTION_MATCH_SAMPLES {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let integer = |field: &str, required: bool| -> Result<usize, CaptureCatalogError> {
            match object.get(field) {
                Some(value) => value
                    .as_u64()
                    .and_then(|value| usize::try_from(value).ok())
                    .ok_or(CaptureCatalogError::InvalidRequest),
                None if required => Err(CaptureCatalogError::InvalidRequest),
                None => Ok(0),
            }
        };
        Ok(Self {
            matches,
            dropped_matches: integer("dropped_matches", false)?,
            bytes_scanned: integer("bytes_scanned", true)?,
            bytes_redacted: integer("bytes_redacted", true)?,
            import_pipeline: true,
        })
    }

    fn json(&self) -> Result<String, CaptureCatalogError> {
        let mut report = serde_json::json!({
            "matches": self.matches,
            "bytes_scanned": self.bytes_scanned,
            "bytes_redacted": self.bytes_redacted,
        });
        if self.import_pipeline {
            let Some(object) = report.as_object_mut() else {
                return Err(CaptureCatalogError::InvalidRequest);
            };
            object.insert(
                "pipeline".to_string(),
                serde_json::Value::String("typed_allowlist".to_string()),
            );
            object.insert(
                "snapshot_redaction".to_string(),
                serde_json::Value::Bool(true),
            );
            object.insert("raw_persisted".to_string(), serde_json::Value::Bool(false));
        }
        if self.dropped_matches != 0 {
            let Some(object) = report.as_object_mut() else {
                return Err(CaptureCatalogError::InvalidRequest);
            };
            object.insert(
                "dropped_matches".to_string(),
                serde_json::Value::from(self.dropped_matches),
            );
        }
        serde_json::to_string(&report).map_err(|_| CaptureCatalogError::InvalidRequest)
    }
}

/// The safe, source-bound facts an import is allowed to attach to a catalog
/// session.  This has no path, transcript, provider payload, or arbitrary
/// JSON channel: `transcript_snapshot` is the redacted-only projection
/// created by the capture snapshot service.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureImportSource {
    source_kind: String,
    source_id: String,
    repository_identity: String,
    source_fingerprint: String,
    /// Ownership schema is deliberately independent from the public catalog
    /// schema.  V2 means the two source fields are repository-keyed HMAC
    /// commitments; V1 remains a read-only compatibility proof only.
    source_identity_schema_version: i64,
    transcript_snapshot: Option<CaptureSnapshotProjection>,
}

impl CaptureImportSource {
    pub(crate) fn new(
        source_kind: impl Into<String>,
        source_id: impl Into<String>,
        repository_identity: impl Into<String>,
        source_fingerprint: impl Into<String>,
        source_identity_schema_version: i64,
        transcript_snapshot: Option<CaptureSnapshotProjection>,
    ) -> Result<Self, CaptureCatalogError> {
        let source = Self {
            source_kind: source_kind.into(),
            source_id: source_id.into(),
            repository_identity: repository_identity.into(),
            source_fingerprint: source_fingerprint.into(),
            source_identity_schema_version,
            transcript_snapshot,
        };
        for value in [
            &source.source_kind,
            &source.source_id,
            &source.repository_identity,
            &source.source_fingerprint,
        ] {
            validate_catalog_text(value, MAX_IMPORT_SOURCE_FIELD_BYTES)?;
        }
        if !matches!(source.source_identity_schema_version, 1 | 2) {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if source.source_identity_schema_version == 2
            && (!matches!(source.source_kind.as_str(), "file" | "export")
                || source.repository_identity != SOURCE_IDENTITY_NOT_RETAINED
                || source.source_fingerprint != source.source_id
                || !is_import_source_hmac_v2(&source.source_id)
                || source
                    .transcript_snapshot
                    .as_ref()
                    .is_some_and(|snapshot| validate_v2_import_snapshot(snapshot).is_err()))
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        Ok(source)
    }

    fn matches_persisted_ownership(
        &self,
        metadata_json: &str,
    ) -> Result<bool, CaptureCatalogError> {
        if self.source_identity_schema_version == 2
            && validate_v2_import_session_metadata(metadata_json).is_err()
        {
            return Ok(false);
        }
        let metadata: serde_json::Value =
            serde_json::from_str(metadata_json).map_err(|_| CaptureCatalogError::InvalidRequest)?;
        let serde_json::Value::Object(metadata) = metadata else {
            return Err(CaptureCatalogError::InvalidRequest);
        };
        for (field, expected) in [
            ("repository_identity", self.repository_identity.as_str()),
            ("source_kind", self.source_kind.as_str()),
            ("source_id", self.source_id.as_str()),
            ("source_fingerprint", self.source_fingerprint.as_str()),
        ] {
            if let Some(actual) = metadata.get(field).and_then(serde_json::Value::as_str)
                && actual != expected
            {
                return Ok(false);
            }
        }
        // V2 never adopts an unversioned/legacy metadata row by accident.
        // V1 is retained only so the scoped migration path can prove exactly
        // what it is replacing before any durable mutation.
        match metadata
            .get("import_source_schema_version")
            .and_then(serde_json::Value::as_i64)
        {
            Some(version) if version == self.source_identity_schema_version => {}
            None if self.source_identity_schema_version == 1 => {}
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// A scoped live-capture row has no import ownership projection yet. It
    /// may be joined by a V2 import, but only when it contains none of the
    /// fields that could identify even a partial import record. The actual
    /// conversion is deferred to the ref/coverage transaction, so an aborted
    /// import cannot replace live metadata with a provisional import record.
    fn may_adopt_live_session_metadata(
        &self,
        metadata_json: &str,
    ) -> Result<bool, CaptureCatalogError> {
        if self.source_identity_schema_version != 2 {
            return Ok(false);
        }
        validate_json_object_keys_are_unique(metadata_json)?;
        let metadata: serde_json::Value =
            serde_json::from_str(metadata_json).map_err(|_| CaptureCatalogError::InvalidRequest)?;
        let serde_json::Value::Object(metadata) = metadata else {
            return Err(CaptureCatalogError::InvalidRequest);
        };
        Ok(IMPORT_OWNERSHIP_METADATA_FIELDS
            .iter()
            .all(|field| !metadata.contains_key(*field)))
    }

    fn matches_persisted_ownership_exact(
        &self,
        metadata_json: &str,
    ) -> Result<bool, CaptureCatalogError> {
        let metadata: serde_json::Value =
            serde_json::from_str(metadata_json).map_err(|_| CaptureCatalogError::InvalidRequest)?;
        let serde_json::Value::Object(metadata) = metadata else {
            return Err(CaptureCatalogError::InvalidRequest);
        };
        for (field, expected) in [
            ("repository_identity", self.repository_identity.as_str()),
            ("source_kind", self.source_kind.as_str()),
            ("source_id", self.source_id.as_str()),
            ("source_fingerprint", self.source_fingerprint.as_str()),
        ] {
            if metadata.get(field).and_then(serde_json::Value::as_str) != Some(expected) {
                return Ok(false);
            }
        }
        Ok(metadata
            .get("import_source_schema_version")
            .and_then(serde_json::Value::as_i64)
            .unwrap_or(1)
            == self.source_identity_schema_version)
    }

    fn ownership_metadata_patch(&self) -> Result<serde_json::Value, CaptureCatalogError> {
        Ok(serde_json::json!({
            "source_kind": self.source_kind,
            "source_id": self.source_id,
            "repository_identity": self.repository_identity,
            "source_fingerprint": self.source_fingerprint,
            "import_source_schema_version": self.source_identity_schema_version,
        }))
    }

    fn metadata_json(
        &self,
        provisional: bool,
        incarnation: Option<String>,
    ) -> Result<String, CaptureCatalogError> {
        let mut metadata = self.ownership_metadata_patch()?;
        let Some(object) = metadata.as_object_mut() else {
            return Err(CaptureCatalogError::InvalidRequest);
        };
        object.extend(serde_json::Map::from_iter([
            (
                "import_provisional".to_string(),
                serde_json::Value::Bool(provisional),
            ),
            (
                "transcript_snapshot".to_string(),
                serde_json::to_value(&self.transcript_snapshot)
                    .map_err(|_| CaptureCatalogError::InvalidRequest)?,
            ),
        ]));
        if !provisional {
            object.insert("imported".to_string(), serde_json::Value::Bool(true));
        }
        if let Some(incarnation) = incarnation {
            object.insert(
                "capture_incarnation".to_string(),
                serde_json::Value::String(incarnation),
            );
        }
        let metadata_json =
            serde_json::to_string(&metadata).map_err(|_| CaptureCatalogError::InvalidRequest)?;
        if self.source_identity_schema_version == 2 {
            sanitize_v2_import_session_metadata(&metadata_json)
        } else {
            Ok(metadata_json)
        }
    }
}

/// The complete, closed V2 metadata object for an imported session.
///
/// This is intentionally not a public wire struct: callers validate or
/// sanitize a serialized object through the narrow functions below rather
/// than gaining an arbitrary JSON extension channel.  In particular, every
/// nested snapshot type is deserialized through its `deny_unknown_fields`
/// projection before cloud or catalog code can persist it.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ClosedV2ImportSessionMetadata {
    repository_identity: String,
    source_kind: String,
    source_id: String,
    source_fingerprint: String,
    import_source_schema_version: i64,
    import_provisional: bool,
    transcript_snapshot: Option<CaptureSnapshotProjection>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    imported: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_incarnation: Option<String>,
    #[serde(
        default,
        rename = "capture_catalog_receipts_v1",
        skip_serializing_if = "Option::is_none"
    )]
    receipt_ledger: Option<StoredReceiptLedger>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    concurrent_active: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_status: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_error_code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_error_stage: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_attempt_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    capture_failed_at: Option<i64>,
}

/// The only live-capture fields that may survive an atomic conversion to the
/// closed V2 import projection. Unknown live metadata is intentionally not
/// copied; it may contain a legacy provider payload and cannot become a
/// cloud-publishable import record by accident.
#[derive(Default, Deserialize)]
struct AdoptableLiveSessionExtensions {
    #[serde(default)]
    capture_incarnation: Option<String>,
    #[serde(default, rename = "capture_catalog_receipts_v1")]
    receipt_ledger: Option<StoredReceiptLedger>,
    #[serde(default)]
    concurrent_active: Option<bool>,
    #[serde(default)]
    capture_status: Option<String>,
    #[serde(default)]
    capture_error_code: Option<String>,
    #[serde(default)]
    capture_error_stage: Option<String>,
    #[serde(default)]
    capture_attempt_id: Option<String>,
    #[serde(default)]
    capture_failed_at: Option<i64>,
}

/// An import lifecycle commit advances `sync_revision`; copying a pending
/// receipt across that boundary would strand a deferred terminal finalizer
/// behind its exact revision fence. This must apply to a just-adopted live
/// row and to later imports of an already-adopted V2 row alike.
fn session_has_no_unsettled_receipts(
    existing: &StoredCatalogSession,
) -> Result<bool, CaptureCatalogError> {
    let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)
        .map_err(|_| CaptureCatalogError::ImportSessionConflict)?;
    Ok(ledger
        .entries
        .iter()
        .all(|receipt| receipt.status == StoredReceiptStatus::Complete))
}

/// A historical import may join a scoped live session only after the live
/// catalog is quiescent. Keep this check at both prepare and commit time.
/// Preparation protects the common path, while the commit-time check closes
/// the live-hook race between lease acquisition and the ref/CAS transaction.
fn live_session_is_adoptable(
    source: &CaptureImportSource,
    existing: &StoredCatalogSession,
) -> Result<bool, CaptureCatalogError> {
    if !matches!(
        existing.state.phase,
        CapturePhase::Active | CapturePhase::Condensed | CapturePhase::Stopped
    ) || !source.may_adopt_live_session_metadata(&existing.metadata_json)?
    {
        return Ok(false);
    }
    session_has_no_unsettled_receipts(existing)
}

fn closed_v2_metadata_from_live_session(
    source: &CaptureImportSource,
    existing: &StoredCatalogSession,
) -> Result<ClosedV2ImportSessionMetadata, CaptureCatalogError> {
    if !live_session_is_adoptable(source, existing)? {
        return Err(CaptureCatalogError::ImportSessionConflict);
    }
    let extensions: AdoptableLiveSessionExtensions = serde_json::from_str(&existing.metadata_json)
        .map_err(|_| CaptureCatalogError::ImportSessionConflict)?;
    let mut metadata =
        parse_closed_v2_import_session_metadata(&source.metadata_json(false, None)?)?;
    metadata.capture_incarnation = extensions.capture_incarnation;
    metadata.receipt_ledger = extensions.receipt_ledger;
    metadata.concurrent_active = extensions.concurrent_active;
    metadata.capture_status = extensions.capture_status;
    metadata.capture_error_code = extensions.capture_error_code;
    metadata.capture_error_stage = extensions.capture_error_stage;
    metadata.capture_attempt_id = extensions.capture_attempt_id;
    metadata.capture_failed_at = extensions.capture_failed_at;
    validate_closed_v2_import_session_metadata(&metadata)?;
    Ok(metadata)
}

fn is_import_source_hmac_v2(value: &str) -> bool {
    let Some(hex) = value.strip_prefix(IMPORT_SOURCE_HMAC_V2_PREFIX) else {
        return false;
    };
    hex.len() == SOURCE_COMMITMENT_HEX_BYTES
        && hex
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

fn is_snapshot_source_commitment_v2(value: &str) -> bool {
    is_import_source_hmac_v2(value)
}

fn is_canonical_uuid(value: &str) -> bool {
    Uuid::parse_str(value).is_ok_and(|uuid| uuid.to_string() == value)
}

fn is_canonical_lifecycle_action_key(value: &str) -> bool {
    value
        .strip_prefix("capture-lifecycle-v1:")
        .is_some_and(is_canonical_uuid)
}

fn is_v2_finalizer_marker_generation(value: &str) -> bool {
    match value.strip_prefix(UNBOUND_FINALIZER_MARKER_PREFIX) {
        Some(action_key) => is_canonical_lifecycle_action_key(action_key),
        None => is_canonical_uuid(value),
    }
}

/// The finalizer reader accepts bare hexadecimal source digests solely to
/// recover immutable V1 evidence. A V2 session, including a V1 ledger being
/// migrated into one, must not carry that ambiguous spelling forward.
fn validate_v2_import_receipt_ledger(
    ledger: &StoredReceiptLedger,
) -> Result<(), CaptureCatalogError> {
    ledger.validate()?;
    for receipt in &ledger.entries {
        let Some(finalizer) = receipt.finalizer.as_ref() else {
            continue;
        };
        if finalizer
            .source_digest
            .as_deref()
            .is_some_and(|digest| !is_snapshot_source_commitment_v2(digest))
            || !is_v2_finalizer_marker_generation(&finalizer.marker_generation)
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
    }
    Ok(())
}

fn validate_v2_import_snapshot(
    snapshot: &CaptureSnapshotProjection,
) -> Result<(), CaptureCatalogError> {
    let transcript_redacted_bytes = catalog_metric_u64(snapshot.transcript_redacted_bytes)?;
    let redaction_bytes_redacted = catalog_metric_u64(snapshot.redaction_bytes_redacted)?;
    let redaction_match_count = catalog_metric_u64(snapshot.redaction_match_count)?;
    let output_cap = if let Some(source) = snapshot.source.as_ref() {
        if source.identity != SOURCE_IDENTITY_NOT_RETAINED
            || source
                .digest_sha256
                .as_deref()
                .is_some_and(|digest| !is_snapshot_source_commitment_v2(digest))
            || source
                .byte_len
                .is_some_and(|bytes| bytes > TRANSCRIPT_READ_HARD_CAP_BYTES)
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        // Parsing the enum is not enough documentation of this boundary:
        // only the three authorization classes may reach a durable import
        // projection, and none carries an identity other than the sentinel.
        match source.kind {
            CaptureSnapshotSourceKind::ProviderFile
            | CaptureSnapshotSourceKind::TrustedExport
            | CaptureSnapshotSourceKind::DiscoveredSubagent => {}
        }
        source
            .byte_len
            .map(capture_redacted_output_cap)
            .unwrap_or_else(snapshot_redacted_output_cap)
    } else {
        snapshot_redacted_output_cap()
    };
    let metric_cap = redaction_metric_cap(output_cap);
    if transcript_redacted_bytes > output_cap
        || redaction_bytes_redacted > metric_cap
        || redaction_match_count > redaction_match_count_cap(metric_cap)
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    match snapshot.completeness {
        CaptureSnapshotCompleteness::Complete => {
            let Some(source) = snapshot.source.as_ref() else {
                return Err(CaptureCatalogError::InvalidRequest);
            };
            let source_byte_len = source.byte_len.ok_or(CaptureCatalogError::InvalidRequest)?;
            let bytes_scanned = catalog_metric_u64(snapshot.redaction_bytes_scanned)?;
            if snapshot.partial_reason.is_some()
                || snapshot.transcript_redacted_bytes == 0
                || source.digest_sha256.is_none()
                // `source_byte_len` describes the authorized pre-redaction
                // input, while `transcript_redacted_bytes` describes emitted
                // safe bytes. A replacement placeholder may legitimately be
                // longer than the secret it replaces, so only the redaction
                // scan boundary is a stable equality invariant.
                || source_byte_len != bytes_scanned
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
        }
        CaptureSnapshotCompleteness::Partial => {
            if snapshot.partial_reason.is_none()
                || snapshot.transcript_redacted_bytes != 0
                || snapshot.redaction_match_count != 0
                || snapshot.redaction_bytes_scanned != 0
                || snapshot.redaction_bytes_redacted != 0
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
        }
    }
    Ok(())
}

/// `serde_json::Value` is a last-key-wins representation. Reject duplicate
/// keys before a V2 ownership record reaches it, otherwise an unvalidated raw
/// value can survive a cloud copy behind a later safe-looking duplicate.
pub(crate) fn validate_json_object_keys_are_unique(value: &str) -> Result<(), CaptureCatalogError> {
    parse_canon_value(value.as_bytes())
        .map(|_| ())
        .map_err(|_| CaptureCatalogError::InvalidRequest)
}

fn parse_closed_v2_import_session_metadata(
    metadata_json: &str,
) -> Result<ClosedV2ImportSessionMetadata, CaptureCatalogError> {
    validate_json_object_keys_are_unique(metadata_json)?;
    let value: serde_json::Value =
        serde_json::from_str(metadata_json).map_err(|_| CaptureCatalogError::InvalidRequest)?;
    let serde_json::Value::Object(object) = &value else {
        return Err(CaptureCatalogError::InvalidRequest);
    };
    for required in [
        "repository_identity",
        "source_kind",
        "source_id",
        "source_fingerprint",
        "import_source_schema_version",
        "import_provisional",
        "transcript_snapshot",
    ] {
        if !object.contains_key(required) {
            return Err(CaptureCatalogError::InvalidRequest);
        }
    }
    serde_json::from_value(value).map_err(|_| CaptureCatalogError::InvalidRequest)
}

fn validate_closed_v2_import_session_metadata(
    metadata: &ClosedV2ImportSessionMetadata,
) -> Result<(), CaptureCatalogError> {
    if metadata.repository_identity != SOURCE_IDENTITY_NOT_RETAINED
        || !matches!(metadata.source_kind.as_str(), "file" | "export")
        || !is_import_source_hmac_v2(&metadata.source_id)
        || metadata.source_fingerprint != metadata.source_id
        || metadata.import_source_schema_version != 2
        || (metadata.import_provisional && metadata.imported.is_some())
        || (!metadata.import_provisional && metadata.imported != Some(true))
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    validate_safe_import_metadata_extensions(metadata)
}

fn validate_safe_import_metadata_extensions(
    metadata: &ClosedV2ImportSessionMetadata,
) -> Result<(), CaptureCatalogError> {
    if let Some(snapshot) = metadata.transcript_snapshot.as_ref() {
        validate_v2_import_snapshot(snapshot)?;
    }
    if metadata
        .capture_incarnation
        .as_deref()
        .is_some_and(|incarnation| {
            incarnation.len() != 32
                || !incarnation
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        })
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    if let Some(ledger) = metadata.receipt_ledger.as_ref() {
        validate_v2_import_receipt_ledger(ledger)?;
    }
    match (
        metadata.capture_status.as_deref(),
        metadata.capture_error_code.as_deref(),
        metadata.capture_error_stage.as_deref(),
        metadata.capture_attempt_id.as_deref(),
        metadata.capture_failed_at,
    ) {
        (None, None, None, None, None) => {}
        (
            Some("retryable"),
            Some("checkpoint_write_failed"),
            Some("maintenance_lock"),
            Some("maintenance-lock"),
            Some(_),
        )
        | (
            Some("retryable"),
            Some("checkpoint_write_failed"),
            Some("checkpoint_write"),
            Some("checkpoint-write"),
            Some(_),
        ) => {}
        _ => return Err(CaptureCatalogError::InvalidRequest),
    }
    Ok(())
}

fn sanitize_v2_import_redaction_report(
    redaction_report_json: &str,
) -> Result<String, CaptureCatalogError> {
    validate_json_object_keys_are_unique(redaction_report_json)?;
    let value: serde_json::Value = serde_json::from_str(redaction_report_json)
        .map_err(|_| CaptureCatalogError::InvalidRequest)?;
    let serde_json::Value::Object(wrapper) = &value else {
        return Err(CaptureCatalogError::InvalidRequest);
    };
    if wrapper.len() != 1 {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let import = wrapper
        .get("import")
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    let Some(import_object) = import.as_object() else {
        return Err(CaptureCatalogError::InvalidRequest);
    };
    const IMPORT_REDACTION_FIELDS: &[&str] = &[
        "pipeline",
        "snapshot_redaction",
        "raw_persisted",
        "matches",
        "bytes_scanned",
        "bytes_redacted",
        "dropped_matches",
    ];
    if import_object
        .keys()
        .any(|key| !IMPORT_REDACTION_FIELDS.iter().any(|allowed| key == allowed))
        || import_object
            .get("pipeline")
            .and_then(serde_json::Value::as_str)
            != Some("typed_allowlist")
        || import_object
            .get("snapshot_redaction")
            .and_then(serde_json::Value::as_bool)
            != Some(true)
        || import_object
            .get("raw_persisted")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let report = CaptureCatalogRedactionReport::from_import_value(import)?;
    let bytes_scanned = catalog_metric_u64(report.bytes_scanned)?;
    let bytes_redacted = catalog_metric_u64(report.bytes_redacted)?;
    let dropped_matches = catalog_metric_u64(report.dropped_matches)?;
    if bytes_scanned > import_redaction_scanned_cap()
        || bytes_redacted > import_redaction_metric_cap()
        || dropped_matches > import_redaction_dropped_match_cap()
        || report.matches.iter().any(|matched| {
            catalog_metric_u64(matched.end)
                .map_or(true, |end| end > import_redaction_coordinate_cap())
                || !is_durable_default_rule_id(&matched.rule_id)
        })
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    import_redaction_report_json(&report)
}

fn validate_v2_import_redaction_report(
    redaction_report_json: &str,
) -> Result<(), CaptureCatalogError> {
    sanitize_v2_import_redaction_report(redaction_report_json).map(|_| ())
}

/// Validate the closed, typed metadata contract for a durable V2 import
/// session. Cloud publish/restore callers use this exact validator so an
/// unknown top-level key, nested locator, bare digest, or malformed receipt
/// cannot be copied into a remote catalog record.
pub(crate) fn validate_v2_import_session_metadata(
    metadata_json: &str,
) -> Result<(), CaptureCatalogError> {
    let metadata = parse_closed_v2_import_session_metadata(metadata_json)?;
    validate_closed_v2_import_session_metadata(&metadata)
}

/// Validate the full durable V2 import session projection.  The ownership
/// metadata and redaction report occupy separate SQL columns, so cloud and
/// migration callers must use this record-level gate before copying either.
pub(crate) fn validate_v2_import_session_record(
    metadata_json: &str,
    redaction_report_json: &str,
) -> Result<(), CaptureCatalogError> {
    validate_v2_import_session_metadata(metadata_json)?;
    validate_v2_import_redaction_report(redaction_report_json)
}

/// Identify an existing V2 import record before a generic live-capture path
/// mutates it. The ownership and report columns form one closed record: a hook
/// update must not turn a valid historical import into a row cloud cannot
/// safely publish or restore.
fn validate_existing_v2_import_session_record(
    metadata_json: &str,
    redaction_report_json: &str,
) -> Result<bool, CaptureCatalogError> {
    // This parser also decides whether the row is V2. Reject duplicates
    // before that decision so a malformed schema-version pair cannot bypass
    // the closed-record validator and be copied as a legacy row.
    validate_json_object_keys_are_unique(metadata_json)?;
    let metadata: serde_json::Value =
        serde_json::from_str(metadata_json).map_err(|_| CaptureCatalogError::InvalidRequest)?;
    let is_v2 = metadata
        .as_object()
        .and_then(|object| object.get("import_source_schema_version"))
        .and_then(serde_json::Value::as_i64)
        == Some(2);
    if is_v2 {
        validate_v2_import_session_record(metadata_json, redaction_report_json)?;
    }
    Ok(is_v2)
}

fn validate_v2_import_session_record_after_mutation(
    is_v2_import: bool,
    metadata_json: &str,
    redaction_report_json: &str,
) -> Result<(), CaptureCatalogError> {
    if is_v2_import {
        validate_v2_import_session_record(metadata_json, redaction_report_json)?;
    }
    Ok(())
}

/// Parse, validate, and re-encode V2 import metadata through its closed
/// schema. The returned JSON contains only the documented typed fields and
/// is safe to use as a replacement value rather than patching an untrusted
/// legacy object.
pub(crate) fn sanitize_v2_import_session_metadata(
    metadata_json: &str,
) -> Result<String, CaptureCatalogError> {
    let metadata = parse_closed_v2_import_session_metadata(metadata_json)?;
    validate_closed_v2_import_session_metadata(&metadata)?;
    serde_json::to_string(&metadata).map_err(|_| CaptureCatalogError::InvalidRequest)
}

/// Rebuild a committed V1 metadata object into the closed V2 schema without
/// patching it. V1 source fields are read only as the caller's already-scoped
/// exact proof; this function copies no V1 ownership string into its output.
/// Unknown top-level fields and malformed nested snapshots fail closed, which
/// leaves the immutable V1 evidence untouched for recovery.
fn sanitize_committed_legacy_import_metadata_for_v2(
    metadata_json: &str,
    v2_source: &CaptureImportSource,
) -> Result<String, CaptureCatalogError> {
    let mut value: serde_json::Value =
        serde_json::from_str(metadata_json).map_err(|_| CaptureCatalogError::InvalidRequest)?;
    let serde_json::Value::Object(object) = &mut value else {
        return Err(CaptureCatalogError::InvalidRequest);
    };
    // The original V1 projection predates the snapshot field and sometimes
    // omitted its schema version. Treat those omissions as their documented
    // legacy defaults only while rebuilding a strictly V2 object.
    object
        .entry("import_source_schema_version".to_string())
        .or_insert_with(|| serde_json::Value::from(1));
    object
        .entry("transcript_snapshot".to_string())
        .or_insert(serde_json::Value::Null);
    for required in [
        "repository_identity",
        "source_kind",
        "source_id",
        "source_fingerprint",
        "import_source_schema_version",
        "import_provisional",
        "imported",
        "transcript_snapshot",
    ] {
        if !object.contains_key(required) {
            return Err(CaptureCatalogError::InvalidRequest);
        }
    }
    let legacy: ClosedV2ImportSessionMetadata =
        serde_json::from_value(value).map_err(|_| CaptureCatalogError::InvalidRequest)?;
    if legacy.import_source_schema_version != 1
        || legacy.import_provisional
        || legacy.imported != Some(true)
    {
        return Err(CaptureCatalogError::ImportSessionConflict);
    }
    validate_safe_import_metadata_extensions(&legacy)?;
    let v2 = ClosedV2ImportSessionMetadata {
        repository_identity: v2_source.repository_identity.clone(),
        source_kind: v2_source.source_kind.clone(),
        source_id: v2_source.source_id.clone(),
        source_fingerprint: v2_source.source_fingerprint.clone(),
        import_source_schema_version: 2,
        import_provisional: false,
        transcript_snapshot: legacy.transcript_snapshot,
        imported: Some(true),
        capture_incarnation: legacy.capture_incarnation,
        receipt_ledger: legacy.receipt_ledger,
        concurrent_active: legacy.concurrent_active,
        capture_status: legacy.capture_status,
        capture_error_code: legacy.capture_error_code,
        capture_error_stage: legacy.capture_error_stage,
        capture_attempt_id: legacy.capture_attempt_id,
        capture_failed_at: legacy.capture_failed_at,
    };
    validate_closed_v2_import_session_metadata(&v2)?;
    serde_json::to_string(&v2).map_err(|_| CaptureCatalogError::InvalidRequest)
}

/// Typed request to materialize the provisional catalog row that backs an
/// import lease.  It is deliberately transaction-bound: calling the regular
/// catalog `apply` port here would split the erase/tombstone barrier from the
/// import identity lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureImportSessionPrepareRequest {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    source: CaptureImportSource,
    redaction_report: CaptureCatalogRedactionReport,
    started_at: i64,
    expected_existing_fingerprint: Option<String>,
}

impl CaptureImportSessionPrepareRequest {
    pub(crate) fn new(
        scope: CaptureScope,
        session: CaptureCatalogSession,
        source: CaptureImportSource,
        redaction_report: CaptureCatalogRedactionReport,
        started_at: i64,
        expected_existing_fingerprint: Option<String>,
    ) -> Result<Self, CaptureCatalogError> {
        session.validate()?;
        if let Some(fingerprint) = expected_existing_fingerprint.as_deref()
            && (fingerprint.len() != 64
                || !fingerprint
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        Ok(Self {
            scope,
            session,
            source,
            redaction_report,
            started_at,
            expected_existing_fingerprint,
        })
    }
}

/// A provisional import session was either created atomically with the lease
/// or an already-prepared matching row was retained.  No caller receives the
/// raw metadata used for ownership validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureImportSessionPrepareResult {
    Created,
    Existing,
}

/// The only lifecycle shapes an import can commit alongside a ref/coverage
/// transaction.  An import may reactivate a newer nonterminal tail and must
/// therefore be allowed to clear a prior terminal timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureImportSessionLifecycleState {
    Active,
    Stopped,
}

impl CaptureImportSessionLifecycleState {
    #[cfg(test)]
    pub(crate) fn from_import_state(
        state: &str,
        stopped_at: Option<i64>,
    ) -> Result<Self, CaptureCatalogError> {
        match (state, stopped_at) {
            ("active", None) => Ok(Self::Active),
            ("stopped", Some(_)) => Ok(Self::Stopped),
            _ => Err(CaptureCatalogError::InvalidRequest),
        }
    }

    const fn as_db(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Stopped => "stopped",
        }
    }
}

/// Transaction companion for one imported turn.  The checkpoint facade runs
/// it in the same ref-CAS transaction as the coverage and import-identity
/// mutation, so it cannot publish lifecycle state separately from the turn.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureImportSessionCommit {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    source: CaptureImportSource,
    redaction_report: CaptureCatalogRedactionReport,
    state: CaptureImportSessionLifecycleState,
    started_at: i64,
    last_event_at: i64,
    stopped_at: Option<i64>,
}

impl CaptureImportSessionCommit {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        scope: CaptureScope,
        session: CaptureCatalogSession,
        source: CaptureImportSource,
        redaction_report: CaptureCatalogRedactionReport,
        state: CaptureImportSessionLifecycleState,
        started_at: i64,
        last_event_at: i64,
        stopped_at: Option<i64>,
    ) -> Result<Self, CaptureCatalogError> {
        session.validate()?;
        match (state, stopped_at) {
            (CaptureImportSessionLifecycleState::Active, None)
            | (CaptureImportSessionLifecycleState::Stopped, Some(_)) => Ok(Self {
                scope,
                session,
                source,
                redaction_report,
                state,
                started_at,
                last_event_at,
                stopped_at,
            }),
            _ => Err(CaptureCatalogError::InvalidRequest),
        }
    }
}

/// The optimistic state/fence precondition and one reducer-produced durable
/// mutation. This is intentionally free of transcript, provider payload, and
/// generic JSON fields.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogMutation {
    expected: Option<DurableCaptureState>,
    next_phase: CapturePhase,
    stopped_at: StoppedAtMutation,
    checkpoint: CheckpointWrite,
    observed_at: i64,
}

impl CaptureCatalogMutation {
    pub(crate) fn from_reducer(
        expected: Option<DurableCaptureState>,
        action: &LifecycleActionPlan,
        observed_at: i64,
    ) -> Result<Self, CaptureCatalogError> {
        if expected.map(|state| state.sync_revision) != action.expected_sync_revision {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let mutation = Self {
            expected,
            next_phase: action.next_phase,
            stopped_at: action.stopped_at,
            checkpoint: action.checkpoint,
            observed_at,
        };
        mutation.validate()?;
        Ok(mutation)
    }

    #[cfg(test)]
    fn new(
        expected: Option<DurableCaptureState>,
        next_phase: CapturePhase,
        stopped_at: StoppedAtMutation,
        checkpoint: CheckpointWrite,
        observed_at: i64,
    ) -> Result<Self, CaptureCatalogError> {
        let mutation = Self {
            expected,
            next_phase,
            stopped_at,
            checkpoint,
            observed_at,
        };
        mutation.validate()?;
        Ok(mutation)
    }

    pub(crate) fn checkpoint(&self) -> CheckpointWrite {
        self.checkpoint
    }

    pub(crate) fn is_terminal(&self) -> bool {
        self.next_phase == CapturePhase::Stopped
    }

    /// A terminal receipt may be acknowledged without manufacturing another
    /// checkpoint only when it was reserved on top of an already published
    /// terminal state. The coordinator additionally requires a fully-covered
    /// coverage-gate outcome; this predicate makes that exceptional route
    /// unavailable for a first terminal transition or an incomplete legacy
    /// stop.
    pub(crate) fn is_durable_terminal_replay_candidate(&self) -> bool {
        self.is_terminal()
            && self.checkpoint == CheckpointWrite::Committed
            && self.expected.is_some_and(|state| {
                state.phase == CapturePhase::Stopped && state.stopped_at.is_some()
            })
    }

    fn validate(&self) -> Result<(), CaptureCatalogError> {
        match (self.next_phase, self.stopped_at) {
            (CapturePhase::Stopped, StoppedAtMutation::Set(_))
            | (
                CapturePhase::Pending | CapturePhase::Active | CapturePhase::Condensed,
                StoppedAtMutation::Preserve,
            ) => {}
            // The reducer never emits a direct quarantine transition. It is
            // reserved for the bounded finalizer/repair policy.
            (CapturePhase::Quarantined, _) => return Err(CaptureCatalogError::InvalidRequest),
            _ => return Err(CaptureCatalogError::InvalidRequest),
        }
        if self
            .expected
            .is_some_and(|state| state.sync_revision == i64::MAX)
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        // A terminal mutation is intentionally deferred until `complete`.
        // Without a checkpoint there is no completion path, which would leave
        // a permanently pending live row rather than falsely publishing it.
        if self.is_terminal() && self.checkpoint == CheckpointWrite::None {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        Ok(())
    }
}

/// Typed state-write request for the catalog port.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogApplyRequest {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    action: CaptureCatalogAction,
    mutation: CaptureCatalogMutation,
    metadata: CaptureCatalogMetadataPatch,
}

impl CaptureCatalogApplyRequest {
    pub(crate) fn new(
        scope: CaptureScope,
        session: CaptureCatalogSession,
        action: CaptureCatalogAction,
        mutation: CaptureCatalogMutation,
    ) -> Result<Self, CaptureCatalogError> {
        session.validate()?;
        mutation.validate()?;
        Ok(Self {
            scope,
            session,
            action,
            mutation,
            metadata: CaptureCatalogMetadataPatch::default(),
        })
    }

    pub(crate) fn with_metadata(mut self, metadata: CaptureCatalogMetadataPatch) -> Self {
        self.metadata = metadata;
        self
    }

    /// Replace only the canonical action identity after the catalog atomically
    /// adopted an eligible local terminal pending receipt. The session,
    /// mutation, scope, and metadata remain those of the current ingress.
    pub(crate) fn with_effective_action(mut self, action: CaptureCatalogAction) -> Self {
        self.action = action;
        self
    }

    pub(crate) fn session(&self) -> &CaptureCatalogSession {
        &self.session
    }

    pub(crate) fn action(&self) -> &CaptureCatalogAction {
        &self.action
    }

    pub(crate) fn mutation(&self) -> &CaptureCatalogMutation {
        &self.mutation
    }
}

/// Request to finish a receipt after the checkpoint facade has committed its
/// object/ref/catalog transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogCompleteRequest {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    action: CaptureCatalogAction,
    completion: CaptureCatalogCompletion,
}

/// The narrowly typed evidence that authorizes a receipt completion.
///
/// A terminal transition normally requires a marker/source proof from its
/// own checkpoint attempt.  `CoveredTerminalReplay` is deliberately narrower:
/// it can only acknowledge a fully covered native replay after an already
/// durable terminal state, and therefore never publishes or changes a
/// terminal state on its own.
#[derive(Clone, Debug, PartialEq, Eq)]
enum CaptureCatalogCompletion {
    /// Retained for nonterminal state-only receipts.
    Ordinary,
    /// A strict terminal proof issued by the finalizer after a durable
    /// checkpoint under the receipt's marker/source fence.
    Finalizer(CaptureCatalogFinalizeProof),
    /// A coverage-gate-confirmed replay of an already durable terminal
    /// session. Construction is restricted to the coordinator reservation.
    CoveredTerminalReplay,
}

impl CaptureCatalogCompleteRequest {
    pub(crate) fn new(
        scope: CaptureScope,
        session: CaptureCatalogSession,
        action: CaptureCatalogAction,
    ) -> Result<Self, CaptureCatalogError> {
        session.validate()?;
        Ok(Self {
            scope,
            session,
            action,
            completion: CaptureCatalogCompletion::Ordinary,
        })
    }

    pub(crate) fn from_apply(
        request: &CaptureCatalogApplyRequest,
    ) -> Result<Self, CaptureCatalogError> {
        Self::new(
            request.scope.clone(),
            request.session.clone(),
            request.action.clone(),
        )
    }

    /// Build a terminal completion that is bound to a durable finalizer
    /// decision. The proof is unforgeable outside this module: it is issued
    /// only after the catalog has revalidated the pending receipt's marker
    /// generation and source digest against a `Durable` checkpoint outcome.
    pub(crate) fn from_finalizer(
        request: &CaptureCatalogApplyRequest,
        proof: CaptureCatalogFinalizeProof,
    ) -> Result<Self, CaptureCatalogError> {
        let mut completion = Self::from_apply(request)?;
        completion.completion = CaptureCatalogCompletion::Finalizer(proof);
        Ok(completion)
    }

    /// Build the only proofless terminal completion. The caller has already
    /// established, through the coverage gate, that every normalized turn is
    /// covered by a prior durable checkpoint; the catalog still rechecks the
    /// pre-reserved terminal state and provisional-finalizer fence.
    pub(in crate::internal::ai::capture) fn from_covered_terminal_replay(
        request: &CaptureCatalogApplyRequest,
    ) -> Result<Self, CaptureCatalogError> {
        if !request.mutation.is_terminal()
            || request.mutation.checkpoint != CheckpointWrite::Committed
            || !request.mutation.is_durable_terminal_replay_candidate()
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let mut completion = Self::from_apply(request)?;
        completion.completion = CaptureCatalogCompletion::CoveredTerminalReplay;
        Ok(completion)
    }
}

/// Content-free stages for the hook's best-effort retry diagnostic. Keeping
/// the vocabulary closed prevents a runtime error string or provider payload
/// from entering `agent_session.metadata_json`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogRetryableStage {
    MaintenanceLock,
    CheckpointWrite,
}

impl CaptureCatalogRetryableStage {
    fn attempt_id(self) -> &'static str {
        match self {
            Self::MaintenanceLock => "maintenance-lock",
            Self::CheckpointWrite => "checkpoint-write",
        }
    }

    fn metadata_stage(self) -> &'static str {
        match self {
            Self::MaintenanceLock => "maintenance_lock",
            Self::CheckpointWrite => "checkpoint_write",
        }
    }
}

/// One scope/session-fenced mutation of the sanitized retry diagnostics.
/// Unlike lifecycle state, this metadata update intentionally preserves the
/// session revision so it cannot fence a pending terminal receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogDiagnostic {
    RecordRetryableCheckpointFailure {
        stage: CaptureCatalogRetryableStage,
        failed_at: i64,
    },
    ClearRetryableCheckpointFailure,
}

/// Content-free evidence issued by the catalog immediately before a strict
/// terminal completion. It is intentionally opaque to callers: only the
/// catalog can construct it after checking the persisted finalizer receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogFinalizeProof {
    replay_key: String,
    marker_generation: String,
    source_digest: Option<String>,
}

/// A bounded finalizer transition attached to an already-reserved terminal
/// catalog receipt. The policy carries no provider identity; the session and
/// action remain separately scope-fenced catalog identities.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogFinalizeRequest {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    action: CaptureCatalogAction,
    checkpoint_write: CheckpointWrite,
    policy: CaptureFinalizePolicy,
    marker_generation: String,
    source_digest: Option<String>,
    now_millis: i64,
    checkpoint: FinalizeCheckpointProgress,
}

impl CaptureCatalogFinalizeRequest {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        scope: CaptureScope,
        session: CaptureCatalogSession,
        action: CaptureCatalogAction,
        checkpoint_write: CheckpointWrite,
        policy: CaptureFinalizePolicy,
        marker_generation: impl Into<String>,
        source_digest: Option<String>,
        now_millis: i64,
        checkpoint: FinalizeCheckpointProgress,
    ) -> Result<Self, CaptureCatalogError> {
        session.validate()?;
        if policy.replay_key() != action.action_key() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if checkpoint_write == CheckpointWrite::None {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let marker_generation = marker_generation.into();
        // Reuse the pure finalizer's bounded/content-free validation before
        // this request reaches the durable ledger.
        PendingFinalizeReceipt::new(
            &policy,
            marker_generation.clone(),
            source_digest.clone(),
            now_millis,
            FinalizePendingStage::Snapshot,
        )
        .map_err(|_| CaptureCatalogError::InvalidRequest)?;
        Ok(Self {
            scope,
            session,
            action,
            checkpoint_write,
            policy,
            marker_generation,
            source_digest,
            now_millis,
            checkpoint,
        })
    }

    pub(crate) fn from_apply(
        apply: &CaptureCatalogApplyRequest,
        policy: CaptureFinalizePolicy,
        marker_generation: impl Into<String>,
        source_digest: Option<String>,
        now_millis: i64,
        checkpoint: FinalizeCheckpointProgress,
    ) -> Result<Self, CaptureCatalogError> {
        Self::new(
            apply.scope.clone(),
            apply.session.clone(),
            apply.action.clone(),
            apply.mutation.checkpoint,
            policy,
            marker_generation,
            source_digest,
            now_millis,
            checkpoint,
        )
    }
}

/// Opaque catalog proof that authorizes exactly one terminal marker
/// registration. The checkpoint store revalidates it inside its own marker
/// transaction, so a catalog decision made after a writer crashed cannot let
/// that writer register or publish under a stale source fence later.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCatalogTerminalAttemptFence {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    action: CaptureCatalogAction,
    checkpoint_write: CheckpointWrite,
    marker_generation: String,
    source_digest: Option<String>,
    manual_artifact: Option<ManualArtifactRegistrationAuthority>,
}

#[derive(Clone)]
struct ManualArtifactRegistrationAuthority {
    header: super::pending::PendingHeader,
    context: PendingSessionContext,
    alias_record: String,
    registration_consumed: Arc<AtomicBool>,
}

impl PartialEq for ManualArtifactRegistrationAuthority {
    fn eq(&self, other: &Self) -> bool {
        self.header == other.header
            && self.context == other.context
            && self.alias_record == other.alias_record
            && Arc::ptr_eq(&self.registration_consumed, &other.registration_consumed)
    }
}

impl Eq for ManualArtifactRegistrationAuthority {}

impl std::fmt::Debug for ManualArtifactRegistrationAuthority {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ManualArtifactRegistrationAuthority")
    }
}

/// Result of revalidating a catalog-owned terminal writer election immediately
/// before the traces marker is registered.  A receipt can complete after a
/// duplicate hook constructed its checkpoint store but before that hook wins
/// the SQLite writer lock.  That is an acknowledgement, not a failed marker
/// write: registering another marker would be stale and charging the
/// finalizer retry budget could quarantine the successful writer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogTerminalAttemptRegistration {
    Authorized,
    TerminalReceiptAlreadyComplete,
}

impl CaptureCatalogTerminalAttemptFence {
    /// Recovery derives the native identity from the catalog capability, not
    /// operator input, an artifact alias, or a checkpoint payload sidecar.
    pub(crate) fn recovery_checkpoint_binding(
        &self,
    ) -> Option<(&CaptureScope, &str, String, &str)> {
        if self.checkpoint_write != CheckpointWrite::Committed || self.source_digest.is_none() {
            return None;
        }
        Some((
            &self.scope,
            &self.session.session_id,
            super::checkpoint::checkpoint_id_for_capture_action(
                self.action.event_id,
                self.checkpoint_write,
            ),
            &self.marker_generation,
        ))
    }

    pub(crate) fn matches_checkpoint_attempt(
        &self,
        scope: Option<&CaptureScope>,
        session_id: &str,
        checkpoint_id: &str,
        marker_generation: &str,
    ) -> bool {
        self.checkpoint_write == CheckpointWrite::Committed
            && scope == Some(&self.scope)
            && session_id == self.session.session_id
            && checkpoint_id
                == super::checkpoint::checkpoint_id_for_capture_action(
                    self.action.event_id,
                    self.checkpoint_write,
                )
            && marker_generation == self.marker_generation
    }

    fn for_bound_attempt(
        request: &CaptureCatalogFinalizeRequest,
        marker_generation: String,
        source_digest: Option<String>,
    ) -> Self {
        Self {
            scope: request.scope.clone(),
            session: request.session.clone(),
            action: request.action.clone(),
            checkpoint_write: request.checkpoint_write,
            marker_generation,
            source_digest,
            manual_artifact: None,
        }
    }
}

/// Outcome of persisting or checking a terminal finalization transition.
/// Every variant is content-free and may be emitted in a hook span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogFinalizeResult {
    /// The terminal receipt remains replayable; the checkpoint must not be
    /// published as a terminal session yet.
    Pending {
        attempts: u8,
        stage: FinalizePendingStage,
    },
    /// A durable checkpoint was observed under the exact marker/source
    /// fences; use this proof with `CaptureCatalogCompleteRequest`.
    ReadyToComplete { proof: CaptureCatalogFinalizeProof },
    /// Retry budget or a stale fence forced a repair-required quarantine.
    Quarantined { reason: FinalizeQuarantineReason },
    /// The receipt had already completed before this finalizer operation.
    AlreadyComplete,
    /// The store changed nothing because a scope/revision/action fence lost.
    ConflictUnchanged { conflict: CaptureCatalogConflict },
}

/// The one durable writer attempt a pending terminal receipt permits.
///
/// This is intentionally returned by the catalog rather than reconstructed
/// from a stale `apply` result: duplicate native deliveries may both reserve
/// before either one binds its finalizer.  The catalog's write transaction
/// elects one marker/source pair and every later delivery adopts that pair.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogTerminalAttempt {
    /// This delivery owns the persisted marker/source fence. It may register
    /// or resume the checkpoint attempt. `attempts` is the persisted,
    /// content-free finalizer retry count for diagnostics.
    Bound {
        marker_generation: String,
        source_digest: Option<String>,
        attempts: u8,
        registration_fence: Box<CaptureCatalogTerminalAttemptFence>,
    },
    /// A different snapshot reached an already-elected native terminal
    /// attempt. It is an observer, not a stale writer: it must leave the
    /// receipt and finalizer untouched while the elected source either
    /// becomes durable or is retried with its original fence.
    Adopted {
        marker_generation: String,
        source_digest: Option<String>,
    },
    /// The deterministic checkpoint is already durable for the persisted
    /// receipt, but its ordinary marker was retired before receipt completion.
    /// This is deliberately not `Bound`: a changed-source replay must prove
    /// and complete the original receipt, never register its own payload.
    DurableReplay,
    AlreadyComplete,
    Quarantined {
        reason: FinalizeQuarantineReason,
    },
    ConflictUnchanged {
        conflict: CaptureCatalogConflict,
    },
}

/// Opaque, catalog-owned identity for doctor recovery of a terminal receipt.
/// It contains only durable identifiers and cannot be forged by a provider or
/// hook adapter. Doctor may inspect the stable checkpoint ID for its existing
/// checkpoint-store classification, then hand this token back for recovery.
#[derive(Clone, Debug)]
pub(crate) struct CaptureCatalogFinalizerRecovery {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    action: CaptureCatalogAction,
    checkpoint_id: String,
    budget_exhausted: bool,
    superseded: bool,
    quarantined: bool,
    artifact_present: bool,
    artifact_pending: bool,
    artifact_manual_attempted: bool,
}

impl CaptureCatalogFinalizerRecovery {
    pub(crate) fn scope(&self) -> &CaptureScope {
        &self.scope
    }

    pub(crate) fn checkpoint_id(&self) -> &str {
        &self.checkpoint_id
    }

    pub(crate) fn superseded(&self) -> bool {
        self.superseded
    }

    pub(crate) fn quarantined(&self) -> bool {
        self.quarantined
    }

    pub(crate) fn budget_exhausted(&self) -> bool {
        self.budget_exhausted
    }

    pub(crate) fn manual_only(&self) -> bool {
        self.superseded || self.quarantined
    }

    pub(crate) fn artifact_present(&self) -> bool {
        self.artifact_present
    }

    pub(crate) fn artifact_pending(&self) -> bool {
        self.artifact_pending
    }

    pub(crate) fn artifact_manual_attempted(&self) -> bool {
        self.artifact_manual_attempted
    }
}

/// Cold diagnostics, not replay authority. Bad rows do not erase the valid
/// portion of a report; bounds/skipped keys are explicit content-free notes.
pub(crate) struct CaptureCatalogFinalizerScan {
    pub(crate) recoveries: Vec<CaptureCatalogFinalizerRecovery>,
    pub(crate) malformed_rows: usize,
    pub(crate) skipped_invalid_keys: bool,
    pub(crate) truncated: bool,
}

/// Result of doctor replaying a pending finalizer after its existing
/// checkpoint scan has independently established durable checkpoint evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogFinalizerRecoveryResult {
    Completed,
    AlreadyComplete,
    MissingDurableCheckpoint,
    Pending,
    Quarantined,
    ConflictUnchanged,
}

/// State of the receipt after a successful catalog apply.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureReceiptDisposition {
    /// The provider supplied no delivery identity, so this action cannot be
    /// safely deduplicated by the catalog.
    NotTracked,
    /// A catalog reservation is durable but the checkpoint must still be
    /// committed (or replayed after a fault). For a terminal action, the
    /// terminal state itself remains deferred until completion.
    Pending,
    /// The action had no checkpoint side effect, so its receipt completed in
    /// the same catalog transaction.
    Complete,
}

/// A normal conflict is an expected, non-mutating result rather than an
/// infrastructure error. The coordinator can surface it deterministically.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogConflict {
    /// The catalog's durable state no longer matches the reducer precondition.
    ExpectedState,
    /// A stable provider session is already represented by a different local
    /// `session_id` or verified working directory.
    SessionIdentity,
    /// The same opaque delivery receipt was attached to a different action.
    ActionMismatch,
    /// A completion attempted to finish an evicted or never-created receipt.
    MissingReceipt,
    /// A strict terminal completion did not carry the durable finalizer proof
    /// for the receipt's marker/source generation.
    FinalizerFence,
    /// A conditional write lost a race after the transaction's read phase.
    ConditionalWrite,
}

/// Result of applying a lifecycle catalog action.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogApplyResult {
    /// State changed exactly once. The caller must run the returned checkpoint
    /// action, if any, and call `complete` for a pending receipt only after
    /// that action succeeds.
    Applied {
        state: DurableCaptureState,
        checkpoint: CheckpointWrite,
        receipt: CaptureReceiptDisposition,
    },
    /// The state mutation from this delivery was already written, but its
    /// checkpoint did not complete. Resume the checkpoint; do not reduce or
    /// mutate lifecycle state again.
    ResumePending {
        state: DurableCaptureState,
        checkpoint: CheckpointWrite,
        /// `true` only when a terminal receipt either predates finalizer
        /// evidence or still has the provisional no-repository marker. Its
        /// first real checkpoint may bind a marker/source under the receipt's
        /// reserved revision. A normal pending terminal receipt must retain
        /// its existing marker fence and therefore reports `false`.
        terminal_finalizer_needs_binding: bool,
        /// The already-bound terminal writer generation, if any. This is an
        /// opaque durable attempt identity, not caller-selected input: a
        /// replay must construct its checkpoint store with this generation
        /// instead of allocating a fresh marker that could take over a live
        /// attempt.
        terminal_marker_generation: Option<String>,
        /// An ID-less SessionEnd may atomically adopt an earlier local pending
        /// terminal receipt. The live factory replaces its pre-applied request
        /// with this identity so checkpoint/finalizer completion addresses the
        /// same receipt rather than creating a second terminal generation.
        adopted_action: Option<CaptureCatalogAction>,
    },
    /// This receipt and its checkpoint are both durable; no action remains.
    AlreadyApplied,
    /// The store changed nothing because the current action/precondition does
    /// not agree with durable catalog facts.
    ConflictUnchanged { conflict: CaptureCatalogConflict },
}

/// Result of marking a pending receipt complete.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogCompleteResult {
    Completed,
    AlreadyComplete,
    ConflictUnchanged { conflict: CaptureCatalogConflict },
}

/// Sanitized catalog failures.  Variants intentionally omit caller text and
/// provider identifiers so an error path cannot echo an unredacted payload.
#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum CaptureCatalogError {
    #[error("capture catalog request is invalid")]
    InvalidRequest,
    #[error("capture catalog receipt key is invalid")]
    InvalidReceiptKey,
    #[error(
        "capture catalog receipt ledger is malformed; inspect it with `libra agent doctor` before retrying"
    )]
    MalformedReceiptLedger,
    #[error(
        "capture catalog receipt capacity is exhausted while prior checkpoints remain pending; inspect with `libra agent doctor` before retrying"
    )]
    ReceiptCapacityExhausted,
    #[error(
        "capture catalog scope or workspace lease is not writable; rerun from the current workspace before retrying"
    )]
    ScopeRejected,
    #[error(
        "capture catalog workspace lease is not live; rerun from the current workspace before retrying"
    )]
    WorkspaceLeaseRejected,
    #[error("capture catalog session is tombstoned and cannot be recreated by a stale hook")]
    Tombstoned,
    #[error("capture catalog import session ownership no longer matches the authorized source")]
    ImportSessionConflict,
    #[error("capture catalog schema is unavailable; run `libra init` before retrying")]
    SchemaUnavailable,
    #[error("capture catalog transaction could not start")]
    TransactionStart,
    #[error("capture catalog deadline elapsed before a durable mutation could commit")]
    DeadlineExceeded,
    #[error("capture catalog transaction could not commit; no capture mutation was acknowledged")]
    CommitFailed,
    #[error("capture catalog database operation failed")]
    Database,
    #[allow(dead_code)] // Test-only in-memory port poison path.
    #[error("capture catalog fake store state is unavailable")]
    FakeStoreUnavailable,
}

/// The narrow port consumed by a future coordinator. It is deliberately
/// independent of provider adapters and checkpoint/ref topology.
#[async_trait]
pub(crate) trait CaptureCatalogPort: Send + Sync {
    async fn apply(
        &self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogApplyResult, CaptureCatalogError>;

    async fn complete(
        &self,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError>;

    /// Persist/check a bounded terminal finalizer attempt. Implementations
    /// must verify scope and the receipt's reserved revision before any
    /// mutation, so a late writer cannot take over a newer session.
    async fn finalize(
        &self,
        request: &CaptureCatalogFinalizeRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError>;

    /// Atomically elect or adopt the single marker/source pair for a pending
    /// terminal checkpoint attempt.  Unlike a plain `finalize(NotStarted)`,
    /// this operation never lets a duplicate delivery replace a concrete
    /// receipt marker with its process-local generation.
    async fn claim_terminal_attempt(
        &self,
        request: &CaptureCatalogFinalizeRequest,
        require_source_match: bool,
    ) -> Result<CaptureCatalogTerminalAttempt, CaptureCatalogError> {
        let _ = (request, require_source_match);
        Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
            conflict: CaptureCatalogConflict::MissingReceipt,
        })
    }

    /// Reissue the persisted strict completion proof after the checkpoint
    /// facade has independently reported a durable replay.  This deliberately
    /// accepts no caller-provided marker or source: a post-crash writer must
    /// use the original receipt fence instead of taking it over with a fresh
    /// inflight marker.
    async fn prove_durable_replay(
        &self,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError>;

    /// Recheck the deterministic committed checkpoint and finish a pending
    /// terminal receipt in one catalog writer transaction.  This is used only
    /// after a changed-source delivery observes that the elected marker was
    /// retired after a durable write.  Implementations must not accept a
    /// caller-provided source or marker for this path.
    async fn complete_durable_replay(
        &self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
        let _ = request;
        Err(CaptureCatalogError::InvalidRequest)
    }

    /// Update only the closed-set retry diagnostics under the same scope and
    /// session fence as lifecycle state. `false` means a concurrent mutation
    /// won; callers must treat diagnostics as best-effort and never retry by
    /// overwriting the newer row.
    async fn update_diagnostic(
        &self,
        request: &CaptureCatalogApplyRequest,
        diagnostic: CaptureCatalogDiagnostic,
    ) -> Result<bool, CaptureCatalogError>;
}

/// SeaORM-backed implementation using the existing `agent_session` table.
#[derive(Clone)]
pub(crate) struct CaptureCatalogStore {
    conn: DatabaseConnection,
    // This paired execution budget remains invocation-local rather than
    // catalog metadata. Its monotonic half bounds cancellable work, while its
    // immutable SQLite half authorizes the final durable commit without
    // re-anchoring an expired invocation to a fresh wall clock.
    execution_deadline: Option<CaptureCommitDeadline>,
}

/// An identity read from the current catalog, not a provider/source locator.
/// Private fields prevent projection/replay callers from manufacturing it.
/// Never implement Debug or Serialize: these are sensitive catalog values.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct PendingSessionContext {
    session: CaptureCatalogSession,
    scope: CaptureScope,
    incarnation: Option<String>,
    revision: i64,
}

impl PendingSessionContext {
    pub(crate) fn agent_kind(&self) -> &str {
        &self.session.agent_kind
    }

    pub(crate) fn session_id(&self) -> &str {
        &self.session.session_id
    }

    pub(crate) fn provider_session_id(&self) -> &str {
        &self.session.provider_session_id
    }

    pub(crate) fn working_dir(&self) -> &str {
        &self.session.working_dir
    }

    pub(crate) fn scope(&self) -> &CaptureScope {
        &self.scope
    }

    pub(crate) fn incarnation(&self) -> Option<&str> {
        self.incarnation.as_deref()
    }
}

/// Indexed PK lookup. Bounds are applied by SQLite before sensitive values
/// are hydrated; the returned capability does not grant any filesystem read.
pub(crate) async fn resolve_pending_session_context(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
    session_id: &str,
) -> Result<PendingSessionContext, CaptureCatalogError> {
    validate_catalog_text(session_id, MAX_SESSION_ID_BYTES)?;
    let repo = crate::internal::workspace::RepoIdentity::resolve(txn)
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    if repo.as_str() != scope.repo_id {
        return Err(CaptureCatalogError::ScopeRejected);
    }
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT agent_kind, provider_session_id, working_dir, metadata_json,
                sync_revision FROM agent_session
         WHERE session_id = ? AND scope_state = 'scoped'
           AND repo_id = ? AND worktree_id = ?
           AND workspace_id IS ? AND workspace_fence IS ?
           AND length(CAST(agent_kind AS BLOB)) <= 96
           AND length(CAST(provider_session_id AS BLOB)) <= 1024
           AND length(CAST(working_dir AS BLOB)) <= 4096
           AND length(CAST(metadata_json AS BLOB)) <= 1048576 LIMIT 1",
            [
                session_id.into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    let session = CaptureCatalogSession::new(
        session_id,
        row.try_get_by::<String, _>("agent_kind")
            .map_err(|_| CaptureCatalogError::Database)?,
        row.try_get_by::<String, _>("provider_session_id")
            .map_err(|_| CaptureCatalogError::Database)?,
        row.try_get_by::<String, _>("working_dir")
            .map_err(|_| CaptureCatalogError::Database)?,
    )?;
    verify_mutable_scope(txn, scope, &session).await?;
    reject_tombstone(txn, &session).await?;
    let metadata: String = row
        .try_get_by("metadata_json")
        .map_err(|_| CaptureCatalogError::Database)?;
    use crate::internal::ai::observed_agents::coverage::CanonValue;
    let metadata = parse_canon_value(metadata.as_bytes())
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)?;
    let CanonValue::Object(object) = metadata else {
        return Err(CaptureCatalogError::MalformedReceiptLedger);
    };
    let incarnation = match object.get("capture_incarnation") {
        None | Some(CanonValue::Null) => None,
        Some(CanonValue::Str(value))
            if value.len() == 32
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            Some(value.clone())
        }
        _ => return Err(CaptureCatalogError::MalformedReceiptLedger),
    };
    let revision = row
        .try_get_by::<i64, _>("sync_revision")
        .map_err(|_| CaptureCatalogError::Database)?;
    if revision <= 0 {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    Ok(PendingSessionContext {
        session,
        scope: scope.clone(),
        incarnation,
        revision,
    })
}

impl CaptureCatalogStore {
    /// Re-elect the exact persisted terminal attempt for one authenticated
    /// pending artifact. The artifact/alias remain discovery inputs only;
    /// current catalog, tombstone, receipt and coverage fences are checked
    /// under the same writer lock which advances the original retry budget.
    pub(crate) async fn claim_pending_artifact_attempt(
        &self,
        recovery: &CaptureCatalogFinalizerRecovery,
        verified: &super::pending::VerifiedPendingPayload,
        identity: &super::pending_identity::PreparedPendingAlias,
        now_millis: i64,
        deadline: CaptureCommitDeadline,
    ) -> Result<CaptureCatalogTerminalAttempt, CaptureCatalogError> {
        let header = verified.header();
        if identity.alias() != header.binding.session_id
            || identity.context().scope() != &recovery.scope
            || identity.context().session_id() != recovery.session.session_id
            || verified.context() != identity.context()
            || verified.binding() != &header.binding
            || header.binding.scope != recovery.scope
            || header.binding.checkpoint_id != recovery.checkpoint_id
            || header.binding.event_id != recovery.action.event_id.to_string()
            || header.binding.action_key != recovery.action.action_key()
            || header.binding.receipt_key != recovery.action.completion_receipt_storage_key()
            || header.manual_attempted
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }

        let txn = begin_catalog_write_transaction(&self.conn, Some(deadline)).await?;
        let result = async {
            if super::pending::current_header_namespace(&txn, header)
                .await
                .map_err(classify_pending_validation_error)?
                != crate::internal::metadata::MetadataScope::AgentCapturePending
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            verify_mutable_scope(&txn, &recovery.scope, &recovery.session).await?;
            reject_tombstone(&txn, &recovery.session).await?;
            if resolve_pending_session_context(&txn, &recovery.scope, &recovery.session.session_id)
                .await?
                != *identity.context()
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            if super::pending_identity::lookup(&txn, &recovery.scope.repo_id, identity.alias())
                .await
                .map_err(classify_pending_validation_error)?
                .is_none_or(|record| !identity.matches_authenticated_record(&record))
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }

            let existing = read_session(&txn, &recovery.session)
                .await?
                .ok_or(CaptureCatalogError::InvalidRequest)?;
            if !existing.matches_scope(&recovery.scope)
                || existing.session_id != recovery.session.session_id
                || existing.working_dir != recovery.session.working_dir
                || existing.state.phase == CapturePhase::Quarantined
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
            let receipt = ledger
                .find(&recovery.action.completion_receipt_storage_key())
                .ok_or(CaptureCatalogError::InvalidRequest)?;
            if !receipt.matches_action(&recovery.action)
                || !receipt.is_deferred_terminal()
                || receipt.status != StoredReceiptStatus::Pending
                || !receipt.can_resume_from(Some(existing.durable_state()))
                || receipt.intent.checkpoint.to_checkpoint_write() == CheckpointWrite::None
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            let finalizer = receipt
                .finalizer
                .as_ref()
                .ok_or(CaptureCatalogError::InvalidRequest)?;
            if finalizer.status != StoredFinalizeStatus::Pending
                || finalizer.is_unbound_snapshot()
                || finalizer.marker_generation != header.binding.marker_generation
                || finalizer.source_digest.as_deref()
                    != Some(header.binding.source_commitment.as_str())
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }

            let request = CaptureCatalogFinalizeRequest::new(
                recovery.scope.clone(),
                recovery.session.clone(),
                recovery.action.clone(),
                receipt.intent.checkpoint.to_checkpoint_write(),
                finalizer.to_policy()?,
                finalizer.marker_generation.clone(),
                finalizer.source_digest.clone(),
                now_millis,
                FinalizeCheckpointProgress::NotStarted,
            )?;
            let fence = CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                &request,
                finalizer.marker_generation.clone(),
                finalizer.source_digest.clone(),
            );
            let expected_binding = pending_artifact_binding(
                &txn,
                &fence,
                identity.alias(),
                verified.coverage().parent_commit.clone(),
            )
            .await?;
            if expected_binding != header.binding {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            crate::internal::ai::coverage_gate::verify_reserved_live_claims_with_conn(
                &txn,
                &recovery.scope,
                identity.context().session_id(),
                &verified.coverage().owner,
                &verified.coverage().claims,
            )
            .await
            .map_err(classify_pending_validation_error)?;
            self.claim_terminal_attempt_inner(&txn, &request, true)
                .await
        }
        .await;
        finish_catalog_transaction(txn, result, &recovery.scope, Some(deadline)).await
    }

    /// Consume an expired artifact's one explicit operator attempt. The MAC
    /// capability is constructed only by the payload loader; the header is
    /// discovery evidence, never authority to bypass current catalog fences.
    pub(crate) async fn claim_manual_pending_artifact(
        &self,
        verified: &super::pending::VerifiedPendingPayload,
        identity: &super::pending_identity::PreparedPendingAlias,
        header: &super::pending::PendingHeader,
        now_millis: i64,
        deadline: CaptureCommitDeadline,
    ) -> Result<CaptureCatalogTerminalAttemptFence, CaptureCatalogError> {
        if verified.header() != header
            || verified.binding() != &header.binding
            || verified.context() != identity.context()
            || verified.coverage().session_id != identity.context().session_id()
            || verified.coverage().checkpoint_id != header.binding.checkpoint_id
            || verified.coverage().parent_commit != header.binding.parent_commit
            || verified.coverage().capture_scope.as_ref() != Some(&header.binding.scope)
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let scope = identity.context().scope();
        let txn = begin_catalog_write_transaction(&self.conn, Some(deadline)).await?;
        let result = async {
            if super::pending::current_header_namespace(&txn, header).await
                .map_err(classify_pending_validation_error)?
                != crate::internal::metadata::MetadataScope::AgentCaptureQuarantine
                || super::pending_identity::lookup(&txn, &scope.repo_id, identity.alias()).await
                    .map_err(classify_pending_validation_error)?.is_none()
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            // Reusing this publication verifier cannot mint: the existing
            // alias was required above under this same SQLite writer lock.
            identity.publish_for_artifact(&txn, &header.binding.checkpoint_id).await
                .map_err(classify_pending_validation_error)?;
            let session = &identity.context().session;
            let existing = read_session(&txn, session).await?
                .ok_or(CaptureCatalogError::InvalidRequest)?;
            let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
            let receipt = ledger.find(&header.binding.receipt_key)
                .ok_or(CaptureCatalogError::InvalidRequest)?;
            let finalizer = receipt.finalizer.as_ref()
                .ok_or(CaptureCatalogError::InvalidRequest)?;
            if existing.state.phase == CapturePhase::Quarantined
                || finalizer.status != StoredFinalizeStatus::Pending
                || finalizer.mode != StoredFinalizeMode::Deferrable
                || finalizer.is_unbound_snapshot()
                || !finalizer.budget_exhausted_at(now_millis)
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            let request = CaptureCatalogFinalizeRequest::new(
                scope.clone(), session.clone(), action_from_stored_receipt(receipt)?,
                CheckpointWrite::Committed, finalizer.to_policy()?,
                finalizer.marker_generation.clone(), finalizer.source_digest.clone(),
                now_millis, FinalizeCheckpointProgress::NotStarted,
            )?;
            let mut fence = CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                &request, finalizer.marker_generation.clone(), finalizer.source_digest.clone(),
            );
            // The one-shot claim precedes proof validation ONLY inside this
            // transaction; any fence/alias/coverage/audit failure rolls it back.
            let consumed = super::pending::claim_manual_attempt(&txn, header).await
                .map_err(classify_pending_validation_error)?;
            let alias_record = super::pending_identity::lookup(&txn, &scope.repo_id, identity.alias())
                .await.map_err(classify_pending_validation_error)?
                .ok_or(CaptureCatalogError::InvalidRequest)?
                .encode().map_err(|_| CaptureCatalogError::InvalidRequest)?;
            fence.manual_artifact = Some(ManualArtifactRegistrationAuthority {
                header: consumed, context: identity.context().clone(), alias_record,
                registration_consumed: Arc::new(AtomicBool::new(false)),
            });
            if pending_artifact_binding(&txn, &fence, identity.alias(),
                verified.coverage().parent_commit.clone()).await? != header.binding
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            crate::internal::ai::coverage_gate::verify_reserved_live_claims_with_conn(
                &txn, scope, identity.context().session_id(), &verified.coverage().owner,
                &verified.coverage().claims,
            ).await.map_err(classify_pending_validation_error)?;
            txn.execute_raw(Statement::from_sql_and_values(txn.get_database_backend(),
                "INSERT INTO agent_audit_log(audit_id,timestamp,action,checkpoint_id,scope,justification,granted)
                 VALUES(?,?,'repair_pending_capture',?,'session',?,1)",
                [Uuid::new_v4().to_string().into(), chrono::Utc::now().to_rfc3339().into(),
                 header.binding.checkpoint_id.clone().into(),
                 "explicit doctor repair of exhausted authenticated artifact".into()],
            )).await.map_err(|_| CaptureCatalogError::Database)?;
            Ok(fence)
        }.await;
        finish_catalog_transaction(txn, result, scope, Some(deadline)).await
    }

    /// Select a retained alias under the writer lock, then authenticate it
    /// outside the transaction. ACF-10 publishes the prepared association
    /// with the artifact after repeating the terminal/source/receipt fences.
    pub(crate) async fn prepare_pending_session_alias(
        &self,
        fence: &CaptureCatalogTerminalAttemptFence,
        storage: &Path,
        root: &Path,
        deadline: CaptureCommitDeadline,
    ) -> Result<
        crate::internal::ai::capture::pending_identity::PreparedPendingAlias,
        CaptureCatalogError,
    > {
        // Manual authority is reserved for one marker registration, never
        // artifact publication or alias preparation. Reject before consuming it.
        if fence.manual_artifact.is_some() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let txn = begin_catalog_write_transaction(&self.conn, Some(deadline)).await?;
        let result = async {
            let checkpoint =
                crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                    fence.action.event_id,
                    fence.checkpoint_write,
                );
            if fence.checkpoint_write != CheckpointWrite::Committed
                || verify_terminal_attempt_registration(
                    &txn,
                    fence,
                    &checkpoint,
                    &fence.marker_generation,
                )
                .await?
                    != CaptureCatalogTerminalAttemptRegistration::Authorized
            {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            let context =
                resolve_pending_session_context(&txn, &fence.scope, &fence.session.session_id)
                    .await?;
            let existing =
                crate::internal::ai::capture::pending_identity::retained_alias(&txn, &context)
                    .await
                    .map_err(classify_pending_validation_error)?;
            Ok((context, existing))
        }
        .await;
        let (context, existing) =
            finish_catalog_transaction(txn, result, &fence.scope, Some(deadline)).await?;
        crate::internal::ai::capture::pending_identity::PendingSessionAlias::prepare(
            &self.conn,
            &context,
            existing,
            storage,
            root,
            deadline.monotonic(),
        )
        .await
        .map_err(classify_pending_validation_error)
    }

    /// Bind an authenticated complete SessionEnd snapshot to its elected
    /// receipt. Hashing happens outside the SQLite writer lock; the exact
    /// immutable binding is revalidated before the artifact transaction.
    pub(crate) async fn persist_pending_artifact(
        &self,
        fence: &CaptureCatalogTerminalAttemptFence,
        payload: &crate::internal::ai::capture::checkpoint::CheckpointRedactedPayload,
        coverage: &crate::internal::ai::coverage_gate::LiveClaimCommitPlan,
        storage: &Path,
        root: &Path,
        deadline: CaptureCommitDeadline,
    ) -> Result<(), CaptureCatalogError> {
        if fence.manual_artifact.is_some() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let identity = self
            .prepare_pending_session_alias(fence, storage, root, deadline)
            .await?;
        let txn = begin_catalog_write_transaction(&self.conn, Some(deadline)).await?;
        let binding = pending_artifact_binding(
            &txn,
            fence,
            identity.alias(),
            coverage.parent_commit.clone(),
        )
        .await;
        let binding =
            finish_catalog_transaction(txn, binding, &fence.scope, Some(deadline)).await?;
        let artifact = crate::internal::ai::capture::pending::SealedPendingArtifact::seal(
            &self.conn,
            storage,
            root,
            crate::internal::ai::capture::pending::PendingSealRequest {
                binding,
                identity,
                payload,
                coverage,
            },
            deadline.monotonic(),
        )
        .await
        .map_err(classify_pending_validation_error)?;
        let txn = begin_catalog_write_transaction(&self.conn, Some(deadline)).await?;
        let result = async {
            let current = pending_artifact_binding(
                &txn,
                fence,
                &artifact.binding().session_id,
                coverage.parent_commit.clone(),
            )
            .await?;
            if artifact.binding() != &current {
                return Err(CaptureCatalogError::InvalidRequest);
            }
            artifact
                .persist(&txn, deadline.monotonic())
                .await
                .map_err(classify_pending_validation_error)
        }
        .await;
        finish_catalog_transaction(txn, result, &fence.scope, Some(deadline)).await
    }

    pub(crate) fn new(conn: DatabaseConnection) -> Self {
        Self {
            conn,
            execution_deadline: None,
        }
    }

    /// Construct the concrete live catalog port with the hook invocation's
    /// paired deadline. The deadline stays in memory with the
    /// reservation/coordinator and is never copied into requests or receipts.
    pub(crate) fn new_until(conn: DatabaseConnection, deadline: CaptureCommitDeadline) -> Self {
        Self {
            conn,
            execution_deadline: Some(deadline),
        }
    }

    /// Bounded cold diagnostics in one read-only snapshot. Superseded and
    /// quarantined receipts stay visible, but never acquire repair authority.
    /// The cursor is a capped raw TEXT key, not a rowid or source locator.
    pub(crate) async fn pending_finalizer_recoveries_for_doctor(
        &self,
        now_millis: i64,
    ) -> Result<CaptureCatalogFinalizerScan, CaptureCatalogError> {
        let txn = self
            .conn
            .begin()
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
        let mut scan = CaptureCatalogFinalizerScan {
            recoveries: Vec::new(),
            malformed_rows: 0,
            skipped_invalid_keys: false,
            truncated: false,
        };
        let mut cursor: Option<Vec<u8>> = None;
        let mut scanned = 0usize;
        'pages: loop {
            let remaining = MAX_DOCTOR_FINALIZER_SESSIONS.saturating_sub(scanned);
            let limit = remaining.clamp(1, DOCTOR_FINALIZER_PAGE_SIZE);
            let rows = txn
                .query_all_raw(doctor_finalizer_scan_statement(
                    txn.get_database_backend(),
                    cursor.as_deref(),
                    scan.skipped_invalid_keys,
                    limit,
                ))
                .await
                .map_err(|_| CaptureCatalogError::Database)?;
            if rows.is_empty() {
                break;
            }
            if remaining == 0 {
                scan.truncated = true;
                break;
            }
            let page_full = rows.len() == limit;
            let previously_skipped = scan.skipped_invalid_keys;
            for row in rows {
                scanned += 1;
                let key = row
                    .try_get_by::<Option<Vec<u8>>, _>("session_id")
                    .map_err(|_| CaptureCatalogError::Database)?;
                if let Some(key) = key {
                    // Even invalid UTF-8 TEXT keys can be paged without
                    // decoding or logging them: CAST(? AS TEXT) retains bytes.
                    cursor = Some(key);
                } else {
                    // We cannot hydrate an oversized/non-TEXT cursor. Drop
                    // only malformed keys from subsequent indexed pages and
                    // report the incomplete diagnostic coverage explicitly.
                    scan.skipped_invalid_keys = true;
                    scan.malformed_rows += 1;
                    continue;
                }
                let Ok((scope, session, state, ledger)) = decode_doctor_finalizer_row(&row) else {
                    scan.malformed_rows += 1;
                    continue;
                };
                for receipt in &ledger.entries {
                    let Some(finalizer) = &receipt.finalizer else {
                        continue;
                    };
                    if !receipt.is_deferred_terminal()
                        || receipt.status != StoredReceiptStatus::Pending
                        || receipt.intent.checkpoint.to_checkpoint_write() == CheckpointWrite::None
                    {
                        continue;
                    }
                    if scan.recoveries.len() == MAX_DOCTOR_FINALIZER_RECOVERIES {
                        scan.truncated = true;
                        break 'pages;
                    }
                    let action = action_from_stored_receipt(receipt)?;
                    let checkpoint_id = super::checkpoint::checkpoint_id_for_capture_action(
                        action.event_id,
                        receipt.intent.checkpoint.to_checkpoint_write(),
                    );
                    let artifact_present = super::pending::has_header_for_checkpoint(
                        &txn,
                        &scope.repo_id,
                        &checkpoint_id,
                    )
                    .await
                    .map_err(|_| CaptureCatalogError::Database)?;
                    let artifact_pending = super::pending::has_pending_header_for_checkpoint(
                        &txn,
                        &scope.repo_id,
                        &checkpoint_id,
                    )
                    .await
                    .map_err(|_| CaptureCatalogError::Database)?;
                    let artifact_manual_attempted =
                        super::pending::has_manual_attempted_header_for_checkpoint(
                            &txn,
                            &scope.repo_id,
                            &checkpoint_id,
                        )
                        .await
                        .map_err(|_| CaptureCatalogError::Database)?;
                    scan.recoveries.push(CaptureCatalogFinalizerRecovery {
                        scope: scope.clone(),
                        session: session.clone(),
                        action,
                        checkpoint_id,
                        budget_exhausted: finalizer.budget_exhausted_at(now_millis),
                        superseded: !receipt.can_resume_from(Some(state)),
                        quarantined: state.phase == CapturePhase::Quarantined
                            || finalizer.status == StoredFinalizeStatus::Quarantined,
                        artifact_present,
                        artifact_pending,
                        artifact_manual_attempted,
                    });
                }
            }
            // A page that introduced the malformed-key filter needs a fresh
            // filtered query even when short: otherwise later valid keys
            // could disappear behind a run of oversized keys.
            if !page_full && previously_skipped == scan.skipped_invalid_keys {
                break;
            }
        }
        txn.commit()
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
        Ok(scan)
    }

    /// Resolve one indexed artifact candidate directly through its authenticated
    /// alias and current receipt. The detached worker must never sweep the
    /// entire session catalog: unrelated session volume cannot hide queued
    /// work behind a diagnostic scan cap.
    pub(crate) async fn pending_finalizer_recovery_for_header(
        &self,
        header: &super::pending::PendingHeader,
        storage: &Path,
        root: &Path,
        now_millis: i64,
        deadline: std::time::Instant,
    ) -> Result<Option<CaptureCatalogFinalizerRecovery>, CaptureCatalogError> {
        let txn = self
            .conn
            .begin()
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
        let result: Result<Option<CaptureCatalogFinalizerRecovery>, CaptureCatalogError> = async {
            let binding = &header.binding;
            let current_scope = CaptureScope::resolve(&txn, root)
                .await
                .map_err(|_| CaptureCatalogError::Database)?;
            if current_scope != binding.scope {
                return Ok(None);
            }
            let Some(alias) = super::pending_identity::lookup_for_worker(
                &txn,
                &binding.scope.repo_id,
                &binding.session_id,
            )
            .await
            .map_err(|error| match error {
                super::pending_identity::WorkerIdentityError::Retryable => {
                    CaptureCatalogError::Database
                }
                super::pending_identity::WorkerIdentityError::Invalid => {
                    CaptureCatalogError::InvalidRequest
                }
            })?
            else {
                return Ok(None);
            };
            let context = match alias
                .resolve_for_worker(&txn, &binding.scope, storage, root, deadline)
                .await
            {
                Ok(context) => context,
                Err(super::pending_identity::WorkerIdentityError::Invalid) => {
                    return Err(CaptureCatalogError::InvalidRequest);
                }
                Err(super::pending_identity::WorkerIdentityError::Retryable) => {
                    return Err(if std::time::Instant::now() >= deadline {
                        CaptureCatalogError::DeadlineExceeded
                    } else {
                        CaptureCatalogError::Database
                    });
                }
            };
            let session = CaptureCatalogSession::new(
                context.session_id(),
                context.agent_kind(),
                context.provider_session_id(),
                context.working_dir(),
            )?;
            let Some(stored) = read_session(&txn, &session).await? else {
                return Ok(None);
            };
            if !stored.matches_scope(&binding.scope) || stored.session_id != context.session_id() {
                return Ok(None);
            }
            let (_, ledger) = decode_receipt_metadata(&stored.metadata_json)?;
            let Some(receipt) = ledger.find(&binding.receipt_key) else {
                return Ok(None);
            };
            let Some(finalizer) = receipt.finalizer.as_ref() else {
                return Ok(None);
            };
            let action = action_from_stored_receipt(receipt)?;
            let checkpoint_id = super::checkpoint::checkpoint_id_for_capture_action(
                action.event_id,
                receipt.intent.checkpoint.to_checkpoint_write(),
            );
            if !receipt.is_deferred_terminal()
                || receipt.status != StoredReceiptStatus::Pending
                || finalizer.status != StoredFinalizeStatus::Pending
                || receipt.reserved_revision != binding.reserved_revision
                || action.event_id.to_string() != binding.event_id
                || action.action_key() != binding.action_key
                || action.completion_receipt_storage_key() != binding.receipt_key
                || checkpoint_id != binding.checkpoint_id
                || finalizer.marker_generation != binding.marker_generation
                || finalizer.source_digest.as_deref() != Some(binding.source_commitment.as_str())
                || finalizer.deadline_millis != binding.original_deadline_millis
                || finalizer.first_attempt_millis != binding.first_attempt_millis
                || !binding.deferrable
                || (finalizer.mode == StoredFinalizeMode::Deferrable) != binding.deferrable
            {
                return Ok(None);
            }
            let artifact_present = super::pending::has_header_for_checkpoint(
                &txn,
                &binding.scope.repo_id,
                &binding.checkpoint_id,
            )
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
            let artifact_pending = super::pending::has_pending_header_for_checkpoint(
                &txn,
                &binding.scope.repo_id,
                &binding.checkpoint_id,
            )
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
            let artifact_manual_attempted =
                super::pending::has_manual_attempted_header_for_checkpoint(
                    &txn,
                    &binding.scope.repo_id,
                    &checkpoint_id,
                )
                .await
                .map_err(|_| CaptureCatalogError::Database)?;
            Ok(Some(CaptureCatalogFinalizerRecovery {
                scope: binding.scope.clone(),
                session,
                action,
                checkpoint_id,
                budget_exhausted: finalizer.budget_exhausted_at(now_millis),
                superseded: !receipt.can_resume_from(Some(stored.state)),
                quarantined: stored.state.phase == CapturePhase::Quarantined,
                artifact_present,
                artifact_pending,
                artifact_manual_attempted,
            }))
        }
        .await;
        match result {
            Ok(value) => txn
                .commit()
                .await
                .map(|()| value)
                .map_err(|_| CaptureCatalogError::Database),
            Err(error) => {
                let _ = txn.rollback().await;
                Err(error)
            }
        }
    }

    /// Complete a pending terminal receipt only after doctor has found the
    /// checkpoint in its existing durable checkpoint classification. The
    /// catalog re-reads every receipt/fence in one write transaction, so an
    /// old doctor scan cannot complete a newer action.
    pub(crate) async fn recover_pending_finalizer_after_durable_checkpoint(
        &self,
        recovery: &CaptureCatalogFinalizerRecovery,
        now_millis: i64,
    ) -> Result<CaptureCatalogFinalizerRecoveryResult, CaptureCatalogError> {
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self
            .recover_pending_finalizer_after_durable_checkpoint_inner(&txn, recovery, now_millis)
            .await;
        finish_catalog_transaction(txn, result, &recovery.scope, deadline).await
    }

    async fn recover_pending_finalizer_after_durable_checkpoint_inner(
        &self,
        txn: &DatabaseTransaction,
        recovery: &CaptureCatalogFinalizerRecovery,
        now_millis: i64,
    ) -> Result<CaptureCatalogFinalizerRecoveryResult, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &recovery.scope, &recovery.session).await?;
        reject_tombstone(txn, &recovery.session).await?;
        match terminal_durable_checkpoint_status(
            txn,
            &recovery.session,
            recovery.action.event_id,
            CheckpointWrite::Committed,
        )
        .await?
        {
            TerminalDurableCheckpointStatus::Exact => {}
            TerminalDurableCheckpointStatus::Absent
            | TerminalDurableCheckpointStatus::Incompatible => {
                return Ok(CaptureCatalogFinalizerRecoveryResult::MissingDurableCheckpoint);
            }
        }
        let Some(existing) = read_session(txn, &recovery.session).await? else {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        };
        if !existing.matches_scope(&recovery.scope)
            || existing.session_id != recovery.session.session_id
            || existing.working_dir != recovery.session.working_dir
        {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        }
        let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
        let receipt_key = recovery.action.completion_receipt_storage_key();
        let Some(receipt) = ledger.find(&receipt_key) else {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        };
        if !receipt.matches_action(&recovery.action) || !receipt.is_deferred_terminal() {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogFinalizerRecoveryResult::AlreadyComplete);
        }
        let Some(finalizer) = receipt.finalizer.as_ref() else {
            return Ok(CaptureCatalogFinalizerRecoveryResult::Pending);
        };
        if finalizer.status == StoredFinalizeStatus::Quarantined {
            return Ok(CaptureCatalogFinalizerRecoveryResult::Quarantined);
        }
        let finalize = CaptureCatalogFinalizeRequest::new(
            recovery.scope.clone(),
            recovery.session.clone(),
            recovery.action.clone(),
            receipt.intent.checkpoint.to_checkpoint_write(),
            finalizer.to_policy()?,
            finalizer.marker_generation.clone(),
            finalizer.source_digest.clone(),
            now_millis,
            FinalizeCheckpointProgress::Durable,
        )?;
        match self.finalize_inner(txn, &finalize).await? {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => {
                let completion = CaptureCatalogCompleteRequest {
                    scope: recovery.scope.clone(),
                    session: recovery.session.clone(),
                    action: recovery.action.clone(),
                    completion: CaptureCatalogCompletion::Finalizer(proof),
                };
                match self.complete_inner(txn, &completion).await? {
                    CaptureCatalogCompleteResult::Completed => {
                        Ok(CaptureCatalogFinalizerRecoveryResult::Completed)
                    }
                    CaptureCatalogCompleteResult::AlreadyComplete => {
                        Ok(CaptureCatalogFinalizerRecoveryResult::AlreadyComplete)
                    }
                    CaptureCatalogCompleteResult::ConflictUnchanged { .. } => {
                        Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged)
                    }
                }
            }
            CaptureCatalogFinalizeResult::AlreadyComplete => {
                Ok(CaptureCatalogFinalizerRecoveryResult::AlreadyComplete)
            }
            CaptureCatalogFinalizeResult::Pending { .. } => {
                Ok(CaptureCatalogFinalizerRecoveryResult::Pending)
            }
            CaptureCatalogFinalizeResult::Quarantined { .. } => {
                Ok(CaptureCatalogFinalizerRecoveryResult::Quarantined)
            }
            CaptureCatalogFinalizeResult::ConflictUnchanged { .. } => {
                Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged)
            }
        }
    }

    /// Advance an exhausted pending receipt without a durable checkpoint to
    /// its bounded quarantine. This is used only by doctor after its existing
    /// checkpoint scan proved that automatic completion is unsafe. A fresh
    /// pending receipt is left untouched; doctor must not consume retry budget
    /// merely by being inspected.
    pub(crate) async fn quarantine_exhausted_pending_finalizer(
        &self,
        recovery: &CaptureCatalogFinalizerRecovery,
        now_millis: i64,
    ) -> Result<CaptureCatalogFinalizerRecoveryResult, CaptureCatalogError> {
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self
            .quarantine_exhausted_pending_finalizer_inner(&txn, recovery, now_millis)
            .await;
        finish_catalog_transaction(txn, result, &recovery.scope, deadline).await
    }

    async fn quarantine_exhausted_pending_finalizer_inner(
        &self,
        txn: &DatabaseTransaction,
        recovery: &CaptureCatalogFinalizerRecovery,
        now_millis: i64,
    ) -> Result<CaptureCatalogFinalizerRecoveryResult, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &recovery.scope, &recovery.session).await?;
        reject_tombstone(txn, &recovery.session).await?;
        if matches!(
            terminal_durable_checkpoint_status(
                txn,
                &recovery.session,
                recovery.action.event_id,
                CheckpointWrite::Committed,
            )
            .await?,
            TerminalDurableCheckpointStatus::Exact
        ) {
            // Do not quarantine a checkpoint that may have become durable
            // after doctor's ref scan. A fresh scan will take the strict
            // durable-completion path instead.
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        }
        let Some(existing) = read_session(txn, &recovery.session).await? else {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        };
        if !existing.matches_scope(&recovery.scope)
            || existing.session_id != recovery.session.session_id
            || existing.working_dir != recovery.session.working_dir
        {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        }
        let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
        let receipt_key = recovery.action.completion_receipt_storage_key();
        let Some(receipt) = ledger.find(&receipt_key) else {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        };
        if !receipt.matches_action(&recovery.action) || !receipt.is_deferred_terminal() {
            return Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged);
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogFinalizerRecoveryResult::AlreadyComplete);
        }
        let Some(finalizer) = receipt.finalizer.as_ref() else {
            return Ok(CaptureCatalogFinalizerRecoveryResult::Pending);
        };
        if finalizer.status == StoredFinalizeStatus::Quarantined {
            return Ok(CaptureCatalogFinalizerRecoveryResult::Quarantined);
        }
        if !finalizer.budget_exhausted_at(now_millis) {
            return Ok(CaptureCatalogFinalizerRecoveryResult::Pending);
        }
        let finalize = CaptureCatalogFinalizeRequest::new(
            recovery.scope.clone(),
            recovery.session.clone(),
            recovery.action.clone(),
            receipt.intent.checkpoint.to_checkpoint_write(),
            finalizer.to_policy()?,
            finalizer.marker_generation.clone(),
            finalizer.source_digest.clone(),
            now_millis,
            FinalizeCheckpointProgress::Retryable(finalizer.stage.to_stage()),
        )?;
        match self.finalize_inner(txn, &finalize).await? {
            CaptureCatalogFinalizeResult::Quarantined { .. } => {
                Ok(CaptureCatalogFinalizerRecoveryResult::Quarantined)
            }
            CaptureCatalogFinalizeResult::AlreadyComplete => {
                Ok(CaptureCatalogFinalizerRecoveryResult::AlreadyComplete)
            }
            CaptureCatalogFinalizeResult::Pending { .. } => {
                Ok(CaptureCatalogFinalizerRecoveryResult::Pending)
            }
            CaptureCatalogFinalizeResult::ReadyToComplete { .. }
            | CaptureCatalogFinalizeResult::ConflictUnchanged { .. } => {
                Ok(CaptureCatalogFinalizerRecoveryResult::ConflictUnchanged)
            }
        }
    }

    /// Take the SQLite writer barrier used by import acquisition before the
    /// caller checks its tombstone.  `working_dir` is intentionally outside
    /// the compatibility tombstone trigger's `UPDATE OF` list, so this keeps
    /// the historical erased-vs-conflict result without exposing a mutable
    /// `agent_session` statement to the import adapter.
    pub(crate) async fn serialize_import_with_erase(
        txn: &DatabaseTransaction,
        scope: &CaptureScope,
        session: &CaptureCatalogSession,
    ) -> Result<(), CaptureCatalogError> {
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_session SET working_dir = working_dir
             WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?",
            [
                session.agent_kind.clone().into(),
                session.provider_session_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
        Ok(())
    }

    /// Validate an existing source-owned row or create the provisional row
    /// inside the caller's import-lease transaction.  It intentionally does
    /// not open or commit a transaction: the caller must keep this write
    /// atomic with the tombstone check and `agent_import_identity` lease.
    pub(crate) async fn prepare_import_session(
        txn: &DatabaseTransaction,
        request: &CaptureImportSessionPrepareRequest,
    ) -> Result<CaptureImportSessionPrepareResult, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;

        let existing = read_session(txn, &request.session).await?;
        if let Some(existing) = existing {
            if !existing.matches_scope(&request.scope)
                || existing.session_id != request.session.session_id
            {
                return Err(CaptureCatalogError::ImportSessionConflict);
            }
            let fingerprint = import_session_ownership_fingerprint(
                &existing.session_id,
                &request.session.agent_kind,
                &request.session.provider_session_id,
                &existing.working_dir,
                &existing.metadata_json,
            );
            let matches_persisted_ownership = request
                .source
                .matches_persisted_ownership(&existing.metadata_json)?;
            let adopts_live_session = live_session_is_adoptable(&request.source, &existing)?;
            if request.expected_existing_fingerprint.as_deref() != Some(fingerprint.as_str())
                || !session_has_no_unsettled_receipts(&existing)?
                || (!matches_persisted_ownership && !adopts_live_session)
            {
                return Err(CaptureCatalogError::ImportSessionConflict);
            }
            if matches_persisted_ownership
                && request.source.source_identity_schema_version == 2
                && validate_v2_import_session_record(
                    &existing.metadata_json,
                    &existing.redaction_report,
                )
                .is_err()
            {
                return Err(CaptureCatalogError::ImportSessionConflict);
            }
            return Ok(CaptureImportSessionPrepareResult::Existing);
        }

        if request.expected_existing_fingerprint.is_some() {
            return Err(CaptureCatalogError::ImportSessionConflict);
        }
        let (sync_revision, incarnation) = initial_session_revision(txn, &request.session).await?;
        let metadata_json = request.source.metadata_json(true, incarnation)?;
        let redaction_report = import_redaction_report_json(&request.redaction_report)?;
        if request.source.source_identity_schema_version == 2 {
            validate_v2_import_session_record(&metadata_json, &redaction_report)?;
        }
        let inserted = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO agent_session (
                    session_id, agent_kind, provider_session_id, state, working_dir,
                    metadata_json, redaction_report, started_at, last_event_at,
                    stopped_at, schema_version, sync_revision,
                    repo_id, worktree_id, workspace_id, workspace_fence, scope_state
                 ) VALUES (?, ?, ?, 'pending', ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?, 'scoped')",
                [
                    request.session.session_id.clone().into(),
                    request.session.agent_kind.clone().into(),
                    request.session.provider_session_id.clone().into(),
                    request.session.working_dir.clone().into(),
                    metadata_json.into(),
                    redaction_report.into(),
                    request.started_at.into(),
                    request.started_at.into(),
                    Option::<i64>::None.into(),
                    sync_revision.into(),
                    request.scope.repo_id.clone().into(),
                    request.scope.worktree_id.clone().into(),
                    request.scope.workspace_id.clone().into(),
                    request.scope.workspace_fence.into(),
                ],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
        if inserted.rows_affected() != 1 {
            return Err(CaptureCatalogError::Database);
        }
        Ok(CaptureImportSessionPrepareResult::Created)
    }

    /// Replace the mutable ownership projection of one fully committed legacy
    /// import while its identity migration holds SQLite's writer lock.  The
    /// immutable checkpoint/traces objects intentionally remain untouched:
    /// they are historical evidence, not a second mutable catalog authority.
    ///
    /// The caller validates the matching identity and any repair marker in the
    /// same transaction.  This method validates the catalog half *before*
    /// writing, so a stale/raw V1 row can never be partially relabelled as V2.
    pub(crate) async fn migrate_import_session_ownership(
        txn: &DatabaseTransaction,
        scope: &CaptureScope,
        session: &CaptureCatalogSession,
        legacy_source: &CaptureImportSource,
        v2_source: &CaptureImportSource,
    ) -> Result<(), CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, scope, session).await?;
        reject_tombstone(txn, session).await?;
        let existing = read_session(txn, session)
            .await?
            .ok_or(CaptureCatalogError::ImportSessionConflict)?;
        if !existing.matches_scope(scope)
            || existing.session_id != session.session_id
            || !legacy_source.matches_persisted_ownership_exact(&existing.metadata_json)?
        {
            return Err(CaptureCatalogError::ImportSessionConflict);
        }
        let metadata_json =
            sanitize_committed_legacy_import_metadata_for_v2(&existing.metadata_json, v2_source)
                // A malformed legacy metadata object is an ownership-proof failure,
                // not a request-shape detail that callers should expose or retry by
                // patching. Preserve V1 unchanged and present the same conflict class
                // as every other scoped migration mismatch.
                .map_err(|_| CaptureCatalogError::ImportSessionConflict)?;
        let redaction_report = sanitize_v2_import_redaction_report(&existing.redaction_report)
            // `redaction_report` is a separate SQL column. Leaving an old
            // free-form V1 payload attached while relabelling ownership V2
            // would create a durable raw-data side channel, so keep V1
            // immutable when it cannot be rebuilt into the typed shape.
            .map_err(|_| CaptureCatalogError::ImportSessionConflict)?;
        let updated = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_session
                 SET metadata_json = ?,
                     redaction_report = ?,
                     sync_revision = sync_revision + 1
                 WHERE session_id = ? AND agent_kind = ? AND provider_session_id = ?
                   AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                   AND workspace_id IS ? AND workspace_fence IS ?",
                [
                    metadata_json.into(),
                    redaction_report.into(),
                    session.session_id.clone().into(),
                    session.agent_kind.clone().into(),
                    session.provider_session_id.clone().into(),
                    scope.repo_id.clone().into(),
                    scope.worktree_id.clone().into(),
                    scope.workspace_id.clone().into(),
                    scope.workspace_fence.into(),
                ],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
        if updated.rows_affected() != 1 {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        Ok(())
    }

    /// Delete only a provisional import session that made no durable progress.
    /// This remains in the caller's release transaction so a racing importer
    /// cannot observe a released identity with a resurrected session row.
    pub(crate) async fn discard_unprogressed_import_session(
        txn: &DatabaseTransaction,
        scope: &CaptureScope,
        session: &CaptureCatalogSession,
    ) -> Result<(), CaptureCatalogError> {
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "DELETE FROM agent_session
             WHERE session_id = ? AND agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND COALESCE(json_extract(metadata_json, '$.import_provisional'), 0) = 1
               AND NOT EXISTS (
                 SELECT 1 FROM agent_checkpoint c
                 WHERE c.session_id = agent_session.session_id
               )
               AND NOT EXISTS (
                 SELECT 1 FROM agent_import_identity i
                 WHERE i.agent_kind = agent_session.agent_kind
                   AND i.provider_session_id = agent_session.provider_session_id
                   AND i.owner IS NOT NULL
               )",
            [
                session.session_id.clone().into(),
                session.agent_kind.clone().into(),
                session.provider_session_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
        Ok(())
    }

    /// Apply a completed import's lifecycle facts in the transaction supplied
    /// by the checkpoint store.  This is intentionally not `CaptureCatalogPort::apply`:
    /// a standalone transaction here would break DR-05's ref/coverage/identity
    /// atomicity.
    pub(crate) async fn apply_import_session_lifecycle(
        txn: &DatabaseTransaction,
        commit: &CaptureImportSessionCommit,
    ) -> Result<(), CaptureCatalogError> {
        verify_mutable_scope(txn, &commit.scope, &commit.session).await?;
        let existing = read_session(txn, &commit.session)
            .await?
            .ok_or(CaptureCatalogError::ScopeRejected)?;
        if !existing.matches_scope(&commit.scope)
            || existing.session_id != commit.session.session_id
        {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        if !session_has_no_unsettled_receipts(&existing)? {
            return Err(CaptureCatalogError::ImportSessionConflict);
        }
        let source_metadata =
            parse_closed_v2_import_session_metadata(&commit.source.metadata_json(false, None)?)?;
        validate_closed_v2_import_session_metadata(&source_metadata)?;
        let matches_persisted_ownership = commit
            .source
            .matches_persisted_ownership(&existing.metadata_json)?;
        let (mut metadata, working_dir) = if matches_persisted_ownership {
            if existing.working_dir != commit.session.working_dir {
                return Err(CaptureCatalogError::ImportSessionConflict);
            }
            validate_v2_import_session_record(&existing.metadata_json, &existing.redaction_report)?;
            // Do not JSON-patch an existing V2 row: that would preserve a raw
            // locator or an unknown nested payload injected before this lifecycle
            // update. Reparse both sides through the closed metadata schema and
            // replace the full object atomically with the typed merge.
            (
                parse_closed_v2_import_session_metadata(&existing.metadata_json)?,
                commit.session.working_dir.clone(),
            )
        } else {
            // A live session may be rooted in a subdirectory of the same
            // repository. Preparation proves its storage identity before the
            // import lease is acquired. Preserve that verified live working
            // directory so the exact live-session fence continues accepting
            // subsequent hooks from the subdirectory after adoption.
            (
                closed_v2_metadata_from_live_session(&commit.source, &existing)?,
                existing.working_dir.clone(),
            )
        };
        validate_closed_v2_import_session_metadata(&metadata)?;
        metadata.repository_identity = source_metadata.repository_identity;
        metadata.source_kind = source_metadata.source_kind;
        metadata.source_id = source_metadata.source_id;
        metadata.source_fingerprint = source_metadata.source_fingerprint;
        metadata.import_source_schema_version = source_metadata.import_source_schema_version;
        metadata.import_provisional = false;
        metadata.transcript_snapshot = source_metadata.transcript_snapshot;
        metadata.imported = Some(true);
        validate_closed_v2_import_session_metadata(&metadata)?;
        let ownership_metadata_json =
            serde_json::to_string(&metadata).map_err(|_| CaptureCatalogError::InvalidRequest)?;
        let redaction_report_json = sanitize_v2_import_redaction_report(
            &import_redaction_report_json(&commit.redaction_report)?,
        )?;
        let updated = txn
            .execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_session
                 SET working_dir = ?,
                     started_at = MIN(started_at, ?),
                     sync_revision = sync_revision + 1,
                     state = CASE
                       WHEN state = 'quarantined' THEN state
                       WHEN ? < last_event_at THEN state
                       ELSE ?
                     END,
                     last_event_at = MAX(last_event_at, ?),
                     stopped_at = CASE
                       WHEN ? < last_event_at THEN stopped_at
                       WHEN ? IS NULL THEN NULL
                       WHEN stopped_at IS NULL THEN ?
                       ELSE MAX(stopped_at, ?)
                     END,
                     metadata_json = ?,
                     redaction_report = ?
                 WHERE session_id = ? AND agent_kind = ? AND provider_session_id = ?
                   AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                   AND workspace_id IS ? AND workspace_fence IS ?",
                [
                    working_dir.into(),
                    commit.started_at.into(),
                    commit.last_event_at.into(),
                    commit.state.as_db().into(),
                    commit.last_event_at.into(),
                    commit.last_event_at.into(),
                    commit.stopped_at.into(),
                    commit.stopped_at.into(),
                    commit.stopped_at.into(),
                    ownership_metadata_json.into(),
                    redaction_report_json.into(),
                    commit.session.session_id.clone().into(),
                    commit.session.agent_kind.clone().into(),
                    commit.session.provider_session_id.clone().into(),
                    commit.scope.repo_id.clone().into(),
                    commit.scope.worktree_id.clone().into(),
                    commit.scope.workspace_id.clone().into(),
                    commit.scope.workspace_fence.into(),
                ],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
        if updated.rows_affected() != 1 {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        Ok(())
    }

    async fn apply_inner(
        &self,
        txn: &DatabaseTransaction,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogApplyResult, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;
        // Claim the provider session atomically with its first checkpointing
        // lifecycle mutation. The hook's early owner lookup is only an
        // optimization: two adapters can both observe no row before either
        // writes. State-only SessionStart/TurnStart actions remain exempt, so
        // select the earliest durable claim rather than merely finding any
        // foreign kind. This runs before reading or creating a receipt, so a
        // losing terminal action returns a harmless conflict instead of
        // leaving a pending finalizer ledger.
        if !request.action.owner_claim_exempt()
            && let Some(owner_kind) =
                scoped_provider_owner_kind(txn, &request.scope, &request.session).await?
            && owner_kind != request.session.agent_kind
        {
            return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }

        let existing = read_session(txn, &request.session).await?;
        if let Some(existing) = &existing {
            if !existing.matches_scope(&request.scope) {
                return Err(CaptureCatalogError::ScopeRejected);
            }
            if existing.session_id != request.session.session_id
                || existing.working_dir != request.session.working_dir
            {
                return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::SessionIdentity,
                });
            }
        }
        let existing_v2_import = existing
            .as_ref()
            .map(|session| {
                validate_existing_v2_import_session_record(
                    &session.metadata_json,
                    &session.redaction_report,
                )
            })
            .transpose()?
            .unwrap_or(false);

        let metadata = existing
            .as_ref()
            .map(|session| session.metadata_json.as_str())
            .unwrap_or("{}");
        let (mut metadata, mut ledger) = decode_receipt_metadata(metadata)?;
        if request.metadata.concurrent_active {
            metadata.insert(
                "concurrent_active".to_string(),
                serde_json::Value::Bool(true),
            );
        }
        let redaction_report = if existing_v2_import {
            // Imported V2 redaction evidence is a closed `{ "import": ... }`
            // record. A normal hook report has a different, non-import shape;
            // retaining it would make the next cloud publish/restore reject a
            // previously valid row. Preserve the verified import evidence
            // while still accepting the lifecycle transition.
            existing
                .as_ref()
                .map(|session| session.redaction_report.clone())
                .ok_or(CaptureCatalogError::InvalidRequest)?
        } else {
            match &request.metadata.redaction_report {
                Some(report) => report.json()?,
                None => existing
                    .as_ref()
                    .map(|session| session.redaction_report.clone())
                    .unwrap_or_else(|| "{}".to_string()),
            }
        };

        let current = existing.as_ref().map(StoredCatalogSession::durable_state);
        // Providers may legitimately omit a native delivery ID for a terminal
        // callback. A fresh ingress UUID on redelivery must not reserve a
        // second finalizer: within this transaction, adopt exactly one local
        // pending terminal receipt that still owns the current revision. The
        // local fallback-key check deliberately excludes native/HMAC receipts;
        // `can_resume_from` fences any intervening live action, so a later
        // genuine SessionEnd receives a new action instead of being dropped.
        if request.action.lifecycle_kind == Some(LifecycleEventKind::SessionEnd)
            && request.mutation.is_terminal()
            && request.action.receipt_key.is_none()
        {
            let mut candidates = ledger.entries.iter().filter(|receipt| {
                receipt.status == StoredReceiptStatus::Pending
                    && receipt.is_deferred_terminal()
                    && receipt.receipt_key == format!("{ACTION_RECEIPT_PREFIX}{}", receipt.event_id)
                    && receipt.can_resume_from(current)
            });
            if let Some(receipt) = candidates.next() {
                if candidates.next().is_some() {
                    return Err(CaptureCatalogError::MalformedReceiptLedger);
                }
                let adopted_action = action_from_stored_receipt(receipt)?
                    .with_lifecycle_kind(LifecycleEventKind::SessionEnd);
                let finalizer = receipt.finalizer.as_ref();
                let unbound = finalizer.is_some_and(StoredFinalizeReceipt::is_unbound_snapshot);
                return Ok(CaptureCatalogApplyResult::ResumePending {
                    state: current.ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
                    checkpoint: receipt.intent.checkpoint.to_checkpoint_write(),
                    terminal_finalizer_needs_binding: finalizer.is_none() || unbound,
                    terminal_marker_generation: finalizer
                        .filter(|finalizer| !finalizer.is_unbound_snapshot())
                        .map(|finalizer| finalizer.marker_generation.clone()),
                    adopted_action: Some(adopted_action),
                });
            }
        }
        let receipt_key = request
            .action
            .receipt_storage_key(request.mutation.is_terminal());
        if let Some(receipt_key) = receipt_key.as_deref()
            && let Some(receipt) = ledger.find(receipt_key)
        {
            if !receipt.matches(&request.action, &request.mutation) {
                return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ActionMismatch,
                });
            }
            if receipt.status == StoredReceiptStatus::Pending
                && receipt.is_deferred_terminal()
                && !receipt.can_resume_from(current)
            {
                return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                });
            }
            return Ok(match receipt.status {
                StoredReceiptStatus::Pending => {
                    let finalizer = receipt.finalizer.as_ref();
                    let unbound = finalizer.is_some_and(StoredFinalizeReceipt::is_unbound_snapshot);
                    CaptureCatalogApplyResult::ResumePending {
                        state: current.ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
                        checkpoint: receipt.intent.checkpoint.to_checkpoint_write(),
                        terminal_finalizer_needs_binding: receipt.is_deferred_terminal()
                            && (finalizer.is_none() || unbound),
                        terminal_marker_generation: finalizer
                            .filter(|finalizer| !finalizer.is_unbound_snapshot())
                            .map(|finalizer| finalizer.marker_generation.clone()),
                        adopted_action: None,
                    }
                }
                StoredReceiptStatus::Complete => CaptureCatalogApplyResult::AlreadyApplied,
            });
        }

        if current != request.mutation.expected {
            return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ExpectedState,
            });
        }

        let initial = if current.is_none() {
            Some(initial_session_revision(txn, &request.session).await?)
        } else {
            None
        };
        // A terminal state is not published until the checkpoint facade calls
        // `complete`. Existing sessions retain their exact phase/stopped_at;
        // first-seen terminal events get a nonterminal `pending` row solely
        // to carry the durable receipt. Reserving the next revision is still
        // necessary: two pending terminal receipts must not overwrite each
        // other in `metadata_json`, and that revision fences a stale finalizer.
        let defer_terminal = request.mutation.is_terminal();
        let next_state = if defer_terminal {
            match current {
                Some(state) => DurableCaptureState {
                    phase: state.phase,
                    stopped_at: state.stopped_at,
                    sync_revision: state
                        .sync_revision
                        .checked_add(1)
                        .ok_or(CaptureCatalogError::InvalidRequest)?,
                },
                None => DurableCaptureState {
                    phase: CapturePhase::Pending,
                    stopped_at: None,
                    sync_revision: initial
                        .as_ref()
                        .map(|(revision, _)| *revision)
                        .ok_or(CaptureCatalogError::InvalidRequest)?,
                },
            }
        } else {
            let stopped_at = match request.mutation.stopped_at {
                StoppedAtMutation::Preserve => current.and_then(|state| state.stopped_at),
                StoppedAtMutation::Set(_) => return Err(CaptureCatalogError::InvalidRequest),
            };
            let sync_revision = match current {
                Some(state) => state
                    .sync_revision
                    .checked_add(1)
                    .ok_or(CaptureCatalogError::InvalidRequest)?,
                None => initial
                    .as_ref()
                    .map(|(revision, _)| *revision)
                    .ok_or(CaptureCatalogError::InvalidRequest)?,
            };
            DurableCaptureState {
                phase: request.mutation.next_phase,
                stopped_at,
                sync_revision,
            }
        };

        let receipt = if let Some(receipt_key) = receipt_key.as_deref() {
            let status = if request.mutation.checkpoint == CheckpointWrite::None {
                StoredReceiptStatus::Complete
            } else {
                StoredReceiptStatus::Pending
            };
            ledger.insert(StoredReceipt::new(
                receipt_key,
                &request.action,
                &request.mutation,
                status,
                next_state.sync_revision,
            ))?;
            match status {
                StoredReceiptStatus::Pending => CaptureReceiptDisposition::Pending,
                StoredReceiptStatus::Complete => CaptureReceiptDisposition::Complete,
            }
        } else {
            CaptureReceiptDisposition::NotTracked
        };

        if existing.is_none() {
            let (_, incarnation) = initial.ok_or(CaptureCatalogError::InvalidRequest)?;
            if let Some(incarnation) = incarnation {
                metadata.insert(
                    "capture_incarnation".to_string(),
                    serde_json::Value::String(incarnation),
                );
            }
        }
        let metadata_json = encode_receipt_metadata(metadata, &ledger)?;
        validate_v2_import_session_record_after_mutation(
            existing_v2_import,
            &metadata_json,
            &redaction_report,
        )?;

        ensure_catalog_mutation_deadline(self.execution_deadline)?;
        let rows_affected = if let Some(existing) = &existing {
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_session
                 SET state = ?, last_event_at = ?, stopped_at = ?, sync_revision = ?,
                     metadata_json = ?, redaction_report = ?
                 WHERE agent_kind = ? AND provider_session_id = ?
                   AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                   AND workspace_id IS ? AND workspace_fence IS ?
                   AND state = ? AND stopped_at IS ? AND sync_revision = ?",
                [
                    next_state.phase.as_db().into(),
                    request.mutation.observed_at.into(),
                    next_state.stopped_at.into(),
                    next_state.sync_revision.into(),
                    metadata_json.into(),
                    redaction_report.into(),
                    request.session.agent_kind.clone().into(),
                    request.session.provider_session_id.clone().into(),
                    request.scope.repo_id.clone().into(),
                    request.scope.worktree_id.clone().into(),
                    request.scope.workspace_id.clone().into(),
                    request.scope.workspace_fence.into(),
                    existing.state.phase.as_db().into(),
                    existing.state.stopped_at.into(),
                    existing.state.sync_revision.into(),
                ],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?
            .rows_affected()
        } else {
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "INSERT INTO agent_session (
                    session_id, agent_kind, provider_session_id, state, working_dir,
                    metadata_json, redaction_report, started_at, last_event_at, stopped_at,
                    sync_revision, repo_id, worktree_id, workspace_id, workspace_fence, scope_state
                 ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'scoped')",
                [
                    request.session.session_id.clone().into(),
                    request.session.agent_kind.clone().into(),
                    request.session.provider_session_id.clone().into(),
                    next_state.phase.as_db().into(),
                    request.session.working_dir.clone().into(),
                    metadata_json.into(),
                    redaction_report.into(),
                    request.mutation.observed_at.into(),
                    request.mutation.observed_at.into(),
                    next_state.stopped_at.into(),
                    next_state.sync_revision.into(),
                    request.scope.repo_id.clone().into(),
                    request.scope.worktree_id.clone().into(),
                    request.scope.workspace_id.clone().into(),
                    request.scope.workspace_fence.into(),
                ],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?
            .rows_affected()
        };
        if rows_affected != 1 {
            return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }

        Ok(CaptureCatalogApplyResult::Applied {
            state: next_state,
            checkpoint: request.mutation.checkpoint,
            receipt,
        })
    }

    /// Persist/check a terminal finalizer attempt under the receipt's
    /// existing revision fence. This deliberately does not manufacture a
    /// checkpoint or publish `stopped`: only a later strict `complete` after
    /// `FinalizeCheckpointProgress::Durable` may do that.
    async fn finalize_inner(
        &self,
        txn: &DatabaseTransaction,
        request: &CaptureCatalogFinalizeRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;
        let Some(existing) = read_session(txn, &request.session).await? else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !existing.matches_scope(&request.scope) {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        if existing.session_id != request.session.session_id
            || existing.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let existing_v2_import = validate_existing_v2_import_session_record(
            &existing.metadata_json,
            &existing.redaction_report,
        )?;

        let (metadata, mut ledger) = decode_receipt_metadata(&existing.metadata_json)?;
        let receipt_key = request.action.completion_receipt_storage_key();
        let Some(stored_receipt) = ledger.find(&receipt_key) else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !stored_receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if !stored_receipt.is_deferred_terminal() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if stored_receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogFinalizeResult::AlreadyComplete);
        }
        if !stored_receipt.can_resume_from(Some(existing.state)) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }

        let persisted_finalizer = stored_receipt.finalizer.clone();
        if let Some(finalizer) = persisted_finalizer.as_ref()
            && finalizer.status == StoredFinalizeStatus::Quarantined
        {
            return Ok(CaptureCatalogFinalizeResult::Quarantined {
                reason: finalizer
                    .quarantine_reason()
                    .ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
            });
        }
        if existing.state.phase == CapturePhase::Quarantined {
            // A repair tool may have quarantined this receipt before the
            // finalizer could store its own reason. Never reopen it from a
            // late hook; leave it for doctor/repair.
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }

        // Preparing a terminal receipt before source/coverage work is
        // intentionally idempotent. A retry of the same no-repository
        // provisional marker must not burn another bounded finalizer attempt
        // merely because no real checkpoint can be started yet.
        if let Some(finalizer) = persisted_finalizer.as_ref()
            && finalizer.is_unbound_snapshot()
            && matches!(request.checkpoint, FinalizeCheckpointProgress::NotStarted)
            && finalizer.marker_generation == request.marker_generation
            && request.source_digest.is_none()
        {
            return Ok(CaptureCatalogFinalizeResult::Pending {
                attempts: finalizer.attempts,
                stage: finalizer.stage.to_stage(),
            });
        }

        let current_receipt = match persisted_finalizer.as_ref() {
            Some(finalizer)
                if finalizer.is_unbound_snapshot()
                    && matches!(request.checkpoint, FinalizeCheckpointProgress::NotStarted)
                    && !request
                        .marker_generation
                        .starts_with(UNBOUND_FINALIZER_MARKER_PREFIX) =>
            {
                Some(finalizer.rebind_unbound_snapshot(
                    request.marker_generation.clone(),
                    request.source_digest.clone(),
                )?)
            }
            Some(finalizer) => Some(finalizer.to_pending_receipt()?),
            None => None,
        };
        // A retry may arrive with a fresh process-local deadline.  The
        // stored policy is authoritative once the first attempt exists:
        // accepting the new policy would let a replay extend its deadline,
        // while rejecting it before marker comparison would hide a stale
        // marker takeover as a generic invalid request.
        let effective_policy = persisted_finalizer
            .as_ref()
            .map(StoredFinalizeReceipt::to_policy)
            .transpose()?
            .unwrap_or_else(|| request.policy.clone());
        let decision = decide_finalization(FinalizeDecisionInput {
            policy: &effective_policy,
            current_receipt: current_receipt.as_ref(),
            marker_generation: &request.marker_generation,
            source_digest: request.source_digest.as_deref(),
            now_millis: request.now_millis,
            checkpoint: request.checkpoint,
        })
        .map_err(|_| CaptureCatalogError::InvalidRequest)?;

        match decision {
            FinalizeDecision::CommitTerminal => {
                let finalizer = persisted_finalizer
                    .as_ref()
                    .ok_or(CaptureCatalogError::MalformedReceiptLedger)?;
                let proof = finalizer
                    .completion_proof()
                    .ok_or(CaptureCatalogError::MalformedReceiptLedger)?;
                Ok(CaptureCatalogFinalizeResult::ReadyToComplete { proof })
            }
            FinalizeDecision::PersistPending(pending) => {
                let Some(stored_receipt) = ledger.find_mut(&receipt_key) else {
                    return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::MissingReceipt,
                    });
                };
                stored_receipt.finalizer = Some(StoredFinalizeReceipt::from_pending(&pending));
                let metadata_json = encode_receipt_metadata(metadata, &ledger)?;
                validate_v2_import_session_record_after_mutation(
                    existing_v2_import,
                    &metadata_json,
                    &existing.redaction_report,
                )?;
                let updated = update_finalizer_metadata(
                    txn,
                    &request.scope,
                    &request.session,
                    existing.state.sync_revision,
                    metadata_json,
                    self.execution_deadline,
                )
                .await?;
                if !updated {
                    return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::ConditionalWrite,
                    });
                }
                Ok(CaptureCatalogFinalizeResult::Pending {
                    attempts: pending.attempts(),
                    stage: pending.stage(),
                })
            }
            FinalizeDecision::Quarantine { reason } => {
                // Expired artifact recovery retains the original reservation:
                // session quarantine would advance its revision and destroy
                // the very fence needed for one explicit operator attempt.
                if effective_policy.mode() == CaptureFinalizeMode::Deferrable
                    && matches!(
                        reason,
                        FinalizeQuarantineReason::AttemptLimit
                            | FinalizeQuarantineReason::WindowLimit
                    )
                    && request.checkpoint_write == CheckpointWrite::Committed
                {
                    let checkpoint = super::checkpoint::checkpoint_id_for_capture_action(
                        request.action.event_id,
                        request.checkpoint_write,
                    );
                    if super::pending::quarantine_checkpoint_if_present(
                        txn,
                        &request.scope.repo_id,
                        &checkpoint,
                    )
                    .await
                    .map_err(|_| CaptureCatalogError::Database)?
                    {
                        return Ok(CaptureCatalogFinalizeResult::Quarantined { reason });
                    }
                }
                // Even an immediately expired synchronous attempt leaves a
                // durable, content-free receipt for doctor. This preserves
                // the original deadline/marker/source commitment rather
                // than turning the timeout into a silent state transition.
                let pending = match current_receipt {
                    Some(receipt) => receipt,
                    None => PendingFinalizeReceipt::new(
                        &effective_policy,
                        request.marker_generation.clone(),
                        request.source_digest.clone(),
                        request.now_millis,
                        stage_for_progress(request.checkpoint),
                    )
                    .map_err(|_| CaptureCatalogError::InvalidRequest)?,
                };
                let Some(stored_receipt) = ledger.find_mut(&receipt_key) else {
                    return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::MissingReceipt,
                    });
                };
                stored_receipt.finalizer =
                    Some(StoredFinalizeReceipt::quarantined(&pending, reason));
                let metadata_json = encode_receipt_metadata(metadata, &ledger)?;
                validate_v2_import_session_record_after_mutation(
                    existing_v2_import,
                    &metadata_json,
                    &existing.redaction_report,
                )?;
                let next_revision = existing
                    .state
                    .sync_revision
                    .checked_add(1)
                    .ok_or(CaptureCatalogError::InvalidRequest)?;
                let updated = quarantine_finalizer_session(
                    txn,
                    request,
                    existing.state.sync_revision,
                    next_revision,
                    metadata_json,
                    self.execution_deadline,
                )
                .await?;
                if !updated {
                    return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::ConditionalWrite,
                    });
                }
                Ok(CaptureCatalogFinalizeResult::Quarantined { reason })
            }
        }
    }

    /// Elect the first concrete terminal writer marker, or return the one a
    /// concurrent delivery has already bound.  This must stay separate from
    /// `finalize_inner`: that lower-level transition correctly treats a
    /// *stale writer* submitting a different marker as a quarantine, while a
    /// duplicate native delivery has not become a writer until this election
    /// atomically chooses its marker.
    async fn claim_terminal_attempt_inner(
        &self,
        txn: &DatabaseTransaction,
        request: &CaptureCatalogFinalizeRequest,
        require_source_match: bool,
    ) -> Result<CaptureCatalogTerminalAttempt, CaptureCatalogError> {
        if !matches!(request.checkpoint, FinalizeCheckpointProgress::NotStarted) {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;
        let Some(existing) = read_session(txn, &request.session).await? else {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !existing.matches_scope(&request.scope)
            || existing.session_id != request.session.session_id
            || existing.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
        let receipt_key = request.action.completion_receipt_storage_key();
        let Some(receipt) = ledger.find(&receipt_key) else {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if !receipt.is_deferred_terminal() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogTerminalAttempt::AlreadyComplete);
        }
        if !receipt.can_resume_from(Some(existing.durable_state())) {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        let persisted = receipt.finalizer.clone();
        if let Some(finalizer) = persisted.as_ref() {
            if finalizer.status == StoredFinalizeStatus::Quarantined {
                return Ok(CaptureCatalogTerminalAttempt::Quarantined {
                    reason: finalizer
                        .quarantine_reason()
                        .ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
                });
            }
            if !finalizer.is_unbound_snapshot() {
                // A same-source retry can resume an elected writer only while
                // its marker is still live. If the process died between the
                // catalog election and marker registration, every later
                // delivery must advance the durable retry accounting instead
                // of repeatedly returning `Bound`. Check under this writer
                // transaction so a racing registration either wins first or
                // is fenced after the next transition commits.
                if require_source_match && finalizer.source_digest == request.source_digest {
                    let checkpoint_id =
                        crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                            request.action.event_id,
                            request.checkpoint_write,
                        );
                    match crate::internal::ai::traces::terminal_attempt_marker_status(
                        txn,
                        &request.session.session_id,
                        &checkpoint_id,
                        &finalizer.marker_generation,
                    )
                    .await
                    .map_err(|_| CaptureCatalogError::Database)?
                    {
                        crate::internal::ai::traces::TerminalAttemptMarkerStatus::RegisteredExact => {
                            // A live writer owns this fence. Its duplicate
                            // delivery must remain observational.
                        }
                        crate::internal::ai::traces::TerminalAttemptMarkerStatus::Incompatible => {
                            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                conflict: CaptureCatalogConflict::FinalizerFence,
                            });
                        }
                        crate::internal::ai::traces::TerminalAttemptMarkerStatus::Absent => {
                            match terminal_durable_checkpoint_status(
                                txn,
                                &request.session,
                                request.action.event_id,
                                request.checkpoint_write,
                            )
                            .await?
                            {
                                // A durable checkpoint is safe to complete
                                // even after the retry budget elapsed; the
                                // finalizer's durable path intentionally
                                // commits before budget classification.
                                TerminalDurableCheckpointStatus::Exact => {
                                    return Ok(CaptureCatalogTerminalAttempt::DurableReplay);
                                }
                                TerminalDurableCheckpointStatus::Incompatible => {
                                    return Ok(
                                        CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                            conflict: CaptureCatalogConflict::FinalizerFence,
                                        },
                                    );
                                }
                                TerminalDurableCheckpointStatus::Absent => {}
                            }
                            // The request carries this delivery's fresh
                            // process-local candidate. Reapply the elected
                            // generation before entering the strict
                            // finalizer transition, otherwise it would be
                            // classified as a stale-marker conflict rather
                            // than the pending retry/window receipt.
                            let mut retry = request.clone();
                            retry.marker_generation = finalizer.marker_generation.clone();
                            return terminal_attempt_from_unregistered_same_source_finalizer_result(
                                request,
                                finalizer.marker_generation.clone(),
                                finalizer.source_digest.clone(),
                                self.finalize_inner(txn, &retry).await?,
                            );
                        }
                    }
                }
                // A duplicate native delivery is not a writer until this
                // election returns `Bound`. If it captured a newer source
                // after another delivery elected this marker, it must merely
                // observe the persisted attempt. Sending that changed source
                // through `finalize_inner` would falsely quarantine the
                // elected writer before its ref/object transaction finishes.
                if require_source_match && finalizer.source_digest != request.source_digest {
                    let checkpoint_id =
                        crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                            request.action.event_id,
                            request.checkpoint_write,
                        );
                    match crate::internal::ai::traces::terminal_attempt_marker_status(
                        txn,
                        &request.session.session_id,
                        &checkpoint_id,
                        &finalizer.marker_generation,
                    )
                    .await
                    .map_err(|_| CaptureCatalogError::Database)?
                    {
                        // A concrete marker proves that the elected source
                        // owns an active/replayable writer attempt. A newer
                        // snapshot is only an observer and must not mutate
                        // its finalizer or source fence.
                        crate::internal::ai::traces::TerminalAttemptMarkerStatus::RegisteredExact => {
                            return Ok(CaptureCatalogTerminalAttempt::Adopted {
                                marker_generation: finalizer.marker_generation.clone(),
                                source_digest: finalizer.source_digest.clone(),
                            });
                        }
                        // Never reinterpret a malformed or foreign marker as
                        // an absent attempt: doing so would let a later
                        // delivery quarantine or replace unknown recovery
                        // evidence.
                        crate::internal::ai::traces::TerminalAttemptMarkerStatus::Incompatible => {
                            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                conflict: CaptureCatalogConflict::FinalizerFence,
                            });
                        }
                        // The elected source never registered its marker, so
                        // its raw snapshot cannot be reconstructed safely.
                        // Transition the exact pending receipt to durable,
                        // repair-required source conflict rather than letting
                        // every changed-source replay remain pending forever.
                        crate::internal::ai::traces::TerminalAttemptMarkerStatus::Absent => {
                            match terminal_durable_checkpoint_status(
                                txn,
                                &request.session,
                                request.action.event_id,
                                request.checkpoint_write,
                            )
                            .await?
                            {
                                // The elected writer committed its ref/catalog
                                // checkpoint and retired the ordinary marker
                                // before it could complete the receipt. The
                                // changed source is replay-only: a separate
                                // catalog operation rechecks this same durable
                                // row immediately before strict completion.
                                TerminalDurableCheckpointStatus::Exact => {
                                    return Ok(CaptureCatalogTerminalAttempt::DurableReplay);
                                }
                                // A deterministic checkpoint id may never be
                                // borrowed from another session or scope.
                                TerminalDurableCheckpointStatus::Incompatible => {
                                    return Ok(
                                        CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                            conflict: CaptureCatalogConflict::FinalizerFence,
                                        },
                                    );
                                }
                                TerminalDurableCheckpointStatus::Absent => {}
                            }
                            let mut source_conflict = request.clone();
                            source_conflict.marker_generation =
                                finalizer.marker_generation.clone();
                            return match self.finalize_inner(txn, &source_conflict).await? {
                                CaptureCatalogFinalizeResult::Quarantined { reason } => {
                                    Ok(CaptureCatalogTerminalAttempt::Quarantined { reason })
                                }
                                CaptureCatalogFinalizeResult::AlreadyComplete => {
                                    Ok(CaptureCatalogTerminalAttempt::AlreadyComplete)
                                }
                                CaptureCatalogFinalizeResult::ConflictUnchanged { conflict } => {
                                    Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                        conflict,
                                    })
                                }
                                CaptureCatalogFinalizeResult::Pending { .. }
                                | CaptureCatalogFinalizeResult::ReadyToComplete { .. } => {
                                    Err(CaptureCatalogError::InvalidRequest)
                                }
                            };
                        }
                    }
                }
                return Ok(CaptureCatalogTerminalAttempt::Bound {
                    marker_generation: finalizer.marker_generation.clone(),
                    source_digest: finalizer.source_digest.clone(),
                    attempts: finalizer.to_pending_receipt()?.attempts(),
                    registration_fence: Box::new(
                        CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                            request,
                            finalizer.marker_generation.clone(),
                            finalizer.source_digest.clone(),
                        ),
                    ),
                });
            }
        }

        // No concrete marker exists yet. The transaction owns the election:
        // if two hook processes reached this branch, SQLite serializes them;
        // the second sees the first's concrete marker on its next read.
        match self.finalize_inner(txn, request).await? {
            CaptureCatalogFinalizeResult::Pending { attempts, .. } => {
                Ok(CaptureCatalogTerminalAttempt::Bound {
                    marker_generation: request.marker_generation.clone(),
                    source_digest: request.source_digest.clone(),
                    attempts,
                    registration_fence: Box::new(
                        CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                            request,
                            request.marker_generation.clone(),
                            request.source_digest.clone(),
                        ),
                    ),
                })
            }
            CaptureCatalogFinalizeResult::Quarantined { reason } => {
                Ok(CaptureCatalogTerminalAttempt::Quarantined { reason })
            }
            CaptureCatalogFinalizeResult::AlreadyComplete => {
                Ok(CaptureCatalogTerminalAttempt::AlreadyComplete)
            }
            CaptureCatalogFinalizeResult::ConflictUnchanged { conflict } => {
                Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged { conflict })
            }
            CaptureCatalogFinalizeResult::ReadyToComplete { .. } => {
                Err(CaptureCatalogError::InvalidRequest)
            }
        }
    }

    /// Reconstruct a strict terminal proof from the existing ledger after
    /// `CheckpointStore` has established that the stable checkpoint ID is
    /// already durable.  This is intentionally read-only: a recovery must
    /// not turn a newly-created marker into ownership of a prior pending
    /// receipt merely because the durable object is present.
    async fn prove_durable_replay_inner(
        &self,
        txn: &DatabaseTransaction,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;
        let Some(existing) = read_session(txn, &request.session).await? else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !existing.matches_scope(&request.scope) {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        if existing.session_id != request.session.session_id
            || existing.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }

        let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
        let receipt_key = request.action.completion_receipt_storage_key();
        let Some(receipt) = ledger.find(&receipt_key) else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if !receipt.is_deferred_terminal() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogFinalizeResult::AlreadyComplete);
        }
        if !receipt.can_resume_from(Some(existing.state)) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        let Some(finalizer) = receipt.finalizer.as_ref() else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            });
        };
        if finalizer.status == StoredFinalizeStatus::Quarantined {
            return Ok(CaptureCatalogFinalizeResult::Quarantined {
                reason: finalizer
                    .quarantine_reason()
                    .ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
            });
        }
        let Some(proof) = finalizer.completion_proof() else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            });
        };
        Ok(CaptureCatalogFinalizeResult::ReadyToComplete { proof })
    }

    /// Finish a terminal replay only while the deterministic checkpoint row
    /// still exists under this receipt's session/scope identity. Keeping this
    /// probe and `complete_inner` in one SQLite writer transaction closes the
    /// gap between a changed-source observer's claim and receipt completion:
    /// if recovery/repair removes the row, no new source bytes can be
    /// registered and the original receipt remains pending.
    async fn complete_durable_replay_inner(
        &self,
        txn: &DatabaseTransaction,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
        if !request.mutation.is_terminal()
            || request.mutation.checkpoint != CheckpointWrite::Committed
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;
        match terminal_durable_checkpoint_status(
            txn,
            &request.session,
            request.action.event_id,
            request.mutation.checkpoint,
        )
        .await?
        {
            TerminalDurableCheckpointStatus::Exact => {}
            TerminalDurableCheckpointStatus::Absent
            | TerminalDurableCheckpointStatus::Incompatible => {
                return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::FinalizerFence,
                });
            }
        }
        let pending_completion = CaptureCatalogCompleteRequest::from_apply(request)?;
        let proof = match self
            .prove_durable_replay_inner(txn, &pending_completion)
            .await?
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            CaptureCatalogFinalizeResult::AlreadyComplete => {
                return Ok(CaptureCatalogCompleteResult::AlreadyComplete);
            }
            CaptureCatalogFinalizeResult::ConflictUnchanged { conflict } => {
                return Ok(CaptureCatalogCompleteResult::ConflictUnchanged { conflict });
            }
            CaptureCatalogFinalizeResult::Pending { .. }
            | CaptureCatalogFinalizeResult::Quarantined { .. } => {
                return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::FinalizerFence,
                });
            }
        };
        let completion = CaptureCatalogCompleteRequest::from_finalizer(request, proof)?;
        self.complete_inner(txn, &completion).await
    }

    /// Mutate only fixed retry metadata without advancing `sync_revision`.
    /// A pending terminal receipt owns that revision; a diagnostic must never
    /// make a later strict completion look stale.
    async fn update_diagnostic_inner(
        &self,
        txn: &DatabaseTransaction,
        request: &CaptureCatalogApplyRequest,
        diagnostic: CaptureCatalogDiagnostic,
    ) -> Result<bool, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;
        let Some(existing) = read_session(txn, &request.session).await? else {
            return Ok(false);
        };
        if !existing.matches_scope(&request.scope) {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        if existing.session_id != request.session.session_id
            || existing.working_dir != request.session.working_dir
        {
            return Ok(false);
        }
        let existing_v2_import = validate_existing_v2_import_session_record(
            &existing.metadata_json,
            &existing.redaction_report,
        )?;
        let (mut metadata, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
        match diagnostic {
            CaptureCatalogDiagnostic::RecordRetryableCheckpointFailure { stage, failed_at } => {
                metadata.insert(
                    "capture_status".to_string(),
                    serde_json::Value::String("retryable".to_string()),
                );
                metadata.insert(
                    "capture_error_code".to_string(),
                    serde_json::Value::String("checkpoint_write_failed".to_string()),
                );
                metadata.insert(
                    "capture_error_stage".to_string(),
                    serde_json::Value::String(stage.metadata_stage().to_string()),
                );
                metadata.insert(
                    "capture_attempt_id".to_string(),
                    serde_json::Value::String(stage.attempt_id().to_string()),
                );
                metadata.insert(
                    "capture_failed_at".to_string(),
                    serde_json::Value::from(failed_at),
                );
            }
            CaptureCatalogDiagnostic::ClearRetryableCheckpointFailure => {
                if metadata
                    .get("capture_status")
                    .and_then(serde_json::Value::as_str)
                    != Some("retryable")
                {
                    return Ok(true);
                }
                for key in [
                    "capture_status",
                    "capture_error_code",
                    "capture_error_stage",
                    "capture_attempt_id",
                    "capture_failed_at",
                ] {
                    metadata.remove(key);
                }
            }
        }
        let metadata_json = encode_receipt_metadata(metadata, &ledger)?;
        validate_v2_import_session_record_after_mutation(
            existing_v2_import,
            &metadata_json,
            &existing.redaction_report,
        )?;
        update_finalizer_metadata(
            txn,
            &request.scope,
            &request.session,
            existing.state.sync_revision,
            metadata_json,
            self.execution_deadline,
        )
        .await
    }

    async fn complete_inner(
        &self,
        txn: &DatabaseTransaction,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
        ensure_catalog_schema(txn).await?;
        verify_mutable_scope(txn, &request.scope, &request.session).await?;
        reject_tombstone(txn, &request.session).await?;
        let Some(existing) = read_session(txn, &request.session).await? else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !existing.matches_scope(&request.scope) {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        if existing.session_id != request.session.session_id
            || existing.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let existing_v2_import = validate_existing_v2_import_session_record(
            &existing.metadata_json,
            &existing.redaction_report,
        )?;

        let (metadata, mut ledger) = decode_receipt_metadata(&existing.metadata_json)?;
        let receipt_key = request.action.completion_receipt_storage_key();
        let Some(receipt) = ledger.find(&receipt_key) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogCompleteResult::AlreadyComplete);
        }
        let deferred_terminal = receipt.is_deferred_terminal();
        let cleanup_incarnation = metadata.get("capture_incarnation").cloned();
        let covered_terminal_replay = matches!(
            &request.completion,
            CaptureCatalogCompletion::CoveredTerminalReplay
        );
        if deferred_terminal {
            match &request.completion {
                CaptureCatalogCompletion::Finalizer(proof) => {
                    let Some(finalizer) = receipt.finalizer.as_ref() else {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    };
                    if !finalizer.matches_proof(proof) {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    }
                }
                CaptureCatalogCompletion::CoveredTerminalReplay => {
                    // This exceptional acknowledgement is safe only after
                    // catalog reservation on top of a terminal state that
                    // was already publishable. A concrete marker/source
                    // fence would belong to a new writer attempt and must
                    // therefore retain the ordinary strict-finalizer path.
                    let Some(finalizer) = receipt.finalizer.as_ref() else {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    };
                    if !finalizer.is_unbound_snapshot()
                        || existing.state.phase != CapturePhase::Stopped
                        || existing.state.stopped_at.is_none()
                        || !has_committed_checkpoint(txn, &request.session).await?
                    {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    }
                }
                CaptureCatalogCompletion::Ordinary => {
                    return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::FinalizerFence,
                    });
                }
            }
        } else if !matches!(&request.completion, CaptureCatalogCompletion::Ordinary) {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let reserved_revision = receipt.reserved_revision;
        let terminal_timestamp = if deferred_terminal && !covered_terminal_replay {
            receipt
                .intent
                .terminal_timestamp()
                .ok_or(CaptureCatalogError::MalformedReceiptLedger)?
        } else {
            0
        };
        let recorded_at = receipt.recorded_at;
        if deferred_terminal && !receipt.can_resume_from(Some(existing.state)) {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        let Some(receipt) = ledger.find_mut(&receipt_key) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        receipt.status = StoredReceiptStatus::Complete;
        let metadata_json = encode_receipt_metadata(metadata, &ledger)?;
        validate_v2_import_session_record_after_mutation(
            existing_v2_import,
            &metadata_json,
            &existing.redaction_report,
        )?;
        ensure_catalog_mutation_deadline(self.execution_deadline)?;
        let updated = if deferred_terminal && !covered_terminal_replay {
            let next_revision = existing
                .state
                .sync_revision
                .checked_add(1)
                .ok_or(CaptureCatalogError::InvalidRequest)?;
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_session
                 SET state = 'stopped', stopped_at = ?, last_event_at = MAX(last_event_at, ?),
                     sync_revision = ?, metadata_json = ?
                 WHERE agent_kind = ? AND provider_session_id = ?
                   AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                   AND workspace_id IS ? AND workspace_fence IS ?
                   AND sync_revision = ?",
                [
                    terminal_timestamp.into(),
                    recorded_at.into(),
                    next_revision.into(),
                    metadata_json.into(),
                    request.session.agent_kind.clone().into(),
                    request.session.provider_session_id.clone().into(),
                    request.scope.repo_id.clone().into(),
                    request.scope.worktree_id.clone().into(),
                    request.scope.workspace_id.clone().into(),
                    request.scope.workspace_fence.into(),
                    reserved_revision.into(),
                ],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?
        } else {
            txn.execute_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "UPDATE agent_session SET metadata_json = ?
                 WHERE agent_kind = ? AND provider_session_id = ?
                   AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
                   AND workspace_id IS ? AND workspace_fence IS ?
                   AND sync_revision = ?",
                [
                    metadata_json.into(),
                    request.session.agent_kind.clone().into(),
                    request.session.provider_session_id.clone().into(),
                    request.scope.repo_id.clone().into(),
                    request.scope.worktree_id.clone().into(),
                    request.scope.workspace_id.clone().into(),
                    request.scope.workspace_fence.into(),
                    existing.state.sync_revision.into(),
                ],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?
        };
        if updated.rows_affected() != 1 {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        if deferred_terminal {
            let checkpoint =
                crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                    request.action.event_id,
                    CheckpointWrite::Committed,
                );
            if crate::internal::ai::capture::pending::has_header_for_checkpoint(
                txn,
                &request.scope.repo_id,
                &checkpoint,
            )
            .await
            .map_err(|_| CaptureCatalogError::Database)?
            {
                // Legacy receipts without artifacts need no new private
                // ownership proof or incarnation validation.
                let incarnation = match cleanup_incarnation.as_ref() {
                    None | Some(serde_json::Value::Null) => None,
                    Some(serde_json::Value::String(value))
                        if value.len() == 32
                            && value.bytes().all(|byte| {
                                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
                            }) =>
                    {
                        Some(value.as_str())
                    }
                    _ => return Err(CaptureCatalogError::MalformedReceiptLedger),
                };
                let owned = crate::internal::ai::capture::pending_identity::aliases_for_erasure(
                    txn,
                    &request.scope,
                    &request.session.session_id,
                    incarnation,
                )
                .await
                .map_err(|_| CaptureCatalogError::Database)?;
                // Retained evidence intentionally stays for cold doctor/GC
                // and explicit erase diagnostics; completion adds no wire.
                crate::internal::ai::capture::pending::remove_artifact(
                    txn,
                    &request.scope.repo_id,
                    &checkpoint,
                    owned.aliases(),
                )
                .await
                .map_err(|_| CaptureCatalogError::Database)?;
            }
        }
        Ok(CaptureCatalogCompleteResult::Completed)
    }
}

fn doctor_finalizer_scan_statement(
    backend: sea_orm::DbBackend,
    cursor: Option<&[u8]>,
    skip_invalid_keys: bool,
    limit: usize,
) -> Statement {
    let mut columns = Vec::new();
    for (name, cap) in [
        ("session_id", MAX_SESSION_ID_BYTES),
        ("agent_kind", MAX_AGENT_KIND_BYTES),
        ("provider_session_id", MAX_PROVIDER_SESSION_ID_BYTES),
        ("working_dir", MAX_WORKING_DIR_BYTES),
        ("metadata_json", MAX_DOCTOR_RECEIPT_METADATA_BYTES),
        ("repo_id", 512),
        ("worktree_id", 1024),
        ("workspace_id", 1024),
        ("state", 32),
    ] {
        columns.push(format!(
            "CASE WHEN typeof({name})='text' AND length(CAST({name} AS BLOB)) <= {cap}
             THEN CAST({name} AS BLOB) ELSE NULL END AS {name}"
        ));
    }
    columns.push("CASE WHEN typeof(sync_revision)='integer' THEN sync_revision ELSE NULL END AS sync_revision".into());
    columns.push("CASE WHEN typeof(stopped_at)='integer' THEN stopped_at ELSE NULL END AS stopped_at, CASE WHEN typeof(workspace_fence)='integer' THEN workspace_fence ELSE NULL END AS workspace_fence, typeof(stopped_at) IN ('null','integer') AS stopped_at_valid, typeof(workspace_fence) IN ('null','integer') AS workspace_fence_valid, typeof(workspace_id)='null' AS workspace_id_null".into());
    let mut sql = format!(
        "SELECT {} FROM agent_session WHERE scope_state='scoped'",
        columns.join(",")
    );
    if skip_invalid_keys {
        sql.push_str(" AND typeof(session_id)='text' AND length(CAST(session_id AS BLOB)) <= 1024");
    }
    let mut values: Vec<sea_orm::Value> = Vec::new();
    if let Some(cursor) = cursor {
        sql.push_str(" AND session_id COLLATE BINARY > CAST(? AS TEXT)");
        values.push(cursor.to_vec().into());
    }
    sql.push_str(&format!(
        " ORDER BY session_id COLLATE BINARY LIMIT {limit}"
    ));
    Statement::from_sql_and_values(backend, sql, values)
}

fn doctor_finalizer_text(
    row: &sea_orm::QueryResult,
    name: &str,
) -> Result<String, CaptureCatalogError> {
    let bytes = row
        .try_get_by::<Option<Vec<u8>>, _>(name)
        .map_err(|_| CaptureCatalogError::InvalidRequest)?
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    String::from_utf8(bytes).map_err(|_| CaptureCatalogError::InvalidRequest)
}

fn decode_doctor_finalizer_row(
    row: &sea_orm::QueryResult,
) -> Result<
    (
        CaptureScope,
        CaptureCatalogSession,
        DurableCaptureState,
        StoredReceiptLedger,
    ),
    CaptureCatalogError,
> {
    let scope = CaptureScope {
        repo_id: doctor_finalizer_text(row, "repo_id")?,
        worktree_id: doctor_finalizer_text(row, "worktree_id")?,
        workspace_id: if row
            .try_get_by::<i64, _>("workspace_id_null")
            .map_err(|_| CaptureCatalogError::InvalidRequest)?
            == 1
        {
            None
        } else {
            Some(doctor_finalizer_text(row, "workspace_id")?)
        },
        workspace_fence: row
            .try_get_by("workspace_fence")
            .map_err(|_| CaptureCatalogError::InvalidRequest)?,
    };
    if scope.repo_id.is_empty()
        || scope.workspace_id.is_some() != scope.workspace_fence.is_some()
        || row
            .try_get_by::<i64, _>("workspace_fence_valid")
            .map_err(|_| CaptureCatalogError::InvalidRequest)?
            != 1
        || row
            .try_get_by::<i64, _>("stopped_at_valid")
            .map_err(|_| CaptureCatalogError::InvalidRequest)?
            != 1
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let session = CaptureCatalogSession::new(
        doctor_finalizer_text(row, "session_id")?,
        doctor_finalizer_text(row, "agent_kind")?,
        doctor_finalizer_text(row, "provider_session_id")?,
        doctor_finalizer_text(row, "working_dir")?,
    )?;
    let state = DurableCaptureState {
        phase: CapturePhase::from_db(&doctor_finalizer_text(row, "state")?)
            .map_err(|_| CaptureCatalogError::InvalidRequest)?,
        stopped_at: row
            .try_get_by("stopped_at")
            .map_err(|_| CaptureCatalogError::InvalidRequest)?,
        sync_revision: row
            .try_get_by("sync_revision")
            .map_err(|_| CaptureCatalogError::InvalidRequest)?,
    };
    if state.sync_revision < 1
        || (state.phase == CapturePhase::Stopped && state.stopped_at.is_none())
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let metadata = doctor_finalizer_text(row, "metadata_json")?;
    parse_canon_value(metadata.as_bytes())
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)?;
    let (_, ledger) = decode_receipt_metadata(&metadata)?;
    Ok((scope, session, state, ledger))
}

async fn pending_artifact_binding(
    txn: &DatabaseTransaction,
    fence: &CaptureCatalogTerminalAttemptFence,
    alias: &str,
    parent_commit: Option<String>,
) -> Result<crate::internal::ai::capture::pending::PendingBinding, CaptureCatalogError> {
    let checkpoint_id = crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
        fence.action.event_id,
        fence.checkpoint_write,
    );
    if fence.checkpoint_write != CheckpointWrite::Committed
        || verify_terminal_attempt_registration_inner(
            txn,
            fence,
            &checkpoint_id,
            &fence.marker_generation,
            false,
        )
        .await?
            != CaptureCatalogTerminalAttemptRegistration::Authorized
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let existing = read_session(txn, &fence.session)
        .await?
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
    let receipt_key = fence.action.completion_receipt_storage_key();
    let receipt = ledger
        .find(&receipt_key)
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    let finalizer = receipt
        .finalizer
        .as_ref()
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    let source = finalizer
        .source_digest
        .clone()
        .ok_or(CaptureCatalogError::InvalidRequest)?;
    Ok(crate::internal::ai::capture::pending::PendingBinding {
        scope: fence.scope.clone(),
        session_id: alias.to_string(),
        checkpoint_id,
        event_id: fence.action.event_id.to_string(),
        action_key: fence.action.action_key().to_string(),
        receipt_key,
        marker_generation: finalizer.marker_generation.clone(),
        source_commitment: source,
        reserved_revision: receipt.reserved_revision,
        original_deadline_millis: finalizer.deadline_millis,
        deferrable: finalizer.mode == StoredFinalizeMode::Deferrable,
        first_attempt_millis: finalizer.first_attempt_millis,
        parent_unborn: parent_commit.is_none(),
        parent_commit,
    })
}

/// Map a same-source retry whose elected marker was never registered. A
/// pending transition owns the same persisted marker/source fence and may
/// retry registration; a durable result is impossible because this caller
/// separately classified an exact durable checkpoint first.
fn terminal_attempt_from_unregistered_same_source_finalizer_result(
    request: &CaptureCatalogFinalizeRequest,
    marker_generation: String,
    source_digest: Option<String>,
    result: CaptureCatalogFinalizeResult,
) -> Result<CaptureCatalogTerminalAttempt, CaptureCatalogError> {
    match result {
        CaptureCatalogFinalizeResult::Pending { attempts, .. } => {
            Ok(CaptureCatalogTerminalAttempt::Bound {
                marker_generation: marker_generation.clone(),
                source_digest: source_digest.clone(),
                attempts,
                registration_fence: Box::new(
                    CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                        request,
                        marker_generation,
                        source_digest,
                    ),
                ),
            })
        }
        CaptureCatalogFinalizeResult::Quarantined { reason } => {
            Ok(CaptureCatalogTerminalAttempt::Quarantined { reason })
        }
        CaptureCatalogFinalizeResult::AlreadyComplete => {
            Ok(CaptureCatalogTerminalAttempt::AlreadyComplete)
        }
        CaptureCatalogFinalizeResult::ConflictUnchanged { conflict } => {
            Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged { conflict })
        }
        CaptureCatalogFinalizeResult::ReadyToComplete { .. } => {
            Err(CaptureCatalogError::InvalidRequest)
        }
    }
}

#[async_trait]
impl CaptureCatalogPort for CaptureCatalogStore {
    async fn apply(
        &self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogApplyResult, CaptureCatalogError> {
        request.session.validate()?;
        request.mutation.validate()?;
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self.apply_inner(&txn, request).await;
        finish_catalog_transaction(txn, result, &request.scope, deadline).await
    }

    async fn complete(
        &self,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
        request.session.validate()?;
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self.complete_inner(&txn, request).await;
        finish_catalog_transaction(txn, result, &request.scope, deadline).await
    }

    async fn finalize(
        &self,
        request: &CaptureCatalogFinalizeRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError> {
        request.session.validate()?;
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self.finalize_inner(&txn, request).await;
        finish_catalog_transaction(txn, result, &request.scope, deadline).await
    }

    async fn claim_terminal_attempt(
        &self,
        request: &CaptureCatalogFinalizeRequest,
        require_source_match: bool,
    ) -> Result<CaptureCatalogTerminalAttempt, CaptureCatalogError> {
        request.session.validate()?;
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self
            .claim_terminal_attempt_inner(&txn, request, require_source_match)
            .await;
        finish_catalog_transaction(txn, result, &request.scope, deadline).await
    }

    async fn prove_durable_replay(
        &self,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError> {
        request.session.validate()?;
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self.prove_durable_replay_inner(&txn, request).await;
        finish_catalog_transaction(txn, result, &request.scope, deadline).await
    }

    async fn complete_durable_replay(
        &self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
        request.session.validate()?;
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self.complete_durable_replay_inner(&txn, request).await;
        finish_catalog_transaction(txn, result, &request.scope, deadline).await
    }

    async fn update_diagnostic(
        &self,
        request: &CaptureCatalogApplyRequest,
        diagnostic: CaptureCatalogDiagnostic,
    ) -> Result<bool, CaptureCatalogError> {
        request.session.validate()?;
        let deadline = self.execution_deadline;
        let txn = begin_catalog_write_transaction(&self.conn, deadline).await?;
        let result = self
            .update_diagnostic_inner(&txn, request, diagnostic)
            .await;
        finish_catalog_transaction(txn, result, &request.scope, deadline).await
    }
}

fn ensure_catalog_deadline(
    deadline: Option<CaptureCommitDeadline>,
) -> Result<(), CaptureCatalogError> {
    if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        return Err(CaptureCatalogError::DeadlineExceeded);
    }
    Ok(())
}

/// Bound only a transaction start or another explicitly read-only preflight.
/// Never use this around catalog DML or a commit acknowledgement: cancelling a
/// future after SQLite has accepted a write leaves the caller unable to prove
/// whether that mutation was dispatched.
async fn run_catalog_begin_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    operation: impl Future<Output = Result<T, CaptureCatalogError>>,
) -> Result<T, CaptureCatalogError> {
    ensure_catalog_deadline(deadline)?;
    let result = match deadline {
        Some(deadline) => tokio::time::timeout_at(
            tokio::time::Instant::from_std(deadline.monotonic()),
            operation,
        )
        .await
        .map_err(|_| CaptureCatalogError::DeadlineExceeded)?,
        None => operation.await,
    };
    ensure_catalog_deadline(deadline)?;
    result
}

async fn begin_catalog_write_transaction(
    conn: &DatabaseConnection,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<DatabaseTransaction, CaptureCatalogError> {
    run_catalog_begin_until(deadline, async {
        db::begin_write_transaction(conn)
            .await
            .map_err(|_| CaptureCatalogError::TransactionStart)
    })
    .await
}

/// Gate each dispatchable catalog mutation after its read-only preflight. The
/// final transaction authorization independently fences a deadline that
/// elapses while SQLite queues or executes the already-started statement.
fn ensure_catalog_mutation_deadline(
    deadline: Option<CaptureCommitDeadline>,
) -> Result<(), CaptureCatalogError> {
    catalog_test_delay_before_dml();
    ensure_catalog_deadline(deadline)
}

async fn finish_catalog_transaction<T>(
    txn: DatabaseTransaction,
    result: Result<T, CaptureCatalogError>,
    scope: &CaptureScope,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<T, CaptureCatalogError> {
    match result {
        Ok(value) => {
            if let Err(error) = ensure_catalog_deadline(deadline) {
                let _ = txn.rollback().await;
                return Err(error);
            }
            // This is deliberately the final SQL operation before the commit
            // acknowledgement. SQLite evaluates both the workspace fence and
            // the immutable ingress deadline in one linearization point, so
            // a deadline that elapsed while the catalog mutation was queued
            // cannot still publish a session or receipt.
            catalog_test_delay_after_final_fence();
            let authorization = authorize_final_capture_commit(Some(scope), &txn, deadline).await;
            if let Err(error) = authorization {
                let _ = txn.rollback().await;
                return Err(match error {
                    CaptureFinalCommitAuthorizationError::DeadlineElapsed => {
                        CaptureCatalogError::DeadlineExceeded
                    }
                    CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected => {
                        CaptureCatalogError::WorkspaceLeaseRejected
                    }
                    CaptureFinalCommitAuthorizationError::Database(_) => {
                        CaptureCatalogError::Database
                    }
                });
            }
            // Do not wrap COMMIT in a timeout. SQLx can dispatch COMMIT before
            // a canceled acknowledgement future is dropped; final
            // authorization above is therefore the deadline boundary, and
            // the acknowledgement must be awaited non-cancellably by the
            // caller's finalization path.
            txn.commit()
                .await
                .map_err(|_| CaptureCatalogError::CommitFailed)?;
            Ok(value)
        }
        Err(error) => {
            let _ = txn.rollback().await;
            Err(error)
        }
    }
}

async fn ensure_catalog_schema(txn: &DatabaseTransaction) -> Result<(), CaptureCatalogError> {
    for table in [
        "agent_session",
        "agent_export_job",
        "agent_import_identity",
        "agent_import_tombstone",
        "agent_capture_incarnation",
    ] {
        let found = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
                [table.into()],
            ))
            .await
            .map_err(|_| CaptureCatalogError::Database)?;
        if found.is_none() {
            return Err(CaptureCatalogError::SchemaUnavailable);
        }
    }
    Ok(())
}

async fn verify_mutable_scope(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
    session: &CaptureCatalogSession,
) -> Result<(), CaptureCatalogError> {
    scope
        .assert_workspace_fence_live(txn)
        .await
        .map_err(classify_workspace_lease_error)?;
    scope
        .assert_provider_session_compatible(txn, session.provider_session_id())
        .await
        .map_err(classify_scope_error)
}

fn classify_scope_error(error: anyhow::Error) -> CaptureCatalogError {
    if error.chain().any(|cause| cause.is::<sea_orm::DbErr>()) {
        CaptureCatalogError::Database
    } else {
        CaptureCatalogError::ScopeRejected
    }
}

fn classify_workspace_lease_error(error: anyhow::Error) -> CaptureCatalogError {
    if error.chain().any(|cause| cause.is::<sea_orm::DbErr>()) {
        CaptureCatalogError::Database
    } else {
        CaptureCatalogError::WorkspaceLeaseRejected
    }
}

fn classify_pending_validation_error(error: anyhow::Error) -> CaptureCatalogError {
    if error.chain().any(|cause| {
        cause.is::<sea_orm::DbErr>()
            || cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(super::pending_identity::transient_io_failure)
            || cause
                .downcast_ref::<crate::internal::workspace::WorkspaceError>()
                .is_some_and(|error| {
                    matches!(
                        error,
                        crate::internal::workspace::WorkspaceError::ReadFailed(_)
                    )
                })
    }) {
        CaptureCatalogError::Database
    } else if error.chain().any(|cause| {
        cause
            .downcast_ref::<crate::internal::workspace::WorkspaceError>()
            .is_some_and(|error| {
                matches!(
                    error,
                    crate::internal::workspace::WorkspaceError::LeaseLost { .. }
                )
            })
            || cause
                .downcast_ref::<
                    crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError,
                >()
                .is_some_and(|error| {
                    matches!(
                        error,
                        crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected
                    )
                })
            || cause
                .downcast_ref::<CaptureCatalogError>()
                .is_some_and(|error| matches!(error, CaptureCatalogError::WorkspaceLeaseRejected))
    }) {
        CaptureCatalogError::WorkspaceLeaseRejected
    } else if error.chain().any(|cause| {
        cause
            .downcast_ref::<
                crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError,
            >()
            .is_some_and(|error| {
                matches!(
                    error,
                    crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed
                )
            })
            || cause
                .downcast_ref::<CaptureCatalogError>()
                .is_some_and(|error| matches!(error, CaptureCatalogError::DeadlineExceeded))
    }) {
        CaptureCatalogError::DeadlineExceeded
    } else if error.chain().any(|cause| {
        cause
            .downcast_ref::<CaptureCatalogError>()
            .is_some_and(|error| {
                matches!(
                    error,
                    CaptureCatalogError::SchemaUnavailable
                )
            })
    }) {
        CaptureCatalogError::SchemaUnavailable
    } else if error.chain().any(|cause| {
        cause
            .downcast_ref::<CaptureCatalogError>()
            .is_some_and(|error| {
                matches!(
                    error,
                    CaptureCatalogError::Database
                        | CaptureCatalogError::TransactionStart
                        | CaptureCatalogError::CommitFailed
                )
            })
    }) {
        CaptureCatalogError::Database
    } else if error
        .chain()
        .any(|cause| cause.is::<super::pending_identity::PendingAliasConflict>())
    {
        // A lost alias mint race rolled back without acknowledging anything.
        CaptureCatalogError::CommitFailed
    } else {
        CaptureCatalogError::InvalidRequest
    }
}

async fn reject_tombstone(
    txn: &DatabaseTransaction,
    session: &CaptureCatalogSession,
) -> Result<(), CaptureCatalogError> {
    let tombstone = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 FROM agent_import_tombstone
             WHERE agent_kind = ? AND provider_session_id = ? LIMIT 1",
            [
                session.agent_kind.clone().into(),
                session.provider_session_id.clone().into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    if tombstone.is_some() {
        return Err(CaptureCatalogError::Tombstoned);
    }
    Ok(())
}

/// Earliest agent kind that claimed this scoped provider session. This must
/// stay in the same write transaction as `apply_inner`: a preflight-only
/// check reopens the first-writer race between adapters.
async fn scoped_provider_owner_kind(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
    session: &CaptureCatalogSession,
) -> Result<Option<String>, CaptureCatalogError> {
    let owner = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT agent_kind FROM agent_session
             WHERE provider_session_id = ? AND scope_state = 'scoped'
               AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
             ORDER BY rowid ASC LIMIT 1",
            [
                session.provider_session_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    owner
        .map(|row| row.try_get_by("agent_kind"))
        .transpose()
        .map_err(|_| CaptureCatalogError::Database)
}

/// The original runtime preserves a post-erasure revision namespace in this
/// existing table. The catalog facade retains that behavior rather than
/// resetting a resurrected provider session back to revision one.
async fn initial_session_revision(
    txn: &DatabaseTransaction,
    session: &CaptureCatalogSession,
) -> Result<(i64, Option<String>), CaptureCatalogError> {
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT next_session_sync_revision, source_namespace
             FROM agent_capture_incarnation
             WHERE agent_kind = ? AND provider_session_id = ?",
            [
                session.agent_kind.clone().into(),
                session.provider_session_id.clone().into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    let Some(row) = row else {
        return Ok((1, None));
    };
    let revision: i64 = row
        .try_get_by("next_session_sync_revision")
        .map_err(|_| CaptureCatalogError::Database)?;
    let namespace: String = row
        .try_get_by("source_namespace")
        .map_err(|_| CaptureCatalogError::Database)?;
    if revision <= 1
        || namespace.len() != 32
        || !namespace.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(CaptureCatalogError::MalformedReceiptLedger);
    }
    Ok((revision, Some(namespace)))
}

#[derive(Clone, Debug)]
struct StoredCatalogSession {
    session_id: String,
    working_dir: String,
    state: DurableCaptureState,
    metadata_json: String,
    redaction_report: String,
    scope_state: String,
    repo_id: Option<String>,
    worktree_id: Option<String>,
    workspace_id: Option<String>,
    workspace_fence: Option<i64>,
}

impl StoredCatalogSession {
    fn durable_state(&self) -> DurableCaptureState {
        self.state
    }

    fn matches_scope(&self, scope: &CaptureScope) -> bool {
        self.scope_state == "scoped"
            && self.repo_id.as_deref() == Some(scope.repo_id.as_str())
            && self.worktree_id.as_deref() == Some(scope.worktree_id.as_str())
            && self.workspace_id == scope.workspace_id
            && self.workspace_fence == scope.workspace_fence
    }
}

async fn read_session(
    txn: &DatabaseTransaction,
    session: &CaptureCatalogSession,
) -> Result<Option<StoredCatalogSession>, CaptureCatalogError> {
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT session_id, working_dir, state, stopped_at, sync_revision, metadata_json,
                    redaction_report,
                    scope_state, repo_id, worktree_id, workspace_id, workspace_fence
             FROM agent_session WHERE agent_kind = ? AND provider_session_id = ? LIMIT 1",
            [
                session.agent_kind.clone().into(),
                session.provider_session_id.clone().into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    let Some(row) = row else {
        return Ok(None);
    };
    let phase: String = row
        .try_get_by("state")
        .map_err(|_| CaptureCatalogError::Database)?;
    let phase = CapturePhase::from_db(&phase).map_err(|_| CaptureCatalogError::InvalidRequest)?;
    Ok(Some(StoredCatalogSession {
        session_id: row
            .try_get_by("session_id")
            .map_err(|_| CaptureCatalogError::Database)?,
        working_dir: row
            .try_get_by("working_dir")
            .map_err(|_| CaptureCatalogError::Database)?,
        state: DurableCaptureState {
            phase,
            stopped_at: row
                .try_get_by("stopped_at")
                .map_err(|_| CaptureCatalogError::Database)?,
            sync_revision: row
                .try_get_by("sync_revision")
                .map_err(|_| CaptureCatalogError::Database)?,
        },
        metadata_json: row
            .try_get_by("metadata_json")
            .map_err(|_| CaptureCatalogError::Database)?,
        redaction_report: row
            .try_get_by("redaction_report")
            .map_err(|_| CaptureCatalogError::Database)?,
        scope_state: row
            .try_get_by("scope_state")
            .map_err(|_| CaptureCatalogError::Database)?,
        repo_id: row
            .try_get_by("repo_id")
            .map_err(|_| CaptureCatalogError::Database)?,
        worktree_id: row
            .try_get_by("worktree_id")
            .map_err(|_| CaptureCatalogError::Database)?,
        workspace_id: row
            .try_get_by("workspace_id")
            .map_err(|_| CaptureCatalogError::Database)?,
        workspace_fence: row
            .try_get_by("workspace_fence")
            .map_err(|_| CaptureCatalogError::Database)?,
    }))
}

/// Revalidate the catalog-owned terminal attempt in the exact write
/// transaction that creates its traces marker. A finalizer claim and marker
/// registration are deliberately separate operations because snapshot/object
/// preparation is expensive; this second fence prevents a writer that lost
/// the receipt in between from publishing stale source bytes.
pub(crate) async fn verify_terminal_attempt_registration(
    txn: &DatabaseTransaction,
    fence: &CaptureCatalogTerminalAttemptFence,
    checkpoint_id: &str,
    marker_generation: &str,
) -> Result<CaptureCatalogTerminalAttemptRegistration, CaptureCatalogError> {
    verify_terminal_attempt_registration_inner(txn, fence, checkpoint_id, marker_generation, true)
        .await
}

async fn verify_terminal_attempt_registration_inner(
    txn: &DatabaseTransaction,
    fence: &CaptureCatalogTerminalAttemptFence,
    checkpoint_id: &str,
    marker_generation: &str,
    consume_manual_registration: bool,
) -> Result<CaptureCatalogTerminalAttemptRegistration, CaptureCatalogError> {
    // Registration spends an already committed manual claim even if an
    // early fence check or the subsequent marker transaction fails. Clones
    // share this one-shot; only the internal binding probe remains read-only.
    if consume_manual_registration && let Some(manual) = &fence.manual_artifact {
        manual
            .registration_consumed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| CaptureCatalogError::InvalidRequest)?;
    }
    if marker_generation != fence.marker_generation {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let expected_checkpoint_id =
        crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
            fence.action.event_id,
            fence.checkpoint_write,
        );
    if checkpoint_id != expected_checkpoint_id {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    verify_mutable_scope(txn, &fence.scope, &fence.session).await?;
    reject_tombstone(txn, &fence.session).await?;
    let Some(existing) = read_session(txn, &fence.session).await? else {
        return Err(CaptureCatalogError::InvalidRequest);
    };
    if !existing.matches_scope(&fence.scope)
        || existing.session_id != fence.session.session_id
        || existing.working_dir != fence.session.working_dir
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let (_, ledger) = decode_receipt_metadata(&existing.metadata_json)?;
    let receipt_key = fence.action.completion_receipt_storage_key();
    let Some(receipt) = ledger.find(&receipt_key) else {
        return Err(CaptureCatalogError::InvalidRequest);
    };
    if !receipt.matches_action(&fence.action)
        || !receipt.is_deferred_terminal()
        || receipt.intent.checkpoint.to_checkpoint_write() != fence.checkpoint_write
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    if receipt.status == StoredReceiptStatus::Complete {
        // A completed terminal receipt is a safe, content-free
        // acknowledgement only when its terminal state was durably
        // published.  Keep malformed ledger/state combinations fail-closed
        // rather than letting a stale writer silently skip an unknown state.
        if existing.state.phase == CapturePhase::Stopped && existing.state.stopped_at.is_some() {
            return Ok(CaptureCatalogTerminalAttemptRegistration::TerminalReceiptAlreadyComplete);
        }
        return Err(CaptureCatalogError::InvalidRequest);
    }
    if receipt.status != StoredReceiptStatus::Pending
        || !receipt.can_resume_from(Some(existing.durable_state()))
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let Some(finalizer) = receipt.finalizer.as_ref() else {
        return Err(CaptureCatalogError::InvalidRequest);
    };
    if finalizer.status != StoredFinalizeStatus::Pending
        || finalizer.is_unbound_snapshot()
        || finalizer.marker_generation != fence.marker_generation
        || finalizer.source_digest != fence.source_digest
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    verify_artifact_registration_authority(txn, fence, checkpoint_id, consume_manual_registration)
        .await?;
    Ok(CaptureCatalogTerminalAttemptRegistration::Authorized)
}

/// A quarantined artifact cannot reuse an earlier automatic election.
/// Manual authority is bound to the committed one-shot header and exact
/// authenticated association, and can register only an absent marker slot.
async fn verify_artifact_registration_authority(
    txn: &DatabaseTransaction,
    fence: &CaptureCatalogTerminalAttemptFence,
    checkpoint_id: &str,
    consume_manual_registration: bool,
) -> Result<(), CaptureCatalogError> {
    let Some(manual) = &fence.manual_artifact else {
        let quarantined = txn.query_one_raw(Statement::from_sql_and_values(txn.get_database_backend(),
            "SELECT 1 FROM metadata_kv WHERE scope='agent_capture_quarantine' AND target=? AND key=? LIMIT 1",
            [fence.scope.repo_id.clone().into(), checkpoint_id.into()],
        )).await.map_err(|_| CaptureCatalogError::Database)?;
        return if quarantined.is_none() {
            Ok(())
        } else {
            Err(CaptureCatalogError::InvalidRequest)
        };
    };
    if !manual.header.manual_attempted
        || manual.header.binding.checkpoint_id != checkpoint_id
        || super::pending::current_header_namespace(txn, &manual.header)
            .await
            .map_err(|_| CaptureCatalogError::InvalidRequest)?
            != crate::internal::metadata::MetadataScope::AgentCaptureQuarantine
        || resolve_pending_session_context(txn, &fence.scope, &fence.session.session_id).await?
            != manual.context
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let alias = super::pending_identity::lookup(
        txn,
        &fence.scope.repo_id,
        &manual.header.binding.session_id,
    )
    .await
    .map_err(|_| CaptureCatalogError::InvalidRequest)?
    .ok_or(CaptureCatalogError::InvalidRequest)?;
    if alias
        .encode()
        .map_err(|_| CaptureCatalogError::InvalidRequest)?
        != manual.alias_record
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    let marker = txn.query_one_raw(Statement::from_sql_and_values(txn.get_database_backend(),
        "SELECT 1 FROM metadata_kv WHERE scope='agent_traces_inflight' AND target=? AND key=? LIMIT 1",
        [fence.session.session_id.clone().into(), checkpoint_id.into()],
    )).await.map_err(|_| CaptureCatalogError::Database)?;
    if marker.is_some() {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    // Fence clones share this process-local one-shot. The durable header bit
    // prevents a replacement capability after restart; a failed registration
    // or later publication still spends the already committed operator claim.
    if !consume_manual_registration && manual.registration_consumed.load(Ordering::Acquire) {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    Ok(())
}

/// Whether the deterministic committed checkpoint for a terminal receipt is
/// durable under the same identity fence. This stays private to the catalog:
/// callers receive only a replay-only outcome, never a durable row they could
/// repurpose for a different source snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TerminalDurableCheckpointStatus {
    Absent,
    Exact,
    Incompatible,
}

async fn terminal_durable_checkpoint_status(
    txn: &DatabaseTransaction,
    session: &CaptureCatalogSession,
    event_id: Uuid,
    checkpoint_write: CheckpointWrite,
) -> Result<TerminalDurableCheckpointStatus, CaptureCatalogError> {
    if checkpoint_write != CheckpointWrite::Committed {
        return Ok(TerminalDurableCheckpointStatus::Incompatible);
    }
    let checkpoint_id = crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
        event_id,
        checkpoint_write,
    );
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT session_id, scope FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
            [checkpoint_id.into()],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    let Some(row) = row else {
        return Ok(TerminalDurableCheckpointStatus::Absent);
    };
    let stored_session: String = row
        .try_get_by("session_id")
        .map_err(|_| CaptureCatalogError::Database)?;
    let stored_scope: String = row
        .try_get_by("scope")
        .map_err(|_| CaptureCatalogError::Database)?;
    if stored_session == session.session_id
        && stored_scope == crate::internal::ai::traces::CheckpointScope::Committed.as_str()
    {
        Ok(TerminalDurableCheckpointStatus::Exact)
    } else {
        Ok(TerminalDurableCheckpointStatus::Incompatible)
    }
}

/// A coverage-confirmed terminal replay may reuse only an already committed
/// checkpoint for the same catalog session. This is intentionally a cheap
/// existence probe rather than a lookup exposed to adapters: coverage has
/// already bound the source turns to a committed checkpoint, and this query
/// prevents a malformed or legacy stopped row from turning that fact into a
/// proofless terminal acknowledgement.
async fn has_committed_checkpoint(
    txn: &DatabaseTransaction,
    session: &CaptureCatalogSession,
) -> Result<bool, CaptureCatalogError> {
    let row = txn
        .query_one_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "SELECT 1 AS present FROM agent_checkpoint \
             WHERE session_id = ? AND scope = 'committed' LIMIT 1",
            [session.session_id.clone().into()],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    Ok(row.is_some())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReceiptLedger {
    version: u8,
    entries: Vec<StoredReceipt>,
}

impl Default for StoredReceiptLedger {
    fn default() -> Self {
        Self {
            version: RECEIPT_LEDGER_VERSION,
            entries: Vec::new(),
        }
    }
}

impl StoredReceiptLedger {
    fn validate(&self) -> Result<(), CaptureCatalogError> {
        if self.version != RECEIPT_LEDGER_VERSION || self.entries.len() > MAX_CAPTURE_RECEIPTS {
            return Err(CaptureCatalogError::MalformedReceiptLedger);
        }
        let mut keys = HashSet::with_capacity(self.entries.len());
        for entry in &self.entries {
            entry.validate()?;
            if !keys.insert(entry.receipt_key.clone()) {
                return Err(CaptureCatalogError::MalformedReceiptLedger);
            }
        }
        Ok(())
    }

    fn find(&self, key: &str) -> Option<&StoredReceipt> {
        self.entries.iter().find(|entry| entry.receipt_key == key)
    }

    fn find_mut(&mut self, key: &str) -> Option<&mut StoredReceipt> {
        self.entries
            .iter_mut()
            .find(|entry| entry.receipt_key == key)
    }

    fn insert(&mut self, entry: StoredReceipt) -> Result<(), CaptureCatalogError> {
        self.validate()?;
        if self.entries.len() == MAX_CAPTURE_RECEIPTS {
            let eviction = self
                .entries
                .iter()
                .enumerate()
                .filter(|(_, existing)| existing.status == StoredReceiptStatus::Complete)
                .min_by_key(|(_, existing)| (existing.recorded_at, existing.receipt_key.as_str()))
                .map(|(index, _)| index)
                .ok_or(CaptureCatalogError::ReceiptCapacityExhausted)?;
            self.entries.remove(eviction);
        }
        self.entries.push(entry);
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReceipt {
    receipt_key: String,
    event_id: String,
    action_key: String,
    intent: StoredReceiptIntent,
    status: StoredReceiptStatus,
    recorded_at: i64,
    /// Revision whose state this pending terminal receipt may publish. A
    /// later lifecycle mutation advances it, fencing this stale finalizer.
    reserved_revision: i64,
    /// Optional because receipts written before ACF-07 remain readable. Once
    /// present, it is the sole durable authority for terminal replay policy.
    #[serde(default)]
    finalizer: Option<StoredFinalizeReceipt>,
}

impl StoredReceipt {
    fn new(
        receipt_key: &str,
        action: &CaptureCatalogAction,
        mutation: &CaptureCatalogMutation,
        status: StoredReceiptStatus,
        reserved_revision: i64,
    ) -> Self {
        Self {
            receipt_key: receipt_key.to_string(),
            event_id: action.event_id.to_string(),
            action_key: action.action_key.as_str().to_string(),
            intent: StoredReceiptIntent::from_mutation(mutation),
            status,
            recorded_at: mutation.observed_at,
            reserved_revision,
            finalizer: None,
        }
    }

    fn validate(&self) -> Result<(), CaptureCatalogError> {
        let event_id = Uuid::parse_str(&self.event_id)
            .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)?;
        if self.action_key != CaptureActionKey::for_event(event_id).as_str() {
            return Err(CaptureCatalogError::MalformedReceiptLedger);
        }
        if !valid_stored_receipt_key(&self.receipt_key, event_id) || self.reserved_revision < 1 {
            return Err(CaptureCatalogError::MalformedReceiptLedger);
        }
        self.intent.validate()?;
        if let Some(finalizer) = &self.finalizer
            && (!self.is_deferred_terminal()
                || finalizer.replay_key != self.action_key
                || finalizer.validate().is_err())
        {
            return Err(CaptureCatalogError::MalformedReceiptLedger);
        }
        Ok(())
    }

    fn matches(&self, action: &CaptureCatalogAction, mutation: &CaptureCatalogMutation) -> bool {
        self.matches_action(action) && self.intent.matches_mutation(mutation)
    }

    fn matches_action(&self, action: &CaptureCatalogAction) -> bool {
        self.event_id == action.event_id.to_string()
            && self.action_key == action.action_key.as_str()
    }

    fn is_deferred_terminal(&self) -> bool {
        self.intent.phase == StoredPhase::Stopped
    }

    fn can_resume_from(&self, current: Option<DurableCaptureState>) -> bool {
        !self.is_deferred_terminal()
            || current.is_some_and(|state| state.sync_revision == self.reserved_revision)
    }
}

/// Versioned, content-free ACF-07 evidence embedded in the existing receipt
/// ledger. It deliberately contains no raw source, path, provider event, or
/// error text, so doctor can classify it without reopening a transcript.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredFinalizeReceipt {
    version: u8,
    replay_key: String,
    marker_generation: String,
    source_digest: Option<String>,
    deadline_millis: Option<i64>,
    mode: StoredFinalizeMode,
    first_attempt_millis: i64,
    attempts: u8,
    stage: StoredFinalizeStage,
    status: StoredFinalizeStatus,
    #[serde(default)]
    quarantine_reason: Option<StoredFinalizeQuarantineReason>,
}

impl StoredFinalizeReceipt {
    fn from_pending(receipt: &PendingFinalizeReceipt) -> Self {
        Self {
            version: FINALIZER_RECEIPT_VERSION,
            replay_key: receipt.replay_key().to_string(),
            marker_generation: receipt.marker_generation().to_string(),
            source_digest: receipt.source_digest().map(str::to_string),
            deadline_millis: receipt.deadline_millis(),
            mode: StoredFinalizeMode::from_mode(receipt.mode()),
            first_attempt_millis: receipt.first_attempt_millis(),
            attempts: receipt.attempts(),
            stage: StoredFinalizeStage::from_stage(receipt.stage()),
            status: StoredFinalizeStatus::Pending,
            quarantine_reason: None,
        }
    }

    fn quarantined(receipt: &PendingFinalizeReceipt, reason: FinalizeQuarantineReason) -> Self {
        let mut stored = Self::from_pending(receipt);
        stored.status = StoredFinalizeStatus::Quarantined;
        stored.quarantine_reason = Some(StoredFinalizeQuarantineReason::from_reason(reason));
        stored
    }

    fn to_pending_receipt(&self) -> Result<PendingFinalizeReceipt, CaptureCatalogError> {
        PendingFinalizeReceipt::restore(
            self.replay_key.clone(),
            self.marker_generation.clone(),
            self.source_digest.clone(),
            self.deadline_millis,
            self.mode.to_mode(),
            self.first_attempt_millis,
            self.attempts,
            self.stage.to_stage(),
        )
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)
    }

    fn to_policy(&self) -> Result<CaptureFinalizePolicy, CaptureCatalogError> {
        CaptureFinalizePolicy::new(
            self.deadline_millis,
            self.mode.to_mode(),
            self.replay_key.clone(),
        )
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)
    }

    /// Whether a doctor replay must no longer leave this receipt pending.
    /// This mirrors the pure finalizer's bounded replay predicate; doctor
    /// passes the same persisted marker/source fence into the actual
    /// transition so this predicate is never itself a state mutation.
    fn budget_exhausted_at(&self, now_millis: i64) -> bool {
        self.attempts >= MAX_FINALIZE_ATTEMPTS
            || now_millis.saturating_sub(self.first_attempt_millis) > MAX_FINALIZE_WINDOW_MILLIS
            || (self.mode == StoredFinalizeMode::Synchronous
                && self
                    .deadline_millis
                    .is_some_and(|deadline| now_millis >= deadline))
    }

    /// A no-repository terminal hook has no checkpoint marker or source to
    /// fence yet. It may be rebound exactly once by the first real checkpoint
    /// attempt; every subsequent receipt uses the ordinary exact fence.
    fn is_unbound_snapshot(&self) -> bool {
        self.status == StoredFinalizeStatus::Pending
            && self
                .marker_generation
                .starts_with(UNBOUND_FINALIZER_MARKER_PREFIX)
            && self.source_digest.is_none()
            && self.stage == StoredFinalizeStage::Snapshot
    }

    fn rebind_unbound_snapshot(
        &self,
        marker_generation: String,
        source_digest: Option<String>,
    ) -> Result<PendingFinalizeReceipt, CaptureCatalogError> {
        if !self.is_unbound_snapshot() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        PendingFinalizeReceipt::restore(
            self.replay_key.clone(),
            marker_generation,
            source_digest,
            self.deadline_millis,
            self.mode.to_mode(),
            self.first_attempt_millis,
            self.attempts,
            self.stage.to_stage(),
        )
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)
    }

    fn completion_proof(&self) -> Option<CaptureCatalogFinalizeProof> {
        (self.status == StoredFinalizeStatus::Pending).then(|| CaptureCatalogFinalizeProof {
            replay_key: self.replay_key.clone(),
            marker_generation: self.marker_generation.clone(),
            source_digest: self.source_digest.clone(),
        })
    }

    fn quarantine_reason(&self) -> Option<FinalizeQuarantineReason> {
        self.quarantine_reason
            .map(StoredFinalizeQuarantineReason::to_reason)
    }

    fn matches_proof(&self, proof: &CaptureCatalogFinalizeProof) -> bool {
        self.status == StoredFinalizeStatus::Pending
            && self.replay_key == proof.replay_key
            && self.marker_generation == proof.marker_generation
            && self.source_digest == proof.source_digest
    }

    fn validate(&self) -> Result<(), ()> {
        if self.version != FINALIZER_RECEIPT_VERSION {
            return Err(());
        }
        self.to_pending_receipt().map_err(|_| ())?;
        match (self.status, self.quarantine_reason) {
            (StoredFinalizeStatus::Pending, None)
            | (StoredFinalizeStatus::Quarantined, Some(_)) => Ok(()),
            _ => Err(()),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredFinalizeMode {
    Synchronous,
    Deferrable,
}

impl StoredFinalizeMode {
    fn from_mode(mode: CaptureFinalizeMode) -> Self {
        match mode {
            CaptureFinalizeMode::Synchronous => Self::Synchronous,
            CaptureFinalizeMode::Deferrable => Self::Deferrable,
        }
    }

    fn to_mode(self) -> CaptureFinalizeMode {
        match self {
            Self::Synchronous => CaptureFinalizeMode::Synchronous,
            Self::Deferrable => CaptureFinalizeMode::Deferrable,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredFinalizeStage {
    Snapshot,
    Marker,
    Checkpoint,
    Cleanup,
}

impl StoredFinalizeStage {
    fn from_stage(stage: FinalizePendingStage) -> Self {
        match stage {
            FinalizePendingStage::Snapshot => Self::Snapshot,
            FinalizePendingStage::Marker => Self::Marker,
            FinalizePendingStage::Checkpoint => Self::Checkpoint,
            FinalizePendingStage::Cleanup => Self::Cleanup,
        }
    }

    fn to_stage(self) -> FinalizePendingStage {
        match self {
            Self::Snapshot => FinalizePendingStage::Snapshot,
            Self::Marker => FinalizePendingStage::Marker,
            Self::Checkpoint => FinalizePendingStage::Checkpoint,
            Self::Cleanup => FinalizePendingStage::Cleanup,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredFinalizeStatus {
    Pending,
    Quarantined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredFinalizeQuarantineReason {
    AttemptLimit,
    WindowLimit,
    SourceDigestConflict,
    MarkerGenerationConflict,
    SynchronousDeadline,
}

impl StoredFinalizeQuarantineReason {
    fn from_reason(reason: FinalizeQuarantineReason) -> Self {
        match reason {
            FinalizeQuarantineReason::AttemptLimit => Self::AttemptLimit,
            FinalizeQuarantineReason::WindowLimit => Self::WindowLimit,
            FinalizeQuarantineReason::SourceDigestConflict => Self::SourceDigestConflict,
            FinalizeQuarantineReason::MarkerGenerationConflict => Self::MarkerGenerationConflict,
            FinalizeQuarantineReason::SynchronousDeadline => Self::SynchronousDeadline,
        }
    }

    fn to_reason(self) -> FinalizeQuarantineReason {
        match self {
            Self::AttemptLimit => FinalizeQuarantineReason::AttemptLimit,
            Self::WindowLimit => FinalizeQuarantineReason::WindowLimit,
            Self::SourceDigestConflict => FinalizeQuarantineReason::SourceDigestConflict,
            Self::MarkerGenerationConflict => FinalizeQuarantineReason::MarkerGenerationConflict,
            Self::SynchronousDeadline => FinalizeQuarantineReason::SynchronousDeadline,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct StoredReceiptIntent {
    phase: StoredPhase,
    stopped_at: StoredStoppedAt,
    checkpoint: StoredCheckpoint,
}

impl StoredReceiptIntent {
    fn from_mutation(mutation: &CaptureCatalogMutation) -> Self {
        Self {
            phase: StoredPhase::from_capture_phase(mutation.next_phase),
            stopped_at: StoredStoppedAt::from_mutation(mutation.stopped_at),
            checkpoint: StoredCheckpoint::from_checkpoint_write(mutation.checkpoint),
        }
    }

    fn validate(self) -> Result<(), CaptureCatalogError> {
        match (self.phase, self.stopped_at) {
            (StoredPhase::Stopped, StoredStoppedAt::Set { .. })
            | (
                StoredPhase::Pending | StoredPhase::Active | StoredPhase::Condensed,
                StoredStoppedAt::Preserve,
            ) => Ok(()),
            _ => Err(CaptureCatalogError::MalformedReceiptLedger),
        }?;
        if self.phase == StoredPhase::Stopped && self.checkpoint == StoredCheckpoint::None {
            return Err(CaptureCatalogError::MalformedReceiptLedger);
        }
        Ok(())
    }

    fn matches_mutation(self, mutation: &CaptureCatalogMutation) -> bool {
        self.phase == StoredPhase::from_capture_phase(mutation.next_phase)
            && self.stopped_at.same_mode(mutation.stopped_at)
            && self.checkpoint == StoredCheckpoint::from_checkpoint_write(mutation.checkpoint)
    }

    fn terminal_timestamp(self) -> Option<i64> {
        match self.stopped_at {
            StoredStoppedAt::Set { timestamp } => Some(timestamp),
            StoredStoppedAt::Preserve => None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredReceiptStatus {
    Pending,
    Complete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredPhase {
    Pending,
    Active,
    Condensed,
    Stopped,
}

impl StoredPhase {
    fn from_capture_phase(phase: CapturePhase) -> Self {
        match phase {
            CapturePhase::Pending => Self::Pending,
            CapturePhase::Active => Self::Active,
            CapturePhase::Condensed => Self::Condensed,
            CapturePhase::Stopped => Self::Stopped,
            CapturePhase::Quarantined => {
                // A receipt is never created for a quarantine state; input
                // validation rejects it before this conversion is reachable.
                Self::Pending
            }
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredStoppedAt {
    Preserve,
    Set { timestamp: i64 },
}

impl StoredStoppedAt {
    fn from_mutation(mutation: StoppedAtMutation) -> Self {
        match mutation {
            StoppedAtMutation::Preserve => Self::Preserve,
            StoppedAtMutation::Set(timestamp) => Self::Set { timestamp },
        }
    }

    fn same_mode(self, mutation: StoppedAtMutation) -> bool {
        matches!(
            (self, mutation),
            (Self::Preserve, StoppedAtMutation::Preserve)
                | (Self::Set { .. }, StoppedAtMutation::Set(_))
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum StoredCheckpoint {
    None,
    Committed,
    SubagentBoundary,
}

impl StoredCheckpoint {
    fn from_checkpoint_write(checkpoint: CheckpointWrite) -> Self {
        match checkpoint {
            CheckpointWrite::None => Self::None,
            CheckpointWrite::Committed => Self::Committed,
            CheckpointWrite::SubagentBoundary => Self::SubagentBoundary,
        }
    }

    fn to_checkpoint_write(self) -> CheckpointWrite {
        match self {
            Self::None => CheckpointWrite::None,
            Self::Committed => CheckpointWrite::Committed,
            Self::SubagentBoundary => CheckpointWrite::SubagentBoundary,
        }
    }
}

fn valid_stored_receipt_key(value: &str, event_id: Uuid) -> bool {
    OpaqueCaptureReceiptKey::parse(value.to_string()).is_ok()
        || value == format!("{ACTION_RECEIPT_PREFIX}{event_id}")
}

/// Reconstruct the exact action identity from an existing ledger entry.
/// Doctor recovery must retain an ingress receipt key when one was present;
/// replacing it with the fallback action key would address a different
/// receipt and could otherwise make a durable checkpoint look unrecoverable.
fn action_from_stored_receipt(
    receipt: &StoredReceipt,
) -> Result<CaptureCatalogAction, CaptureCatalogError> {
    let event_id = Uuid::parse_str(&receipt.event_id)
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)?;
    let fallback = format!("{ACTION_RECEIPT_PREFIX}{event_id}");
    let receipt_key = if receipt.receipt_key == fallback {
        None
    } else {
        Some(OpaqueCaptureReceiptKey::parse(receipt.receipt_key.clone())?)
    };
    let action = CaptureCatalogAction::lifecycle(event_id, receipt_key);
    if action.action_key.as_str() != receipt.action_key {
        return Err(CaptureCatalogError::MalformedReceiptLedger);
    }
    Ok(action)
}

fn decode_receipt_metadata(
    metadata_json: &str,
) -> Result<
    (
        serde_json::Map<String, serde_json::Value>,
        StoredReceiptLedger,
    ),
    CaptureCatalogError,
> {
    let value: serde_json::Value = serde_json::from_str(metadata_json)
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)?;
    let serde_json::Value::Object(map) = value else {
        return Err(CaptureCatalogError::MalformedReceiptLedger);
    };
    let ledger = match map.get(RECEIPT_LEDGER_FIELD) {
        Some(value) => serde_json::from_value(value.clone())
            .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)?,
        None => StoredReceiptLedger::default(),
    };
    ledger.validate()?;
    Ok((map, ledger))
}

fn encode_receipt_metadata(
    mut metadata: serde_json::Map<String, serde_json::Value>,
    ledger: &StoredReceiptLedger,
) -> Result<String, CaptureCatalogError> {
    ledger.validate()?;
    if ledger.entries.is_empty() {
        metadata.remove(RECEIPT_LEDGER_FIELD);
    } else {
        metadata.insert(
            RECEIPT_LEDGER_FIELD.to_string(),
            serde_json::to_value(ledger)
                .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)?,
        );
    }
    serde_json::to_string(&serde_json::Value::Object(metadata))
        .map_err(|_| CaptureCatalogError::MalformedReceiptLedger)
}

fn validate_catalog_text(value: &str, max_bytes: usize) -> Result<(), CaptureCatalogError> {
    if value.is_empty()
        || value.len() > max_bytes
        || value
            .chars()
            .any(|character| character.is_control() || character == '\0')
    {
        return Err(CaptureCatalogError::InvalidRequest);
    }
    Ok(())
}

fn import_session_ownership_fingerprint(
    session_id: &str,
    agent_kind: &str,
    provider_session_id: &str,
    working_dir: &str,
    metadata_json: &str,
) -> String {
    let mut digest = Sha256::new();
    for value in [
        session_id,
        agent_kind,
        provider_session_id,
        working_dir,
        metadata_json,
    ] {
        digest.update((value.len() as u64).to_be_bytes());
        digest.update(value.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn import_redaction_report_json(
    report: &CaptureCatalogRedactionReport,
) -> Result<String, CaptureCatalogError> {
    let report: serde_json::Value =
        serde_json::from_str(&report.json()?).map_err(|_| CaptureCatalogError::InvalidRequest)?;
    serde_json::to_string(&serde_json::json!({ "import": report }))
        .map_err(|_| CaptureCatalogError::InvalidRequest)
}

fn stage_for_progress(progress: FinalizeCheckpointProgress) -> FinalizePendingStage {
    match progress {
        FinalizeCheckpointProgress::NotStarted => FinalizePendingStage::Snapshot,
        FinalizeCheckpointProgress::Retryable(stage) => stage,
        FinalizeCheckpointProgress::PendingCleanup => FinalizePendingStage::Cleanup,
        // A durable result without a prior receipt is rejected by the pure
        // finalizer before this helper is reached. Keep a harmless stage here
        // to make the helper total and avoid a hidden panic in recovery code.
        FinalizeCheckpointProgress::Durable => FinalizePendingStage::Checkpoint,
    }
}

async fn update_finalizer_metadata(
    txn: &DatabaseTransaction,
    scope: &CaptureScope,
    session: &CaptureCatalogSession,
    expected_revision: i64,
    metadata_json: String,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<bool, CaptureCatalogError> {
    ensure_catalog_mutation_deadline(deadline)?;
    let updated = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_session SET metadata_json = ?
             WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND sync_revision = ?",
            [
                metadata_json.into(),
                session.agent_kind.clone().into(),
                session.provider_session_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.worktree_id.clone().into(),
                scope.workspace_id.clone().into(),
                scope.workspace_fence.into(),
                expected_revision.into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    Ok(updated.rows_affected() == 1)
}

async fn quarantine_finalizer_session(
    txn: &DatabaseTransaction,
    request: &CaptureCatalogFinalizeRequest,
    expected_revision: i64,
    next_revision: i64,
    metadata_json: String,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<bool, CaptureCatalogError> {
    ensure_catalog_mutation_deadline(deadline)?;
    let updated = txn
        .execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "UPDATE agent_session
             SET state = 'quarantined', stopped_at = NULL,
                 last_event_at = MAX(last_event_at, ?), sync_revision = ?, metadata_json = ?
             WHERE agent_kind = ? AND provider_session_id = ?
               AND scope_state = 'scoped' AND repo_id = ? AND worktree_id = ?
               AND workspace_id IS ? AND workspace_fence IS ?
               AND sync_revision = ?",
            [
                request.now_millis.into(),
                next_revision.into(),
                metadata_json.into(),
                request.session.agent_kind.clone().into(),
                request.session.provider_session_id.clone().into(),
                request.scope.repo_id.clone().into(),
                request.scope.worktree_id.clone().into(),
                request.scope.workspace_id.clone().into(),
                request.scope.workspace_fence.into(),
                expected_revision.into(),
            ],
        ))
        .await
        .map_err(|_| CaptureCatalogError::Database)?;
    Ok(updated.rows_affected() == 1)
}

/// In-memory port used by coordinator unit tests. The fault queue is applied
/// before any mutation, making it possible to prove that a simulated lost
/// fence or commit failure never leaks a partially written receipt.
#[allow(dead_code)] // Test/future coordinator seam; production uses CaptureCatalogStore.
#[derive(Clone, Default)]
pub(crate) struct FakeCaptureCatalogStore {
    state: Arc<Mutex<FakeCatalogState>>,
}

#[allow(dead_code)]
#[derive(Default)]
struct FakeCatalogState {
    sessions: Vec<FakeCatalogSession>,
    faults: VecDeque<CaptureCatalogFault>,
    /// The fake checkpoint boundary cannot publish a real metadata marker,
    /// so coordinator tests explicitly record the terminal marker that has
    /// crossed registration. This preserves the production distinction
    /// between a live elected source and an unregistered, unrecoverable one.
    /// MetadataKv has one value per `(session_id, checkpoint_id)` key. Keep
    /// the fake's registration state single-slot too: a later foreign marker
    /// replaces the prior generation and is therefore incompatible with the
    /// catalog's elected attempt, rather than making both generations appear
    /// registered simultaneously.
    terminal_attempt_markers: HashMap<(String, String), String>,
    /// A terminal checkpoint whose ref/catalog transaction committed and
    /// whose ordinary marker has already been retired. This is separate from
    /// the marker set so changed-source replay follows SQLite's replay-only
    /// branch rather than treating a durable writer as unregistered.
    durable_terminal_checkpoints: HashSet<(String, String)>,
}

#[allow(dead_code)]
struct FakeCatalogSession {
    scope: CaptureScope,
    session: CaptureCatalogSession,
    state: DurableCaptureState,
    ledger: StoredReceiptLedger,
}

/// Deterministic fault injection for the port boundary.
#[allow(dead_code)] // Exposed for deterministic catalog fault tests.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCatalogFault {
    Conflict,
    LostFence,
    CommitFailure,
}

#[allow(dead_code)]
impl FakeCaptureCatalogStore {
    pub(crate) fn enqueue_fault(
        &self,
        fault: CaptureCatalogFault,
    ) -> Result<(), CaptureCatalogError> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        state.faults.push_back(fault);
        Ok(())
    }

    /// Record that the fake checkpoint port registered this exact terminal
    /// marker. A changed-source duplicate may observe it but must not become
    /// a writer. Tests deliberately call this only after simulating marker
    /// registration; the default absence follows production's bounded
    /// repair path for a bind-before-registration crash.
    pub(crate) fn mark_terminal_attempt_registered(
        &self,
        session_id: &str,
        action: &CaptureCatalogAction,
        checkpoint_write: CheckpointWrite,
        marker_generation: &str,
    ) -> Result<(), CaptureCatalogError> {
        if checkpoint_write == CheckpointWrite::None || marker_generation.is_empty() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                action.event_id,
                checkpoint_write,
            );
        let mut state = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        state.terminal_attempt_markers.insert(
            (session_id.to_string(), checkpoint_id),
            marker_generation.to_string(),
        );
        Ok(())
    }

    /// Record a committed terminal checkpoint after the fake writer retired
    /// its ordinary marker. Keeping this explicit lets unit tests model the
    /// post-write/pre-receipt-completion crash window without pretending that
    /// a live marker still exists.
    pub(crate) fn mark_terminal_checkpoint_durable(
        &self,
        session_id: &str,
        action: &CaptureCatalogAction,
        checkpoint_write: CheckpointWrite,
    ) -> Result<(), CaptureCatalogError> {
        if checkpoint_write != CheckpointWrite::Committed {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                action.event_id,
                checkpoint_write,
            );
        let mut state = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        state
            .durable_terminal_checkpoints
            .insert((session_id.to_string(), checkpoint_id));
        Ok(())
    }

    fn take_fault(
        state: &mut FakeCatalogState,
    ) -> Result<Option<CaptureCatalogApplyResult>, CaptureCatalogError> {
        match state.faults.pop_front() {
            None => Ok(None),
            Some(CaptureCatalogFault::Conflict) => {
                Ok(Some(CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                }))
            }
            Some(CaptureCatalogFault::LostFence) => Err(CaptureCatalogError::ScopeRejected),
            Some(CaptureCatalogFault::CommitFailure) => Err(CaptureCatalogError::CommitFailed),
        }
    }
}

#[allow(dead_code)]
#[async_trait]
impl CaptureCatalogPort for FakeCaptureCatalogStore {
    async fn apply(
        &self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogApplyResult, CaptureCatalogError> {
        request.session.validate()?;
        request.mutation.validate()?;
        let mut catalog = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        if let Some(result) = Self::take_fault(&mut catalog)? {
            return Ok(result);
        }
        if catalog.sessions.iter().any(|stored| {
            stored.session.provider_session_id == request.session.provider_session_id
                && stored.scope != request.scope
        }) {
            return Err(CaptureCatalogError::ScopeRejected);
        }
        // Mirror the production transaction's rowid-first owner claim while
        // preserving SessionStart/TurnStart's state-only exemptions. Vector
        // insertion order is the fake store's durable rowid order.
        if !request.action.owner_claim_exempt()
            && let Some(owner) = catalog.sessions.iter().find(|stored| {
                stored.scope == request.scope
                    && stored.session.provider_session_id == request.session.provider_session_id
            })
            && owner.session.agent_kind != request.session.agent_kind
        {
            return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let existing_index = catalog.sessions.iter().position(|stored| {
            stored.scope == request.scope
                && stored.session.agent_kind == request.session.agent_kind
                && stored.session.provider_session_id == request.session.provider_session_id
        });
        if let Some(index) = existing_index {
            let existing = &mut catalog.sessions[index];
            if existing.session.session_id != request.session.session_id
                || existing.session.working_dir != request.session.working_dir
            {
                return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::SessionIdentity,
                });
            }
            // Keep the fake port semantically identical to the transactional
            // catalog: an ID-less terminal redelivery must resume the one
            // current local pending receipt rather than minting a new action.
            // This also lets runtime fault-injection tests exercise the same
            // revision fence as production.
            if request.action.lifecycle_kind == Some(LifecycleEventKind::SessionEnd)
                && request.mutation.is_terminal()
                && request.action.receipt_key.is_none()
            {
                let mut candidates = existing.ledger.entries.iter().filter(|receipt| {
                    receipt.status == StoredReceiptStatus::Pending
                        && receipt.is_deferred_terminal()
                        && receipt.receipt_key
                            == format!("{ACTION_RECEIPT_PREFIX}{}", receipt.event_id)
                        && receipt.can_resume_from(Some(existing.state))
                });
                if let Some(receipt) = candidates.next() {
                    if candidates.next().is_some() {
                        return Err(CaptureCatalogError::MalformedReceiptLedger);
                    }
                    let adopted_action = action_from_stored_receipt(receipt)?
                        .with_lifecycle_kind(LifecycleEventKind::SessionEnd);
                    let finalizer = receipt.finalizer.as_ref();
                    let unbound = finalizer.is_some_and(StoredFinalizeReceipt::is_unbound_snapshot);
                    return Ok(CaptureCatalogApplyResult::ResumePending {
                        state: existing.state,
                        checkpoint: receipt.intent.checkpoint.to_checkpoint_write(),
                        terminal_finalizer_needs_binding: finalizer.is_none() || unbound,
                        terminal_marker_generation: finalizer
                            .filter(|finalizer| !finalizer.is_unbound_snapshot())
                            .map(|finalizer| finalizer.marker_generation.clone()),
                        adopted_action: Some(adopted_action),
                    });
                }
            }
            let receipt_key = request
                .action
                .receipt_storage_key(request.mutation.is_terminal());
            if let Some(key) = receipt_key.as_deref()
                && let Some(receipt) = existing.ledger.find(key)
            {
                if !receipt.matches(&request.action, &request.mutation) {
                    return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::ActionMismatch,
                    });
                }
                if receipt.status == StoredReceiptStatus::Pending
                    && receipt.is_deferred_terminal()
                    && !receipt.can_resume_from(Some(existing.state))
                {
                    return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::ConditionalWrite,
                    });
                }
                return Ok(match receipt.status {
                    StoredReceiptStatus::Pending => {
                        let finalizer = receipt.finalizer.as_ref();
                        let unbound =
                            finalizer.is_some_and(StoredFinalizeReceipt::is_unbound_snapshot);
                        CaptureCatalogApplyResult::ResumePending {
                            state: existing.state,
                            checkpoint: receipt.intent.checkpoint.to_checkpoint_write(),
                            terminal_finalizer_needs_binding: receipt.is_deferred_terminal()
                                && (finalizer.is_none() || unbound),
                            terminal_marker_generation: finalizer
                                .filter(|finalizer| !finalizer.is_unbound_snapshot())
                                .map(|finalizer| finalizer.marker_generation.clone()),
                            adopted_action: None,
                        }
                    }
                    StoredReceiptStatus::Complete => CaptureCatalogApplyResult::AlreadyApplied,
                });
            }
            if Some(existing.state) != request.mutation.expected {
                return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ExpectedState,
                });
            }
            let next_state = if request.mutation.is_terminal() {
                DurableCaptureState {
                    phase: existing.state.phase,
                    stopped_at: existing.state.stopped_at,
                    sync_revision: existing
                        .state
                        .sync_revision
                        .checked_add(1)
                        .ok_or(CaptureCatalogError::InvalidRequest)?,
                }
            } else {
                DurableCaptureState {
                    phase: request.mutation.next_phase,
                    stopped_at: existing.state.stopped_at,
                    sync_revision: existing
                        .state
                        .sync_revision
                        .checked_add(1)
                        .ok_or(CaptureCatalogError::InvalidRequest)?,
                }
            };
            let receipt = insert_fake_receipt(existing, request, next_state.sync_revision)?;
            existing.state = next_state;
            return Ok(CaptureCatalogApplyResult::Applied {
                state: next_state,
                checkpoint: request.mutation.checkpoint,
                receipt,
            });
        }
        if request.mutation.expected.is_some() {
            return Ok(CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ExpectedState,
            });
        }
        let mut new_session = FakeCatalogSession {
            scope: request.scope.clone(),
            session: request.session.clone(),
            state: if request.mutation.is_terminal() {
                DurableCaptureState {
                    phase: CapturePhase::Pending,
                    stopped_at: None,
                    sync_revision: 1,
                }
            } else {
                DurableCaptureState {
                    phase: request.mutation.next_phase,
                    stopped_at: None,
                    sync_revision: 1,
                }
            },
            ledger: StoredReceiptLedger::default(),
        };
        let receipt = insert_fake_receipt(&mut new_session, request, 1)?;
        let state = new_session.state;
        catalog.sessions.push(new_session);
        Ok(CaptureCatalogApplyResult::Applied {
            state,
            checkpoint: request.mutation.checkpoint,
            receipt,
        })
    }

    async fn complete(
        &self,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
        let mut catalog = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        match catalog.faults.pop_front() {
            Some(CaptureCatalogFault::Conflict) => {
                return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                });
            }
            Some(CaptureCatalogFault::LostFence) => return Err(CaptureCatalogError::ScopeRejected),
            Some(CaptureCatalogFault::CommitFailure) => {
                return Err(CaptureCatalogError::CommitFailed);
            }
            None => {}
        }
        let has_committed_checkpoint = catalog
            .durable_terminal_checkpoints
            .iter()
            .any(|(session_id, _)| session_id == &request.session.session_id);
        let Some(existing) = catalog.sessions.iter_mut().find(|stored| {
            stored.scope == request.scope
                && stored.session.agent_kind == request.session.agent_kind
                && stored.session.provider_session_id == request.session.provider_session_id
        }) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if existing.session.session_id != request.session.session_id
            || existing.session.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let key = request.action.completion_receipt_storage_key();
        let Some(receipt) = existing.ledger.find(&key) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogCompleteResult::AlreadyComplete);
        }
        let deferred_terminal = receipt.is_deferred_terminal();
        let covered_terminal_replay = matches!(
            &request.completion,
            CaptureCatalogCompletion::CoveredTerminalReplay
        );
        if deferred_terminal {
            match &request.completion {
                CaptureCatalogCompletion::Finalizer(proof) => {
                    let Some(finalizer) = receipt.finalizer.as_ref() else {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    };
                    if !finalizer.matches_proof(proof) {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    }
                }
                CaptureCatalogCompletion::CoveredTerminalReplay => {
                    let Some(finalizer) = receipt.finalizer.as_ref() else {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    };
                    if !finalizer.is_unbound_snapshot()
                        || existing.state.phase != CapturePhase::Stopped
                        || existing.state.stopped_at.is_none()
                        || !has_committed_checkpoint
                    {
                        return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::FinalizerFence,
                        });
                    }
                }
                CaptureCatalogCompletion::Ordinary => {
                    return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::FinalizerFence,
                    });
                }
            }
        } else if !matches!(&request.completion, CaptureCatalogCompletion::Ordinary) {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let reserved_revision = receipt.reserved_revision;
        let terminal_timestamp = if deferred_terminal && !covered_terminal_replay {
            receipt
                .intent
                .terminal_timestamp()
                .ok_or(CaptureCatalogError::MalformedReceiptLedger)?
        } else {
            0
        };
        if deferred_terminal && !receipt.can_resume_from(Some(existing.state)) {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        let Some(receipt) = existing.ledger.find_mut(&key) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        receipt.status = StoredReceiptStatus::Complete;
        if deferred_terminal && !covered_terminal_replay {
            existing.state = DurableCaptureState {
                phase: CapturePhase::Stopped,
                stopped_at: Some(terminal_timestamp),
                sync_revision: reserved_revision
                    .checked_add(1)
                    .ok_or(CaptureCatalogError::InvalidRequest)?,
            };
        }
        Ok(CaptureCatalogCompleteResult::Completed)
    }

    async fn finalize(
        &self,
        request: &CaptureCatalogFinalizeRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError> {
        request.session.validate()?;
        let mut catalog = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        match catalog.faults.pop_front() {
            Some(CaptureCatalogFault::Conflict) => {
                return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                });
            }
            Some(CaptureCatalogFault::LostFence) => return Err(CaptureCatalogError::ScopeRejected),
            Some(CaptureCatalogFault::CommitFailure) => {
                return Err(CaptureCatalogError::CommitFailed);
            }
            None => {}
        }
        let Some(existing) = catalog.sessions.iter_mut().find(|stored| {
            stored.scope == request.scope
                && stored.session.agent_kind == request.session.agent_kind
                && stored.session.provider_session_id == request.session.provider_session_id
        }) else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if existing.session.session_id != request.session.session_id
            || existing.session.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let key = request.action.completion_receipt_storage_key();
        let Some(stored_receipt) = existing.ledger.find(&key) else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !stored_receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if !stored_receipt.is_deferred_terminal() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if stored_receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogFinalizeResult::AlreadyComplete);
        }
        if !stored_receipt.can_resume_from(Some(existing.state)) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        let persisted_finalizer = stored_receipt.finalizer.clone();
        if let Some(finalizer) = persisted_finalizer.as_ref()
            && finalizer.status == StoredFinalizeStatus::Quarantined
        {
            return Ok(CaptureCatalogFinalizeResult::Quarantined {
                reason: finalizer
                    .quarantine_reason()
                    .ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
            });
        }
        if existing.state.phase == CapturePhase::Quarantined {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        if let Some(finalizer) = persisted_finalizer.as_ref()
            && finalizer.is_unbound_snapshot()
            && matches!(request.checkpoint, FinalizeCheckpointProgress::NotStarted)
            && finalizer.marker_generation == request.marker_generation
            && request.source_digest.is_none()
        {
            return Ok(CaptureCatalogFinalizeResult::Pending {
                attempts: finalizer.attempts,
                stage: finalizer.stage.to_stage(),
            });
        }
        let current_receipt = match persisted_finalizer.as_ref() {
            Some(finalizer)
                if finalizer.is_unbound_snapshot()
                    && matches!(request.checkpoint, FinalizeCheckpointProgress::NotStarted)
                    && !request
                        .marker_generation
                        .starts_with(UNBOUND_FINALIZER_MARKER_PREFIX) =>
            {
                Some(finalizer.rebind_unbound_snapshot(
                    request.marker_generation.clone(),
                    request.source_digest.clone(),
                )?)
            }
            Some(finalizer) => Some(finalizer.to_pending_receipt()?),
            None => None,
        };
        let effective_policy = persisted_finalizer
            .as_ref()
            .map(StoredFinalizeReceipt::to_policy)
            .transpose()?
            .unwrap_or_else(|| request.policy.clone());
        let decision = decide_finalization(FinalizeDecisionInput {
            policy: &effective_policy,
            current_receipt: current_receipt.as_ref(),
            marker_generation: &request.marker_generation,
            source_digest: request.source_digest.as_deref(),
            now_millis: request.now_millis,
            checkpoint: request.checkpoint,
        })
        .map_err(|_| CaptureCatalogError::InvalidRequest)?;
        match decision {
            FinalizeDecision::CommitTerminal => {
                let proof = persisted_finalizer
                    .as_ref()
                    .and_then(StoredFinalizeReceipt::completion_proof)
                    .ok_or(CaptureCatalogError::MalformedReceiptLedger)?;
                Ok(CaptureCatalogFinalizeResult::ReadyToComplete { proof })
            }
            FinalizeDecision::PersistPending(pending) => {
                let Some(stored_receipt) = existing.ledger.find_mut(&key) else {
                    return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::MissingReceipt,
                    });
                };
                stored_receipt.finalizer = Some(StoredFinalizeReceipt::from_pending(&pending));
                Ok(CaptureCatalogFinalizeResult::Pending {
                    attempts: pending.attempts(),
                    stage: pending.stage(),
                })
            }
            FinalizeDecision::Quarantine { reason } => {
                let pending = match current_receipt {
                    Some(receipt) => receipt,
                    None => PendingFinalizeReceipt::new(
                        &effective_policy,
                        request.marker_generation.clone(),
                        request.source_digest.clone(),
                        request.now_millis,
                        stage_for_progress(request.checkpoint),
                    )
                    .map_err(|_| CaptureCatalogError::InvalidRequest)?,
                };
                let Some(stored_receipt) = existing.ledger.find_mut(&key) else {
                    return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::MissingReceipt,
                    });
                };
                stored_receipt.finalizer =
                    Some(StoredFinalizeReceipt::quarantined(&pending, reason));
                existing.state = DurableCaptureState {
                    phase: CapturePhase::Quarantined,
                    stopped_at: None,
                    sync_revision: existing
                        .state
                        .sync_revision
                        .checked_add(1)
                        .ok_or(CaptureCatalogError::InvalidRequest)?,
                };
                Ok(CaptureCatalogFinalizeResult::Quarantined { reason })
            }
        }
    }

    async fn claim_terminal_attempt(
        &self,
        request: &CaptureCatalogFinalizeRequest,
        require_source_match: bool,
    ) -> Result<CaptureCatalogTerminalAttempt, CaptureCatalogError> {
        request.session.validate()?;
        if !matches!(request.checkpoint, FinalizeCheckpointProgress::NotStarted) {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        let mut catalog = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        match catalog.faults.pop_front() {
            Some(CaptureCatalogFault::Conflict) => {
                return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                });
            }
            Some(CaptureCatalogFault::LostFence) => return Err(CaptureCatalogError::ScopeRejected),
            Some(CaptureCatalogFault::CommitFailure) => {
                return Err(CaptureCatalogError::CommitFailed);
            }
            None => {}
        }
        // Keep a snapshot of the fake marker registry before borrowing a
        // session mutably below. The real implementation observes this in
        // its SQLite writer transaction; the mutex gives the fake the same
        // atomicity boundary.
        let registered_terminal_markers = catalog.terminal_attempt_markers.clone();
        let durable_terminal_checkpoints = catalog.durable_terminal_checkpoints.clone();
        let Some(existing) = catalog.sessions.iter_mut().find(|stored| {
            stored.scope == request.scope
                && stored.session.agent_kind == request.session.agent_kind
                && stored.session.provider_session_id == request.session.provider_session_id
        }) else {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if existing.session.session_id != request.session.session_id
            || existing.session.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let key = request.action.completion_receipt_storage_key();
        let Some(stored_receipt) = existing.ledger.find(&key).cloned() else {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !stored_receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if !stored_receipt.is_deferred_terminal() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if stored_receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogTerminalAttempt::AlreadyComplete);
        }
        if !stored_receipt.can_resume_from(Some(existing.state)) {
            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        let persisted = stored_receipt.finalizer.clone();
        if let Some(finalizer) = persisted.as_ref() {
            if finalizer.status == StoredFinalizeStatus::Quarantined {
                return Ok(CaptureCatalogTerminalAttempt::Quarantined {
                    reason: finalizer
                        .quarantine_reason()
                        .ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
                });
            }
            if !finalizer.is_unbound_snapshot() {
                // Keep the fake's mutex-serialized election aligned with the
                // SQLite implementation. A same-source process that died
                // before marker registration must advance its persisted retry
                // accounting on every later delivery.
                if require_source_match && finalizer.source_digest == request.source_digest {
                    let checkpoint_id =
                        crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                            request.action.event_id,
                            request.checkpoint_write,
                        );
                    let marker_slot = (request.session.session_id.clone(), checkpoint_id);
                    match registered_terminal_markers.get(&marker_slot) {
                        Some(generation) if generation == &finalizer.marker_generation => {
                            // The real store observes this as RegisteredExact
                            // and leaves a concurrent same-generation writer
                            // alone.
                        }
                        Some(_) => {
                            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                conflict: CaptureCatalogConflict::FinalizerFence,
                            });
                        }
                        None if durable_terminal_checkpoints.contains(&marker_slot) => {
                            // Match the durable-first finalizer contract: an
                            // already committed checkpoint is replay-only,
                            // not a retry-budget quarantine.
                            return Ok(CaptureCatalogTerminalAttempt::DurableReplay);
                        }
                        None => {
                            let pending = finalizer.to_pending_receipt()?;
                            let policy = finalizer.to_policy()?;
                            let decision = decide_finalization(FinalizeDecisionInput {
                                policy: &policy,
                                current_receipt: Some(&pending),
                                marker_generation: &finalizer.marker_generation,
                                source_digest: request.source_digest.as_deref(),
                                now_millis: request.now_millis,
                                checkpoint: request.checkpoint,
                            })
                            .map_err(|_| CaptureCatalogError::InvalidRequest)?;
                            match decision {
                                FinalizeDecision::PersistPending(next_pending) => {
                                    let marker_generation =
                                        next_pending.marker_generation().to_string();
                                    let source_digest =
                                        next_pending.source_digest().map(str::to_string);
                                    let attempts = next_pending.attempts();
                                    let Some(receipt) = existing.ledger.find_mut(&key) else {
                                        return Ok(
                                            CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                                conflict: CaptureCatalogConflict::MissingReceipt,
                                            },
                                        );
                                    };
                                    receipt.finalizer =
                                        Some(StoredFinalizeReceipt::from_pending(&next_pending));
                                    return Ok(CaptureCatalogTerminalAttempt::Bound {
                                        marker_generation: marker_generation.clone(),
                                        source_digest: source_digest.clone(),
                                        attempts,
                                        registration_fence: Box::new(
                                            CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                                                request,
                                                marker_generation,
                                                source_digest,
                                            ),
                                        ),
                                    });
                                }
                                FinalizeDecision::Quarantine { reason } => {
                                    let Some(receipt) = existing.ledger.find_mut(&key) else {
                                        return Ok(
                                            CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                                conflict: CaptureCatalogConflict::MissingReceipt,
                                            },
                                        );
                                    };
                                    receipt.finalizer =
                                        Some(StoredFinalizeReceipt::quarantined(&pending, reason));
                                    existing.state = DurableCaptureState {
                                        phase: CapturePhase::Quarantined,
                                        stopped_at: None,
                                        sync_revision: existing
                                            .state
                                            .sync_revision
                                            .checked_add(1)
                                            .ok_or(CaptureCatalogError::InvalidRequest)?,
                                    };
                                    return Ok(CaptureCatalogTerminalAttempt::Quarantined {
                                        reason,
                                    });
                                }
                                FinalizeDecision::CommitTerminal => {
                                    return Err(CaptureCatalogError::InvalidRequest);
                                }
                            }
                        }
                    }
                }
                if require_source_match && finalizer.source_digest != request.source_digest {
                    let checkpoint_id =
                        crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                            request.action.event_id,
                            request.checkpoint_write,
                        );
                    let marker_slot = (request.session.session_id.clone(), checkpoint_id);
                    match registered_terminal_markers.get(&marker_slot) {
                        Some(generation) if generation == &finalizer.marker_generation => {
                            return Ok(CaptureCatalogTerminalAttempt::Adopted {
                                marker_generation: finalizer.marker_generation.clone(),
                                source_digest: finalizer.source_digest.clone(),
                            });
                        }
                        // Metadata stores one marker per (session,
                        // checkpoint), not one marker per generation. A
                        // different generation at this same slot is
                        // malformed/foreign recovery state, just as
                        // `terminal_attempt_marker_status` reports
                        // `Incompatible` in the real catalog. It must win
                        // over the durable-row probe below: treating it as
                        // absent would let a fake-only test model a
                        // quarantine or replay that production rejects
                        // behind FinalizerFence.
                        Some(_) => {
                            return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                                conflict: CaptureCatalogConflict::FinalizerFence,
                            });
                        }
                        None if durable_terminal_checkpoints.contains(&marker_slot) => {
                            return Ok(CaptureCatalogTerminalAttempt::DurableReplay);
                        }
                        None => {}
                    }

                    // Keep the fake's durable semantics aligned with the
                    // real catalog: a different source with no registered
                    // marker cannot reconstruct the elected bytes, so it
                    // becomes repair-required rather than retrying forever.
                    let pending = finalizer.to_pending_receipt()?;
                    let Some(receipt) = existing.ledger.find_mut(&key) else {
                        return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                            conflict: CaptureCatalogConflict::MissingReceipt,
                        });
                    };
                    receipt.finalizer = Some(StoredFinalizeReceipt::quarantined(
                        &pending,
                        FinalizeQuarantineReason::SourceDigestConflict,
                    ));
                    existing.state = DurableCaptureState {
                        phase: CapturePhase::Quarantined,
                        stopped_at: None,
                        sync_revision: existing
                            .state
                            .sync_revision
                            .checked_add(1)
                            .ok_or(CaptureCatalogError::InvalidRequest)?,
                    };
                    return Ok(CaptureCatalogTerminalAttempt::Quarantined {
                        reason: FinalizeQuarantineReason::SourceDigestConflict,
                    });
                }
                return Ok(CaptureCatalogTerminalAttempt::Bound {
                    marker_generation: finalizer.marker_generation.clone(),
                    source_digest: finalizer.source_digest.clone(),
                    attempts: finalizer.to_pending_receipt()?.attempts(),
                    registration_fence: Box::new(
                        CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                            request,
                            finalizer.marker_generation.clone(),
                            finalizer.source_digest.clone(),
                        ),
                    ),
                });
            }
        }

        let current_receipt = match persisted.as_ref() {
            Some(finalizer)
                if finalizer.is_unbound_snapshot()
                    && !request
                        .marker_generation
                        .starts_with(UNBOUND_FINALIZER_MARKER_PREFIX) =>
            {
                Some(finalizer.rebind_unbound_snapshot(
                    request.marker_generation.clone(),
                    request.source_digest.clone(),
                )?)
            }
            Some(finalizer) => Some(finalizer.to_pending_receipt()?),
            None => None,
        };
        let effective_policy = persisted
            .as_ref()
            .map(StoredFinalizeReceipt::to_policy)
            .transpose()?
            .unwrap_or_else(|| request.policy.clone());
        let decision = decide_finalization(FinalizeDecisionInput {
            policy: &effective_policy,
            current_receipt: current_receipt.as_ref(),
            marker_generation: &request.marker_generation,
            source_digest: request.source_digest.as_deref(),
            now_millis: request.now_millis,
            checkpoint: request.checkpoint,
        })
        .map_err(|_| CaptureCatalogError::InvalidRequest)?;
        match decision {
            FinalizeDecision::PersistPending(pending) => {
                let Some(receipt) = existing.ledger.find_mut(&key) else {
                    return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::MissingReceipt,
                    });
                };
                receipt.finalizer = Some(StoredFinalizeReceipt::from_pending(&pending));
                Ok(CaptureCatalogTerminalAttempt::Bound {
                    marker_generation: pending.marker_generation().to_string(),
                    source_digest: pending.source_digest().map(str::to_string),
                    attempts: pending.attempts(),
                    registration_fence: Box::new(
                        CaptureCatalogTerminalAttemptFence::for_bound_attempt(
                            request,
                            pending.marker_generation().to_string(),
                            pending.source_digest().map(str::to_string),
                        ),
                    ),
                })
            }
            FinalizeDecision::Quarantine { reason } => {
                let pending = current_receipt.ok_or(CaptureCatalogError::InvalidRequest)?;
                let Some(receipt) = existing.ledger.find_mut(&key) else {
                    return Ok(CaptureCatalogTerminalAttempt::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::MissingReceipt,
                    });
                };
                receipt.finalizer = Some(StoredFinalizeReceipt::quarantined(&pending, reason));
                existing.state = DurableCaptureState {
                    phase: CapturePhase::Quarantined,
                    stopped_at: None,
                    sync_revision: existing
                        .state
                        .sync_revision
                        .checked_add(1)
                        .ok_or(CaptureCatalogError::InvalidRequest)?,
                };
                Ok(CaptureCatalogTerminalAttempt::Quarantined { reason })
            }
            FinalizeDecision::CommitTerminal => Err(CaptureCatalogError::InvalidRequest),
        }
    }

    async fn prove_durable_replay(
        &self,
        request: &CaptureCatalogCompleteRequest,
    ) -> Result<CaptureCatalogFinalizeResult, CaptureCatalogError> {
        request.session.validate()?;
        let catalog = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        let Some(existing) = catalog.sessions.iter().find(|stored| {
            stored.scope == request.scope
                && stored.session.agent_kind == request.session.agent_kind
                && stored.session.provider_session_id == request.session.provider_session_id
        }) else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if existing.session.session_id != request.session.session_id
            || existing.session.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let key = request.action.completion_receipt_storage_key();
        let Some(receipt) = existing.ledger.find(&key) else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if !receipt.is_deferred_terminal() {
            return Err(CaptureCatalogError::InvalidRequest);
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogFinalizeResult::AlreadyComplete);
        }
        if !receipt.can_resume_from(Some(existing.state)) {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            });
        }
        let Some(finalizer) = receipt.finalizer.as_ref() else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            });
        };
        if finalizer.status == StoredFinalizeStatus::Quarantined {
            return Ok(CaptureCatalogFinalizeResult::Quarantined {
                reason: finalizer
                    .quarantine_reason()
                    .ok_or(CaptureCatalogError::MalformedReceiptLedger)?,
            });
        }
        let Some(proof) = finalizer.completion_proof() else {
            return Ok(CaptureCatalogFinalizeResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            });
        };
        Ok(CaptureCatalogFinalizeResult::ReadyToComplete { proof })
    }

    async fn complete_durable_replay(
        &self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
        request.session.validate()?;
        if !request.mutation.is_terminal()
            || request.mutation.checkpoint != CheckpointWrite::Committed
        {
            return Err(CaptureCatalogError::InvalidRequest);
        }

        // The mutex is the fake's writer-transaction boundary. Keep the
        // checkpoint probe and receipt completion under this one lock, just
        // as the SQLite implementation does, so a coordinator test cannot
        // accidentally model a changed source as a new writer.
        let mut catalog = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        match catalog.faults.pop_front() {
            Some(CaptureCatalogFault::Conflict) => {
                return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                });
            }
            Some(CaptureCatalogFault::LostFence) => return Err(CaptureCatalogError::ScopeRejected),
            Some(CaptureCatalogFault::CommitFailure) => {
                return Err(CaptureCatalogError::CommitFailed);
            }
            None => {}
        }
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                request.action.event_id,
                CheckpointWrite::Committed,
            );
        if !catalog
            .durable_terminal_checkpoints
            .contains(&(request.session.session_id.clone(), checkpoint_id))
        {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            });
        }
        let Some(existing) = catalog.sessions.iter_mut().find(|stored| {
            stored.scope == request.scope
                && stored.session.agent_kind == request.session.agent_kind
                && stored.session.provider_session_id == request.session.provider_session_id
        }) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if existing.session.session_id != request.session.session_id
            || existing.session.working_dir != request.session.working_dir
        {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            });
        }
        let key = request.action.completion_receipt_storage_key();
        let Some(receipt) = existing.ledger.find(&key) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        if !receipt.matches_action(&request.action) {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            });
        }
        if receipt.status == StoredReceiptStatus::Complete {
            return Ok(CaptureCatalogCompleteResult::AlreadyComplete);
        }
        if !receipt.is_deferred_terminal()
            || !receipt.can_resume_from(Some(existing.state))
            || receipt
                .finalizer
                .as_ref()
                .and_then(StoredFinalizeReceipt::completion_proof)
                .is_none()
        {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            });
        }
        let reserved_revision = receipt.reserved_revision;
        let terminal_timestamp = receipt
            .intent
            .terminal_timestamp()
            .ok_or(CaptureCatalogError::MalformedReceiptLedger)?;
        let Some(receipt) = existing.ledger.find_mut(&key) else {
            return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::MissingReceipt,
            });
        };
        receipt.status = StoredReceiptStatus::Complete;
        existing.state = DurableCaptureState {
            phase: CapturePhase::Stopped,
            stopped_at: Some(terminal_timestamp),
            sync_revision: reserved_revision
                .checked_add(1)
                .ok_or(CaptureCatalogError::InvalidRequest)?,
        };
        Ok(CaptureCatalogCompleteResult::Completed)
    }

    async fn update_diagnostic(
        &self,
        request: &CaptureCatalogApplyRequest,
        _: CaptureCatalogDiagnostic,
    ) -> Result<bool, CaptureCatalogError> {
        request.session.validate()?;
        let catalog = self
            .state
            .lock()
            .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
        Ok(catalog.sessions.iter().any(|stored| {
            stored.scope == request.scope
                && stored.session.agent_kind == request.session.agent_kind
                && stored.session.provider_session_id == request.session.provider_session_id
                && stored.session.session_id == request.session.session_id
                && stored.session.working_dir == request.session.working_dir
        }))
    }
}

#[allow(dead_code)]
fn insert_fake_receipt(
    session: &mut FakeCatalogSession,
    request: &CaptureCatalogApplyRequest,
    reserved_revision: i64,
) -> Result<CaptureReceiptDisposition, CaptureCatalogError> {
    let Some(key) = request
        .action
        .receipt_storage_key(request.mutation.is_terminal())
    else {
        return Ok(CaptureReceiptDisposition::NotTracked);
    };
    let status = if request.mutation.checkpoint == CheckpointWrite::None {
        StoredReceiptStatus::Complete
    } else {
        StoredReceiptStatus::Pending
    };
    session.ledger.insert(StoredReceipt::new(
        &key,
        &request.action,
        &request.mutation,
        status,
        reserved_revision,
    ))?;
    Ok(match status {
        StoredReceiptStatus::Pending => CaptureReceiptDisposition::Pending,
        StoredReceiptStatus::Complete => CaptureReceiptDisposition::Complete,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, TransactionTrait};
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn reducer_revision_snapshot_must_match_catalog_precondition() {
        let current = DurableCaptureState {
            phase: CapturePhase::Active,
            stopped_at: None,
            sync_revision: 7,
        };
        let action = crate::internal::ai::capture::state::reduce_lifecycle(
            crate::internal::ai::capture::state::LifecycleReducerInput {
                current: Some(current),
                event_kind: LifecycleEventKind::TurnStart,
                event_id: Uuid::from_u128(7),
                occurred_at: 1_700_000_000,
                deadline: None,
            },
        )
        .expect("active turn-start is reducible");
        assert!(
            CaptureCatalogMutation::from_reducer(Some(current), &action, 1_700_000_000).is_ok()
        );

        let stale = DurableCaptureState {
            sync_revision: 8,
            ..current
        };
        assert_eq!(
            CaptureCatalogMutation::from_reducer(Some(stale), &action, 1_700_000_000),
            Err(CaptureCatalogError::InvalidRequest),
            "catalog precondition must use the revision snapshot emitted by the reducer"
        );
    }

    #[test]
    fn pending_validation_errors_preserve_database_and_lease_classification() {
        assert_eq!(
            classify_pending_validation_error(anyhow::Error::new(
                crate::internal::workspace::WorkspaceError::ReadFailed(
                    "database temporarily unavailable".to_owned(),
                ),
            )),
            CaptureCatalogError::Database
        );
        assert_eq!(
            classify_pending_validation_error(anyhow::Error::new(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "key file temporarily unavailable",
            ))),
            CaptureCatalogError::Database
        );
        assert_eq!(
            classify_pending_validation_error(anyhow::anyhow!(
                CaptureCatalogError::DeadlineExceeded
            )),
            CaptureCatalogError::DeadlineExceeded
        );
        assert_eq!(
            classify_pending_validation_error(anyhow::anyhow!(
                CaptureCatalogError::WorkspaceLeaseRejected
            )),
            CaptureCatalogError::WorkspaceLeaseRejected
        );
        assert_eq!(
            classify_pending_validation_error(anyhow::Error::new(
                crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::WorkspaceFenceRejected,
            )),
            CaptureCatalogError::WorkspaceLeaseRejected
        );
        assert_eq!(
            classify_pending_validation_error(anyhow::Error::new(
                crate::internal::ai::capture_scope::CaptureFinalCommitAuthorizationError::DeadlineElapsed,
            )),
            CaptureCatalogError::DeadlineExceeded
        );
        assert_eq!(
            classify_pending_validation_error(anyhow::anyhow!(
                "authenticated session ownership changed"
            )),
            CaptureCatalogError::InvalidRequest
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn alias_artifact_publication_is_atomic() {
        use crate::internal::{
            ai::capture::pending_identity::{PendingSessionAlias, PreparedPendingAlias, lookup},
            config::ConfigKv,
            metadata::{MetadataKv, MetadataScope, MetadataValueType},
        };

        async fn header(
            txn: &DatabaseTransaction,
            scope: &CaptureScope,
            alias: &PreparedPendingAlias,
            checkpoint: &str,
        ) {
            let binding = crate::internal::ai::capture::pending::PendingBinding {
                scope: scope.clone(),
                session_id: alias.alias().to_owned(),
                checkpoint_id: checkpoint.to_owned(),
                event_id: Uuid::new_v4().to_string(),
                action_key: "private-test-action".into(),
                receipt_key: "private-test-receipt".into(),
                marker_generation: "private-test-marker".into(),
                source_commitment: format!("source/hmac-v2/{}", "a".repeat(64)),
                reserved_revision: 1,
                original_deadline_millis: None,
                deferrable: true,
                first_attempt_millis: 1,
                parent_commit: None,
                parent_unborn: true,
            };
            let value = format!(
                "{{\"version\":1,\"binding\":{},\"mac\":\"pending-envelope/hmac-v1/{}\",\"envelope_bytes\":1,\"chunks\":1,\"manual_attempted\":false}}",
                serde_json::to_string(&binding).unwrap(),
                "b".repeat(64)
            );
            MetadataKv::set_with_conn(
                txn,
                MetadataScope::AgentCapturePending,
                &scope.repo_id,
                checkpoint,
                &value,
                MetadataValueType::Text,
            )
            .await
            .unwrap();
        }

        let root = tempfile::tempdir().unwrap();
        let storage = root.path().join(".libra");
        std::fs::create_dir_all(storage.join("objects")).unwrap();
        let path = storage.join("libra.db");
        let conn = db::create_database(path.to_str().unwrap()).await.unwrap();
        ConfigKv::set_with_conn(&conn, "libra.repoid", "atomic-alias-repo", false)
            .await
            .unwrap();
        crate::internal::ai::capture::key::load_capture_dedup_secret(&storage).unwrap();
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
              state, working_dir, metadata_json, started_at, last_event_at, sync_revision,
              repo_id, worktree_id, scope_state) VALUES ('claude__native-session', 'claude_code',
              'native-session', 'active', ?, '{}', 1, 1, 1, 'atomic-alias-repo', '', 'scoped')",
            [root.path().to_string_lossy().into_owned().into()],
        ))
        .await
        .unwrap();
        let scope = CaptureScope::resolve(&conn, root.path()).await.unwrap();
        let txn = begin_catalog_write_transaction(&conn, None).await.unwrap();
        let context = resolve_pending_session_context(&txn, &scope, "claude__native-session")
            .await
            .unwrap();
        txn.commit().await.unwrap();
        let deadline = Instant::now() + Duration::from_secs(30);
        let first =
            PendingSessionAlias::prepare(&conn, &context, None, &storage, root.path(), deadline)
                .await
                .unwrap();
        let second =
            PendingSessionAlias::prepare(&conn, &context, None, &storage, root.path(), deadline)
                .await
                .unwrap();
        let checkpoint = Uuid::new_v4().to_string();
        let txn = begin_catalog_write_transaction(&conn, None).await.unwrap();
        header(&txn, &scope, &first, &checkpoint).await;
        first.publish_for_artifact(&txn, &checkpoint).await.unwrap();
        txn.rollback().await.unwrap();
        assert!(
            lookup(&conn, &scope.repo_id, first.alias())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentCapturePending,
                &scope.repo_id,
                &checkpoint
            )
            .await
            .unwrap()
            .is_none()
        );

        conn.execute_unprepared("PRAGMA journal_mode = WAL")
            .await
            .unwrap();
        let contender = db::establish_connection_with_busy_timeout(
            path.to_str().unwrap(),
            Duration::from_secs(5),
        )
        .await
        .unwrap();
        contender
            .execute_unprepared("PRAGMA busy_timeout = 0")
            .await
            .unwrap();
        let txn = begin_catalog_write_transaction(&conn, None).await.unwrap();
        header(&txn, &scope, &first, &checkpoint).await;
        first.publish_for_artifact(&txn, &checkpoint).await.unwrap();
        assert!(
            begin_catalog_write_transaction(&contender, None)
                .await
                .is_err(),
            "competing writer must fail/retry before reverse lookup, not mint a second alias"
        );
        txn.commit().await.unwrap();
        let other_checkpoint = Uuid::new_v4().to_string();
        let txn = begin_catalog_write_transaction(&contender, None)
            .await
            .unwrap();
        header(&txn, &scope, &second, &other_checkpoint).await;
        let lost = second
            .publish_for_artifact(&txn, &other_checkpoint)
            .await
            .expect_err("the losing mint must not publish a second alias");
        assert!(
            lost.chain().any(|cause| {
                cause.is::<crate::internal::ai::capture::pending_identity::PendingAliasConflict>()
            }),
            "losing the mint race is a typed conflict, not untrusted association data"
        );
        assert_eq!(
            classify_pending_validation_error(lost),
            CaptureCatalogError::CommitFailed,
            "a lost mint race must classify as conflict/retry, never InvalidRequest"
        );
        txn.rollback().await.unwrap();
        assert!(
            lookup(&conn, &scope.repo_id, first.alias())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            lookup(&conn, &scope.repo_id, second.alias())
                .await
                .unwrap()
                .is_none()
        );
        assert!(
            MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentCapturePending,
                &scope.repo_id,
                &other_checkpoint
            )
            .await
            .unwrap()
            .is_none()
        );

        // A stale WAL reader cannot upgrade to a writer even after the other
        // writer commits. This must remain conflict/retry, not a second mint.
        let reader = conn.begin().await.unwrap();
        reader
            .query_one_raw(Statement::from_string(
                reader.get_database_backend(),
                "SELECT COUNT(*) FROM metadata_kv".to_owned(),
            ))
            .await
            .unwrap();
        MetadataKv::set_with_conn(
            &contender,
            MetadataScope::Branch,
            "fixture",
            "wal-generation",
            "advanced",
            MetadataValueType::Text,
        )
        .await
        .unwrap();
        let stale = first
            .publish_for_artifact(&reader, &checkpoint)
            .await
            .expect_err("a stale snapshot cannot upgrade to the alias writer");
        assert_eq!(
            classify_pending_validation_error(stale),
            CaptureCatalogError::Database,
            "a stale snapshot is a retryable store fault, not untrusted association data"
        );
        reader.rollback().await.unwrap();
        let fresh = begin_catalog_write_transaction(&conn, None).await.unwrap();
        first
            .publish_for_artifact(&fresh, &checkpoint)
            .await
            .unwrap();
        fresh.commit().await.unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pending_artifact_commit_is_atomic() {
        use crate::internal::{
            ai::{
                capture::{
                    checkpoint::{
                        CheckpointConflictReason, CheckpointRedactedPayload, CheckpointStore,
                        CheckpointWriteOutcome, CheckpointWriteRequest, TracesCheckpointStore,
                        checkpoint_id_for_capture_action,
                    },
                    key,
                    pending::{self, PendingHeader},
                    snapshot::CaptureSnapshotService,
                },
                coverage_gate::{LiveClaimCommitPlan, ReservedTurnClaim},
                hooks::lifecycle::{
                    CanonicalEventContext, LifecycleEvent, LifecycleIdentityScheme,
                    lifecycle_event_canonical_json_with_identity,
                },
                observed_agents::{Completeness, ExportAuthorized, Redactor, TranscriptSource},
            },
            config::ConfigKv,
            metadata::{MetadataKv, MetadataScope},
        };

        const PK: &str = "claude__native-session";
        const NATIVE: &str = "native-session";
        for (completion_path, unknown_collision, exhausted, resume_before_claim) in [
            ("strict", false, None, false),
            ("durable-replay", false, None, false),
            ("doctor", false, None, false),
            ("strict", true, None, false),
            (
                "strict",
                false,
                Some(FinalizeQuarantineReason::AttemptLimit),
                false,
            ),
            (
                "strict",
                false,
                Some(FinalizeQuarantineReason::WindowLimit),
                false,
            ),
            (
                "strict",
                false,
                Some(FinalizeQuarantineReason::AttemptLimit),
                true,
            ),
            (
                "strict",
                false,
                Some(FinalizeQuarantineReason::WindowLimit),
                true,
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let storage = root.path().join(".libra");
            std::fs::create_dir_all(storage.join("objects")).unwrap();
            let conn = db::create_database(storage.join("libra.db").to_str().unwrap())
                .await
                .unwrap();
            ConfigKv::set_with_conn(&conn, "libra.repoid", "repo-a", false)
                .await
                .unwrap();
            key::load_capture_dedup_secret(&storage).unwrap();
            let scoped = CaptureScope::resolve(&conn, root.path()).await.unwrap();
            let identity = CaptureCatalogSession::new(
                PK,
                "claude_code",
                NATIVE,
                root.path().to_string_lossy(),
            )
            .unwrap();
            let store = CaptureCatalogStore::new(conn.clone());
            let start = CaptureCatalogApplyRequest::new(
                scoped.clone(),
                identity.clone(),
                action(0xacf100, Some(receipt('a'))),
                mutation(None, CheckpointWrite::None),
            )
            .unwrap();
            store.apply(&start).await.unwrap();
            let terminal_action = action(0xacf101, Some(receipt('b')));
            let stop = CaptureCatalogApplyRequest::new(
                scoped.clone(),
                identity,
                terminal_action.clone(),
                terminal_mutation(Some(DurableCaptureState {
                    phase: CapturePhase::Active,
                    stopped_at: None,
                    sync_revision: 1,
                })),
            )
            .unwrap();
            store.apply(&stop).await.unwrap();
            let checkpoint = checkpoint_id_for_capture_action(
                terminal_action.event_id,
                CheckpointWrite::Committed,
            );
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO agent_coverage_claim (
                    session_id, logical_turn_key, coverage_schema_version, coverage_digest,
                    completeness, revision, state, owner, fence_token, source_channel, created_at, updated_at)
                 VALUES (?, 'turn-1', 1, ?, 'complete', 0, 'reserved_live', 'catalog-live-owner', 1, 'live', 1, 1)",
                [PK.into(), "d".repeat(64).into()],
            )).await.unwrap();
            let source = root.path().join("source.jsonl");
            std::fs::write(&source, b"redacted terminal snapshot AKIAABCDEFGHIJKLMNOP").unwrap();
            let bytes = std::fs::read(&source).unwrap();
            let auth = ExportAuthorized::issue("claude_code", NATIVE, &bytes);
            let mut snapshot = CaptureSnapshotService::capture_authorized(
                TranscriptSource::Bytes { bytes, auth },
                "claude_code",
                NATIVE,
                Default::default(),
            );
            let deadline = deadline_after(Duration::from_secs(60));
            let source_mac = key::derive_snapshot_content_commitment_in_scope_until(
                &conn,
                &scoped,
                &storage,
                root.path(),
                &snapshot.redacted_digest_preimage().unwrap(),
                deadline.monotonic(),
            )
            .await
            .unwrap();
            assert!(snapshot.bind_source_commitment(source_mac.clone()));
            let report = serde_json::to_value(snapshot.redaction_report()).unwrap();
            let metadata = serde_json::json!({
                "schema_version":2, "checkpoint_id":checkpoint, "session_id":PK,
                "provider_session_id":NATIVE, "working_dir":root.path().to_string_lossy(),
                "agent_kind":"claude_code", "scope":"committed", "model":null,
                "created_at":1, "redaction_report":report,
                "transcript_snapshot":snapshot.safe_projection(), "extraction":{}
            });
            let event = lifecycle_event_canonical_json_with_identity(
                &LifecycleEvent {
                    kind: LifecycleEventKind::SessionEnd,
                    session_id: NATIVE.into(),
                    session_ref: None,
                    prompt: None,
                    model: None,
                    source: None,
                    tool_name: None,
                    tool_input: None,
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
                terminal_action.event_id,
                false,
            );
            let mut events = serde_json::to_vec(&event).unwrap();
            events.push(b'\n');
            let redact = |bytes: &[u8]| Redactor::new_default().redact(bytes).0;
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
                checkpoint_id: checkpoint.clone(),
                owner: "catalog-live-owner".into(),
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
                capture_scope: Some(scoped.clone()),
            };
            let policy = finalizer_policy(&terminal_action, None);
            let marker = crate::internal::ai::traces::TracesInflightMarker::new(
                PK,
                &checkpoint,
                chrono::Utc::now().timestamp_millis(),
            );
            let generation = marker.generation.clone().unwrap();
            let fence = match store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        &generation,
                        Some(&source_mac),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .unwrap()
            {
                CaptureCatalogTerminalAttempt::Bound {
                    registration_fence, ..
                } => *registration_fence,
                other => panic!("expected real terminal election, got {other:?}"),
            };

            // The failure occurs after chunks/header insertion, while the
            // private association is being published in the same writer.
            conn.execute_unprepared(
                "CREATE TRIGGER fail_pending_alias BEFORE INSERT ON metadata_kv
                 WHEN NEW.scope = 'agent_capture_session_alias'
                 BEGIN SELECT RAISE(ABORT, 'injected alias publication failure'); END",
            )
            .await
            .unwrap();
            assert!(
                store
                    .persist_pending_artifact(
                        &fence,
                        &payload,
                        &coverage,
                        &storage,
                        root.path(),
                        deadline,
                    )
                    .await
                    .is_err()
            );
            let private_count = || async {
                conn.query_one_raw(Statement::from_string(DbBackend::Sqlite,
                    "SELECT COUNT(*) AS count FROM metadata_kv WHERE scope IN
                     ('agent_capture_pending', 'agent_capture_quarantine', 'agent_capture_pending_chunk', 'agent_capture_session_alias')".to_owned(),
                )).await.unwrap().unwrap().try_get_by::<i64, _>("count").unwrap()
            };
            assert_eq!(
                private_count().await,
                0,
                "failed publication must roll back every private row"
            );
            conn.execute_unprepared("DROP TRIGGER fail_pending_alias")
                .await
                .unwrap();
            store
                .persist_pending_artifact(
                    &fence,
                    &payload,
                    &coverage,
                    &storage,
                    root.path(),
                    deadline,
                )
                .await
                .unwrap();
            let retained = private_count().await;
            assert_eq!(retained, 3, "one header, one chunk, one association");
            store
                .persist_pending_artifact(
                    &fence,
                    &payload,
                    &coverage,
                    &storage,
                    root.path(),
                    deadline,
                )
                .await
                .unwrap();
            assert_eq!(
                private_count().await,
                retained,
                "redelivery must reuse the alias"
            );
            std::fs::remove_file(source).unwrap();
            conn.execute_unprepared("VACUUM").await.unwrap();
            store
                .persist_pending_artifact(
                    &fence,
                    &payload,
                    &coverage,
                    &storage,
                    root.path(),
                    deadline,
                )
                .await
                .unwrap();
            assert_eq!(
                private_count().await,
                retained,
                "DB-only redelivery after VACUUM remains idempotent"
            );
            let header = PendingHeader::decode(
                &MetadataKv::get_with_conn(
                    &conn,
                    MetadataScope::AgentCapturePending,
                    &scoped.repo_id,
                    &checkpoint,
                )
                .await
                .unwrap()
                .unwrap()
                .value,
            )
            .unwrap();
            let prepared = store
                .prepare_pending_session_alias(&fence, &storage, root.path(), deadline)
                .await
                .unwrap();
            let replayed = pending::load_verified_payload(
                &conn,
                &storage,
                root.path(),
                &prepared,
                &header.binding,
                &header,
                deadline.monotonic(),
            )
            .await
            .unwrap();
            assert_eq!(
                replayed.payload().metadata_json().bytes(),
                payload.metadata_json().bytes()
            );
            assert_eq!(
                replayed.payload().transcript().bytes(),
                payload.transcript().bytes()
            );
            assert_eq!(replayed.coverage().session_id, PK);
            assert_eq!(
                store
                    .claim_manual_pending_artifact(
                        &replayed,
                        &prepared,
                        &header,
                        MAX_FINALIZE_WINDOW_MILLIS + 2,
                        deadline,
                    )
                    .await
                    .unwrap_err(),
                CaptureCatalogError::InvalidRequest,
                "a pending namespace cannot grant an operator attempt"
            );

            let session_row = || async {
                let txn = conn.begin().await.unwrap();
                let row = read_session(&txn, &stop.session).await.unwrap().unwrap();
                txn.commit().await.unwrap();
                row
            };
            let header_scope = if let Some(reason) = exhausted {
                if reason == FinalizeQuarantineReason::AttemptLimit {
                    for attempt in 2..=MAX_FINALIZE_ATTEMPTS {
                        assert!(matches!(
                            store.finalize(&finalize_request(
                                &stop, policy.clone(), &generation, Some(&source_mac),
                                i64::from(attempt), FinalizeCheckpointProgress::NotStarted,
                            )).await.unwrap(),
                            CaptureCatalogFinalizeResult::Pending { attempts, .. }
                                if attempts == attempt
                        ));
                    }
                }
                let now = match reason {
                    FinalizeQuarantineReason::AttemptLimit => i64::from(MAX_FINALIZE_ATTEMPTS) + 1,
                    FinalizeQuarantineReason::WindowLimit => MAX_FINALIZE_WINDOW_MILLIS + 2,
                    _ => unreachable!("closed exhaustion test cases"),
                };
                let original = session_row().await;
                let original_header = MetadataKv::get_with_conn(
                    &conn,
                    MetadataScope::AgentCapturePending,
                    &scoped.repo_id,
                    &checkpoint,
                )
                .await
                .unwrap()
                .unwrap();
                let header_evidence = || async {
                    conn.query_all_raw(Statement::from_sql_and_values(
                        conn.get_database_backend(),
                        "SELECT hex(CAST(value AS BLOB)) AS value_hex, typeof(value) AS value_kind,
                                value_type, created_at, updated_at FROM metadata_kv
                         WHERE target = ? AND key = ? AND scope IN
                           ('agent_capture_pending', 'agent_capture_quarantine') ORDER BY scope",
                        [scoped.repo_id.clone().into(), checkpoint.clone().into()],
                    ))
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| {
                        [
                            "value_hex",
                            "value_kind",
                            "value_type",
                            "created_at",
                            "updated_at",
                        ]
                        .map(|field| row.try_get_by::<String, _>(field).unwrap())
                    })
                    .collect::<Vec<_>>()
                };
                let original_header_evidence = header_evidence().await;
                let evidence = || async {
                    conn.query_all_raw(Statement::from_sql_and_values(
                        conn.get_database_backend(),
                        "SELECT scope, key, hex(CAST(value AS BLOB)) AS value_hex, value_type,
                                created_at, updated_at FROM metadata_kv
                         WHERE target = ? AND scope IN ('agent_capture_pending_chunk',
                           'agent_capture_session_alias') ORDER BY scope, key",
                        [scoped.repo_id.clone().into()],
                    ))
                    .await
                    .unwrap()
                    .into_iter()
                    .map(|row| {
                        [
                            "scope",
                            "key",
                            "value_hex",
                            "value_type",
                            "created_at",
                            "updated_at",
                        ]
                        .map(|field| row.try_get_by::<String, _>(field).unwrap())
                    })
                    .collect::<Vec<_>>()
                };
                let original_evidence = evidence().await;
                for route in ["doctor", "finalize", "doctor"] {
                    if route == "doctor" {
                        let mut recoveries = store
                            .pending_finalizer_recoveries_for_doctor(now)
                            .await
                            .unwrap()
                            .recoveries;
                        assert_eq!(recoveries.len(), 1);
                        let recovery = recoveries.pop().unwrap();
                        assert!(recovery.budget_exhausted());
                        assert_eq!(
                            store
                                .quarantine_exhausted_pending_finalizer(&recovery, now)
                                .await
                                .unwrap(),
                            CaptureCatalogFinalizerRecoveryResult::Quarantined
                        );
                    } else {
                        assert_eq!(
                            store
                                .finalize(&finalize_request(
                                    &stop,
                                    policy.clone(),
                                    &generation,
                                    Some(&source_mac),
                                    now,
                                    FinalizeCheckpointProgress::NotStarted,
                                ))
                                .await
                                .unwrap(),
                            CaptureCatalogFinalizeResult::Quarantined { reason }
                        );
                    }
                    let after = session_row().await;
                    assert_eq!(
                        after.state, original.state,
                        "routing cannot change phase/revision"
                    );
                    assert_eq!(
                        after.metadata_json, original.metadata_json,
                        "routing cannot change original receipt status/counters/policy"
                    );
                    assert!(
                        MetadataKv::get_with_conn(
                            &conn,
                            MetadataScope::AgentCapturePending,
                            &scoped.repo_id,
                            &checkpoint
                        )
                        .await
                        .unwrap()
                        .is_none()
                    );
                    let routed = MetadataKv::get_with_conn(
                        &conn,
                        MetadataScope::AgentCaptureQuarantine,
                        &scoped.repo_id,
                        &checkpoint,
                    )
                    .await
                    .unwrap()
                    .unwrap();
                    assert_eq!(routed.value, original_header.value);
                    assert_eq!(routed.value_type, original_header.value_type);
                    assert_eq!(header_evidence().await, original_header_evidence);
                    assert_eq!(evidence().await, original_evidence);
                    assert_eq!(private_count().await, retained);
                    let txn = begin_catalog_write_transaction(&conn, None).await.unwrap();
                    assert_eq!(
                        verify_terminal_attempt_registration(
                            &txn,
                            &fence,
                            &checkpoint,
                            &generation,
                        )
                        .await
                        .unwrap_err(),
                        CaptureCatalogError::InvalidRequest,
                        "an earlier automatic election cannot register after artifact quarantine"
                    );
                    txn.commit().await.unwrap();
                }
                if resume_before_claim {
                    let resume = CaptureCatalogApplyRequest::new(
                        scoped.clone(),
                        stop.session.clone(),
                        action(0xacf102, Some(receipt('c'))),
                        CaptureCatalogMutation::new(
                            Some(original.state),
                            CapturePhase::Active,
                            StoppedAtMutation::Preserve,
                            CheckpointWrite::None,
                            1_700_000_001,
                        )
                        .unwrap(),
                    )
                    .unwrap();
                    assert!(matches!(
                        store.apply(&resume).await.unwrap(),
                        CaptureCatalogApplyResult::Applied { .. }
                    ));
                    let resumed = session_row().await;
                    assert_eq!(
                        resumed.state.sync_revision,
                        original.state.sync_revision + 1
                    );
                    assert_eq!(
                        store
                            .claim_manual_pending_artifact(
                                &replayed, &prepared, &header, now, deadline,
                            )
                            .await
                            .unwrap_err(),
                        CaptureCatalogError::InvalidRequest
                    );
                    assert_eq!(session_row().await.state, resumed.state);
                    assert_eq!(session_row().await.metadata_json, resumed.metadata_json);
                    assert_eq!(header_evidence().await, original_header_evidence);
                    assert_eq!(evidence().await, original_evidence);
                    let audit = conn.query_one_raw(Statement::from_string(conn.get_database_backend(),
                        "SELECT COUNT(*) AS n FROM agent_audit_log WHERE action='repair_pending_capture'".to_owned(),
                    )).await.unwrap().unwrap();
                    assert_eq!(audit.try_get_by::<i64, _>("n").unwrap(), 0);
                    continue;
                }

                // The operator claim is audited in the SAME writer: a late
                // audit failure cannot consume the private one-shot bit.
                let txn = begin_catalog_write_transaction(&conn, None).await.unwrap();
                crate::internal::ai::traces::write_traces_inflight_marker(&txn, &marker)
                    .await
                    .unwrap();
                txn.commit().await.unwrap();
                assert_eq!(store.claim_manual_pending_artifact(
                    &replayed, &prepared, &header, now, deadline,
                ).await.unwrap_err(), CaptureCatalogError::InvalidRequest,
                    "manual repair cannot adopt a registered foreground marker");
                assert_eq!(header_evidence().await, original_header_evidence);
                conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                    "DELETE FROM metadata_kv WHERE scope='agent_traces_inflight' AND target=? AND key=?",
                    [PK.into(), checkpoint.clone().into()],
                )).await.unwrap();
                conn.execute_unprepared(
                    "CREATE TRIGGER fail_manual_audit BEFORE INSERT ON agent_audit_log
                     WHEN NEW.action = 'repair_pending_capture'
                     BEGIN SELECT RAISE(ABORT, 'injected manual audit failure'); END",
                )
                .await
                .unwrap();
                assert_eq!(store.claim_manual_pending_artifact(
                    &replayed, &prepared, &header, now, deadline,
                ).await.unwrap_err(), CaptureCatalogError::Database);
                assert_eq!(header_evidence().await, original_header_evidence);
                assert_eq!(session_row().await.metadata_json, original.metadata_json);
                conn.execute_unprepared("DROP TRIGGER fail_manual_audit")
                    .await
                    .unwrap();

                // A real coverage takeover must fail before consuming that
                // bit, even though the MAC and receipt are otherwise valid.
                conn.execute_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "UPDATE agent_coverage_claim SET owner='foreign-owner', fence_token=2
                     WHERE session_id=? AND logical_turn_key='turn-1'",
                    [PK.into()],
                ))
                .await
                .unwrap();
                assert_eq!(store.claim_manual_pending_artifact(
                    &replayed, &prepared, &header, now, deadline,
                ).await.unwrap_err(), CaptureCatalogError::InvalidRequest);
                assert_eq!(header_evidence().await, original_header_evidence);
                // Restore the isolated fixture's claim, not a production
                // recovery transition or a takeover-reset implementation.
                conn.execute_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "UPDATE agent_coverage_claim SET owner='catalog-live-owner', fence_token=1
                     WHERE session_id=? AND logical_turn_key='turn-1'",
                    [PK.into()],
                ))
                .await
                .unwrap();
                let manual_fence = store
                    .claim_manual_pending_artifact(&replayed, &prepared, &header, now, deadline)
                    .await
                    .unwrap();
                assert_ne!(
                    manual_fence, fence,
                    "manual capability differs from an old automatic election"
                );
                assert_eq!(manual_fence.marker_generation, fence.marker_generation);
                assert_eq!(manual_fence.source_digest, fence.source_digest);
                assert_eq!(manual_fence.action, fence.action);
                assert!(manual_fence.manual_artifact.is_some());
                let recovery_store = TracesCheckpointStore::from_terminal_recovery(
                    &conn,
                    root.path(),
                    &[],
                    manual_fence.clone(),
                )
                .unwrap();
                assert_eq!(recovery_store.marker_generation(), generation);
                // A caller can attach a genuine capability to a differently
                // constructed store. Bind the marker's native PK before any
                // SQL registration or one-shot authority consumption.
                let wrong_store = TracesCheckpointStore::new_with_persisted_marker_generation(
                    &conn,
                    root.path(),
                    "claude__foreign-native",
                    &checkpoint,
                    &generation,
                    &[],
                )
                .unwrap()
                .with_capture_scope(scoped.clone())
                .with_terminal_attempt_fence(manual_fence.clone());
                let wrong_request = CheckpointWriteRequest::new(
                    "wrong-native-marker",
                    &checkpoint,
                    "claude__foreign-native",
                    "claude_code",
                    None,
                    crate::internal::ai::traces::CheckpointScope::Committed,
                    &generation,
                    None,
                    replayed.payload(),
                    None,
                    Some(deadline),
                )
                .unwrap();
                assert_eq!(
                    wrong_store.write(wrong_request).await.unwrap(),
                    CheckpointWriteOutcome::ConflictUnchanged {
                        reason: CheckpointConflictReason::ScopeFence
                    }
                );
                assert!(
                    !manual_fence
                        .manual_artifact
                        .as_ref()
                        .unwrap()
                        .registration_consumed
                        .load(Ordering::SeqCst)
                );
                assert!(matches!(
                    store
                        .prepare_pending_session_alias(
                            &manual_fence,
                            &storage,
                            root.path(),
                            deadline,
                        )
                        .await,
                    Err(CaptureCatalogError::InvalidRequest)
                ));
                assert_eq!(
                    store
                        .persist_pending_artifact(
                            &manual_fence,
                            &payload,
                            &coverage,
                            &storage,
                            root.path(),
                            deadline,
                        )
                        .await
                        .unwrap_err(),
                    CaptureCatalogError::InvalidRequest
                );
                let after = session_row().await;
                assert_eq!(after.state, original.state);
                assert_eq!(after.metadata_json, original.metadata_json);
                assert_eq!(evidence().await, original_evidence);
                let consumed = PendingHeader::decode(
                    &MetadataKv::get_with_conn(
                        &conn,
                        MetadataScope::AgentCaptureQuarantine,
                        &scoped.repo_id,
                        &checkpoint,
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
                )
                .unwrap();
                assert!(consumed.manual_attempted);
                assert!(consumed.binding == header.binding);
                let mut expected_consumed = header.clone();
                expected_consumed.manual_attempted = true;
                assert!(
                    consumed == expected_consumed,
                    "only the one-shot bit changes"
                );
                for second in [&header, &consumed] {
                    assert_eq!(
                        store
                            .claim_manual_pending_artifact(
                                &replayed, &prepared, second, now, deadline,
                            )
                            .await
                            .unwrap_err(),
                        CaptureCatalogError::InvalidRequest
                    );
                }
                let audit = conn
                    .query_all_raw(Statement::from_sql_and_values(
                        conn.get_database_backend(),
                        "SELECT action, checkpoint_id, scope, justification, granted, export_path
                     FROM agent_audit_log WHERE action='repair_pending_capture'",
                        [],
                    ))
                    .await
                    .unwrap();
                assert_eq!(audit.len(), 1, "only the successful claim is audited");
                assert_eq!(
                    audit[0].try_get_by::<String, _>("checkpoint_id").unwrap(),
                    checkpoint
                );
                assert_eq!(
                    audit[0].try_get_by::<String, _>("scope").unwrap(),
                    "session"
                );
                assert_eq!(
                    audit[0].try_get_by::<String, _>("justification").unwrap(),
                    "explicit doctor repair of exhausted authenticated artifact"
                );
                assert_eq!(audit[0].try_get_by::<i64, _>("granted").unwrap(), 1);
                assert!(
                    audit[0]
                        .try_get_by::<Option<String>, _>("export_path")
                        .unwrap()
                        .is_none()
                );
                let txn = begin_catalog_write_transaction(&conn, None).await.unwrap();
                assert_eq!(
                    verify_terminal_attempt_registration(&txn, &fence, &checkpoint, &generation,)
                        .await
                        .unwrap_err(),
                    CaptureCatalogError::InvalidRequest
                );
                assert_eq!(
                    verify_terminal_attempt_registration(
                        &txn,
                        &manual_fence,
                        &checkpoint,
                        &generation,
                    )
                    .await
                    .unwrap(),
                    CaptureCatalogTerminalAttemptRegistration::Authorized
                );
                txn.rollback().await.unwrap();
                // Even a later marker transaction failure cannot reuse this
                // capability through a clone or an otherwise absent slot.
                let cloned_manual = manual_fence.clone();
                let txn = begin_catalog_write_transaction(&conn, None).await.unwrap();
                assert_eq!(
                    verify_terminal_attempt_registration(
                        &txn,
                        &cloned_manual,
                        &checkpoint,
                        &generation,
                    )
                    .await
                    .unwrap_err(),
                    CaptureCatalogError::InvalidRequest,
                    "a rolled-back registration still consumes the shared manual capability"
                );
                txn.rollback().await.unwrap();
                MetadataScope::AgentCaptureQuarantine
            } else {
                MetadataScope::AgentCapturePending
            };

            // These two consumers classify an already-durable checkpoint.
            // Only the catalog row is seeded: this tests catalog/cleanup, not
            // object publication, which belongs to the checkpoint store.
            if completion_path != "strict" {
                conn.execute_raw(Statement::from_sql_and_values(
                    DbBackend::Sqlite,
                    "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope,
                     tree_oid, metadata_blob_oid, traces_commit, created_at)
                     VALUES (?, ?, 'committed', ?, ?, ?, 1)",
                    [
                        checkpoint.clone().into(),
                        PK.into(),
                        "a".repeat(40).into(),
                        "b".repeat(40).into(),
                        "c".repeat(40).into(),
                    ],
                ))
                .await
                .unwrap();
            }
            let completion = match store
                .finalize(&finalize_request(
                    &stop,
                    policy,
                    &generation,
                    Some(&source_mac),
                    2,
                    FinalizeCheckpointProgress::Durable,
                ))
                .await
                .unwrap()
            {
                CaptureCatalogFinalizeResult::ReadyToComplete { proof } => {
                    CaptureCatalogCompleteRequest::from_finalizer(&stop, proof).unwrap()
                }
                other => panic!("expected strict proof, got {other:?}"),
            };
            let recovery = if completion_path == "doctor" {
                Some(
                    store
                        .pending_finalizer_recoveries_for_doctor(2)
                        .await
                        .unwrap()
                        .recoveries
                        .pop()
                        .unwrap(),
                )
            } else {
                None
            };
            let complete = || async {
                match completion_path {
                    "strict" => store
                        .complete(&completion)
                        .await
                        .map(|r| r == CaptureCatalogCompleteResult::Completed),
                    "durable-replay" => store
                        .complete_durable_replay(&stop)
                        .await
                        .map(|r| r == CaptureCatalogCompleteResult::Completed),
                    "doctor" => store
                        .recover_pending_finalizer_after_durable_checkpoint(
                            recovery.as_ref().unwrap(),
                            2,
                        )
                        .await
                        .map(|r| r == CaptureCatalogFinalizerRecoveryResult::Completed),
                    _ => unreachable!("closed test cases"),
                }
            };
            let before = session_row().await.metadata_json;
            let cleanup_scope = if exhausted.is_some() {
                "agent_capture_quarantine"
            } else {
                "agent_capture_pending"
            };
            for failure_scope in [cleanup_scope, "agent_capture_session_alias"] {
                conn.execute_unprepared(&format!(
                    "CREATE TRIGGER fail_pending_cleanup BEFORE DELETE ON metadata_kv
                     WHEN OLD.scope = '{failure_scope}'
                     BEGIN SELECT RAISE(ABORT, 'injected artifact cleanup failure'); END",
                ))
                .await
                .unwrap();
                assert!(
                    matches!(complete().await, Err(CaptureCatalogError::Database)),
                    "{completion_path} must surface the injected {failure_scope} database failure"
                );
                assert_eq!(private_count().await, retained);
                assert!(
                    MetadataKv::get_with_conn(&conn, header_scope, &scoped.repo_id, &checkpoint)
                        .await
                        .unwrap()
                        .is_some()
                );
                assert!(
                    MetadataKv::get_with_conn(
                        &conn,
                        MetadataScope::AgentCapturePendingChunk,
                        &scoped.repo_id,
                        &format!("{checkpoint}:000")
                    )
                    .await
                    .unwrap()
                    .is_some()
                );
                let after = session_row().await;
                assert_eq!(
                    after.metadata_json, before,
                    "receipt must roll back with even late alias cleanup failure"
                );
                assert_eq!(after.state.phase, CapturePhase::Active);
                conn.execute_unprepared("DROP TRIGGER fail_pending_cleanup")
                    .await
                    .unwrap();
            }
            let chunk_before_collision = MetadataKv::get_with_conn(
                &conn,
                MetadataScope::AgentCapturePendingChunk,
                &scoped.repo_id,
                &format!("{checkpoint}:000"),
            )
            .await
            .unwrap()
            .unwrap()
            .value;
            if unknown_collision {
                conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                    "INSERT INTO metadata_kv(scope,target,key,value,value_type,created_at,updated_at)
                     VALUES('agent_capture_quarantine',?,?,CAST(X'FF' AS TEXT),'text','collision-created','collision-updated')",
                    [scoped.repo_id.clone().into(), checkpoint.clone().into()])).await.unwrap();
            }
            assert!(
                complete().await.unwrap(),
                "{completion_path} must complete after retry"
            );
            let retained_after_completion = if unknown_collision { 3 } else { 0 };
            assert_eq!(private_count().await, retained_after_completion);
            if unknown_collision {
                let row = conn.query_one_raw(Statement::from_sql_and_values(conn.get_database_backend(),
                    "SELECT hex(CAST(value AS BLOB)) AS v, typeof(value) AS t, value_type, created_at,updated_at
                     FROM metadata_kv WHERE scope='agent_capture_quarantine' AND target=? AND key=?",
                    [scoped.repo_id.clone().into(), checkpoint.clone().into()])).await.unwrap().unwrap();
                let actual: Vec<String> = ["v", "t", "value_type", "created_at", "updated_at"]
                    .into_iter()
                    .map(|field| row.try_get_by::<String, _>(field).unwrap())
                    .collect();
                assert_eq!(
                    actual,
                    [
                        "FF",
                        "text",
                        "text",
                        "collision-created",
                        "collision-updated"
                    ]
                );
                assert_eq!(
                    MetadataKv::get_with_conn(
                        &conn,
                        MetadataScope::AgentCapturePendingChunk,
                        &scoped.repo_id,
                        &format!("{checkpoint}:000")
                    )
                    .await
                    .unwrap()
                    .unwrap()
                    .value,
                    chunk_before_collision
                );
                assert!(
                    MetadataKv::get_with_conn(
                        &conn,
                        MetadataScope::AgentCaptureSessionAlias,
                        &scoped.repo_id,
                        &header.binding.session_id
                    )
                    .await
                    .unwrap()
                    .is_some()
                );
            }
            assert_eq!(session_row().await.state.phase, CapturePhase::Stopped);
            for index in 0..=MAX_CAPTURE_RECEIPTS {
                let current = session_row().await.state;
                let key = OpaqueCaptureReceiptKey::parse(format!(
                    "{RECEIPT_PREFIX_V2}{:064x}",
                    index + 1
                ))
                .unwrap();
                let churn = CaptureCatalogApplyRequest::new(
                    scoped.clone(),
                    stop.session.clone(),
                    action(0xacf200 + index as u128, Some(key)),
                    CaptureCatalogMutation::new(
                        Some(current),
                        CapturePhase::Active,
                        StoppedAtMutation::Preserve,
                        CheckpointWrite::None,
                        1_700_000_100 + index as i64,
                    )
                    .unwrap(),
                )
                .unwrap();
                assert!(matches!(
                    store.apply(&churn).await.unwrap(),
                    CaptureCatalogApplyResult::Applied { .. }
                ));
            }
            let (_, ledger) = decode_receipt_metadata(&session_row().await.metadata_json).unwrap();
            assert!(
                ledger
                    .find(&stop.action.completion_receipt_storage_key())
                    .is_none(),
                "actual catalog traffic must evict the old completed receipt"
            );
            let before_replay = session_row().await;
            let current = before_replay.state;
            assert_eq!(
                store.apply(&stop).await.unwrap(),
                CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ExpectedState,
                },
                "an evicted terminal event cannot reuse its obsolete revision fence"
            );
            assert_eq!(
                store
                    .persist_pending_artifact(
                        &fence,
                        &payload,
                        &coverage,
                        &storage,
                        root.path(),
                        deadline_after(Duration::from_secs(60))
                    )
                    .await
                    .unwrap_err(),
                CaptureCatalogError::InvalidRequest,
                "the stale terminal fence must not recreate private evidence"
            );
            assert!(
                !complete().await.unwrap(),
                "an evicted receipt cannot complete again"
            );
            assert_eq!(
                session_row().await.state,
                current,
                "stale terminal redelivery cannot move phase/revision backwards"
            );
            assert_eq!(
                session_row().await.metadata_json,
                before_replay.metadata_json,
                "stale redelivery cannot mutate or recreate the evicted ledger entry"
            );
            assert_eq!(
                private_count().await,
                retained_after_completion,
                "ledger churn cannot resurrect cleaned evidence or aliases"
            );
        }
    }

    fn deadline_after(duration: Duration) -> CaptureCommitDeadline {
        let monotonic = Instant::now()
            .checked_add(duration)
            .expect("test deadline fits the monotonic clock");
        let absolute_millis = chrono::Utc::now()
            .timestamp_millis()
            .checked_add(
                i64::try_from(duration.as_millis()).expect("test deadline fits SQLite millis"),
            )
            .expect("test deadline fits SQLite millis");
        CaptureCommitDeadline::from_test_pair(monotonic, absolute_millis)
    }

    fn scope() -> CaptureScope {
        CaptureScope {
            repo_id: "repo-a".to_string(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        }
    }

    fn leased_scope() -> CaptureScope {
        CaptureScope {
            repo_id: "repo-a".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("workspace-a".to_string()),
            workspace_fence: Some(1),
        }
    }

    async fn seed_live_workspace_scope(conn: &DatabaseConnection) {
        conn.execute_unprepared(
            "INSERT INTO workspace_record (
                workspace_id, repo_id, lease_fence, state, lease_owner, lease_expires_at
             ) VALUES ('workspace-a', 'repo-a', 1, 'active', 'catalog-test-owner',
                       unixepoch('now') * 1000 + 60000)",
        )
        .await
        .expect("seed live workspace scope");
    }

    async fn expire_scope_after_agent_session_update(conn: &DatabaseConnection) {
        conn.execute_unprepared(
            "CREATE TRIGGER expire_catalog_scope_after_agent_session_update
             AFTER UPDATE ON agent_session
             BEGIN
                 UPDATE workspace_record
                    SET lease_expires_at = 0
                  WHERE workspace_id = 'workspace-a';
             END",
        )
        .await
        .expect("install scope-expiry trigger");
    }

    async fn drop_scope_expiry_trigger(conn: &DatabaseConnection) {
        conn.execute_unprepared("DROP TRIGGER expire_catalog_scope_after_agent_session_update")
            .await
            .expect("remove scope-expiry trigger");
    }

    fn session() -> CaptureCatalogSession {
        CaptureCatalogSession::new("session-a", "claude_code", "provider-a", "/repo")
            .expect("valid catalog session")
    }

    fn receipt(seed: char) -> OpaqueCaptureReceiptKey {
        OpaqueCaptureReceiptKey::parse(format!(
            "{RECEIPT_PREFIX_V1}{}",
            seed.to_string().repeat(64)
        ))
        .expect("valid opaque receipt")
    }

    #[test]
    fn opaque_receipt_parser_accepts_legacy_v1_and_current_v2_only() {
        for prefix in [RECEIPT_PREFIX_V1, RECEIPT_PREFIX_V2] {
            assert!(
                OpaqueCaptureReceiptKey::parse(format!("{prefix}{}", "a".repeat(64))).is_ok(),
                "catalog must retain readable receipts for {prefix}"
            );
        }
        for invalid in [
            format!("capture-dedup-v3:{}", "a".repeat(64)),
            format!("{RECEIPT_PREFIX_V2}{}", "A".repeat(64)),
            format!("{RECEIPT_PREFIX_V2}{}", "a".repeat(63)),
        ] {
            assert_eq!(
                OpaqueCaptureReceiptKey::parse(invalid),
                Err(CaptureCatalogError::InvalidReceiptKey),
                "only known opaque HMAC receipt grammars are acceptable"
            );
        }
    }

    fn valid_v2_import_metadata(snapshot: serde_json::Value) -> String {
        serde_json::json!({
            "repository_identity": SOURCE_IDENTITY_NOT_RETAINED,
            "source_kind": "file",
            "source_id": format!("{IMPORT_SOURCE_HMAC_V2_PREFIX}{}", "a".repeat(64)),
            "source_fingerprint": format!("{IMPORT_SOURCE_HMAC_V2_PREFIX}{}", "a".repeat(64)),
            "import_source_schema_version": 2,
            "import_provisional": false,
            "imported": true,
            "transcript_snapshot": snapshot,
        })
        .to_string()
    }

    fn valid_v2_import_snapshot() -> serde_json::Value {
        serde_json::json!({
            "completeness": "complete",
            "partial_reason": null,
            "source": {
                "kind": "trusted_export",
                "identity": SOURCE_IDENTITY_NOT_RETAINED,
                "digest_sha256": format!("{IMPORT_SOURCE_HMAC_V2_PREFIX}{}", "b".repeat(64)),
                "byte_len": 12,
            },
            "transcript_redacted_bytes": 12,
            "redaction_match_count": 0,
            "redaction_bytes_scanned": 12,
            "redaction_bytes_redacted": 0,
        })
    }

    fn valid_v2_import_redaction_report() -> String {
        serde_json::json!({
            "import": {
                "pipeline": "typed_allowlist",
                "snapshot_redaction": true,
                "raw_persisted": false,
                "matches": [],
                "bytes_scanned": 0,
                "bytes_redacted": 0,
            }
        })
        .to_string()
    }

    fn v2_import_source() -> CaptureImportSource {
        let source_id = format!("{IMPORT_SOURCE_HMAC_V2_PREFIX}{}", "a".repeat(64));
        CaptureImportSource::new(
            "file",
            source_id.clone(),
            SOURCE_IDENTITY_NOT_RETAINED,
            source_id,
            2,
            None,
        )
        .expect("valid V2 import source")
    }

    #[test]
    fn v2_live_adoption_rejects_partial_ownership_and_rebuilds_closed_metadata() {
        let source = v2_import_source();
        assert!(
            source
                .may_adopt_live_session_metadata("{}")
                .expect("empty live metadata is adoptable")
        );
        assert!(
            !source
                .may_adopt_live_session_metadata(r#"{"source_id":"partial"}"#)
                .expect("partial import metadata is parsed"),
            "a partial import record must not be treated as a live session"
        );

        let incarnation = "b".repeat(32);
        let live = StoredCatalogSession {
            session_id: "live-session".to_string(),
            working_dir: "/repo".to_string(),
            state: DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            },
            metadata_json: format!(
                r#"{{"capture_incarnation":"{}","concurrent_active":true,"legacy_locator":"/private/live-source.jsonl"}}"#,
                incarnation
            ),
            redaction_report: "{}".to_string(),
            scope_state: "scoped".to_string(),
            repo_id: Some("repo-a".to_string()),
            worktree_id: Some(String::new()),
            workspace_id: None,
            workspace_fence: None,
        };
        let metadata = closed_v2_metadata_from_live_session(&source, &live)
            .expect("the live conversion retains only typed extensions");
        let encoded = serde_json::to_string(&metadata).expect("serialize closed V2 metadata");
        assert!(
            validate_v2_import_session_metadata(&encoded).is_ok(),
            "the reconstructed row must be eligible for the V2 catalog/cloud contract"
        );
        assert!(
            !encoded.contains("legacy_locator") && !encoded.contains("/private/live-source.jsonl"),
            "live-only unknown metadata must not become durable import metadata"
        );
        assert!(metadata.concurrent_active == Some(true));
        assert_eq!(
            metadata.capture_incarnation.as_deref(),
            Some(incarnation.as_str())
        );
    }

    #[test]
    fn v2_live_adoption_rejects_pending_receipts_and_nonquiescent_state() {
        let source = v2_import_source();
        let action = action(72, None);
        let mutation = terminal_mutation(None);
        let receipt_key = action
            .receipt_storage_key(true)
            .expect("terminal action has a local receipt key");
        let ledger = StoredReceiptLedger {
            version: RECEIPT_LEDGER_VERSION,
            entries: vec![StoredReceipt::new(
                &receipt_key,
                &action,
                &mutation,
                StoredReceiptStatus::Pending,
                1,
            )],
        };
        let metadata_json = encode_receipt_metadata(serde_json::Map::new(), &ledger)
            .expect("encode valid pending terminal receipt");
        let mut live = StoredCatalogSession {
            session_id: "live-session".to_string(),
            working_dir: "/repo".to_string(),
            state: DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            },
            metadata_json,
            redaction_report: "{}".to_string(),
            scope_state: "scoped".to_string(),
            repo_id: Some("repo-a".to_string()),
            worktree_id: Some(String::new()),
            workspace_id: None,
            workspace_fence: None,
        };
        assert!(
            !live_session_is_adoptable(&source, &live)
                .expect("pending terminal receipt is parsed fail-closed"),
            "an import must not advance a receipt's reserved revision"
        );

        live.metadata_json = "{}".to_string();
        live.state.phase = CapturePhase::Pending;
        assert!(
            !live_session_is_adoptable(&source, &live)
                .expect("pending session phase is parsed fail-closed"),
            "an unfinished live session is not safe to relabel as an import"
        );
    }

    #[test]
    fn v2_import_snapshot_accepts_redaction_expansion_but_pins_source_scan_length() {
        let mut expanded = valid_v2_import_snapshot();
        // A 20-byte AWS access key becomes the 27-byte
        // `<REDACTED:aws-access-key-id>` placeholder. This remains within
        // the shared 1.5x source-specific redaction allowance.
        expanded["source"]["byte_len"] = serde_json::json!(20);
        expanded["transcript_redacted_bytes"] = serde_json::json!(27);
        expanded["redaction_match_count"] = serde_json::json!(1);
        expanded["redaction_bytes_scanned"] = serde_json::json!(20);
        expanded["redaction_bytes_redacted"] = serde_json::json!(20);
        let metadata = valid_v2_import_metadata(expanded.clone());
        assert!(
            validate_v2_import_session_metadata(&metadata).is_ok(),
            "a redaction placeholder may expand relative to the authorized source"
        );

        expanded["source"]["byte_len"] = serde_json::json!(19);
        let mismatched_source_length = valid_v2_import_metadata(expanded);
        assert_eq!(
            validate_v2_import_session_metadata(&mismatched_source_length),
            Err(CaptureCatalogError::InvalidRequest),
            "the source length must still match the redaction scan boundary"
        );
    }

    #[test]
    fn v2_import_snapshot_accepts_cap_sized_expansion_but_rejects_unbounded_output() {
        let raw_cap = TRANSCRIPT_READ_HARD_CAP_BYTES;
        let output_cap = snapshot_redacted_output_cap();
        let mut snapshot = valid_v2_import_snapshot();
        snapshot["source"]["byte_len"] = serde_json::json!(raw_cap);
        snapshot["transcript_redacted_bytes"] = serde_json::json!(output_cap);
        snapshot["redaction_match_count"] = serde_json::json!(1);
        snapshot["redaction_bytes_scanned"] = serde_json::json!(raw_cap);
        snapshot["redaction_bytes_redacted"] = serde_json::json!(20);
        assert!(
            validate_v2_import_session_metadata(&valid_v2_import_metadata(snapshot.clone()))
                .is_ok(),
            "a cap-sized authorized source may expand during redaction"
        );

        snapshot["transcript_redacted_bytes"] = serde_json::json!(output_cap.saturating_add(1));
        assert_eq!(
            validate_v2_import_session_metadata(&valid_v2_import_metadata(snapshot)),
            Err(CaptureCatalogError::InvalidRequest),
            "the shared 1.5x source-redaction cap must remain a durable bound"
        );
    }

    #[test]
    fn v2_import_snapshot_accepts_cumulative_redaction_metric_with_a_fixed_cap() {
        let mut snapshot = valid_v2_import_snapshot();
        snapshot["source"]["byte_len"] = serde_json::json!(20);
        snapshot["transcript_redacted_bytes"] = serde_json::json!(27);
        snapshot["redaction_match_count"] = serde_json::json!(1);
        snapshot["redaction_bytes_scanned"] = serde_json::json!(20);
        // A later redaction pass can see the expanding first replacement, so
        // this is intentionally larger than the original scan length.
        snapshot["redaction_bytes_redacted"] = serde_json::json!(28);
        assert!(
            validate_v2_import_session_metadata(&valid_v2_import_metadata(snapshot.clone()))
                .is_ok(),
            "cumulative redaction metrics are not bounded by the raw scan length"
        );

        let metric_cap = redaction_metric_cap(capture_redacted_output_cap(20));
        snapshot["redaction_bytes_redacted"] = serde_json::json!(metric_cap.saturating_add(1));
        assert_eq!(
            validate_v2_import_session_metadata(&valid_v2_import_metadata(snapshot.clone())),
            Err(CaptureCatalogError::InvalidRequest),
            "a cumulative metric still has a fixed pass-budget cap"
        );

        snapshot["redaction_bytes_redacted"] = serde_json::json!(28);
        snapshot["redaction_match_count"] =
            serde_json::json!(redaction_match_count_cap(metric_cap).saturating_add(1));
        assert_eq!(
            validate_v2_import_session_metadata(&valid_v2_import_metadata(snapshot)),
            Err(CaptureCatalogError::InvalidRequest),
            "the complete redaction-match count must remain bounded"
        );
    }

    #[test]
    fn v2_import_snapshot_rejects_global_cap_output_that_exceeds_its_source_bound() {
        let mut snapshot = valid_v2_import_snapshot();
        snapshot["source"]["byte_len"] = serde_json::json!(12);
        snapshot["transcript_redacted_bytes"] = serde_json::json!(20);
        snapshot["redaction_match_count"] = serde_json::json!(1);
        snapshot["redaction_bytes_scanned"] = serde_json::json!(12);
        snapshot["redaction_bytes_redacted"] = serde_json::json!(12);
        assert_eq!(
            validate_v2_import_session_metadata(&valid_v2_import_metadata(snapshot)),
            Err(CaptureCatalogError::InvalidRequest),
            "a cloud-restored V2 snapshot must satisfy the same source-specific bound as local capture"
        );
    }

    #[test]
    fn v2_import_report_accepts_evolving_offsets_but_rejects_fixed_cap_overflows() {
        let metadata = valid_v2_import_metadata(valid_v2_import_snapshot());
        let mut report: serde_json::Value =
            serde_json::from_str(&valid_v2_import_redaction_report())
                .expect("decode valid import redaction report");
        report["import"]["matches"] = serde_json::json!([
            {
                "rule_id": "aws-access-key-id",
                "start": 12,
                "end": 13,
            }
        ]);
        report["import"]["bytes_scanned"] = serde_json::json!(12);
        // This report merges two redaction stages. Its cumulative count and
        // local evolving offset are both valid even though they exceed the
        // aggregate raw scan count.
        report["import"]["bytes_redacted"] = serde_json::json!(13);
        assert!(
            validate_v2_import_session_record(&metadata, &report.to_string()).is_ok(),
            "a report offset is local to its evolving pass buffer, not the aggregate scan"
        );

        let mut over_coordinate = report.clone();
        over_coordinate["import"]["matches"][0]["end"] =
            serde_json::json!(import_redaction_coordinate_cap().saturating_add(1));
        assert_eq!(
            validate_v2_import_session_record(&metadata, &over_coordinate.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "evolving offsets still have the shared two-stage output cap"
        );

        let mut over_scanned = report.clone();
        over_scanned["import"]["bytes_scanned"] =
            serde_json::json!(import_redaction_scanned_cap().saturating_add(1));
        assert_eq!(
            validate_v2_import_session_record(&metadata, &over_scanned.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "the merged report scan metric must remain fixed-bounded"
        );

        let mut over_redacted = report.clone();
        over_redacted["import"]["bytes_redacted"] =
            serde_json::json!(import_redaction_metric_cap().saturating_add(1));
        assert_eq!(
            validate_v2_import_session_record(&metadata, &over_redacted.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "the merged cumulative redaction metric must remain fixed-bounded"
        );

        let mut over_dropped = report;
        over_dropped["import"]["dropped_matches"] =
            serde_json::json!(import_redaction_dropped_match_cap().saturating_add(1));
        assert_eq!(
            validate_v2_import_session_record(&metadata, &over_dropped.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "the merged dropped-match count must remain fixed-bounded"
        );
    }

    #[test]
    fn v2_import_metadata_validator_requires_closed_typed_redacted_snapshot() {
        let metadata = valid_v2_import_metadata(valid_v2_import_snapshot());
        assert!(validate_v2_import_session_metadata(&metadata).is_ok());
        assert!(sanitize_v2_import_session_metadata(&metadata).is_ok());
        assert!(
            validate_v2_import_session_record(&metadata, &valid_v2_import_redaction_report())
                .is_ok()
        );

        let mut nested_identity: serde_json::Value =
            serde_json::from_str(&metadata).expect("decode valid metadata");
        nested_identity["transcript_snapshot"]["source"]["identity"] =
            serde_json::Value::String("/private/provider/session.jsonl".to_string());
        assert_eq!(
            validate_v2_import_session_metadata(&nested_identity.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "a nested source identity must be the fixed noncorrelating sentinel"
        );

        let mut raw_locator: serde_json::Value =
            serde_json::from_str(&metadata).expect("decode valid metadata");
        raw_locator["transcript_snapshot"]["source"]["raw_locator"] =
            serde_json::Value::String("/private/provider/session.jsonl".to_string());
        assert_eq!(
            validate_v2_import_session_metadata(&raw_locator.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "unknown nested source fields must not become a locator channel"
        );

        let mut bare_digest: serde_json::Value =
            serde_json::from_str(&metadata).expect("decode valid metadata");
        bare_digest["transcript_snapshot"]["source"]["digest_sha256"] =
            serde_json::Value::String("b".repeat(64));
        assert_eq!(
            validate_v2_import_session_metadata(&bare_digest.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "ambiguous bare digests are legacy-only and cannot enter V2 metadata"
        );

        let mut unknown_top_level: serde_json::Value =
            serde_json::from_str(&metadata).expect("decode valid metadata");
        unknown_top_level["transcript_path"] =
            serde_json::Value::String("/private/provider/session.jsonl".to_string());
        assert_eq!(
            validate_v2_import_session_metadata(&unknown_top_level.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "unknown V2 metadata keys must be rejected rather than preserved"
        );

        let receipt_action = action(0xcafe, Some(receipt('e')));
        let receipt_mutation = terminal_mutation(None);
        let mut receipt_entry = StoredReceipt::new(
            &receipt_action.completion_receipt_storage_key(),
            &receipt_action,
            &receipt_mutation,
            StoredReceiptStatus::Pending,
            1,
        );
        let finalizer = PendingFinalizeReceipt::new(
            &finalizer_policy(&receipt_action, None),
            Uuid::from_u128(0xcafebabe).to_string(),
            Some(format!("{IMPORT_SOURCE_HMAC_V2_PREFIX}{}", "c".repeat(64))),
            1,
            FinalizePendingStage::Snapshot,
        )
        .expect("valid tagged finalizer digest");
        receipt_entry.finalizer = Some(StoredFinalizeReceipt::from_pending(&finalizer));
        let mut nested_receipt =
            serde_json::from_str::<serde_json::Value>(&metadata).expect("decode valid metadata");
        nested_receipt[RECEIPT_LEDGER_FIELD] = serde_json::to_value(StoredReceiptLedger {
            version: RECEIPT_LEDGER_VERSION,
            entries: vec![receipt_entry],
        })
        .expect("encode valid receipt ledger");
        assert!(
            validate_v2_import_session_metadata(&nested_receipt.to_string()).is_ok(),
            "a tagged finalizer digest and canonical marker are valid V2 receipt evidence"
        );
        nested_receipt[RECEIPT_LEDGER_FIELD]["entries"][0]["finalizer"]["source_digest"] =
            serde_json::Value::String("c".repeat(64));
        assert_eq!(
            validate_v2_import_session_metadata(&nested_receipt.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "a bare digest inside a V2 receipt ledger is not legacy-readable V2 metadata"
        );
        nested_receipt[RECEIPT_LEDGER_FIELD]["entries"][0]["finalizer"]["source_digest"] =
            serde_json::Value::String(format!("{IMPORT_SOURCE_HMAC_V2_PREFIX}{}", "c".repeat(64)));
        nested_receipt[RECEIPT_LEDGER_FIELD]["entries"][0]["finalizer"]["marker_generation"] =
            serde_json::Value::String("/private/marker".to_string());
        assert_eq!(
            validate_v2_import_session_metadata(&nested_receipt.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "a finalizer marker must be a canonical opaque UUID, not a raw locator"
        );

        let mut raw_redaction: serde_json::Value =
            serde_json::from_str(&valid_v2_import_redaction_report())
                .expect("decode valid redaction report");
        raw_redaction["import"]["raw_locator"] =
            serde_json::Value::String("/private/provider/session.jsonl".to_string());
        assert_eq!(
            validate_v2_import_session_record(&metadata, &raw_redaction.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "an unknown redaction-report field must not become a raw locator channel"
        );
        let mut raw_rule: serde_json::Value =
            serde_json::from_str(&valid_v2_import_redaction_report())
                .expect("decode valid redaction report");
        raw_rule["import"]["matches"] = serde_json::json!([
            { "rule_id": "/private/provider/session.jsonl", "start": 0, "end": 1 }
        ]);
        raw_rule["import"]["bytes_scanned"] = serde_json::Value::from(1);
        assert_eq!(
            validate_v2_import_session_record(&metadata, &raw_rule.to_string()),
            Err(CaptureCatalogError::InvalidRequest),
            "a report sample must use a shipped rule ID rather than caller text"
        );
    }

    #[test]
    fn v2_import_validators_reject_duplicate_keys_before_last_key_wins_parsing() {
        let metadata = valid_v2_import_metadata(valid_v2_import_snapshot());
        let duplicate_top_level = metadata.replacen(
            &format!("\"repository_identity\":\"{SOURCE_IDENTITY_NOT_RETAINED}\""),
            &format!(
                "\"repository_identity\":\"/private/provider/session.jsonl\",\
                 \"repository_identity\":\"{SOURCE_IDENTITY_NOT_RETAINED}\""
            ),
            1,
        );
        assert_eq!(
            validate_v2_import_session_metadata(&duplicate_top_level),
            Err(CaptureCatalogError::InvalidRequest),
            "a duplicated top-level key must not preserve a hidden raw locator"
        );

        let duplicate_nested_source = metadata.replacen(
            &format!("\"identity\":\"{SOURCE_IDENTITY_NOT_RETAINED}\""),
            &format!(
                "\"identity\":\"/private/provider/session.jsonl\",\
                 \"identity\":\"{SOURCE_IDENTITY_NOT_RETAINED}\""
            ),
            1,
        );
        assert_eq!(
            validate_v2_import_session_metadata(&duplicate_nested_source),
            Err(CaptureCatalogError::InvalidRequest),
            "the duplicate-key guard must recurse into transcript snapshots"
        );

        let report = valid_v2_import_redaction_report();
        let duplicate_import_wrapper = report.replacen(
            "\"import\":",
            "\"import\":{\"raw_locator\":\"/private/provider/session.jsonl\"},\"import\":",
            1,
        );
        assert_eq!(
            validate_v2_import_session_record(&metadata, &duplicate_import_wrapper),
            Err(CaptureCatalogError::InvalidRequest),
            "a duplicated report wrapper must not survive cloud copy as raw JSON"
        );

        let duplicate_pipeline = report.replacen(
            "\"pipeline\":\"typed_allowlist\"",
            "\"pipeline\":\"/private/provider/session.jsonl\",\
             \"pipeline\":\"typed_allowlist\"",
            1,
        );
        assert_eq!(
            validate_v2_import_session_record(&metadata, &duplicate_pipeline),
            Err(CaptureCatalogError::InvalidRequest),
            "the duplicate-key guard must recurse into the typed report"
        );
    }

    const TEST_SOURCE_DIGEST_A: &str = concat!(
        "source/hmac-v2/",
        "1111111111111111",
        "1111111111111111",
        "1111111111111111",
        "1111111111111111",
    );
    const TEST_SOURCE_DIGEST_X: &str = concat!(
        "source/hmac-v2/",
        "2222222222222222",
        "2222222222222222",
        "2222222222222222",
        "2222222222222222",
    );
    const TEST_SOURCE_DIGEST_Y: &str = concat!(
        "source/hmac-v2/",
        "3333333333333333",
        "3333333333333333",
        "3333333333333333",
        "3333333333333333",
    );
    const TEST_SOURCE_DIGEST_ELECTED: &str = concat!(
        "source/hmac-v2/",
        "4444444444444444",
        "4444444444444444",
        "4444444444444444",
        "4444444444444444",
    );
    const TEST_SOURCE_DIGEST_LATER: &str = concat!(
        "source/hmac-v2/",
        "5555555555555555",
        "5555555555555555",
        "5555555555555555",
        "5555555555555555",
    );

    fn action(seed: u128, receipt_key: Option<OpaqueCaptureReceiptKey>) -> CaptureCatalogAction {
        CaptureCatalogAction::lifecycle(Uuid::from_u128(seed), receipt_key)
    }

    fn ingress_action(
        seed: u128,
        receipt_key: Option<OpaqueCaptureReceiptKey>,
        lifecycle_kind: LifecycleEventKind,
    ) -> CaptureCatalogAction {
        CaptureCatalogAction::from_ingress(
            Uuid::from_u128(seed),
            receipt_key.as_ref().map(OpaqueCaptureReceiptKey::as_str),
            lifecycle_kind,
        )
        .expect("valid ingress catalog action")
    }

    fn mutation(
        expected: Option<DurableCaptureState>,
        checkpoint: CheckpointWrite,
    ) -> CaptureCatalogMutation {
        CaptureCatalogMutation::new(
            expected,
            CapturePhase::Active,
            StoppedAtMutation::Preserve,
            checkpoint,
            1_700_000_000,
        )
        .expect("valid state mutation")
    }

    fn terminal_mutation(expected: Option<DurableCaptureState>) -> CaptureCatalogMutation {
        CaptureCatalogMutation::new(
            expected,
            CapturePhase::Stopped,
            StoppedAtMutation::Set(1_700_000_010),
            CheckpointWrite::Committed,
            1_700_000_010,
        )
        .expect("valid terminal mutation")
    }

    async fn install_catalog_test_schema(conn: &DatabaseConnection) {
        for statement in [
            "CREATE TABLE config_kv (id TEXT PRIMARY KEY)",
            "CREATE TABLE agent_session (
                session_id TEXT PRIMARY KEY,
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL,
                state TEXT NOT NULL,
                working_dir TEXT NOT NULL,
                metadata_json TEXT NOT NULL DEFAULT '{}',
                redaction_report TEXT NOT NULL DEFAULT '{}',
                started_at INTEGER NOT NULL,
                last_event_at INTEGER NOT NULL,
                stopped_at INTEGER,
                sync_revision INTEGER NOT NULL DEFAULT 0,
                repo_id TEXT,
                worktree_id TEXT,
                workspace_id TEXT,
                workspace_fence INTEGER,
                scope_state TEXT NOT NULL,
                UNIQUE(agent_kind, provider_session_id)
            )",
            "CREATE TABLE agent_export_job (
                provider_session_id TEXT,
                scope_state TEXT,
                repo_id TEXT,
                worktree_id TEXT,
                workspace_id TEXT,
                workspace_fence INTEGER
            )",
            "CREATE TABLE agent_checkpoint (
                checkpoint_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                scope TEXT NOT NULL
            )",
            "CREATE TABLE agent_import_identity (
                provider_session_id TEXT,
                scope_state TEXT,
                repo_id TEXT,
                worktree_id TEXT,
                workspace_id TEXT,
                workspace_fence INTEGER
            )",
            "CREATE TABLE agent_import_tombstone (
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL
            )",
            "CREATE TABLE agent_capture_incarnation (
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL,
                next_session_sync_revision INTEGER NOT NULL,
                source_namespace TEXT NOT NULL,
                PRIMARY KEY(agent_kind, provider_session_id)
            )",
            "CREATE TABLE workspace_record (
                workspace_id TEXT PRIMARY KEY,
                repo_id TEXT NOT NULL,
                lease_fence INTEGER NOT NULL,
                state TEXT NOT NULL,
                lease_owner TEXT,
                lease_expires_at INTEGER
            )",
            "CREATE TABLE metadata_kv (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                scope TEXT NOT NULL,
                target TEXT NOT NULL,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                value_type TEXT NOT NULL DEFAULT 'text',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL,
                UNIQUE(scope, target, key)
            )",
        ] {
            conn.execute_unprepared(statement)
                .await
                .expect("create catalog test schema");
        }
    }

    async fn catalog_db() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open memory database");
        install_catalog_test_schema(&conn).await;
        conn
    }

    async fn connect_catalog_file_db(path: &Path) -> DatabaseConnection {
        let mut options = ConnectOptions::new(format!("sqlite://{}", path.display()));
        options.sqlx_logging(false);
        options.map_sqlx_sqlite_opts(|sqlite| sqlite.busy_timeout(Duration::from_secs(5)));
        Database::connect(options)
            .await
            .expect("open file-backed catalog test database")
    }

    async fn file_catalog_db() -> (TempDir, DatabaseConnection, DatabaseConnection) {
        let directory = tempfile::tempdir().expect("create file-backed catalog test directory");
        let path = directory.path().join("catalog.sqlite");
        std::fs::File::create(&path).expect("create file-backed catalog test database");
        let writer = connect_catalog_file_db(&path).await;
        install_catalog_test_schema(&writer).await;
        let locker = connect_catalog_file_db(&path).await;
        for conn in [&writer, &locker] {
            conn.execute_unprepared("PRAGMA journal_mode = DELETE")
                .await
                .expect("force rollback-journal mode for catalog lock regression");
        }
        (directory, writer, locker)
    }

    async fn session_revision(conn: &DatabaseConnection) -> i64 {
        conn.query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT sync_revision FROM agent_session WHERE provider_session_id = 'provider-a'",
            [],
        ))
        .await
        .expect("read session")
        .expect("session row")
        .try_get_by("sync_revision")
        .expect("revision")
    }

    async fn session_state(conn: &DatabaseConnection) -> (String, Option<i64>, i64) {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT state, stopped_at, sync_revision FROM agent_session \
                 WHERE provider_session_id = 'provider-a'",
                [],
            ))
            .await
            .expect("read session")
            .expect("session row");
        (
            row.try_get_by("state").expect("state"),
            row.try_get_by("stopped_at").expect("stopped timestamp"),
            row.try_get_by("sync_revision").expect("revision"),
        )
    }

    async fn session_catalog_snapshot(
        conn: &DatabaseConnection,
    ) -> (String, Option<i64>, i64, String, String) {
        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT state, stopped_at, sync_revision, metadata_json, redaction_report
                 FROM agent_session WHERE provider_session_id = 'provider-a'",
                [],
            ))
            .await
            .expect("read catalog session")
            .expect("catalog session row");
        (
            row.try_get_by("state").expect("state"),
            row.try_get_by("stopped_at").expect("stopped timestamp"),
            row.try_get_by("sync_revision").expect("revision"),
            row.try_get_by("metadata_json").expect("receipt metadata"),
            row.try_get_by("redaction_report")
                .expect("redaction report"),
        )
    }

    async fn seed_committed_v2_import_session(conn: &DatabaseConnection) -> (String, String) {
        let seeded_session = session();
        let metadata = valid_v2_import_metadata(valid_v2_import_snapshot());
        let redaction_report = valid_v2_import_redaction_report();
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO agent_session (
                session_id, agent_kind, provider_session_id, state, working_dir,
                metadata_json, redaction_report, started_at, last_event_at, stopped_at,
                sync_revision, repo_id, worktree_id, workspace_id, workspace_fence, scope_state
             ) VALUES (?, ?, ?, 'active', ?, ?, ?, 1, 1, NULL, 1, ?, ?, ?, ?, 'scoped')",
            [
                seeded_session.session_id.into(),
                seeded_session.agent_kind.into(),
                seeded_session.provider_session_id.into(),
                seeded_session.working_dir.into(),
                metadata.clone().into(),
                redaction_report.clone().into(),
                "repo-a".into(),
                "".into(),
                Option::<String>::None.into(),
                Option::<i64>::None.into(),
            ],
        ))
        .await
        .expect("seed a committed V2 import row");
        (metadata, redaction_report)
    }

    #[tokio::test]
    async fn generic_hook_apply_preserves_a_closed_v2_import_record() {
        let conn = catalog_db().await;
        let seeded_session = session();
        let (_metadata, redaction_report) = seed_committed_v2_import_session(&conn).await;

        let generic_hook_report =
            CaptureCatalogRedactionReport::from_report(&RedactionReport::default());
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            seeded_session,
            ingress_action(0xfeed, Some(receipt('a')), LifecycleEventKind::TurnEnd),
            mutation(
                Some(DurableCaptureState {
                    phase: CapturePhase::Active,
                    stopped_at: None,
                    sync_revision: 1,
                }),
                CheckpointWrite::None,
            ),
        )
        .expect("normal hook-shaped request")
        .with_metadata(CaptureCatalogMetadataPatch::new(
            true,
            Some(generic_hook_report),
        ));

        let store = CaptureCatalogStore::new(conn.clone());
        assert!(matches!(
            store
                .apply(&request)
                .await
                .expect("apply generic hook update"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Complete,
                ..
            }
        ));

        let (_, _, _, metadata_after, report_after) = session_catalog_snapshot(&conn).await;
        assert_eq!(
            report_after, redaction_report,
            "a generic hook report must not replace the closed V2 import report"
        );
        assert!(
            validate_v2_import_session_record(&metadata_after, &report_after).is_ok(),
            "the retained row must stay eligible for cloud publish and restore"
        );
        let metadata_value: serde_json::Value =
            serde_json::from_str(&metadata_after).expect("decode retained V2 metadata");
        assert_eq!(
            metadata_value.get("concurrent_active"),
            Some(&serde_json::Value::Bool(true)),
            "the allowed generic lifecycle bit still merges into the V2 record"
        );
    }

    #[tokio::test]
    async fn v2_import_finalizer_rejects_noncanonical_marker_and_retains_closed_record() {
        let conn = catalog_db().await;
        let (_metadata, redaction_report) = seed_committed_v2_import_session(&conn).await;
        let store = CaptureCatalogStore::new(conn.clone());
        let terminal_action =
            ingress_action(0xf1a1, Some(receipt('b')), LifecycleEventKind::SessionEnd);
        let terminal_request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            terminal_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request over imported V2 session");
        assert!(matches!(
            store
                .apply(&terminal_request)
                .await
                .expect("reserve imported V2 terminal receipt"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));
        let before_bad_marker = session_catalog_snapshot(&conn).await;

        let bad_marker = CaptureCatalogFinalizeRequest::new(
            scope(),
            session(),
            terminal_action.clone(),
            CheckpointWrite::Committed,
            finalizer_policy(&terminal_action, None),
            "not-a-canonical-v2-marker",
            Some(TEST_SOURCE_DIGEST_A.to_string()),
            1,
            FinalizeCheckpointProgress::NotStarted,
        )
        .expect("generic finalizer request remains valid before the V2 record gate");
        assert_eq!(
            store.finalize(&bad_marker).await,
            Err(CaptureCatalogError::InvalidRequest),
            "a V2 ledger may not persist an arbitrary marker string"
        );
        assert_eq!(
            session_catalog_snapshot(&conn).await,
            before_bad_marker,
            "a rejected V2 finalizer marker must leave the durable record unchanged"
        );

        let marker = Uuid::from_u128(0xf1a1).to_string();
        let pending = CaptureCatalogFinalizeRequest::new(
            scope(),
            session(),
            terminal_action.clone(),
            CheckpointWrite::Committed,
            finalizer_policy(&terminal_action, None),
            marker.clone(),
            Some(TEST_SOURCE_DIGEST_A.to_string()),
            2,
            FinalizeCheckpointProgress::NotStarted,
        )
        .expect("canonical V2 finalizer request");
        assert!(matches!(
            store
                .finalize(&pending)
                .await
                .expect("persist canonical marker"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let durable = CaptureCatalogFinalizeRequest::new(
            scope(),
            session(),
            terminal_action.clone(),
            CheckpointWrite::Committed,
            finalizer_policy(&terminal_action, None),
            marker,
            Some(TEST_SOURCE_DIGEST_A.to_string()),
            3,
            FinalizeCheckpointProgress::Durable,
        )
        .expect("canonical durable-finalizer request");
        let proof = match store
            .finalize(&durable)
            .await
            .expect("derive canonical finalizer proof")
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            other => panic!("expected durable finalizer proof, got {other:?}"),
        };
        let completion = CaptureCatalogCompleteRequest::from_finalizer(&terminal_request, proof)
            .expect("V2 completion request");
        assert_eq!(
            store
                .complete(&completion)
                .await
                .expect("complete V2 terminal"),
            CaptureCatalogCompleteResult::Completed
        );
        assert!(
            store
                .update_diagnostic(
                    &terminal_request,
                    CaptureCatalogDiagnostic::RecordRetryableCheckpointFailure {
                        stage: CaptureCatalogRetryableStage::CheckpointWrite,
                        failed_at: 4,
                    },
                )
                .await
                .expect("write allowlisted V2 diagnostic"),
            "the typed diagnostic update should remain available"
        );
        let (_, _, _, metadata_after, report_after) = session_catalog_snapshot(&conn).await;
        assert_eq!(report_after, redaction_report);
        assert!(
            validate_v2_import_session_record(&metadata_after, &report_after).is_ok(),
            "canonical finalizer, completion, and diagnostic updates retain a cloud-valid V2 row"
        );
    }

    #[tokio::test]
    async fn v2_import_lifecycle_rejects_pending_live_finalizer_without_stranding_recovery() {
        let conn = catalog_db().await;
        seed_committed_v2_import_session(&conn).await;
        let store = CaptureCatalogStore::new(conn.clone());
        let terminal_action =
            ingress_action(0xf1a2, Some(receipt('c')), LifecycleEventKind::SessionEnd);
        let terminal_request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            terminal_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request over adopted V2 session");
        assert!(matches!(
            store
                .apply(&terminal_request)
                .await
                .expect("reserve adopted V2 terminal receipt"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));
        let marker = Uuid::from_u128(0xf1a2).to_string();
        let pending = CaptureCatalogFinalizeRequest::new(
            scope(),
            session(),
            terminal_action.clone(),
            CheckpointWrite::Committed,
            finalizer_policy(&terminal_action, None),
            marker,
            Some(TEST_SOURCE_DIGEST_A.to_string()),
            2,
            FinalizeCheckpointProgress::NotStarted,
        )
        .expect("canonical pending finalizer request");
        assert!(matches!(
            store
                .finalize(&pending)
                .await
                .expect("persist pending terminal finalizer"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let before = session_catalog_snapshot(&conn).await;

        let import_report = CaptureCatalogRedactionReport::from_import_value(&serde_json::json!({
            "pipeline": "typed_allowlist",
            "snapshot_redaction": true,
            "raw_persisted": false,
            "matches": [],
            "bytes_scanned": 0,
            "bytes_redacted": 0,
        }))
        .expect("valid typed import report");
        let commit = CaptureImportSessionCommit::new(
            scope(),
            session(),
            v2_import_source(),
            import_report,
            CaptureImportSessionLifecycleState::Active,
            1,
            2,
            None,
        )
        .expect("valid import lifecycle commit");
        let txn = conn.begin().await.expect("begin rejected import lifecycle");
        assert_eq!(
            CaptureCatalogStore::apply_import_session_lifecycle(&txn, &commit).await,
            Err(CaptureCatalogError::ImportSessionConflict),
            "a V2 replay must not advance a pending live terminal receipt"
        );
        txn.rollback()
            .await
            .expect("rollback rejected import lifecycle");
        assert_eq!(
            session_catalog_snapshot(&conn).await,
            before,
            "the rejected import must leave the live finalizer fence byte-identical"
        );
        let recoveries = store
            .pending_finalizer_recoveries_for_doctor(3)
            .await
            .expect("scan the original pending finalizer")
            .recoveries;
        assert_eq!(recoveries.len(), 1);
        assert_eq!(recoveries[0].action.event_id, terminal_action.event_id);
        assert_eq!(
            recoveries[0].action.completion_receipt_storage_key(),
            terminal_action.completion_receipt_storage_key()
        );
    }

    async fn agent_session_count(conn: &DatabaseConnection) -> i64 {
        conn.query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "SELECT COUNT(*) AS count FROM agent_session",
            [],
        ))
        .await
        .expect("count catalog sessions")
        .expect("count row")
        .try_get_by("count")
        .expect("session count")
    }

    async fn seed_doctor_pending_source(
        conn: &DatabaseConnection,
        index: usize,
    ) -> CaptureCatalogApplyRequest {
        let session = CaptureCatalogSession::new(
            format!("session-doctor-{index:04}"),
            "claude_code",
            format!("provider-doctor-{index:04}"),
            "/absent-provider-source",
        )
        .unwrap();
        let stop_action = action(100_000 + index as u128, None);
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session,
            stop_action.clone(),
            terminal_mutation(None),
        )
        .unwrap();
        let store = CaptureCatalogStore::new(conn.clone());
        store.apply(&stop).await.unwrap();
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &stop,
                    finalizer_policy(&stop_action, None),
                    &Uuid::new_v4().to_string(),
                    None,
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .unwrap(),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        stop
    }

    #[tokio::test]
    async fn doctor_finalizer_scan_pages_past_malformed_rows_without_hydrating_oversize_values() {
        let conn = catalog_db().await;
        for index in 0..40 {
            seed_doctor_pending_source(&conn, index).await;
        }
        // Corrupt native fields, including SQLite TEXT with invalid UTF-8,
        // without giving doctor any provider source or private key.
        for (key, field, value) in [
            ("00-bad-json", "metadata_json", "'{\"x\":1,\"x\":2}'"),
            ("01-bad-utf8", "metadata_json", "CAST(x'ff' AS TEXT)"),
            (
                "02-big-json",
                "metadata_json",
                "CAST(zeroblob(1048577) AS TEXT)",
            ),
            ("03-big-dir", "working_dir", "CAST(zeroblob(4097) AS TEXT)"),
            ("04-bad-revision", "sync_revision", "'not-an-integer'"),
            (
                "05-big-stopped",
                "stopped_at",
                "CAST(zeroblob(1048577) AS TEXT)",
            ),
            (
                "06-big-workspace",
                "workspace_fence",
                "CAST(zeroblob(1048577) AS TEXT)",
            ),
        ] {
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO agent_session SELECT ?, agent_kind, ?, state, working_dir,
                    metadata_json, redaction_report, started_at, last_event_at, stopped_at,
                    sync_revision, repo_id, worktree_id, workspace_id, workspace_fence,
                    scope_state FROM agent_session WHERE session_id='session-doctor-0000'",
                [key.into(), key.into()],
            ))
            .await
            .unwrap();
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!("UPDATE agent_session SET {field}={value} WHERE session_id=?"),
                [key.into()],
            ))
            .await
            .unwrap();
        }
        let rows = conn
            .query_all_raw(doctor_finalizer_scan_statement(
                DbBackend::Sqlite,
                None,
                false,
                32,
            ))
            .await
            .unwrap();
        assert_eq!(rows.len(), 32);
        for (index, name) in [(2, "metadata_json"), (3, "working_dir")] {
            assert!(
                rows[index]
                    .try_get_by::<Option<Vec<u8>>, _>(name)
                    .unwrap()
                    .is_none()
            );
        }
        for (index, name) in [(5, "stopped_at"), (6, "workspace_fence")] {
            assert!(
                rows[index]
                    .try_get_by::<Option<i64>, _>(name)
                    .unwrap()
                    .is_none()
            );
        }
        // Oversized keys sort before valid sessions. The bounded-key filter
        // must still reach valid rows after this short malformed-key page.
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE agent_session SET session_id=? WHERE session_id='03-big-dir'",
            [format!("000{}", "k".repeat(1025)).into()],
        ))
        .await
        .unwrap();
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "UPDATE agent_session SET session_id=CAST(x'fe' AS TEXT)
             WHERE session_id='01-bad-utf8'",
            [],
        ))
        .await
        .unwrap();
        let scan = CaptureCatalogStore::new(conn.clone())
            .pending_finalizer_recoveries_for_doctor(3)
            .await
            .unwrap();
        assert_eq!(scan.recoveries.len(), 40);
        assert_eq!(scan.malformed_rows, 7);
        assert!(scan.skipped_invalid_keys);
        assert!(!scan.truncated);
        assert!(scan.recoveries.iter().all(|r| !r.artifact_present()));

        let statement = doctor_finalizer_scan_statement(
            DbBackend::Sqlite,
            Some(b"session-doctor-0000"),
            true,
            32,
        );
        let query_plan = conn
            .query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                format!("EXPLAIN QUERY PLAN {}", statement.sql),
                statement.values.unwrap().0,
            ))
            .await
            .unwrap();
        assert!(query_plan.iter().any(|r| {
            let detail: String = r.try_get_by("detail").unwrap();
            detail.contains("SEARCH agent_session") && detail.contains("session_id>?")
        }));
    }

    #[tokio::test]
    async fn doctor_finalizer_scan_keeps_superseded_pending_source_read_only() {
        let (directory, conn, _) = file_catalog_db().await;
        let stop = seed_doctor_pending_source(&conn, 0).await;
        let store = CaptureCatalogStore::new(conn.clone());
        let txn = conn.begin().await.unwrap();
        let current = read_session(&txn, &stop.session).await.unwrap().unwrap();
        txn.commit().await.unwrap();
        let resume = CaptureCatalogApplyRequest::new(
            scope(),
            stop.session.clone(),
            action(200_000, None),
            mutation(Some(current.state), CheckpointWrite::None),
        )
        .unwrap();
        store.apply(&resume).await.unwrap();
        let path = directory.path().join("catalog.sqlite");
        let before = std::fs::read(&path).unwrap();
        let scan = store
            .pending_finalizer_recoveries_for_doctor(3)
            .await
            .unwrap();
        assert_eq!(scan.recoveries.len(), 1);
        assert!(scan.recoveries[0].manual_only());
        assert!(!scan.recoveries[0].artifact_present());
        assert!(!scan.recoveries[0].budget_exhausted());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert!(!directory.path().join("capture.key").exists());
        assert!(
            conn.query_all_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT 1 FROM metadata_kv",
                [],
            ))
            .await
            .unwrap()
            .is_empty()
        );
    }

    #[tokio::test]
    async fn doctor_finalizer_scan_pins_output_and_session_bounds() {
        for count in [128, 129] {
            let conn = catalog_db().await;
            for index in 0..count {
                seed_doctor_pending_source(&conn, index).await;
            }
            let scan = CaptureCatalogStore::new(conn)
                .pending_finalizer_recoveries_for_doctor(3)
                .await
                .unwrap();
            assert_eq!(scan.recoveries.len(), 128);
            assert_eq!(scan.truncated, count > 128);
        }
        let conn = catalog_db().await;
        seed_doctor_pending_source(&conn, 0).await;
        // Healthy non-finalizer rows consume the cold scan's session budget.
        // The finalizer sorts beyond 512 and must not be silently called clean.
        for index in 0..512 {
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO agent_session SELECT ?, agent_kind, ?, state, working_dir,
                    '{}', redaction_report, started_at, last_event_at, stopped_at,
                    sync_revision, repo_id, worktree_id, workspace_id, workspace_fence,
                    scope_state FROM agent_session WHERE session_id='session-doctor-0000'",
                [
                    format!("00-healthy-{index:04}").into(),
                    format!("healthy-{index:04}").into(),
                ],
            ))
            .await
            .unwrap();
        }
        let scan = CaptureCatalogStore::new(conn)
            .pending_finalizer_recoveries_for_doctor(3)
            .await
            .unwrap();
        assert!(scan.recoveries.is_empty());
        assert!(scan.truncated);
        assert_eq!(scan.malformed_rows, 0);
    }

    #[tokio::test]
    async fn pending_receipt_resumes_then_completes_without_second_state_mutation() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let action = action(1, Some(receipt('a')));
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action.clone(),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("request");

        assert!(matches!(
            store.apply(&request).await.expect("first apply"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                checkpoint: CheckpointWrite::Committed,
                ..
            }
        ));
        assert_eq!(session_revision(&conn).await, 1);

        assert!(matches!(
            store.apply(&request).await.expect("same receipt"),
            CaptureCatalogApplyResult::ResumePending {
                checkpoint: CheckpointWrite::Committed,
                ..
            }
        ));
        assert_eq!(session_revision(&conn).await, 1);

        let complete = CaptureCatalogCompleteRequest::new(scope(), session(), action)
            .expect("completion request");
        assert_eq!(
            store.complete(&complete).await.expect("complete receipt"),
            CaptureCatalogCompleteResult::Completed
        );
        assert!(matches!(
            store.apply(&request).await.expect("completed replay"),
            CaptureCatalogApplyResult::AlreadyApplied
        ));
        assert_eq!(session_revision(&conn).await, 1);
    }

    #[tokio::test]
    async fn catalog_deadline_before_first_dml_publishes_no_session_or_receipt() {
        let conn = catalog_db().await;
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(0xdeda, Some(receipt('e'))),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("pre-DML deadline apply request");
        let store =
            CaptureCatalogStore::new_until(conn.clone(), deadline_after(Duration::from_millis(20)));

        let error = with_catalog_before_dml_delay(Duration::from_millis(80), store.apply(&request))
            .await
            .expect_err("a deadline crossed during pre-DML work must reject the mutation");
        assert_eq!(error, CaptureCatalogError::DeadlineExceeded);
        assert_eq!(
            agent_session_count(&conn).await,
            0,
            "a rejected first mutation must publish neither a session revision nor its receipt"
        );
    }

    #[tokio::test]
    async fn catalog_deadline_bounds_file_sqlite_writer_acquisition_without_delayed_apply() {
        let (_directory, conn, locker) = file_catalog_db().await;
        let backend = locker.get_database_backend();
        locker
            .execute_raw(Statement::from_string(
                backend,
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive catalog writer lock");
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(0xded0, Some(receipt('f'))),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("file-lock catalog apply request");
        let store =
            CaptureCatalogStore::new_until(conn.clone(), deadline_after(Duration::from_millis(30)));
        let started = Instant::now();
        let apply = tokio::time::timeout(Duration::from_secs(2), store.apply(&request))
            .await
            .expect("catalog writer acquisition must honor its deadline");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK".to_string()))
            .await
            .expect("release exclusive catalog writer lock");

        assert_eq!(apply, Err(CaptureCatalogError::DeadlineExceeded));
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "catalog writer acquisition must not wait for SQLite's busy timeout"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            agent_session_count(&conn).await,
            0,
            "a cancelled writer acquisition must not publish a delayed session or receipt"
        );
    }

    #[tokio::test]
    async fn catalog_deadline_after_final_fence_rolls_back_apply_and_completion() {
        let conn = catalog_db().await;
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(0xdead, Some(receipt('d'))),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("deadline apply request");
        let apply_store =
            CaptureCatalogStore::new_until(conn.clone(), deadline_after(Duration::from_millis(20)));
        let apply_error =
            with_catalog_final_fence_delay(Duration::from_millis(80), apply_store.apply(&request))
                .await
                .expect_err("post-fence deadline must roll back the fresh catalog apply");
        assert_eq!(apply_error, CaptureCatalogError::DeadlineExceeded);
        assert_eq!(
            agent_session_count(&conn).await,
            0,
            "expired apply must not create a session or pending receipt"
        );

        let ordinary_store = CaptureCatalogStore::new(conn.clone());
        assert!(matches!(
            ordinary_store
                .apply(&request)
                .await
                .expect("ordinary apply"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));
        let before_completion = session_catalog_snapshot(&conn).await;
        let completion =
            CaptureCatalogCompleteRequest::new(scope(), session(), request.action().clone())
                .expect("deadline completion request");
        let completion_store =
            CaptureCatalogStore::new_until(conn.clone(), deadline_after(Duration::from_millis(20)));
        let completion_error = with_catalog_final_fence_delay(
            Duration::from_millis(80),
            completion_store.complete(&completion),
        )
        .await
        .expect_err("post-fence deadline must roll back receipt completion");
        assert_eq!(completion_error, CaptureCatalogError::DeadlineExceeded);
        assert_eq!(
            session_catalog_snapshot(&conn).await,
            before_completion,
            "expired completion must leave the session and its pending receipt byte-identical"
        );
    }

    #[tokio::test]
    async fn catalog_final_authorization_rejects_an_expired_sqlite_half_after_mutation() {
        let conn = catalog_db().await;
        seed_live_workspace_scope(&conn).await;
        let request = CaptureCatalogApplyRequest::new(
            leased_scope(),
            session(),
            action(0xbeef, Some(receipt('e'))),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("wall-expired catalog request");
        // Keep the process-side half live. Only the SQLite-side final
        // authorization may reject this transaction, after the reducer has
        // prepared its session/receipt writes but before COMMIT is issued.
        let deadline = CaptureCommitDeadline::from_test_pair(
            Instant::now() + Duration::from_secs(5),
            chrono::Utc::now().timestamp_millis().saturating_sub(1),
        );
        let error = CaptureCatalogStore::new_until(conn.clone(), deadline)
            .apply(&request)
            .await
            .expect_err("wall-expired final authorization must reject the catalog write");
        assert_eq!(error, CaptureCatalogError::DeadlineExceeded);
        assert_eq!(
            agent_session_count(&conn).await,
            0,
            "a SQLite-authorized deadline rejection must roll back the pending session and receipt"
        );
    }

    #[tokio::test]
    async fn idless_session_end_redelivery_adopts_only_the_current_local_pending_receipt() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let first_action = ingress_action(0x510, None, LifecycleEventKind::SessionEnd);
        let first = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            first_action.clone(),
            terminal_mutation(None),
        )
        .expect("first id-less terminal request");
        assert!(matches!(
            store.apply(&first).await.expect("reserve first terminal"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));
        let pending_state = DurableCaptureState {
            phase: CapturePhase::Pending,
            stopped_at: None,
            sync_revision: 1,
        };

        // Ingress has no native ID, so this is a deliberately distinct fresh
        // UUID. The catalog must atomically recover the old local action,
        // without adding a second receipt or advancing its reserved revision.
        let redelivery_action = ingress_action(0x511, None, LifecycleEventKind::SessionEnd);
        let redelivery = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            redelivery_action,
            terminal_mutation(Some(pending_state)),
        )
        .expect("redelivered id-less terminal request");
        let result = store
            .apply(&redelivery)
            .await
            .expect("adopt pending terminal");
        match result {
            CaptureCatalogApplyResult::ResumePending {
                adopted_action: Some(adopted),
                checkpoint: CheckpointWrite::Committed,
                ..
            } => assert_eq!(adopted, first_action),
            other => panic!("id-less terminal must adopt the current pending receipt: {other:?}"),
        }
        assert_eq!(session_revision(&conn).await, 1);

        let metadata: String = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT metadata_json FROM agent_session WHERE provider_session_id = 'provider-a'",
                [],
            ))
            .await
            .expect("read receipt ledger")
            .expect("session row")
            .try_get_by("metadata_json")
            .expect("ledger metadata");
        let (_, ledger) = decode_receipt_metadata(&metadata).expect("valid receipt ledger");
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(
            ledger.entries[0].event_id,
            first_action.event_id().to_string()
        );
    }

    #[tokio::test]
    async fn fake_idless_session_end_redelivery_adopts_the_current_local_pending_receipt() {
        let store = FakeCaptureCatalogStore::default();
        let first_action = ingress_action(0x515, None, LifecycleEventKind::SessionEnd);
        let first = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            first_action.clone(),
            terminal_mutation(None),
        )
        .expect("first fake id-less terminal request");
        assert!(matches!(
            store.apply(&first).await.expect("reserve fake terminal"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));
        let redelivery = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            ingress_action(0x516, None, LifecycleEventKind::SessionEnd),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Pending,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("redelivered fake id-less terminal request");
        match store.apply(&redelivery).await.expect("adopt fake terminal") {
            CaptureCatalogApplyResult::ResumePending {
                adopted_action: Some(adopted),
                checkpoint: CheckpointWrite::Committed,
                ..
            } => assert_eq!(adopted, first_action),
            other => panic!("fake id-less terminal must adopt the current receipt: {other:?}"),
        }
    }

    #[tokio::test]
    async fn idless_session_end_after_a_live_revision_does_not_adopt_stale_pending_receipt() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let first = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            ingress_action(0x520, None, LifecycleEventKind::SessionEnd),
            terminal_mutation(None),
        )
        .expect("first terminal request");
        store.apply(&first).await.expect("reserve first terminal");

        // A live event advances the durable revision, fencing the old pending
        // finalizer. A later genuine ID-less SessionEnd must not adopt it.
        let live = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            ingress_action(0x521, Some(receipt('b')), LifecycleEventKind::SessionStart),
            mutation(
                Some(DurableCaptureState {
                    phase: CapturePhase::Pending,
                    stopped_at: None,
                    sync_revision: 1,
                }),
                CheckpointWrite::None,
            ),
        )
        .expect("live mutation after pending terminal");
        assert!(matches!(
            store.apply(&live).await.expect("apply live mutation"),
            CaptureCatalogApplyResult::Applied { .. }
        ));
        let later = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            ingress_action(0x522, None, LifecycleEventKind::SessionEnd),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 2,
            })),
        )
        .expect("later id-less terminal");
        assert!(matches!(
            store.apply(&later).await.expect("reserve new terminal"),
            CaptureCatalogApplyResult::Applied { .. }
        ));
        assert_eq!(session_revision(&conn).await, 3);
    }

    #[tokio::test]
    async fn foreign_owner_terminal_receipt_is_rejected_before_any_loser_mutation() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let winner_action = action(41, Some(receipt('e')));
        let winner = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            winner_action.clone(),
            terminal_mutation(None),
        )
        .expect("winner terminal request");
        assert!(matches!(
            store.apply(&winner).await.expect("reserve winner"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));

        let loser = CaptureCatalogApplyRequest::new(
            scope(),
            CaptureCatalogSession::new("session-b", "codex", "provider-a", "/repo")
                .expect("loser session"),
            action(42, Some(receipt('f'))),
            terminal_mutation(None),
        )
        .expect("loser terminal request");
        let expected_conflict = CaptureCatalogApplyResult::ConflictUnchanged {
            conflict: CaptureCatalogConflict::SessionIdentity,
        };
        assert_eq!(
            store.apply(&loser).await.expect("reject loser"),
            expected_conflict
        );
        assert_eq!(
            store.apply(&loser).await.expect("reject loser replay"),
            expected_conflict,
            "a rejected loser must not create a replayable pending receipt"
        );

        let row = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT session_id, metadata_json FROM agent_session \
                 WHERE provider_session_id = 'provider-a'",
                [],
            ))
            .await
            .expect("read owner row")
            .expect("exactly one owner row");
        let owner_session: String = row.try_get_by("session_id").expect("owner session");
        assert_eq!(owner_session, "session-a");
        let metadata_json: String = row.try_get_by("metadata_json").expect("owner metadata");
        let (_, ledger) = decode_receipt_metadata(&metadata_json).expect("owner receipt ledger");
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].action_key, winner_action.action_key());
        assert!(ledger.entries[0].finalizer.is_none());

        let count: i64 = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM agent_session WHERE provider_session_id = 'provider-a'",
                [],
            ))
            .await
            .expect("count owner rows")
            .expect("count row")
            .try_get_by("count")
            .expect("count value");
        assert_eq!(count, 1, "loser may not create an agent_session row");
    }

    #[tokio::test]
    async fn fake_catalog_rejects_foreign_owner_before_terminal_receipt_mutates() {
        let store = FakeCaptureCatalogStore::default();
        let winner = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(43, Some(receipt('a'))),
            terminal_mutation(None),
        )
        .expect("winner terminal request");
        store.apply(&winner).await.expect("reserve winner");
        let loser = CaptureCatalogApplyRequest::new(
            scope(),
            CaptureCatalogSession::new("session-b", "codex", "provider-a", "/repo")
                .expect("loser session"),
            action(44, Some(receipt('b'))),
            terminal_mutation(None),
        )
        .expect("loser terminal request");
        assert_eq!(
            store.apply(&loser).await.expect("reject loser"),
            CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            }
        );
        assert_eq!(
            store.apply(&loser).await.expect("reject loser replay"),
            CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            }
        );
    }

    #[tokio::test]
    async fn fake_terminal_source_conflict_matches_registered_marker_semantics() {
        let absent_store = FakeCaptureCatalogStore::default();
        let (absent_stop, _, absent_policy) =
            reserve_fake_terminal_attempt(&absent_store, 0x91).await;
        assert!(matches!(
            absent_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &absent_stop,
                        absent_policy.clone(),
                        "fake-marker-absent",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect fake source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        assert_eq!(
            absent_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &absent_stop,
                        absent_policy,
                        "fake-marker-later",
                        Some(TEST_SOURCE_DIGEST_Y),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("classify unregistered fake source"),
            CaptureCatalogTerminalAttempt::Quarantined {
                reason: FinalizeQuarantineReason::SourceDigestConflict,
            },
            "an absent elected marker cannot safely accept changed bytes"
        );

        let registered_store = FakeCaptureCatalogStore::default();
        let (registered_stop, registered_action, registered_policy) =
            reserve_fake_terminal_attempt(&registered_store, 0xa1).await;
        assert!(matches!(
            registered_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &registered_stop,
                        registered_policy.clone(),
                        "fake-marker-registered",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect registered fake source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        registered_store
            .mark_terminal_attempt_registered(
                "session-a",
                &registered_action,
                CheckpointWrite::Committed,
                "fake-marker-registered",
            )
            .expect("record fake marker registration");
        assert_eq!(
            registered_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &registered_stop,
                        registered_policy,
                        "fake-marker-later",
                        Some(TEST_SOURCE_DIGEST_Y),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("observe registered fake source"),
            CaptureCatalogTerminalAttempt::Adopted {
                marker_generation: "fake-marker-registered".to_string(),
                source_digest: Some(TEST_SOURCE_DIGEST_X.to_string()),
            },
            "a registered elected marker is observed, never quarantined by a changed source"
        );
    }

    #[tokio::test]
    async fn fake_terminal_marker_slot_replaces_stale_generation() {
        let store = FakeCaptureCatalogStore::default();
        let (stop, action, policy) = reserve_fake_terminal_attempt(&store, 0xa3).await;
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "fake-marker-elected",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect fake terminal source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        store
            .mark_terminal_attempt_registered(
                "session-a",
                &action,
                CheckpointWrite::Committed,
                "fake-marker-elected",
            )
            .expect("record elected fake marker");
        // MetadataKv has one marker value for this `(session, checkpoint)`
        // key. A later foreign value replaces the elected generation; a
        // stale same-source delivery must therefore be fenced rather than
        // observing the obsolete generation as still registered.
        store
            .mark_terminal_attempt_registered(
                "session-a",
                &action,
                CheckpointWrite::Committed,
                "fake-marker-foreign",
            )
            .expect("replace fake marker slot with foreign generation");

        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "fake-marker-stale-delivery",
                        Some(TEST_SOURCE_DIGEST_X),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("fence stale fake generation"),
            CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            },
            "a foreign value replacing the MetadataKv slot must be incompatible"
        );
    }

    #[tokio::test]
    async fn fake_same_source_replay_without_registered_marker_respects_finalizer_window() {
        let store = FakeCaptureCatalogStore::default();
        let (stop, _, policy) = reserve_fake_terminal_attempt(&store, 0xa5).await;
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "fake-marker-elected",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect fake source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "fake-marker-replay-before-expiry",
                        Some(TEST_SOURCE_DIGEST_X),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("same source may resume before the budget expires"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "fake-marker-replay-after-expiry",
                        Some(TEST_SOURCE_DIGEST_X),
                        MAX_FINALIZE_WINDOW_MILLIS + 2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("classify expired unregistered same-source replay"),
            CaptureCatalogTerminalAttempt::Quarantined {
                reason: FinalizeQuarantineReason::WindowLimit,
            }
        );
        let catalog = store.state.lock().expect("read fake durable repair state");
        assert_eq!(
            catalog.sessions[0].state.phase,
            CapturePhase::Quarantined,
            "an unregistered same-source replay cannot bypass the durable retry budget"
        );
    }

    #[tokio::test]
    async fn fake_same_source_replay_without_registered_marker_respects_attempt_limit() {
        let store = FakeCaptureCatalogStore::default();
        let (stop, _, policy) = reserve_fake_terminal_attempt(&store, 0xa8).await;
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "fake-marker-elected",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect fake source"),
            CaptureCatalogTerminalAttempt::Bound { attempts: 1, .. }
        ));
        for expected_attempts in 2..=MAX_FINALIZE_ATTEMPTS {
            let marker_generation = format!("fake-marker-retry-{expected_attempts}");
            assert!(matches!(
                store
                    .claim_terminal_attempt(
                        &finalize_request(
                            &stop,
                            policy.clone(),
                            &marker_generation,
                            Some(TEST_SOURCE_DIGEST_X),
                            i64::from(expected_attempts),
                            FinalizeCheckpointProgress::NotStarted,
                        ),
                        true,
                    )
                    .await
                    .expect("persist same-source fake retry"),
                CaptureCatalogTerminalAttempt::Bound { attempts, .. }
                    if attempts == expected_attempts
            ));
        }
        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "fake-marker-retry-limit",
                        Some(TEST_SOURCE_DIGEST_X),
                        i64::from(MAX_FINALIZE_ATTEMPTS) + 1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("quarantine exhausted fake retry budget"),
            CaptureCatalogTerminalAttempt::Quarantined {
                reason: FinalizeQuarantineReason::AttemptLimit,
            }
        );
        assert_eq!(
            store
                .state
                .lock()
                .expect("read fake attempt-limit state")
                .sessions[0]
                .state
                .phase,
            CapturePhase::Quarantined
        );
    }

    #[tokio::test]
    async fn fake_same_source_expired_replay_preserves_registered_and_durable_evidence() {
        let registered_store = FakeCaptureCatalogStore::default();
        let (registered_stop, registered_action, registered_policy) =
            reserve_fake_terminal_attempt(&registered_store, 0xa6).await;
        assert!(matches!(
            registered_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &registered_stop,
                        registered_policy.clone(),
                        "fake-marker-live",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect fake terminal source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        registered_store
            .mark_terminal_attempt_registered(
                "session-a",
                &registered_action,
                CheckpointWrite::Committed,
                "fake-marker-live",
            )
            .expect("record exact fake marker");
        assert!(matches!(
            registered_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &registered_stop,
                        registered_policy,
                        "fake-marker-late-duplicate",
                        Some(TEST_SOURCE_DIGEST_X),
                        MAX_FINALIZE_WINDOW_MILLIS + 2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("observe live fake writer after the budget window"),
            CaptureCatalogTerminalAttempt::Bound {
                marker_generation,
                ..
            } if marker_generation == "fake-marker-live"
        ));

        let durable_store = FakeCaptureCatalogStore::default();
        let (durable_stop, durable_action, durable_policy) =
            reserve_fake_terminal_attempt(&durable_store, 0xa7).await;
        assert!(matches!(
            durable_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &durable_stop,
                        durable_policy.clone(),
                        "fake-marker-durable",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect fake durable source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        durable_store
            .mark_terminal_checkpoint_durable(
                "session-a",
                &durable_action,
                CheckpointWrite::Committed,
            )
            .expect("record durable fake checkpoint");
        assert_eq!(
            durable_store
                .claim_terminal_attempt(
                    &finalize_request(
                        &durable_stop,
                        durable_policy,
                        "fake-marker-late-duplicate",
                        Some(TEST_SOURCE_DIGEST_X),
                        MAX_FINALIZE_WINDOW_MILLIS + 2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("classify durable fake replay"),
            CaptureCatalogTerminalAttempt::DurableReplay
        );
    }

    #[tokio::test]
    async fn fake_changed_source_rejects_different_registered_marker_generation() {
        let store = FakeCaptureCatalogStore::default();
        let (stop, action, policy) = reserve_fake_terminal_attempt(&store, 0xaa).await;
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "fake-marker-elected",
                        Some(TEST_SOURCE_DIGEST_X),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect fake source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        // A different marker generation occupies the same persistent marker
        // slot. Seed durable evidence as well to assert that fake ordering
        // matches SQLite: incompatible marker state takes precedence over a
        // replayable checkpoint row.
        store
            .mark_terminal_attempt_registered(
                "session-a",
                &action,
                CheckpointWrite::Committed,
                "fake-marker-foreign",
            )
            .expect("seed incompatible fake marker");
        store
            .mark_terminal_checkpoint_durable("session-a", &action, CheckpointWrite::Committed)
            .expect("seed durable fake checkpoint");

        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "fake-marker-changed-source",
                        Some(TEST_SOURCE_DIGEST_Y),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("reject incompatible fake marker"),
            CaptureCatalogTerminalAttempt::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            },
            "a different generation in the persistent marker slot must not be treated as absent"
        );
    }

    #[tokio::test]
    async fn fake_changed_source_durable_replay_completes_only_elected_receipt() {
        let store = FakeCaptureCatalogStore::default();
        let (stop, action, policy) = reserve_fake_terminal_attempt(&store, 0xb1).await;
        let elected = finalize_request(
            &stop,
            policy.clone(),
            "fake-marker-durable",
            Some(TEST_SOURCE_DIGEST_X),
            1,
            FinalizeCheckpointProgress::NotStarted,
        );
        assert!(matches!(
            store
                .claim_terminal_attempt(&elected, true)
                .await
                .expect("elect fake source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy.clone(),
                    "fake-marker-durable",
                    Some(TEST_SOURCE_DIGEST_X),
                    2,
                    FinalizeCheckpointProgress::Durable,
                ))
                .await
                .expect("record fake durable finalizer"),
            CaptureCatalogFinalizeResult::ReadyToComplete { .. }
        ));
        store
            .mark_terminal_checkpoint_durable("session-a", &action, CheckpointWrite::Committed)
            .expect("record retired fake checkpoint marker");

        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "fake-marker-newer-source",
                        Some(TEST_SOURCE_DIGEST_Y),
                        3,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("classify changed source after durable write"),
            CaptureCatalogTerminalAttempt::DurableReplay,
            "a changed source after marker retirement is replay-only"
        );
        assert_eq!(
            store
                .complete_durable_replay(&stop)
                .await
                .expect("complete original fake receipt"),
            CaptureCatalogCompleteResult::Completed
        );
        assert_eq!(
            store
                .claim_terminal_attempt(&elected, true)
                .await
                .expect("read completed receipt"),
            CaptureCatalogTerminalAttempt::AlreadyComplete,
            "the changed replay may complete only the elected terminal receipt"
        );
    }

    #[tokio::test]
    async fn state_only_second_provider_claim_does_not_block_the_rowid_first_checkpoint_owner() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let claude_session = session();
        let codex_session = CaptureCatalogSession::new("session-b", "codex", "provider-a", "/repo")
            .expect("codex session");
        let claude_start = CaptureCatalogApplyRequest::new(
            scope(),
            claude_session.clone(),
            ingress_action(45, Some(receipt('c')), LifecycleEventKind::SessionStart),
            mutation(None, CheckpointWrite::None),
        )
        .expect("claude state-only start");
        let codex_start = CaptureCatalogApplyRequest::new(
            scope(),
            codex_session.clone(),
            ingress_action(46, Some(receipt('d')), LifecycleEventKind::TurnStart),
            mutation(None, CheckpointWrite::None),
        )
        .expect("codex state-only start");
        assert!(matches!(
            store.apply(&claude_start).await.expect("claude start"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Complete,
                ..
            }
        ));
        assert!(matches!(
            store.apply(&codex_start).await.expect("codex exempt start"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Complete,
                ..
            }
        ));

        let active = DurableCaptureState {
            phase: CapturePhase::Active,
            stopped_at: None,
            sync_revision: 1,
        };
        let owner_terminal = CaptureCatalogApplyRequest::new(
            scope(),
            claude_session,
            action(47, Some(receipt('e'))),
            terminal_mutation(Some(active)),
        )
        .expect("owner terminal request");
        assert!(matches!(
            store
                .apply(&owner_terminal)
                .await
                .expect("rowid-first owner may checkpoint"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));

        let loser_terminal = CaptureCatalogApplyRequest::new(
            scope(),
            codex_session,
            action(48, Some(receipt('f'))),
            terminal_mutation(Some(active)),
        )
        .expect("non-owner terminal request");
        assert_eq!(
            store
                .apply(&loser_terminal)
                .await
                .expect("non-owner must not reserve a terminal receipt"),
            CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::SessionIdentity,
            }
        );

        let count: i64 = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM agent_session WHERE provider_session_id = 'provider-a'",
                [],
            ))
            .await
            .expect("count session claims")
            .expect("count row")
            .try_get_by("count")
            .expect("count value");
        assert_eq!(count, 2, "both state-only exempt claims remain valid");
    }

    #[tokio::test]
    async fn same_opaque_receipt_with_different_action_is_conflict_unchanged() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let receipt_key = receipt('b');
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(2, Some(receipt_key.clone())),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("request");
        store.apply(&request).await.expect("first apply");

        let mismatched = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(3, Some(receipt_key)),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("mismatched request");
        assert_eq!(
            store.apply(&mismatched).await.expect("conflict result"),
            CaptureCatalogApplyResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ActionMismatch,
            }
        );
        assert_eq!(session_revision(&conn).await, 1);
    }

    #[tokio::test]
    async fn terminal_state_is_deferred_until_checkpoint_receipt_completes_and_stale_complete_cannot_stop_newer_activity()
     {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(20, Some(receipt('8'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start request");
        store.apply(&start).await.expect("create active session");
        let active = DurableCaptureState {
            phase: CapturePhase::Active,
            stopped_at: None,
            sync_revision: 1,
        };
        let terminal_reserved = DurableCaptureState {
            phase: CapturePhase::Active,
            stopped_at: None,
            sync_revision: 2,
        };
        let stop_action = action(21, Some(receipt('9')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(active)),
        )
        .expect("stop request");
        assert!(matches!(
            store.apply(&stop).await.expect("reserve terminal receipt"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                state: DurableCaptureState {
                    phase: CapturePhase::Active,
                    stopped_at: None,
                    sync_revision: 2,
                },
                ..
            }
        ));
        assert_eq!(session_state(&conn).await, ("active".to_string(), None, 2));
        assert!(matches!(
            store.apply(&stop).await.expect("pending replay"),
            CaptureCatalogApplyResult::ResumePending { .. }
        ));
        let policy = finalizer_policy(&stop_action, None);
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy.clone(),
                    "marker-stale-fence",
                    None,
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("persist terminal fence"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let proof = match store
            .finalize(&finalize_request(
                &stop,
                policy,
                "marker-stale-fence",
                None,
                2,
                FinalizeCheckpointProgress::Durable,
            ))
            .await
            .expect("derive strict terminal proof")
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            other => panic!("expected strict proof, got {other:?}"),
        };

        // A later live event advances the state fence. The old terminal
        // checkpoint may finish physically, but it must not overwrite newer
        // activity when its stale receipt attempts to publish `stopped`.
        let later = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(22, Some(receipt('a'))),
            mutation(Some(terminal_reserved), CheckpointWrite::None),
        )
        .expect("later event");
        store.apply(&later).await.expect("later state mutation");
        let complete = CaptureCatalogCompleteRequest::from_finalizer(&stop, proof)
            .expect("strict completion request");
        assert_eq!(
            store
                .complete(&complete)
                .await
                .expect("stale completion result"),
            CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::ConditionalWrite,
            }
        );
        assert_eq!(session_state(&conn).await, ("active".to_string(), None, 3));
    }

    #[tokio::test]
    async fn first_seen_terminal_uses_local_opaque_receipt_and_publishes_only_on_complete() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        // No provider-native receipt is available. The catalog uses the
        // action UUID only for this terminal pending record; it never derives
        // one from untrusted payload text.
        let stop_action = action(23, None);
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(None),
        )
        .expect("terminal request");
        assert!(matches!(
            store.apply(&stop).await.expect("reserve terminal receipt"),
            CaptureCatalogApplyResult::Applied {
                state: DurableCaptureState {
                    phase: CapturePhase::Pending,
                    stopped_at: None,
                    sync_revision: 1,
                },
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));
        assert_eq!(session_state(&conn).await, ("pending".to_string(), None, 1));

        let legacy_complete = CaptureCatalogCompleteRequest::from_apply(&stop)
            .expect("legacy completion from apply request");
        assert_eq!(
            store
                .complete(&legacy_complete)
                .await
                .expect("reject proofless terminal completion"),
            CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            }
        );
        let policy = finalizer_policy(&stop_action, None);
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy.clone(),
                    "marker-local-receipt",
                    None,
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("persist terminal fence"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let proof = match store
            .finalize(&finalize_request(
                &stop,
                policy,
                "marker-local-receipt",
                None,
                2,
                FinalizeCheckpointProgress::Durable,
            ))
            .await
            .expect("derive strict terminal proof")
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            other => panic!("expected strict proof, got {other:?}"),
        };
        let complete = CaptureCatalogCompleteRequest::from_finalizer(&stop, proof)
            .expect("strict completion from finalizer proof");
        assert_eq!(
            store.complete(&complete).await.expect("complete terminal"),
            CaptureCatalogCompleteResult::Completed
        );
        assert_eq!(
            session_state(&conn).await,
            ("stopped".to_string(), Some(1_700_000_010), 2)
        );
        assert!(matches!(
            store.apply(&stop).await.expect("completed terminal replay"),
            CaptureCatalogApplyResult::AlreadyApplied
        ));
    }

    #[tokio::test]
    async fn completed_terminal_receipt_acknowledges_stale_marker_without_checkpoint_row() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let stop_action = action(0x7a0, Some(receipt('f')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(None),
        )
        .expect("terminal request");
        assert!(matches!(
            store.apply(&stop).await.expect("reserve terminal receipt"),
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
        ));

        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                stop_action.event_id,
                CheckpointWrite::Committed,
            );
        let marker = crate::internal::ai::traces::TracesInflightMarker::new(
            "session-a",
            &checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        let marker_generation = marker
            .generation
            .clone()
            .expect("new traces marker has a generation");
        let policy = finalizer_policy(&stop_action, None);
        let fence = match store
            .claim_terminal_attempt(
                &finalize_request(
                    &stop,
                    policy.clone(),
                    &marker_generation,
                    Some(TEST_SOURCE_DIGEST_A),
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ),
                true,
            )
            .await
            .expect("bind terminal writer")
        {
            CaptureCatalogTerminalAttempt::Bound {
                registration_fence, ..
            } => *registration_fence,
            other => panic!("expected bound terminal writer, got {other:?}"),
        };
        let proof = match store
            .finalize(&finalize_request(
                &stop,
                policy,
                &marker_generation,
                Some(TEST_SOURCE_DIGEST_A),
                2,
                FinalizeCheckpointProgress::Durable,
            ))
            .await
            .expect("derive strict terminal proof")
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            other => panic!("expected strict proof, got {other:?}"),
        };
        let completion = CaptureCatalogCompleteRequest::from_finalizer(&stop, proof)
            .expect("strict completion request");
        assert_eq!(
            store
                .complete(&completion)
                .await
                .expect("complete terminal"),
            CaptureCatalogCompleteResult::Completed
        );

        let checkpoint_count: i64 = conn
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM agent_checkpoint".to_string(),
            ))
            .await
            .expect("count action checkpoints")
            .expect("checkpoint count row")
            .try_get_by("count")
            .expect("decode checkpoint count");
        assert_eq!(
            checkpoint_count, 0,
            "the acknowledgement must not pretend that this action has a checkpoint row"
        );

        let registration =
            crate::internal::ai::traces::register_traces_write_attempt_with_capture_scope(
                &conn,
                None,
                &marker,
                &[],
                Some(crate::internal::ai::traces::CheckpointScope::Committed),
                Some(&fence),
            )
            .await
            .expect("completed terminal receipt is a typed marker acknowledgement");
        assert_eq!(
            registration,
            crate::internal::ai::traces::TracesWriteAttemptRegistration::TerminalReceiptAlreadyComplete
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                "session-a",
                &checkpoint_id,
            )
            .await
            .expect("read stale marker slot")
            .is_none(),
            "a completed receipt must not recreate its terminal writer marker"
        );
    }

    fn finalizer_policy(
        action: &CaptureCatalogAction,
        deadline: Option<i64>,
    ) -> CaptureFinalizePolicy {
        CaptureFinalizePolicy::new(
            deadline,
            CaptureFinalizeMode::Deferrable,
            action.action_key(),
        )
        .expect("finalizer policy")
    }

    #[tokio::test]
    async fn covered_terminal_replay_requires_a_prior_checkpoint_and_unbound_finalizer() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());

        // Establish the prior, strictly completed terminal state that a
        // cross-channel coverage replay is allowed to acknowledge.
        let prior_action = action(0x7a1, Some(receipt('a')));
        let prior = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            prior_action.clone(),
            terminal_mutation(None),
        )
        .expect("prior terminal request");
        store.apply(&prior).await.expect("reserve prior terminal");
        let prior_policy = finalizer_policy(&prior_action, None);
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &prior,
                    prior_policy.clone(),
                    "prior-terminal-marker",
                    None,
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("bind prior finalizer"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let prior_proof = match store
            .finalize(&finalize_request(
                &prior,
                prior_policy,
                "prior-terminal-marker",
                None,
                2,
                FinalizeCheckpointProgress::Durable,
            ))
            .await
            .expect("make prior terminal durable")
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            other => panic!("expected prior terminal proof, got {other:?}"),
        };
        let prior_complete = CaptureCatalogCompleteRequest::from_finalizer(&prior, prior_proof)
            .expect("prior completion request");
        assert_eq!(
            store
                .complete(&prior_complete)
                .await
                .expect("complete prior terminal"),
            CaptureCatalogCompleteResult::Completed
        );
        let durable_stopped = DurableCaptureState {
            phase: CapturePhase::Stopped,
            stopped_at: Some(1_700_000_010),
            sync_revision: 2,
        };
        let replay_action = action(0x7a2, Some(receipt('b')));
        let replay = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            replay_action.clone(),
            terminal_mutation(Some(durable_stopped)),
        )
        .expect("covered replay request");
        store.apply(&replay).await.expect("reserve covered replay");
        let unbound_marker = format!(
            "{UNBOUND_FINALIZER_MARKER_PREFIX}{}",
            replay_action.action_key()
        );
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &replay,
                    finalizer_policy(&replay_action, None),
                    &unbound_marker,
                    None,
                    3,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("prepare covered replay"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let covered_completion =
            CaptureCatalogCompleteRequest::from_covered_terminal_replay(&replay)
                .expect("covered replay completion request");
        assert_eq!(
            store
                .complete(&covered_completion)
                .await
                .expect("reject replay without prior checkpoint"),
            CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            },
            "a stopped row alone is not sufficient proof for coverage replay"
        );
        conn.execute_unprepared(
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope) \
             VALUES ('prior-terminal-checkpoint', 'session-a', 'committed')",
        )
        .await
        .expect("seed prior committed checkpoint");
        assert_eq!(
            store
                .complete(&covered_completion)
                .await
                .expect("complete covered replay"),
            CaptureCatalogCompleteResult::Completed
        );
        assert_eq!(
            session_state(&conn).await,
            ("stopped".to_string(), Some(1_700_000_010), 3),
            "the replay receipt completes without re-publishing terminal state"
        );

        // A concrete marker belongs to a new writer attempt; it cannot use
        // the proofless coverage acknowledgement even over the same stopped
        // state and prior checkpoint.
        let bound_action = action(0x7a3, Some(receipt('c')));
        let bound = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            bound_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                sync_revision: 3,
                ..durable_stopped
            })),
        )
        .expect("bound replay request");
        store.apply(&bound).await.expect("reserve bound replay");
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &bound,
                    finalizer_policy(&bound_action, None),
                    "bound-terminal-marker",
                    None,
                    4,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("bind concrete replay marker"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let bound_completion = CaptureCatalogCompleteRequest::from_covered_terminal_replay(&bound)
            .expect("bound replay completion request");
        assert_eq!(
            store
                .complete(&bound_completion)
                .await
                .expect("reject concrete-marker coverage completion"),
            CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            }
        );

        let active_terminal = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(0x7a4, Some(receipt('d'))),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 4,
            })),
        )
        .expect("first terminal request");
        assert_eq!(
            CaptureCatalogCompleteRequest::from_covered_terminal_replay(&active_terminal),
            Err(CaptureCatalogError::InvalidRequest),
            "a first terminal transition must retain the strict finalizer path"
        );
    }

    fn finalize_request(
        stop: &CaptureCatalogApplyRequest,
        policy: CaptureFinalizePolicy,
        marker_generation: &str,
        source_digest: Option<&str>,
        now_millis: i64,
        checkpoint: FinalizeCheckpointProgress,
    ) -> CaptureCatalogFinalizeRequest {
        CaptureCatalogFinalizeRequest::from_apply(
            stop,
            policy,
            marker_generation,
            source_digest.map(str::to_string),
            now_millis,
            checkpoint,
        )
        .expect("finalizer request")
    }

    #[test]
    fn finalizer_request_rejects_nonopaque_source_digest_before_catalog_persistence() {
        for (seed, source_digest) in [
            (0x701, "raw transcript content"),
            (
                0x702,
                "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
            ),
        ] {
            let catalog_action = action(seed, None);
            assert!(matches!(
                CaptureCatalogFinalizeRequest::new(
                    scope(),
                    session(),
                    catalog_action.clone(),
                    CheckpointWrite::Committed,
                    finalizer_policy(&catalog_action, None),
                    "marker-generation",
                    Some(source_digest.to_string()),
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ),
                Err(CaptureCatalogError::InvalidRequest)
            ));
        }

        let catalog_action = action(0x703, None);
        assert!(
            CaptureCatalogFinalizeRequest::new(
                scope(),
                session(),
                catalog_action.clone(),
                CheckpointWrite::Committed,
                finalizer_policy(&catalog_action, None),
                "marker-generation",
                Some(TEST_SOURCE_DIGEST_A.to_string()),
                1,
                FinalizeCheckpointProgress::NotStarted,
            )
            .is_ok()
        );
    }

    async fn reserve_fake_terminal_attempt(
        store: &FakeCaptureCatalogStore,
        seed: u128,
    ) -> (
        CaptureCatalogApplyRequest,
        CaptureCatalogAction,
        CaptureFinalizePolicy,
    ) {
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(seed, Some(receipt('a'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("fake start request");
        store.apply(&start).await.expect("fake start applied");
        let stop_action = action(seed + 1, Some(receipt('b')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("fake terminal request");
        store.apply(&stop).await.expect("fake terminal reserved");
        let policy = finalizer_policy(&stop_action, None);
        (stop, stop_action, policy)
    }

    async fn reserve_sqlite_terminal_attempt(
        store: &CaptureCatalogStore,
        seed: u128,
    ) -> (
        CaptureCatalogApplyRequest,
        CaptureCatalogAction,
        CaptureFinalizePolicy,
    ) {
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(seed, Some(receipt('a'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start request");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(seed + 1, Some(receipt('b')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);
        (stop, stop_action, policy)
    }

    #[tokio::test]
    async fn pending_finalizer_persists_exact_fences_and_strict_proof_is_required_for_safe_completion()
     {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(30, Some(receipt('b'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start");
        store.apply(&start).await.expect("start applied");
        let active = DurableCaptureState {
            phase: CapturePhase::Active,
            stopped_at: None,
            sync_revision: 1,
        };
        let stop_action = action(31, Some(receipt('c')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(active)),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, Some(1_000));

        assert_eq!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy.clone(),
                    "marker-generation-a",
                    Some(TEST_SOURCE_DIGEST_A),
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("persist first pending attempt"),
            CaptureCatalogFinalizeResult::Pending {
                attempts: 1,
                stage: FinalizePendingStage::Snapshot,
            }
        );

        let proof = match store
            .finalize(&finalize_request(
                &stop,
                policy,
                "marker-generation-a",
                Some(TEST_SOURCE_DIGEST_A),
                2,
                FinalizeCheckpointProgress::Durable,
            ))
            .await
            .expect("durable finalizer check")
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            other => panic!("expected strict proof, got {other:?}"),
        };

        let mut stale_proof = proof.clone();
        stale_proof.marker_generation = "different-generation".to_string();
        let stale_completion = CaptureCatalogCompleteRequest::from_finalizer(&stop, stale_proof)
            .expect("strict stale completion request");
        assert_eq!(
            store
                .complete(&stale_completion)
                .await
                .expect("stale completion result"),
            CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            }
        );
        assert_eq!(session_state(&conn).await, ("active".to_string(), None, 2));

        let completion = CaptureCatalogCompleteRequest::from_finalizer(&stop, proof)
            .expect("strict completion request");
        assert_eq!(
            store
                .complete(&completion)
                .await
                .expect("strict completion"),
            CaptureCatalogCompleteResult::Completed
        );
        assert_eq!(
            session_state(&conn).await,
            ("stopped".to_string(), Some(1_700_000_010), 3)
        );
    }

    #[tokio::test]
    async fn finalizer_replay_is_bounded_and_stale_marker_takeover_quarantines_without_stopping() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(32, Some(receipt('d'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(33, Some(receipt('e')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);

        store
            .finalize(&finalize_request(
                &stop,
                policy.clone(),
                "marker-generation-a",
                Some(TEST_SOURCE_DIGEST_A),
                1,
                FinalizeCheckpointProgress::NotStarted,
            ))
            .await
            .expect("first pending");
        let takeover = store
            .finalize(&finalize_request(
                &stop,
                policy,
                "marker-generation-b",
                Some(TEST_SOURCE_DIGEST_A),
                2,
                FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Marker),
            ))
            .await
            .expect("stale marker is classified");
        assert_eq!(
            takeover,
            CaptureCatalogFinalizeResult::Quarantined {
                reason: FinalizeQuarantineReason::MarkerGenerationConflict,
            }
        );
        assert_eq!(
            session_state(&conn).await,
            ("quarantined".to_string(), None, 3)
        );
    }

    fn synchronous_finalizer_policy(
        action: &CaptureCatalogAction,
        deadline: i64,
    ) -> CaptureFinalizePolicy {
        CaptureFinalizePolicy::new(
            Some(deadline),
            CaptureFinalizeMode::Synchronous,
            action.action_key(),
        )
        .expect("synchronous finalizer policy")
    }

    /// Decode the persisted finalizer of `action`'s terminal receipt from
    /// the SQLite ledger exactly as the catalog will restore it.
    async fn stored_terminal_finalizer(
        conn: &DatabaseConnection,
        action: &CaptureCatalogAction,
    ) -> StoredFinalizeReceipt {
        let metadata: String = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT metadata_json FROM agent_session WHERE provider_session_id = 'provider-a'",
                [],
            ))
            .await
            .expect("read receipt ledger")
            .expect("session row")
            .try_get_by("metadata_json")
            .expect("ledger metadata");
        let (_, ledger) = decode_receipt_metadata(&metadata).expect("valid receipt ledger");
        ledger
            .find(&action.completion_receipt_storage_key())
            .expect("terminal receipt")
            .finalizer
            .clone()
            .expect("persisted finalizer")
    }

    /// An immediately expired synchronous host budget only reports
    /// repair-required: the catalog keeps a durable, content-free receipt,
    /// quarantines (never stops) the session, and later retries can neither
    /// reopen it nor consume another attempt.
    #[tokio::test]
    async fn expired_synchronous_finalizer_quarantines_without_stopping() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let (stop, stop_action, _) = reserve_sqlite_terminal_attempt(&store, 0x3c0).await;
        let synchronous = synchronous_finalizer_policy(&stop_action, 1_000);
        assert_eq!(session_state(&conn).await, ("active".to_string(), None, 2));

        assert_eq!(
            store
                .finalize(&finalize_request(
                    &stop,
                    synchronous.clone(),
                    "marker-generation-synchronous",
                    Some(TEST_SOURCE_DIGEST_A),
                    1_000,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("classify expired synchronous attempt"),
            CaptureCatalogFinalizeResult::Quarantined {
                reason: FinalizeQuarantineReason::SynchronousDeadline,
            }
        );
        assert_eq!(
            session_state(&conn).await,
            ("quarantined".to_string(), None, 3),
            "an expired synchronous terminal is repair-required, never stopped"
        );
        let finalizer = stored_terminal_finalizer(&conn, &stop_action).await;
        assert_eq!(finalizer.status, StoredFinalizeStatus::Quarantined);
        assert_eq!(
            finalizer.quarantine_reason,
            Some(StoredFinalizeQuarantineReason::SynchronousDeadline)
        );
        assert_eq!(finalizer.mode, StoredFinalizeMode::Synchronous);
        assert_eq!(finalizer.deadline_millis, Some(1_000));
        assert_eq!(finalizer.first_attempt_millis, 1_000);
        assert_eq!(finalizer.attempts, 1);
        assert_eq!(finalizer.marker_generation, "marker-generation-synchronous");
        assert_eq!(
            finalizer.source_digest.as_deref(),
            Some(TEST_SOURCE_DIGEST_A)
        );
        // The receipt is a closed record of identities, an opaque digest,
        // the stage, and bounded counts; nothing else may be persisted.
        let stored = serde_json::to_value(&finalizer).expect("serialize stored finalizer");
        let mut fields = stored
            .as_object()
            .expect("stored finalizer object")
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "attempts",
                "deadline_millis",
                "first_attempt_millis",
                "marker_generation",
                "mode",
                "quarantine_reason",
                "replay_key",
                "source_digest",
                "stage",
                "status",
                "version",
            ]
        );
        assert_eq!(stored["replay_key"], stop_action.action_key());

        for (now_millis, checkpoint) in [
            (
                1_001,
                FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
            ),
            (1_002, FinalizeCheckpointProgress::Durable),
        ] {
            assert_eq!(
                store
                    .finalize(&finalize_request(
                        &stop,
                        synchronous.clone(),
                        "marker-generation-synchronous",
                        Some(TEST_SOURCE_DIGEST_A),
                        now_millis,
                        checkpoint,
                    ))
                    .await
                    .expect("retry a quarantined synchronous receipt"),
                CaptureCatalogFinalizeResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                },
                "quarantine advanced the receipt fence, so no retry may issue a completion proof"
            );
            assert_eq!(
                serde_json::to_value(stored_terminal_finalizer(&conn, &stop_action).await)
                    .expect("serialize retried finalizer"),
                stored,
                "a retry must not reopen the receipt or consume another attempt"
            );
            assert_eq!(
                session_state(&conn).await,
                ("quarantined".to_string(), None, 3)
            );
        }
    }

    /// An existing synchronous receipt whose host deadline elapses is
    /// classified before any retry accounting: its attempt count, stage and
    /// first-attempt time stay exactly as the last in-budget attempt left
    /// them. Doctor mirrors the same deadline as an exhausted budget.
    #[tokio::test]
    async fn expired_synchronous_retry_quarantines_before_attempt_accounting() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let (stop, stop_action, _) = reserve_sqlite_terminal_attempt(&store, 0x3c2).await;
        let synchronous = synchronous_finalizer_policy(&stop_action, 1_000);
        assert_eq!(
            store
                .finalize(&finalize_request(
                    &stop,
                    synchronous.clone(),
                    "marker-generation-a",
                    Some(TEST_SOURCE_DIGEST_A),
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("persist in-budget synchronous attempt"),
            CaptureCatalogFinalizeResult::Pending {
                attempts: 1,
                stage: FinalizePendingStage::Snapshot,
            }
        );
        let inside = store
            .pending_finalizer_recoveries_for_doctor(999)
            .await
            .expect("doctor scan inside the synchronous budget");
        assert_eq!(inside.recoveries.len(), 1);
        assert!(!inside.recoveries[0].budget_exhausted());
        let expired = store
            .pending_finalizer_recoveries_for_doctor(1_000)
            .await
            .expect("doctor scan after the synchronous deadline");
        assert_eq!(expired.recoveries.len(), 1);
        assert!(
            expired.recoveries[0].budget_exhausted(),
            "doctor must treat an elapsed synchronous deadline as an exhausted budget"
        );
        assert!(!expired.recoveries[0].quarantined());
        assert_eq!(session_state(&conn).await, ("active".to_string(), None, 2));

        assert_eq!(
            store
                .finalize(&finalize_request(
                    &stop,
                    synchronous,
                    "marker-generation-a",
                    Some(TEST_SOURCE_DIGEST_A),
                    1_000,
                    FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
                ))
                .await
                .expect("classify expired synchronous retry"),
            CaptureCatalogFinalizeResult::Quarantined {
                reason: FinalizeQuarantineReason::SynchronousDeadline,
            }
        );
        assert_eq!(
            session_state(&conn).await,
            ("quarantined".to_string(), None, 3),
            "an expired synchronous retry is repair-required, never stopped"
        );
        let finalizer = stored_terminal_finalizer(&conn, &stop_action).await;
        assert_eq!(finalizer.status, StoredFinalizeStatus::Quarantined);
        assert_eq!(
            finalizer.quarantine_reason,
            Some(StoredFinalizeQuarantineReason::SynchronousDeadline)
        );
        assert_eq!(
            finalizer.attempts, 1,
            "the deadline is checked before attempt accounting"
        );
        assert_eq!(finalizer.stage, StoredFinalizeStage::Snapshot);
        assert_eq!(finalizer.first_attempt_millis, 1);
        assert_eq!(finalizer.deadline_millis, Some(1_000));
        assert_eq!(finalizer.mode, StoredFinalizeMode::Synchronous);
        let quarantined = store
            .pending_finalizer_recoveries_for_doctor(1_000)
            .await
            .expect("doctor scan of the quarantined receipt");
        assert_eq!(quarantined.recoveries.len(), 1);
        assert!(quarantined.recoveries[0].quarantined());
        assert!(quarantined.recoveries[0].manual_only());
    }

    /// A retry may arrive with a fresh process-local policy, but the first
    /// persisted policy stays authoritative in both stores: a different mode
    /// or a later deadline can neither re-anchor the stored budget nor reset
    /// its first-attempt time.
    #[tokio::test]
    async fn finalizer_retry_cannot_reanchor_the_persisted_policy() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let fake = FakeCaptureCatalogStore::default();
        let (stop, stop_action, _) = reserve_sqlite_terminal_attempt(&store, 0x3c4).await;
        let (fake_stop, fake_action, _) = reserve_fake_terminal_attempt(&fake, 0x3c4).await;
        assert_eq!(stop_action, fake_action);
        let original = finalizer_policy(&stop_action, Some(1_000));
        // If honored, this re-anchored synchronous policy would quarantine
        // immediately because its own deadline has already elapsed.
        let reanchored_synchronous = synchronous_finalizer_policy(&stop_action, 1);
        // If honored, this later deadline would extend the host's budget.
        let later = finalizer_policy(&stop_action, Some(i64::MAX));
        let attempts = [
            (original, 1, FinalizeCheckpointProgress::NotStarted, 1),
            (
                reanchored_synchronous,
                2,
                FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Marker),
                2,
            ),
            (
                later,
                3,
                FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
                3,
            ),
        ];
        for (policy, now_millis, checkpoint, expected_attempts) in attempts {
            let expected = CaptureCatalogFinalizeResult::Pending {
                attempts: expected_attempts,
                stage: stage_for_progress(checkpoint),
            };
            assert_eq!(
                store
                    .finalize(&finalize_request(
                        &stop,
                        policy.clone(),
                        "marker-generation-a",
                        Some(TEST_SOURCE_DIGEST_A),
                        now_millis,
                        checkpoint,
                    ))
                    .await
                    .expect("sqlite finalizer retry"),
                expected
            );
            assert_eq!(
                fake.finalize(&finalize_request(
                    &fake_stop,
                    policy,
                    "marker-generation-a",
                    Some(TEST_SOURCE_DIGEST_A),
                    now_millis,
                    checkpoint,
                ))
                .await
                .expect("fake finalizer retry"),
                expected
            );
        }

        let key = stop_action.completion_receipt_storage_key();
        let fake_finalizer = fake.state.lock().expect("read fake finalizer").sessions[0]
            .ledger
            .find(&key)
            .expect("fake terminal receipt")
            .finalizer
            .clone()
            .expect("fake persisted finalizer");
        for finalizer in [
            stored_terminal_finalizer(&conn, &stop_action).await,
            fake_finalizer,
        ] {
            assert_eq!(finalizer.status, StoredFinalizeStatus::Pending);
            assert_eq!(finalizer.deadline_millis, Some(1_000));
            assert_eq!(finalizer.mode, StoredFinalizeMode::Deferrable);
            assert_eq!(finalizer.first_attempt_millis, 1);
            assert_eq!(finalizer.attempts, 3);
            assert_eq!(finalizer.stage, StoredFinalizeStage::Checkpoint);
        }
        assert_eq!(session_state(&conn).await, ("active".to_string(), None, 2));
    }

    /// A changed-source replay whose elected writer never registered its
    /// marker cannot safely reconstruct the original bytes. The exact
    /// pending receipt must become actionable repair evidence rather than
    /// silently retrying forever.
    #[tokio::test]
    async fn changed_source_duplicate_without_registered_marker_quarantines_for_repair() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(38, Some(receipt('3'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start request");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(39, Some(receipt('4')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);

        let elected = store
            .claim_terminal_attempt(
                &finalize_request(
                    &stop,
                    policy.clone(),
                    "marker-generation-elected",
                    Some(TEST_SOURCE_DIGEST_ELECTED),
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ),
                true,
            )
            .await
            .expect("elect first terminal source");
        assert!(matches!(
            elected,
            CaptureCatalogTerminalAttempt::Bound {
                marker_generation,
                source_digest,
                ..
            } if marker_generation == "marker-generation-elected"
                && source_digest.as_deref() == Some(TEST_SOURCE_DIGEST_ELECTED)
        ));
        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "marker-generation-later",
                        Some(TEST_SOURCE_DIGEST_LATER),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("later source is classified for repair"),
            CaptureCatalogTerminalAttempt::Quarantined {
                reason: FinalizeQuarantineReason::SourceDigestConflict,
            }
        );
        assert_eq!(
            session_state(&conn).await,
            ("quarantined".to_string(), None, 3),
            "an unregistered elected source must not leave changed-source replays pending forever"
        );
    }

    #[tokio::test]
    async fn same_source_replay_without_registered_marker_respects_finalizer_window() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(0x3a0, Some(receipt('a'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start request");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(0x3a1, Some(receipt('b')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);

        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "marker-generation-elected",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect terminal source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "marker-generation-replay-before-expiry",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("same source may resume before the budget expires"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "marker-generation-replay-after-expiry",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        MAX_FINALIZE_WINDOW_MILLIS + 2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("classify expired unregistered same-source replay"),
            CaptureCatalogTerminalAttempt::Quarantined {
                reason: FinalizeQuarantineReason::WindowLimit,
            }
        );
        assert_eq!(
            session_state(&conn).await,
            ("quarantined".to_string(), None, 3),
            "an unregistered same-source replay cannot borrow the elected fence past its retry window"
        );
    }

    #[tokio::test]
    async fn same_source_replay_without_registered_marker_respects_attempt_limit() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let (stop, _, policy) = reserve_sqlite_terminal_attempt(&store, 0x3a2).await;
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "marker-generation-elected",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect terminal source"),
            CaptureCatalogTerminalAttempt::Bound { attempts: 1, .. }
        ));
        for expected_attempts in 2..=MAX_FINALIZE_ATTEMPTS {
            let marker_generation = format!("marker-generation-retry-{expected_attempts}");
            assert!(matches!(
                store
                    .claim_terminal_attempt(
                        &finalize_request(
                            &stop,
                            policy.clone(),
                            &marker_generation,
                            Some(TEST_SOURCE_DIGEST_ELECTED),
                            i64::from(expected_attempts),
                            FinalizeCheckpointProgress::NotStarted,
                        ),
                        true,
                    )
                    .await
                    .expect("persist same-source retry"),
                CaptureCatalogTerminalAttempt::Bound { attempts, .. }
                    if attempts == expected_attempts
            ));
        }
        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "marker-generation-retry-limit",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        i64::from(MAX_FINALIZE_ATTEMPTS) + 1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("quarantine exhausted retry budget"),
            CaptureCatalogTerminalAttempt::Quarantined {
                reason: FinalizeQuarantineReason::AttemptLimit,
            }
        );
        assert_eq!(
            session_state(&conn).await,
            ("quarantined".to_string(), None, 3),
            "rapid unregistered same-source replays cannot bypass the durable attempt budget"
        );
    }

    #[tokio::test]
    async fn same_source_expired_replay_with_registered_marker_remains_bound() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let (stop, stop_action, policy) = reserve_sqlite_terminal_attempt(&store, 0x3b0).await;
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                stop_action.event_id,
                CheckpointWrite::Committed,
            );
        let marker =
            crate::internal::ai::traces::TracesInflightMarker::new("session-a", &checkpoint_id, 1);
        let marker_generation = marker
            .generation
            .clone()
            .expect("new traces marker has a generation");
        let fence = match store
            .claim_terminal_attempt(
                &finalize_request(
                    &stop,
                    policy.clone(),
                    &marker_generation,
                    Some(TEST_SOURCE_DIGEST_ELECTED),
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ),
                true,
            )
            .await
            .expect("elect terminal source")
        {
            CaptureCatalogTerminalAttempt::Bound {
                registration_fence, ..
            } => *registration_fence,
            other => panic!("expected bound terminal writer, got {other:?}"),
        };
        assert_eq!(
            crate::internal::ai::traces::register_traces_write_attempt_with_capture_scope(
                &conn,
                None,
                &marker,
                &[],
                Some(crate::internal::ai::traces::CheckpointScope::Committed),
                Some(&fence),
            )
            .await
            .expect("register elected terminal marker"),
            crate::internal::ai::traces::TracesWriteAttemptRegistration::Registered
        );
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "marker-generation-late-duplicate",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        MAX_FINALIZE_WINDOW_MILLIS + 2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("observe live same-source writer after the budget window"),
            CaptureCatalogTerminalAttempt::Bound {
                marker_generation: observed,
                ..
            } if observed == marker_generation
        ));
        assert_eq!(session_state(&conn).await.0, "active");
    }

    #[tokio::test]
    async fn same_source_expired_replay_with_durable_checkpoint_is_replay_only() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let (stop, stop_action, policy) = reserve_sqlite_terminal_attempt(&store, 0x3c0).await;
        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        "marker-generation-durable",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect terminal source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                stop_action.event_id,
                CheckpointWrite::Committed,
            );
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope) \
             VALUES (?, 'session-a', 'committed')",
            [checkpoint_id.into()],
        ))
        .await
        .expect("seed durable terminal checkpoint after marker retirement");
        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "marker-generation-late-duplicate",
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        MAX_FINALIZE_WINDOW_MILLIS + 2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("classify durable same-source replay"),
            CaptureCatalogTerminalAttempt::DurableReplay
        );
        assert_eq!(session_state(&conn).await.0, "active");
    }

    /// The changed-source durable-replay classification is only a hint. The
    /// completion operation must re-read the checkpoint row in its own write
    /// transaction, because repair can remove the row after claim but before
    /// the observer attempts receipt completion.
    #[tokio::test]
    async fn durable_replay_rechecks_checkpoint_before_completing_receipt() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(48, Some(receipt('7'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start request");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(49, Some(receipt('8')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);
        let elected = finalize_request(
            &stop,
            policy.clone(),
            "marker-generation-durable-recheck",
            Some(TEST_SOURCE_DIGEST_ELECTED),
            1,
            FinalizeCheckpointProgress::NotStarted,
        );
        assert!(matches!(
            store
                .claim_terminal_attempt(&elected, true)
                .await
                .expect("elect source X"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy.clone(),
                    "marker-generation-durable-recheck",
                    Some(TEST_SOURCE_DIGEST_ELECTED),
                    2,
                    FinalizeCheckpointProgress::Durable,
                ))
                .await
                .expect("persist source X durable proof"),
            CaptureCatalogFinalizeResult::ReadyToComplete { .. }
        ));
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                stop_action.event_id,
                CheckpointWrite::Committed,
            );
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope)
             VALUES (?, 'session-a', 'committed')",
            [checkpoint_id.clone().into()],
        ))
        .await
        .expect("seed source X durable checkpoint");
        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        "marker-generation-source-y",
                        Some(TEST_SOURCE_DIGEST_Y),
                        3,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("classify source Y as durable replay"),
            CaptureCatalogTerminalAttempt::DurableReplay
        );

        let before = conn
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT metadata_json FROM agent_session WHERE provider_session_id = 'provider-a'"
                    .to_string(),
            ))
            .await
            .expect("read pending receipt")
            .expect("pending session row")
            .try_get_by::<String, _>("metadata_json")
            .expect("decode pending receipt");
        conn.execute_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "DELETE FROM agent_checkpoint WHERE checkpoint_id = ?",
            [checkpoint_id.into()],
        ))
        .await
        .expect("simulate repair removing checkpoint after claim");

        assert_eq!(
            store
                .complete_durable_replay(&stop)
                .await
                .expect("missing durable row is a typed conflict"),
            CaptureCatalogCompleteResult::ConflictUnchanged {
                conflict: CaptureCatalogConflict::FinalizerFence,
            }
        );
        let after = conn
            .query_one_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT metadata_json FROM agent_session WHERE provider_session_id = 'provider-a'"
                    .to_string(),
            ))
            .await
            .expect("read retained pending receipt")
            .expect("retained session row")
            .try_get_by::<String, _>("metadata_json")
            .expect("decode retained receipt");
        assert_eq!(
            before, after,
            "a missing checkpoint cannot alter the receipt"
        );
        assert_eq!(
            session_state(&conn).await,
            ("active".to_string(), None, 2),
            "receipt remains active and replayable after the recheck rejects completion"
        );
    }

    /// Once the elected source has durably registered its exact marker, a
    /// later snapshot is only an observer. It must leave the live writer's
    /// receipt untouched rather than quarantining the still-recoverable
    /// source fence.
    #[tokio::test]
    async fn changed_source_duplicate_with_registered_marker_adopts_without_quarantine() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(40, Some(receipt('5'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start request");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(41, Some(receipt('6')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);
        let elected_generation = Uuid::from_u128(0x41).to_string();

        assert!(matches!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy.clone(),
                        &elected_generation,
                        Some(TEST_SOURCE_DIGEST_ELECTED),
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("elect first terminal source"),
            CaptureCatalogTerminalAttempt::Bound { .. }
        ));
        let checkpoint_id =
            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                stop_action.event_id,
                CheckpointWrite::Committed,
            );
        let mut marker = crate::internal::ai::traces::TracesInflightMarker::new(
            "session-a",
            &checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        marker.generation = Some(elected_generation.clone());
        crate::internal::ai::traces::write_traces_inflight_marker(&conn, &marker)
            .await
            .expect("register elected writer marker");
        let before = session_state(&conn).await;

        assert_eq!(
            store
                .claim_terminal_attempt(
                    &finalize_request(
                        &stop,
                        policy,
                        &Uuid::from_u128(0x42).to_string(),
                        Some(TEST_SOURCE_DIGEST_LATER),
                        2,
                        FinalizeCheckpointProgress::NotStarted,
                    ),
                    true,
                )
                .await
                .expect("later source only observes registered attempt"),
            CaptureCatalogTerminalAttempt::Adopted {
                marker_generation: elected_generation,
                source_digest: Some(TEST_SOURCE_DIGEST_ELECTED.to_string()),
            }
        );
        assert_eq!(
            session_state(&conn).await,
            before,
            "a changed-source observer cannot mutate or quarantine a registered writer"
        );
    }

    #[tokio::test]
    async fn finalizer_attempt_and_wall_clock_limits_are_durable_repair_states() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(34, Some(receipt('f'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(35, Some(receipt('0')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);
        store
            .finalize(&finalize_request(
                &stop,
                policy.clone(),
                "marker-generation-a",
                None,
                1,
                FinalizeCheckpointProgress::NotStarted,
            ))
            .await
            .expect("first pending");
        for attempt in 2..=crate::internal::ai::capture::finalizer::MAX_FINALIZE_ATTEMPTS {
            assert!(matches!(
                store
                    .finalize(&finalize_request(
                        &stop,
                        policy.clone(),
                        "marker-generation-a",
                        None,
                        i64::from(attempt),
                        FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
                    ))
                    .await
                    .expect("bounded replay"),
                CaptureCatalogFinalizeResult::Pending { attempts, .. } if attempts == attempt
            ));
        }
        assert_eq!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy,
                    "marker-generation-a",
                    None,
                    7,
                    FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
                ))
                .await
                .expect("attempt limit"),
            CaptureCatalogFinalizeResult::Quarantined {
                reason: FinalizeQuarantineReason::AttemptLimit,
            }
        );
        assert_eq!(session_state(&conn).await.0, "quarantined");
    }

    #[tokio::test]
    async fn finalizer_window_expiry_quarantines_the_original_pending_receipt() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let start = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(36, Some(receipt('1'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("start");
        store.apply(&start).await.expect("start applied");
        let stop_action = action(37, Some(receipt('2')));
        let stop = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            stop_action.clone(),
            terminal_mutation(Some(DurableCaptureState {
                phase: CapturePhase::Active,
                stopped_at: None,
                sync_revision: 1,
            })),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("terminal reserved");
        let policy = finalizer_policy(&stop_action, None);
        store
            .finalize(&finalize_request(
                &stop,
                policy.clone(),
                "marker-generation-a",
                None,
                1,
                FinalizeCheckpointProgress::NotStarted,
            ))
            .await
            .expect("first pending");
        assert_eq!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy,
                    "marker-generation-a",
                    None,
                    1 + crate::internal::ai::capture::finalizer::MAX_FINALIZE_WINDOW_MILLIS + 1,
                    FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
                ))
                .await
                .expect("window expiry"),
            CaptureCatalogFinalizeResult::Quarantined {
                reason: FinalizeQuarantineReason::WindowLimit,
            }
        );
        assert_eq!(session_state(&conn).await.0, "quarantined");
    }

    #[test]
    fn terminal_mutation_without_checkpoint_is_rejected() {
        assert_eq!(
            CaptureCatalogMutation::new(
                None,
                CapturePhase::Stopped,
                StoppedAtMutation::Set(1_700_000_010),
                CheckpointWrite::None,
                1_700_000_010,
            ),
            Err(CaptureCatalogError::InvalidRequest)
        );
    }

    #[tokio::test]
    async fn foreign_scope_and_stale_workspace_fence_reject_before_mutation() {
        let conn = catalog_db().await;
        let store = CaptureCatalogStore::new(conn.clone());
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(4, Some(receipt('c'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("request");
        store.apply(&request).await.expect("initial scope claim");

        let mut foreign_scope = scope();
        foreign_scope.repo_id = "repo-b".to_string();
        let foreign = CaptureCatalogApplyRequest::new(
            foreign_scope,
            session(),
            action(5, Some(receipt('d'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("foreign request");
        assert_eq!(
            store
                .apply(&foreign)
                .await
                .expect_err("foreign scope rejects"),
            CaptureCatalogError::ScopeRejected
        );
        assert_eq!(session_revision(&conn).await, 1);

        let stale_scope = CaptureScope {
            repo_id: "repo-a".to_string(),
            worktree_id: "worktree-a".to_string(),
            workspace_id: Some("workspace-a".to_string()),
            workspace_fence: Some(9),
        };
        let stale = CaptureCatalogApplyRequest::new(
            stale_scope,
            CaptureCatalogSession::new("session-stale", "claude_code", "provider-stale", "/repo")
                .expect("session"),
            action(6, Some(receipt('e'))),
            mutation(None, CheckpointWrite::None),
        )
        .expect("stale request");
        assert_eq!(
            store
                .apply(&stale)
                .await
                .expect_err("missing lease rejects"),
            CaptureCatalogError::WorkspaceLeaseRejected
        );
        let count: i64 = conn
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM agent_session",
                [],
            ))
            .await
            .expect("count")
            .expect("count row")
            .try_get_by("count")
            .expect("count value");
        assert_eq!(count, 1, "rejected scope/fence never inserts a row");
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_terminal_completion_and_receipt() {
        let conn = catalog_db().await;
        seed_live_workspace_scope(&conn).await;
        let store = CaptureCatalogStore::new(conn.clone());
        let scoped = leased_scope();
        let stop_action = action(45, Some(receipt('5')));
        let stop = CaptureCatalogApplyRequest::new(
            scoped,
            session(),
            stop_action.clone(),
            terminal_mutation(None),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("reserve terminal receipt");
        let policy = finalizer_policy(&stop_action, None);
        assert!(matches!(
            store
                .finalize(&finalize_request(
                    &stop,
                    policy.clone(),
                    "scope-fence-complete",
                    None,
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                ))
                .await
                .expect("persist pending terminal finalizer"),
            CaptureCatalogFinalizeResult::Pending { .. }
        ));
        let proof = match store
            .finalize(&finalize_request(
                &stop,
                policy,
                "scope-fence-complete",
                None,
                2,
                FinalizeCheckpointProgress::Durable,
            ))
            .await
            .expect("derive terminal completion proof")
        {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => proof,
            other => panic!("expected terminal proof, got {other:?}"),
        };
        let completion = CaptureCatalogCompleteRequest::from_finalizer(&stop, proof)
            .expect("completion request");
        let pending_state = session_state(&conn).await;

        expire_scope_after_agent_session_update(&conn).await;
        assert_eq!(
            store
                .complete(&completion)
                .await
                .expect_err("expired scope rejects terminal completion"),
            CaptureCatalogError::WorkspaceLeaseRejected
        );
        assert_eq!(
            session_state(&conn).await,
            pending_state,
            "the receipt and terminal state roll back with the expired lease"
        );

        drop_scope_expiry_trigger(&conn).await;
        assert_eq!(
            store
                .complete(&completion)
                .await
                .expect("the rolled-back receipt remains completable"),
            CaptureCatalogCompleteResult::Completed
        );
        assert_eq!(
            session_state(&conn).await,
            ("stopped".to_string(), Some(1_700_000_010), 2)
        );
    }

    #[tokio::test]
    async fn finish_time_scope_fence_rolls_back_doctor_quarantine_recovery() {
        let conn = catalog_db().await;
        seed_live_workspace_scope(&conn).await;
        let store = CaptureCatalogStore::new(conn.clone());
        let scoped = leased_scope();
        let stop_action = action(46, Some(receipt('6')));
        let stop = CaptureCatalogApplyRequest::new(
            scoped.clone(),
            session(),
            stop_action.clone(),
            terminal_mutation(None),
        )
        .expect("terminal request");
        store.apply(&stop).await.expect("reserve terminal receipt");
        store
            .finalize(&finalize_request(
                &stop,
                finalizer_policy(&stop_action, None),
                "scope-fence-doctor",
                None,
                1,
                FinalizeCheckpointProgress::NotStarted,
            ))
            .await
            .expect("persist pending terminal finalizer");
        let recovery = CaptureCatalogFinalizerRecovery {
            scope: scoped,
            session: session(),
            action: stop_action.clone(),
            checkpoint_id:
                crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                    stop_action.event_id,
                    CheckpointWrite::Committed,
                ),
            budget_exhausted: true,
            superseded: false,
            quarantined: false,
            artifact_present: false,
            artifact_pending: false,
            artifact_manual_attempted: false,
        };
        let expiry = 1 + crate::internal::ai::capture::finalizer::MAX_FINALIZE_WINDOW_MILLIS + 1;
        let pending_state = session_state(&conn).await;

        expire_scope_after_agent_session_update(&conn).await;
        assert_eq!(
            store
                .quarantine_exhausted_pending_finalizer(&recovery, expiry)
                .await
                .expect_err("expired scope rejects doctor quarantine"),
            CaptureCatalogError::WorkspaceLeaseRejected
        );
        assert_eq!(
            session_state(&conn).await,
            pending_state,
            "doctor's quarantine transition rolls back with the expired lease"
        );

        drop_scope_expiry_trigger(&conn).await;
        assert_eq!(
            store
                .quarantine_exhausted_pending_finalizer(&recovery, expiry)
                .await
                .expect("the rolled-back doctor recovery remains retryable"),
            CaptureCatalogFinalizerRecoveryResult::Quarantined
        );
        assert_eq!(session_state(&conn).await.0, "quarantined");
    }

    /// A checkpoint row sharing the deterministic ID and session can still
    /// be a nonterminal checkpoint. Doctor must not let it certify a terminal
    /// completion or postpone repair of the exhausted original receipt.
    #[tokio::test]
    async fn doctor_noncommitted_checkpoint_neither_completes_terminal_nor_defers_exhaustion_quarantine()
     {
        for checkpoint_scope in [
            crate::internal::ai::traces::CheckpointScope::Subagent,
            crate::internal::ai::traces::CheckpointScope::Temporary,
        ] {
            let conn = catalog_db().await;
            let store = CaptureCatalogStore::new(conn.clone());
            let start = CaptureCatalogApplyRequest::new(
                scope(),
                session(),
                action(52, Some(receipt('7'))),
                mutation(None, CheckpointWrite::None),
            )
            .expect("start request");
            store.apply(&start).await.expect("start applied");
            let stop_action = action(53, Some(receipt('8')));
            let stop = CaptureCatalogApplyRequest::new(
                scope(),
                session(),
                stop_action.clone(),
                terminal_mutation(Some(DurableCaptureState {
                    phase: CapturePhase::Active,
                    stopped_at: None,
                    sync_revision: 1,
                })),
            )
            .expect("terminal request");
            store.apply(&stop).await.expect("terminal reserved");
            assert!(matches!(
                store
                    .finalize(&finalize_request(
                        &stop,
                        finalizer_policy(&stop_action, None),
                        "noncommitted-doctor-checkpoint",
                        None,
                        1,
                        FinalizeCheckpointProgress::NotStarted,
                    ))
                    .await
                    .expect("persist pending terminal finalizer"),
                CaptureCatalogFinalizeResult::Pending { .. }
            ));

            let expiry =
                1 + crate::internal::ai::capture::finalizer::MAX_FINALIZE_WINDOW_MILLIS + 1;
            let mut recoveries = store
                .pending_finalizer_recoveries_for_doctor(expiry)
                .await
                .expect("doctor scan pending finalizer")
                .recoveries;
            assert_eq!(recoveries.len(), 1, "doctor sees the pending receipt");
            let recovery = recoveries.pop().expect("one recovery");
            let checkpoint_id =
                crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                    stop_action.event_id,
                    CheckpointWrite::Committed,
                );
            assert_eq!(recovery.checkpoint_id(), checkpoint_id.as_str());
            assert!(recovery.budget_exhausted());
            conn.execute_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope)
                 VALUES (?, 'session-a', ?)",
                [checkpoint_id.into(), checkpoint_scope.as_str().into()],
            ))
            .await
            .expect("seed same-id noncommitted checkpoint");

            let pending_state = session_state(&conn).await;
            assert_eq!(
                store
                    .recover_pending_finalizer_after_durable_checkpoint(&recovery, expiry)
                    .await
                    .expect("noncommitted checkpoint does not complete terminal receipt"),
                CaptureCatalogFinalizerRecoveryResult::MissingDurableCheckpoint,
                "{checkpoint_scope:?} checkpoint cannot certify terminal durability"
            );
            assert_eq!(
                session_state(&conn).await,
                pending_state,
                "{checkpoint_scope:?} checkpoint leaves the terminal receipt pending"
            );
            assert_eq!(
                store
                    .quarantine_exhausted_pending_finalizer(&recovery, expiry)
                    .await
                    .expect("noncommitted checkpoint does not defer quarantine"),
                CaptureCatalogFinalizerRecoveryResult::Quarantined,
                "{checkpoint_scope:?} checkpoint cannot defer repair"
            );
            assert_eq!(session_state(&conn).await.0, "quarantined");
        }
    }

    #[test]
    fn receipt_ledger_is_bounded_and_never_evicts_pending_recovery_work() {
        let mut ledger = StoredReceiptLedger::default();
        for number in 0..MAX_CAPTURE_RECEIPTS {
            let event_id = Uuid::from_u128(number as u128 + 10);
            // A realistic ledger cannot contain duplicate receipt keys. Use a
            // deterministic distinct hex sequence instead.
            let receipt_key =
                OpaqueCaptureReceiptKey::parse(format!("{RECEIPT_PREFIX_V1}{:064x}", number + 1))
                    .expect("receipt");
            ledger
                .insert(StoredReceipt::new(
                    receipt_key.as_str(),
                    &action(event_id.as_u128(), Some(receipt_key.clone())),
                    &mutation(None, CheckpointWrite::None),
                    StoredReceiptStatus::Complete,
                    1,
                ))
                .expect("completed receipt fits");
        }
        let next_key = OpaqueCaptureReceiptKey::parse(format!(
            "{RECEIPT_PREFIX_V1}{:064x}",
            MAX_CAPTURE_RECEIPTS + 1
        ))
        .expect("receipt");
        ledger
            .insert(StoredReceipt::new(
                next_key.as_str(),
                &action(999, Some(next_key.clone())),
                &mutation(None, CheckpointWrite::None),
                StoredReceiptStatus::Complete,
                1,
            ))
            .expect("old completed receipt is pruned");
        assert_eq!(ledger.entries.len(), MAX_CAPTURE_RECEIPTS);

        for entry in &mut ledger.entries {
            entry.status = StoredReceiptStatus::Pending;
        }
        let pending_key = OpaqueCaptureReceiptKey::parse(format!(
            "{RECEIPT_PREFIX_V1}{:064x}",
            MAX_CAPTURE_RECEIPTS + 2
        ))
        .expect("receipt");
        assert_eq!(
            ledger
                .insert(StoredReceipt::new(
                    pending_key.as_str(),
                    &action(1000, Some(pending_key.clone())),
                    &mutation(None, CheckpointWrite::Committed),
                    StoredReceiptStatus::Pending,
                    1,
                ))
                .expect_err("pending receipts cannot be discarded"),
            CaptureCatalogError::ReceiptCapacityExhausted
        );
    }

    #[tokio::test]
    async fn fake_faults_are_pre_mutation_and_individually_injectable() {
        let store = FakeCaptureCatalogStore::default();
        let request = CaptureCatalogApplyRequest::new(
            scope(),
            session(),
            action(7, Some(receipt('f'))),
            mutation(None, CheckpointWrite::Committed),
        )
        .expect("request");
        for fault in [
            CaptureCatalogFault::Conflict,
            CaptureCatalogFault::LostFence,
            CaptureCatalogFault::CommitFailure,
        ] {
            store.enqueue_fault(fault).expect("arm fault");
            match fault {
                CaptureCatalogFault::Conflict => assert!(matches!(
                    store.apply(&request).await.expect("conflict result"),
                    CaptureCatalogApplyResult::ConflictUnchanged { .. }
                )),
                CaptureCatalogFault::LostFence => assert_eq!(
                    store.apply(&request).await.expect_err("lost fence"),
                    CaptureCatalogError::ScopeRejected
                ),
                CaptureCatalogFault::CommitFailure => assert_eq!(
                    store.apply(&request).await.expect_err("commit failure"),
                    CaptureCatalogError::CommitFailed
                ),
            }
        }
        assert!(matches!(
            store.apply(&request).await.expect("first real mutation"),
            CaptureCatalogApplyResult::Applied { .. }
        ));
    }

    #[tokio::test]
    async fn fake_terminal_claim_faults_are_pre_mutation_and_individually_injectable() {
        let store = FakeCaptureCatalogStore::default();
        let (stop, _, policy) = reserve_fake_terminal_attempt(&store, 0xf1).await;
        let request = finalize_request(
            &stop,
            policy,
            "fake-marker-fault",
            Some(TEST_SOURCE_DIGEST_X),
            1,
            FinalizeCheckpointProgress::NotStarted,
        );
        for fault in [
            CaptureCatalogFault::Conflict,
            CaptureCatalogFault::LostFence,
            CaptureCatalogFault::CommitFailure,
        ] {
            store
                .enqueue_fault(fault)
                .expect("arm terminal claim fault");
            match fault {
                CaptureCatalogFault::Conflict => assert_eq!(
                    store
                        .claim_terminal_attempt(&request, true)
                        .await
                        .expect("terminal claim conflict"),
                    CaptureCatalogTerminalAttempt::ConflictUnchanged {
                        conflict: CaptureCatalogConflict::ConditionalWrite,
                    }
                ),
                CaptureCatalogFault::LostFence => assert_eq!(
                    store
                        .claim_terminal_attempt(&request, true)
                        .await
                        .expect_err("terminal claim lost fence"),
                    CaptureCatalogError::ScopeRejected
                ),
                CaptureCatalogFault::CommitFailure => assert_eq!(
                    store
                        .claim_terminal_attempt(&request, true)
                        .await
                        .expect_err("terminal claim commit failure"),
                    CaptureCatalogError::CommitFailed
                ),
            }
        }
        let key = request.action.completion_receipt_storage_key();
        assert!(
            store
                .state
                .lock()
                .expect("read unmodified fake terminal receipt")
                .sessions[0]
                .ledger
                .find(&key)
                .expect("reserved fake terminal receipt")
                .finalizer
                .is_none(),
            "a claimed fault must be consumed before it binds a finalizer"
        );
        assert!(matches!(
            store
                .claim_terminal_attempt(&request, true)
                .await
                .expect("first unfaulted terminal claim"),
            CaptureCatalogTerminalAttempt::Bound { attempts: 1, .. }
        ));
    }
}
