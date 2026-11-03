//! Typed checkpoint persistence boundary for Agent Capture.
//!
//! The trace writer remains the owner of the `refs/libra/traces` layout.  This
//! module deliberately presents a request/result port instead: callers can
//! supply only a complete [`CaptureSnapshot`] (and therefore redacted bytes),
//! a previously registered marker generation, and the transaction companion
//! writes that must commit with the ref CAS.  It is intentionally agnostic to
//! provider names and ref names.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
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
use crate::internal::{
    ai::{
        capture::{
            catalog::CaptureCatalogTerminalAttemptFence,
            key,
            pending_payload::{
                PendingReasoningReplay, VersionedPendingPayloadRequirement,
                VersionedPendingReasoningReplay,
            },
            snapshot::{
                CaptureSnapshotProjection, VerifiedOrdinaryPartialBinding,
                VerifiedOrdinaryPartialSnapshot,
            },
            state::CheckpointWrite,
        },
        capture_scope::{CaptureCommitDeadline, CaptureScope},
        coverage_gate::{
            CommittedTurnClaimSnapshot, LiveCaptureClaimCommitPlan, LiveClaimCommitPlan,
            NativeVersionedReasoningClaimCommitPlan, VersionedLiveClaimCommitPlan,
        },
        history::{CheckpointAppendConflict, CheckpointCompanionTransactionFailed},
        observed_agents::{
            Completeness, CoverageSchemaVersion, NormalizedTurn, RedactedBytes,
            encode_coverage_turn_for_schema,
            reasoning::{
                BoundIdentityInputRef, NativeProvenReasoning, ReasoningSourceDisposition,
                ReasoningSourceState, VerifiedCaptureBinding, VerifiedCurrentBinding,
                VerifiedImportTurnMapping, VerifiedPendingBinding, borrow_native_identity_bindings,
                partition_import_reasoning,
            },
            select_coverage_schema,
        },
        traces::{
            self, CheckpointCommitParams, CheckpointScope, TracesCommitCtx, TracesCoverageFence,
            TracesInflightMarker, TracesTxnExtra,
        },
    },
    metadata::ReasoningIdentityKind,
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
        static PENDING_MEMBER_SUBSTITUTION: Cell<bool> = const { Cell::new(false) };
    }
    #[cfg(unix)]
    pub(crate) fn fail_versioned_pending_member_once() {
        PENDING_MEMBER_SUBSTITUTION.with(|flag| flag.set(true));
    }
    pub(crate) fn take_versioned_pending_member_substitution() -> bool {
        PENDING_MEMBER_SUBSTITUTION.with(|flag| flag.replace(false))
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
pub struct CheckpointRedactedPayload {
    transcript: RedactedBytes,
    snapshot: CaptureSnapshotProjection,
    provenance: CheckpointPayloadProvenance,
    metadata_json: RedactedBytes,
    lifecycle_events_jsonl: RedactedBytes,
    redaction_report_json: RedactedBytes,
    reasoning: ReasoningSourceDisposition,
    pending_reasoning: Option<PendingReasoningReplay>,
    versioned_pending_reasoning: Option<VersionedPendingReasoningReplay>,
    versioned_pending_requirement: Option<VersionedPendingPayloadRequirement>,
    verified_partial_binding: Option<VerifiedOrdinaryPartialBinding>,
}

/// What the redacted transcript blob represents relative to its safe source
/// projection.  This prevents an import's compact per-turn JSONL line from
/// ever being mislabeled as the full authorized provider transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CheckpointPayloadProvenance {
    ExactSnapshot,
    DerivedTurnProjection,
    MetadataFallback,
    VerifiedOrdinaryPartial,
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

impl fmt::Debug for CheckpointRedactedPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CheckpointRedactedPayload")
            .field("transcript_bytes", &self.transcript.len())
            .field("complete", &self.is_exact_complete_snapshot())
            .field("reasoning_ordinary", &self.reasoning.is_ordinary())
            .finish_non_exhaustive()
    }
}

fn validate_reasoning_capture_binding(
    batch: &NativeProvenReasoning,
    capture_session: &str,
    provider: &str,
) -> Result<VerifiedCaptureBinding, CheckpointStoreError> {
    VerifiedCaptureBinding::from_validated_capture_context(batch, capture_session, provider)
        .map_err(|_| CheckpointStoreError::InvalidRequest {
            field: "reasoning capture binding".to_owned(),
        })
}

/// The only lower native-identity borrower. Later coverage/import adapters
/// consume metadata views without gaining native mint or payload authority.
#[allow(dead_code)] // OG-19/20 consume this deliberately closed preparatory port.
pub(crate) fn prepare_reasoning_identity_inputs<'a>(
    payload: &'a CheckpointRedactedPayload,
    capture_session: &str,
    provider: &str,
) -> Result<Vec<BoundIdentityInputRef<'a>>, CheckpointStoreError> {
    // Storage-only does not create Empty native identity authority for future
    // OG-19/20 consumers. The generic Unproven branch below remains closed.
    if payload.provenance == CheckpointPayloadProvenance::VerifiedOrdinaryPartial {
        payload.validate_verified_partial_capture(capture_session, provider)?;
        return Ok(Vec::new());
    }
    prepare_reasoning_identity_inputs_from_disposition(
        &payload.reasoning,
        capture_session,
        provider,
    )
}

fn prepare_reasoning_identity_inputs_from_disposition<'a>(
    reasoning: &'a ReasoningSourceDisposition,
    capture_session: &str,
    provider: &str,
) -> Result<Vec<BoundIdentityInputRef<'a>>, CheckpointStoreError> {
    match reasoning {
        ReasoningSourceDisposition::Ordinary => Ok(Vec::new()),
        ReasoningSourceDisposition::NativeProven(batch) => {
            let binding = validate_reasoning_capture_binding(batch, capture_session, provider)?;
            borrow_native_identity_bindings(batch, &binding).map_err(|_| {
                CheckpointStoreError::InvalidRequest {
                    field: "reasoning capture binding".to_owned(),
                }
            })
        }
        _ => Err(CheckpointStoreError::InvalidRequest {
            field: "reasoning source preparation".to_owned(),
        }),
    }
}

/// Validate the complete authorized import association before the importer
/// acquires an identity lease, then move each proven item into its turn's
/// private batch. The lower mapping factory has no capture or catalog access;
/// this adapter is its sole production caller.
pub(crate) fn partition_reasoning_for_import(
    batch: NativeProvenReasoning,
    capture_session: &str,
    provider: &str,
    source_session: &str,
    turns: &[NormalizedTurn],
) -> Result<BTreeMap<String, NativeProvenReasoning>, CheckpointStoreError> {
    validate_reasoning_capture_binding(&batch, capture_session, provider)?;
    let keys = turns
        .iter()
        .map(|turn| turn.logical_turn_key.as_str())
        .collect::<Vec<_>>();
    let mapping = VerifiedImportTurnMapping::from_validated_import_mapping(
        &batch,
        capture_session,
        provider,
        source_session,
        &keys,
    )
    .map_err(|_| CheckpointStoreError::InvalidRequest {
        field: "reasoning import turn association".to_owned(),
    })?;
    partition_import_reasoning(batch, &mapping).map_err(|_| CheckpointStoreError::InvalidRequest {
        field: "reasoning import turn association".to_owned(),
    })
}

/// Immutable, locally authenticated comparison inputs. Construction is owned
/// by this adapter and consumes only the sealed native metadata port above;
/// a coverage caller cannot mint one from arbitrary bytes or a native label.
#[derive(Clone)]
pub(crate) struct ReasoningCoveragePlan {
    scope: CaptureScope,
    session_id: String,
    storage_path: PathBuf,
    authorized_root: PathBuf,
    deadline: Instant,
    turns: BTreeMap<String, ReasoningTurnIdentity>,
}

/// Read authority for an ordinary/legacy capture. This can authenticate an
/// existing private map and look up a turn, but cannot assert a native source
/// or manufacture a comparable Empty/Nonempty candidate.
#[derive(Clone)]
pub(crate) struct ReasoningIdentityReadContext {
    scope: CaptureScope,
    session_id: String,
    storage_path: PathBuf,
    authorized_root: PathBuf,
    deadline: Instant,
}

impl ReasoningIdentityReadContext {
    pub(crate) fn new(
        scope: &CaptureScope,
        session_id: &str,
        storage_path: &Path,
        authorized_root: &Path,
        deadline: Instant,
    ) -> Self {
        Self {
            scope: scope.clone(),
            session_id: session_id.to_owned(),
            storage_path: storage_path.to_path_buf(),
            authorized_root: authorized_root.to_path_buf(),
            deadline,
        }
    }

    pub(crate) fn validate_for(
        &self,
        scope: &CaptureScope,
        session_id: &str,
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            &self.scope == scope && self.session_id == session_id,
            "reasoning identity reader does not match the current capture; run libra agent doctor"
        );
        anyhow::ensure!(
            Instant::now() < self.deadline,
            "reasoning coverage authentication exceeded its execution deadline"
        );
        Ok(())
    }

    pub(crate) async fn authenticate_record<C: ConnectionTrait>(
        &self,
        conn: &C,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        self.authenticate_record_for_schema_version(conn, 1, body, expected_mac)
            .await
    }

    pub(crate) async fn authenticate_record_for_schema_version<C: ConnectionTrait>(
        &self,
        conn: &C,
        coverage_schema_version: i64,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        let purpose = super::key::ReasoningIdentityMacPurpose::record_for_schema_version(
            coverage_schema_version,
        )?;
        self.authenticate_with_purpose(conn, purpose, body, expected_mac)
            .await
    }

    // COV2-14 will connect the dual-map codec to this explicit version gate.
    #[allow(dead_code)]
    pub(crate) async fn authenticate_map_for_schema_version<C: ConnectionTrait>(
        &self,
        conn: &C,
        coverage_schema_version: i64,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        let purpose = super::key::ReasoningIdentityMacPurpose::map_for_schema_version(
            coverage_schema_version,
        )?;
        self.authenticate_with_purpose(conn, purpose, body, expected_mac)
            .await
    }

    async fn authenticate_with_purpose<C: ConnectionTrait>(
        &self,
        conn: &C,
        purpose: super::key::ReasoningIdentityMacPurpose,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        self.validate_for(&self.scope, &self.session_id)?;
        super::key::authenticate_reasoning_identity_in_scope_until(
            conn,
            &self.scope,
            &self.storage_path,
            &self.authorized_root,
            super::key::ReasoningIdentityAuthentication {
                purpose,
                body,
                expected_mac,
            },
            self.deadline,
        )
        .await
    }

    pub(crate) async fn turn_slot<C: ConnectionTrait>(
        &self,
        conn: &C,
        logical_turn_key: &str,
    ) -> anyhow::Result<String> {
        self.turn_slot_for_schema_version(conn, 1, logical_turn_key)
            .await
    }

    pub(crate) async fn turn_slot_for_schema_version<C: ConnectionTrait>(
        &self,
        conn: &C,
        coverage_schema_version: i64,
        logical_turn_key: &str,
    ) -> anyhow::Result<String> {
        let purpose = super::key::ReasoningIdentityMacPurpose::descriptor_for_schema_version(
            coverage_schema_version,
        )?;
        let body = reasoning_turn_slot_body_for_schema_version(
            &self.scope,
            &self.session_id,
            logical_turn_key,
            coverage_schema_version,
        )?;
        self.authenticate_with_purpose(conn, purpose, &body, None)
            .await
    }
}

fn reasoning_identity_body_version(coverage_schema_version: i64) -> anyhow::Result<u8> {
    match coverage_schema_version {
        1 => Ok(1),
        2 => Ok(2),
        _ => {
            anyhow::bail!("unsupported private reasoning identity version; run libra agent doctor")
        }
    }
}

fn reasoning_turn_slot_body(
    scope: &CaptureScope,
    session_id: &str,
    logical_turn_key: &str,
) -> anyhow::Result<Vec<u8>> {
    reasoning_turn_slot_body_for_schema_version(scope, session_id, logical_turn_key, 1)
}

pub(crate) fn reasoning_turn_slot_body_for_schema_version(
    scope: &CaptureScope,
    session_id: &str,
    logical_turn_key: &str,
    coverage_schema_version: i64,
) -> anyhow::Result<Vec<u8>> {
    let version = reasoning_identity_body_version(coverage_schema_version)?;
    let bytes = serde_json::to_vec(&(
        "reasoning-turn-slot",
        version,
        &scope.repo_id,
        session_id,
        logical_turn_key,
    ))
    .context("cannot prepare reasoning coverage identity")?;
    anyhow::ensure!(
        bytes.len() <= traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES,
        "reasoning coverage exceeds its safe byte limit"
    );
    Ok(bytes)
}

pub(crate) fn reasoning_turn_descriptor_body_for_schema_version<E: serde::Serialize>(
    scope: &CaptureScope,
    session_id: &str,
    logical_turn_key: &str,
    entries: &E,
    coverage_schema_version: i64,
) -> anyhow::Result<Vec<u8>> {
    let version = reasoning_identity_body_version(coverage_schema_version)?;
    serde_json::to_vec(&(
        "reasoning-turn-identity",
        version,
        &scope.repo_id,
        session_id,
        logical_turn_key,
        entries,
    ))
    .context("cannot prepare reasoning coverage identity")
}

#[derive(Clone)]
pub(crate) struct ReasoningTurnIdentity {
    slot: String,
    kind: ReasoningIdentityKind,
    descriptor_mac: String,
}

impl ReasoningTurnIdentity {
    pub(crate) fn slot(&self) -> &str {
        &self.slot
    }

    pub(crate) fn kind(&self) -> ReasoningIdentityKind {
        self.kind
    }

    pub(crate) fn descriptor_mac(&self) -> &str {
        &self.descriptor_mac
    }
}

impl ReasoningCoveragePlan {
    pub(crate) fn read_context(&self) -> ReasoningIdentityReadContext {
        ReasoningIdentityReadContext::new(
            &self.scope,
            &self.session_id,
            &self.storage_path,
            &self.authorized_root,
            self.deadline,
        )
    }

    pub(crate) fn validate_for(
        &self,
        scope: &CaptureScope,
        session_id: &str,
        turns: &[NormalizedTurn],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            &self.scope == scope
                && self.session_id == session_id
                && turns
                    .iter()
                    .all(|turn| self.turns.contains_key(&turn.logical_turn_key)),
            "reasoning coverage source does not match the current capture; run libra agent doctor"
        );
        anyhow::ensure!(
            Instant::now() < self.deadline,
            "reasoning coverage authentication exceeded its execution deadline"
        );
        Ok(())
    }

    pub(crate) fn turn(&self, key: &str) -> Option<&ReasoningTurnIdentity> {
        self.turns.get(key)
    }

    pub(crate) async fn authenticate_record<C: ConnectionTrait>(
        &self,
        conn: &C,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        self.authenticate_record_for_schema_version(conn, 1, body, expected_mac)
            .await
    }

    pub(crate) async fn authenticate_record_for_schema_version<C: ConnectionTrait>(
        &self,
        conn: &C,
        coverage_schema_version: i64,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        let purpose = super::key::ReasoningIdentityMacPurpose::record_for_schema_version(
            coverage_schema_version,
        )?;
        self.authenticate_with_purpose(conn, purpose, body, expected_mac)
            .await
    }

    // COV2-14 will connect the dual-map writer to this explicit version gate.
    #[allow(dead_code)]
    pub(crate) async fn authenticate_map_for_schema_version<C: ConnectionTrait>(
        &self,
        conn: &C,
        coverage_schema_version: i64,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        let purpose = super::key::ReasoningIdentityMacPurpose::map_for_schema_version(
            coverage_schema_version,
        )?;
        self.authenticate_with_purpose(conn, purpose, body, expected_mac)
            .await
    }

    async fn authenticate_with_purpose<C: ConnectionTrait>(
        &self,
        conn: &C,
        purpose: super::key::ReasoningIdentityMacPurpose,
        body: &[u8],
        expected_mac: Option<&str>,
    ) -> anyhow::Result<String> {
        super::key::authenticate_reasoning_identity_in_scope_until(
            conn,
            &self.scope,
            &self.storage_path,
            &self.authorized_root,
            super::key::ReasoningIdentityAuthentication {
                purpose,
                body,
                expected_mac,
            },
            self.deadline,
        )
        .await
    }
}

pub(crate) struct ReasoningIdentityPreparation<'a> {
    pub(crate) conn: &'a DatabaseConnection,
    pub(crate) scope: &'a CaptureScope,
    pub(crate) storage_path: &'a Path,
    pub(crate) authorized_root: &'a Path,
    pub(crate) session_id: &'a str,
    pub(crate) provider: &'a str,
    pub(crate) turns: &'a [NormalizedTurn],
    pub(crate) deadline: Instant,
}

/// Produce the same per-turn identity for live and import. Whole-export
/// checksums, transport nonces, claim fences and checkpoint generations never
/// enter this stable content descriptor. Those fences authenticate the later
/// private record instead. Every budget is checked before any reservation.
#[cfg(all(test, unix))]
pub(crate) async fn prepare_reasoning_coverage_plan(
    payload: &CheckpointRedactedPayload,
    request: ReasoningIdentityPreparation<'_>,
) -> anyhow::Result<Option<ReasoningCoveragePlan>> {
    prepare_reasoning_coverage_plan_from_disposition(&payload.reasoning, request).await
}

/// Prepare the stable per-turn identity directly from the sealed disposition
/// emitted by the authorized snapshot boundary. The payload-free typed value
/// never crosses an ordinary serializer or metadata fallback.
pub(crate) async fn prepare_reasoning_coverage_plan_from_disposition(
    reasoning: &ReasoningSourceDisposition,
    request: ReasoningIdentityPreparation<'_>,
) -> anyhow::Result<Option<ReasoningCoveragePlan>> {
    let keys = request
        .turns
        .iter()
        .map(|turn| turn.logical_turn_key.as_str())
        .collect::<Vec<_>>();
    prepare_reasoning_coverage_plan_for_keys(
        reasoning,
        ReasoningIdentityKeyPreparation {
            conn: request.conn,
            scope: request.scope,
            storage_path: request.storage_path,
            authorized_root: request.authorized_root,
            session_id: request.session_id,
            provider: request.provider,
            turn_keys: &keys,
            allow_covered_prefix: false,
            deadline: request.deadline,
        },
    )
    .await
}

struct ReasoningIdentityKeyPreparation<'a> {
    conn: &'a DatabaseConnection,
    scope: &'a CaptureScope,
    storage_path: &'a Path,
    authorized_root: &'a Path,
    session_id: &'a str,
    provider: &'a str,
    turn_keys: &'a [&'a str],
    allow_covered_prefix: bool,
    deadline: Instant,
}

async fn prepare_reasoning_coverage_plan_for_keys(
    reasoning: &ReasoningSourceDisposition,
    request: ReasoningIdentityKeyPreparation<'_>,
) -> anyhow::Result<Option<ReasoningCoveragePlan>> {
    use sha2::{Digest, Sha256};

    use super::key::{
        ReasoningIdentityAuthentication, ReasoningIdentityMacPurpose,
        authenticate_reasoning_identity_in_scope_until,
    };

    anyhow::ensure!(
        Instant::now() < request.deadline,
        "reasoning coverage authentication exceeded its execution deadline"
    );
    let inputs = prepare_reasoning_identity_inputs_from_disposition(
        reasoning,
        request.session_id,
        request.provider,
    )?;
    if reasoning.state() == ReasoningSourceState::Ordinary {
        return Ok(None);
    }
    let artifacts = prepare_reasoning_artifact_params_from_disposition(
        reasoning,
        request.session_id,
        request.provider,
    )?;
    anyhow::ensure!(
        inputs.len() == artifacts.len()
            && inputs.len() <= traces::REASONING_ARTIFACT_MAX_ENTRIES
            && request.turn_keys.len() <= traces::REASONING_ARTIFACT_MAX_ENTRIES,
        "reasoning coverage exceeds its safe entry limit"
    );
    let turn_keys = request.turn_keys.iter().copied().collect::<BTreeSet<_>>();
    anyhow::ensure!(
        turn_keys.len() == request.turn_keys.len()
            && (request.allow_covered_prefix
                || inputs
                    .iter()
                    .all(|input| turn_keys.contains(input.logical_turn()))),
        "reasoning source cannot be associated with its logical turns; run libra agent doctor"
    );
    let mut artifact_bytes = 0u64;
    for artifact in artifacts {
        artifact_bytes = artifact_bytes
            .checked_add(artifact.bytes.len() as u64)
            .context("reasoning coverage exceeds its safe byte limit")?;
        anyhow::ensure!(
            artifact_bytes <= traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES,
            "reasoning coverage exceeds its safe byte limit"
        );
    }

    // Construct and bound every descriptor before accessing the key. Entries
    // retain source order within each turn; an appended later turn therefore
    // cannot change any earlier turn's stable commitment.
    let mut descriptors = Vec::with_capacity(request.turn_keys.len());
    let mut descriptor_bytes = 0usize;
    for turn in request.turn_keys {
        anyhow::ensure!(
            Instant::now() < request.deadline,
            "reasoning coverage authentication exceeded its execution deadline"
        );
        let entries = inputs
            .iter()
            .zip(artifacts)
            .filter(|(input, _)| input.logical_turn() == *turn)
            .map(|(input, artifact)| {
                (
                    "native_proven",
                    input.issuer_tag(),
                    hex::encode(input.source_binding_sha256()),
                    hex::encode(input.original_wire_sha256()),
                    &artifact.locator,
                    artifact.provider,
                    artifact.source_kind,
                    hex::encode(Sha256::digest(artifact.bytes.as_bytes())),
                )
            })
            .collect::<Vec<_>>();
        let kind = if entries.is_empty() {
            ReasoningIdentityKind::Empty
        } else {
            ReasoningIdentityKind::Nonempty
        };
        let descriptor = reasoning_turn_descriptor_body_for_schema_version(
            request.scope,
            request.session_id,
            turn,
            &entries,
            1,
        )?;
        let slot = reasoning_turn_slot_body(request.scope, request.session_id, turn)?;
        descriptor_bytes = descriptor_bytes
            .checked_add(descriptor.len())
            .and_then(|size| size.checked_add(slot.len()))
            .context("reasoning coverage exceeds its safe byte limit")?;
        anyhow::ensure!(
            descriptor_bytes <= traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES,
            "reasoning coverage exceeds its safe byte limit"
        );
        descriptors.push((turn, kind, descriptor, slot));
    }
    let mut turns = BTreeMap::new();
    for (key, kind, descriptor, slot) in descriptors {
        let authenticate = |body| ReasoningIdentityAuthentication {
            purpose: ReasoningIdentityMacPurpose::Descriptor,
            body,
            expected_mac: None,
        };
        let descriptor_mac = authenticate_reasoning_identity_in_scope_until(
            request.conn,
            request.scope,
            request.storage_path,
            request.authorized_root,
            authenticate(&descriptor),
            request.deadline,
        )
        .await?;
        let slot = authenticate_reasoning_identity_in_scope_until(
            request.conn,
            request.scope,
            request.storage_path,
            request.authorized_root,
            authenticate(&slot),
            request.deadline,
        )
        .await?;
        turns.insert(
            (*key).to_owned(),
            ReasoningTurnIdentity {
                slot,
                kind,
                descriptor_mac,
            },
        );
    }
    Ok(Some(ReasoningCoveragePlan {
        scope: request.scope.clone(),
        session_id: request.session_id.to_owned(),
        storage_path: request.storage_path.to_path_buf(),
        authorized_root: request.authorized_root.to_path_buf(),
        deadline: request.deadline,
        turns,
    }))
}

/// A v2 writer's source-derived identity set. Only the payload-owned factory
/// below can create this value; a read context or a version label grants no
/// producer authority. Equal logical keys remain separate across versions.
#[cfg_attr(not(all(test, unix)), allow(dead_code))]
#[derive(Clone)]
pub(crate) struct NativeVersionedReasoningCoveragePlan {
    scope: CaptureScope,
    session_id: String,
    reader: ReasoningIdentityReadContext,
    deadline: Instant,
    turns: BTreeMap<(i64, String), ReasoningTurnIdentity>,
    claims: BTreeMap<(i64, String), (String, Completeness)>,
    source_tag: [u8; 32],
}

#[cfg_attr(not(all(test, unix)), allow(dead_code))]
impl NativeVersionedReasoningCoveragePlan {
    pub(crate) fn read_context(&self) -> ReasoningIdentityReadContext {
        self.reader.clone()
    }

    pub(crate) fn turn(
        &self,
        version: CoverageSchemaVersion,
        key: &str,
    ) -> Option<&ReasoningTurnIdentity> {
        self.turns.get(&(version.as_i64(), key.to_owned()))
    }

    pub(crate) fn validate_for(
        &self,
        scope: &CaptureScope,
        session_id: &str,
        turns: &[NormalizedTurn],
    ) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.scope == *scope && self.session_id == session_id,
            "reasoning coverage source does not match the current capture; run libra agent doctor"
        );
        anyhow::ensure!(
            Instant::now() < self.deadline,
            "reasoning coverage authentication exceeded its execution deadline"
        );
        if !turns.is_empty() {
            let mut incoming = BTreeMap::new();
            for turn in turns {
                let version = select_coverage_schema(turn);
                let encoded = encode_coverage_turn_for_schema(version.as_i64(), turn)?;
                anyhow::ensure!(
                    incoming
                        .insert(
                            (version.as_i64(), turn.logical_turn_key.clone()),
                            (encoded.digest_hex, turn.completeness),
                        )
                        .is_none(),
                    "duplicate versioned reasoning coverage turn; run libra agent doctor"
                );
            }
            anyhow::ensure!(
                incoming
                    .iter()
                    .all(|(key, value)| self.claims.get(key) == Some(value)),
                "reasoning coverage source changed before reservation; run libra agent doctor"
            );
        }
        Ok(())
    }

    pub(crate) fn bind_claims(&self, plan: &VersionedLiveClaimCommitPlan) -> anyhow::Result<()> {
        self.validate_for(
            plan.ordinary()
                .capture_scope
                .as_ref()
                .context("reasoning coverage requires the current capture scope")?,
            &plan.ordinary().session_id,
            &[],
        )?;
        let mut bound = BTreeMap::new();
        for (version, claim) in plan.claims() {
            anyhow::ensure!(
                bound
                    .insert(
                        (version.as_i64(), claim.logical_turn_key.clone()),
                        (claim.coverage_digest.clone(), claim.completeness),
                    )
                    .is_none(),
                "duplicate versioned reasoning claim; run libra agent doctor"
            );
        }
        anyhow::ensure!(
            bound == self.claims,
            "reasoning coverage source does not match its reserved turns; run libra agent doctor"
        );
        Ok(())
    }

    pub(crate) fn matches_payload(
        &self,
        payload: &CheckpointRedactedPayload,
        session_id: &str,
        provider: &str,
    ) -> Result<bool, CheckpointStoreError> {
        Ok(self.source_tag
            == native_versioned_source_tag(&payload.reasoning, session_id, provider)?)
    }
}

/// Stable, non-authoritative in-memory equality tag. The native borrow and
/// capture binding are revalidated for both preparation and request binding;
/// this digest only prevents a different sealed payload from being paired
/// with the already-created identity plan.
#[cfg_attr(not(all(test, unix)), allow(dead_code))]
fn native_versioned_source_tag(
    reasoning: &ReasoningSourceDisposition,
    capture_session: &str,
    provider: &str,
) -> Result<[u8; 32], CheckpointStoreError> {
    use sha2::{Digest, Sha256};

    let ReasoningSourceDisposition::NativeProven(_) = reasoning else {
        return Err(CheckpointStoreError::InvalidRequest {
            field: "native v2 reasoning source".to_owned(),
        });
    };
    let inputs =
        prepare_reasoning_identity_inputs_from_disposition(reasoning, capture_session, provider)?;
    let artifacts =
        prepare_reasoning_artifact_params_from_disposition(reasoning, capture_session, provider)?;
    if inputs.len() != artifacts.len() || inputs.len() > traces::REASONING_ARTIFACT_MAX_ENTRIES {
        return Err(CheckpointStoreError::InvalidRequest {
            field: "native v2 reasoning source".to_owned(),
        });
    }
    let mut digest = Sha256::new();
    digest.update(b"libra/native-v2-identity-source/v1\0");
    for field in [capture_session.as_bytes(), provider.as_bytes()] {
        digest.update((field.len() as u64).to_le_bytes());
        digest.update(field);
    }
    digest.update((inputs.len() as u64).to_le_bytes());
    for (input, artifact) in inputs.iter().zip(artifacts) {
        let entry = serde_json::to_vec(&(
            input.logical_turn(),
            input.issuer_tag(),
            hex::encode(input.source_binding_sha256()),
            hex::encode(input.original_wire_sha256()),
            &artifact.locator,
            artifact.provider,
            artifact.source_kind,
            artifact.availability,
            &artifact.decrypt_capability,
            hex::encode(Sha256::digest(artifact.bytes.as_bytes())),
        ))
        .map_err(|_| CheckpointStoreError::InvalidRequest {
            field: "native v2 reasoning source".to_owned(),
        })?;
        digest.update((entry.len() as u64).to_le_bytes());
        digest.update(entry);
    }
    Ok(digest.finalize().into())
}

fn versioned_pending_replay_authentication_error(
    error: anyhow::Error,
    deadline: CaptureCommitDeadline,
    field: &'static str,
) -> CheckpointStoreError {
    if Instant::now() >= deadline.monotonic() {
        CheckpointStoreError::DeadlineExceeded
    } else if super::pending::retryable_load_failure(&error, deadline.monotonic()) {
        CheckpointStoreError::StoreFailure {
            stage: CheckpointStoreStage::CatalogTransaction,
        }
    } else {
        CheckpointStoreError::InvalidRequest {
            field: field.to_owned(),
        }
    }
}

/// Only this owner can construct restored-pending inputs, after the lower
/// fixed-source issuer returned NativeProven. Retained tuples alone cannot
/// enter this mode; ordinary callers keep their NormalizedTurn input API.
pub(crate) struct RestoredPendingVersionedIdentityPreparation<'a> {
    request: ReasoningIdentityPreparation<'a>,
    source: &'a ReasoningSourceDisposition,
    coverage: &'a VersionedLiveClaimCommitPlan,
}
pub(crate) enum NativeVersionedReasoningPreparation<'a> {
    Normalized(ReasoningIdentityPreparation<'a>),
    RestoredPending(RestoredPendingVersionedIdentityPreparation<'a>),
}
impl<'a> From<ReasoningIdentityPreparation<'a>> for NativeVersionedReasoningPreparation<'a> {
    fn from(request: ReasoningIdentityPreparation<'a>) -> Self {
        Self::Normalized(request)
    }
}

/// Derive both coverage domains from one sealed native payload. The source
/// tuple is frozen before reservations are bound; later claim ownership data
/// cannot manufacture an identity or change a digest.
#[cfg_attr(not(all(test, unix)), allow(dead_code))]
pub(crate) async fn prepare_native_versioned_reasoning_coverage_plan<'a>(
    payload: &CheckpointRedactedPayload,
    request: impl Into<NativeVersionedReasoningPreparation<'a>>,
) -> anyhow::Result<NativeVersionedReasoningCoveragePlan> {
    use sha2::{Digest, Sha256};

    use super::key::ReasoningIdentityMacPurpose;

    let (request, restored) = match request.into() {
        NativeVersionedReasoningPreparation::Normalized(request) => (request, None),
        NativeVersionedReasoningPreparation::RestoredPending(restored) => {
            (restored.request, Some((restored.source, restored.coverage)))
        }
    };
    let disposition = restored.map_or(&payload.reasoning, |(source, _)| source);
    let source_tag =
        native_versioned_source_tag(disposition, request.session_id, request.provider)?;
    let claim_count = restored.map_or(request.turns.len(), |(_, coverage)| {
        coverage.ordinary().claims.len()
    });
    anyhow::ensure!(
        claim_count > 0 && claim_count <= traces::REASONING_ARTIFACT_MAX_ENTRIES,
        "native v2 reasoning coverage exceeds its safe turn limit"
    );
    let mut prepared = Vec::with_capacity(claim_count);
    if let Some((_, coverage)) = restored {
        coverage
            .ordinary()
            .capture_scope
            .as_ref()
            .filter(|scope| *scope == request.scope)
            .context("pending reasoning coverage scope changed; run libra agent doctor")?;
        let mut bare_keys = BTreeSet::new();
        for (version, claim) in coverage.claims() {
            anyhow::ensure!(
                bare_keys.insert(claim.logical_turn_key.as_str()),
                "native pending coverage has ambiguous cross-version keys; keep the artifact and run libra agent doctor"
            );
            prepared.push((
                version,
                claim.logical_turn_key.clone(),
                claim.coverage_digest.clone(),
                claim.completeness,
            ));
        }
    } else {
        for turn in request.turns {
            let version = select_coverage_schema(turn);
            let encoded = encode_coverage_turn_for_schema(version.as_i64(), turn)?;
            prepared.push((
                version,
                turn.logical_turn_key.clone(),
                encoded.digest_hex,
                turn.completeness,
            ));
        }
    }
    let inputs = prepare_reasoning_identity_inputs_from_disposition(
        disposition,
        request.session_id,
        request.provider,
    )?;
    let artifacts = prepare_reasoning_artifact_params_from_disposition(
        disposition,
        request.session_id,
        request.provider,
    )?;
    anyhow::ensure!(
        !prepared.is_empty()
            && prepared.len() <= traces::REASONING_ARTIFACT_MAX_ENTRIES
            && inputs.len() == artifacts.len(),
        "native v2 reasoning coverage exceeds its safe turn limit"
    );
    let known_keys = prepared
        .iter()
        .map(|(_, key, _, _)| key.as_str())
        .collect::<BTreeSet<_>>();
    anyhow::ensure!(
        restored.is_some()
            || inputs
                .iter()
                .all(|input| known_keys.contains(input.logical_turn())),
        "reasoning source cannot be associated with its logical turns; run libra agent doctor"
    );
    let mut artifact_bytes = 0u64;
    for artifact in artifacts {
        artifact_bytes = artifact_bytes
            .checked_add(artifact.bytes.len() as u64)
            .context("reasoning coverage exceeds its safe byte limit")?;
        anyhow::ensure!(
            artifact_bytes <= traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES,
            "reasoning coverage exceeds its safe byte limit"
        );
    }
    let reader = ReasoningIdentityReadContext::new(
        request.scope,
        request.session_id,
        request.storage_path,
        request.authorized_root,
        request.deadline,
    );
    let mut turns = BTreeMap::new();
    let mut claims = BTreeMap::new();
    let mut descriptor_bytes = 0usize;
    let mut has_v2 = false;
    for (version, logical_turn_key, digest, completeness) in &prepared {
        anyhow::ensure!(
            Instant::now() < request.deadline,
            "reasoning coverage authentication exceeded its execution deadline"
        );
        has_v2 |= *version == CoverageSchemaVersion::V2;
        let key = (version.as_i64(), logical_turn_key.clone());
        anyhow::ensure!(
            claims
                .insert(key.clone(), (digest.clone(), *completeness))
                .is_none(),
            "duplicate versioned reasoning coverage turn; run libra agent doctor"
        );
        let entries = inputs
            .iter()
            .zip(artifacts)
            .filter(|(input, _)| input.logical_turn() == logical_turn_key)
            .map(|(input, artifact)| {
                (
                    "native_proven",
                    input.issuer_tag(),
                    hex::encode(input.source_binding_sha256()),
                    hex::encode(input.original_wire_sha256()),
                    &artifact.locator,
                    artifact.provider,
                    artifact.source_kind,
                    hex::encode(Sha256::digest(artifact.bytes.as_bytes())),
                )
            })
            .collect::<Vec<_>>();
        let kind = if entries.is_empty() {
            ReasoningIdentityKind::Empty
        } else {
            ReasoningIdentityKind::Nonempty
        };
        let descriptor = reasoning_turn_descriptor_body_for_schema_version(
            request.scope,
            request.session_id,
            logical_turn_key,
            &entries,
            version.as_i64(),
        )?;
        let slot = reasoning_turn_slot_body_for_schema_version(
            request.scope,
            request.session_id,
            logical_turn_key,
            version.as_i64(),
        )?;
        descriptor_bytes = descriptor_bytes
            .checked_add(descriptor.len())
            .and_then(|size| size.checked_add(slot.len()))
            .context("reasoning coverage exceeds its safe byte limit")?;
        anyhow::ensure!(
            descriptor_bytes <= traces::REASONING_ARTIFACT_MANIFEST_MAX_BYTES,
            "reasoning coverage exceeds its safe byte limit"
        );
        let purpose = ReasoningIdentityMacPurpose::descriptor_for_schema_version(version.as_i64())?;
        let descriptor_mac = reader
            .authenticate_with_purpose(request.conn, purpose, &descriptor, None)
            .await?;
        let slot = reader
            .authenticate_with_purpose(request.conn, purpose, &slot, None)
            .await?;
        turns.insert(
            key,
            ReasoningTurnIdentity {
                slot,
                kind,
                descriptor_mac,
            },
        );
    }
    anyhow::ensure!(
        has_v2,
        "native v2 reasoning source has no typed v2 turn; run libra agent doctor"
    );
    Ok(NativeVersionedReasoningCoveragePlan {
        scope: request.scope.clone(),
        session_id: request.session_id.to_owned(),
        reader,
        deadline: request.deadline,
        turns,
        claims,
        source_tag,
    })
}

#[cfg(all(test, unix))]
pub(crate) fn reasoning_coverage_test_payload(fields: Vec<Vec<u8>>) -> CheckpointRedactedPayload {
    use crate::internal::ai::observed_agents::reasoning::{
        carrier_test_support as fixture, synthetic_native_preparation,
    };
    let snapshot = super::snapshot::carrier_test_snapshot(&fixture::NONE, b"ordinary".to_vec());
    let metadata = serde_json::json!({ "created_at": Utc::now().timestamp() });
    let mut payload = CheckpointRedactedPayload::from_snapshot(
        snapshot,
        RedactedBytes::new_unchecked(
            serde_json::to_vec(&metadata).expect("serialize synthetic checkpoint timestamp"),
        ),
        RedactedBytes::new_unchecked(b"{}\n".to_vec()),
        RedactedBytes::new_unchecked(b"{}".to_vec()),
    )
    .expect("synthetic ordinary snapshot");
    let prepared = synthetic_native_preparation(
        b"ordinary".to_vec(),
        fixture::CAPTURE,
        fixture::SOURCE,
        fields,
    );
    payload.reasoning = ReasoningSourceDisposition::from_prepared_authorized_source(
        prepared,
        true,
        traces::REASONING_ARTIFACT_TOTAL_MAX_BYTES,
        "claude_code",
        fixture::CAPTURE,
        Some(fixture::SOURCE),
    )
    .expect("synthetic native proof validation")
    .1;
    payload
}

#[cfg(all(test, unix))]
pub(crate) fn reasoning_coverage_test_payload_for_native_turns(
    fields: Vec<(u64, Vec<u8>)>,
) -> CheckpointRedactedPayload {
    use crate::internal::ai::observed_agents::reasoning::{
        carrier_test_support as fixture, synthetic_native_batch_for_turns,
    };
    let mut payload = reasoning_coverage_test_payload(vec![b"synthetic native seed".to_vec()]);
    payload.reasoning = ReasoningSourceDisposition::NativeProven(synthetic_native_batch_for_turns(
        fixture::CAPTURE,
        fixture::SOURCE,
        fields,
    ));
    payload
}

#[cfg(all(test, unix))]
pub(crate) fn reasoning_coverage_test_native_batch(
    payload: &CheckpointRedactedPayload,
) -> &NativeProvenReasoning {
    let ReasoningSourceDisposition::NativeProven(batch) = &payload.reasoning else {
        panic!("expected a synthetic native batch")
    };
    batch
}

#[cfg(all(test, unix))]
pub(crate) fn reasoning_coverage_test_disposition(
    fields: Vec<Vec<u8>>,
) -> ReasoningSourceDisposition {
    reasoning_coverage_test_payload(fields).reasoning
}

fn prepare_reasoning_artifact_params<'a>(
    payload: &'a CheckpointRedactedPayload,
    capture_session: &str,
    provider: &str,
) -> Result<&'a [traces::ReasoningArtifactParam], CheckpointStoreError> {
    if payload.provenance == CheckpointPayloadProvenance::VerifiedOrdinaryPartial {
        payload.validate_verified_partial_capture(capture_session, provider)?;
        return Ok(&[]);
    }
    prepare_reasoning_artifact_params_from_disposition(
        &payload.reasoning,
        capture_session,
        provider,
    )
}

fn prepare_reasoning_artifact_params_from_disposition<'a>(
    reasoning: &'a ReasoningSourceDisposition,
    capture_session: &str,
    provider: &str,
) -> Result<&'a [traces::ReasoningArtifactParam], CheckpointStoreError> {
    match reasoning.state() {
        ReasoningSourceState::Ordinary => Ok(&[]),
        ReasoningSourceState::NativeProven => {
            let ReasoningSourceDisposition::NativeProven(batch) = reasoning else {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "reasoning source preparation".to_owned(),
                });
            };
            let binding = validate_reasoning_capture_binding(batch, capture_session, provider)?;
            batch
                .artifact_params(&binding)
                .map_err(|_| CheckpointStoreError::InvalidRequest {
                    field: "reasoning capture binding".to_owned(),
                })
        }
        _ => Err(CheckpointStoreError::InvalidRequest {
            field: "reasoning source preparation".to_owned(),
        }),
    }
}

/// A single checked borrow from a source-derived native payload into the
/// pending codec. The claim associations and binding cannot be recombined
/// independently by a pending writer.
pub(crate) struct PreparedPendingNativeSeal<'a> {
    pub(crate) batch: &'a NativeProvenReasoning,
    pub(crate) binding: VerifiedPendingBinding,
    pub(crate) associations: Vec<(String, ReasoningTurnIdentity)>,
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
        let (projection, transcript, reasoning) = snapshot.into_checkpoint_parts();
        let transcript = transcript.ok_or(CheckpointStoreError::IncompleteSnapshot {
            reason: projection
                .partial_reason
                .map(|reason| format!("{reason:?}"))
                .unwrap_or_else(|| "missing redacted transcript".to_string()),
        })?;
        Ok(Self {
            transcript,
            snapshot: projection,
            reasoning,
            pending_reasoning: None,
            versioned_pending_reasoning: None,
            versioned_pending_requirement: None,
            verified_partial_binding: None,
            provenance: CheckpointPayloadProvenance::ExactSnapshot,
            metadata_json,
            lifecycle_events_jsonl,
            redaction_report_json,
        })
    }

    /// Sole storage consumer of the scoped owning ordinary-partial capsule.
    /// It never relabels Unproven as Ordinary or admits native/pending bytes.
    pub(crate) fn from_verified_ordinary_partial(
        source: VerifiedOrdinaryPartialSnapshot,
        metadata_json: RedactedBytes,
        lifecycle_events_jsonl: RedactedBytes,
        redaction_report_json: RedactedBytes,
    ) -> Result<Self, CheckpointStoreError> {
        let (transcript, snapshot, binding) = source.into_payload_parts();
        let invalid = || CheckpointStoreError::InvalidRequest {
            field: "verified ordinary partial source binding".to_owned(),
        };
        let metadata =
            crate::internal::ai::observed_agents::parse_canon_value(metadata_json.bytes())
                .map_err(|_| invalid())?;
        if metadata.get("session_id").and_then(|v| v.as_str()) != Some(binding.capture_session())
            || metadata.get("provider_session_id").and_then(|v| v.as_str())
                != Some(binding.source_session())
            || metadata.get("agent_kind").and_then(|v| v.as_str()) != Some("opencode")
            || metadata.get("scope").and_then(|v| v.as_str()) != Some("committed")
        {
            return Err(invalid());
        }
        // The sidecar is already redacted, but cannot substitute a different
        // projection than the one authorized at the scoped HMAC bind.
        let value: serde_json::Value =
            serde_json::from_slice(metadata_json.bytes()).map_err(|_| invalid())?;
        let written_snapshot: CaptureSnapshotProjection = serde_json::from_value(
            value
                .get("transcript_snapshot")
                .cloned()
                .ok_or_else(invalid)?,
        )
        .map_err(|_| invalid())?;
        if written_snapshot != snapshot {
            return Err(invalid());
        }
        let payload = Self {
            transcript,
            snapshot,
            provenance: CheckpointPayloadProvenance::VerifiedOrdinaryPartial,
            metadata_json,
            lifecycle_events_jsonl,
            redaction_report_json,
            reasoning: ReasoningSourceDisposition::Unproven,
            pending_reasoning: None,
            versioned_pending_reasoning: None,
            versioned_pending_requirement: None,
            verified_partial_binding: Some(binding),
        };
        let session = payload
            .verified_partial_binding
            .as_ref()
            .ok_or_else(invalid)?
            .capture_session();
        payload.validate_verified_partial_capture(session, "opencode")?;
        Ok(payload)
    }

    fn validate_verified_partial_capture(
        &self,
        session: &str,
        provider: &str,
    ) -> Result<(), CheckpointStoreError> {
        if self.provenance != CheckpointPayloadProvenance::VerifiedOrdinaryPartial
            || !matches!(self.reasoning, ReasoningSourceDisposition::Unproven)
            || self.has_deferred_pending_reasoning()
            || !self
                .verified_partial_binding
                .as_ref()
                .is_some_and(|binding| binding.matches_capture(session, provider))
            || self.snapshot.completeness != super::snapshot::CaptureSnapshotCompleteness::Partial
            || self.snapshot.partial_reason
                != Some(super::snapshot::CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
            || self.snapshot.source.as_ref().is_none_or(|source| {
                source.kind != super::snapshot::CaptureSnapshotSourceKind::TrustedExport
            })
            || !self.snapshot.has_durable_source_commitment()
            || self.transcript.is_empty()
            || self.snapshot.transcript_redacted_bytes != self.transcript.len()
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "verified ordinary partial source binding".to_owned(),
            });
        }
        Ok(())
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
        Self::from_redacted_capture_with_reasoning(
            transcript,
            snapshot,
            metadata_json,
            lifecycle_events_jsonl,
            redaction_report_json,
            ReasoningSourceDisposition::Ordinary,
        )
    }

    /// Construct a capture payload while preserving its sealed typed
    /// reasoning disposition. Native-proven artifacts require an exact,
    /// complete snapshot with a durable source commitment; partial or
    /// unsupported dispositions cannot be downgraded to ordinary content.
    pub(crate) fn from_redacted_capture_with_reasoning(
        transcript: RedactedBytes,
        snapshot: Option<CaptureSnapshotProjection>,
        metadata_json: RedactedBytes,
        lifecycle_events_jsonl: RedactedBytes,
        redaction_report_json: RedactedBytes,
        reasoning: ReasoningSourceDisposition,
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
        if !reasoning.has_capture_consumer()
            || (reasoning.state() == ReasoningSourceState::NativeProven
                && provenance != CheckpointPayloadProvenance::ExactSnapshot)
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "reasoning source preparation".to_string(),
            });
        }
        Ok(Self {
            transcript,
            snapshot,
            provenance,
            reasoning,
            pending_reasoning: None,
            versioned_pending_reasoning: None,
            versioned_pending_requirement: None,
            verified_partial_binding: None,
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
        Self::from_derived_turn_projection_with_reasoning(
            transcript,
            source_snapshot,
            metadata_json,
            lifecycle_events_jsonl,
            redaction_report_json,
            None,
        )
    }

    /// Consume only a previously partitioned native turn batch. An empty
    /// turn keeps the same ordinary payload bytes while its separately owned
    /// coverage plan records an authenticated Empty identity on commit.
    pub(crate) fn from_derived_turn_projection_with_reasoning(
        transcript: RedactedBytes,
        source_snapshot: CaptureSnapshotProjection,
        metadata_json: RedactedBytes,
        lifecycle_events_jsonl: RedactedBytes,
        redaction_report_json: RedactedBytes,
        native: Option<NativeProvenReasoning>,
    ) -> Result<Self, CheckpointStoreError> {
        let framed_metadata = serde_json::from_slice::<serde_json::Value>(metadata_json.bytes())
            .ok()
            .and_then(|value| {
                value
                    .get("agent_kind")
                    .and_then(serde_json::Value::as_str)
                    .map(|kind| matches!(kind, "hermes" | "pi"))
            })
            .unwrap_or(false);
        // Only a Hermes or Pi ordinary snapshot, which its worker frame marks
        // partial, may keep retained bytes while partial. The reason records
        // a missing native issuer. It is not a failed read.
        let unproven_ordinary = framed_metadata
            && source_snapshot.completeness
                == crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Partial
            && source_snapshot.partial_reason
                == Some(
                    crate::internal::ai::capture::snapshot::CaptureSnapshotPartialReason::UnprovenOrdinary,
                );
        if (!unproven_ordinary
            && source_snapshot.completeness
                != crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Complete)
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
            reasoning: native.map_or(
                ReasoningSourceDisposition::Ordinary,
                ReasoningSourceDisposition::NativeProven,
            ),
            pending_reasoning: None,
            versioned_pending_reasoning: None,
            versioned_pending_requirement: None,
            verified_partial_binding: None,
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

    /// Pending V1 carries only ordinary redacted content. Refuse before
    /// sealing or MAC writes; the caller still owns this immutable payload.
    pub(crate) fn authorize_ordinary_pending_payload(&self) -> Result<(), CheckpointStoreError> {
        if self.reasoning.is_ordinary() && !self.has_deferred_pending_reasoning() {
            Ok(())
        } else {
            Err(CheckpointStoreError::InvalidRequest {
                field: "reasoning pending consumer".to_owned(),
            })
        }
    }

    /// Called only by the authenticated v2 loader. The ordinary redacted
    /// fields remain usable before late restoration; no opaque byte is
    /// materialized by this early operation.
    pub(crate) fn with_deferred_pending_reasoning(
        mut self,
        replay: PendingReasoningReplay,
    ) -> Result<Self, CheckpointStoreError> {
        if !self.reasoning.is_ordinary() || self.has_deferred_pending_reasoning() {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "deferred pending reasoning".to_owned(),
            });
        }
        self.pending_reasoning = Some(replay);
        Ok(self)
    }

    pub(crate) fn deferred_pending_reasoning(&self) -> Option<&PendingReasoningReplay> {
        self.pending_reasoning.as_ref()
    }

    pub(crate) fn has_deferred_pending_reasoning(&self) -> bool {
        self.pending_reasoning.is_some()
            || self.versioned_pending_reasoning.is_some()
            || self.versioned_pending_requirement.is_some()
    }

    pub(crate) fn with_versioned_pending_requirement(
        mut self,
        requirement: VersionedPendingPayloadRequirement,
    ) -> Result<Self, CheckpointStoreError> {
        if self.has_deferred_pending_reasoning()
            || !self.reasoning.is_ordinary()
            || !requirement.matches_payload(&self)
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned pending redacted binding".to_owned(),
            });
        }
        self.versioned_pending_requirement = Some(requirement);
        Ok(self)
    }

    pub(crate) fn with_deferred_versioned_pending_reasoning(
        mut self,
        replay: VersionedPendingReasoningReplay,
    ) -> Result<Self, CheckpointStoreError> {
        if !self.reasoning.is_ordinary()
            || self.pending_reasoning.is_some()
            || self.versioned_pending_reasoning.is_some()
            || !self
                .versioned_pending_requirement
                .as_ref()
                .is_some_and(|requirement| {
                    requirement.matches_payload(&self)
                        && requirement.matches_original(replay.original_envelope())
                })
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "deferred versioned pending reasoning".to_owned(),
            });
        }
        self.versioned_pending_reasoning = Some(replay);
        Ok(self)
    }

    /// This is the sole production factory for a pending native borrow. It
    /// checks the exact reserved claim subset against the already prepared
    /// source-derived identity plan before the lower sealer sees the witness.
    pub(crate) fn prepare_pending_native_seal(
        &self,
        scope: &CaptureScope,
        coverage: &LiveClaimCommitPlan,
        identity: &ReasoningCoveragePlan,
        provider: &str,
    ) -> Result<PreparedPendingNativeSeal<'_>, CheckpointStoreError> {
        let ReasoningSourceDisposition::NativeProven(batch) = &self.reasoning else {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending native source".to_owned(),
            });
        };
        identity
            .validate_for(scope, &coverage.session_id, &[])
            .map_err(|_| CheckpointStoreError::InvalidRequest {
                field: "pending native identity plan".to_owned(),
            })?;
        if coverage.claims.is_empty()
            || batch
                .pending_logical_turns()
                .any(|key| identity.turn(key).is_none())
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending native claim association".to_owned(),
            });
        }
        let associations = coverage
            .claims
            .iter()
            .map(|claim| {
                identity
                    .turn(&claim.logical_turn_key)
                    .cloned()
                    .map(|turn| (claim.logical_turn_key.clone(), turn))
                    .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                        field: "pending native claim association".to_owned(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let binding = VerifiedPendingBinding::from_validated_pending_context(
            batch,
            &coverage.session_id,
            provider,
        )
        .map_err(|_| CheckpointStoreError::InvalidRequest {
            field: "pending native source binding".to_owned(),
        })?;
        Ok(PreparedPendingNativeSeal {
            batch,
            binding,
            associations,
        })
    }

    /// Borrow the versioned identity set from this exact sealed native source.
    /// The pending projection still uses bare logical keys, so a native key
    /// shared by two schema versions cannot be represented safely yet.
    #[cfg_attr(not(all(test, unix)), allow(dead_code))]
    pub(crate) fn prepare_pending_native_versioned_seal(
        &self,
        scope: &CaptureScope,
        companion: &NativeVersionedReasoningClaimCommitPlan,
        provider: &str,
    ) -> Result<PreparedPendingNativeSeal<'_>, CheckpointStoreError> {
        let ReasoningSourceDisposition::NativeProven(batch) = &self.reasoning else {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending native v2 source".to_owned(),
            });
        };
        let versioned = companion.versioned();
        let ordinary = versioned.ordinary();
        let claim_count = versioned.claims().count();
        if claim_count == 0 || claim_count > traces::REASONING_ARTIFACT_MAX_ENTRIES {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending native v2 claim limit".to_owned(),
            });
        }
        if ordinary.capture_scope.as_ref() != Some(scope)
            || !companion
                .reasoning()
                .matches_payload(self, &ordinary.session_id, provider)?
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending native v2 source binding".to_owned(),
            });
        }
        companion.reasoning().bind_claims(versioned).map_err(|_| {
            CheckpointStoreError::InvalidRequest {
                field: "pending native v2 claim binding".to_owned(),
            }
        })?;

        // Check every bare key before allocating the association vector. The
        // old inner projection cannot distinguish (v1,key) from (v2,key).
        let mut bare_keys = BTreeSet::new();
        for (_, claim) in versioned.claims() {
            if !bare_keys.insert(claim.logical_turn_key.as_str()) {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "pending native v2 bare key alias".to_owned(),
                });
            }
        }
        if batch
            .pending_logical_turns()
            .any(|key| !bare_keys.contains(key))
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending native v2 claim association".to_owned(),
            });
        }
        let associations = versioned
            .claims()
            .map(|(version, claim)| {
                companion
                    .reasoning()
                    .turn(version, &claim.logical_turn_key)
                    .cloned()
                    .map(|identity| (claim.logical_turn_key.clone(), identity))
                    .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                        field: "pending native v2 claim association".to_owned(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let binding = VerifiedPendingBinding::from_validated_pending_context(
            batch,
            &ordinary.session_id,
            provider,
        )
        .map_err(|_| CheckpointStoreError::InvalidRequest {
            field: "pending native v2 source binding".to_owned(),
        })?;
        Ok(PreparedPendingNativeSeal {
            batch,
            binding,
            associations,
        })
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
    /// Native-v2 source and claims are carried as data until the concrete
    /// store has checked the exact marker fences and registered its attempt.
    native_versioned_companion: Option<&'a NativeVersionedReasoningClaimCommitPlan>,
    /// The same closed per-claim binding supplied as the transaction extra.
    /// A versioned request cannot substitute an unrelated companion.
    versioned_coverage: Option<&'a VersionedLiveClaimCommitPlan>,
    /// Set exclusively by the typed import adapter after its attempt binding.
    imported_versioned_prebound: bool,
    /// Set only by authenticated pending recovery after the retained claim
    /// plan was checked against the original envelope and catalog fences.
    pending_coverage: Option<&'a LiveClaimCommitPlan>,
    pending_versioned_header: Option<&'a super::pending::PendingHeader>,
    pending_key_storage: Option<&'a Path>,
    pending_authorized_root: Option<&'a Path>,
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
        if txn_extra.is_some_and(|extra| extra.requires_versioned_coverage_binding()) {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned coverage claim plan".to_owned(),
            });
        }
        if payload.provenance == CheckpointPayloadProvenance::VerifiedOrdinaryPartial
            && (scope != CheckpointScope::Committed || txn_extra.is_none())
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "verified ordinary partial claim barrier".to_owned(),
            });
        }
        prepare_reasoning_artifact_params(payload, session_id, agent_kind)?;
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
            native_versioned_companion: None,
            versioned_coverage: None,
            imported_versioned_prebound: false,
            pending_coverage: None,
            pending_versioned_header: None,
            pending_key_storage: None,
            pending_authorized_root: None,
            deadline,
        })
    }

    /// Carry the exact retained claim plan into the late, store-owned
    /// preflight. This is a data association, not a current-fence capability;
    /// the terminal catalog fence and marker registration must still pass.
    pub(crate) fn with_pending_coverage(
        mut self,
        coverage: &'a LiveClaimCommitPlan,
        key_storage: &'a Path,
        authorized_root: &'a Path,
    ) -> Result<Self, CheckpointStoreError> {
        if self.payload.deferred_pending_reasoning().is_none()
            || self.scope != CheckpointScope::Committed
            || coverage.source_channel != "live"
            || coverage.session_id != self.session_id
            || coverage.checkpoint_id != self.checkpoint_id
            || coverage.parent_commit.as_deref() != self.parent_commit
            || coverage.capture_scope.is_none()
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending reasoning claim plan".to_owned(),
            });
        }
        self.pending_coverage = Some(coverage);
        self.pending_key_storage = Some(key_storage);
        self.pending_authorized_root = Some(authorized_root);
        Ok(self)
    }

    pub(crate) fn with_versioned_pending_coverage(
        mut self,
        coverage: &'a VersionedLiveClaimCommitPlan,
        header: &'a super::pending::PendingHeader,
        key_storage: &'a Path,
        authorized_root: &'a Path,
    ) -> Result<Self, CheckpointStoreError> {
        let ordinary = coverage.ordinary();
        if self.payload.versioned_pending_reasoning.is_none()
            || self.payload.pending_reasoning.is_some()
            || self.txn_extra.is_some()
            || self.native_versioned_companion.is_some()
            || self.pending_coverage.is_some()
            || self.versioned_coverage.is_some()
            || self.scope != CheckpointScope::Committed
            || ordinary.source_channel != "live"
            || ordinary.session_id != self.session_id
            || ordinary.checkpoint_id != self.checkpoint_id
            || ordinary.parent_commit.as_deref() != self.parent_commit
            || ordinary.capture_scope.as_ref() != Some(&header.binding.scope)
            || header.binding.checkpoint_id != self.checkpoint_id
            || header.binding.action_key != self.replay_key
            || header.binding.marker_generation != self.marker_generation
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned pending reasoning claim plan".to_owned(),
            });
        }
        self.versioned_coverage = Some(coverage);
        self.pending_coverage = Some(ordinary);
        self.pending_versioned_header = Some(header);
        self.pending_key_storage = Some(key_storage);
        self.pending_authorized_root = Some(authorized_root);
        Ok(self)
    }

    /// Bind the immutable version sidecar as both replay expectation and the
    /// ref-CAS companion. The store separately matches it to its marker fences.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn with_versioned_claims(
        mut self,
        plan: &'a VersionedLiveClaimCommitPlan,
    ) -> Result<Self, CheckpointStoreError> {
        let ordinary = plan.ordinary();
        if self.txn_extra.is_some()
            || self.native_versioned_companion.is_some()
            || self.pending_coverage.is_some()
            || !self.payload.reasoning.is_ordinary()
            || self.payload.has_deferred_pending_reasoning()
            || ordinary.claims.is_empty()
            || ordinary.session_id != self.session_id
            || ordinary.checkpoint_id != self.checkpoint_id
            || ordinary.parent_commit.as_deref() != self.parent_commit
            || self.scope != CheckpointScope::Committed
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned coverage claim plan".to_owned(),
            });
        }
        self.txn_extra = Some(plan);
        self.versioned_coverage = Some(plan);
        Ok(self)
    }

    /// Bind the actual live/export transaction companion to its exact
    /// versioned marker/replay fences. Its claim/catalog writes apply once.
    pub(crate) fn with_runtime_versioned_claims(
        mut self,
        companion: &'a LiveCaptureClaimCommitPlan,
    ) -> Result<Self, CheckpointStoreError> {
        let plan =
            companion
                .runtime_versioned()
                .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                    field: "runtime versioned coverage claim plan".to_owned(),
                })?;
        let ordinary = plan.ordinary();
        if self.txn_extra.is_some()
            || self.native_versioned_companion.is_some()
            || self.versioned_coverage.is_some()
            || self.pending_coverage.is_some()
            || !self.payload.reasoning.is_ordinary()
            || self.payload.has_deferred_pending_reasoning()
            || ordinary.claims.is_empty()
            || ordinary.session_id != self.session_id
            || ordinary.checkpoint_id != self.checkpoint_id
            || ordinary.parent_commit.as_deref() != self.parent_commit
            || ordinary.capture_scope.is_none()
            || !matches!(ordinary.source_channel, "live" | "export")
            || self.scope != CheckpointScope::Committed
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "runtime versioned coverage claim plan".to_owned(),
            });
        }
        self.txn_extra = Some(companion);
        self.versioned_coverage = Some(plan);
        Ok(self)
    }

    /// Pair a sealed native v2 source, its exact versioned claims and the
    /// transaction companion in one request. The ordinary constructor alone
    /// cannot carry a versioned companion past marker/replay fences.
    #[cfg_attr(not(all(test, unix)), allow(dead_code))]
    pub(crate) fn with_native_versioned_claims(
        mut self,
        companion: &'a NativeVersionedReasoningClaimCommitPlan,
    ) -> Result<Self, CheckpointStoreError> {
        let ordinary = companion.versioned().ordinary();
        let matches_source = companion.reasoning().matches_payload(
            self.payload,
            self.session_id,
            self.agent_kind,
        )?;
        if self.txn_extra.is_some()
            || self.native_versioned_companion.is_some()
            || self.versioned_coverage.is_some()
            || self.pending_coverage.is_some()
            || self.payload.has_deferred_pending_reasoning()
            || !matches_source
            || ordinary.claims.is_empty()
            || ordinary.session_id != self.session_id
            || ordinary.checkpoint_id != self.checkpoint_id
            || ordinary.parent_commit.as_deref() != self.parent_commit
            || self.scope != CheckpointScope::Committed
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "native v2 reasoning claim plan".to_owned(),
            });
        }
        self.native_versioned_companion = Some(companion);
        self.versioned_coverage = Some(companion.versioned());
        Ok(self)
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

    #[allow(dead_code)] // OG-21 late restore pairs this with a lower current binding.
    pub(crate) fn pending_coverage(&self) -> Option<&LiveClaimCommitPlan> {
        self.pending_coverage
    }
}

/// Constructible only by this checkpoint store module after the versioned
/// marker has been registered. A crate sibling cannot call the companion's
/// low-level transaction method without this capability.
pub(crate) struct StoreBoundNativeVersionedPermit {
    _private: (),
}

/// Only the concrete store constructs this adapter after the versioned
/// registration matched its request and the marker was registered. Keeping
/// the native companion itself out of `TracesTxnExtra` prevents a direct
/// HistoryManager append from skipping those store-owned fences.
struct StoreBoundNativeVersionedTxnExtra<'a> {
    companion: &'a NativeVersionedReasoningClaimCommitPlan,
    permit: StoreBoundNativeVersionedPermit,
}

#[async_trait]
impl TracesTxnExtra for StoreBoundNativeVersionedTxnExtra<'_> {
    async fn apply(&self, txn: &DatabaseTransaction, ctx: &TracesCommitCtx) -> anyhow::Result<()> {
        self.companion
            .apply_store_bound(&self.permit, txn, ctx)
            .await
    }

    fn requires_versioned_coverage_binding(&self) -> bool {
        true
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
    versioned_coverage: Option<&'a VersionedLiveClaimCommitPlan>,
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
            versioned_coverage: None,
            deadline,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new_versioned(
        checkpoint_id: &'a str,
        session_id: &'a str,
        agent_kind: &'a str,
        parent_commit: Option<&'a str>,
        payload: &'a CheckpointRedactedPayload,
        plan: &'a VersionedLiveClaimCommitPlan,
        deadline: CaptureCommitDeadline,
    ) -> Result<Self, CheckpointStoreError> {
        let ordinary = plan.ordinary();
        if ordinary.source_channel != "import"
            || ordinary.session_id != session_id
            || ordinary.checkpoint_id != checkpoint_id
            || ordinary.parent_commit.as_deref() != parent_commit
            || ordinary.capture_scope.is_none()
            || ordinary.import_identity.is_none()
            || ordinary.import_session.is_none()
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned import checkpoint companion".to_owned(),
            });
        }
        let mut request = Self::new(
            checkpoint_id,
            session_id,
            agent_kind,
            parent_commit,
            payload,
            plan,
            deadline,
        )?;
        request.versioned_coverage = Some(plan);
        Ok(request)
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
        // A killed import binds the marker, and may record object ids, before
        // the checkpoint row commits. Promoting that row to cleanup_pending
        // leaves an in-flight marker after the retry has committed every turn.
        // Nothing references those objects, so drop the marker. A checkpoint
        // that did commit keeps the shared cleanup-pending rule.
        let committed = txn
            .query_one_raw(Statement::from_sql_and_values(
                txn.get_database_backend(),
                "SELECT 1 FROM agent_checkpoint \
                 WHERE checkpoint_id = ? AND session_id = ? AND scope = 'committed'",
                [checkpoint_id.into(), session_id.into()],
            ))
            .await
            .map_err(|_| CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::Cleanup,
            })?;
        if committed.is_none() {
            traces::clear_traces_inflight_marker(txn, session_id, checkpoint_id)
                .await
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::Cleanup,
                })?;
            return Ok(());
        }
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
        let versioned = request.versioned_coverage;
        let mut request = CheckpointWriteRequest::new(
            request.checkpoint_id,
            request.checkpoint_id,
            request.session_id,
            request.agent_kind,
            request.parent_commit,
            CheckpointScope::Committed,
            self.store.marker_generation(),
            None,
            request.payload,
            if versioned.is_some() {
                None
            } else {
                Some(request.txn_extra)
            },
            Some(request.deadline),
        )?;
        if let Some(plan) = versioned {
            request = request.with_versioned_claims(plan)?;
            request.imported_versioned_prebound = true;
        }
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

struct CheckpointReplayExpectation<'a, 'fence> {
    claim_set: Option<&'a [TracesCoverageFence<'fence>]>,
    native_companion: Option<&'a NativeVersionedReasoningClaimCommitPlan>,
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
    coverage_schema_version: crate::internal::ai::observed_agents::CoverageSchemaVersion,
    logical_turn_key: String,
    coverage_digest: String,
    completeness: crate::internal::ai::observed_agents::Completeness,
    next_revision: i64,
    owner: String,
    fence_token: i64,
    reservation_state: String,
}

struct RestoredPendingReasoning {
    artifacts: Vec<traces::ReasoningArtifactParam>,
    commit_plan: RestoredPendingClaimPlan,
}
enum RestoredPendingClaimPlan {
    V1(Box<LiveCaptureClaimCommitPlan>),
    V2(Box<NativeVersionedReasoningClaimCommitPlan>),
}

impl OwnedRegistrationFence {
    fn borrowed(&self) -> TracesCoverageFence<'_> {
        TracesCoverageFence {
            coverage_schema_version: self.coverage_schema_version,
            logical_turn_key: &self.logical_turn_key,
            coverage_digest: &self.coverage_digest,
            completeness: self.completeness,
            next_revision: self.next_revision,
            owner: &self.owner,
            fence_token: self.fence_token,
            reservation_state: &self.reservation_state,
        }
    }
}

fn registration_matches_pending_claims(
    registration: &[OwnedRegistrationFence],
    coverage: &LiveClaimCommitPlan,
) -> bool {
    let mut seen = BTreeSet::new();
    !coverage.owner.is_empty()
        && !coverage.claims.is_empty()
        && registration.len() == coverage.claims.len()
        && registration
            .iter()
            .zip(&coverage.claims)
            .all(|(fence, claim)| {
                !claim.logical_turn_key.is_empty()
                    && claim.fence_token > 0
                    && seen.insert(claim.logical_turn_key.as_str())
                    && fence.coverage_schema_version
                        == crate::internal::ai::observed_agents::CoverageSchemaVersion::V1
                    && fence.logical_turn_key == claim.logical_turn_key
                    && fence.coverage_digest == claim.coverage_digest
                    && fence.completeness == claim.completeness
                    && fence.next_revision == claim.next_revision
                    && fence.owner == coverage.owner
                    && fence.fence_token == claim.fence_token
                    && fence.reservation_state == "reserved_live"
            })
}

fn registration_matches_versioned_claims(
    registration: &[OwnedRegistrationFence],
    plan: &VersionedLiveClaimCommitPlan,
) -> bool {
    let ordinary = plan.ordinary();
    let expected_state = match ordinary.source_channel {
        "live" | "export" => "reserved_live",
        "import" => "reserved_import",
        _ => return false,
    };
    !ordinary.owner.is_empty()
        && !ordinary.claims.is_empty()
        && registration.len() == ordinary.claims.len()
        && registration
            .iter()
            .zip(plan.claims())
            .all(|(fence, (version, claim))| {
                fence.coverage_schema_version == version
                    && fence.logical_turn_key == claim.logical_turn_key
                    && fence.coverage_digest == claim.coverage_digest
                    && fence.completeness == claim.completeness
                    && fence.next_revision == claim.next_revision
                    && fence.owner == ordinary.owner
                    && fence.fence_token == claim.fence_token
                    && fence.reservation_state == expected_state
            })
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
                        coverage_schema_version: fence.coverage_schema_version,
                        logical_turn_key: fence.logical_turn_key.to_string(),
                        coverage_digest: fence.coverage_digest.to_string(),
                        completeness: fence.completeness,
                        next_revision: fence.next_revision,
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
        let mut store = Self::new_recovery_base(
            conn,
            repo_path,
            scope,
            session_id,
            &checkpoint_id,
            generation,
            registration_fences,
        )?;
        store.terminal_attempt_fence = Some(terminal_attempt_fence);
        Ok(store)
    }

    fn new_recovery_base(
        conn: &'a DatabaseConnection,
        repo_path: impl AsRef<Path>,
        scope: &CaptureScope,
        session_id: &str,
        checkpoint_id: &str,
        generation: &str,
        registration_fences: &[TracesCoverageFence<'_>],
    ) -> Result<Self, CheckpointStoreError> {
        let mut store = Self::new_with_persisted_marker_generation(
            conn,
            repo_path,
            session_id,
            checkpoint_id,
            generation,
            registration_fences,
        )?;
        store.storage_mode = CheckpointStorageMode::LocalExisting;
        store.capture_scope = Some(scope.clone());
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
        if let Some(binding) = &request.payload.verified_partial_binding
            && (!binding.matches_scope(self.capture_scope.as_ref())
                || !binding.matches_capture(request.session_id(), request.agent_kind))
        {
            return Err(CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ScopeFence,
            });
        }
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

    /// Narrow late-store adapter for pending reasoning. It requires the
    /// catalog-elected terminal attempt and the exact claims whose authority
    /// marker registration just revalidated. This returns no native proof;
    /// OG-21's lower issuer consumes it only after registration succeeds.
    fn prepare_current_pending_reasoning_fence(
        &self,
        request: &CheckpointWriteRequest<'_>,
    ) -> Result<VerifiedCurrentBinding, CheckpointStoreError> {
        let coverage =
            request
                .pending_coverage()
                .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                    field: "pending reasoning claim plan".to_owned(),
                })?;
        let scope =
            self.capture_scope
                .as_ref()
                .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                    field: "pending reasoning scope".to_owned(),
                })?;
        let fence = self.terminal_attempt_fence.as_ref().ok_or_else(|| {
            CheckpointStoreError::InvalidRequest {
                field: "pending reasoning terminal fence".to_owned(),
            }
        })?;
        let MarkerRegistration::Required(registration) = &self.registration else {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending reasoning marker registration".to_owned(),
            });
        };
        if self.storage_mode != CheckpointStorageMode::LocalExisting
            || request.scope() != CheckpointScope::Committed
            || coverage.capture_scope.as_ref() != Some(scope)
            || !request.payload().has_deferred_pending_reasoning()
            || request.pending_key_storage.is_none()
            || request.pending_authorized_root.is_none()
            || !match request.versioned_coverage {
                Some(plan) if request.pending_versioned_header.is_some() => {
                    registration_matches_versioned_claims(registration, plan)
                }
                None => registration_matches_pending_claims(registration, coverage),
                _ => false,
            }
            || self.request_matches_attempt(request).is_err()
            || !fence.matches_checkpoint_attempt(
                Some(scope),
                request.session_id(),
                request.checkpoint_id(),
                request.marker_generation(),
            )
            || fence.source_commitment()
                != request
                    .payload()
                    .snapshot()
                    .source
                    .as_ref()
                    .and_then(|source| source.digest_sha256.as_deref())
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending reasoning current binding".to_owned(),
            });
        }
        VerifiedCurrentBinding::from_validated_current_context(
            request.session_id(),
            request.agent_kind(),
        )
        .map_err(|_| CheckpointStoreError::InvalidRequest {
            field: "pending reasoning current source".to_owned(),
        })
    }

    async fn restore_pending_reasoning(
        &self,
        request: &CheckpointWriteRequest<'_>,
        current: &VerifiedCurrentBinding,
    ) -> Result<RestoredPendingReasoning, CheckpointStoreError> {
        let scope =
            self.capture_scope
                .as_ref()
                .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                    field: "pending reasoning scope".to_owned(),
                })?;
        let coverage =
            request
                .pending_coverage()
                .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                    field: "pending reasoning claim plan".to_owned(),
                })?;
        let legacy = request.payload().deferred_pending_reasoning();
        let versioned = request.payload().versioned_pending_reasoning.as_ref();
        let (bytes, mac, source) = match (legacy, versioned) {
            (Some(replay), None) => (
                replay.original_envelope(),
                replay.original_mac(),
                replay.source(),
            ),
            (None, Some(replay)) => (
                replay.original_envelope(),
                replay.original_mac(),
                replay.source(),
            ),
            _ => {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "pending reasoning original envelope".to_owned(),
                });
            }
        };
        let storage =
            request
                .pending_key_storage
                .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                    field: "pending reasoning private key storage".to_owned(),
                })?;
        let root = request.pending_authorized_root.ok_or_else(|| {
            CheckpointStoreError::InvalidRequest {
                field: "pending reasoning authorized root".to_owned(),
            }
        })?;
        let deadline = request
            .deadline()
            .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                field: "pending reasoning replay deadline".to_owned(),
            })?;
        let integrity = if let Some(replay) = versioned {
            let header = request.pending_versioned_header.ok_or_else(|| {
                CheckpointStoreError::InvalidRequest {
                    field: "versioned pending header".to_owned(),
                }
            })?;
            let authentication = AuthenticatedPendingEnvelope::verify(
                self.conn,
                storage,
                root,
                scope,
                bytes,
                mac,
                deadline.monotonic(),
            )
            .await
            .map_err(|error| {
                versioned_pending_replay_authentication_error(
                    error,
                    deadline,
                    "versioned pending original authentication",
                )
            })?;
            let candidate = super::pending::decode_authenticated_versioned_envelope(
                &authentication,
                &header.binding,
            )
            .map_err(|_| CheckpointStoreError::InvalidRequest {
                field: "versioned pending original members".to_owned(),
            })?;
            // A test-only substitution proves the late key-owner verifier
            // compares against original authenticated members after registration.
            #[cfg(test)]
            let candidate = {
                let mut candidate = candidate;
                if test_support::take_versioned_pending_member_substitution() {
                    candidate.binding.marker_generation.push_str("-substituted");
                }
                candidate
            };
            let token = key::authenticate_pending_versioned_reasoning_members_in_scope_until(
                self.conn,
                key::PendingVersionedReasoningAuthentication {
                    scope,
                    storage_path: storage,
                    authorized_root: root,
                    original_envelope: bytes,
                    original_mac: mac,
                    expected_binding: &header.binding,
                    candidate: &candidate,
                    deadline: deadline.monotonic(),
                },
            )
            .await
            .map_err(|error| {
                versioned_pending_replay_authentication_error(
                    error,
                    deadline,
                    "versioned pending late original authentication",
                )
            })?;
            if !replay.matches_integrity(&token) {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "versioned pending retained membership".to_owned(),
                });
            }
            token
        } else {
            let token = key::authenticate_pending_reasoning_original_in_scope_until(
                self.conn,
                scope,
                storage,
                root,
                bytes,
                mac,
                deadline.monotonic(),
            )
            .await
            .map_err(|_| CheckpointStoreError::InvalidRequest {
                field: "pending reasoning original authentication".to_owned(),
            })?;
            if !legacy.is_some_and(|replay| replay.matches_integrity(&token)) {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "pending reasoning retained membership".to_owned(),
                });
            }
            token
        };
        let native = crate::internal::ai::observed_agents::reasoning::restore_pending(
            source, integrity, current,
        )
        .map_err(|_| CheckpointStoreError::InvalidRequest {
            field: "pending reasoning fixed native source proof".to_owned(),
        })?;
        let disposition = ReasoningSourceDisposition::NativeProven(native);
        if let Some(replay) = versioned {
            let versioned_coverage =
                request
                    .versioned_coverage
                    .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                        field: "versioned pending claims".to_owned(),
                    })?;
            let identity = prepare_native_versioned_reasoning_coverage_plan(
                request.payload(),
                NativeVersionedReasoningPreparation::RestoredPending(
                    RestoredPendingVersionedIdentityPreparation {
                        request: ReasoningIdentityPreparation {
                            conn: self.conn,
                            scope,
                            storage_path: storage,
                            authorized_root: root,
                            session_id: request.session_id(),
                            provider: request.agent_kind(),
                            turns: &[],
                            deadline: deadline.monotonic(),
                        },
                        source: &disposition,
                        coverage: versioned_coverage,
                    },
                ),
            )
            .await
            .map_err(|error| {
                versioned_pending_replay_authentication_error(
                    error,
                    deadline,
                    "pending versioned stable identity",
                )
            })?;
            replay
                .verify_recomputed_associations(versioned_coverage, &identity)
                .map_err(|_| CheckpointStoreError::InvalidRequest {
                    field: "pending versioned identity association".to_owned(),
                })?;
            let ReasoningSourceDisposition::NativeProven(native) = disposition else {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "pending versioned restored source".to_owned(),
                });
            };
            let keys = versioned_coverage
                .claims()
                .map(|(_, claim)| claim.logical_turn_key.clone())
                .collect::<BTreeSet<_>>();
            let artifacts = native
                .into_pending_claim_artifacts(current, &keys)
                .map_err(|_| CheckpointStoreError::InvalidRequest {
                    field: "pending versioned artifact bounds".to_owned(),
                })?;
            let commit_plan =
                NativeVersionedReasoningClaimCommitPlan::new(versioned_coverage.clone(), &identity)
                    .map_err(|_| CheckpointStoreError::InvalidRequest {
                        field: "pending versioned atomic identity commit".to_owned(),
                    })?;
            return Ok(RestoredPendingReasoning {
                artifacts,
                commit_plan: RestoredPendingClaimPlan::V2(Box::new(commit_plan)),
            });
        }
        let keys = coverage
            .claims
            .iter()
            .map(|claim| claim.logical_turn_key.as_str())
            .collect::<Vec<_>>();
        let identity = prepare_reasoning_coverage_plan_for_keys(
            &disposition,
            ReasoningIdentityKeyPreparation {
                conn: self.conn,
                scope,
                storage_path: storage,
                authorized_root: root,
                session_id: request.session_id(),
                provider: request.agent_kind(),
                turn_keys: &keys,
                allow_covered_prefix: true,
                deadline: deadline.monotonic(),
            },
        )
        .await
        .map_err(|_| CheckpointStoreError::InvalidRequest {
            field: "pending reasoning stable identity".to_owned(),
        })?
        .ok_or_else(|| CheckpointStoreError::InvalidRequest {
            field: "pending reasoning stable identity".to_owned(),
        })?;
        legacy
            .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                field: "pending legacy source".to_owned(),
            })?
            .verify_recomputed_associations(coverage, &identity)
            .map_err(|_| CheckpointStoreError::InvalidRequest {
                field: "pending reasoning identity association".to_owned(),
            })?;
        let ReasoningSourceDisposition::NativeProven(native) = disposition else {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending reasoning restored source".to_owned(),
            });
        };
        let claimed_keys = keys.into_iter().map(str::to_owned).collect::<BTreeSet<_>>();
        let artifacts = native
            .into_pending_claim_artifacts(current, &claimed_keys)
            .map_err(|_| CheckpointStoreError::InvalidRequest {
                field: "pending reasoning artifact bounds".to_owned(),
            })?;
        let commit_plan = LiveCaptureClaimCommitPlan::new(coverage.clone(), Some(&identity))
            .map_err(|_| CheckpointStoreError::InvalidRequest {
                field: "pending reasoning atomic identity commit".to_owned(),
            })?;
        Ok(RestoredPendingReasoning {
            artifacts,
            commit_plan: RestoredPendingClaimPlan::V1(Box::new(commit_plan)),
        })
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

    /// Detect the read-only same-action upgrade boundary before version
    /// selection. Unknown versions and database errors are never legacy.
    /// The checkpoint point-read and revision probe are indexed separately.
    pub(crate) async fn durable_legacy_replay_exists_for(
        conn: &DatabaseConnection,
        capture_scope: &CaptureScope,
        checkpoint_id: &str,
        session_id: &str,
        deadline: Option<CaptureCommitDeadline>,
    ) -> Result<bool, CheckpointStoreError> {
        await_checkpoint_precommit_read_until(deadline, async {
            capture_scope
                .assert_workspace_fence_live(conn)
                .await
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::ScopeFence,
                })
        })
        .await?;
        if !Self::durable_replay_exists_for(
            conn,
            checkpoint_id,
            session_id,
            CheckpointScope::Committed,
            deadline,
        )
        .await?
        {
            return Ok(false);
        }
        let non_v1 = await_checkpoint_precommit_read_until(deadline, async {
            conn.query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT 1 FROM agent_coverage_revision
                 WHERE checkpoint_id = ? AND coverage_schema_version <> 1 LIMIT 1",
                [checkpoint_id.into()],
            ))
            .await
            .map_err(|_| CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::CatalogTransaction,
            })
        })
        .await?;
        Ok(non_v1.is_none())
    }

    /// Accept only gate-owned committed observations. A v2 replay binds all
    /// this action's observed v1/v2 rows as one exact immutable revision set;
    /// unrelated checkpoints cannot supply authority for this action.
    pub(crate) async fn settle_runtime_covered_replay_without_write(
        conn: &DatabaseConnection,
        capture_scope: &CaptureScope,
        checkpoint_id: &str,
        session_id: &str,
        deadline: Option<CaptureCommitDeadline>,
        covered: &[CommittedTurnClaimSnapshot],
    ) -> Result<CheckpointReplayStatus, CheckpointStoreError> {
        let action_claims = covered
            .iter()
            .filter(|claim| claim.checkpoint_id() == checkpoint_id)
            .collect::<Vec<_>>();
        if !action_claims
            .iter()
            .any(|claim| claim.version() == CoverageSchemaVersion::V2)
        {
            return Self::settle_committed_replay_without_write(
                conn,
                capture_scope,
                checkpoint_id,
                session_id,
                deadline,
            )
            .await;
        }
        await_checkpoint_precommit_read_until(deadline, async {
            capture_scope
                .assert_workspace_fence_live(conn)
                .await
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::ScopeFence,
                })?;
            for claim in &action_claims {
                claim
                    .verify_current_with_conn(conn, session_id)
                    .await
                    .map_err(|_| CheckpointStoreError::ReplayPayloadMismatch)?;
            }
            Ok(())
        })
        .await?;
        let fences = action_claims
            .iter()
            .map(|claim| claim.replay_fence())
            .collect::<Vec<_>>();
        match Self::replay_status_for_bound(
            conn,
            Some(capture_scope),
            checkpoint_id,
            session_id,
            CheckpointScope::Committed,
            deadline,
            CheckpointReplayExpectation {
                claim_set: Some(&fences),
                native_companion: None,
            },
        )
        .await?
        {
            CheckpointReplayStatus::Missing => Err(CheckpointStoreError::ReplayPayloadMismatch),
            status => Ok(status),
        }
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
        Self::replay_status_for_bound(
            conn,
            capture_scope,
            checkpoint_id,
            session_id,
            scope,
            deadline,
            CheckpointReplayExpectation {
                claim_set: None,
                native_companion: None,
            },
        )
        .await
    }

    async fn replay_status_for_bound(
        conn: &DatabaseConnection,
        capture_scope: Option<&CaptureScope>,
        checkpoint_id: &str,
        session_id: &str,
        scope: CheckpointScope,
        deadline: Option<CaptureCommitDeadline>,
        expectation: CheckpointReplayExpectation<'_, '_>,
    ) -> Result<CheckpointReplayStatus, CheckpointStoreError> {
        if deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
            return Err(CheckpointStoreError::DeadlineExceeded);
        }
        if !Self::durable_replay_exists_for(conn, checkpoint_id, session_id, scope, deadline)
            .await?
        {
            return Ok(CheckpointReplayStatus::Missing);
        }
        // A no-write settlement (or legacy prebound recovery) has no
        // authoritative expected claim set. An explicit empty set from a
        // marker-bound writer does: it must reject committed revisions.
        if let Some(coverage_fences) = expectation.claim_set {
            await_checkpoint_precommit_read_until(deadline, async {
                traces::verify_checkpoint_claim_set_with_conn(
                    conn,
                    session_id,
                    checkpoint_id,
                    coverage_fences,
                )
                .await
                .map_err(|error| {
                    if error.is::<traces::TracesCheckpointClaimMismatch>() {
                        CheckpointStoreError::ReplayPayloadMismatch
                    } else {
                        CheckpointStoreError::StoreFailure {
                            stage: CheckpointStoreStage::CatalogTransaction,
                        }
                    }
                })
            })
            .await?;
        } else {
            // A legacy no-write or prebound replay has no authoritative
            // expected set. Preserve pre-coverage catalogs, but never
            // acknowledge a durable non-v1 revision through this path.
            let has_versioned_revision = await_checkpoint_precommit_read_until(deadline, async {
                if traces::legacy_coverage_tables_absent_with_conn(conn)
                    .await
                    .map_err(|error| {
                        if error.is::<traces::TracesCheckpointClaimMismatch>() {
                            CheckpointStoreError::ReplayPayloadMismatch
                        } else {
                            CheckpointStoreError::StoreFailure {
                                stage: CheckpointStoreStage::CatalogTransaction,
                            }
                        }
                    })?
                {
                    return Ok(false);
                }
                conn.query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT 1 AS present FROM agent_coverage_revision
                     WHERE checkpoint_id = ? AND coverage_schema_version <> 1 LIMIT 1",
                    [checkpoint_id.into()],
                ))
                .await
                .map(|row| row.is_some())
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::CatalogTransaction,
                })
            })
            .await?;
            if has_versioned_revision {
                return Err(CheckpointStoreError::ReplayPayloadMismatch);
            }
        }
        // A revision-set match alone cannot authorize native replay: an
        // ordinary versioned writer can commit the same tuple. Check the
        // authenticated private identity before settling any old marker.
        if let Some(companion) = expectation.native_companion {
            let matches = await_checkpoint_precommit_read_until(deadline, async {
                companion
                    .verify_committed_native_replay(conn)
                    .await
                    .map_err(|_| {
                        tracing::warn!(
                            stage = "native_replay_identity",
                            "cannot authenticate native replay identity; run libra agent doctor"
                        );
                        CheckpointStoreError::StoreFailure {
                            stage: CheckpointStoreStage::CatalogTransaction,
                        }
                    })
            })
            .await?;
            if !matches {
                return Err(CheckpointStoreError::ReplayPayloadMismatch);
            }
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

    /// No-write v2 settlement still requires the exact original envelope,
    /// current catalog fence, fresh lower native proof and committed private
    /// identities. A durable row or matching coverage revisions alone is not ACK.
    pub(crate) async fn settle_versioned_pending_replay_without_write(
        &self,
        request: &CheckpointWriteRequest<'_>,
    ) -> Result<CheckpointReplayStatus, CheckpointStoreError> {
        if request.pending_versioned_header.is_none()
            || request.payload().versioned_pending_reasoning.is_none()
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned pending durable proof".to_owned(),
            });
        }
        if !Self::durable_replay_exists_for(
            self.conn,
            request.checkpoint_id(),
            request.session_id(),
            request.scope(),
            request.deadline(),
        )
        .await?
        {
            return Ok(CheckpointReplayStatus::Missing);
        }
        let scope =
            self.capture_scope
                .as_ref()
                .ok_or_else(|| CheckpointStoreError::InvalidRequest {
                    field: "versioned pending durable scope".to_owned(),
                })?;
        await_checkpoint_precommit_read_until(request.deadline(), async {
            scope
                .assert_workspace_fence_live(self.conn)
                .await
                .map_err(|_| CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::ScopeFence,
                })
        })
        .await?;
        let current = self.prepare_current_pending_reasoning_fence(request)?;
        #[cfg(test)]
        test_support::pause_after_registration_if_armed(&self.repo_path).await;
        let restored = self.restore_pending_reasoning(request, &current).await?;
        let RestoredPendingClaimPlan::V2(companion) = &restored.commit_plan else {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned pending durable identity".to_owned(),
            });
        };
        let MarkerRegistration::Required(registration) = &self.registration else {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "versioned pending durable claim set".to_owned(),
            });
        };
        let fences = registration
            .iter()
            .map(OwnedRegistrationFence::borrowed)
            .collect::<Vec<_>>();
        Self::replay_status_for_bound(
            self.conn,
            Some(scope),
            request.checkpoint_id(),
            request.session_id(),
            request.scope(),
            request.deadline(),
            CheckpointReplayExpectation {
                claim_set: Some(&fences),
                native_companion: Some(companion),
            },
        )
        .await
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
    ) -> Result<CheckpointCommitParams<'request>, CheckpointStoreError> {
        Ok(CheckpointCommitParams {
            reasoning_artifacts: prepare_reasoning_artifact_params(
                request.payload(),
                request.session_id(),
                request.agent_kind(),
            )?,
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
        })
    }

    /// An exact revision-set replay is not an authoritative native-source
    /// replay until the persisted private identity proves the same companion.
    async fn native_replay_matches(
        &self,
        request: &CheckpointWriteRequest<'_>,
    ) -> Result<bool, CheckpointStoreError> {
        if request.pending_versioned_header.is_some() {
            return Ok(matches!(
                self.settle_versioned_pending_replay_without_write(request)
                    .await?,
                CheckpointReplayStatus::Complete
            ));
        }
        let Some(companion) = request.native_versioned_companion else {
            return Ok(true);
        };
        await_checkpoint_precommit_read_until(request.deadline(), async {
            companion
                .verify_committed_native_replay(self.conn)
                .await
                .map_err(|_| {
                    tracing::warn!(
                        stage = "native_replay_identity",
                        "cannot authenticate native replay identity; run libra agent doctor"
                    );
                    CheckpointStoreError::StoreFailure {
                        stage: CheckpointStoreStage::CatalogTransaction,
                    }
                })
        })
        .await
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
        // The ordinary request constructor precedes the private recovery
        // association. A caller that omits that association must never write
        // a redacted-only checkpoint and retire a native pending artifact.
        if request.payload().has_deferred_pending_reasoning()
            != request.pending_coverage().is_some()
        {
            return Err(CheckpointStoreError::InvalidRequest {
                field: "pending reasoning claim plan".to_owned(),
            });
        }
        if request
            .deadline()
            .is_some_and(|deadline| Instant::now() >= deadline.monotonic())
        {
            return Err(CheckpointStoreError::DeadlineExceeded);
        }
        if let Err(outcome) = self.request_matches_attempt(&request) {
            return Ok(outcome);
        }
        match (&self.registration, request.versioned_coverage) {
            (MarkerRegistration::Required(fences), Some(plan))
                if registration_matches_versioned_claims(fences, plan) => {}
            (MarkerRegistration::Required(fences), None)
                if fences.iter().all(|fence| {
                    fence.coverage_schema_version
                        == crate::internal::ai::observed_agents::CoverageSchemaVersion::V1
                }) => {}
            (MarkerRegistration::Prebound, None) => {}
            (MarkerRegistration::Prebound, Some(plan))
                if request.imported_versioned_prebound
                    && plan.ordinary().source_channel == "import"
                    && plan.ordinary().capture_scope.as_ref() == self.capture_scope.as_ref()
                    && plan.ordinary().import_identity.is_some()
                    && plan.ordinary().import_session.is_some() => {}
            _ => {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "checkpoint versioned coverage fence".to_owned(),
                });
            }
        }
        if let Some(companion) = request.native_versioned_companion {
            let Some(scope) = self.capture_scope.as_ref() else {
                return Err(CheckpointStoreError::InvalidRequest {
                    field: "native v2 capture scope".to_owned(),
                });
            };
            companion
                .reasoning()
                .validate_for(scope, request.session_id(), &[])
                .map_err(|_| CheckpointStoreError::InvalidRequest {
                    field: "native v2 capture scope".to_owned(),
                })?;
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
        let replay_fences = match &self.registration {
            MarkerRegistration::Required(fences) => Some(
                fences
                    .iter()
                    .map(OwnedRegistrationFence::borrowed)
                    .collect::<Vec<_>>(),
            ),
            MarkerRegistration::Prebound => None,
        };
        let replay_status = if request.pending_versioned_header.is_some() {
            self.settle_versioned_pending_replay_without_write(&request)
                .await
        } else {
            Self::replay_status_for_bound(
                self.conn,
                self.capture_scope.as_ref(),
                request.checkpoint_id(),
                request.session_id(),
                request.scope(),
                request.deadline(),
                CheckpointReplayExpectation {
                    claim_set: replay_fences.as_deref(),
                    native_companion: request.native_versioned_companion,
                },
            )
            .await
        };
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
            let registration = match registration {
                Ok(registration) => registration,
                Err(error) => {
                    if typed_append_cause::<traces::TracesCheckpointClaimMismatch>(&error).is_some()
                    {
                        return Ok(CheckpointWriteOutcome::ConflictUnchanged {
                            reason: CheckpointConflictReason::ReplayPayloadMismatch,
                        });
                    }
                    return Err(
                        if request
                            .deadline()
                            .is_some_and(|deadline| Instant::now() >= deadline.monotonic())
                        {
                            CheckpointStoreError::DeadlineExceeded
                        } else {
                            CheckpointStoreError::StoreFailure {
                                stage: CheckpointStoreStage::Marker,
                            }
                        },
                    );
                }
            };
            match registration {
                traces::TracesWriteAttemptRegistration::Registered => {}
                traces::TracesWriteAttemptRegistration::AlreadyInFlightSameGeneration => {
                    return Ok(CheckpointWriteOutcome::AttemptInFlight {
                        checkpoint_id: request.checkpoint_id().to_string(),
                        marker_generation: self.marker_generation.clone(),
                    });
                }
                traces::TracesWriteAttemptRegistration::AlreadyCommitted => {
                    if !self.native_replay_matches(&request).await? {
                        return Ok(CheckpointWriteOutcome::ConflictUnchanged {
                            reason: CheckpointConflictReason::ReplayPayloadMismatch,
                        });
                    }
                    return Ok(CheckpointWriteOutcome::AlreadyExists {
                        checkpoint_id: request.checkpoint_id().to_string(),
                    });
                }
                traces::TracesWriteAttemptRegistration::TerminalReceiptAlreadyComplete => {
                    if !self.native_replay_matches(&request).await? {
                        return Ok(CheckpointWriteOutcome::ConflictUnchanged {
                            reason: CheckpointConflictReason::ReplayPayloadMismatch,
                        });
                    }
                    return Ok(CheckpointWriteOutcome::TerminalReceiptAlreadyApplied);
                }
            }
        }

        // A pending reasoning plan is checked only after the marker
        // registration transaction revalidated the catalog and claim fences.
        // The later lower issuer still has to verify the fixed source proof.
        let pending_current = if request.pending_coverage().is_some() {
            match self.prepare_current_pending_reasoning_fence(&request) {
                Ok(current) => Some(current),
                Err(error) => {
                    self.retire_failed_attempt_with_diagnostic(
                        "pending_current_fence",
                        request.deadline(),
                    )
                    .await;
                    return Err(error);
                }
            }
        } else {
            None
        };

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

        // This batch and its one-use key token exist only inside this append
        // attempt. Every retry loads and authenticates the retained original
        // envelope again after the current terminal and marker fences.
        let restored_pending = if let Some(current) = pending_current.as_ref() {
            match self.restore_pending_reasoning(&request, current).await {
                Ok(restored) => Some(restored),
                Err(error) => {
                    self.retire_failed_attempt_with_diagnostic(
                        "pending_reasoning_restore",
                        request.deadline(),
                    )
                    .await;
                    return Err(error);
                }
            }
        } else {
            None
        };

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
                match crate::utils::client_storage::ClientStorage::open_init_storage(objects_dir) {
                    Ok(storage) => std::sync::Arc::new(storage),
                    Err(_error) => {
                        self.retire_failed_attempt_with_diagnostic(
                            "storage_config",
                            request.deadline(),
                        )
                        .await;
                        return Err(CheckpointStoreError::StoreFailure {
                            stage: CheckpointStoreStage::ObjectWrite,
                        });
                    }
                }
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
        let store_bound_native = request
            .native_versioned_companion
            .or_else(|| {
                restored_pending
                    .as_ref()
                    .and_then(|restored| match &restored.commit_plan {
                        RestoredPendingClaimPlan::V2(plan) => Some(plan.as_ref()),
                        RestoredPendingClaimPlan::V1(_) => None,
                    })
            })
            .map(|companion| StoreBoundNativeVersionedTxnExtra {
                companion,
                permit: StoreBoundNativeVersionedPermit { _private: () },
            });
        let mut params = self.append_params(&request)?;
        if let Some(bound) = store_bound_native.as_ref() {
            params.txn_extra = Some(bound);
        }
        if let Some(restored) = restored_pending.as_ref() {
            params.reasoning_artifacts = &restored.artifacts;
            if let RestoredPendingClaimPlan::V1(plan) = &restored.commit_plan {
                params.txn_extra = Some(plan.as_ref());
            }
        }
        #[cfg(test)]
        let companion_fault = test_support::companion_fault(params.txn_extra);
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
    use serial_test::serial;
    use sha2::Digest;

    use super::*;

    #[cfg(unix)]
    async fn cov2_pending_native_companion(
        fixture: &crate::internal::ai::coverage_gate::tests::ReasoningFixture,
        payload: &CheckpointRedactedPayload,
        turns: &[NormalizedTurn],
    ) -> NativeVersionedReasoningClaimCommitPlan {
        use crate::internal::ai::{
            coverage_gate::{ReservedTurnClaim, VersionedReservedTurnClaim},
            observed_agents::reasoning::carrier_test_support as native,
        };

        let source = prepare_native_versioned_reasoning_coverage_plan(
            payload,
            ReasoningIdentityPreparation {
                conn: &fixture.conn,
                scope: &fixture.scope,
                storage_path: &fixture.dir.path().join(crate::utils::util::ROOT_DIR),
                authorized_root: fixture.dir.path(),
                session_id: native::CAPTURE,
                provider: "claude_code",
                turns,
                deadline: Instant::now() + Duration::from_secs(30),
            },
        )
        .await
        .expect("derive the synthetic source-bound identity set");
        let claims = turns
            .iter()
            .map(|turn| {
                let version = select_coverage_schema(turn);
                let digest = encode_coverage_turn_for_schema(version.as_i64(), turn)
                    .expect("encode the synthetic normalized turn")
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
                .expect("keep a known coverage schema version")
            })
            .collect::<Vec<_>>();
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: native::CAPTURE.to_owned(),
            checkpoint_id: "pending-native-checkpoint".to_owned(),
            owner: "pending-native-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: claims.iter().map(|bound| bound.claim.clone()).collect(),
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let versioned = VersionedLiveClaimCommitPlan::new(ordinary, &claims)
            .expect("bind exact synthetic version sidecar");
        NativeVersionedReasoningClaimCommitPlan::new(versioned, &source)
            .expect("bind the sealed source and its complete claims")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_pending_native_versioned_source_borrow_rejects_recombination() {
        use crate::internal::ai::{
            coverage_gate::tests::{ReasoningFixture, cov2_native_v2_turn},
            observed_agents::reasoning::{
                ReasoningSourceDisposition, carrier_test_support as native,
            },
        };

        let fixture = ReasoningFixture::new().await;
        let payload = reasoning_coverage_test_payload(vec![b"synthetic sealed proof".to_vec()]);
        let turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "visible typed test turn",
            Completeness::Complete,
        )];
        let companion = cov2_pending_native_companion(&fixture, &payload, &turns).await;
        let prepared = payload
            .prepare_pending_native_versioned_seal(&fixture.scope, &companion, "claude_code")
            .expect("the exact source may supply a pending native borrow");
        assert_eq!(prepared.associations.len(), 1);
        assert_eq!(prepared.associations[0].0, "synthetic-turn-0");
        assert_eq!(
            prepared.associations[0].1.slot(),
            companion
                .reasoning()
                .turn(CoverageSchemaVersion::V2, "synthetic-turn-0")
                .expect("source-derived identity")
                .slot()
        );
        let other = reasoning_coverage_test_payload(vec![b"independent sealed proof".to_vec()]);
        assert!(
            prepare_native_versioned_reasoning_coverage_plan(
                &other,
                ReasoningIdentityPreparation {
                    conn: &fixture.conn,
                    scope: &fixture.scope,
                    storage_path: &fixture.dir.path().join(crate::utils::util::ROOT_DIR),
                    authorized_root: fixture.dir.path(),
                    session_id: native::CAPTURE,
                    provider: "claude_code",
                    turns: &turns,
                    deadline: Instant::now() + Duration::from_secs(30),
                },
            )
            .await
            .is_ok(),
            "the second native payload is independently valid"
        );
        assert!(
            other
                .prepare_pending_native_versioned_seal(&fixture.scope, &companion, "claude_code")
                .is_err(),
            "another valid payload cannot borrow the first source tag"
        );
        let mut wrong_scope = fixture.scope.clone();
        wrong_scope.repo_id.push_str("-other");
        assert!(
            payload
                .prepare_pending_native_versioned_seal(&wrong_scope, &companion, "claude_code")
                .is_err()
        );
        assert!(
            payload
                .prepare_pending_native_versioned_seal(&fixture.scope, &companion, "codex")
                .is_err(),
            "provider drift cannot borrow the original capture proof"
        );
        assert!(
            companion
                .reasoning()
                .matches_payload(&payload, "different-session", "claude_code")
                .is_err(),
            "the sealed source rejects a changed capture session"
        );
        for state in [
            ReasoningSourceDisposition::Ordinary,
            ReasoningSourceDisposition::Unproven,
            ReasoningSourceDisposition::Unmapped,
            ReasoningSourceDisposition::HelperUnsupported,
        ] {
            let mut non_native =
                reasoning_coverage_test_payload(vec![b"synthetic sealed proof".to_vec()]);
            non_native.reasoning = state;
            assert!(
                non_native
                    .prepare_pending_native_versioned_seal(
                        &fixture.scope,
                        &companion,
                        "claude_code",
                    )
                    .is_err(),
                "ordinary or partial sources cannot borrow native authority"
            );
        }
        assert_eq!(
            fixture_row_count(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0,
            "the prepare-only entry never persists a checkpoint"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_pending_native_versioned_claims_bind_by_version_and_order() {
        use crate::internal::ai::coverage_gate::{
            VersionedReservedTurnClaim,
            tests::{ReasoningFixture, cov2_native_v2_turn, turn},
        };

        let fixture = ReasoningFixture::new().await;
        let payload = reasoning_coverage_test_payload_for_native_turns(vec![
            (0, b"synthetic field zero".to_vec()),
            (1, b"synthetic field one".to_vec()),
            (2, b"synthetic field two".to_vec()),
        ]);
        // The claim order is deliberately not the source map's key order.
        let turns = [
            cov2_native_v2_turn("synthetic-turn-2", "visible two", Completeness::Complete),
            turn("synthetic-turn-0", "visible zero", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-1", "visible one", Completeness::Complete),
        ];
        let companion = cov2_pending_native_companion(&fixture, &payload, &turns).await;
        let prepared = payload
            .prepare_pending_native_versioned_seal(&fixture.scope, &companion, "claude_code")
            .expect("all ordered versioned claims are bound to the sealed source");
        assert_eq!(prepared.associations.len(), turns.len());
        assert!(std::ptr::eq(
            prepared.batch,
            reasoning_coverage_test_native_batch(&payload)
        ));
        for ((key, identity), (version, claim)) in prepared
            .associations
            .iter()
            .zip(companion.versioned().claims())
        {
            assert_eq!(key, &claim.logical_turn_key, "preserve claim order");
            let expected = companion
                .reasoning()
                .turn(version, &claim.logical_turn_key)
                .expect("source-derived versioned identity");
            assert_eq!(identity.slot(), expected.slot());
            assert_eq!(identity.kind(), expected.kind());
            assert_eq!(identity.descriptor_mac(), expected.descriptor_mac());
        }
        for proof_key in reasoning_coverage_test_native_batch(&payload).pending_logical_turns() {
            assert_eq!(
                prepared
                    .associations
                    .iter()
                    .filter(|(key, _)| key == proof_key)
                    .count(),
                1,
                "each proof-bearing key has exactly one versioned association"
            );
        }

        // A malformed source/claim pair cannot pass the predecessor's sealed
        // companion constructor; the entry above re-runs this same binding.
        let canonical = companion
            .versioned()
            .claims()
            .map(|(version, claim)| VersionedReservedTurnClaim {
                coverage_schema_version: version,
                claim: claim.clone(),
            })
            .collect::<Vec<_>>();
        let make_versioned = |bound: &[VersionedReservedTurnClaim]| {
            let mut ordinary = companion.versioned().ordinary().clone();
            ordinary.claims = bound.iter().map(|item| item.claim.clone()).collect();
            VersionedLiveClaimCommitPlan::new(ordinary, bound)
                .expect("test mutation keeps its ordinal sidecar aligned")
        };
        let mut missing = canonical.clone();
        missing.pop();
        let mut extra = canonical.clone();
        let mut additional = canonical[0].clone();
        additional.claim.logical_turn_key = "synthetic-turn-extra".to_owned();
        extra.push(additional);
        let mut changed_version = canonical.clone();
        changed_version[0].coverage_schema_version = CoverageSchemaVersion::V1;
        let mut changed_digest = canonical.clone();
        changed_digest[0].claim.coverage_digest = "0".repeat(64);
        let mut changed_completeness = canonical.clone();
        changed_completeness[0].claim.completeness = Completeness::Incomplete;
        for changed in [
            missing,
            extra,
            changed_version,
            changed_digest,
            changed_completeness,
        ] {
            let versioned = make_versioned(&changed);
            assert!(companion.reasoning().bind_claims(&versioned).is_err());
            assert!(
                NativeVersionedReasoningClaimCommitPlan::new(versioned, companion.reasoning())
                    .is_err(),
                "a recombined claim set cannot create a pending-ready companion"
            );
        }
        let mut reversed = canonical;
        reversed.reverse();
        let reversed_companion = NativeVersionedReasoningClaimCommitPlan::new(
            make_versioned(&reversed),
            companion.reasoning(),
        )
        .expect("claim order changes no canonical source set");
        let reversed_prepared = payload
            .prepare_pending_native_versioned_seal(
                &fixture.scope,
                &reversed_companion,
                "claude_code",
            )
            .expect("the reordered sealed companion remains source-bound");
        assert_eq!(
            reversed_prepared
                .associations
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            reversed
                .iter()
                .map(|item| item.claim.logical_turn_key.as_str())
                .collect::<Vec<_>>(),
            "the pending borrow must follow the companion's original claim order"
        );
        assert_eq!(
            fixture_row_count(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0,
            "claim binding and borrowing must not write pending state"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_pending_native_versioned_claim_count_is_bounded() {
        use crate::internal::ai::coverage_gate::{
            VersionedReservedTurnClaim,
            tests::{ReasoningFixture, cov2_native_v2_turn},
        };

        let fixture = ReasoningFixture::new().await;
        let payload = reasoning_coverage_test_payload(vec![b"synthetic sealed proof".to_vec()]);
        let turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "visible typed turn",
            Completeness::Complete,
        )];
        let companion = cov2_pending_native_companion(&fixture, &payload, &turns).await;
        assert!(
            payload
                .prepare_pending_native_versioned_seal(&fixture.scope, &companion, "claude_code")
                .is_ok(),
            "the normal one-claim sealed companion reaches this entry"
        );

        let mut empty_source = companion.reasoning().clone();
        empty_source.turns.clear();
        empty_source.claims.clear();
        let mut empty_ordinary = companion.versioned().ordinary().clone();
        empty_ordinary.claims.clear();
        let empty_versioned = VersionedLiveClaimCommitPlan::new(empty_ordinary, &[])
            .expect("construct a test-only empty versioned sidecar");
        let empty_companion =
            NativeVersionedReasoningClaimCommitPlan::new(empty_versioned, &empty_source)
                .expect("the predecessor accepts a matching test-only empty source");
        let empty_error = match payload.prepare_pending_native_versioned_seal(
            &fixture.scope,
            &empty_companion,
            "claude_code",
        ) {
            Ok(_) => panic!("empty claims produced a pending native witness"),
            Err(error) => error,
        };
        assert!(matches!(
            empty_error,
            CheckpointStoreError::InvalidRequest { ref field }
                if field == "pending native v2 claim limit"
        ));

        // This same-module test seam deliberately bypasses the COV2-19
        // factory's 512-entry limit. The source map, identities and claims
        // stay internally paired so only this pending entry's guard rejects.
        let mut oversized_source = companion.reasoning().clone();
        let identity = oversized_source
            .turn(CoverageSchemaVersion::V2, "synthetic-turn-0")
            .expect("original source identity")
            .clone();
        let mut bound = companion
            .versioned()
            .claims()
            .map(|(version, claim)| VersionedReservedTurnClaim {
                coverage_schema_version: version,
                claim: claim.clone(),
            })
            .collect::<Vec<_>>();
        for index in 1..=traces::REASONING_ARTIFACT_MAX_ENTRIES {
            let key = format!("synthetic-turn-{index}");
            let mut next = bound[0].clone();
            next.claim.logical_turn_key = key.clone();
            oversized_source.turns.insert(
                (CoverageSchemaVersion::V2.as_i64(), key.clone()),
                identity.clone(),
            );
            oversized_source.claims.insert(
                (CoverageSchemaVersion::V2.as_i64(), key),
                (next.claim.coverage_digest.clone(), next.claim.completeness),
            );
            bound.push(next);
        }
        assert_eq!(bound.len(), traces::REASONING_ARTIFACT_MAX_ENTRIES + 1);
        assert_eq!(oversized_source.turns.len(), bound.len());
        assert_eq!(oversized_source.claims.len(), bound.len());
        let mut oversized_ordinary = companion.versioned().ordinary().clone();
        oversized_ordinary.claims = bound.iter().map(|item| item.claim.clone()).collect();
        let oversized_versioned = VersionedLiveClaimCommitPlan::new(oversized_ordinary, &bound)
            .expect("construct the internally paired 513-claim sidecar");
        let oversized_companion =
            NativeVersionedReasoningClaimCommitPlan::new(oversized_versioned, &oversized_source)
                .expect("the predecessor binding accepts the test-only 513-claim companion");
        assert!(
            oversized_companion
                .reasoning()
                .matches_payload(
                    &payload,
                    companion.versioned().ordinary().session_id.as_str(),
                    "claude_code"
                )
                .expect("compare the same original native source"),
            "the oversized fixture must remain source-bound"
        );
        let oversized_error = match payload.prepare_pending_native_versioned_seal(
            &fixture.scope,
            &oversized_companion,
            "claude_code",
        ) {
            Ok(_) => panic!("513 claims produced a pending native witness"),
            Err(error) => error,
        };
        assert!(matches!(
            oversized_error,
            CheckpointStoreError::InvalidRequest { ref field }
                if field == "pending native v2 claim limit"
        ));
        assert_eq!(
            fixture_row_count(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0,
            "both entry-limit failures remain prepare-only"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_pending_native_same_bare_key_fails_closed() {
        use crate::internal::ai::coverage_gate::tests::{
            ReasoningFixture, cov2_native_v2_turn, turn,
        };

        let fixture = ReasoningFixture::new().await;
        let payload = reasoning_coverage_test_payload_for_native_turns(vec![(
            0,
            b"synthetic shared-key field".to_vec(),
        )]);
        let same_key_turns = [
            turn("synthetic-turn-0", "ordinary v1", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-0", "visible v2", Completeness::Complete),
        ];
        let companion = cov2_pending_native_companion(&fixture, &payload, &same_key_turns).await;
        assert_eq!(companion.versioned().claims().count(), 2);
        assert!(
            companion
                .reasoning()
                .turn(CoverageSchemaVersion::V1, "synthetic-turn-0")
                .is_some()
        );
        assert!(
            companion
                .reasoning()
                .turn(CoverageSchemaVersion::V2, "synthetic-turn-0")
                .is_some()
        );
        let alias_error = match payload.prepare_pending_native_versioned_seal(
            &fixture.scope,
            &companion,
            "claude_code",
        ) {
            Ok(_) => panic!("cross-version native bare-key alias produced a pending witness"),
            Err(error) => error,
        };
        assert!(matches!(
            alias_error,
            CheckpointStoreError::InvalidRequest { ref field }
                if field == "pending native v2 bare key alias"
        ));

        let distinct_payload = reasoning_coverage_test_payload_for_native_turns(vec![
            (0, b"synthetic v1 field".to_vec()),
            (1, b"synthetic v2 field".to_vec()),
        ]);
        let distinct_turns = [
            turn("synthetic-turn-0", "ordinary v1", Completeness::Complete),
            cov2_native_v2_turn("synthetic-turn-1", "visible v2", Completeness::Complete),
        ];
        let distinct =
            cov2_pending_native_companion(&fixture, &distinct_payload, &distinct_turns).await;
        let prepared = distinct_payload
            .prepare_pending_native_versioned_seal(&fixture.scope, &distinct, "claude_code")
            .expect("different bare keys can borrow their versioned identities");
        assert_eq!(
            prepared
                .associations
                .iter()
                .map(|(key, _)| key.as_str())
                .collect::<Vec<_>>(),
            ["synthetic-turn-0", "synthetic-turn-1"]
        );
        assert_eq!(
            fixture_row_count(&fixture.conn, "SELECT COUNT(*) AS n FROM agent_checkpoint").await,
            0,
            "the native alias rejection and success path do not write an artifact"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_v2_native_plan_rejects_unbound_source() {
        use crate::internal::ai::{
            coverage_gate::tests::{
                ReasoningFixture, cov2_native_v2_source_and_reservation, cov2_native_v2_turn,
                cov2_native_v2_write,
            },
            observed_agents::reasoning::{
                ReasoningSourceDisposition, carrier_test_support as native,
            },
        };

        let fixture = ReasoningFixture::new().await;
        let storage = fixture.dir.path().join(crate::utils::util::ROOT_DIR);
        let turns = [cov2_native_v2_turn(
            "synthetic-turn-0",
            "visible typed test turn",
            Completeness::Complete,
        )];
        let payload = reasoning_coverage_test_payload(vec![b"synthetic sealed proof".to_vec()]);
        let request =
            |session_id: &'static str, provider: &'static str| ReasoningIdentityPreparation {
                conn: &fixture.conn,
                scope: &fixture.scope,
                storage_path: &storage,
                authorized_root: fixture.dir.path(),
                session_id,
                provider,
                turns: &turns,
                deadline: Instant::now() + Duration::from_secs(30),
            };
        let source = prepare_native_versioned_reasoning_coverage_plan(
            &payload,
            request(native::CAPTURE, "claude_code"),
        )
        .await
        .expect("sealed synthetic source can prepare a v2 identity");
        assert!(
            source
                .matches_payload(&payload, native::CAPTURE, "claude_code")
                .expect("compare the original sealed native source"),
            "the companion must match the payload that produced it"
        );
        let other_native_payload =
            reasoning_coverage_test_payload(vec![b"independent legal sealed proof".to_vec()]);
        assert!(
            !source
                .matches_payload(&other_native_payload, native::CAPTURE, "claude_code")
                .expect("compare a second sealed native source"),
            "a different legal opaque proof cannot borrow the first source tag"
        );
        assert!(
            prepare_native_versioned_reasoning_coverage_plan(
                &other_native_payload,
                request(native::CAPTURE, "claude_code"),
            )
            .await
            .is_ok(),
            "the second payload must be independently valid so only the source binding can reject it"
        );
        let (write_source, reserved) = cov2_native_v2_source_and_reservation(
            &fixture,
            &payload,
            &turns,
            "source-binding-owner",
        )
        .await;
        let write_error = cov2_native_v2_write(
            &fixture,
            &other_native_payload,
            &write_source,
            &reserved,
            "source-binding-owner",
            "source-binding-checkpoint",
        )
        .await
        .expect_err("the real checkpoint writer must reject an unrelated native source");
        assert_eq!(
            write_error.to_string(),
            "capture checkpoint request has an invalid native v2 reasoning claim plan"
        );
        for sql in [
            "SELECT COUNT(*) AS n FROM agent_checkpoint WHERE checkpoint_id = 'source-binding-checkpoint'",
            "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE checkpoint_id = 'source-binding-checkpoint'",
            "SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'",
            "SELECT COUNT(*) AS n FROM object_index",
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'catalog_committed'",
        ] {
            assert_eq!(fixture_row_count(&fixture.conn, sql).await, 0, "{sql}");
        }
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &fixture.conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                native::CAPTURE,
                "source-binding-checkpoint",
            )
            .await
            .expect("read source-binding checkpoint marker")
            .is_none(),
            "rejected source binding must not register a checkpoint marker"
        );
        assert!(
            source
                .turn(CoverageSchemaVersion::V2, "synthetic-turn-0")
                .is_some()
        );
        assert!(
            prepare_native_versioned_reasoning_coverage_plan(
                &payload,
                request("different-capture", "claude_code"),
            )
            .await
            .is_err(),
            "capture session drift must reject the existing proof binding"
        );
        assert!(
            prepare_native_versioned_reasoning_coverage_plan(
                &payload,
                request(native::CAPTURE, "codex"),
            )
            .await
            .is_err(),
            "provider drift must reject the existing proof binding"
        );
        let mut changed_source =
            reasoning_coverage_test_payload(vec![b"synthetic sealed proof".to_vec()]);
        native::tamper_carrier_locator(&mut changed_source.reasoning);
        assert!(
            prepare_native_versioned_reasoning_coverage_plan(
                &changed_source,
                request(native::CAPTURE, "claude_code"),
            )
            .await
            .is_err(),
            "a changed sealed source locator must fail before identity preparation"
        );
        for state in [
            ReasoningSourceDisposition::Ordinary,
            ReasoningSourceDisposition::Unproven,
            ReasoningSourceDisposition::Unmapped,
            ReasoningSourceDisposition::HelperUnsupported,
        ] {
            let mut non_native =
                reasoning_coverage_test_payload(vec![b"synthetic sealed proof".to_vec()]);
            non_native.reasoning = state;
            assert!(
                prepare_native_versioned_reasoning_coverage_plan(
                    &non_native,
                    request(native::CAPTURE, "claude_code"),
                )
                .await
                .is_err(),
                "only the sealed NativeProven payload may prepare a v2 identity"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov11_legacy_same_action_probe_is_read_only_and_action_local() {
        use crate::internal::ai::{
            coverage_gate::tests::ReasoningFixture,
            observed_agents::reasoning::carrier_test_support as fixture_ids,
        };
        let fixture = ReasoningFixture::new().await;
        fixture
            .seed_legacy_v1_checkpoint_revision("legacy-action")
            .await;
        let probe = |checkpoint| {
            TracesCheckpointStore::durable_legacy_replay_exists_for(
                &fixture.conn,
                &fixture.scope,
                checkpoint,
                fixture_ids::CAPTURE,
                None,
            )
        };
        assert!(probe("legacy-action").await.unwrap());
        assert!(!probe("missing-action").await.unwrap());
        fixture
            .conn
            .execute_unprepared(
                "INSERT INTO agent_checkpoint (checkpoint_id, session_id, scope, parent_commit,
             tree_oid, metadata_blob_oid, traces_commit, created_at)
             SELECT 'empty-action', session_id, scope, parent_commit,
             tree_oid, metadata_blob_oid, traces_commit, created_at FROM agent_checkpoint
             WHERE checkpoint_id = 'legacy-action'",
            )
            .await
            .unwrap();
        assert!(probe("empty-action").await.unwrap());
        fixture
            .conn
            .execute_unprepared(
                "INSERT INTO agent_coverage_revision SELECT session_id, 'extra-v2-key', 2, 1,
             'newer-action', coverage_digest, completeness, source_channel, created_at
             FROM agent_coverage_revision WHERE checkpoint_id = 'legacy-action'",
            )
            .await
            .unwrap();
        assert!(
            probe("legacy-action").await.unwrap(),
            "unrelated v2 history cannot prevent this action's legacy settlement"
        );
        assert!(probe("empty-action").await.unwrap());
        assert_eq!(
            TracesCheckpointStore::settle_committed_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                "legacy-action",
                fixture_ids::CAPTURE,
                None,
            )
            .await,
            Ok(CheckpointReplayStatus::Complete)
        );
        assert_eq!(
            TracesCheckpointStore::settle_committed_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                "empty-action",
                fixture_ids::CAPTURE,
                None,
            )
            .await,
            Ok(CheckpointReplayStatus::Complete)
        );
        assert_eq!(
            TracesCheckpointStore::durable_legacy_replay_exists_for(
                &fixture.conn,
                &fixture.scope,
                "legacy-action",
                "foreign-session",
                None,
            )
            .await,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        );
        fixture
            .conn
            .execute_unprepared(
                "UPDATE agent_coverage_revision SET coverage_schema_version = 7
             WHERE checkpoint_id = 'legacy-action'",
            )
            .await
            .unwrap();
        assert!(
            !probe("legacy-action").await.unwrap(),
            "unknown coverage is never v1-only"
        );
        let row = fixture
            .conn
            .query_one_raw(Statement::from_string(
                fixture.conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_coverage_claim".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.try_get_by::<i64, _>("n").unwrap(),
            0,
            "legacy probing and settlement must never enter either reservation gate"
        );
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_millis(1), 0);
        assert_eq!(
            TracesCheckpointStore::durable_legacy_replay_exists_for(
                &fixture.conn,
                &fixture.scope,
                "empty-action",
                fixture_ids::CAPTURE,
                Some(expired),
            )
            .await,
            Err(CheckpointStoreError::DeadlineExceeded)
        );
        fixture
            .conn
            .execute_unprepared("DROP TABLE agent_coverage_revision")
            .await
            .unwrap();
        assert!(
            probe("empty-action").await.is_err(),
            "database failure cannot authorize legacy settlement"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov11_incomplete_covered_replay_binds_exact_committed_state() {
        use sea_orm::TransactionTrait;

        use crate::internal::ai::{
            coverage_gate::{
                CaptureReservationExecution, ReasoningCoverageInput,
                reserve_runtime_turn_claims_with_reasoning,
                tests::{ReasoningFixture, cov2_visible_turn},
            },
            observed_agents::reasoning::carrier_test_support as fixture_ids,
        };
        let fixture = ReasoningFixture::new().await;
        let checkpoint_id = "incomplete-runtime-checkpoint";
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap();
        let reader = ReasoningIdentityReadContext::new(
            &fixture.scope,
            fixture_ids::CAPTURE,
            &fixture.dir.path().join(crate::utils::util::ROOT_DIR),
            fixture.dir.path(),
            deadline.monotonic(),
        );
        let turns = [cov2_visible_turn(
            "incomplete-visible-key",
            "redacted typed data",
            Completeness::Incomplete,
        )];
        let reserve = || {
            reserve_runtime_turn_claims_with_reasoning(
                &fixture.conn,
                &fixture.scope,
                fixture_ids::CAPTURE,
                &turns,
                "runtime-owner",
                1,
                CaptureReservationExecution::export(deadline),
                ReasoningCoverageInput::ordinary(&reader),
                false,
            )
        };
        let reserved = reserve().await.unwrap();
        assert_eq!(reserved.versioned_reserved.len(), 1);
        assert_eq!(
            reserved.versioned_reserved[0].coverage_schema_version,
            CoverageSchemaVersion::V2
        );
        let ordinary = LiveClaimCommitPlan {
            source_channel: "export",
            session_id: fixture_ids::CAPTURE.to_owned(),
            checkpoint_id: checkpoint_id.to_owned(),
            owner: "runtime-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: reserved.ordinary.reserved,
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let plan = LiveCaptureClaimCommitPlan::new_runtime_versioned(
            ordinary,
            &reserved.versioned_reserved,
            None,
        )
        .unwrap();
        let txn = fixture.conn.begin().await.unwrap();
        plan.apply(
            &txn,
            &TracesCommitCtx {
                commit_hash: "a".repeat(40),
                tree_oid: "b".repeat(40),
                metadata_blob_oid: "c".repeat(40),
            },
        )
        .await
        .unwrap();
        txn.commit().await.unwrap();
        let covered = reserve().await.unwrap();
        assert!(covered.ordinary.reserved.is_empty());
        assert_eq!(covered.ordinary.skipped_covered, 1);
        let settle = || {
            TracesCheckpointStore::settle_runtime_covered_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                checkpoint_id,
                fixture_ids::CAPTURE,
                Some(deadline),
                &covered.covered,
            )
        };
        assert_eq!(settle().await, Ok(CheckpointReplayStatus::Complete));
        assert_eq!(settle().await, Ok(CheckpointReplayStatus::Complete));
        for sql in [
            "SELECT COUNT(*) AS n FROM agent_checkpoint",
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version = 2 AND completeness = 'incomplete' AND revision = 1 AND state = 'catalog_committed'",
            "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE coverage_schema_version = 2 AND completeness = 'incomplete' AND revision = 1",
        ] {
            let row = fixture
                .conn
                .query_one_raw(Statement::from_string(
                    fixture.conn.get_database_backend(),
                    sql.to_owned(),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 1, "{sql}");
        }
        // Completeness is an exact committed tuple member, not a condition
        // for whether this immutable action's receipt may settle.
        fixture.conn.execute_unprepared(
            "UPDATE agent_coverage_revision SET completeness = 'complete' WHERE coverage_schema_version = 2",
        ).await.unwrap();
        assert_eq!(
            settle().await,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        );
        fixture.conn.execute_unprepared(
            "UPDATE agent_coverage_revision SET completeness = 'incomplete' WHERE coverage_schema_version = 2",
        ).await.unwrap();
        assert_eq!(settle().await, Ok(CheckpointReplayStatus::Complete));
        fixture.conn.execute_unprepared(
            "UPDATE agent_coverage_claim SET completeness = 'complete' WHERE coverage_schema_version = 2",
        ).await.unwrap();
        assert_eq!(
            settle().await,
            Err(CheckpointStoreError::ReplayPayloadMismatch)
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov11_runtime_versioned_companion_applies_claims_and_catalog_once() {
        use sea_orm::TransactionTrait;

        use crate::internal::ai::{
            coverage_gate::{
                CaptureReservationExecution, ReasoningCoverageInput,
                reserve_runtime_turn_claims_with_reasoning,
                tests::{ReasoningFixture, cov2_visible_turn},
            },
            observed_agents::reasoning::carrier_test_support as fixture_ids,
        };
        let fixture = ReasoningFixture::new().await;
        fixture
            .conn
            .execute_unprepared("CREATE TABLE cov11_checkpoint_insert_count (attempt INTEGER)")
            .await
            .unwrap();
        fixture.conn.execute_unprepared("CREATE TRIGGER cov11_count_checkpoint_insert BEFORE INSERT ON agent_checkpoint BEGIN INSERT INTO cov11_checkpoint_insert_count VALUES (1); END").await.unwrap();

        let reader = ReasoningIdentityReadContext::new(
            &fixture.scope,
            fixture_ids::CAPTURE,
            &fixture.dir.path().join(crate::utils::util::ROOT_DIR),
            fixture.dir.path(),
            Instant::now() + Duration::from_secs(30),
        );
        let turns = [cov2_visible_turn(
            "visible-key",
            "redacted typed data",
            Completeness::Complete,
        )];
        let reserved = reserve_runtime_turn_claims_with_reasoning(
            &fixture.conn,
            &fixture.scope,
            fixture_ids::CAPTURE,
            &turns,
            "runtime-owner",
            1,
            CaptureReservationExecution::live(
                CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap(),
            ),
            ReasoningCoverageInput::ordinary(&reader),
            false,
        )
        .await
        .unwrap();
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: fixture_ids::CAPTURE.to_owned(),
            checkpoint_id: "runtime-checkpoint".to_owned(),
            owner: "runtime-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: reserved.ordinary.reserved,
            import_session: None,
            import_identity: None,
            capture_scope: Some(fixture.scope.clone()),
        };
        let plan = LiveCaptureClaimCommitPlan::new_runtime_versioned(
            ordinary,
            &reserved.versioned_reserved,
            None,
        )
        .unwrap();
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .unwrap();
        let request = CheckpointWriteRequest::new(
            "runtime-action",
            "runtime-checkpoint",
            fixture_ids::CAPTURE,
            "claude_code",
            None,
            CheckpointScope::Committed,
            "runtime-generation",
            None,
            &payload,
            None,
            None,
        )
        .unwrap()
        .with_runtime_versioned_claims(&plan)
        .unwrap();
        assert!(request.versioned_coverage.is_some());
        let extra = request.txn_extra.unwrap();
        let txn = fixture.conn.begin().await.unwrap();
        extra
            .apply(
                &txn,
                &TracesCommitCtx {
                    commit_hash: "a".repeat(40),
                    tree_oid: "b".repeat(40),
                    metadata_blob_oid: "c".repeat(40),
                },
            )
            .await
            .unwrap();
        txn.commit().await.unwrap();
        for sql in [
            "SELECT COUNT(*) AS n FROM cov11_checkpoint_insert_count",
            "SELECT COUNT(*) AS n FROM agent_checkpoint",
            "SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE coverage_schema_version = 2",
            "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state = 'catalog_committed' AND coverage_schema_version = 2",
        ] {
            let row = fixture
                .conn
                .query_one_raw(Statement::from_string(
                    fixture.conn.get_database_backend(),
                    sql.to_owned(),
                ))
                .await
                .unwrap()
                .unwrap();
            assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 1, "{sql}");
        }
    }

    #[tokio::test]
    async fn cov2_all_marker_constructors_keep_version() {
        use crate::internal::ai::{
            coverage_gate::{ReservedTurnClaim, VersionedReservedTurnClaim},
            observed_agents::{Completeness, CoverageSchemaVersion},
        };

        let conn = Database::connect("sqlite::memory:")
            .await
            .expect("open constructor fixture");
        let repo = tempfile::tempdir().expect("create constructor repository path");
        let digest_v1 = "a".repeat(64);
        let digest_v2 = "b".repeat(64);
        let claims = vec![
            ReservedTurnClaim {
                logical_turn_key: "shared-turn".to_owned(),
                coverage_digest: digest_v1.clone(),
                completeness: Completeness::Complete,
                fence_token: 3,
                next_revision: 1,
            },
            ReservedTurnClaim {
                logical_turn_key: "shared-turn".to_owned(),
                coverage_digest: digest_v2.clone(),
                completeness: Completeness::Incomplete,
                fence_token: 7,
                next_revision: 2,
            },
        ];
        let ordinary = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: "constructor-session".to_owned(),
            checkpoint_id: "constructor-checkpoint".to_owned(),
            owner: "owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: claims.clone(),
            import_session: None,
            import_identity: None,
            capture_scope: None,
        };
        let versioned = VersionedLiveClaimCommitPlan::new(
            ordinary,
            &[
                VersionedReservedTurnClaim::new(1, claims[0].clone()).expect("v1 claim"),
                VersionedReservedTurnClaim::new(2, claims[1].clone()).expect("v2 claim"),
            ],
        )
        .expect("same key in separate versions is allowed");
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("build ordinary constructor payload");
        let generation = "00000000-0000-4000-8000-000000000000";
        assert!(
            CheckpointWriteRequest::new(
                "constructor-replay",
                "constructor-checkpoint",
                "constructor-session",
                "claude_code",
                None,
                CheckpointScope::Committed,
                generation,
                None,
                &payload,
                Some(&versioned),
                None,
            )
            .is_err(),
            "versioned companion must not bypass the closed request binding"
        );
        let request = CheckpointWriteRequest::new(
            "constructor-replay",
            "constructor-checkpoint",
            "constructor-session",
            "claude_code",
            None,
            CheckpointScope::Committed,
            generation,
            None,
            &payload,
            None,
            None,
        )
        .expect("create ordinary request")
        .with_versioned_claims(&versioned)
        .expect("bind versioned companion through the typed request");
        assert!(request.versioned_coverage.is_some() && request.txn_extra.is_some());
        let fences = [
            TracesCoverageFence {
                coverage_schema_version: CoverageSchemaVersion::V1,
                logical_turn_key: "shared-turn",
                coverage_digest: &digest_v1,
                completeness: Completeness::Complete,
                next_revision: 1,
                owner: "owner",
                fence_token: 3,
                reservation_state: "reserved_live",
            },
            TracesCoverageFence {
                coverage_schema_version: CoverageSchemaVersion::V2,
                logical_turn_key: "shared-turn",
                coverage_digest: &digest_v2,
                completeness: Completeness::Incomplete,
                next_revision: 2,
                owner: "owner",
                fence_token: 7,
                reservation_state: "reserved_live",
            },
        ];
        let scope = CaptureScope {
            repo_id: "constructor-repo".to_owned(),
            worktree_id: String::new(),
            workspace_id: None,
            workspace_fence: None,
        };
        let stores = [
            TracesCheckpointStore::new(
                &conn,
                repo.path(),
                "constructor-session",
                "constructor-checkpoint",
                &fences,
            )
            .expect("fresh constructor"),
            TracesCheckpointStore::new_with_persisted_marker_generation(
                &conn,
                repo.path(),
                "constructor-session",
                "constructor-checkpoint",
                generation,
                &fences,
            )
            .expect("live persisted-generation constructor"),
            TracesCheckpointStore::new_recovery_base(
                &conn,
                repo.path(),
                &scope,
                "constructor-session",
                "constructor-checkpoint",
                generation,
                &fences,
            )
            .expect("recovery constructor shared path"),
        ];
        for store in stores {
            let MarkerRegistration::Required(bound) = &store.registration else {
                panic!("constructor must retain marker fences");
            };
            assert!(registration_matches_versioned_claims(bound, &versioned));
            assert_eq!(bound[0].coverage_schema_version, CoverageSchemaVersion::V1);
            assert_eq!(bound[1].coverage_schema_version, CoverageSchemaVersion::V2);
            assert_eq!(bound[1].coverage_digest, digest_v2);
            assert_eq!(bound[1].next_revision, 2);
        }
    }

    #[test]
    fn pending_reasoning_registration_requires_exact_retained_claims() {
        use crate::internal::ai::{
            coverage_gate::ReservedTurnClaim, observed_agents::coverage::Completeness,
        };

        let coverage = LiveClaimCommitPlan {
            source_channel: "live",
            session_id: "session".to_owned(),
            checkpoint_id: "checkpoint".to_owned(),
            owner: "retained-owner".to_owned(),
            parent_commit: None,
            created_at: 1,
            now_ms: 1,
            claims: vec![ReservedTurnClaim {
                logical_turn_key: "turn-1".to_owned(),
                coverage_digest: "a".repeat(64),
                completeness: Completeness::Complete,
                fence_token: 7,
                next_revision: 1,
            }],
            import_session: None,
            import_identity: None,
            capture_scope: None,
        };
        let registration = vec![OwnedRegistrationFence {
            coverage_schema_version:
                crate::internal::ai::observed_agents::CoverageSchemaVersion::V1,
            logical_turn_key: "turn-1".to_owned(),
            coverage_digest: "a".repeat(64),
            completeness: Completeness::Complete,
            next_revision: 1,
            owner: "retained-owner".to_owned(),
            fence_token: 7,
            reservation_state: "reserved_live".to_owned(),
        }];
        assert!(registration_matches_pending_claims(
            &registration,
            &coverage
        ));
        let mut changed_key = registration.clone();
        changed_key[0].logical_turn_key = "turn-2".to_owned();
        assert!(!registration_matches_pending_claims(
            &changed_key,
            &coverage
        ));
        let mut changed_token = registration.clone();
        changed_token[0].fence_token += 1;
        assert!(!registration_matches_pending_claims(
            &changed_token,
            &coverage
        ));
        let mut changed_owner = registration.clone();
        changed_owner[0].owner = "foreign-owner".to_owned();
        assert!(!registration_matches_pending_claims(
            &changed_owner,
            &coverage
        ));
        let mut changed_state = registration.clone();
        changed_state[0].reservation_state = "abandoned".to_owned();
        assert!(!registration_matches_pending_claims(
            &changed_state,
            &coverage
        ));
        assert!(!registration_matches_pending_claims(&[], &coverage));
        let mut duplicate_plan = coverage;
        duplicate_plan.claims.push(duplicate_plan.claims[0].clone());
        let mut duplicate_registration = registration;
        duplicate_registration.push(duplicate_registration[0].clone());
        assert!(!registration_matches_pending_claims(
            &duplicate_registration,
            &duplicate_plan
        ));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_late_restore_uses_actual_terminal_claim_authority() {
        crate::internal::ai::capture::pending::tests::assert_pending_late_restore_requires_terminal_claim_authority().await;
    }
    use crate::internal::ai::{
        capture::snapshot::CaptureSnapshotService,
        observed_agents::{ExportAuthorized, TranscriptSource},
    };

    fn snapshot() -> CaptureSnapshot {
        let bytes = b"checkpoint secret AKIAABCDEFGHIJKLMNOP".to_vec();
        let auth = ExportAuthorized::issue("claude_code", "session", &bytes);
        CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes { bytes, auth },
            "claude_code",
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

        let locker = crate::internal::db::establish_connection_with_lock_timeout(
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

        let locker = crate::internal::db::establish_connection_with_lock_timeout(
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
        assert_eq!(
            TracesCheckpointStore::replay_status_for(
                &conn,
                None,
                "registration-checkpoint",
                "registration-session",
                CheckpointScope::Committed,
                None,
            )
            .await,
            Ok(CheckpointReplayStatus::Complete),
            "a no-write v1 replay must also settle without coverage tables"
        );
        conn.execute_unprepared("CREATE TABLE agent_coverage_claim (id INTEGER PRIMARY KEY)")
            .await
            .expect("seed a partial modern coverage schema");
        assert!(
            TracesCheckpointStore::replay_status_for(
                &conn,
                None,
                "registration-checkpoint",
                "registration-session",
                CheckpointScope::Committed,
                None,
            )
            .await
            .is_err(),
            "a partial schema must not be misclassified as a frozen v1 catalog"
        );
        conn.execute_unprepared("DROP TABLE agent_coverage_claim")
            .await
            .expect("remove the partial coverage table");
        conn.execute_unprepared(
            "CREATE TABLE schema_versions (
                 version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL
             )",
        )
        .await
        .expect("create migration receipt ledger");
        conn.execute_unprepared(
            "INSERT INTO schema_versions VALUES
                 (2026071301, 'agent_coverage_gate', 'fixture')",
        )
        .await
        .expect("seed modern migration provenance after coverage tables are lost");
        assert_eq!(
            TracesCheckpointStore::replay_status_for(
                &conn,
                None,
                "registration-checkpoint",
                "registration-session",
                CheckpointScope::Committed,
                None,
            )
            .await,
            Err(CheckpointStoreError::ReplayPayloadMismatch),
            "modern missing coverage tables must reject unbound replay"
        );
        assert!(
            traces::register_traces_write_attempt_with_capture_scope(
                &conn,
                None,
                &marker,
                &[],
                Some(CheckpointScope::Committed),
                None,
            )
            .await
            .is_err(),
            "modern missing coverage tables must reject empty registration replay"
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
            let storage = std::sync::Arc::new(
                crate::utils::client_storage::ClientStorage::try_init(self.repo.join("objects"))
                    .expect("storage"),
            );
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

    #[tokio::test]
    #[serial(env)]
    async fn storage_config_upcast_checkpoint_confirmed_type_retires_marker() {
        let _type = crate::utils::test::ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "gcs-canary");
        let fixture = store_replay_fixture().await;
        let result = fixture.write().await;
        let marker = fixture.marker().await;
        assert_eq!(
            result,
            Err(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::ObjectWrite,
            })
        );
        assert!(
            marker.is_none(),
            "ordinary marker must be retired: {marker:?}"
        );
    }

    #[tokio::test]
    #[serial(env)]
    async fn storage_config_upcast_checkpoint_retire_failure_still_returns_object_write() {
        let _type = crate::utils::test::ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "gcs-canary");
        let fixture = store_replay_fixture().await;
        test_support::fail_once_at(test_support::CheckpointFaultPoint::MarkerCleanup);
        let result = fixture.write().await;
        assert_eq!(
            test_support::armed_fault(),
            None,
            "MarkerCleanup must fire while retiring the failed attempt"
        );
        assert_eq!(
            result,
            Err(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::ObjectWrite,
            })
        );
    }

    #[tokio::test]
    #[serial(env)]
    async fn storage_config_upcast_checkpoint_local_word_succeeds() {
        let _type = crate::utils::test::ScopedEnvVar::set("LIBRA_STORAGE_TYPE", "local");
        let fixture = store_replay_fixture().await;
        let result = fixture.write().await;
        assert!(
            result.is_ok(),
            "TYPE=local must keep the historical writable path: {result:?}"
        );
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
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "INSERT INTO agent_coverage_revision
                 (session_id, logical_turn_key, coverage_schema_version, revision,
                  checkpoint_id, coverage_digest, completeness, source_channel, created_at)
                 VALUES (?, 'legacy-turn', 1, 1, ?, ?, 'complete', 'live', 1)",
                [
                    REPLAY_SESSION.into(),
                    REPLAY_CHECKPOINT.into(),
                    "e".repeat(64).into(),
                ],
            ))
            .await
            .expect("seed old committed v1 revision after real checkpoint write");
        assert_eq!(
            fixture.write().await,
            Ok(CheckpointWriteOutcome::ConflictUnchanged {
                reason: CheckpointConflictReason::ReplayPayloadMismatch,
            }),
            "a writer with an authoritative empty expected set must reject the v1 revision"
        );
        assert_eq!(fixture.marker().await, Some(leftover.clone()));
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
        fixture
            .conn
            .execute_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "INSERT INTO agent_coverage_revision
                 (session_id, logical_turn_key, coverage_schema_version, revision,
                  checkpoint_id, coverage_digest, completeness, source_channel, created_at)
                 VALUES (?, 'versioned-turn', 2, 1, ?, ?, 'complete', 'live', 1)",
                [
                    REPLAY_SESSION.into(),
                    REPLAY_CHECKPOINT.into(),
                    "f".repeat(64).into(),
                ],
            ))
            .await
            .expect("seed durable v2 revision on the no-write replay key");
        assert_eq!(
            settle(None).await,
            Err(CheckpointStoreError::ReplayPayloadMismatch),
            "a no-write replay without authoritative fences cannot acknowledge v2"
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
    fn carrier_payload(
        adapter: &dyn crate::internal::ai::observed_agents::ObservedAgent,
    ) -> CheckpointRedactedPayload {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
        let bytes = [b"ordinary ".as_slice(), fixture::CANARY].concat();
        CheckpointRedactedPayload::from_snapshot(
            crate::internal::ai::capture::snapshot::carrier_test_snapshot(adapter, bytes),
            sidecar(br#"{"created_at":1}"#),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("synthetic fixture owns its sanitized component")
    }

    #[tokio::test]
    async fn reasoning_none_store_bytes_unchanged() {
        let fixture = store_replay_fixture().await;
        fixture
            .write()
            .await
            .expect("ordinary incumbent store path");
        assert_eq!(fixture.checkpoint_rows().await, 1);
        let payload = CheckpointRedactedPayload::from_snapshot(
            snapshot(),
            sidecar(b"{}"),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .unwrap();
        assert_eq!(
            payload.transcript().bytes(),
            sidecar(b"checkpoint secret AKIAABCDEFGHIJKLMNOP").bytes()
        );
        assert!(
            prepare_reasoning_artifact_params(&payload, REPLAY_SESSION, "claude_code")
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn reasoning_candidate_absent_from_ordinary_sink() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
        let payload = carrier_payload(&fixture::NATIVE);
        assert_eq!(payload.transcript().bytes(), b"ordinary ");
        assert!(
            !String::from_utf8_lossy(payload.metadata_json().bytes())
                .contains(std::str::from_utf8(fixture::CANARY).unwrap())
        );
        let artifacts =
            prepare_reasoning_artifact_params(&payload, fixture::CAPTURE, "claude_code").unwrap();
        assert_eq!(artifacts[0].bytes.as_bytes(), fixture::CANARY);
    }

    #[cfg(unix)]
    #[test]
    fn typed_capture_constructor_preserves_native_reasoning_disposition() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;

        let payload = reasoning_coverage_test_payload(vec![b"synthetic private wire".to_vec()]);
        let mut projection = payload.snapshot.clone();
        projection
            .source
            .as_mut()
            .expect("synthetic snapshot has a source projection")
            .digest_sha256 = Some(format!("source/hmac-v2/{}", "a".repeat(64)));
        let rebuilt = CheckpointRedactedPayload::from_redacted_capture_with_reasoning(
            payload.transcript,
            Some(projection),
            payload.metadata_json,
            payload.lifecycle_events_jsonl,
            payload.redaction_report_json,
            payload.reasoning,
        )
        .expect("complete typed capture remains a checkpoint payload");

        assert!(matches!(
            rebuilt.reasoning.state(),
            ReasoningSourceState::NativeProven
        ));
        let artifacts =
            prepare_reasoning_artifact_params(&rebuilt, fixture::CAPTURE, "claude_code").unwrap();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].bytes.as_bytes(), b"synthetic private wire");
    }

    #[tokio::test]
    async fn typed_reasoning_live_carrier_reaches_shared_store() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_path_buf();
        let conn = crate::internal::db::create_database(
            repo.join(crate::utils::util::DATABASE).to_str().unwrap(),
        )
        .await
        .unwrap();
        crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
            .await
            .unwrap();
        let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
        conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir,
             metadata_json, started_at, last_event_at, sync_revision, repo_id, worktree_id, scope_state)
             VALUES (?, 'claude_code', ?, 'active', ?, '{}', 1, 1, 1, ?, '', 'scoped')",
            [native::CAPTURE.into(), native::SOURCE.into(), repo.to_string_lossy().into_owned().into(), scope.repo_id.clone().into()])).await.unwrap();
        let fixture = StoreReplayFixture {
            _dir: dir,
            repo,
            conn,
        };
        let payload = carrier_payload(&native::NATIVE);
        let store = TracesCheckpointStore::new(
            &fixture.conn,
            &fixture.repo,
            native::CAPTURE,
            REPLAY_CHECKPOINT,
            &[],
        )
        .unwrap()
        .with_capture_scope(scope);
        let companion = CatalogCompanion(
            CheckpointCatalogMutation::new(
                REPLAY_CHECKPOINT,
                native::CAPTURE,
                CheckpointScope::Committed,
                None,
                1,
            )
            .unwrap(),
        );
        let request = CheckpointWriteRequest::new(
            "og16-native-key",
            REPLAY_CHECKPOINT,
            native::CAPTURE,
            "claude_code",
            None,
            CheckpointScope::Committed,
            store.marker_generation(),
            None,
            &payload,
            Some(&companion),
            None,
        )
        .unwrap();
        let params = store.append_params(&request).unwrap();
        assert_eq!(params.reasoning_artifacts.len(), 2);
        assert_eq!(
            params.reasoning_artifacts[0].bytes.as_bytes(),
            native::CANARY
        );
        assert_eq!(
            params.reasoning_artifacts[1].bytes.as_bytes(),
            b"second-opaque-wire"
        );
        assert_eq!(
            params.reasoning_artifacts[0].locator,
            "claude_code:msg=0/part=0/metadata=signature"
        );
        assert_eq!(
            params.reasoning_artifacts[1].locator,
            "claude_code:msg=0/part=1/metadata=signature"
        );
        assert!(matches!(
            store.write(request).await.unwrap(),
            CheckpointWriteOutcome::Written { .. }
        ));
        let tree_oid: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT tree_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
                [REPLAY_CHECKPOINT.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("tree_oid")
            .unwrap();
        let proofs = crate::internal::ai::history::checkpoint_reasoning_artifact_proofs(
            &fixture.repo,
            &[
                crate::internal::ai::history::ReasoningArtifactCheckpointSpec {
                    checkpoint_id: REPLAY_CHECKPOINT.to_owned(),
                    tree_oid,
                },
            ],
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap();
        let artifacts = proofs[0].artifacts.as_ref().unwrap();
        assert_eq!(artifacts.len(), 2);
        let bytes = artifacts
            .iter()
            .map(|proof| {
                let oid = proof.oid.parse().unwrap();
                crate::utils::object::read_git_object(&fixture.repo, &oid).unwrap()
            })
            .collect::<Vec<_>>();
        assert!(bytes.contains(&native::CANARY.to_vec()));
        assert!(bytes.contains(&b"second-opaque-wire".to_vec()));
    }

    #[test]
    fn native_reasoning_proof_is_bound_to_actual_capture_context() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
        let payload = carrier_payload(&fixture::NATIVE);
        assert!(
            prepare_reasoning_identity_inputs(&payload, "wrong-capture", "claude_code").is_err()
        );
        assert!(prepare_reasoning_identity_inputs(&payload, fixture::CAPTURE, "opencode").is_err());
        let views =
            prepare_reasoning_identity_inputs(&payload, fixture::CAPTURE, "claude_code").unwrap();
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].logical_turn(), "synthetic-turn-0");
        assert_eq!(views[1].logical_turn(), "synthetic-turn-0");
        assert_eq!(views[0].issuer_tag(), "synthetic-test-only-v1");
        assert_eq!(
            *views[0].original_wire_sha256(),
            <[u8; 32]>::from(sha2::Sha256::digest(fixture::CANARY))
        );
        assert_ne!(
            views[0].source_binding_sha256(),
            views[1].source_binding_sha256()
        );
        assert_eq!(
            views[0].original_wire_sha256(),
            &<[u8; 32]>::from(sha2::Sha256::digest(
                prepare_reasoning_artifact_params(&payload, fixture::CAPTURE, "claude_code")
                    .unwrap()[0]
                    .bytes
                    .as_bytes()
            ))
        );
    }

    #[tokio::test]
    async fn native_reasoning_wrong_legal_locator_fails_before_writes() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let fixture = store_replay_fixture().await;
        let mut payload = carrier_payload(&native::NATIVE);
        native::tamper_carrier_locator(&mut payload.reasoning);
        assert!(
            crate::internal::ai::observed_agents::reasoning::artifact_locator_is_valid(
                crate::internal::ai::observed_agents::reasoning::ReasoningProvider::ClaudeCode,
                Some(
                    crate::internal::ai::observed_agents::reasoning::ReasoningSourceKind::Signature
                ),
                "claude_code:msg=0/part=100/metadata=signature"
            )
        );
        let store = TracesCheckpointStore::new(
            &fixture.conn,
            &fixture.repo,
            native::CAPTURE,
            REPLAY_CHECKPOINT,
            &[],
        )
        .unwrap();
        assert!(
            CheckpointWriteRequest::new(
                "og16-native-key",
                REPLAY_CHECKPOINT,
                native::CAPTURE,
                "claude_code",
                None,
                CheckpointScope::Committed,
                store.marker_generation(),
                None,
                &payload,
                None,
                None
            )
            .is_err()
        );
        assert_eq!(fixture.checkpoint_rows().await, 0);
        assert!(fixture.traces_head().await.is_none());
        assert!(fixture.marker().await.is_none());
        assert!(!fixture.repo.join("objects").exists());
    }

    #[test]
    fn reasoning_payload_debug_hides_native_coordinates_and_bytes() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as fixture;
        let payload = carrier_payload(&fixture::NATIVE);
        let debug = format!("{payload:?}");
        for forbidden in [
            fixture::CAPTURE,
            fixture::SOURCE,
            "synthetic-turn-0",
            "metadata=signature",
            std::str::from_utf8(fixture::CANARY).unwrap(),
            "second-opaque-wire",
        ] {
            assert!(!debug.contains(forbidden), "Debug must be content-free");
        }
    }

    #[tokio::test]
    async fn reasoning_carrier_disabled_archive_keeps_safe_sinks() {
        use crate::internal::ai::observed_agents::reasoning::carrier_test_support as native;
        let dir = tempfile::tempdir().unwrap();
        let repo = dir.path().to_path_buf();
        let conn = crate::internal::db::create_database(
            repo.join(crate::utils::util::DATABASE).to_str().unwrap(),
        )
        .await
        .unwrap();
        crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
            .await
            .unwrap();
        let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
        conn.execute_raw(Statement::from_sql_and_values(conn.get_database_backend(),
            "INSERT INTO agent_session (session_id, agent_kind, provider_session_id, state, working_dir,
             metadata_json, started_at, last_event_at, sync_revision, repo_id, worktree_id, scope_state)
             VALUES (?, 'claude_code', ?, 'active', ?, '{}', 1, 1, 1, ?, '', 'scoped')",
            [native::CAPTURE.into(), native::SOURCE.into(), repo.to_string_lossy().into_owned().into(), scope.repo_id.clone().into()])).await.unwrap();
        let fixture = StoreReplayFixture {
            _dir: dir,
            repo,
            conn,
        };
        let before = carrier_payload(&native::NATIVE);
        let store = TracesCheckpointStore::new(
            &fixture.conn,
            &fixture.repo,
            native::CAPTURE,
            REPLAY_CHECKPOINT,
            &[],
        )
        .unwrap()
        .with_capture_scope(scope);
        let companion = CatalogCompanion(
            CheckpointCatalogMutation::new(
                REPLAY_CHECKPOINT,
                native::CAPTURE,
                CheckpointScope::Committed,
                None,
                1,
            )
            .unwrap(),
        );
        let request = CheckpointWriteRequest::new(
            "og16-retained-native-key",
            REPLAY_CHECKPOINT,
            native::CAPTURE,
            "claude_code",
            None,
            CheckpointScope::Committed,
            store.marker_generation(),
            None,
            &before,
            Some(&companion),
            None,
        )
        .unwrap();
        assert!(matches!(
            store.write(request).await.unwrap(),
            CheckpointWriteOutcome::Written { .. }
        ));
        let committed = fixture.catalog_commit().await;
        let tree_oid: String = fixture
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                fixture.conn.get_database_backend(),
                "SELECT tree_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
                [REPLAY_CHECKPOINT.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("tree_oid")
            .unwrap();
        let spec = [
            crate::internal::ai::history::ReasoningArtifactCheckpointSpec {
                checkpoint_id: REPLAY_CHECKPOINT.to_owned(),
                tree_oid,
            },
        ];
        let stored = crate::internal::ai::history::checkpoint_reasoning_artifact_proofs(
            &fixture.repo,
            &spec,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap();
        let original = stored[0].artifacts.as_ref().unwrap();
        assert_eq!(original.len(), 2);
        let original_objects = original
            .iter()
            .map(|proof| {
                let oid = proof.oid.parse().unwrap();
                (
                    proof.oid.clone(),
                    crate::utils::object::read_git_object(&fixture.repo, &oid).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert!(
            original_objects
                .iter()
                .any(|(_, bytes)| bytes.as_slice() == native::CANARY)
        );
        assert!(
            original_objects
                .iter()
                .any(|(_, bytes)| bytes.as_slice() == b"second-opaque-wire")
        );

        // Stop admission while retaining the detector. Refusal must neither
        // publish an ordinary fallback nor mutate the prior stored artifacts.
        let disabled = carrier_payload(&native::STRIP);
        assert!(disabled.authorize_ordinary_pending_payload().is_err());
        assert_eq!(disabled.transcript().bytes(), b"ordinary ");
        assert!(
            prepare_reasoning_artifact_params(&disabled, native::CAPTURE, "claude_code").is_err()
        );
        assert!(
            CheckpointWriteRequest::new(
                "og16-retained-native-key",
                REPLAY_CHECKPOINT,
                native::CAPTURE,
                "claude_code",
                None,
                CheckpointScope::Committed,
                store.marker_generation(),
                None,
                &disabled,
                Some(&companion),
                None,
            )
            .is_err()
        );
        assert_eq!(fixture.catalog_commit().await, committed);
        assert_eq!(fixture.checkpoint_rows().await, 1);
        let retained = crate::internal::ai::history::checkpoint_reasoning_artifact_proofs(
            &fixture.repo,
            &spec,
            Instant::now() + Duration::from_secs(5),
        )
        .await
        .unwrap();
        let retained_objects = retained[0]
            .artifacts
            .as_ref()
            .unwrap()
            .iter()
            .map(|proof| {
                let oid = proof.oid.parse().unwrap();
                (
                    proof.oid.clone(),
                    crate::utils::object::read_git_object(&fixture.repo, &oid).unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            retained_objects, original_objects,
            "disabling admission must preserve persisted artifact OIDs and byte-exact readback"
        );
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn verified_partial_payload_has_no_native_authority() {
        use crate::internal::ai::capture::snapshot::{
            verified_partial_fixture_snapshot, verified_partial_key_fixture,
        };
        let (dir, storage, conn, scope) = verified_partial_key_fixture().await;
        let capture = "opencode__partial-payload";
        let source = "ses_partial_payload";
        let snapshot = verified_partial_fixture_snapshot(capture, source, dir.path());
        assert!(
            CheckpointRedactedPayload::from_redacted_capture_with_reasoning(
                snapshot.transcript().unwrap().clone(),
                Some(snapshot.safe_projection()),
                sidecar(b"{}"),
                sidecar(b"{}\n"),
                sidecar(b"{}"),
                ReasoningSourceDisposition::Unproven
            )
            .is_err()
        );
        let carrier = snapshot
            .try_into_verified_ordinary_partial_in_scope_until(
                &conn,
                &scope,
                &storage,
                dir.path(),
                Instant::now() + Duration::from_secs(10),
            )
            .await
            .unwrap_or_else(|_| panic!("sealed partial fixture"));
        let metadata=serde_json::to_vec(&serde_json::json!({"session_id":capture,"provider_session_id":source,
            "agent_kind":"opencode","scope":"committed","transcript_snapshot":carrier.projection()})).unwrap();
        let payload = CheckpointRedactedPayload::from_verified_ordinary_partial(
            carrier,
            sidecar(&metadata),
            sidecar(b"{}\n"),
            sidecar(b"{}"),
        )
        .expect("storage-only sealed partial factory");
        assert!(!payload.reasoning.has_capture_consumer());
        assert!(!payload.reasoning.is_ordinary());
        assert!(!payload.is_exact_complete_snapshot());
        assert!(payload.authorize_ordinary_pending_payload().is_err());
        assert!(payload.pending_reasoning.is_none());
        assert_eq!(
            payload.provenance,
            CheckpointPayloadProvenance::VerifiedOrdinaryPartial
        );
        assert_eq!(
            payload.snapshot.completeness,
            super::super::snapshot::CaptureSnapshotCompleteness::Partial
        );
        assert_eq!(
            payload.snapshot.partial_reason,
            Some(super::super::snapshot::CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
        );
        assert!(
            prepare_reasoning_artifact_params(&payload, capture, "opencode")
                .unwrap()
                .is_empty()
        );
        assert!(
            prepare_reasoning_identity_inputs(&payload, capture, "opencode")
                .unwrap()
                .is_empty()
        );
        assert!(prepare_reasoning_artifact_params(&payload, "different", "opencode").is_err());
        assert!(prepare_reasoning_artifact_params(&payload, capture, "codex").is_err());
        assert!(prepare_reasoning_identity_inputs(&payload, "different", "opencode").is_err());
        assert!(prepare_reasoning_identity_inputs(&payload, capture, "codex").is_err());
        for (field, replacement) in [
            ("session_id", serde_json::json!("foreign_capture")),
            ("provider_session_id", serde_json::json!("foreign_source")),
            ("agent_kind", serde_json::json!("codex")),
            ("scope", serde_json::json!("uncommitted")),
            ("provider_session_id", serde_json::json!(null)),
        ] {
            let carrier = verified_partial_fixture_snapshot(capture, source, dir.path())
                .try_into_verified_ordinary_partial_in_scope_until(
                    &conn,
                    &scope,
                    &storage,
                    dir.path(),
                    Instant::now() + Duration::from_secs(10),
                )
                .await
                .unwrap_or_else(|_| panic!("metadata identity test carrier"));
            let mut metadata = serde_json::json!({"session_id":capture,"provider_session_id":source,
                "agent_kind":"opencode","scope":"committed","transcript_snapshot":carrier.projection()});
            metadata[field] = replacement;
            let metadata = serde_json::to_vec(&metadata).unwrap();
            assert!(
                CheckpointRedactedPayload::from_verified_ordinary_partial(
                    carrier,
                    sidecar(&metadata),
                    sidecar(b"{}\n"),
                    sidecar(b"{}")
                )
                .is_err(),
                "mismatched metadata {field} cannot consume the carrier"
            );
        }
        assert!(
            prepare_reasoning_identity_inputs_from_disposition(
                &payload.reasoning,
                capture,
                "opencode"
            )
            .is_err()
        );
        assert!(
            prepare_reasoning_artifact_params_from_disposition(
                &payload.reasoning,
                capture,
                "opencode"
            )
            .is_err()
        );
        let mismatch_carrier = verified_partial_fixture_snapshot(capture, source, dir.path())
            .try_into_verified_ordinary_partial_in_scope_until(
                &conn,
                &scope,
                &storage,
                dir.path(),
                Instant::now() + Duration::from_secs(10),
            )
            .await
            .unwrap_or_else(|_| panic!("mismatch carrier bound"));
        let mut mismatch_projection = mismatch_carrier.projection();
        mismatch_projection.transcript_redacted_bytes += 1;
        let mismatch_metadata = serde_json::to_vec(
            &serde_json::json!({"session_id":capture,"provider_session_id":source,
            "agent_kind":"opencode","scope":"committed","transcript_snapshot":mismatch_projection}),
        )
        .unwrap();
        assert!(
            CheckpointRedactedPayload::from_verified_ordinary_partial(
                mismatch_carrier,
                sidecar(&mismatch_metadata),
                sidecar(b"{}\n"),
                sidecar(b"{}")
            )
            .is_err()
        );
        assert!(
            CheckpointWriteRequest::new(
                "replay",
                "checkpoint",
                capture,
                "opencode",
                None,
                CheckpointScope::Committed,
                "generation",
                None,
                &payload,
                None,
                None
            )
            .is_err()
        );
    }
}
