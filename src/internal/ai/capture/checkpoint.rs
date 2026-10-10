//! Typed checkpoint persistence boundary for Agent Capture.
//!
//! The trace writer remains the owner of the `refs/libra/traces` layout.  This
//! module deliberately presents a request/result port instead: callers can
//! supply only a complete [`CaptureSnapshot`] (and therefore redacted bytes),
//! a previously registered marker generation, and the transaction companion
//! writes that must commit with the ref CAS.  It is intentionally agnostic to
//! provider names and ref names.

use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use anyhow::Context;
use async_trait::async_trait;
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction, Statement};
use thiserror::Error;

#[cfg(test)]
use crate::internal::ai::capture::snapshot::CaptureSnapshot;
use crate::internal::ai::{
    capture::{
        catalog::CaptureCatalogTerminalAttemptFence, snapshot::CaptureSnapshotProjection,
        state::CheckpointWrite,
    },
    capture_scope::{CaptureCommitDeadline, CaptureScope},
    history::{CheckpointAppendConflict, CheckpointCompanionTransactionFailed},
    observed_agents::RedactedBytes,
    traces::{
        self, CheckpointCommitParams, CheckpointScope, TracesCommitCtx, TracesCoverageFence,
        TracesInflightMarker, TracesTxnExtra,
    },
};

/// A completed ref/catalog CAS already has durable checkpoint state.  The
/// follow-up marker refresh is recovery evidence only, so it gets one small,
/// fixed opportunity to acquire SQLite rather than borrowing the capture
/// deadline's unbounded connection wait.
const POST_CAS_MARKER_REFRESH_GRACE: Duration = Duration::from_millis(250);

/// Bound only pre-mutation SQLite reads for a deadline-aware checkpoint.
///
/// Marker/ref/catalog mutations and their commits intentionally stay outside
/// this helper: cancelling a dispatched write cannot establish that SQLite
/// rolled it back.
async fn await_checkpoint_precommit_read_until<T>(
    deadline: Option<CaptureCommitDeadline>,
    read: impl std::future::Future<Output = Result<T, CheckpointStoreError>>,
) -> Result<T, CheckpointStoreError> {
    if let Some(deadline) = deadline {
        if Instant::now() >= deadline.monotonic() {
            return Err(CheckpointStoreError::DeadlineExceeded);
        }
        return tokio::time::timeout_at(tokio::time::Instant::from_std(deadline.monotonic()), read)
            .await
            .map_err(|_| CheckpointStoreError::DeadlineExceeded)?;
    }
    read.await
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::{
        cell::Cell,
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex, OnceLock,
            atomic::{AtomicBool, Ordering},
        },
    };

    use async_trait::async_trait;
    use sea_orm::{DatabaseConnection, DatabaseTransaction};
    use tokio::sync::Notify;

    use crate::internal::ai::{
        history::HistoryManager,
        traces::{
            self, CheckpointCommitParams, TracesCommitCtx, TracesInflightMarker, TracesTxnExtra,
        },
    };

    /// One-shot fault points along a single [`super::TracesCheckpointStore`]
    /// write. Each is consumed by the first write that reaches it, so a
    /// replay of the same request runs the unmodified production path.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) enum CheckpointFaultPoint {
        /// Marker registration fails before any marker is published.
        MarkerRegistration,
        /// The marker is registered, then the attempt fails before any
        /// object is written.
        AfterMarkerRegistration,
        /// Every attempt object is written and owned by the marker, then the
        /// attempt fails before the ref CAS transaction.
        AfterObjectWrite,
        /// Every bounded ref-CAS attempt loses to a concurrent traces head.
        RefCasExhausted,
        /// An expired takeover replaces the marker generation after
        /// registration and before append.
        StaleMarkerGeneration,
        /// The companion catalog transaction applies, then fails inside the
        /// ref-CAS transaction.
        CompanionTransaction,
        /// The post-CAS ordinary marker retirement fails.
        MarkerCleanup,
    }

    impl CheckpointFaultPoint {
        pub(crate) const ALL: [Self; 7] = [
            Self::MarkerRegistration,
            Self::AfterMarkerRegistration,
            Self::AfterObjectWrite,
            Self::RefCasExhausted,
            Self::StaleMarkerGeneration,
            Self::CompanionTransaction,
            Self::MarkerCleanup,
        ];
    }

    thread_local! {
        static ARMED_FAULT: Cell<Option<CheckpointFaultPoint>> = const { Cell::new(None) };
    }

    #[derive(Debug)]
    pub(crate) struct RegistrationPause {
        entered: Notify,
        release: Notify,
        released: AtomicBool,
    }

    #[derive(Debug)]
    struct ArmedRegistrationPause {
        repo_path: PathBuf,
        pause: Arc<RegistrationPause>,
    }

    #[derive(Clone, Copy)]
    enum RegistrationPauseStage {
        BeforeRegistration,
        AfterRegistration,
    }

    pub(crate) struct RegistrationPauseGuard {
        repo_path: PathBuf,
        pause: Arc<RegistrationPause>,
        stage: RegistrationPauseStage,
    }

    static REGISTRATION_PAUSE: OnceLock<Mutex<Vec<ArmedRegistrationPause>>> = OnceLock::new();
    static PRE_REGISTRATION_PAUSE: OnceLock<Mutex<Vec<ArmedRegistrationPause>>> = OnceLock::new();

    fn registration_pause_slot() -> &'static Mutex<Vec<ArmedRegistrationPause>> {
        REGISTRATION_PAUSE.get_or_init(|| Mutex::new(Vec::new()))
    }

    fn pre_registration_pause_slot() -> &'static Mutex<Vec<ArmedRegistrationPause>> {
        PRE_REGISTRATION_PAUSE.get_or_init(|| Mutex::new(Vec::new()))
    }

    fn pause_slot(stage: RegistrationPauseStage) -> &'static Mutex<Vec<ArmedRegistrationPause>> {
        match stage {
            RegistrationPauseStage::BeforeRegistration => pre_registration_pause_slot(),
            RegistrationPauseStage::AfterRegistration => registration_pause_slot(),
        }
    }

    fn arm_registration_pause(
        repo_path: &Path,
        stage: RegistrationPauseStage,
    ) -> RegistrationPauseGuard {
        let pause = Arc::new(RegistrationPause {
            entered: Notify::new(),
            release: Notify::new(),
            released: AtomicBool::new(false),
        });
        pause_slot(stage)
            .lock()
            .expect("checkpoint test registration-pause lock")
            .push(ArmedRegistrationPause {
                repo_path: repo_path.to_path_buf(),
                pause: pause.clone(),
            });
        RegistrationPauseGuard {
            repo_path: repo_path.to_path_buf(),
            pause,
            stage,
        }
    }

    fn take_registration_pause(
        repo_path: &Path,
        stage: RegistrationPauseStage,
    ) -> Option<Arc<RegistrationPause>> {
        let mut pauses = pause_slot(stage)
            .lock()
            .expect("checkpoint test registration-pause lock");
        let index = pauses
            .iter()
            .position(|armed| armed.repo_path == repo_path)?;
        Some(pauses.swap_remove(index).pause)
    }

    fn disarm_registration_pause(
        repo_path: &Path,
        pause: &Arc<RegistrationPause>,
        stage: RegistrationPauseStage,
    ) {
        let mut pauses = pause_slot(stage)
            .lock()
            .expect("checkpoint test registration-pause lock");
        if let Some(index) = pauses
            .iter()
            .position(|armed| armed.repo_path == repo_path && Arc::ptr_eq(&armed.pause, pause))
        {
            pauses.swap_remove(index);
        }
    }

    /// Arm exactly one fault for the next store write on this thread.
    pub(crate) fn fail_once_at(point: CheckpointFaultPoint) {
        ARMED_FAULT.with(|armed| armed.set(Some(point)));
    }

    /// The fault still waiting to fire, if any. Replay matrices assert this
    /// is `None` so a fault that was never reached cannot pass vacuously.
    pub(crate) fn armed_fault() -> Option<CheckpointFaultPoint> {
        ARMED_FAULT.with(Cell::get)
    }

    pub(crate) fn take_fault(point: CheckpointFaultPoint) -> bool {
        ARMED_FAULT.with(|armed| {
            if armed.get() == Some(point) {
                armed.set(None);
                true
            } else {
                false
            }
        })
    }

    pub(crate) fn fail_marker_cleanup_once() {
        fail_once_at(CheckpointFaultPoint::MarkerCleanup);
    }

    pub(crate) fn fail_post_registration_once() {
        fail_once_at(CheckpointFaultPoint::AfterMarkerRegistration);
    }

    /// Install the armed history-level fault on the manager built for this
    /// write. Both seams fire inside `append_checkpoint_commit`, so the
    /// store's real error classification and marker handling run after them.
    pub(crate) fn with_history_fault(mut manager: HistoryManager) -> HistoryManager {
        if take_fault(CheckpointFaultPoint::AfterObjectWrite) {
            manager.fail_once_before_checkpoint_ref_cas();
        } else if take_fault(CheckpointFaultPoint::RefCasExhausted) {
            manager.lose_every_checkpoint_ref_cas();
        }
        manager
    }

    /// Model a recovery takeover: replace the registered marker with an
    /// expired, empty marker of a different generation under the same key.
    pub(crate) async fn replace_marker_with_expired_takeover_if_armed(
        conn: &DatabaseConnection,
        marker: &TracesInflightMarker,
    ) {
        if take_fault(CheckpointFaultPoint::StaleMarkerGeneration) {
            let takeover = TracesInflightMarker::new(&marker.session_id, &marker.attempt_id, 0);
            traces::write_traces_inflight_marker(conn, &takeover)
                .await
                .expect("replace checkpoint marker with an expired takeover generation");
        }
    }

    /// Companion that applies the request's real transaction extra and then
    /// fails, proving the ref, catalog row, and claims roll back together.
    pub(crate) struct FailingCompanion<'a> {
        inner: Option<&'a dyn TracesTxnExtra>,
    }

    #[async_trait]
    impl TracesTxnExtra for FailingCompanion<'_> {
        async fn apply(
            &self,
            txn: &DatabaseTransaction,
            ctx: &TracesCommitCtx,
        ) -> anyhow::Result<()> {
            if let Some(inner) = self.inner {
                inner.apply(txn, ctx).await?;
            }
            anyhow::bail!("injected checkpoint companion transaction failure")
        }
    }

    pub(crate) fn companion_fault<'a>(
        inner: Option<&'a dyn TracesTxnExtra>,
    ) -> Option<FailingCompanion<'a>> {
        take_fault(CheckpointFaultPoint::CompanionTransaction).then_some(FailingCompanion { inner })
    }

    pub(crate) fn with_companion_fault<'p>(
        mut params: CheckpointCommitParams<'p>,
        companion: Option<&'p FailingCompanion<'_>>,
    ) -> CheckpointCommitParams<'p> {
        if let Some(companion) = companion {
            params.txn_extra = Some(companion);
        }
        params
    }

    /// Pause exactly one in-process writer after it has durably registered
    /// its marker and before it begins object/ref work. This verifies the
    /// duplicate terminal delivery protocol without exposing a hook-host
    /// environment knob in production binaries.
    pub(crate) fn pause_after_registration_once(
        repo_path: impl AsRef<Path>,
    ) -> RegistrationPauseGuard {
        arm_registration_pause(
            repo_path.as_ref(),
            RegistrationPauseStage::AfterRegistration,
        )
    }

    /// Pause one in-process writer after catalog election but before marker
    /// registration. This models a process that loses its source snapshot at
    /// the narrow bind-to-marker boundary, without exposing a hook-host fault
    /// knob in production binaries.
    pub(crate) fn pause_before_registration_once(
        repo_path: impl AsRef<Path>,
    ) -> RegistrationPauseGuard {
        arm_registration_pause(
            repo_path.as_ref(),
            RegistrationPauseStage::BeforeRegistration,
        )
    }

    pub(crate) async fn pause_after_registration_if_armed(repo_path: &Path) {
        let pause = take_registration_pause(repo_path, RegistrationPauseStage::AfterRegistration);
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    pub(crate) async fn pause_before_registration_if_armed(repo_path: &Path) {
        let pause = take_registration_pause(repo_path, RegistrationPauseStage::BeforeRegistration);
        if let Some(pause) = pause {
            pause.entered.notify_one();
            pause.release.notified().await;
        }
    }

    impl RegistrationPauseGuard {
        pub(crate) async fn wait_until_entered(&self) {
            self.pause.entered.notified().await;
        }

        pub(crate) fn release(&self) {
            disarm_registration_pause(&self.repo_path, &self.pause, self.stage);
            if !self.pause.released.swap(true, Ordering::AcqRel) {
                self.pause.release.notify_one();
            }
        }
    }

    impl Drop for RegistrationPauseGuard {
        fn drop(&mut self) {
            self.release();
        }
    }
}

/// Fixed namespace for deterministic checkpoint IDs derived from a canonical
/// ingress event. This belongs to the checkpoint boundary so hook, import,
/// and doctor recovery all reconstruct the same durable identity.
const CAPTURE_CHECKPOINT_ID_NAMESPACE: uuid::Uuid = uuid::Uuid::from_bytes([
    0x46, 0x10, 0x71, 0x1d, 0x6e, 0x76, 0x49, 0x2f, 0xa1, 0x24, 0x11, 0x98, 0x52, 0x4d, 0x2c, 0x87,
]);

/// Return the replay-stable ID for a checkpoint caused by one canonical
/// ingress event. The UUID name contains no source content: only the already
/// normalized event UUID and the checkpoint class participate.
pub(crate) fn checkpoint_id_for_capture_action(
    event_id: uuid::Uuid,
    checkpoint: CheckpointWrite,
) -> String {
    let checkpoint_class = match checkpoint {
        CheckpointWrite::None => "none",
        CheckpointWrite::Committed => "committed",
        CheckpointWrite::SubagentBoundary => "subagent",
    };
    let mut name = Vec::with_capacity(48);
    name.extend_from_slice(b"libra-capture-checkpoint-v1\0");
    name.extend_from_slice(checkpoint_class.as_bytes());
    name.push(0);
    name.extend_from_slice(event_id.as_bytes());
    uuid::Uuid::new_v5(&CAPTURE_CHECKPOINT_ID_NAMESPACE, &name).to_string()
}

/// The only checkpoint payload accepted by the capture store.
///
/// Construction consumes a complete [`CaptureSnapshot`], so a caller cannot
/// accidentally pass arbitrary raw source bytes to the durable trace sink.
#[derive(Debug)]
pub struct CheckpointRedactedPayload {
    transcript: RedactedBytes,
    snapshot: CaptureSnapshotProjection,
    provenance: CheckpointPayloadProvenance,
    metadata_json: RedactedBytes,
    lifecycle_events_jsonl: RedactedBytes,
    redaction_report_json: RedactedBytes,
}

/// What the redacted transcript blob represents relative to its safe source
/// projection.  This prevents an import's compact per-turn JSONL line from
/// ever being mislabeled as the full authorized provider transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckpointPayloadProvenance {
    ExactSnapshot,
    DerivedTurnProjection,
    MetadataFallback,
}

/// Immutable evidence that the repository-private envelope MAC was checked.
/// Artifact parsing, expected binding and current receipt/coverage fences are
/// still the pending store's responsibility. No unchecked constructor exists.
/// This capability is not Debug/Serialize and cannot authorize a source read.
pub(crate) struct AuthenticatedPendingEnvelope<'a> {
    bytes: &'a [u8],
    scope: &'a CaptureScope,
}

impl<'a> AuthenticatedPendingEnvelope<'a> {
    pub(crate) async fn verify<C: ConnectionTrait>(
        conn: &C,
        storage: &Path,
        root: &Path,
        scope: &'a CaptureScope,
        bytes: &'a [u8],
        mac: &str,
        deadline: Instant,
    ) -> anyhow::Result<Self> {
        // Sidecar commitments share the private artifact MAC domain but have
        // a binary, purpose-framed body. Only a canonical JSON object can be
        // an envelope; reject other purposes before issuing a witness.
        anyhow::ensure!(
            bytes.first() == Some(&b'{'),
            "capture recovery authentication failed; run `libra agent doctor`"
        );
        scope
            .verify_pending_envelope_until(conn, storage, root, bytes, mac, deadline)
            .await?;
        Ok(Self { bytes, scope })
    }

    pub(crate) fn bytes(&self) -> &[u8] {
        self.bytes
    }

    pub(crate) fn scope(&self) -> &CaptureScope {
        self.scope
    }
}

impl CheckpointRedactedPayload {
    /// Turn a complete snapshot and already-redacted sidecars into a sealed
    /// persistence payload.  Partial snapshots must be handled by the
    /// coordinator/finalizer; writing one as an empty complete checkpoint
    /// would make a data-loss failure look successful.
    #[cfg(test)]
    pub fn from_snapshot(
        snapshot: CaptureSnapshot,
        metadata_json: RedactedBytes,
        lifecycle_events_jsonl: RedactedBytes,
        redaction_report_json: RedactedBytes,
    ) -> Result<Self, CheckpointStoreError> {
        let projection = snapshot.safe_projection();
        let transcript = snapshot.into_redacted_transcript().ok_or(
            CheckpointStoreError::IncompleteSnapshot {
                reason: projection
                    .partial_reason
                    .map(|reason| format!("{reason:?}"))
                    .unwrap_or_else(|| "missing redacted transcript".to_string()),
            },
        )?;
        Ok(Self {
            transcript,
            snapshot: projection,
            provenance: CheckpointPayloadProvenance::ExactSnapshot,
            metadata_json,
            lifecycle_events_jsonl,
            redaction_report_json,
        })
    }

    /// Construct a checkpoint payload from redacted capture material.
    ///
    /// The primary path uses [`Self::from_snapshot`].  This compatibility
    /// constructor preserves existing metadata-only checkpoints when a
    /// provider has no authorized transcript source for an event. It accepts
    /// only redacted bytes plus a safe snapshot projection, never raw input;
    /// a complete projection is checked against the supplied redacted length
    /// so callers cannot label unrelated fallback text as a source snapshot.
    pub(crate) fn from_redacted_capture(
        transcript: RedactedBytes,
        snapshot: Option<CaptureSnapshotProjection>,
        metadata_json: RedactedBytes,
        lifecycle_events_jsonl: RedactedBytes,
        redaction_report_json: RedactedBytes,
    ) -> Result<Self, CheckpointStoreError> {
        let snapshot = snapshot.unwrap_or(CaptureSnapshotProjection {
            completeness:
                crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Partial,
            partial_reason: Some(
                crate::internal::ai::capture::snapshot::CaptureSnapshotPartialReason::SourceAbsent,
            ),
            source: None,
            transcript_redacted_bytes: 0,
            redaction_match_count: 0,
            redaction_bytes_scanned: 0,
            redaction_bytes_redacted: 0,
        });
        if !snapshot.source_commitment_is_durable_or_absent() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "source snapshot commitment".to_string(),
            });
        }
        let provenance = if snapshot.completeness
            == crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Complete
        {
            if snapshot.source.is_none()
                || !snapshot.has_durable_source_commitment()
                || snapshot.transcript_redacted_bytes != transcript.len()
            {
                return Err(CheckpointStoreError::IncompleteSnapshot {
                    reason: "complete source projection does not match the redacted transcript"
                        .to_string(),
                });
            }
            CheckpointPayloadProvenance::ExactSnapshot
        } else {
            CheckpointPayloadProvenance::MetadataFallback
        };
        Ok(Self {
            transcript,
            snapshot,
            provenance,
            metadata_json,
            lifecycle_events_jsonl,
            redaction_report_json,
        })
    }

    /// Construct a compact historical-import turn payload.
    ///
    /// `transcript` is a schema-controlled, redacted JSONL projection of one
    /// turn, while `source_snapshot` describes the complete authorized source
    /// from which that turn was derived. Their byte lengths intentionally do
    /// not match. The distinct provenance makes that difference explicit to
    /// every store consumer.
    pub(crate) fn from_derived_turn_projection(
        transcript: RedactedBytes,
        source_snapshot: CaptureSnapshotProjection,
        metadata_json: RedactedBytes,
        lifecycle_events_jsonl: RedactedBytes,
        redaction_report_json: RedactedBytes,
    ) -> Result<Self, CheckpointStoreError> {
        if source_snapshot.completeness
            != crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Complete
            || source_snapshot.source.is_none()
            || !source_snapshot.has_durable_source_commitment()
            || source_snapshot.transcript_redacted_bytes == 0
        {
            return Err(CheckpointStoreError::IncompleteSnapshot {
                reason: "derived turn projection requires a complete authorized source snapshot"
                    .to_string(),
            });
        }
        Ok(Self {
            transcript,
            snapshot: source_snapshot,
            provenance: CheckpointPayloadProvenance::DerivedTurnProjection,
            metadata_json,
            lifecycle_events_jsonl,
            redaction_report_json,
        })
    }

    /// Safe provenance for diagnostics and metadata.  It contains no
    /// transcript content.
    pub fn snapshot(&self) -> &CaptureSnapshotProjection {
        &self.snapshot
    }

    pub(crate) fn is_exact_complete_snapshot(&self) -> bool {
        self.provenance == CheckpointPayloadProvenance::ExactSnapshot
            && self.snapshot.completeness
                == crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Complete
            && self.snapshot.has_durable_source_commitment()
            && self.snapshot.transcript_redacted_bytes == self.transcript.len()
    }

    fn provenance(&self) -> CheckpointPayloadProvenance {
        self.provenance
    }

    pub(crate) fn transcript(&self) -> &RedactedBytes {
        &self.transcript
    }

    pub(crate) fn metadata_json(&self) -> &RedactedBytes {
        &self.metadata_json
    }

    pub(crate) fn lifecycle_events_jsonl(&self) -> &RedactedBytes {
        &self.lifecycle_events_jsonl
    }

    pub(crate) fn redaction_report_json(&self) -> &RedactedBytes {
        &self.redaction_report_json
    }
}

/// A request to write one capture checkpoint.
///
/// Fields remain private to prevent call sites from forging an incomplete
/// redaction/marker contract.  The constructor deliberately has no provider
/// or trace-ref argument: both belong behind the store implementation.
pub struct CheckpointWriteRequest<'a> {
    replay_key: &'a str,
    checkpoint_id: &'a str,
    session_id: &'a str,
    agent_kind: &'a str,
    parent_commit: Option<&'a str>,
    scope: CheckpointScope,
    marker_generation: &'a str,
    tool_use_id: Option<&'a str>,
    payload: &'a CheckpointRedactedPayload,
    txn_extra: Option<&'a dyn TracesTxnExtra>,
    deadline: Option<CaptureCommitDeadline>,
}

impl<'a> CheckpointWriteRequest<'a> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        replay_key: &'a str,
        checkpoint_id: &'a str,
        session_id: &'a str,
        agent_kind: &'a str,
        parent_commit: Option<&'a str>,
        scope: CheckpointScope,
        marker_generation: &'a str,
        tool_use_id: Option<&'a str>,
        payload: &'a CheckpointRedactedPayload,
        txn_extra: Option<&'a dyn TracesTxnExtra>,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<Self, CheckpointStoreError> {
        for (label, value) in [
            ("checkpoint replay key", replay_key),
            ("checkpoint id", checkpoint_id),
            ("checkpoint session id", session_id),
            ("checkpoint agent kind", agent_kind),
            ("checkpoint marker generation", marker_generation),
        ] {
            if value.is_empty() {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: label.to_string(),
                });
            }
        }
        Ok(Self {
            replay_key,
            checkpoint_id,
            session_id,
            agent_kind,
            parent_commit,
            scope,
            marker_generation,
            tool_use_id,
            payload,
            txn_extra,
            deadline,
        })
    }

    pub fn replay_key(&self) -> &str {
        self.replay_key
    }

    pub fn checkpoint_id(&self) -> &str {
        self.checkpoint_id
    }

    pub fn session_id(&self) -> &str {
        self.session_id
    }

    pub fn scope(&self) -> CheckpointScope {
        self.scope
    }

    pub fn deadline(&self) -> Option<CaptureCommitDeadline> {
        self.deadline
    }

    pub(crate) fn agent_kind(&self) -> &str {
        self.agent_kind
    }

    pub(crate) fn parent_commit(&self) -> Option<&str> {
        self.parent_commit
    }

    pub(crate) fn marker_generation(&self) -> &str {
        self.marker_generation
    }

    pub(crate) fn tool_use_id(&self) -> Option<&str> {
        self.tool_use_id
    }

    pub(crate) fn payload(&self) -> &CheckpointRedactedPayload {
        self.payload
    }

    pub(crate) fn txn_extra(&self) -> Option<&dyn TracesTxnExtra> {
        self.txn_extra
    }
}

/// Fixed token for provider-derived identities in diagnostics. A canonical
/// session ID can embed native provider material, and a tool-use ID is
/// provider-chosen, so neither may appear verbatim (or as a derivative) in
/// `Debug` output.
const REDACTED_IDENTITY: &str = "***";

impl std::fmt::Debug for CheckpointWriteRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckpointWriteRequest")
            .field("replay_key", &self.replay_key)
            .field("checkpoint_id", &self.checkpoint_id)
            .field("session_id", &REDACTED_IDENTITY)
            .field("agent_kind", &self.agent_kind)
            .field("scope", &self.scope)
            .field("marker_generation", &"<opaque>")
            .field("tool_use_id", &self.tool_use_id.map(|_| REDACTED_IDENTITY))
            .field("snapshot", self.payload.snapshot())
            .field("payload_provenance", &self.payload.provenance())
            .field("deadline", &self.deadline)
            .finish()
    }
}

/// The durable `agent_checkpoint` companion of a successful traces ref CAS.
/// It is intentionally owned by the checkpoint seam, not by coverage or an
/// import adapter: the IDs from [`TracesCommitCtx`] are meaningful only in the
/// ref transaction that produced them.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct CheckpointCatalogMutation {
    checkpoint_id: String,
    session_id: String,
    scope: CheckpointScope,
    parent_commit: Option<String>,
    created_at: i64,
}

impl std::fmt::Debug for CheckpointCatalogMutation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CheckpointCatalogMutation")
            .field("checkpoint_id", &self.checkpoint_id)
            .field("session_id", &REDACTED_IDENTITY)
            .field("scope", &self.scope)
            .field("parent_commit", &self.parent_commit)
            .field("created_at", &self.created_at)
            .finish()
    }
}

impl CheckpointCatalogMutation {
    pub(crate) fn new(
        checkpoint_id: impl Into<String>,
        session_id: impl Into<String>,
        scope: CheckpointScope,
        parent_commit: Option<String>,
        created_at: i64,
    ) -> Result<Self, CheckpointStoreError> {
        let mutation = Self {
            checkpoint_id: checkpoint_id.into(),
            session_id: session_id.into(),
            scope,
            parent_commit,
            created_at,
        };
        if mutation.checkpoint_id.is_empty() || mutation.session_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "checkpoint catalog identity".to_string(),
            });
        }
        Ok(mutation)
    }

    /// Execute strictly inside the transaction supplied by the traces writer.
    /// `ON CONFLICT DO NOTHING` is the same crash-retry backstop as the
    /// legacy coverage path; a caller must still advance its own fences in
    /// this transaction before it reports success.
    pub(crate) async fn apply(
        &self,
        txn: &DatabaseTransaction,
        ctx: &TracesCommitCtx,
    ) -> Result<(), CheckpointStoreError> {
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                self.checkpoint_id.clone().into(),
                self.session_id.clone().into(),
                self.scope.as_str().into(),
                self.parent_commit.clone().into(),
                ctx.tree_oid.clone().into(),
                ctx.metadata_blob_oid.clone().into(),
                ctx.commit_hash.clone().into(),
                self.created_at.into(),
            ],
        ))
        .await
        .map_err(|_| CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::CatalogTransaction,
        })?;
        Ok(())
    }
}

/// Transaction companion for a `scope='subagent'` checkpoint. The linkage
/// columns are part of the checkpoint contract and therefore live beside the
/// traces transaction boundary rather than in a hook adapter.
pub(crate) struct SubagentCheckpointCommitPlan {
    checkpoint_id: String,
    session_id: String,
    parent_checkpoint_id: Option<String>,
    parent_commit: Option<String>,
    tool_use_id: Option<String>,
    subagent_session_id: Option<String>,
    description: Option<String>,
    created_at: i64,
    capture_scope: CaptureScope,
}

impl SubagentCheckpointCommitPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        checkpoint_id: String,
        session_id: String,
        parent_checkpoint_id: Option<String>,
        parent_commit: Option<String>,
        tool_use_id: Option<String>,
        subagent_session_id: Option<String>,
        description: Option<String>,
        created_at: i64,
        capture_scope: CaptureScope,
    ) -> Result<Self, CheckpointStoreError> {
        if checkpoint_id.is_empty() || session_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "subagent checkpoint identity".to_string(),
            });
        }
        Ok(Self {
            checkpoint_id,
            session_id,
            parent_checkpoint_id,
            parent_commit,
            tool_use_id,
            subagent_session_id,
            description,
            created_at,
            capture_scope,
        })
    }
}

#[async_trait]
impl TracesTxnExtra for SubagentCheckpointCommitPlan {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> anyhow::Result<()> {
        self.capture_scope
            .assert_workspace_fence_live(txn)
            .await
            .context("verify capture workspace lease before subagent checkpoint transaction")?;
        let writable = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 AS writable
                 FROM agent_session s
                 WHERE s.session_id = ?
                   AND NOT EXISTS (
                     SELECT 1 FROM agent_import_tombstone t
                     WHERE t.agent_kind = s.agent_kind
                       AND t.provider_session_id = s.provider_session_id
                   )",
                [self.session_id.clone().into()],
            ))
            .await
            .context("verify subagent tombstone write barrier")?;
        if writable.is_none() {
            anyhow::bail!(
                "agent session was erased or tombstoned while the subagent checkpoint was in flight"
            );
        }
        txn.execute_raw(Statement::from_sql_and_values(
            txn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at
             ) VALUES (?, ?, ?, 'subagent', ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                self.checkpoint_id.clone().into(),
                self.session_id.clone().into(),
                self.parent_checkpoint_id.clone().into(),
                self.parent_commit.clone().into(),
                ctx.tree_oid.clone().into(),
                ctx.metadata_blob_oid.clone().into(),
                ctx.commit_hash.clone().into(),
                self.tool_use_id.clone().into(),
                self.subagent_session_id.clone().into(),
                self.description.clone().into(),
                self.created_at.into(),
            ],
        ))
        .await
        .context("insert subagent checkpoint in traces ref transaction")?;
        Ok(())
    }
}

/// Column values for one `scope='subagent'` `agent_checkpoint` row. These
/// public compatibility helpers are consumed by doctor and crash-replay tests;
/// the mutation itself remains owned by this checkpoint boundary.
#[derive(Debug)]
pub struct SubagentCheckpointRow<'a> {
    pub checkpoint_id: &'a str,
    pub session_id: &'a str,
    pub parent_commit: Option<&'a str>,
    pub parent_checkpoint_id: Option<&'a str>,
    pub subagent_session_id: Option<&'a str>,
    pub tool_use_id: Option<&'a str>,
    pub description: Option<&'a str>,
    pub tree_oid: &'a str,
    pub metadata_blob_oid: &'a str,
    pub traces_commit: &'a str,
    pub created_at: i64,
}

/// Idempotently insert a subagent checkpoint row after a traces commit. A
/// traces-commit probe makes a crash retry and doctor backfill safe even when
/// the checkpoint UUID differs from an earlier stale attempt.
pub async fn insert_subagent_checkpoint_row_idempotent<C: ConnectionTrait>(
    conn: &C,
    row: &SubagentCheckpointRow<'_>,
) -> anyhow::Result<bool> {
    if let Some(existing_id) =
        traces::agent_checkpoint_id_for_traces_commit(conn, row.traces_commit).await?
    {
        tracing::info!(
            checkpoint_id = %row.checkpoint_id,
            existing_checkpoint_id = %existing_id,
            "agent_checkpoint subagent row already present for traces commit; skipping INSERT"
        );
        return Ok(false);
    }
    let parent_commit_value: sea_orm::Value = row.parent_commit.map(str::to_string).into();
    let parent_checkpoint_value: sea_orm::Value =
        row.parent_checkpoint_id.map(str::to_string).into();
    let subagent_session_value: sea_orm::Value = row.subagent_session_id.map(str::to_string).into();
    let tool_use_value: sea_orm::Value = row.tool_use_id.map(str::to_string).into();
    let description_value: sea_orm::Value = row.description.map(str::to_string).into();
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, parent_checkpoint_id, scope, parent_commit,
                tree_oid, metadata_blob_oid, traces_commit, tool_use_id,
                subagent_session_id, description, created_at
             ) VALUES (?, ?, ?, 'subagent', ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                row.checkpoint_id.into(),
                row.session_id.into(),
                parent_checkpoint_value,
                parent_commit_value,
                row.tree_oid.into(),
                row.metadata_blob_oid.into(),
                row.traces_commit.into(),
                tool_use_value,
                subagent_session_value,
                description_value,
                row.created_at.into(),
            ],
        ))
        .await
        .context("failed to insert subagent agent_checkpoint row")?;
    Ok(result.rows_affected() > 0)
}

/// Column values for one committed `agent_checkpoint` row.
#[derive(Debug)]
pub struct AgentCheckpointRow<'a> {
    pub checkpoint_id: &'a str,
    pub session_id: &'a str,
    pub parent_commit: Option<&'a str>,
    pub tree_oid: &'a str,
    pub metadata_blob_oid: &'a str,
    pub traces_commit: &'a str,
    pub created_at: i64,
}

/// Stage (d) of the checkpoint write sequence, made idempotent by the
/// traces-commit probe and an `ON CONFLICT(checkpoint_id)` backstop.
pub async fn insert_agent_checkpoint_row_idempotent<C: ConnectionTrait>(
    conn: &C,
    row: &AgentCheckpointRow<'_>,
) -> anyhow::Result<bool> {
    if let Some(existing_id) =
        traces::agent_checkpoint_id_for_traces_commit(conn, row.traces_commit).await?
    {
        tracing::info!(
            checkpoint_id = %row.checkpoint_id,
            existing_checkpoint_id = %existing_id,
            "agent_checkpoint row already present for traces commit; skipping INSERT"
        );
        return Ok(false);
    }
    let parent_commit_value: sea_orm::Value = row.parent_commit.map(str::to_string).into();
    let result = conn
        .execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (
                checkpoint_id, session_id, scope, parent_commit, tree_oid,
                metadata_blob_oid, traces_commit, created_at
             ) VALUES (?, ?, 'committed', ?, ?, ?, ?, ?)
             ON CONFLICT(checkpoint_id) DO NOTHING",
            [
                row.checkpoint_id.into(),
                row.session_id.into(),
                parent_commit_value,
                row.tree_oid.into(),
                row.metadata_blob_oid.into(),
                row.traces_commit.into(),
                row.created_at.into(),
            ],
        ))
        .await
        .context("failed to insert agent_checkpoint row")?;
    Ok(result.rows_affected() > 0)
}

/// Import-only checkpoint adapter.  Historical import first binds its marker
/// in the same transaction as its identity and coverage fences, then hands
/// the opaque marker to this adapter.  Keeping that prebound lifecycle here
/// prevents import orchestration from constructing the generic traces store
/// or reproducing its marker/ref request shape.
pub(crate) struct ImportedCheckpointWriter<'a> {
    store: TracesCheckpointStore<'a>,
}

/// Typed input for one derived historical-import turn.  The constructor has
/// no marker, ref, scope, tool-use, or byte-slice field: the adapter fixes
/// those to the prebound committed import contract and accepts only the
/// redacted payload and transaction companion.
pub(crate) struct ImportedCheckpointWriteRequest<'a> {
    checkpoint_id: &'a str,
    session_id: &'a str,
    agent_kind: &'a str,
    parent_commit: Option<&'a str>,
    payload: &'a CheckpointRedactedPayload,
    txn_extra: &'a dyn TracesTxnExtra,
    deadline: CaptureCommitDeadline,
}

impl<'a> ImportedCheckpointWriteRequest<'a> {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        checkpoint_id: &'a str,
        session_id: &'a str,
        agent_kind: &'a str,
        parent_commit: Option<&'a str>,
        payload: &'a CheckpointRedactedPayload,
        txn_extra: &'a dyn TracesTxnExtra,
        deadline: CaptureCommitDeadline,
    ) -> Result<Self, CheckpointStoreError> {
        for (label, value) in [
            ("import checkpoint id", checkpoint_id),
            ("import checkpoint session id", session_id),
            ("import checkpoint agent kind", agent_kind),
        ] {
            if value.is_empty() {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: label.to_string(),
                });
            }
        }
        if Instant::now() >= deadline.monotonic() {
            return Err(CheckpointStoreError::DeadlineExceeded);
        }
        if payload.provenance != CheckpointPayloadProvenance::DerivedTurnProjection {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "historical import derived turn payload".to_string(),
            });
        }
        Ok(Self {
            checkpoint_id,
            session_id,
            agent_kind,
            parent_commit,
            payload,
            txn_extra,
            deadline,
        })
    }
}

impl<'a> ImportedCheckpointWriter<'a> {
    /// Register a marker in the import caller's existing transaction. The
    /// caller is responsible for binding its import identity and coverage
    /// claim immediately before this call; this method never starts a second
    /// transaction.
    pub(crate) async fn bind_prebound_attempt(
        txn: &DatabaseTransaction,
        session_id: &str,
        checkpoint_id: &str,
        created_at: i64,
    ) -> Result<TracesInflightMarker, CheckpointStoreError> {
        if session_id.is_empty() || checkpoint_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "prebound import checkpoint identity".to_string(),
            });
        }
        let marker = TracesInflightMarker::new(session_id, checkpoint_id, created_at);
        traces::write_traces_inflight_marker(txn, &marker)
            .await
            .map_err(|_| CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Marker,
            })?;
        Ok(marker)
    }

    /// Retire a stale, fenced marker while the import caller still holds its
    /// lease transaction. This is a checkpoint concern, not an import SQL
    /// fallback, and deliberately preserves the history layer's generation
    /// and cleanup-pending rules.
    pub(crate) async fn retire_stale_prebound_attempt(
        txn: &DatabaseTransaction,
        session_id: &str,
        checkpoint_id: &str,
    ) -> Result<(), CheckpointStoreError> {
        traces::retire_stale_traces_inflight_marker(txn, session_id, checkpoint_id)
            .await
            .map_err(|_| CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Cleanup,
            })
    }

    /// Clear an uncommitted prebound marker only under its exact generation
    /// fence. A durable cleanup-pending marker is therefore retained for
    /// doctor/GC rather than being erased by import error cleanup.
    pub(crate) async fn clear_uncommitted_prebound_attempt(
        txn: &DatabaseTransaction,
        session_id: &str,
        checkpoint_id: &str,
        marker_generation: &str,
    ) -> Result<(), CheckpointStoreError> {
        traces::clear_non_cleanup_traces_inflight_marker(
            txn,
            session_id,
            checkpoint_id,
            marker_generation,
        )
        .await
        .map_err(|_| CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::Cleanup,
        })
        .map(|_| ())
    }

    pub(crate) fn from_prebound_attempt(
        conn: &'a DatabaseConnection,
        repo_path: impl AsRef<Path>,
        marker: TracesInflightMarker,
    ) -> Result<Self, CheckpointStoreError> {
        Ok(Self {
            store: TracesCheckpointStore::from_prebound_attempt(conn, repo_path, marker)?,
        })
    }

    /// Preserve the import's workspace lease through object construction and
    /// marker replay rather than relying solely on its earlier bind
    /// transaction.
    pub(crate) fn with_capture_scope(mut self, capture_scope: CaptureScope) -> Self {
        self.store = self.store.with_capture_scope(capture_scope);
        self
    }

    /// A zero-cost type witness for adapters that need to prove they are
    /// bound to the common concrete checkpoint backend without receiving the
    /// backend value or being able to construct it themselves.
    pub(crate) fn backend_type(&self) -> std::marker::PhantomData<TracesCheckpointStore<'a>> {
        std::marker::PhantomData
    }

    pub(crate) async fn write_turn(
        &self,
        request: ImportedCheckpointWriteRequest<'_>,
    ) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
        let request = CheckpointWriteRequest::new(
            request.checkpoint_id,
            request.checkpoint_id,
            request.session_id,
            request.agent_kind,
            request.parent_commit,
            CheckpointScope::Committed,
            self.store.marker_generation(),
            None,
            request.payload,
            Some(request.txn_extra),
            Some(request.deadline),
        )?;
        self.store.write(request).await
    }
}

#[async_trait]
impl CheckpointStore for ImportedCheckpointWriter<'_> {
    async fn write(
        &self,
        request: CheckpointWriteRequest<'_>,
    ) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
        self.store.write(request).await
    }
}

/// Typed outcome of a checkpoint write attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CheckpointWriteOutcome {
    /// A new trace commit and its catalog companion were committed atomically.
    Written {
        commit_hash: String,
        marker_generation: String,
        cas_retries: u64,
        object_count: u64,
    },
    /// The same replay key was previously committed; no new object was made.
    AlreadyExists { checkpoint_id: String },
    /// The terminal receipt completed while a duplicate delivery was waiting
    /// to register its marker. No checkpoint, marker, or finalizer mutation
    /// was made by this caller; unlike `AlreadyExists`, this does not assert
    /// that the current action has a durable checkpoint row.
    TerminalReceiptAlreadyApplied,
    /// Another delivery of this replay-stable checkpoint already owns the
    /// exact persisted marker generation. No ref/object/coverage mutation
    /// was made by this caller, and terminal retry accounting must remain
    /// unchanged while that writer is live.
    AttemptInFlight {
        checkpoint_id: String,
        marker_generation: String,
    },
    /// A generation/fence/CAS conflict left the request unmodified.
    ConflictUnchanged { reason: CheckpointConflictReason },
    /// The ref commit succeeded but cleanup remains durable and repairable.
    PendingCleanup {
        checkpoint_id: String,
        marker_generation: String,
    },
}

/// Durable replay state observed before an adapter performs another source
/// read or ref mutation. `PendingCleanup` deliberately prevents a terminal
/// receipt from being completed while the trace writer marker still protects
/// a committed attempt for doctor/GC recovery.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CheckpointReplayStatus {
    Missing,
    Complete,
    PendingCleanup { marker_generation: String },
}

/// Safe conflict classes used by a coordinator/finalizer to choose replay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointConflictReason {
    StaleMarkerGeneration,
    RefCas,
    ScopeFence,
    ReplayPayloadMismatch,
}

/// A provider-neutral checkpoint persistence port.
///
/// The implementation owns marker registration/refresh/cleanup, object
/// construction, ref CAS and the supplied `TracesTxnExtra`.  A successful
/// `Written` outcome means all transactional companion writes committed with
/// the ref update; callers must not recreate a catalog row afterward.
#[async_trait]
pub trait CheckpointStore: Send + Sync {
    async fn write(
        &self,
        request: CheckpointWriteRequest<'_>,
    ) -> Result<CheckpointWriteOutcome, CheckpointStoreError>;
}

/// The production checkpoint backend for the existing `refs/libra/traces`
/// topology.
///
/// This is deliberately the one place that coordinates the durable writer
/// marker with `HistoryManager`'s object construction and ref-CAS loop.  The
/// caller supplies only typed redacted payloads and the transactional extra;
/// it never receives a trace ref name, object path, or a raw transcript
/// buffer.  The marker is created at construction time and its generation is
/// exposed solely to seal the matching [`CheckpointWriteRequest`].
pub(crate) struct TracesCheckpointStore<'a> {
    conn: &'a DatabaseConnection,
    repo_path: PathBuf,
    marker: TracesInflightMarker,
    marker_generation: String,
    registration: MarkerRegistration,
    storage_mode: CheckpointStorageMode,
    capture_scope: Option<CaptureScope>,
    /// Catalog-owned proof for a terminal receipt that elected this marker.
    /// It is revalidated inside the marker-registration write transaction;
    /// without that second check a writer paused after election could append
    /// a stale source after a later delivery quarantined the receipt.
    terminal_attempt_fence: Option<CaptureCatalogTerminalAttemptFence>,
}

/// Whether this store must register a marker or was handed a marker already
/// committed by an import identity/coverage transaction.
enum MarkerRegistration {
    Required(Vec<OwnedRegistrationFence>),
    Prebound,
}

/// Object-store behavior is part of the capture authorization boundary.
/// Live hook writes retain the repository's configured backend; historical
/// import is explicitly local-existing so it cannot create object directories
/// on the foreground path or activate a configured remote backend.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckpointStorageMode {
    ConfiguredCreate,
    LocalExisting,
}

/// Owned equivalent of [`TracesCoverageFence`].  The public traces type is a
/// borrowed view tailored to one SQL call; the store must retain the same
/// values across marker registration and the subsequent asynchronous append.
#[derive(Clone, Debug, PartialEq, Eq)]
struct OwnedRegistrationFence {
    logical_turn_key: String,
    owner: String,
    fence_token: i64,
    reservation_state: String,
}

impl OwnedRegistrationFence {
    fn borrowed(&self) -> TracesCoverageFence<'_> {
        TracesCoverageFence {
            logical_turn_key: &self.logical_turn_key,
            owner: &self.owner,
            fence_token: self.fence_token,
            reservation_state: &self.reservation_state,
        }
    }
}

impl<'a> TracesCheckpointStore<'a> {
    /// Allocate an opaque generation for the catalog's one-shot terminal
    /// attempt election. The catalog may reject this candidate and return an
    /// already-persisted generation; callers must construct the store only
    /// from that durable result.
    pub(crate) fn new_terminal_attempt_generation() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    /// Start a store-owned checkpoint attempt.  The store owns all marker
    /// registration, marker refresh, and ordinary-marker retirement; callers
    /// only pass coverage fences that must be verified during registration.
    pub(crate) fn new(
        conn: &'a DatabaseConnection,
        repo_path: impl AsRef<Path>,
        session_id: &str,
        checkpoint_id: &str,
        registration_fences: &[TracesCoverageFence<'_>],
    ) -> Result<Self, CheckpointStoreError> {
        if session_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "checkpoint session id".to_string(),
            });
        }
        if checkpoint_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "checkpoint id".to_string(),
            });
        }
        let marker =
            TracesInflightMarker::new(session_id, checkpoint_id, Utc::now().timestamp_millis());
        // `TracesInflightMarker::new` always creates this UUID.  Store it
        // independently so no later marker mutation can accidentally change
        // the request/append fence.
        let marker_generation =
            marker
                .generation
                .clone()
                .ok_or(CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::Marker,
                })?;
        Ok(Self {
            conn,
            repo_path: repo_path.as_ref().to_path_buf(),
            marker,
            marker_generation,
            registration: MarkerRegistration::Required(
                registration_fences
                    .iter()
                    .map(|fence| OwnedRegistrationFence {
                        logical_turn_key: fence.logical_turn_key.to_string(),
                        owner: fence.owner.to_string(),
                        fence_token: fence.fence_token,
                        reservation_state: fence.reservation_state.to_string(),
                    })
                    .collect(),
            ),
            storage_mode: CheckpointStorageMode::ConfiguredCreate,
            capture_scope: None,
            terminal_attempt_fence: None,
        })
    }

    /// Recreate a store for the exact terminal attempt already persisted by
    /// the catalog. The generation is never caller-selected on the live
    /// path: it is read from the pending receipt after its revision fence was
    /// verified. Reusing it lets a pre-CAS retry continue the same attempt
    /// without overwriting a concurrent writer's marker.
    pub(crate) fn new_with_persisted_marker_generation(
        conn: &'a DatabaseConnection,
        repo_path: impl AsRef<Path>,
        session_id: &str,
        checkpoint_id: &str,
        marker_generation: &str,
        registration_fences: &[TracesCoverageFence<'_>],
    ) -> Result<Self, CheckpointStoreError> {
        uuid::Uuid::parse_str(marker_generation).map_err(|_| {
            CheckpointStoreError::InvalidRequest {
                field: "persisted checkpoint marker generation".to_string(),
            }
        })?;
        let mut store = Self::new(
            conn,
            repo_path,
            session_id,
            checkpoint_id,
            registration_fences,
        )?;
        store.marker.generation = Some(marker_generation.to_string());
        store.marker_generation = marker_generation.to_string();
        Ok(store)
    }

    /// A pending-artifact replay must stay local and must register the native
    /// PK/generation sealed by the catalog. This constructor neither loads
    /// configured storage/global config nor consumes registration authority.
    pub(crate) fn from_terminal_recovery(
        conn: &'a DatabaseConnection,
        repo_path: impl AsRef<Path>,
        registration_fences: &[TracesCoverageFence<'_>],
        terminal_attempt_fence: CaptureCatalogTerminalAttemptFence,
    ) -> Result<Self, CheckpointStoreError> {
        let (scope, session_id, checkpoint_id, generation) = terminal_attempt_fence
            .recovery_checkpoint_binding()
            .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                field: "terminal recovery catalog identity".to_string(),
            })?;
        let mut store = Self::new_with_persisted_marker_generation(
            conn,
            repo_path,
            session_id,
            &checkpoint_id,
            generation,
            registration_fences,
        )?;
        store.storage_mode = CheckpointStorageMode::LocalExisting;
        store.capture_scope = Some(scope.clone());
        store.terminal_attempt_fence = Some(terminal_attempt_fence);
        Ok(store)
    }

    /// Adopt an import attempt whose marker was persisted in the same
    /// transaction as its import-identity and coverage fences. The store does
    /// not re-register that marker (which would split the fence proof into a
    /// second transaction), but retains ownership of append, marker refresh,
    /// and conditional retirement.
    pub(crate) fn from_prebound_attempt(
        conn: &'a DatabaseConnection,
        repo_path: impl AsRef<Path>,
        marker: TracesInflightMarker,
    ) -> Result<Self, CheckpointStoreError> {
        if marker.session_id.is_empty() || marker.attempt_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "prebound checkpoint marker identity".to_string(),
            });
        }
        let marker_generation =
            marker
                .generation
                .clone()
                .ok_or(CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::Marker,
                })?;
        Ok(Self {
            conn,
            repo_path: repo_path.as_ref().to_path_buf(),
            marker,
            marker_generation,
            registration: MarkerRegistration::Prebound,
            storage_mode: CheckpointStorageMode::LocalExisting,
            capture_scope: None,
            terminal_attempt_fence: None,
        })
    }

    /// Carry the resolved ingress scope through marker registration and the
    /// concrete checkpoint write. A matching check must also live in the
    /// ref-CAS transaction companion because object construction can outlast
    /// this preflight.
    pub(crate) fn with_capture_scope(mut self, capture_scope: CaptureScope) -> Self {
        self.capture_scope = Some(capture_scope);
        self
    }

    /// Bind this store to the one terminal marker/source pair elected by the
    /// catalog.  This opaque proof is intentionally consumed only by the
    /// traces registration transaction; adapters cannot inspect or recreate
    /// it from provider input.
    pub(crate) fn with_terminal_attempt_fence(
        mut self,
        terminal_attempt_fence: CaptureCatalogTerminalAttemptFence,
    ) -> Self {
        self.terminal_attempt_fence = Some(terminal_attempt_fence);
        self
    }

    /// Generation that seals the request to this exact store-owned attempt.
    /// It is an opaque fence, not a durable caller-selected identity.
    pub(crate) fn marker_generation(&self) -> &str {
        &self.marker_generation
    }

    fn request_matches_attempt(
        &self,
        request: &CheckpointWriteRequest<'_>,
    ) -> Result<(), CheckpointWriteOutcome> {
        if let Some(fence) = &self.terminal_attempt_fence
            && (request.scope() != CheckpointScope::Committed
                || !fence.matches_checkpoint_attempt(
                    self.capture_scope.as_ref(),
                    &self.marker.session_id,
                    &self.marker.attempt_id,
                    &self.marker_generation,
                ))
        {
            return Err(CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ScopeFence,
            });
        }
        if request.session_id() != self.marker.session_id {
            return Err(CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ScopeFence,
            });
        }
        if request.checkpoint_id() != self.marker.attempt_id
            || request.marker_generation() != self.marker_generation
        {
            return Err(CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::StaleMarkerGeneration,
            });
        }
        Ok(())
    }

    /// Probe a replay-stable checkpoint identity before a caller performs
    /// expensive source preparation.  This is intentionally a store API: a
    /// hook/import adapter must not reimplement direct `agent_checkpoint` SQL
    /// merely to avoid a duplicate ref append.
    pub(crate) async fn durable_replay_exists_for(
        conn: &DatabaseConnection,
        checkpoint_id: &str,
        session_id: &str,
        scope: CheckpointScope,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<bool, CheckpointStoreError> {
        let row = await_checkpoint_precommit_read_until(deadline, async {
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT session_id, scope FROM agent_checkpoint WHERE checkpoint_id = ? LIMIT 1",
                [checkpoint_id.into()],
            ))
            .await
            .map_err(|_| CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::CatalogTransaction,
            })
        })
        .await?;
        let Some(row) = row else {
            return Ok(false);
        };
        let stored_session: String =
            row.try_get_by("session_id")
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::CatalogTransaction,
                })?;
        let stored_scope: String =
            row.try_get_by("scope")
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::CatalogTransaction,
                })?;
        if stored_session != session_id || stored_scope != scope.as_str() {
            return Err(CheckpointStoreError::ReplayPayloadMismatch);
        }
        Ok(true)
    }

    /// Resolve a durable replay and settle any ordinary marker left by a
    /// post-CAS cleanup failure. A `cleanup_pending` marker is never erased
    /// here: it names repairable objects and must be handled by doctor/GC.
    pub(crate) async fn replay_status_for(
        conn: &DatabaseConnection,
        capture_scope: Option<&CaptureScope>,
        checkpoint_id: &str,
        session_id: &str,
        scope: CheckpointScope,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<CheckpointReplayStatus, CheckpointStoreError> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
            return Err(CheckpointStoreError::DeadlineExceeded);
        }
        if !Self::durable_replay_exists_for(conn, checkpoint_id, session_id, scope, deadline)
            .await?
        {
            return Ok(CheckpointReplayStatus::Missing);
        }
        let entry = await_checkpoint_precommit_read_until(deadline, async {
            crate::internal::metadata::MetadataKv::get_with_conn(
                conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                session_id,
                checkpoint_id,
            )
            .await
            .map_err(|_| CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Cleanup,
            })
        })
        .await?;
        let Some(entry) = entry else {
            return Ok(CheckpointReplayStatus::Complete);
        };
        let marker = traces::decode_and_validate_traces_inflight_marker(
            &entry.value,
            session_id,
            checkpoint_id,
        )
        .map_err(|_| CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::Cleanup,
        })?;
        let Some(marker_generation) = marker.generation.clone() else {
            return Err(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Cleanup,
            });
        };
        if marker.cleanup_pending {
            return Ok(CheckpointReplayStatus::PendingCleanup { marker_generation });
        }
        let cleared = match deadline {
            Some(deadline) => {
                traces::clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
                    conn,
                    capture_scope,
                    session_id,
                    checkpoint_id,
                    &marker_generation,
                    deadline,
                )
                .await
            }
            None => {
                traces::clear_non_cleanup_traces_inflight_marker_with_capture_scope(
                    conn,
                    capture_scope,
                    session_id,
                    checkpoint_id,
                    &marker_generation,
                )
                .await
            }
        };
        match cleared {
            Ok(true) => Ok(CheckpointReplayStatus::Complete),
            Ok(false) => {
                tracing::warn!(
                    checkpoint_id = %checkpoint_id,
                    stage = "marker_cleanup",
                    "durable checkpoint replay could not conditionally clear its ordinary marker"
                );
                Ok(CheckpointReplayStatus::PendingCleanup { marker_generation })
            }
            Err(_) if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) => {
                Err(CheckpointStoreError::DeadlineExceeded)
            }
            Err(_) => {
                tracing::warn!(
                    checkpoint_id = %checkpoint_id,
                    stage = "marker_cleanup",
                    "durable checkpoint replay failed to clear its ordinary marker"
                );
                Ok(CheckpointReplayStatus::PendingCleanup { marker_generation })
            }
        }
    }

    /// Settle one lifecycle action's durable replay before an adapter
    /// completes its receipt *without* a checkpoint write, e.g. a coverage
    /// no-op whose turns this action's own earlier delivery already
    /// committed. This is the replay authority [`CheckpointStore::write`]
    /// consults before registering a marker, so a failure outcome resolves
    /// identically whether or not the replay reaches the store's write:
    ///
    /// - no durable committed row for `checkpoint_id` → `Missing`; the marker
    ///   slot is neither read nor touched (it may belong to a live writer);
    /// - a durable row → its leftover ordinary marker is retired under the
    ///   exact persisted generation and the workspace fence → `Complete`;
    /// - a `cleanup_pending` marker, or an unsuccessful conditional
    ///   retirement → `PendingCleanup`; repairable objects stay owned by
    ///   doctor/GC and the receipt must not be completed.
    ///
    /// Only the `(session_id, checkpoint_id)` marker slot is addressed, so no
    /// other action's marker can be observed or retired.
    pub(crate) async fn settle_committed_replay_without_write(
        conn: &DatabaseConnection,
        capture_scope: &CaptureScope,
        checkpoint_id: &str,
        session_id: &str,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<CheckpointReplayStatus, CheckpointStoreError> {
        if session_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "checkpoint session id".to_string(),
            });
        }
        if checkpoint_id.is_empty() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "checkpoint id".to_string(),
            });
        }
        // Same preflight as `write`: an expired workspace lease must not
        // even classify (let alone retire) recovery evidence.
        await_checkpoint_precommit_read_until(deadline, async {
            capture_scope
                .assert_workspace_fence_live(conn)
                .await
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::ScopeFence,
                })
        })
        .await?;
        Self::replay_status_for(
            conn,
            Some(capture_scope),
            checkpoint_id,
            session_id,
            CheckpointScope::Committed,
            deadline,
        )
        .await
    }

    async fn retire_ordinary_marker(
        &self,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<bool, CheckpointStoreError> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
            return Err(CheckpointStoreError::DeadlineExceeded);
        }
        #[cfg(test)]
        if test_support::take_fault(test_support::CheckpointFaultPoint::MarkerCleanup) {
            return Err(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Cleanup,
            });
        }
        let cleared = match deadline {
            Some(deadline) => {
                traces::clear_non_cleanup_traces_inflight_marker_with_capture_scope_until(
                    self.conn,
                    self.capture_scope.as_ref(),
                    &self.marker.session_id,
                    &self.marker.attempt_id,
                    &self.marker_generation,
                    deadline,
                )
                .await
            }
            None => {
                traces::clear_non_cleanup_traces_inflight_marker_with_capture_scope(
                    self.conn,
                    self.capture_scope.as_ref(),
                    &self.marker.session_id,
                    &self.marker.attempt_id,
                    &self.marker_generation,
                )
                .await
            }
        };
        cleared.map_err(|_| {
            if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
                CheckpointStoreError::DeadlineExceeded
            } else {
                CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::Cleanup,
                }
            }
        })
    }

    /// Preserve the primary failure while making an unsuccessful conditional
    /// cleanup observable. A committed write reports `PendingCleanup`
    /// instead; these callers failed before they could truthfully claim a
    /// durable checkpoint outcome.
    async fn retire_failed_attempt_with_diagnostic(
        &self,
        reason: &'static str,
        deadline: Option<CaptureCommitDeadline>,
    ) {
        match self.retire_ordinary_marker(deadline).await {
            Ok(true) => {}
            Ok(false) => tracing::warn!(
                checkpoint_id = %self.marker.attempt_id,
                stage = "marker_cleanup",
                reason,
                "capture checkpoint failure left an ordinary marker for recovery"
            ),
            Err(error) => tracing::warn!(
                checkpoint_id = %self.marker.attempt_id,
                stage = "marker_cleanup",
                reason,
                error = %error,
                "capture checkpoint failure could not retire its marker"
            ),
        }
    }

    /// Refresh post-CAS recovery evidence under a fresh writer transaction.
    /// The workspace fence is checked in that transaction, so a lease that
    /// expires after the ref/catalog CAS cannot let the old hook overwrite a
    /// marker owned by a newer lease holder. The original marker remains
    /// durable recovery evidence if this short best-effort refresh cannot
    /// obtain the writer.
    async fn refresh_committed_marker(
        &self,
        marker: &TracesInflightMarker,
        expected_generation: &str,
    ) -> Result<bool, CheckpointStoreError> {
        // This is deliberately a new, short recovery-only deadline rather
        // than the already-spent capture budget. It bounds writer acquisition
        // and the fence read while the original marker remains evidence if
        // refresh cannot begin its mutation in time.
        let recovery_deadline = CaptureCommitDeadline::from_budget(POST_CAS_MARKER_REFRESH_GRACE)
            .map_err(|_| CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::Marker,
        })?;
        let txn = tokio::time::timeout_at(
            tokio::time::Instant::from_std(recovery_deadline.monotonic()),
            crate::internal::db::begin_write_transaction(self.conn),
        )
        .await
        .map_err(|_| CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::Marker,
        })?
        .map_err(|_| CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::Marker,
        })?;
        let updated =
            match traces::update_traces_inflight_marker_if_generation_with_capture_scope_until(
                &txn,
                self.capture_scope.as_ref(),
                marker,
                expected_generation,
                recovery_deadline,
            )
            .await
            {
                Ok(updated) => updated,
                Err(_) => {
                    txn.rollback().await.ok();
                    return Err(CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::Marker,
                    });
                }
            };
        txn.commit()
            .await
            .map_err(|_| CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Marker,
            })?;
        Ok(updated)
    }

    fn append_params<'request>(
        &'request self,
        request: &'request CheckpointWriteRequest<'request>,
    ) -> CheckpointCommitParams<'request> {
        CheckpointCommitParams {
            reasoning_artifacts: &[],
            checkpoint_id: request.checkpoint_id(),
            session_id: request.session_id(),
            marker_generation: &self.marker_generation,
            capture_scope: self.capture_scope.as_ref(),
            agent_kind: request.agent_kind(),
            parent_commit: request.parent_commit(),
            scope: request.scope(),
            tool_use_id: request.tool_use_id(),
            metadata_json: request.payload().metadata_json(),
            transcript_redacted: request.payload().transcript(),
            lifecycle_events_jsonl: request.payload().lifecycle_events_jsonl(),
            redaction_report_json: request.payload().redaction_report_json(),
            txn_extra: request.txn_extra(),
            deadline: request.deadline(),
        }
    }

    /// Classify a failed append from its typed causes alone. Display text is
    /// deliberately ignored: deadline and SQLite errors may carry context
    /// that mentions a marker generation without being a fence conflict.
    fn classify_append_error(error: &anyhow::Error) -> AppendFailureClass {
        if let Some(conflict) = typed_append_cause::<CheckpointAppendConflict>(error) {
            return AppendFailureClass::Conflict(match conflict {
                CheckpointAppendConflict::RefCasExhausted => CheckpointConflictReason::RefCas,
                CheckpointAppendConflict::MarkerFenced(_) => {
                    CheckpointConflictReason::StaleMarkerGeneration
                }
            });
        }
        if typed_append_cause::<CheckpointCompanionTransactionFailed>(error).is_some() {
            return AppendFailureClass::Store(CheckpointStoreStage::CatalogTransaction);
        }
        AppendFailureClass::Store(CheckpointStoreStage::ObjectWrite)
    }
}

/// Content-free classification of a `HistoryManager` append failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum AppendFailureClass {
    /// The ref was left unchanged because another writer or recovery won.
    Conflict(CheckpointConflictReason),
    /// The append failed at this stage without a typed conflict.
    Store(CheckpointStoreStage),
}

/// Find a typed cause whether it is the root error, an `anyhow` context
/// layer (`Error::context` wraps it in a private type that `chain()` alone
/// cannot downcast), or a `source()` link below a foreign error type.
fn typed_append_cause<T>(error: &anyhow::Error) -> Option<&T>
where
    T: std::error::Error + Send + Sync + 'static,
{
    error
        .downcast_ref::<T>()
        .or_else(|| error.chain().find_map(|cause| cause.downcast_ref::<T>()))
}

#[async_trait]
impl CheckpointStore for TracesCheckpointStore<'_> {
    async fn write(
        &self,
        request: CheckpointWriteRequest<'_>,
    ) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
        if request
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline.monotonic())
        {
            return Err(CheckpointStoreError::DeadlineExceeded);
        }
        if let Err(outcome) = self.request_matches_attempt(&request) {
            return Ok(outcome);
        }
        if let Some(scope) = &self.capture_scope {
            await_checkpoint_precommit_read_until(request.deadline(), async {
                scope
                    .assert_workspace_fence_live(self.conn)
                    .await
                    .map_err(|_| CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::ScopeFence,
                    })
            })
            .await?;
        }
        let replay_status = Self::replay_status_for(
            self.conn,
            self.capture_scope.as_ref(),
            request.checkpoint_id(),
            request.session_id(),
            request.scope(),
            request.deadline(),
        )
        .await;
        if matches!(
            replay_status,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        ) {
            return Ok(CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ReplayPayloadMismatch,
            });
        }
        match replay_status? {
            CheckpointReplayStatus::Complete => {
                return Ok(CheckpointWriteOutcome::AlreadyExists {
                    checkpoint_id: request.checkpoint_id().to_string(),
                });
            }
            CheckpointReplayStatus::PendingCleanup { marker_generation } => {
                return Ok(CheckpointWriteOutcome::PendingCleanup {
                    checkpoint_id: request.checkpoint_id().to_string(),
                    marker_generation,
                });
            }
            CheckpointReplayStatus::Missing => {}
        }

        #[cfg(test)]
        test_support::pause_before_registration_if_armed(&self.repo_path).await;

        if let MarkerRegistration::Required(fences) = &self.registration {
            #[cfg(test)]
            if test_support::take_fault(test_support::CheckpointFaultPoint::MarkerRegistration) {
                return Err(CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::Marker,
                });
            }
            let registration_fences = fences
                .iter()
                .map(OwnedRegistrationFence::borrowed)
                .collect::<Vec<_>>();
            let registration = match request.deadline() {
                Some(deadline) => {
                    traces::register_traces_write_attempt_with_capture_scope_until(
                        self.conn,
                        self.capture_scope.as_ref(),
                        &self.marker,
                        &registration_fences,
                        Some(request.scope()),
                        self.terminal_attempt_fence.as_ref(),
                        deadline,
                    )
                    .await
                }
                None => {
                    traces::register_traces_write_attempt_with_capture_scope(
                        self.conn,
                        self.capture_scope.as_ref(),
                        &self.marker,
                        &registration_fences,
                        Some(request.scope()),
                        self.terminal_attempt_fence.as_ref(),
                    )
                    .await
                }
            };
            match registration.map_err(|_| {
                if request
                    .deadline()
                    .is_some_and(|deadline| Instant::now() >= deadline.monotonic())
                {
                    CheckpointStoreError::DeadlineExceeded
                } else {
                    CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::Marker,
                    }
                }
            })? {
                traces::TracesWriteAttemptRegistration::Registered => {}
                traces::TracesWriteAttemptRegistration::AlreadyInFlightSameGeneration => {
                    return Ok(CheckpointWriteOutcome::AttemptInFlight {
                        checkpoint_id: request.checkpoint_id().to_string(),
                        marker_generation: self.marker_generation.clone(),
                    });
                }
                traces::TracesWriteAttemptRegistration::AlreadyCommitted => {
                    return Ok(CheckpointWriteOutcome::AlreadyExists {
                        checkpoint_id: request.checkpoint_id().to_string(),
                    });
                }
                traces::TracesWriteAttemptRegistration::TerminalReceiptAlreadyComplete => {
                    return Ok(CheckpointWriteOutcome::TerminalReceiptAlreadyApplied);
                }
            }
        }

        #[cfg(test)]
        if test_support::take_fault(test_support::CheckpointFaultPoint::AfterMarkerRegistration) {
            self.retire_failed_attempt_with_diagnostic(
                "injected_post_registration_failure",
                request.deadline(),
            )
            .await;
            return Err(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Marker,
            });
        }

        #[cfg(test)]
        test_support::pause_after_registration_if_armed(&self.repo_path).await;

        #[cfg(test)]
        test_support::replace_marker_with_expired_takeover_if_armed(self.conn, &self.marker).await;

        let objects_dir = self.repo_path.join("objects");
        let storage = match self.storage_mode {
            CheckpointStorageMode::ConfiguredCreate => {
                if std::fs::create_dir_all(&objects_dir).is_err() {
                    // A registered marker is durable recovery evidence.
                    // Retire it only when it is still an ordinary attempt;
                    // a concurrent writer or HistoryManager cleanup may have
                    // promoted it to `cleanup_pending`, which must remain
                    // for doctor/GC.
                    self.retire_failed_attempt_with_diagnostic(
                        "objects_directory_create",
                        request.deadline(),
                    )
                    .await;
                    return Err(CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::ObjectWrite,
                    });
                }
                std::sync::Arc::new(crate::utils::client_storage::ClientStorage::init(
                    objects_dir,
                ))
            }
            CheckpointStorageMode::LocalExisting => std::sync::Arc::new(
                crate::utils::client_storage::ClientStorage::init_local_existing(objects_dir),
            ),
        };
        let manager = crate::internal::ai::history::HistoryManager::for_traces(
            storage,
            self.repo_path.clone(),
            std::sync::Arc::new(self.conn.clone()),
        );
        #[cfg(test)]
        let manager = test_support::with_history_fault(manager);
        #[cfg(test)]
        let companion_fault = test_support::companion_fault(request.txn_extra());
        let params = self.append_params(&request);
        #[cfg(test)]
        let params = test_support::with_companion_fault(params, companion_fault.as_ref());
        let written = match manager.append_checkpoint_commit(params).await {
            Ok(written) => written,
            Err(error) => {
                // `append_checkpoint_commit` either upgrades failed-object
                // ownership to `cleanup_pending` or reports a typed deferred
                // cleanup while retaining the original marker. Ordinary
                // retirement must not erase either form of recovery evidence.
                // The deferred cause is normally an `anyhow` context layer,
                // which only a typed downcast (not `chain().is`) can see.
                if typed_append_cause::<
                    crate::internal::ai::traces::RejectedCheckpointCleanupDeferred,
                >(&error)
                .is_some()
                {
                    tracing::warn!(
                        checkpoint_id = %request.checkpoint_id(),
                        stage = "marker_cleanup",
                        "rejected checkpoint cleanup was deferred; retaining the original marker as recovery evidence"
                    );
                } else {
                    self.retire_failed_attempt_with_diagnostic(
                        "append_failure",
                        request.deadline(),
                    )
                    .await;
                }
                let stage = match Self::classify_append_error(&error) {
                    AppendFailureClass::Conflict(reason) => {
                        return Ok(CheckpointWriteOutcome::ConflictUnchanged { reason });
                    }
                    AppendFailureClass::Store(stage) => stage,
                };
                if request
                    .deadline()
                    .is_some_and(|deadline| Instant::now() >= deadline.monotonic())
                {
                    return Err(CheckpointStoreError::DeadlineExceeded);
                }
                return Err(CheckpointStoreError::StoreFailure { stage });
            }
        };

        let mut committed_marker = self.marker.clone();
        committed_marker.commit = Some(written.commit_hash.to_string());
        committed_marker.oids = vec![
            written.tree_oid.to_string(),
            written.metadata_blob_oid.to_string(),
        ];
        let marker_refreshed = match self
            .refresh_committed_marker(&committed_marker, &written.marker_generation)
            .await
        {
            Ok(true) => true,
            Ok(false) => {
                tracing::warn!(
                    checkpoint_id = %request.checkpoint_id(),
                    stage = "marker_refresh",
                    "capture checkpoint marker was replaced before durable cleanup"
                );
                false
            }
            Err(_) => {
                tracing::warn!(
                    checkpoint_id = %request.checkpoint_id(),
                    stage = "marker_refresh",
                    "capture checkpoint marker refresh failed; leaving recovery evidence"
                );
                false
            }
        };
        let marker_cleared = if marker_refreshed {
            match self.retire_ordinary_marker(request.deadline()).await {
                Ok(true) => true,
                Ok(false) => {
                    tracing::warn!(
                        checkpoint_id = %request.checkpoint_id(),
                        stage = "marker_cleanup",
                        "capture checkpoint marker remains as durable cleanup evidence"
                    );
                    false
                }
                Err(_) => {
                    tracing::warn!(
                        checkpoint_id = %request.checkpoint_id(),
                        stage = "marker_cleanup",
                        "capture checkpoint marker cleanup failed; leaving recovery evidence"
                    );
                    false
                }
            }
        } else {
            false
        };
        if !marker_cleared {
            // The ref CAS and `TracesTxnExtra` have already committed.  Keep
            // the marker as durable recovery evidence rather than falsely
            // reporting a clean finalization to the coordinator.
            return Ok(CheckpointWriteOutcome::PendingCleanup {
                checkpoint_id: request.checkpoint_id().to_string(),
                marker_generation: written.marker_generation,
            });
        }
        Ok(CheckpointWriteOutcome::Written {
            commit_hash: written.commit_hash.to_string(),
            marker_generation: written.marker_generation,
            cas_retries: written.cas_retries,
            object_count: written.object_count,
        })
    }
}

/// Fail-closed errors exposed by the checkpoint port.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum CheckpointStoreError {
    #[error("capture checkpoint request has an invalid {field}")]
    InvalidRequest { field: String },
    #[error("capture snapshot is incomplete ({reason}); checkpoint was not written")]
    IncompleteSnapshot { reason: String },
    #[error("capture checkpoint deadline elapsed before the write began")]
    DeadlineExceeded,
    #[error("capture checkpoint replay key is bound to a different durable payload")]
    ReplayPayloadMismatch,
    #[error("capture checkpoint store failed at {stage}")]
    StoreFailure { stage: CheckpointStoreStage },
}

/// Content-free stage label for checkpoint errors and finalizer receipts.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CheckpointStoreStage {
    ScopeFence,
    Marker,
    ObjectWrite,
    CatalogTransaction,
    Cleanup,
}

impl std::fmt::Display for CheckpointStoreStage {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let stage = match self {
            Self::ScopeFence => "scope_fence",
            Self::Marker => "marker",
            Self::ObjectWrite => "object_write",
            Self::CatalogTransaction => "catalog_transaction",
            Self::Cleanup => "cleanup",
        };
        formatter.write_str(stage)
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use sea_orm::{ConnectionTrait, Database, Statement};

    use super::*;
    use crate::internal::ai::{
        capture::snapshot::CaptureSnapshotService,
        observed_agents::{ExportAuthorized, TranscriptSource},
    };

    fn snapshot() -> CaptureSnapshot {
        let bytes = b"checkpoint secret AKIAABCDEFGHIJKLMNOP".to_vec();
        let auth = ExportAuthorized::issue("agent", "session", &bytes);
        CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes { bytes, auth },
            "agent",
            "session",
            Default::default(),
        )
    }

    fn sidecar(value: &[u8]) -> RedactedBytes {
        crate::internal::ai::observed_agents::Redactor::new_default()
            .redact(value)
            .0
    }

    /// A deliberately small fixture for the marker lifecycle.  The audit
    /// trigger distinguishes "no marker remains" from the stronger contract
    /// that an elapsed deadline never publishes a marker at all.
    async fn marker_lifecycle_fixture() -> DatabaseConnection {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open checkpoint marker lifecycle fixture");
        let backend = conn.get_database_backend();
        for statement in [
            "CREATE TABLE config_kv (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                encrypted INTEGER NOT NULL DEFAULT 0
            )",
            "CREATE TABLE agent_session (
                session_id TEXT PRIMARY KEY,
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL
            )",
            "CREATE TABLE agent_import_tombstone (
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL
            )",
            "CREATE TABLE agent_checkpoint (
                checkpoint_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                scope TEXT NOT NULL
            )",
            "CREATE TABLE agent_coverage_claim (id INTEGER PRIMARY KEY)",
            "CREATE TABLE reference (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                name TEXT,
                kind TEXT NOT NULL,
                \"commit\" TEXT,
                remote TEXT
            )",
            "CREATE TABLE checkpoint_marker_write_audit (
                id INTEGER PRIMARY KEY AUTOINCREMENT
            )",
        ] {
            conn.execute_raw(Statement::from_string(backend, statement.to_string()))
                .await
                .expect("create checkpoint marker lifecycle fixture table");
        }
        conn.execute_raw(Statement::from_string(
            backend,
            include_str!("../../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
        ))
        .await
        .expect("create checkpoint marker registry");
        for statement in [
            "CREATE TRIGGER checkpoint_marker_write_audit_insert
             AFTER INSERT ON metadata_kv
             WHEN NEW.scope = 'agent_traces_inflight'
             BEGIN
                 INSERT INTO checkpoint_marker_write_audit (id) VALUES (NULL);
             END",
            "CREATE TRIGGER checkpoint_marker_write_audit_update
             AFTER UPDATE OF value ON metadata_kv
             WHEN NEW.scope = 'agent_traces_inflight'
             BEGIN
                 INSERT INTO checkpoint_marker_write_audit (id) VALUES (NULL);
             END",
        ] {
            conn.execute_raw(Statement::from_string(backend, statement.to_string()))
                .await
                .expect("install checkpoint marker write audit trigger");
        }
        conn.execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id)
             VALUES ('deadline-session', 'claude_code', 'deadline-provider')"
                .to_string(),
        ))
        .await
        .expect("seed checkpoint marker lifecycle session");
        conn
    }

    async fn marker_exists(conn: &DatabaseConnection, checkpoint_id: &str) -> bool {
        crate::internal::metadata::MetadataKv::get_with_conn(
            conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            "deadline-session",
            checkpoint_id,
        )
        .await
        .expect("read checkpoint marker")
        .is_some()
    }

    async fn fixture_row_count(conn: &DatabaseConnection, sql: &'static str) -> i64 {
        conn.query_one_raw(Statement::from_string(
            conn.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("count checkpoint marker lifecycle fixture rows")
        .expect("checkpoint marker lifecycle fixture count row")
        .try_get_by("n")
        .expect("decode checkpoint marker lifecycle fixture count")
    }

    #[tokio::test]
    async fn registration_pause_is_repo_scoped_and_guard_drop_releases_writer() {
        let owner_repo = tempfile::tempdir().expect("create pause owner repository");
        let unrelated_repo = tempfile::tempdir().expect("create unrelated repository");
        let pause = test_support::pause_before_registration_once(owner_repo.path());

        tokio::time::timeout(
            Duration::from_secs(2),
            test_support::pause_before_registration_if_armed(unrelated_repo.path()),
        )
        .await
        .expect("an unrelated repository must not consume or wait on the armed pause");

        let owner_path = owner_repo.path().to_path_buf();
        let writer = tokio::spawn(async move {
            test_support::pause_before_registration_if_armed(&owner_path).await;
        });
        tokio::time::timeout(Duration::from_secs(2), pause.wait_until_entered())
            .await
            .expect("owner writer must consume its matching pause");

        // A failing test drops its guard during unwind. That must both remove
        // an unconsumed slot and unblock a writer that already consumed it.
        drop(pause);
        tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .expect("dropping the pause guard must release a paused writer")
            .expect("join paused writer");
    }

    #[tokio::test]
    async fn post_cas_marker_refresh_lock_contention_is_bounded_and_retains_original_marker() {
        let repo = tempfile::tempdir().expect("create marker refresh repository");
        let database_path = repo.path().join("libra.db");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("marker refresh database path is utf-8"),
        )
        .await
        .expect("create marker refresh database");
        let marker = TracesInflightMarker::new(
            "marker-refresh-session",
            "marker-refresh-checkpoint",
            Utc::now().timestamp_millis(),
        );
        traces::write_traces_inflight_marker(&conn, &marker)
            .await
            .expect("seed original in-flight marker");
        let store =
            TracesCheckpointStore::from_prebound_attempt(&conn, repo.path(), marker.clone())
                .expect("create prebound marker refresh store");
        let expected_generation = marker
            .generation
            .clone()
            .expect("fresh marker carries a generation");
        let mut committed_marker = marker.clone();
        committed_marker.commit = Some("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".to_string());
        committed_marker.oids = vec!["e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".to_string()];

        let locker = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("marker refresh database path is utf-8"),
            Duration::from_millis(50),
        )
        .await
        .expect("open marker refresh lock holder");
        let lock = crate::internal::db::begin_write_transaction(&locker)
            .await
            .expect("acquire marker refresh writer lock");

        let started = Instant::now();
        let refresh = tokio::time::timeout(
            Duration::from_secs(2),
            store.refresh_committed_marker(&committed_marker, &expected_generation),
        )
        .await
        .expect("post-CAS marker refresh must use its short recovery grace");
        lock.rollback()
            .await
            .expect("release marker refresh writer lock");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "writer contention must not outlive the short recovery grace"
        );
        // Releasing the separate connection cannot resume a cancelled writer
        // acquisition and publish a marker after the recovery deadline.
        tokio::time::sleep(Duration::from_millis(100)).await;

        assert!(
            matches!(
                refresh,
                Err(CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::Marker
                })
            ),
            "lock contention must leave post-CAS marker refresh as recovery work: {refresh:?}"
        );
        let entry = crate::internal::metadata::MetadataKv::get_with_conn(
            &conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            "marker-refresh-session",
            "marker-refresh-checkpoint",
        )
        .await
        .expect("read original marker after bounded refresh")
        .expect("original marker must remain as recovery evidence");
        let persisted = traces::decode_and_validate_traces_inflight_marker(
            &entry.value,
            "marker-refresh-session",
            "marker-refresh-checkpoint",
        )
        .expect("decode original marker after bounded refresh");
        assert_eq!(
            persisted, marker,
            "a timed-out recovery refresh must not replace the existing marker evidence"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn post_cas_marker_refresh_bounds_delayed_fence_read_without_late_marker_update() {
        let repo = tempfile::tempdir().expect("create delayed-fence marker refresh repository");
        let database_path = repo.path().join("libra.db");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("delayed-fence marker refresh database path is utf-8"),
        )
        .await
        .expect("create delayed-fence marker refresh database");
        let backend = conn.get_database_backend();
        let scope = CaptureScope {
            repo_id: "marker-refresh-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("marker-refresh-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/marker-refresh-workspace', 'agent',
                       'active', 'marker-refresh-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed delayed-fence marker refresh workspace lease");

        let marker = TracesInflightMarker::new(
            "marker-refresh-fence-session",
            "marker-refresh-fence-checkpoint",
            Utc::now().timestamp_millis(),
        );
        traces::write_traces_inflight_marker(&conn, &marker)
            .await
            .expect("seed delayed-fence original in-flight marker");
        conn.execute_raw(Statement::from_string(
            backend,
            "CREATE TABLE checkpoint_marker_refresh_audit (
                id INTEGER PRIMARY KEY AUTOINCREMENT
            )"
            .to_string(),
        ))
        .await
        .expect("create delayed-fence marker refresh audit table");
        conn.execute_raw(Statement::from_string(
            backend,
            "CREATE TRIGGER checkpoint_marker_refresh_audit_update
             AFTER UPDATE OF value ON metadata_kv
             WHEN NEW.scope = 'agent_traces_inflight'
             BEGIN
                 INSERT INTO checkpoint_marker_refresh_audit (id) VALUES (NULL);
             END"
            .to_string(),
        ))
        .await
        .expect("install delayed-fence marker refresh audit trigger");

        let store =
            TracesCheckpointStore::from_prebound_attempt(&conn, repo.path(), marker.clone())
                .expect("create delayed-fence prebound marker refresh store")
                .with_capture_scope(scope);
        let expected_generation = marker
            .generation
            .clone()
            .expect("fresh marker carries a generation");
        let mut committed_marker = marker.clone();
        committed_marker.commit = Some("e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".to_string());
        committed_marker.oids = vec!["e69de29bb2d1d6434b8b29ae775ad8c2e48c5391".to_string()];

        let started = Instant::now();
        let refresh = tokio::time::timeout(
            Duration::from_secs(2),
            traces::with_traces_marker_precommit_read_delay(
                POST_CAS_MARKER_REFRESH_GRACE + Duration::from_millis(200),
                store.refresh_committed_marker(&committed_marker, &expected_generation),
            ),
        )
        .await
        .expect("delayed fence read must honor the bounded recovery grace");
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the fence-read timeout must finish near the recovery grace"
        );
        assert!(
            matches!(
                refresh,
                Err(CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::Marker
                })
            ),
            "a delayed fence read must retain recovery work instead of refreshing the marker: {refresh:?}"
        );
        // The timeout cancels only the pure fence read. Give a dropped future
        // time to prove it cannot later dispatch the marker UPDATE.
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            fixture_row_count(
                &conn,
                "SELECT COUNT(*) AS n FROM checkpoint_marker_refresh_audit",
            )
            .await,
            0,
            "a delayed fence read must not dispatch a marker update after its deadline"
        );
        let entry = crate::internal::metadata::MetadataKv::get_with_conn(
            &conn,
            crate::internal::metadata::MetadataScope::AgentTracesInflight,
            "marker-refresh-fence-session",
            "marker-refresh-fence-checkpoint",
        )
        .await
        .expect("read original marker after delayed fence timeout")
        .expect("original marker must remain as recovery evidence");
        let persisted = traces::decode_and_validate_traces_inflight_marker(
            &entry.value,
            "marker-refresh-fence-session",
            "marker-refresh-fence-checkpoint",
        )
        .expect("decode original marker after delayed fence timeout");
        assert_eq!(
            persisted, marker,
            "a delayed fence read must preserve the original marker evidence"
        );
    }

    #[tokio::test]
    async fn scoped_checkpoint_deadline_bounds_preflight_reads_under_exclusive_sqlite_lock() {
        let repository = tempfile::tempdir().expect("create checkpoint deadline repository");
        let database_path = repository.path().join("checkpoint-deadline.sqlite");
        let conn = crate::internal::db::create_database(
            database_path
                .to_str()
                .expect("checkpoint deadline database path is utf-8"),
        )
        .await
        .expect("create checkpoint deadline database");
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_string(
            backend,
            "PRAGMA journal_mode = DELETE".to_string(),
        ))
        .await
        .expect("force rollback-journal mode for checkpoint deadline regression");
        let scope = CaptureScope {
            repo_id: "checkpoint-deadline-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: Some("checkpoint-deadline-workspace".to_string()),
            workspace_fence: Some(7),
        };
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO workspace_record (
                workspace_id, repo_id, kind, worktree_id, path, owner_kind,
                state, lease_owner, lease_fence, lease_expires_at, created_at, updated_at
             ) VALUES (?, ?, 'task_copy', NULL, '/tmp/checkpoint-deadline-workspace', 'agent',
                       'active', 'checkpoint-deadline-owner', ?, 9999999999999, 1, 1)",
            [
                scope.workspace_id.clone().into(),
                scope.repo_id.clone().into(),
                scope.workspace_fence.into(),
            ],
        ))
        .await
        .expect("seed checkpoint deadline workspace lease");

        let locker = crate::internal::db::establish_connection_with_busy_timeout(
            database_path
                .to_str()
                .expect("checkpoint deadline database path is utf-8"),
            Duration::from_millis(50),
        )
        .await
        .expect("open checkpoint deadline lock holder");
        locker
            .execute_raw(Statement::from_string(
                backend,
                "PRAGMA journal_mode = DELETE".to_string(),
            ))
            .await
            .expect("force rollback-journal mode on checkpoint deadline lock holder");
        locker
            .execute_raw(Statement::from_string(
                backend,
                "BEGIN EXCLUSIVE".to_string(),
            ))
            .await
            .expect("acquire exclusive checkpoint deadline lock");

        let checkpoint_id = "checkpoint-deadline-lock-checkpoint";
        let store = TracesCheckpointStore::new(
            &conn,
            repository.path(),
            "checkpoint-deadline-lock-session",
            checkpoint_id,
            &[],
        )
        .expect("create scoped checkpoint deadline store")
        .with_capture_scope(scope);
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("build checkpoint deadline payload");
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_millis(100))
            .expect("establish checkpoint preflight deadline");
        let request = CheckpointWriteRequest::new(
            "checkpoint-deadline-lock-replay",
            checkpoint_id,
            "checkpoint-deadline-lock-session",
            "claude_code",
            None,
            CheckpointScope::Committed,
            store.marker_generation(),
            None,
            &payload,
            None,
            Some(deadline),
        )
        .expect("create scoped checkpoint deadline request");
        let started = Instant::now();
        let write = tokio::time::timeout(Duration::from_secs(2), store.write(request))
            .await
            .expect("exclusive-lock checkpoint preflight must honor its deadline");
        locker
            .execute_raw(Statement::from_string(backend, "ROLLBACK".to_string()))
            .await
            .expect("release exclusive checkpoint deadline lock");

        assert!(
            matches!(write, Err(CheckpointStoreError::DeadlineExceeded)),
            "exclusive preflight lock must report DeadlineExceeded: {write:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "checkpoint preflight must not wait for SQLite's busy timeout"
        );
        for (sink, sql) in [
            (
                "traces marker",
                "SELECT COUNT(*) AS n FROM metadata_kv
                 WHERE scope = 'agent_traces_inflight'
                   AND target = 'checkpoint-deadline-lock-session'
                   AND key = 'checkpoint-deadline-lock-checkpoint'",
            ),
            (
                "traces ref",
                "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'",
            ),
            (
                "checkpoint catalog row",
                "SELECT COUNT(*) AS n FROM agent_checkpoint
                 WHERE checkpoint_id = 'checkpoint-deadline-lock-checkpoint'",
            ),
        ] {
            assert_eq!(
                fixture_row_count(&conn, sql).await,
                0,
                "deadline-cancelled checkpoint preflight must not publish a {sink}"
            );
        }
    }

    #[tokio::test]
    async fn elapsed_deadline_before_registration_never_publishes_marker_claim_or_ref() {
        let conn = marker_lifecycle_fixture().await;
        let repo = tempfile::tempdir().expect("create checkpoint deadline repository");
        let pause = test_support::pause_before_registration_once(repo.path());
        let (deadline_tx, deadline_rx) = tokio::sync::oneshot::channel();
        let writer_conn = conn.clone();
        let writer_repo = repo.path().to_path_buf();
        let writer = tokio::spawn(async move {
            let store = TracesCheckpointStore::new(
                &writer_conn,
                &writer_repo,
                "deadline-session",
                "deadline-registration-checkpoint",
                &[],
            )
            .expect("create deadline checkpoint store");
            let payload = CheckpointRedactedPayload::from_snapshot(
                snapshot(),
                sidecar(b"{}"),
                sidecar(b"{}\n"),
                sidecar(b"{}"),
            )
            .expect("build redacted deadline checkpoint payload");
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_millis(50))
                .expect("establish checkpoint registration deadline");
            deadline_tx
                .send(deadline)
                .expect("send checkpoint registration deadline");
            let request = CheckpointWriteRequest::new(
                "deadline-registration-replay",
                "deadline-registration-checkpoint",
                "deadline-session",
                "claude_code",
                None,
                CheckpointScope::Committed,
                store.marker_generation(),
                None,
                &payload,
                None,
                Some(deadline),
            )
            .expect("create deadline checkpoint request");
            store.write(request).await
        });
        let deadline = deadline_rx
            .await
            .expect("writer must publish its registration deadline");
        tokio::time::timeout(Duration::from_secs(2), pause.wait_until_entered())
            .await
            .expect("writer must pause before marker registration");
        tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.monotonic())).await;
        pause.release();

        let result = tokio::time::timeout(Duration::from_secs(2), writer)
            .await
            .expect("deadline writer must finish")
            .expect("join deadline writer");
        assert!(
            matches!(result, Err(CheckpointStoreError::DeadlineExceeded)),
            "expired pre-registration deadline must be reported before any durable sink: {result:?}"
        );
        assert!(
            !marker_exists(&conn, "deadline-registration-checkpoint").await,
            "expired registration must leave no live marker"
        );
        assert_eq!(
            fixture_row_count(
                &conn,
                "SELECT COUNT(*) AS n FROM checkpoint_marker_write_audit",
            )
            .await,
            0,
            "expired registration must not even transiently publish a marker"
        );
        for (sink, sql) in [
            (
                "coverage claim",
                "SELECT COUNT(*) AS n FROM agent_coverage_claim",
            ),
            ("traces ref", "SELECT COUNT(*) AS n FROM reference"),
            (
                "checkpoint catalog row",
                "SELECT COUNT(*) AS n FROM agent_checkpoint",
            ),
        ] {
            assert_eq!(
                fixture_row_count(&conn, sql).await,
                0,
                "expired registration must not write {sink}"
            );
        }
    }

    #[tokio::test]
    async fn expired_replay_and_prebound_retirement_preserve_marker() {
        let conn = marker_lifecycle_fixture().await;
        let checkpoint_id = "deadline-prebound-checkpoint";
        conn.execute_raw(Statement::from_string(
            conn.get_database_backend(),
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope)
             VALUES ('deadline-prebound-checkpoint', 'deadline-session', 'committed')"
                .to_string(),
        ))
        .await
        .expect("seed durable replay checkpoint");
        let marker = TracesInflightMarker::new(
            "deadline-session",
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        traces::write_traces_inflight_marker(&conn, &marker)
            .await
            .expect("seed ordinary marker");
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_millis(1), 0);
        let replay = TracesCheckpointStore::replay_status_for(
            &conn,
            None,
            checkpoint_id,
            "deadline-session",
            CheckpointScope::Committed,
            Some(expired),
        )
        .await;
        assert!(
            matches!(replay, Err(CheckpointStoreError::DeadlineExceeded)),
            "expired replay must not retire its ordinary marker: {replay:?}"
        );
        assert!(
            marker_exists(&conn, checkpoint_id).await,
            "expired replay must retain recovery evidence"
        );

        let repo = tempfile::tempdir().expect("create expired prebound checkpoint repository");
        let store = TracesCheckpointStore::from_prebound_attempt(&conn, repo.path(), marker)
            .expect("create prebound checkpoint store");
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("build redacted prebound checkpoint payload");
        let request = CheckpointWriteRequest::new(
            "deadline-prebound-replay",
            checkpoint_id,
            "deadline-session",
            "claude_code",
            None,
            CheckpointScope::Committed,
            store.marker_generation(),
            None,
            &payload,
            None,
            Some(CaptureCommitDeadline::from_test_pair(
                Instant::now() - Duration::from_millis(1),
                0,
            )),
        )
        .expect("create expired prebound checkpoint request");
        let write = store.write(request).await;
        assert!(
            matches!(write, Err(CheckpointStoreError::DeadlineExceeded)),
            "expired prebound write must not clean up its marker: {write:?}"
        );
        assert!(
            marker_exists(&conn, checkpoint_id).await,
            "expired prebound write must retain recovery evidence"
        );

        let retirement = store
            .retire_ordinary_marker(Some(CaptureCommitDeadline::from_test_pair(
                Instant::now() - Duration::from_millis(1),
                0,
            )))
            .await;
        assert!(
            matches!(retirement, Err(CheckpointStoreError::DeadlineExceeded)),
            "expired marker retirement must preserve recovery evidence: {retirement:?}"
        );
        assert!(
            marker_exists(&conn, checkpoint_id).await,
            "expired retirement must leave the ordinary marker for recovery"
        );
    }

    #[test]
    fn payload_accepts_only_a_complete_redacted_snapshot() {
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("complete snapshot is accepted");
        assert!(
            payload
                .transcript()
                .bytes()
                .windows(4)
                .all(|w| w != b"AKIA")
        );
        assert_eq!(
            payload.snapshot().completeness,
            crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Complete
        );
    }

    #[test]
    fn expired_request_reaches_the_store_for_terminal_pending_finalization() {
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("complete snapshot");
        let result = CheckpointWriteRequest::new(
            "replay",
            "checkpoint",
            "session",
            "agent",
            None,
            CheckpointScope::Committed,
            "generation",
            None,
            &payload,
            None,
            Some(CaptureCommitDeadline::from_test_pair(
                Instant::now() - Duration::from_millis(1),
                0,
            )),
        );
        assert!(
            result.is_ok(),
            "the coordinator must receive an expired request so a terminal receipt can persist pending evidence"
        );
    }

    #[test]
    fn derived_turn_projection_keeps_complete_source_provenance_without_length_aliasing() {
        let mut source_snapshot = snapshot().safe_projection();
        assert!(
            source_snapshot.bind_source_commitment(format!("source/hmac-v2/{}", "a".repeat(64)))
        );
        let payload = CheckpointRedactedPayload::from_derived_turn_projection(
            sidecar(b"{\"role\":\"assistant\",\"content\":\"turn\"}\n"),
            source_snapshot.clone(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("complete source may produce a compact derived turn projection");
        assert_eq!(
            payload.provenance(),
            CheckpointPayloadProvenance::DerivedTurnProjection
        );
        assert_eq!(payload.snapshot(), &source_snapshot);
        assert_ne!(
            payload.transcript().len(),
            source_snapshot.transcript_redacted_bytes,
            "the per-turn projection must not masquerade as the whole source"
        );
    }

    #[test]
    fn durable_payload_rejects_a_transient_source_checksum() {
        let snapshot = snapshot();
        let projection = snapshot.safe_projection();
        let transcript = snapshot
            .into_redacted_transcript()
            .expect("fixture snapshot is complete");
        assert!(matches!(
            CheckpointRedactedPayload::from_redacted_capture(
                transcript,
                Some(projection),
                sidecar(b"{}"),
                sidecar(b"{}\n"),
                sidecar(b"{}"),
            ),
            Err(CheckpointStoreError::InvalidRequest { field }) if field == "source snapshot commitment"
        ));
    }

    #[test]
    fn durable_payload_rejects_a_transient_checksum_on_a_partial_snapshot() {
        let partial = CaptureSnapshotProjection {
            completeness:
                crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Partial,
            partial_reason: Some(
                crate::internal::ai::capture::snapshot::CaptureSnapshotPartialReason::DeadlineExceeded,
            ),
            source: Some(crate::internal::ai::capture::snapshot::CaptureSnapshotSource {
                kind: crate::internal::ai::capture::snapshot::CaptureSnapshotSourceKind::ProviderFile,
                identity: "not_retained:v1".to_string(),
                digest_sha256: Some(format!("sha256:{}", "b".repeat(64))),
                byte_len: Some(1),
            }),
            transcript_redacted_bytes: 0,
            redaction_match_count: 0,
            redaction_bytes_scanned: 0,
            redaction_bytes_redacted: 0,
        };
        assert!(matches!(
            CheckpointRedactedPayload::from_redacted_capture(
                sidecar(b"fallback"),
                Some(partial),
                sidecar(b"{}"),
                sidecar(b"{}\n"),
                sidecar(b"{}"),
            ),
            Err(CheckpointStoreError::InvalidRequest { field }) if field == "source snapshot commitment"
        ));
    }

    #[tokio::test]
    async fn marker_registration_replays_a_durable_checkpoint_without_creating_a_marker() {
        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open checkpoint registration fixture");
        let backend = conn.get_database_backend();
        for statement in [
            "CREATE TABLE config_kv (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                key TEXT NOT NULL,
                value TEXT NOT NULL,
                encrypted INTEGER NOT NULL DEFAULT 0
            )",
            "CREATE TABLE agent_session (
                session_id TEXT PRIMARY KEY,
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL
            )",
            "CREATE TABLE agent_import_tombstone (
                agent_kind TEXT NOT NULL,
                provider_session_id TEXT NOT NULL
            )",
            "CREATE TABLE agent_checkpoint (
                checkpoint_id TEXT PRIMARY KEY,
                session_id TEXT NOT NULL,
                scope TEXT NOT NULL
            )",
        ] {
            conn.execute_raw(Statement::from_string(backend, statement.to_string()))
                .await
                .expect("create checkpoint registration fixture table");
        }
        conn.execute_raw(Statement::from_string(
            backend,
            include_str!("../../../../sql/migrations/2026070201_metadata_kv.sql").to_string(),
        ))
        .await
        .expect("create checkpoint marker registry");
        conn.execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id)
             VALUES ('registration-session', 'claude_code', 'registration-provider')"
                .to_string(),
        ))
        .await
        .expect("seed checkpoint registration session");
        conn.execute_raw(Statement::from_string(
            backend,
            "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope)
             VALUES ('registration-checkpoint', 'registration-session', 'committed')"
                .to_string(),
        ))
        .await
        .expect("seed durable checkpoint");

        let marker = traces::TracesInflightMarker::new(
            "registration-session",
            "registration-checkpoint",
            chrono::Utc::now().timestamp_millis(),
        );
        let outcome = traces::register_traces_write_attempt_with_capture_scope(
            &conn,
            None,
            &marker,
            &[],
            Some(CheckpointScope::Committed),
            None,
        )
        .await
        .expect("durable checkpoint replay is a typed registration outcome");
        assert_eq!(
            outcome,
            traces::TracesWriteAttemptRegistration::AlreadyCommitted
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                "registration-session",
                "registration-checkpoint",
            )
            .await
            .expect("read checkpoint marker after durable replay")
            .is_none(),
            "durable replay must not recreate a cleared writer marker"
        );
    }

    /// A real repository database, loose-object store, and session row so a
    /// store write runs the production marker/object/ref/catalog path.
    struct StoreReplayFixture {
        _dir: tempfile::TempDir,
        repo: PathBuf,
        conn: DatabaseConnection,
    }

    const REPLAY_SESSION: &str = "claude__store-replay-native";
    const REPLAY_CHECKPOINT: &str = "5f0d6f8e-6c7a-4b1e-9b5e-3d2a1c0b9a87";

    async fn store_replay_fixture() -> StoreReplayFixture {
        let dir = tempfile::tempdir().expect("create checkpoint replay repository");
        let repo = dir.path().to_path_buf();
        let conn = crate::internal::db::create_database(
            repo.join(crate::utils::util::DATABASE)
                .to_str()
                .expect("checkpoint replay database path is utf-8"),
        )
        .await
        .expect("create checkpoint replay database");
        conn.execute_raw(Statement::from_sql_and_values(
            conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id,
             state, working_dir, metadata_json, started_at, last_event_at, sync_revision,
             repo_id, worktree_id, scope_state)
             VALUES (?, 'claude_code', 'store-replay-native', 'active', ?, '{}',
             1, 1, 1, 'store-replay-repo', '', 'scoped')",
            [
                REPLAY_SESSION.into(),
                repo.to_string_lossy().into_owned().into(),
            ],
        ))
        .await
        .expect("seed checkpoint replay session");
        StoreReplayFixture {
            _dir: dir,
            repo,
            conn,
        }
    }

    /// The production catalog companion, applied inside the ref-CAS
    /// transaction exactly as the live coverage gate does.
    struct CatalogCompanion(CheckpointCatalogMutation);

    #[async_trait]
    impl TracesTxnExtra for CatalogCompanion {
        async fn apply(
            &self,
            txn: &DatabaseTransaction,
            ctx: &TracesCommitCtx,
        ) -> anyhow::Result<()> {
            self.0.apply(txn, ctx).await.map_err(anyhow::Error::new)
        }
    }

    impl StoreReplayFixture {
        /// Deliver the same replay-stable checkpoint request through a fresh
        /// store, as a redelivered hook or retry would.
        async fn write(&self) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
            let store = TracesCheckpointStore::new(
                &self.conn,
                &self.repo,
                REPLAY_SESSION,
                REPLAY_CHECKPOINT,
                &[],
            )
            .expect("create checkpoint replay store");
            let payload = CheckpointRedactedPayload::from_snapshot(
                snapshot(),
                sidecar(b"{}"),
                sidecar(b"{}\n"),
                sidecar(b"{}"),
            )
            .expect("build checkpoint replay payload");
            let companion = CatalogCompanion(
                CheckpointCatalogMutation::new(
                    REPLAY_CHECKPOINT,
                    REPLAY_SESSION,
                    CheckpointScope::Committed,
                    None,
                    1,
                )
                .expect("build checkpoint replay catalog companion"),
            );
            let request = CheckpointWriteRequest::new(
                "store-replay-key",
                REPLAY_CHECKPOINT,
                REPLAY_SESSION,
                "claude_code",
                None,
                CheckpointScope::Committed,
                store.marker_generation(),
                None,
                &payload,
                Some(&companion),
                None,
            )
            .expect("create checkpoint replay request");
            store.write(request).await
        }

        async fn checkpoint_rows(&self) -> i64 {
            self.conn
                .query_one_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = ?",
                    [REPLAY_CHECKPOINT.into()],
                ))
                .await
                .expect("count replay checkpoint rows")
                .expect("replay checkpoint count row")
                .try_get_by("n")
                .expect("decode replay checkpoint count")
        }

        async fn catalog_commit(&self) -> String {
            self.conn
                .query_one_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "SELECT traces_commit FROM agent_checkpoint WHERE checkpoint_id = ?",
                    [REPLAY_CHECKPOINT.into()],
                ))
                .await
                .expect("read replay checkpoint commit")
                .expect("replay checkpoint row exists")
                .try_get_by("traces_commit")
                .expect("decode replay checkpoint commit")
        }

        async fn traces_head(&self) -> Option<String> {
            self.conn
                .query_one_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "SELECT \"commit\" AS head FROM reference
                     WHERE name = ? AND kind = 'Branch' AND remote IS NULL",
                    [crate::internal::branch::TRACES_BRANCH.into()],
                ))
                .await
                .expect("read traces head")
                .and_then(|row| {
                    row.try_get_by::<Option<String>, _>("head")
                        .expect("decode traces head")
                })
        }

        async fn marker(&self) -> Option<TracesInflightMarker> {
            crate::internal::metadata::MetadataKv::get_with_conn(
                &self.conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                REPLAY_SESSION,
                REPLAY_CHECKPOINT,
            )
            .await
            .expect("read replay checkpoint marker")
            .map(|entry| {
                traces::decode_and_validate_traces_inflight_marker(
                    &entry.value,
                    REPLAY_SESSION,
                    REPLAY_CHECKPOINT,
                )
                .expect("decode replay checkpoint marker")
            })
        }

        /// `libra agent doctor --repair`'s marker entry point.
        async fn doctor_repair_marker(&self) -> bool {
            let storage = std::sync::Arc::new(crate::utils::client_storage::ClientStorage::init(
                self.repo.join("objects"),
            ));
            crate::internal::ai::history::HistoryManager::for_traces(
                storage,
                self.repo.clone(),
                std::sync::Arc::new(self.conn.clone()),
            )
            .repair_expired_traces_inflight_marker_for_test(
                REPLAY_SESSION,
                REPLAY_CHECKPOINT,
                Utc::now().timestamp_millis(),
            )
            .await
            .expect("doctor repair of the replay checkpoint marker")
        }
    }

    /// Durable evidence a failed first delivery must leave for recovery.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FaultEvidence {
        /// No marker remains; the replay can register immediately.
        Absent,
        /// The attempt's own marker names created objects for doctor/GC.
        CleanupPending,
        /// A takeover generation owns the key; the stale writer left it.
        Takeover,
        /// The ref/catalog committed; an ordinary marker awaits cleanup.
        CommittedOrdinary,
    }

    /// AC4/AC7 step-by-step fault matrix through the concrete store. Each
    /// point fails exactly once with its typed classification and evidence;
    /// deterministic recovery then lets the same replay key commit one
    /// checkpoint, and a further replay performs no second ref/catalog write.
    #[tokio::test]
    async fn store_fault_matrix_replays_one_checkpoint_without_duplicate_action() {
        use test_support::CheckpointFaultPoint as Point;

        for point in Point::ALL {
            let fixture = store_replay_fixture().await;
            test_support::fail_once_at(point);
            let first = fixture.write().await;
            assert_eq!(
                test_support::armed_fault(),
                None,
                "{point:?}: the armed fault must fire on the first delivery"
            );

            let (expected_first, evidence) = match point {
                Point::MarkerRegistration | Point::AfterMarkerRegistration => (
                    Err(CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::Marker,
                    }),
                    FaultEvidence::Absent,
                ),
                Point::AfterObjectWrite => (
                    Err(CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::ObjectWrite,
                    }),
                    FaultEvidence::CleanupPending,
                ),
                Point::RefCasExhausted => (
                    Ok(CheckpointWriteOutcome::ConflictUnchanged {
                        reason: CheckpointConflictReason::RefCas,
                    }),
                    FaultEvidence::CleanupPending,
                ),
                Point::StaleMarkerGeneration => (
                    Ok(CheckpointWriteOutcome::ConflictUnchanged {
                        reason: CheckpointConflictReason::StaleMarkerGeneration,
                    }),
                    FaultEvidence::Takeover,
                ),
                Point::CompanionTransaction => (
                    Err(CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::CatalogTransaction,
                    }),
                    FaultEvidence::CleanupPending,
                ),
                Point::MarkerCleanup => (
                    Ok(CheckpointWriteOutcome::PendingCleanup {
                        checkpoint_id: REPLAY_CHECKPOINT.to_string(),
                        marker_generation: String::new(),
                    }),
                    FaultEvidence::CommittedOrdinary,
                ),
            };
            match (&first, &expected_first) {
                (
                    Ok(CheckpointWriteOutcome::PendingCleanup { checkpoint_id, .. }),
                    Ok(CheckpointWriteOutcome::PendingCleanup {
                        checkpoint_id: expected,
                        ..
                    }),
                ) => assert_eq!(checkpoint_id, expected, "{point:?}"),
                _ => assert_eq!(
                    first, expected_first,
                    "{point:?}: unexpected first-delivery classification"
                ),
            }

            let marker = fixture.marker().await;
            match evidence {
                FaultEvidence::Absent => assert!(
                    marker.is_none(),
                    "{point:?}: a pre-object failure must retire its ordinary marker"
                ),
                FaultEvidence::CleanupPending => {
                    let marker = marker.unwrap_or_else(|| {
                        panic!("{point:?}: created objects must keep durable ownership evidence")
                    });
                    assert!(
                        marker.cleanup_pending && !marker.created_oids.is_empty(),
                        "{point:?}: rejected objects must be handed to doctor/GC: {marker:?}"
                    );
                }
                FaultEvidence::Takeover => {
                    let marker = marker.unwrap_or_else(|| {
                        panic!("{point:?}: the stale writer must not erase the takeover marker")
                    });
                    assert!(
                        !marker.cleanup_pending && marker.started_at_ms == 0,
                        "{point:?}: the takeover marker must survive unchanged: {marker:?}"
                    );
                }
                FaultEvidence::CommittedOrdinary => {
                    let marker = marker.unwrap_or_else(|| {
                        panic!("{point:?}: a failed post-CAS cleanup must keep its marker")
                    });
                    assert!(
                        !marker.cleanup_pending && marker.commit.is_some(),
                        "{point:?}: the refreshed marker must name the committed attempt: {marker:?}"
                    );
                }
            }
            if evidence == FaultEvidence::CommittedOrdinary {
                assert_eq!(fixture.checkpoint_rows().await, 1, "{point:?}");
                assert_eq!(
                    fixture.traces_head().await,
                    Some(fixture.catalog_commit().await),
                    "{point:?}: the committed ref and catalog row must agree"
                );
            } else {
                assert_eq!(
                    fixture.checkpoint_rows().await,
                    0,
                    "{point:?}: a failed write must not leave a catalog row"
                );
                if point == Point::RefCasExhausted {
                    assert!(
                        fixture.traces_head().await.is_some(),
                        "{point:?}: the competing writer owns the traces head"
                    );
                } else {
                    assert_eq!(
                        fixture.traces_head().await,
                        None,
                        "{point:?}: a failed write must leave refs/libra/traces unchanged"
                    );
                }
            }

            if matches!(
                evidence,
                FaultEvidence::CleanupPending | FaultEvidence::Takeover
            ) {
                assert!(
                    fixture.doctor_repair_marker().await,
                    "{point:?}: doctor repair must retire the recovery marker"
                );
                assert!(fixture.marker().await.is_none(), "{point:?}");
            }

            let replay = fixture.write().await;
            if evidence == FaultEvidence::CommittedOrdinary {
                assert_eq!(
                    replay,
                    Ok(CheckpointWriteOutcome::AlreadyExists {
                        checkpoint_id: REPLAY_CHECKPOINT.to_string(),
                    }),
                    "{point:?}: the replay must settle cleanup without appending"
                );
            } else {
                assert!(
                    matches!(replay, Ok(CheckpointWriteOutcome::Written { .. })),
                    "{point:?}: the recovered replay must commit: {replay:?}"
                );
            }
            assert_eq!(fixture.checkpoint_rows().await, 1, "{point:?}");
            let head = fixture.traces_head().await;
            assert_eq!(
                head,
                Some(fixture.catalog_commit().await),
                "{point:?}: refs/libra/traces must name the one durable checkpoint"
            );
            assert!(
                fixture.marker().await.is_none(),
                "{point:?}: a settled replay leaves no writer marker"
            );

            let duplicate = fixture.write().await;
            assert_eq!(
                duplicate,
                Ok(CheckpointWriteOutcome::AlreadyExists {
                    checkpoint_id: REPLAY_CHECKPOINT.to_string(),
                }),
                "{point:?}: a duplicate replay must be an acknowledgement"
            );
            assert_eq!(fixture.checkpoint_rows().await, 1, "{point:?}");
            assert_eq!(
                fixture.traces_head().await,
                head,
                "{point:?}: a duplicate replay must not advance refs/libra/traces"
            );
        }
    }

    /// ACF-06 AC4: a replay that settles without a checkpoint write (the live
    /// coverage no-op) resolves the action's failure outcome through the same
    /// store replay as `write`. Only this action's ordinary leftover marker is
    /// retired; absent durability, cleanup ownership, a foreign identity, an
    /// expired deadline, or a dead workspace lease leave every marker intact.
    #[tokio::test]
    async fn committed_replay_without_write_retires_only_this_actions_ordinary_marker() {
        use test_support::CheckpointFaultPoint as Point;

        let fixture = store_replay_fixture().await;
        let scope = CaptureScope {
            repo_id: "store-replay-repo".to_string(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let settle = |deadline: Option<CaptureCommitDeadline>| {
            TracesCheckpointStore::settle_committed_replay_without_write(
                &fixture.conn,
                &scope,
                REPLAY_CHECKPOINT,
                REPLAY_SESSION,
                deadline,
            )
        };

        // No durable row: a marker for this slot may belong to a live
        // writer, so classification must not touch it.
        let pre_durable = TracesInflightMarker::new(
            REPLAY_SESSION,
            REPLAY_CHECKPOINT,
            Utc::now().timestamp_millis(),
        );
        traces::write_traces_inflight_marker(&fixture.conn, &pre_durable)
            .await
            .expect("seed pre-durable writer marker");
        assert_eq!(settle(None).await, Ok(CheckpointReplayStatus::Missing));
        assert_eq!(
            fixture.marker().await,
            Some(pre_durable.clone()),
            "a missing checkpoint must leave its writer marker untouched"
        );
        traces::clear_traces_inflight_marker(&fixture.conn, REPLAY_SESSION, REPLAY_CHECKPOINT)
            .await
            .expect("drop pre-durable writer marker");

        // The real first-delivery failure: ref/catalog committed, ordinary
        // marker retirement failed.
        test_support::fail_once_at(Point::MarkerCleanup);
        assert!(matches!(
            fixture.write().await,
            Ok(CheckpointWriteOutcome::PendingCleanup { .. })
        ));
        let leftover = fixture
            .marker()
            .await
            .expect("failed cleanup leaves its ordinary marker");
        assert!(!leftover.cleanup_pending && leftover.commit.is_some());
        let other_action = TracesInflightMarker::new(
            REPLAY_SESSION,
            "6a1e7f9f-7d8b-4c2f-8c6f-4e3b2d1c0b98",
            Utc::now().timestamp_millis(),
        );
        traces::write_traces_inflight_marker(&fixture.conn, &other_action)
            .await
            .expect("seed another action's writer marker");
        let other_marker = || async {
            crate::internal::metadata::MetadataKv::get_with_conn(
                &fixture.conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                REPLAY_SESSION,
                &other_action.attempt_id,
            )
            .await
            .expect("read another action's writer marker")
            .is_some()
        };

        // Fail-closed preflights retain the evidence.
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_millis(1), 0);
        assert_eq!(
            settle(Some(expired)).await,
            Err(CheckpointStoreError::DeadlineExceeded)
        );
        let dead_lease = CaptureScope {
            workspace_id: Some("store-replay-released-workspace".to_string()),
            workspace_fence: Some(1),
            ..scope.clone()
        };
        assert_eq!(
            TracesCheckpointStore::settle_committed_replay_without_write(
                &fixture.conn,
                &dead_lease,
                REPLAY_CHECKPOINT,
                REPLAY_SESSION,
                None,
            )
            .await,
            Err(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::ScopeFence,
            })
        );
        assert_eq!(
            TracesCheckpointStore::settle_committed_replay_without_write(
                &fixture.conn,
                &scope,
                REPLAY_CHECKPOINT,
                "claude__another-session",
                None,
            )
            .await,
            Err(CheckpointStoreError::ReplayPayloadMismatch),
            "a checkpoint id bound to another session is an identity conflict"
        );
        assert_eq!(fixture.marker().await, Some(leftover.clone()));

        // Objects still owned for doctor/GC are never erased here.
        let mut cleanup_owned = leftover.clone();
        cleanup_owned.cleanup_pending = true;
        traces::write_traces_inflight_marker(&fixture.conn, &cleanup_owned)
            .await
            .expect("promote marker to cleanup ownership");
        assert!(matches!(
            settle(None).await,
            Ok(CheckpointReplayStatus::PendingCleanup { .. })
        ));
        assert_eq!(fixture.marker().await, Some(cleanup_owned));
        traces::write_traces_inflight_marker(&fixture.conn, &leftover)
            .await
            .expect("restore the ordinary leftover marker");

        // The covered replay retires exactly this action's ordinary marker.
        assert_eq!(settle(None).await, Ok(CheckpointReplayStatus::Complete));
        assert!(
            fixture.marker().await.is_none(),
            "the settled replay must retire this action's ordinary marker"
        );
        assert!(
            other_marker().await,
            "another action's writer marker must survive the settlement"
        );
        assert_eq!(fixture.checkpoint_rows().await, 1);
        assert_eq!(
            settle(None).await,
            Ok(CheckpointReplayStatus::Complete),
            "a settled replay is idempotent"
        );
    }

    /// A request sealed to another marker generation never reaches marker
    /// registration, objects, the ref, or the catalog.
    #[tokio::test]
    async fn foreign_marker_generation_is_a_stale_conflict_without_side_effects() {
        let fixture = store_replay_fixture().await;
        let store = TracesCheckpointStore::new(
            &fixture.conn,
            &fixture.repo,
            REPLAY_SESSION,
            REPLAY_CHECKPOINT,
            &[],
        )
        .expect("create checkpoint replay store");
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("build checkpoint replay payload");
        let foreign_generation = uuid::Uuid::new_v4().to_string();
        let request = CheckpointWriteRequest::new(
            "store-replay-key",
            REPLAY_CHECKPOINT,
            REPLAY_SESSION,
            "claude_code",
            None,
            CheckpointScope::Committed,
            &foreign_generation,
            None,
            &payload,
            None,
            None,
        )
        .expect("create foreign-generation request");
        assert_eq!(
            store.write(request).await,
            Ok(CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::StaleMarkerGeneration,
            })
        );
        assert!(fixture.marker().await.is_none());
        assert_eq!(fixture.checkpoint_rows().await, 0);
        assert_eq!(fixture.traces_head().await, None);
    }

    /// Classification follows typed causes through `anyhow` context layers
    /// and ignores display text that merely resembles a conflict.
    #[test]
    fn append_error_classification_uses_typed_causes_not_display_text() {
        use crate::internal::ai::traces::RejectedCheckpointCleanupDeferred;

        let exhausted = anyhow::Error::new(CheckpointAppendConflict::RefCasExhausted)
            .context("append traces checkpoint");
        assert_eq!(
            TracesCheckpointStore::classify_append_error(&exhausted),
            AppendFailureClass::Conflict(CheckpointConflictReason::RefCas)
        );

        let fenced = anyhow::Error::new(CheckpointAppendConflict::MarkerFenced(
            "checkpoint writer marker was fenced before ref update; retry the operation",
        ))
        .context("revalidate marker generation before ref update")
        .context(RejectedCheckpointCleanupDeferred {
            reason: "fixture".to_string(),
        });
        assert_eq!(
            TracesCheckpointStore::classify_append_error(&fenced),
            AppendFailureClass::Conflict(CheckpointConflictReason::StaleMarkerGeneration)
        );
        assert!(
            typed_append_cause::<RejectedCheckpointCleanupDeferred>(&fenced).is_some(),
            "a deferred-cleanup context layer must keep the original marker"
        );

        let companion = anyhow::anyhow!("constraint failed")
            .context(CheckpointCompanionTransactionFailed)
            .context(RejectedCheckpointCleanupDeferred {
                reason: "fixture".to_string(),
            });
        assert_eq!(
            TracesCheckpointStore::classify_append_error(&companion),
            AppendFailureClass::Store(CheckpointStoreStage::CatalogTransaction)
        );

        // Former text-matched spellings are now plain failures: a SQLite or
        // deadline error under a "marker generation" context is not a fence
        // conflict.
        let text_only = anyhow::anyhow!("checkpoint CAS retry loop exhausted")
            .context("load checkpoint writer marker generation");
        assert_eq!(
            TracesCheckpointStore::classify_append_error(&text_only),
            AppendFailureClass::Store(CheckpointStoreStage::ObjectWrite)
        );
    }

    #[test]
    fn checkpoint_debug_output_never_echoes_provider_identities() {
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("build debug fixture payload");
        let request = CheckpointWriteRequest::new(
            "debug-replay",
            "debug-checkpoint",
            "claude__native-session-material",
            "claude_code",
            None,
            CheckpointScope::Subagent,
            "debug-generation",
            Some("toolu_native-tool-material"),
            &payload,
            None,
            None,
        )
        .expect("create debug fixture request");
        let rendered = format!("{request:?}");
        for secret in [
            "native-session-material",
            "native-tool-material",
            "debug-generation",
        ] {
            assert!(
                !rendered.contains(secret),
                "request Debug leaked {secret}: {rendered}"
            );
        }
        assert!(rendered.contains("debug-checkpoint"), "{rendered}");

        let mutation = CheckpointCatalogMutation::new(
            "debug-checkpoint",
            "claude__native-session-material",
            CheckpointScope::Committed,
            None,
            1,
        )
        .expect("create debug fixture mutation");
        let rendered = format!("{mutation:?}");
        assert!(
            !rendered.contains("native-session-material"),
            "catalog mutation Debug leaked the session ID: {rendered}"
        );
    }
}
