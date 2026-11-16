//! Bounded local replay for authenticated pending terminal artifacts.
//!
//! Discovery headers are never replay authority. This executor authenticates
//! the retained alias and envelope, then asks the catalog to re-elect the
//! original receipt fences before the local-only checkpoint store can write.

use std::path::Path;

use anyhow::{Context, Result};
use sea_orm::{DatabaseConnection, TransactionTrait};

use crate::internal::ai::{
    capture::{
        catalog::{
            CaptureCatalogFinalizerRecovery, CaptureCatalogFinalizerRecoveryResult,
            CaptureCatalogStore, CaptureCatalogTerminalAttempt, PendingArtifactProof,
        },
        checkpoint::{
            CheckpointReplayStatus, CheckpointStore, CheckpointStoreError, CheckpointWriteOutcome,
            TracesCheckpointStore,
        },
        pending::{self, PendingHeader},
        pending_identity::{self, PendingSessionAlias, PreparedPendingAlias},
    },
    capture_scope::CaptureCommitDeadline,
    traces::{CheckpointScope, TracesCoverageFence},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ArtifactRecoveryOutcome {
    Completed,
    AlreadyComplete,
    DeferredByBatchLimit,
    RetryLater,
    ManualRequired,
}

/// Diagnostic reason survives the fixed RetryLater mapping. It grants no
/// completion or repair authority and contains no provider data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ArtifactRecoveryResult {
    pub(crate) outcome: ArtifactRecoveryOutcome,
    pub(crate) waiting_for_children: bool,
}

pub(crate) struct DoctorReplayRequest<'a> {
    pub(crate) conn: &'a DatabaseConnection,
    pub(crate) recovery: &'a CaptureCatalogFinalizerRecovery,
    pub(crate) storage: &'a Path,
    pub(crate) root: &'a Path,
    pub(crate) repo_path: &'a Path,
    pub(crate) now_millis: i64,
    pub(crate) deadline: CaptureCommitDeadline,
    pub(crate) manual: bool,
}

/// Closed dispatch on authenticated loader results, never discovery versions,
/// reasoning-field presence, or the display text of a load error.
enum RecoveryArtifact {
    V1(Box<pending::VerifiedPendingPayload>),
    V2(Box<pending::VerifiedVersionedPendingPayload>),
}
impl RecoveryArtifact {
    fn proof(&self) -> PendingArtifactProof<'_> {
        match self {
            Self::V1(v) => v.as_ref().into(),
            Self::V2(v) => v.as_ref().into(),
        }
    }
    fn ordinary(&self) -> &crate::internal::ai::coverage_gate::LiveClaimCommitPlan {
        match self {
            Self::V1(v) => v.coverage(),
            Self::V2(v) => v.coverage().ordinary(),
        }
    }
    fn claims(
        &self,
    ) -> Vec<(
        crate::internal::ai::observed_agents::CoverageSchemaVersion,
        &crate::internal::ai::coverage_gate::ReservedTurnClaim,
    )> {
        match self {
            Self::V1(v) => v
                .coverage()
                .claims
                .iter()
                .map(|claim| {
                    (
                        crate::internal::ai::observed_agents::CoverageSchemaVersion::V1,
                        claim,
                    )
                })
                .collect(),
            Self::V2(v) => v.coverage().claims().collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CandidateDisposition {
    Selected,
    Deferred,
    Refused,
}

/// A namespace collision is isolated to its key: queue selection already
/// excludes that key and leaves its evidence untouched, so it never poisons
/// unrelated authenticated candidates and is not a disposition input.
fn candidate_disposition(
    checkpoint_id: &str,
    candidate_ids: &[&str],
    window_full: bool,
) -> CandidateDisposition {
    if candidate_ids.contains(&checkpoint_id) {
        CandidateDisposition::Selected
    } else if window_full {
        CandidateDisposition::Deferred
    } else {
        CandidateDisposition::Refused
    }
}

/// Replays one doctor-selected receipt. `manual` is only used by an explicit
/// `doctor --repair` call after the original automatic budget has expired; it
/// spends the catalog's audited one-shot capability and never resets policy.
pub(crate) async fn replay_for_doctor_with_child_status(
    request: DoctorReplayRequest<'_>,
) -> Result<ArtifactRecoveryResult> {
    match replay_for_doctor(request).await {
        Err(error)
            if super::live_checkpoint::child_checkpoint_wait_error(error.as_ref()).is_some() =>
        {
            Ok(ArtifactRecoveryResult {
                outcome: ArtifactRecoveryOutcome::RetryLater,
                waiting_for_children: true,
            })
        }
        result => result.map(|outcome| ArtifactRecoveryResult {
            outcome,
            waiting_for_children: false,
        }),
    }
}

pub(crate) async fn replay_for_doctor(
    request: DoctorReplayRequest<'_>,
) -> Result<ArtifactRecoveryOutcome> {
    let DoctorReplayRequest {
        conn,
        recovery,
        storage,
        root,
        repo_path,
        now_millis,
        deadline,
        manual,
    } = request;
    let current_scope = crate::internal::ai::capture_scope::CaptureScope::resolve(conn, root)
        .await
        .context("resolve current scope before pending-artifact replay")?;
    if current_scope != *recovery.scope() {
        return Ok(ArtifactRecoveryOutcome::RetryLater);
    }
    if current_scope
        .assert_workspace_fence_live(conn)
        .await
        .is_err()
    {
        return Ok(ArtifactRecoveryOutcome::RetryLater);
    }
    if recovery.manual_only() || recovery.superseded() || recovery.quarantined() {
        return Ok(ArtifactRecoveryOutcome::ManualRequired);
    }
    crate::internal::ai::subagent_content::recover_terminal_children(
        conn, storage, recovery, deadline,
    )
    .await?;
    if manual != recovery.budget_exhausted() {
        return Ok(ArtifactRecoveryOutcome::ManualRequired);
    }

    if !manual {
        let mut disposition = CandidateDisposition::Refused;
        for _ in 0..=pending::MAX_ARTIFACTS {
            if std::time::Instant::now() >= deadline.monotonic() {
                return Ok(ArtifactRecoveryOutcome::RetryLater);
            }
            let txn = conn
                .begin()
                .await
                .context("open bounded pending-artifact queue transaction")?;
            let batch = pending::pending_candidates_for_scope(&txn, recovery.scope()).await?;
            if batch.quarantined > 0 {
                tracing::debug!(
                    target: "agent.capture.recovery",
                    quarantined_rows = batch.quarantined,
                    reason = "invalid_pending_header",
                    "invalid capture recovery headers were retained in quarantine"
                );
            }
            if batch.conflicting_namespaces {
                tracing::debug!(
                    target: "agent.capture.recovery",
                    reason = "conflicting_pending_namespace",
                    "colliding capture recovery headers stay excluded for doctor diagnosis"
                );
            }
            txn.commit()
                .await
                .context("commit bounded pending-artifact queue transaction")?;
            let candidate_ids = batch
                .headers
                .iter()
                .map(|candidate| candidate.binding.checkpoint_id.as_str())
                .collect::<Vec<_>>();
            disposition =
                candidate_disposition(recovery.checkpoint_id(), &candidate_ids, batch.window_full);
            if disposition == CandidateDisposition::Selected || batch.quarantined == 0 {
                break;
            }
        }
        match disposition {
            CandidateDisposition::Selected => {}
            CandidateDisposition::Deferred => {
                return Ok(ArtifactRecoveryOutcome::DeferredByBatchLimit);
            }
            CandidateDisposition::Refused => {
                return Ok(ArtifactRecoveryOutcome::ManualRequired);
            }
        }
    }

    let Some((header, namespace)) = pending::header_for_checkpoint(
        conn,
        recovery.scope().repo_id.as_str(),
        recovery.checkpoint_id(),
    )
    .await?
    else {
        return Ok(ArtifactRecoveryOutcome::ManualRequired);
    };
    if header.binding.scope != *recovery.scope()
        || header.binding.checkpoint_id != recovery.checkpoint_id()
        || (manual
            && (namespace != crate::internal::metadata::MetadataScope::AgentCaptureQuarantine
                || header.manual_attempted))
        || (!manual && namespace != crate::internal::metadata::MetadataScope::AgentCapturePending)
    {
        return Ok(ArtifactRecoveryOutcome::ManualRequired);
    }

    let identity = match prepare_existing_identity(conn, &header, storage, root, deadline).await {
        Ok(identity) => identity,
        Err(error) if pending::retryable_load_failure(&error, deadline.monotonic()) => {
            return Ok(ArtifactRecoveryOutcome::RetryLater);
        }
        Err(_) => return Ok(ArtifactRecoveryOutcome::ManualRequired),
    };
    let verified = match pending::load_verified_payload(
        conn,
        storage,
        root,
        &identity,
        &header.binding,
        &header,
        deadline.monotonic(),
    )
    .await
    {
        Ok(verified) => RecoveryArtifact::V1(Box::new(verified)),
        Err(error) if pending::retryable_load_failure(&error, deadline.monotonic()) => {
            return Ok(ArtifactRecoveryOutcome::RetryLater);
        }
        Err(_) => match pending::load_verified_versioned_payload(
            conn,
            storage,
            root,
            &identity,
            &header.binding,
            &header,
            deadline.monotonic(),
        )
        .await
        {
            Ok(verified) => RecoveryArtifact::V2(Box::new(verified)),
            Err(error) if pending::retryable_load_failure(&error, deadline.monotonic()) => {
                return Ok(ArtifactRecoveryOutcome::RetryLater);
            }
            Err(_) => return Ok(ArtifactRecoveryOutcome::ManualRequired),
        },
    };

    // A prior attempt may have committed the ref, checkpoint, claims and
    // native identities but failed to retire its ordinary marker or finish
    // the terminal receipt. Its claims are no longer reserved; asking the
    // pending path to reserve them again would reject a valid durable commit.
    // Classify only after authenticating the original envelope and alias,
    // then let the existing catalog durable-replay path recheck the receipt.
    if matches!(&verified, RecoveryArtifact::V1(v) if v.payload().deferred_pending_reasoning().is_some())
    {
        match TracesCheckpointStore::settle_committed_replay_without_write(
            conn,
            recovery.scope(),
            recovery.checkpoint_id(),
            identity.context().session_id(),
            Some(deadline),
        )
        .await
        {
            Ok(CheckpointReplayStatus::Complete) => {
                return complete_durable_receipt(
                    CaptureCatalogStore::new_until(conn.clone(), deadline),
                    recovery,
                    now_millis,
                )
                .await;
            }
            Ok(CheckpointReplayStatus::PendingCleanup { .. })
            | Err(CheckpointStoreError::DeadlineExceeded) => {
                return Ok(ArtifactRecoveryOutcome::RetryLater);
            }
            Ok(CheckpointReplayStatus::Missing) => {}
            Err(error) => {
                return Err(error).context(
                    "classify durable pending reasoning checkpoint; run `libra agent doctor`",
                );
            }
        }
    }

    let catalog = CaptureCatalogStore::new_until(conn.clone(), deadline);
    let durable_v2 = matches!(&verified, RecoveryArtifact::V2(_))
        && TracesCheckpointStore::durable_replay_exists_for(
            conn,
            recovery.checkpoint_id(),
            identity.context().session_id(),
            CheckpointScope::Committed,
            Some(deadline),
        )
        .await
        .context("classify durable versioned pending checkpoint; run `libra agent doctor`")?;
    let terminal_fence = if durable_v2 {
        let RecoveryArtifact::V2(v) = &verified else {
            return Ok(ArtifactRecoveryOutcome::ManualRequired);
        };
        match catalog
            .authorize_versioned_pending_durable_replay(
                recovery, v, &identity, now_millis, deadline,
            )
            .await
        {
            Ok(fence) => fence,
            Err(crate::internal::ai::capture::catalog::CaptureCatalogError::DeadlineExceeded) => {
                return Ok(ArtifactRecoveryOutcome::RetryLater);
            }
            Err(crate::internal::ai::capture::catalog::CaptureCatalogError::ScopeRejected) => {
                return Ok(ArtifactRecoveryOutcome::ManualRequired);
            }
            Err(error) => {
                return Err(error).context(
                    "revalidate durable versioned pending receipt; run `libra agent doctor`",
                );
            }
        }
    } else if manual {
        match catalog
            .claim_manual_pending_artifact(
                verified.proof(),
                &identity,
                &header,
                now_millis,
                deadline,
            )
            .await
        {
            Ok(fence) => fence,
            Err(crate::internal::ai::capture::catalog::CaptureCatalogError::ScopeRejected) => {
                return Ok(ArtifactRecoveryOutcome::ManualRequired);
            }
            Err(error) => {
                return Err(error).context(
                    "authorize one audited capture repair attempt; run `libra agent doctor`",
                );
            }
        }
    } else {
        let attempt = match catalog
            .claim_pending_artifact_attempt(
                recovery,
                verified.proof(),
                &identity,
                now_millis,
                deadline,
            )
            .await
        {
            Ok(attempt) => attempt,
            Err(crate::internal::ai::capture::catalog::CaptureCatalogError::ScopeRejected) => {
                return Ok(ArtifactRecoveryOutcome::ManualRequired);
            }
            Err(error) => {
                return Err(error)
                    .context("revalidate pending capture receipt; run `libra agent doctor`");
            }
        };
        match attempt {
            CaptureCatalogTerminalAttempt::Bound {
                registration_fence, ..
            } => *registration_fence,
            CaptureCatalogTerminalAttempt::AlreadyComplete
            | CaptureCatalogTerminalAttempt::DurableReplay => {
                // Versioned pending cannot inherit the legacy receipt shortcut:
                // retry from its original artifact to freshly prove the commit.
                if matches!(verified, RecoveryArtifact::V2(_)) {
                    return Ok(ArtifactRecoveryOutcome::RetryLater);
                }
                return complete_durable_receipt(catalog, recovery, now_millis).await;
            }
            CaptureCatalogTerminalAttempt::Quarantined { .. }
            | CaptureCatalogTerminalAttempt::Adopted { .. }
            | CaptureCatalogTerminalAttempt::ConflictUnchanged { .. } => {
                return Ok(ArtifactRecoveryOutcome::ManualRequired);
            }
        }
    };

    let owner = &verified.ordinary().owner;
    let claims = verified.claims();
    let registration_fences = claims
        .iter()
        .map(|(version, claim)| TracesCoverageFence {
            coverage_schema_version: *version,
            logical_turn_key: &claim.logical_turn_key,
            coverage_digest: &claim.coverage_digest,
            completeness: claim.completeness,
            next_revision: claim.next_revision,
            owner,
            fence_token: claim.fence_token,
            reservation_state: "reserved_live",
        })
        .collect::<Vec<_>>();
    let Some((scope, native_session_id, checkpoint_id, marker_generation)) = terminal_fence
        .recovery_checkpoint_binding()
        .map(|(scope, session_id, checkpoint_id, generation)| {
            (
                scope.clone(),
                session_id.to_owned(),
                checkpoint_id,
                generation.to_owned(),
            )
        })
    else {
        return Ok(ArtifactRecoveryOutcome::ManualRequired);
    };
    if &scope != recovery.scope()
        || checkpoint_id != recovery.checkpoint_id()
        || checkpoint_id != header.binding.checkpoint_id
        || native_session_id != identity.context().session_id()
        || marker_generation != header.binding.marker_generation
    {
        return Ok(ArtifactRecoveryOutcome::ManualRequired);
    }

    let checkpoint_store = TracesCheckpointStore::from_terminal_recovery(
        conn,
        repo_path,
        &registration_fences,
        terminal_fence,
    )
    .context("prepare local capture checkpoint recovery; run `libra agent doctor`")?;
    let outcome = match verified {
        RecoveryArtifact::V1(verified) => {
            let request = crate::internal::ai::capture::checkpoint::CheckpointWriteRequest::new(
                &header.binding.action_key,
                &checkpoint_id,
                &native_session_id,
                identity.context().agent_kind(),
                verified.coverage().parent_commit.as_deref(),
                CheckpointScope::Committed,
                &marker_generation,
                None,
                verified.payload(),
                Some(verified.coverage()),
                Some(deadline),
            )
            .context("prepare authenticated capture checkpoint; run `libra agent doctor`")?;
            let request = if verified.payload().deferred_pending_reasoning().is_some() {
                request.with_pending_coverage(verified.coverage(), storage, root).context(
                    "bind authenticated pending reasoning to current claims; run `libra agent doctor`",
                )?
            } else {
                request
            };
            checkpoint_store
                .write(request)
                .await
                .context("write authenticated capture checkpoint; run `libra agent doctor`")?
        }
        RecoveryArtifact::V2(verified) => {
            let (payload, coverage, replay, retained_header, _) = verified.into_replay_parts();
            let payload = payload
                .with_deferred_versioned_pending_reasoning(replay)
                .context(
                    "retain authenticated versioned pending payload; run `libra agent doctor`",
                )?;
            let request = crate::internal::ai::capture::checkpoint::CheckpointWriteRequest::new(
                &header.binding.action_key,
                &checkpoint_id,
                &native_session_id,
                identity.context().agent_kind(),
                coverage.ordinary().parent_commit.as_deref(),
                CheckpointScope::Committed,
                &marker_generation,
                None,
                &payload,
                None,
                Some(deadline),
            )
            .context(
                "prepare authenticated versioned capture checkpoint; run `libra agent doctor`",
            )?
            .with_versioned_pending_coverage(&coverage, &retained_header, storage, root)
            .context("bind versioned pending replay to current claims; run `libra agent doctor`")?;
            if durable_v2 {
                match checkpoint_store
                    .settle_versioned_pending_replay_without_write(&request)
                    .await
                {
                    Ok(CheckpointReplayStatus::Complete) => CheckpointWriteOutcome::AlreadyExists {
                        checkpoint_id: checkpoint_id.clone(),
                    },
                    Ok(CheckpointReplayStatus::PendingCleanup { .. })
                    | Err(CheckpointStoreError::DeadlineExceeded)
                    | Err(CheckpointStoreError::StoreFailure { .. }) => {
                        return Ok(ArtifactRecoveryOutcome::RetryLater);
                    }
                    Ok(CheckpointReplayStatus::Missing) => {
                        return Ok(ArtifactRecoveryOutcome::RetryLater);
                    }
                    Err(error) => {
                        return Err(error).context(
                            "settle durable versioned pending checkpoint; run `libra agent doctor`",
                        );
                    }
                }
            } else {
                checkpoint_store.write(request).await.context(
                    "write authenticated versioned capture checkpoint; run `libra agent doctor`",
                )?
            }
        }
    };
    match outcome {
        CheckpointWriteOutcome::Written { .. } | CheckpointWriteOutcome::AlreadyExists { .. } => {
            complete_durable_receipt(catalog, recovery, now_millis).await
        }
        CheckpointWriteOutcome::TerminalReceiptAlreadyApplied => {
            Ok(ArtifactRecoveryOutcome::AlreadyComplete)
        }
        CheckpointWriteOutcome::AttemptInFlight { .. }
        | CheckpointWriteOutcome::PendingCleanup { .. } => Ok(ArtifactRecoveryOutcome::RetryLater),
        CheckpointWriteOutcome::ConflictUnchanged { .. } => {
            Ok(ArtifactRecoveryOutcome::ManualRequired)
        }
    }
}

async fn complete_durable_receipt(
    catalog: CaptureCatalogStore,
    recovery: &CaptureCatalogFinalizerRecovery,
    now_millis: i64,
) -> Result<ArtifactRecoveryOutcome> {
    match catalog
        .recover_pending_finalizer_after_durable_checkpoint(recovery, now_millis)
        .await
    {
        Ok(CaptureCatalogFinalizerRecoveryResult::Completed) => {
            Ok(ArtifactRecoveryOutcome::Completed)
        }
        Ok(CaptureCatalogFinalizerRecoveryResult::AlreadyComplete) => {
            Ok(ArtifactRecoveryOutcome::AlreadyComplete)
        }
        Ok(_) | Err(crate::internal::ai::capture::catalog::CaptureCatalogError::ScopeRejected) => {
            Ok(ArtifactRecoveryOutcome::ManualRequired)
        }
        Err(error) => {
            Err(error).context("complete durable capture receipt; run `libra agent doctor`")
        }
    }
}

async fn prepare_existing_identity(
    conn: &DatabaseConnection,
    header: &PendingHeader,
    storage: &Path,
    root: &Path,
    deadline: CaptureCommitDeadline,
) -> Result<PreparedPendingAlias> {
    let txn = conn
        .begin()
        .await
        .context("open read-only capture identity snapshot")?;
    let record = pending_identity::lookup(
        &txn,
        &header.binding.scope.repo_id,
        &header.binding.session_id,
    )
    .await?
    .context("retained capture identity is missing")?;
    let context = record
        .resolve(
            &txn,
            &header.binding.scope,
            storage,
            root,
            deadline.monotonic(),
        )
        .await?;
    txn.commit()
        .await
        .context("finish capture identity snapshot")?;
    PendingSessionAlias::prepare(
        conn,
        &context,
        Some(record),
        storage,
        root,
        deadline.monotonic(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::utils::json::normalize_json_value;

    /// Actual export pending receipt replay through the shared authorized
    /// snapshot stage and real catalog/lease/checkpoint stores, no Session.
    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_runtime_claim_replay_version() {
        crate::internal::ai::capture::live_checkpoint::tests::assert_cov2_runtime_claim_replay_version().await;
    }

    #[cfg(unix)]
    mod versioned_fixture {
        use std::{
            path::PathBuf,
            time::{Duration, Instant},
        };

        use sea_orm::{ConnectionTrait, Statement};

        use super::*;
        use crate::internal::{
            ai::{
                capture::{
                    catalog::{
                        CaptureCatalogAction, CaptureCatalogApplyRequest,
                        CaptureCatalogFinalizeRequest, CaptureCatalogFinalizeResult,
                        CaptureCatalogMutation, CaptureCatalogPort, CaptureCatalogSession,
                        resolve_pending_session_context,
                    },
                    checkpoint::{
                        CheckpointRedactedPayload, ReasoningIdentityPreparation,
                        prepare_native_versioned_reasoning_coverage_plan,
                    },
                    finalizer::{
                        CaptureFinalizeMode, CaptureFinalizePolicy, FinalizeCheckpointProgress,
                    },
                    key,
                    pending::{
                        PendingBinding, SealedVersionedPendingArtifact, VersionedPendingSealRequest,
                    },
                    pending_identity::PendingSessionAlias,
                    snapshot::CaptureSnapshotService,
                    state::{
                        CapturePhase, CheckpointWrite, DurableCaptureState, LifecycleReducerInput,
                        reduce_lifecycle,
                    },
                },
                coverage_gate::{
                    LiveClaimCommitPlan, NativeVersionedReasoningClaimCommitPlan,
                    ReservedTurnClaim, VersionedLiveClaimCommitPlan, VersionedReservedTurnClaim,
                    tests::{ReasoningFixture, cov2_native_v2_turn},
                },
                hooks::lifecycle::{
                    CanonicalEventContext, LifecycleEvent, LifecycleEventKind,
                    LifecycleIdentityScheme, lifecycle_event_canonical_json_with_identity,
                },
                observed_agents::{
                    Completeness, ExportAuthorized, RedactedBytes, SemanticRecord,
                    TranscriptSource, encode_coverage_turn_for_schema,
                    reasoning::{
                        ReasoningSourceDisposition, carrier_test_support as native,
                        synthetic_native_batch_for_turns,
                    },
                    select_coverage_schema,
                },
            },
            db,
        };

        pub(super) struct Fixture {
            pub(super) base: ReasoningFixture,
            pub(super) storage: PathBuf,
            pub(super) artifact: SealedVersionedPendingArtifact,
            pub(super) recovery: CaptureCatalogFinalizerRecovery,
            pub(super) ordered: Vec<(i64, String)>,
        }
        impl Fixture {
            pub(super) async fn new() -> Self {
                let base = ReasoningFixture::new().await;
                let storage = base.dir.path().join(".libra");
                std::fs::create_dir_all(storage.join("objects")).unwrap();
                let limit = Instant::now() + Duration::from_secs(60);
                let event_id = uuid::Uuid::new_v4();
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
                let marker_generation = uuid::Uuid::new_v4().to_string();
                let txn = db::begin_write_transaction(&base.conn).await.unwrap();
                let context = resolve_pending_session_context(&txn, &base.scope, native::CAPTURE)
                    .await
                    .unwrap();
                txn.commit().await.unwrap();
                let identity = PendingSessionAlias::prepare(
                    &base.conn,
                    &context,
                    None,
                    &storage,
                    base.dir.path(),
                    limit,
                )
                .await
                .unwrap();
                let bytes = b"complete safe mechanism transcript".to_vec();
                let auth = ExportAuthorized::issue("claude_code", native::SOURCE, &bytes);
                let mut snapshot = CaptureSnapshotService::capture_authorized(
                    TranscriptSource::Bytes { bytes, auth },
                    "claude_code",
                    native::SOURCE,
                    Default::default(),
                );
                let source = key::derive_snapshot_content_commitment_in_scope_until(
                    &base.conn,
                    &base.scope,
                    &storage,
                    base.dir.path(),
                    &snapshot.redacted_digest_preimage().unwrap(),
                    limit,
                )
                .await
                .unwrap();
                assert!(snapshot.bind_source_commitment(source.clone()));
                let binding = PendingBinding {
                    scope: base.scope.clone(),
                    session_id: identity.alias().to_owned(),
                    checkpoint_id: checkpoint_id.clone(),
                    event_id: event_id.to_string(),
                    action_key: action.action_key().to_owned(),
                    receipt_key,
                    marker_generation: marker_generation.clone(),
                    source_commitment: source.clone(),
                    reserved_revision: 2,
                    original_deadline_millis: None,
                    deferrable: true,
                    first_attempt_millis: 1,
                    parent_commit: None,
                    parent_unborn: true,
                };
                let report = serde_json::to_value(snapshot.redaction_report()).unwrap();
                let metadata = serde_json::json!({"schema_version":2,"checkpoint_id":checkpoint_id,"session_id":native::CAPTURE,
                    "provider_session_id":native::SOURCE,"working_dir":context.working_dir(),"agent_kind":"claude_code","scope":"committed",
                    "model":{"name":"mechanism"},"created_at":1,"redaction_report":report,"transcript_snapshot":snapshot.safe_projection(),"extraction":{}});
                let event = lifecycle_event_canonical_json_with_identity(
                    &LifecycleEvent {
                        kind: LifecycleEventKind::SessionEnd,
                        session_id: native::SOURCE.to_owned(),
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
                        session_id: native::CAPTURE,
                        provider_session_id: native::SOURCE,
                        identity_scheme: LifecycleIdentityScheme::NativeReplayHmacV2,
                        provenance: serde_json::json!({"channel":"hook","hook_event_name":"SessionEnd"}),
                    },
                    event_id,
                    false,
                );
                let mut events = serde_json::to_vec(&event).unwrap();
                events.push(b'\n');
                let ordinary = CheckpointRedactedPayload::from_snapshot(
                    snapshot,
                    RedactedBytes::new_unchecked(
                        serde_json::to_vec_pretty(&normalize_json_value(metadata)).unwrap(),
                    ),
                    RedactedBytes::new_unchecked(events),
                    RedactedBytes::new_unchecked(
                        serde_json::to_vec_pretty(&normalize_json_value(report)).unwrap(),
                    ),
                )
                .unwrap();
                let payload = CheckpointRedactedPayload::from_redacted_capture_with_reasoning(
                    RedactedBytes::new_unchecked(ordinary.transcript().bytes().to_vec()),
                    Some(ordinary.snapshot().clone()),
                    RedactedBytes::new_unchecked(ordinary.metadata_json().bytes().to_vec()),
                    RedactedBytes::new_unchecked(
                        ordinary.lifecycle_events_jsonl().bytes().to_vec(),
                    ),
                    RedactedBytes::new_unchecked(ordinary.redaction_report_json().bytes().to_vec()),
                    ReasoningSourceDisposition::NativeProven(synthetic_native_batch_for_turns(
                        native::CAPTURE,
                        native::SOURCE,
                        vec![
                            (0, b"synthetic opaque zero".to_vec()),
                            (1, b"synthetic opaque one".to_vec()),
                        ],
                    )),
                )
                .unwrap();
                let mut v1 =
                    cov2_native_v2_turn("synthetic-turn-1", "ordinary v1", Completeness::Complete);
                v1.records
                    .retain(|record| !matches!(record, SemanticRecord::Reasoning { .. }));
                let turns = [
                    cov2_native_v2_turn(
                        "synthetic-turn-0",
                        "visible typed v2",
                        Completeness::Complete,
                    ),
                    v1,
                ];
                let identity_plan = prepare_native_versioned_reasoning_coverage_plan(
                    &payload,
                    ReasoningIdentityPreparation {
                        conn: &base.conn,
                        scope: &base.scope,
                        storage_path: &storage,
                        authorized_root: base.dir.path(),
                        session_id: native::CAPTURE,
                        provider: "claude_code",
                        turns: &turns,
                        deadline: limit,
                    },
                )
                .await
                .unwrap();
                let claims = turns
                    .iter()
                    .map(|turn| {
                        let version = select_coverage_schema(turn);
                        let digest = encode_coverage_turn_for_schema(version.as_i64(), turn)
                            .unwrap()
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
                        .unwrap()
                    })
                    .collect::<Vec<_>>();
                let ordered = claims
                    .iter()
                    .map(|claim| {
                        (
                            claim.coverage_schema_version.as_i64(),
                            claim.claim.logical_turn_key.clone(),
                        )
                    })
                    .collect();
                for bound in &claims {
                    base.conn.execute_raw(Statement::from_sql_and_values(base.conn.get_database_backend(),
                        "INSERT INTO agent_coverage_claim (session_id,logical_turn_key,coverage_schema_version,coverage_digest,completeness,revision,state,owner,fence_token,source_channel,created_at,updated_at)
                         VALUES (?,?,?,?,'complete',0,'reserved_live','cov08-owner',1,'live',1,1)",
                        [native::CAPTURE.into(),bound.claim.logical_turn_key.clone().into(),bound.coverage_schema_version.as_i64().into(),bound.claim.coverage_digest.clone().into()])).await.unwrap();
                }
                let plan = VersionedLiveClaimCommitPlan::new(
                    LiveClaimCommitPlan {
                        source_channel: "live",
                        session_id: native::CAPTURE.to_owned(),
                        checkpoint_id: checkpoint_id.clone(),
                        owner: "cov08-owner".to_owned(),
                        parent_commit: None,
                        created_at: 1,
                        now_ms: 1,
                        claims: claims.iter().map(|v| v.claim.clone()).collect(),
                        import_session: None,
                        import_identity: None,
                        capture_scope: Some(base.scope.clone()),
                    },
                    &claims,
                )
                .unwrap();
                let companion =
                    NativeVersionedReasoningClaimCommitPlan::new(plan, &identity_plan).unwrap();
                let artifact = SealedVersionedPendingArtifact::seal(
                    &base.conn,
                    &storage,
                    base.dir.path(),
                    VersionedPendingSealRequest {
                        binding,
                        identity,
                        payload: &payload,
                        companion: &companion,
                    },
                    limit,
                )
                .await
                .unwrap();
                let txn = db::begin_write_transaction(&base.conn).await.unwrap();
                artifact.persist(&txn, limit).await.unwrap();
                txn.commit().await.unwrap();
                let active = DurableCaptureState {
                    phase: CapturePhase::Active,
                    stopped_at: None,
                    sync_revision: 1,
                };
                let reduced = reduce_lifecycle(LifecycleReducerInput {
                    current: Some(active),
                    event_kind: LifecycleEventKind::SessionEnd,
                    event_id,
                    occurred_at: 1_700_000_000,
                    deadline: None,
                })
                .unwrap();
                let mutation =
                    CaptureCatalogMutation::from_reducer(Some(active), &reduced, 1_700_000_000)
                        .unwrap();
                let session = CaptureCatalogSession::new(
                    native::CAPTURE,
                    "claude_code",
                    native::SOURCE,
                    base.dir.path().to_string_lossy(),
                )
                .unwrap();
                let apply = CaptureCatalogApplyRequest::new(
                    base.scope.clone(),
                    session,
                    action.clone(),
                    mutation,
                )
                .unwrap();
                let catalog = CaptureCatalogStore::new(base.conn.clone());
                catalog.apply(&apply).await.unwrap();
                let policy = CaptureFinalizePolicy::new(
                    None,
                    CaptureFinalizeMode::Deferrable,
                    action.action_key(),
                )
                .unwrap();
                let finalize = CaptureCatalogFinalizeRequest::from_apply(
                    &apply,
                    policy,
                    marker_generation,
                    Some(source),
                    1,
                    FinalizeCheckpointProgress::NotStarted,
                )
                .unwrap();
                assert!(matches!(
                    catalog.finalize(&finalize).await.unwrap(),
                    CaptureCatalogFinalizeResult::Pending { .. }
                ));
                let recovery = catalog
                    .pending_finalizer_recoveries_for_doctor(2)
                    .await
                    .unwrap()
                    .recoveries
                    .into_iter()
                    .find(|r| r.checkpoint_id() == checkpoint_id)
                    .unwrap();
                Self {
                    base,
                    storage,
                    artifact,
                    recovery,
                    ordered,
                }
            }
            pub(super) async fn unknown_issuer_with_valid_mac(&self) {
                use base64::{Engine, engine::general_purpose::STANDARD};

                use crate::internal::metadata::{MetadataKv, MetadataScope, MetadataValueType};
                let mut value: serde_json::Value =
                    serde_json::from_slice(self.artifact.bytes()).unwrap();
                value["payload"]["reasoning_source"]["items"][0]["proof"]["issuer"] =
                    "unregistered".into();
                let envelope: pending::VersionedPendingEnvelope =
                    serde_json::from_value(value).unwrap();
                let bytes = serde_json::to_vec(&envelope).unwrap();
                let header = pending::header_for_checkpoint(
                    &self.base.conn,
                    &self.base.scope.repo_id,
                    self.recovery.checkpoint_id(),
                )
                .await
                .unwrap()
                .unwrap()
                .0;
                let mut header = serde_json::to_value(header).unwrap();
                assert_eq!(
                    header["chunks"], 1,
                    "small pure fixture is one actual chunk"
                );
                let mac = self
                    .base
                    .scope
                    .sign_pending_envelope_until(
                        &self.base.conn,
                        &self.storage,
                        self.base.dir.path(),
                        &bytes,
                        Instant::now() + Duration::from_secs(60),
                    )
                    .await
                    .unwrap();
                header["mac"] = mac.into();
                header["envelope_bytes"] = bytes.len().into();
                // Retain the original header owner's canonical struct order;
                // an arbitrary Value map would fail before issuer validation.
                let header: pending::PendingHeader = serde_json::from_value(header).unwrap();
                MetadataKv::set_with_conn(
                    &self.base.conn,
                    MetadataScope::AgentCapturePendingChunk,
                    &self.base.scope.repo_id,
                    &format!("{}:000", self.recovery.checkpoint_id()),
                    &STANDARD.encode(&bytes),
                    MetadataValueType::Binary,
                )
                .await
                .unwrap();
                MetadataKv::set_with_conn(
                    &self.base.conn,
                    MetadataScope::AgentCapturePending,
                    &self.base.scope.repo_id,
                    self.recovery.checkpoint_id(),
                    &serde_json::to_string(&header).unwrap(),
                    MetadataValueType::Text,
                )
                .await
                .unwrap();
            }
            pub(super) async fn replay_until(
                &self,
                deadline: CaptureCommitDeadline,
            ) -> Result<ArtifactRecoveryOutcome> {
                let helper = crate::utils::test::real_libra_helper_binary();
                assert!(
                    helper.is_file(),
                    "build the existing Libra object-I/O test helper before this mechanism fixture"
                );
                crate::internal::ai::authorized_read::with_test_helper_program(
                    helper,
                    replay_for_doctor(DoctorReplayRequest {
                        conn: &self.base.conn,
                        recovery: &self.recovery,
                        storage: &self.storage,
                        root: self.base.dir.path(),
                        repo_path: &self.storage,
                        now_millis: 2,
                        deadline,
                        manual: false,
                    }),
                )
                .await
            }
            pub(super) async fn replay(&self) -> Result<ArtifactRecoveryOutcome> {
                self.replay_until(
                    CaptureCommitDeadline::from_budget(Duration::from_secs(60)).unwrap(),
                )
                .await
            }
            pub(super) async fn rows(&self, tables: &[&str]) -> Vec<Vec<String>> {
                let mut result = Vec::new();
                for table in tables {
                    let columns = self
                        .base
                        .conn
                        .query_all_raw(Statement::from_string(
                            self.base.conn.get_database_backend(),
                            format!("PRAGMA table_info(\"{table}\")"),
                        ))
                        .await
                        .unwrap();
                    let fields = columns
                        .iter()
                        .flat_map(|r| {
                            let name = r.try_get_by::<String, _>("name").unwrap();
                            [format!("quote(\"{name}\")"), format!("typeof(\"{name}\")")]
                        })
                        .collect::<Vec<_>>()
                        .join(",");
                    let rows=self.base.conn.query_all_raw(Statement::from_string(self.base.conn.get_database_backend(),format!("SELECT json_array({fields}) AS record FROM \"{table}\" ORDER BY rowid"))).await.unwrap();
                    result.push(
                        rows.iter()
                            .map(|r| r.try_get_by::<String, _>("record").unwrap())
                            .collect(),
                    );
                }
                result
            }
            pub(super) async fn pending_rows(&self) -> Vec<Vec<String>> {
                let rows=self.base.conn.query_all_raw(Statement::from_string(self.base.conn.get_database_backend(),
                    "SELECT scope,key,hex(CAST(value AS BLOB)) AS value,typeof(value) AS sql_type,value_type,created_at,updated_at FROM metadata_kv
                     WHERE scope IN ('agent_capture_pending','agent_capture_quarantine','agent_capture_pending_chunk','agent_capture_session_alias') ORDER BY scope,target,key".to_owned())).await.unwrap();
                vec![
                    rows.iter()
                        .map(|r| {
                            [
                                "scope",
                                "key",
                                "value",
                                "sql_type",
                                "value_type",
                                "created_at",
                                "updated_at",
                            ]
                            .iter()
                            .map(|name| r.try_get_by::<String, _>(*name).unwrap())
                            .collect::<Vec<_>>()
                            .join("|")
                        })
                        .collect(),
                ]
            }
            pub(super) async fn no_ack(&self) {
                self.no_ack_with_competing_head(false).await;
            }
            pub(super) async fn no_ack_with_competing_head(&self, competing_head: bool) {
                for table in ["agent_checkpoint", "agent_coverage_revision", "reflog"] {
                    assert!(
                        self.rows(&[table]).await[0].is_empty(),
                        "unexpected ACK in {table}"
                    );
                }
                if competing_head {
                    // The existing real CAS fault publishes only a competing
                    // writer's commit, never this attempt's checkpoint tree.
                    assert_eq!(self.rows(&["reference"]).await[0].len(), 1);
                    let storage = std::sync::Arc::new(
                        crate::utils::client_storage::ClientStorage::init_local_existing(
                            self.storage.join("objects"),
                        ),
                    );
                    let manager = crate::internal::ai::history::HistoryManager::for_traces(
                        storage,
                        self.storage.clone(),
                        std::sync::Arc::new(self.base.conn.clone()),
                    );
                    assert!(manager.resolve_history_head().await.unwrap().is_some());
                    assert_eq!(manager.list_object_types().await.unwrap(), ["competing"]);
                } else {
                    assert!(
                        self.rows(&["reference"]).await[0].is_empty(),
                        "this failed attempt must not move the traces ref"
                    );
                }
                let row=self.base.conn.query_one_raw(Statement::from_string(self.base.conn.get_database_backend(),"SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_reasoning_identity'".to_owned())).await.unwrap().unwrap();
                assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 0);
                let row = self
                    .base
                    .conn
                    .query_one_raw(Statement::from_sql_and_values(
                        self.base.conn.get_database_backend(),
                        "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                        [native::CAPTURE.into()],
                    ))
                    .await
                    .unwrap()
                    .unwrap();
                let metadata: serde_json::Value =
                    serde_json::from_str(&row.try_get_by::<String, _>("metadata_json").unwrap())
                        .unwrap();
                assert_eq!(
                    metadata["capture_catalog_receipts_v1"]["entries"][0]["status"], "pending",
                    "zero ACK requires the receipt remain pending"
                );
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_replay_preserves_claim_version() {
        use sea_orm::{ConnectionTrait, Statement};
        let f = versioned_fixture::Fixture::new().await;
        let original = f.pending_rows().await;
        let header = pending::header_for_checkpoint(
            &f.base.conn,
            &f.base.scope.repo_id,
            f.recovery.checkpoint_id(),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
        let identity = prepare_existing_identity(
            &f.base.conn,
            &header,
            &f.storage,
            f.base.dir.path(),
            CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(60)).unwrap(),
        )
        .await
        .unwrap();
        let verified = pending::load_verified_versioned_payload(
            &f.base.conn,
            &f.storage,
            f.base.dir.path(),
            &identity,
            &header.binding,
            &header,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert_eq!(
            verified
                .coverage()
                .claims()
                .map(|(v, c)| (v.as_i64(), c.logical_turn_key.clone()))
                .collect::<Vec<_>>(),
            f.ordered
        );
        assert_eq!(
            f.ordered,
            [
                (2, "synthetic-turn-0".to_owned()),
                (1, "synthetic-turn-1".to_owned())
            ]
        );
        assert_eq!(
            f.replay().await.unwrap(),
            ArtifactRecoveryOutcome::Completed
        );
        let rows=f.base.conn.query_all_raw(Statement::from_string(f.base.conn.get_database_backend(),"SELECT coverage_schema_version,logical_turn_key FROM agent_coverage_revision ORDER BY coverage_schema_version DESC".to_owned())).await.unwrap();
        assert_eq!(
            rows.iter()
                .map(|r| (
                    r.try_get_by::<i64, _>("coverage_schema_version").unwrap(),
                    r.try_get_by::<String, _>("logical_turn_key").unwrap()
                ))
                .collect::<Vec<_>>(),
            f.ordered
        );
        assert_eq!(
            f.rows(&["agent_checkpoint", "reference"])
                .await
                .iter()
                .map(Vec::len)
                .collect::<Vec<_>>(),
            [1, 1]
        );
        let maps = f
            .base
            .conn
            .query_all_raw(Statement::from_string(
                f.base.conn.get_database_backend(),
                "SELECT key FROM metadata_kv WHERE scope = 'agent_reasoning_identity' ORDER BY key"
                    .to_owned(),
            ))
            .await
            .unwrap();
        assert_eq!(
            maps.len(),
            2,
            "both source-derived schema domains commit with the checkpoint"
        );
        assert!(!original[0].is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_replay_owner_fence_revision_fail_closed() {
        use sea_orm::{ConnectionTrait, Statement};
        for assignment in [
            "owner = 'takeover-owner'",
            "fence_token = fence_token + 1",
            "revision = revision + 1",
        ] {
            let f = versioned_fixture::Fixture::new().await;
            f.base.conn.execute_raw(Statement::from_string(f.base.conn.get_database_backend(),format!("UPDATE agent_coverage_claim SET {assignment} WHERE coverage_schema_version = 2"))).await.unwrap();
            let before = f
                .rows(&[
                    "agent_checkpoint",
                    "agent_coverage_claim",
                    "agent_coverage_revision",
                    "reference",
                    "reflog",
                    "agent_session",
                    "metadata_kv",
                    "agent_audit_log",
                ])
                .await;
            let result = f.replay().await;
            assert!(
                !matches!(
                    result,
                    Ok(ArtifactRecoveryOutcome::Completed
                        | ArtifactRecoveryOutcome::AlreadyComplete)
                ),
                "{assignment} acknowledged stale ownership"
            );
            f.no_ack().await;
            assert_eq!(
                f.rows(&[
                    "agent_checkpoint",
                    "agent_coverage_claim",
                    "agent_coverage_revision",
                    "reference",
                    "reflog",
                    "agent_session",
                    "metadata_kv",
                    "agent_audit_log"
                ])
                .await,
                before,
                "{assignment} changed retained rows/types/ref/receipt/marker"
            );
        }
        // A takeover after authenticated load and successful marker registration
        // must also be rejected by the actual ref-CAS transaction companion.
        for assignment in [
            "owner = 'late-takeover'",
            "fence_token = fence_token + 1",
            "revision = revision + 1",
        ] {
            use crate::internal::ai::capture::checkpoint::test_support;
            let f = versioned_fixture::Fixture::new().await;
            let original = f.pending_rows().await;
            let pause = test_support::pause_after_registration_once(&f.storage);
            let takeover = async {
                tokio::time::timeout(
                    std::time::Duration::from_secs(20),
                    pause.wait_until_entered(),
                )
                .await
                .unwrap();
                f.base.conn.execute_raw(Statement::from_string(f.base.conn.get_database_backend(),format!("UPDATE agent_coverage_claim SET {assignment} WHERE coverage_schema_version = 2"))).await.unwrap();
                let claims = f.rows(&["agent_coverage_claim"]).await;
                pause.release();
                claims
            };
            let (result, claims) = tokio::join!(f.replay(), takeover);
            assert!(
                !matches!(
                    result,
                    Ok(ArtifactRecoveryOutcome::Completed
                        | ArtifactRecoveryOutcome::AlreadyComplete)
                ),
                "{assignment} late ACK"
            );
            f.no_ack().await;
            assert_eq!(f.rows(&["agent_coverage_claim"]).await, claims);
            assert_eq!(
                f.pending_rows().await,
                original,
                "late takeover must retain original bytes/MAC/alias"
            );
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_failed_or_interrupted_replay_keeps_pending() {
        use sea_orm::{ConnectionTrait, Statement};

        use crate::internal::ai::capture::checkpoint::test_support::{self, CheckpointFaultPoint};
        // Actual late member verifier failure after successful loader+marker.
        let f = versioned_fixture::Fixture::new().await;
        let before = f.pending_rows().await;
        test_support::fail_versioned_pending_member_once();
        assert!(f.replay().await.is_err());
        assert!(
            !test_support::take_versioned_pending_member_substitution(),
            "late verifier seam must have fired"
        );
        f.no_ack().await;
        assert_eq!(f.pending_rows().await, before);
        // Production interruption points must retain original artifact, never ACK.
        for fault in [
            CheckpointFaultPoint::AfterMarkerRegistration,
            CheckpointFaultPoint::AfterObjectWrite,
            CheckpointFaultPoint::RefCasExhausted,
            CheckpointFaultPoint::CompanionTransaction,
        ] {
            let f = versioned_fixture::Fixture::new().await;
            let before = f.pending_rows().await;
            let claims = f.rows(&["agent_coverage_claim"]).await;
            test_support::fail_once_at(fault);
            let result = f.replay().await;
            assert_eq!(test_support::armed_fault(), None, "{fault:?} not reached");
            assert!(!matches!(
                result,
                Ok(ArtifactRecoveryOutcome::Completed | ArtifactRecoveryOutcome::AlreadyComplete)
            ));
            f.no_ack_with_competing_head(fault == CheckpointFaultPoint::RefCasExhausted)
                .await;
            assert_eq!(
                f.rows(&["agent_coverage_claim"]).await,
                claims,
                "{fault:?} changed claims without an ACK"
            );
            assert_eq!(
                f.pending_rows().await,
                before,
                "{fault:?} lost original committed artifact"
            );
        }
        let f = versioned_fixture::Fixture::new().await;
        let original = f.pending_rows().await;
        let pause = test_support::pause_after_registration_once(&f.storage);
        let rotate = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(20),
                pause.wait_until_entered(),
            )
            .await
            .unwrap();
            let path = f
                .storage
                .join(crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_DIR)
                .join(crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_FILE);
            let old = std::fs::read(&path).unwrap();
            let mut changed = old.clone();
            changed[0] ^= 1;
            std::fs::write(&path, &changed).unwrap();
            pause.release();
            old
        };
        let (result, key) = tokio::join!(f.replay(), rotate);
        assert!(
            result.is_err(),
            "key replacement after load must reject original authentication"
        );
        std::fs::write(
            f.storage
                .join(crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_DIR)
                .join(crate::internal::ai::capture::key::CAPTURE_DEDUP_SECRET_FILE),
            key,
        )
        .unwrap();
        f.no_ack().await;
        assert_eq!(f.pending_rows().await, original);
        let f = versioned_fixture::Fixture::new().await;
        let original = f.pending_rows().await;
        let pause = test_support::pause_after_registration_once(&f.storage);
        let deadline =
            CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(5)).unwrap();
        let expire = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(4),
                pause.wait_until_entered(),
            )
            .await
            .unwrap();
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.monotonic())).await;
            pause.release();
        };
        let (result, ()) = tokio::join!(f.replay_until(deadline), expire);
        assert!(!matches!(
            result,
            Ok(ArtifactRecoveryOutcome::Completed | ArtifactRecoveryOutcome::AlreadyComplete)
        ));
        f.no_ack().await;
        assert_eq!(
            f.pending_rows().await,
            original,
            "deadline must retain every original chunk/header/alias"
        );

        // A committed checkpoint still needs its receipt settled. Transient
        // catalog/cleanup failures must leave it retryable, never quarantine it.
        for catalog_failure in [true, false] {
            let f = versioned_fixture::Fixture::new().await;
            test_support::fail_once_at(CheckpointFaultPoint::MarkerCleanup);
            assert_eq!(
                f.replay().await.unwrap(),
                ArtifactRecoveryOutcome::RetryLater
            );
            assert_eq!(test_support::armed_fault(), None);
            let protected = [
                "agent_checkpoint",
                "agent_coverage_claim",
                "agent_coverage_revision",
                "reference",
                "reflog",
                "agent_session",
                "metadata_kv",
                "agent_audit_log",
            ];
            let before = f.rows(&protected).await;
            let pending_before = f.pending_rows().await;
            if catalog_failure {
                use crate::internal::ai::capture::catalog::{
                    CaptureCatalogError, test_support as catalog_test_support,
                };
                catalog_test_support::fail_versioned_durable_authorization_once();
                let error = f.replay().await.unwrap_err();
                assert!(
                    !catalog_test_support::take_versioned_durable_authorization_failure(),
                    "the committed replay must reach durable catalog authorization"
                );
                assert!(error.chain().any(|cause| matches!(
                    cause.downcast_ref::<CaptureCatalogError>(),
                    Some(CaptureCatalogError::Database)
                )));
                assert!(
                    pending::retryable_load_failure(
                        &error,
                        std::time::Instant::now() + std::time::Duration::from_secs(60)
                    ),
                    "doctor must classify the propagated catalog error as RetryLater"
                );
            } else {
                f.base
                    .conn
                    .execute_raw(Statement::from_string(
                        f.base.conn.get_database_backend(),
                        "CREATE TRIGGER cov08_transient_marker_cleanup
                         BEFORE DELETE ON metadata_kv
                         WHEN OLD.scope = 'agent_traces_inflight'
                         BEGIN SELECT RAISE(FAIL, 'transient marker cleanup failure'); END"
                            .to_owned(),
                    ))
                    .await
                    .unwrap();
                assert_eq!(
                    f.replay().await.unwrap(),
                    ArtifactRecoveryOutcome::RetryLater,
                    "a real marker-cleanup SQL failure must not require manual repair"
                );
                f.base
                    .conn
                    .execute_raw(Statement::from_string(
                        f.base.conn.get_database_backend(),
                        "DROP TRIGGER cov08_transient_marker_cleanup".to_owned(),
                    ))
                    .await
                    .unwrap();
            }
            assert_eq!(f.rows(&protected).await, before);
            assert_eq!(f.pending_rows().await, pending_before);
            assert_eq!(
                f.replay().await.unwrap(),
                ArtifactRecoveryOutcome::Completed,
                "the same committed artifact must settle once the transient fault clears"
            );
        }
        // Expire only after a durable retry reaches late authentication. This
        // must stay pending/retryable rather than becoming a permanent refusal.
        let f = versioned_fixture::Fixture::new().await;
        test_support::fail_once_at(CheckpointFaultPoint::MarkerCleanup);
        assert_eq!(
            f.replay().await.unwrap(),
            ArtifactRecoveryOutcome::RetryLater
        );
        assert_eq!(test_support::armed_fault(), None);
        let protected = [
            "agent_checkpoint",
            "agent_coverage_claim",
            "agent_coverage_revision",
            "reference",
            "reflog",
            "agent_session",
            "metadata_kv",
            "agent_audit_log",
        ];
        let before = f.rows(&protected).await;
        let pending_before = f.pending_rows().await;
        let pause = test_support::pause_after_registration_once(&f.storage);
        let deadline =
            CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(10)).unwrap();
        let expire_durable_authentication = async {
            tokio::time::timeout(
                std::time::Duration::from_secs(8),
                pause.wait_until_entered(),
            )
            .await
            .unwrap();
            tokio::time::sleep_until(tokio::time::Instant::from_std(deadline.monotonic())).await;
            pause.release();
        };
        let (result, ()) = tokio::join!(f.replay_until(deadline), expire_durable_authentication);
        assert_eq!(result.unwrap(), ArtifactRecoveryOutcome::RetryLater);
        assert_eq!(f.rows(&protected).await, before);
        assert_eq!(f.pending_rows().await, pending_before);
        assert_eq!(
            f.replay().await.unwrap(),
            ArtifactRecoveryOutcome::Completed
        );

        // Commit occurred but ordinary-marker cleanup interrupted: exact checked
        // no-write replay authenticates native identities, not reserved claims.
        let f = versioned_fixture::Fixture::new().await;
        test_support::fail_once_at(CheckpointFaultPoint::MarkerCleanup);
        assert_eq!(
            f.replay().await.unwrap(),
            ArtifactRecoveryOutcome::RetryLater
        );
        assert_eq!(test_support::armed_fault(), None);
        let durable = f
            .rows(&[
                "agent_checkpoint",
                "agent_coverage_claim",
                "agent_coverage_revision",
                "reference",
                "reflog",
            ])
            .await;
        assert_eq!(durable[0].len(), 1);
        let identities = f.base.conn.query_all_raw(Statement::from_string(f.base.conn.get_database_backend(),
            "SELECT target,key,value FROM metadata_kv WHERE scope = 'agent_reasoning_identity' ORDER BY target,key".to_owned())).await.unwrap();
        f.base
            .conn
            .execute_raw(Statement::from_string(
                f.base.conn.get_database_backend(),
                "UPDATE metadata_kv SET value = '{}' WHERE scope = 'agent_reasoning_identity'"
                    .to_owned(),
            ))
            .await
            .unwrap();
        let protected = [
            "agent_checkpoint",
            "agent_coverage_claim",
            "agent_coverage_revision",
            "reference",
            "reflog",
            "agent_session",
            "metadata_kv",
            "agent_audit_log",
        ];
        let rejected_before = f.rows(&protected).await;
        let rejected = f.replay().await;
        assert!(
            !matches!(
                rejected,
                Ok(ArtifactRecoveryOutcome::Completed | ArtifactRecoveryOutcome::AlreadyComplete)
            ),
            "an existing checkpoint is not native authority for corrupted committed identity"
        );
        assert_eq!(
            f.rows(&protected).await,
            rejected_before,
            "failed durable proof must not retire the marker or complete receipt"
        );
        for record in identities {
            f.base.conn.execute_raw(Statement::from_sql_and_values(f.base.conn.get_database_backend(),
                "UPDATE metadata_kv SET value = ? WHERE scope = 'agent_reasoning_identity' AND target = ? AND key = ?",
                [record.try_get_by::<String,_>("value").unwrap().into(),record.try_get_by::<String,_>("target").unwrap().into(),record.try_get_by::<String,_>("key").unwrap().into()])).await.unwrap();
        }
        assert_eq!(
            f.replay().await.unwrap(),
            ArtifactRecoveryOutcome::Completed
        );
        assert_eq!(
            f.rows(&[
                "agent_checkpoint",
                "agent_coverage_claim",
                "agent_coverage_revision",
                "reference",
                "reflog"
            ])
            .await,
            durable,
            "durable retry must not append twice"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_authenticated_pending_has_no_native_authority() {
        use crate::internal::ai::capture::checkpoint::{
            CheckpointStore, CheckpointStoreError, CheckpointWriteRequest,
        };
        let f = versioned_fixture::Fixture::new().await;
        let header = pending::header_for_checkpoint(
            &f.base.conn,
            &f.base.scope.repo_id,
            f.recovery.checkpoint_id(),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
        let identity = prepare_existing_identity(
            &f.base.conn,
            &header,
            &f.storage,
            f.base.dir.path(),
            CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(60)).unwrap(),
        )
        .await
        .unwrap();
        let verified = pending::load_verified_versioned_payload(
            &f.base.conn,
            &f.storage,
            f.base.dir.path(),
            &identity,
            &header.binding,
            &header,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
        let early_store = TracesCheckpointStore::new_with_persisted_marker_generation(
            &f.base.conn,
            &f.storage,
            identity.context().session_id(),
            f.recovery.checkpoint_id(),
            &header.binding.marker_generation,
            &[],
        )
        .unwrap()
        .with_capture_scope(f.base.scope.clone());
        let early = CheckpointWriteRequest::new(
            &header.binding.action_key,
            &header.binding.checkpoint_id,
            identity.context().session_id(),
            "claude_code",
            None,
            CheckpointScope::Committed,
            &header.binding.marker_generation,
            None,
            verified.payload(),
            None,
            Some(CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(60)).unwrap()),
        )
        .unwrap();
        assert_eq!(
            early_store.write(early).await.unwrap_err(),
            CheckpointStoreError::InvalidRequest {
                field: "pending reasoning claim plan".to_owned()
            },
            "same-marker early payload must reach the sidecar-loss barrier"
        );
        let (payload, coverage, replay, header, _) = verified.into_replay_parts();
        // The successfully MAC-authenticated DTO remains ordinary/deferred.
        assert!(
            payload.authorize_ordinary_pending_payload().is_err(),
            "early redacted projection must not shed the versioned deferred barrier"
        );
        let payload = payload
            .with_deferred_versioned_pending_reasoning(replay)
            .unwrap();
        assert!(payload.authorize_ordinary_pending_payload().is_err());
        let fences = coverage
            .claims()
            .map(|(version, claim)| TracesCoverageFence {
                coverage_schema_version: version,
                logical_turn_key: &claim.logical_turn_key,
                coverage_digest: &claim.coverage_digest,
                completeness: claim.completeness,
                next_revision: claim.next_revision,
                owner: &coverage.ordinary().owner,
                fence_token: claim.fence_token,
                reservation_state: "reserved_live",
            })
            .collect::<Vec<_>>();
        // Match the actual marker and claims so this negative reaches the
        // missing terminal-authority check, rather than a stale random marker.
        let store = TracesCheckpointStore::new_with_persisted_marker_generation(
            &f.base.conn,
            &f.storage,
            identity.context().session_id(),
            f.recovery.checkpoint_id(),
            &header.binding.marker_generation,
            &fences,
        )
        .unwrap()
        .with_capture_scope(f.base.scope.clone());
        let req = || {
            CheckpointWriteRequest::new(
                &header.binding.action_key,
                &header.binding.checkpoint_id,
                identity.context().session_id(),
                "claude_code",
                None,
                CheckpointScope::Committed,
                &header.binding.marker_generation,
                None,
                &payload,
                None,
                Some(
                    CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(60)).unwrap(),
                ),
            )
            .unwrap()
        };
        assert_eq!(
            store.write(req()).await.unwrap_err(),
            CheckpointStoreError::InvalidRequest {
                field: "pending reasoning claim plan".to_owned()
            },
            "same-marker attached payload must reach the sidecar-loss barrier"
        );
        assert!(req().with_versioned_claims(&coverage).is_err());
        assert_eq!(
            store
                .write(
                    req()
                        .with_versioned_pending_coverage(
                            &coverage,
                            &header,
                            &f.storage,
                            f.base.dir.path()
                        )
                        .unwrap()
                )
                .await
                .unwrap_err(),
            CheckpointStoreError::InvalidRequest {
                field: "pending reasoning terminal fence".to_owned()
            },
            "same-marker integrity-only payload must reach the missing-terminal fence"
        );
        f.no_ack().await;
        let f = versioned_fixture::Fixture::new().await;
        f.unknown_issuer_with_valid_mac().await;
        let before = f.pending_rows().await;
        let header = pending::header_for_checkpoint(
            &f.base.conn,
            &f.base.scope.repo_id,
            f.recovery.checkpoint_id(),
        )
        .await
        .unwrap()
        .unwrap()
        .0;
        let identity = prepare_existing_identity(
            &f.base.conn,
            &header,
            &f.storage,
            f.base.dir.path(),
            CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(60)).unwrap(),
        )
        .await
        .unwrap();
        let authenticated = pending::load_verified_versioned_payload(
            &f.base.conn,
            &f.storage,
            f.base.dir.path(),
            &identity,
            &header.binding,
            &header,
            std::time::Instant::now() + std::time::Duration::from_secs(60),
        )
        .await
        .unwrap();
        assert_eq!(
            authenticated.coverage().claims().count(),
            2,
            "integrity-only load is a real positive even for an unknown issuer"
        );
        let error = f.replay().await.unwrap_err();
        assert_eq!(
            error.downcast_ref::<CheckpointStoreError>(),
            Some(&CheckpointStoreError::InvalidRequest {
                field: "pending reasoning fixed native source proof".to_owned()
            }),
            "loader-positive unknown issuer must reach the lower fixed-source issuer after late authentication"
        );
        f.no_ack().await;
        assert_eq!(f.pending_rows().await, before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_actual_borrowed_recovery_request_reauthenticates_each_attempt() {
        super::super::pending::tests::assert_actual_borrowed_recovery_reauthenticates_each_attempt(
        )
        .await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_v1_recovery_settles_nonempty_prior_revision() {
        use crate::internal::ai::{
            coverage_gate::tests::ReasoningFixture,
            observed_agents::reasoning::carrier_test_support as native,
            traces::{self, TracesInflightMarker},
        };

        let fixture = ReasoningFixture::new().await;
        let checkpoint_id = "legacy-v1-pending-recovery";
        fixture
            .seed_legacy_v1_checkpoint_revision(checkpoint_id)
            .await;
        let marker = TracesInflightMarker::new(
            native::CAPTURE,
            checkpoint_id,
            chrono::Utc::now().timestamp_millis(),
        );
        traces::write_traces_inflight_marker(&fixture.conn, &marker)
            .await
            .expect("seed old recovery's ordinary marker");
        let deadline = CaptureCommitDeadline::from_budget(std::time::Duration::from_secs(5))
            .expect("recovery test deadline");
        assert_eq!(
            TracesCheckpointStore::settle_committed_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                checkpoint_id,
                native::CAPTURE,
                Some(deadline),
            )
            .await
            .expect("classify old v1 recovery's committed checkpoint"),
            CheckpointReplayStatus::Complete,
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &fixture.conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                native::CAPTURE,
                checkpoint_id,
            )
            .await
            .expect("read old recovery marker")
            .is_none(),
            "recovery's ordinary marker must be retired"
        );
    }

    #[test]
    fn bounded_queue_selects_defers_and_fails_closed() {
        let ids = ["one", "two", "three", "four", "five"];
        assert_eq!(
            candidate_disposition("three", &ids, true),
            CandidateDisposition::Selected
        );
        assert_eq!(
            candidate_disposition("three", &ids, false),
            CandidateDisposition::Selected
        );
        assert_eq!(
            candidate_disposition("six", &ids, true),
            CandidateDisposition::Deferred
        );
        assert_eq!(
            candidate_disposition("six", &ids[..4], false),
            CandidateDisposition::Refused
        );
        assert_eq!(
            candidate_disposition("six", &ids[..4], true),
            CandidateDisposition::Deferred
        );
    }
}
