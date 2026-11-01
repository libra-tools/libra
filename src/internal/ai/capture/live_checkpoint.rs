//! Live checkpoint writers and their reservation settlement (ADR-ACF-10,
//! ACF-19).
//!
//! `capture::live_pipeline` calls into this module once a catalog reservation
//! exists: the committed and subagent checkpoint writers (with their source,
//! coverage, export and checkpoint stages), terminal recovery-artifact
//! eligibility, the coverage-claim cleanup helpers, no-checkpoint settlement,
//! and the extraction and redaction metadata helpers. Durable effects go
//! through the capture coordinator and the catalog/checkpoint stores.
//!
//! ACF-20: the export-job runner lease is a `capture::live::LiveExportRunner`
//! token, settled at exactly two sites — `run_export_stage` (source/coverage
//! exits, `ExportStageExit`) and `run_checkpoint_stage` (checkpoint exits,
//! `CheckpointStageExit`) — through the exhaustive mappings
//! `export_stage_lease_disposition` / `checkpoint_stage_lease_disposition`.
//! The transcript itself comes from the provider's `LiveTranscriptExporter`
//! capability; this module names no provider.

use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use chrono::Utc;
use serde_json::json;

#[cfg(test)]
use super::live_pipeline::test_support;
use crate::{
    internal::ai::{
        capture::{
            catalog::{
                CaptureCatalogApplyRequest, CaptureCatalogError, CaptureCatalogStore,
                CaptureCatalogTerminalAttempt,
            },
            checkpoint::{
                CheckpointConflictReason, CheckpointRedactedPayload, CheckpointReplayStatus,
                CheckpointStoreError, CheckpointWriteRequest, ReasoningCoveragePlan,
                ReasoningIdentityPreparation, ReasoningIdentityReadContext,
                SubagentCheckpointCommitPlan, TracesCheckpointStore,
            },
            coordinator::{
                CaptureCoordinator, CaptureCoordinatorError, CaptureCoordinatorFinalizer,
                CaptureCoordinatorOutcome, CaptureCoordinatorRequest,
                CaptureCoordinatorReserveOutcome, CoordinatorReservationClass,
                DurableFinalizerDisposition,
            },
            finalizer::{CaptureFinalizeMode, CaptureFinalizePolicy},
            key::derive_snapshot_content_commitment_in_scope_until,
            live::{
                LeaseDisposition, LeaseLogContext, LiveCaptureReservation, LiveExportLeasePort,
                LiveExportRunner, LiveExportTarget, latest_committed_checkpoint_id,
                record_live_checkpoint_write_failure_until, redact_session_id,
                reserve_capture_catalog_until,
            },
            snapshot::{
                CaptureSnapshot, CaptureSnapshotCompleteness, CaptureSnapshotPartialReason,
                CaptureSnapshotPolicy, CaptureSnapshotProjection, CaptureSnapshotService,
                DeadlineExtractionResult, VerifiedOrdinaryPartialSnapshot,
            },
        },
        capture_scope::{CaptureCommitDeadline, CaptureScope},
        coverage_gate::{
            CaptureReservationExecution, CommittedTurnClaimSnapshot, ReasoningCoverageInput,
            ReservedTurnClaim, VersionedReservedTurnClaim,
        },
        hooks::lifecycle::{
            CanonicalEventContext, LifecycleEvent, LifecycleEventKind, LifecycleIdentityScheme,
            SessionHookEnvelope, lifecycle_event_canonical_json_with_identity,
        },
        observed_agents::{
            RedactedBytes, TranscriptSource,
            live_capture::{
                LiveCandidateUnavailable, LiveCaptureBinding, LiveCaptureContext,
                LiveCoverageNormalizer, LiveTranscriptExporter,
            },
            reasoning::ReasoningSourceDisposition,
        },
    },
    utils::{json::normalize_json_value, util},
};

/// Recognize only typed child waiting states, including coordinator fields
/// deliberately absent from Error::source(). Never classify by display text.
pub(crate) fn child_checkpoint_wait_error<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a CaptureCatalogError> {
    let mut current = Some(error);
    while let Some(cause) = current {
        if let Some(catalog) = cause.downcast_ref::<CaptureCatalogError>()
            && matches!(
                catalog,
                CaptureCatalogError::ChildCheckpointsPending
                    | CaptureCatalogError::ChildGateCapacity
            )
        {
            return Some(catalog);
        }
        if let Some(coordinator) = cause.downcast_ref::<CaptureCoordinatorError>() {
            let nested = match coordinator {
                CaptureCoordinatorError::DurableCheckpointCompletion { cause, .. }
                | CaptureCoordinatorError::CheckpointReplayCompletion { cause, .. } => {
                    Some(cause.as_ref())
                }
                CaptureCoordinatorError::CheckpointWriteFinalizer { .. }
                | CaptureCoordinatorError::CheckpointConflictFinalizer { .. } => {
                    // The real checkpoint failure/conflict stays causal.
                    // Its incidental finalizer wait must not suppress the
                    // original reservation and failure accounting.
                    return None;
                }
                _ => None,
            };
            if let Some(wait) = nested.and_then(|error| child_checkpoint_wait_error(error)) {
                return Some(wait);
            }
        }
        current = cause.source();
    }
    None
}

/// The coverage claims one committed-checkpoint attempt may have reserved
/// before its checkpoint. Whether they are abandoned is decided only by
/// `claim_owner`: it is `None` before a coverage reservation and after
/// `persist_pending_artifact` hands the claims to a durable recovery
/// artifact. The export-job runner lease is not part of this value; its
/// `LiveExportRunner` token settles it (ADR-ACF-10, ACF-20).
struct CheckpointReservationCleanup<'a> {
    session_id: &'a str,
    checkpoint_id: &'a str,
    scope: &'a CaptureScope,
    claim_channel: &'static str,
    claim_owner: Option<&'a str>,
}

/// Claims and the optional source-derived identity plan travel together from
/// reservation to the existing checkpoint transaction. The plan is never
/// persisted before the claim/ref CAS companion commits.
pub(super) struct CaptureClaimReservation {
    owner: String,
    now_ms: i64,
    claims: Vec<ReservedTurnClaim>,
    versioned_claims: Vec<VersionedReservedTurnClaim>,
    reasoning: Option<Box<ReasoningCoveragePlan>>,
}

async fn cleanup_failed_checkpoint_side_effects(
    conn: &sea_orm::DatabaseConnection,
    diagnostic_request: &CaptureCatalogApplyRequest,
    cleanup: &CheckpointReservationCleanup<'_>,
    deadline: Option<CaptureCommitDeadline>,
) {
    record_live_checkpoint_write_failure_until(conn.clone(), diagnostic_request, deadline).await;
    release_reserved_checkpoint_side_effects(conn, cleanup, deadline).await;
}

/// Release pre-checkpoint coverage reservations on a terminal outcome that
/// deliberately did not attempt a checkpoint. Unlike a real store failure,
/// `Adopted`, `AlreadyComplete`, and durable quarantine have their own
/// catalog evidence, so recording a retryable checkpoint diagnostic here
/// would falsely imply that the elected writer failed.
async fn release_reserved_checkpoint_side_effects(
    conn: &sea_orm::DatabaseConnection,
    cleanup: &CheckpointReservationCleanup<'_>,
    deadline: Option<CaptureCommitDeadline>,
) {
    if let Some(owner) = cleanup.claim_owner {
        let release = match deadline {
            Some(deadline) => {
                crate::internal::ai::coverage_gate::abandon_reserved_turn_claims_with_capture_scope_until(
                    conn,
                    cleanup.scope,
                    cleanup.session_id,
                    owner,
                    cleanup.claim_channel,
                    chrono::Utc::now().timestamp_millis(),
                    deadline,
                )
                .await
            }
            None => {
                crate::internal::ai::coverage_gate::abandon_reserved_turn_claims_with_capture_scope(
                    conn,
                    cleanup.scope,
                    cleanup.session_id,
                    owner,
                    cleanup.claim_channel,
                    chrono::Utc::now().timestamp_millis(),
                )
                .await
            }
        };
        if release.is_err() {
            tracing::warn!(
                checkpoint_id = %cleanup.checkpoint_id,
                reason = "release_claims_after_checkpoint_failed",
                "failed to release claims after checkpoint write failure"
            );
        }
    }
}

/// Consume a catalog reservation when source/coverage work proves that this
/// lifecycle action has no checkpoint to append. Nonterminal receipts are
/// completed through the coordinator's typed catalog edge; terminal receipts
/// remain bounded, durable finalizer work until a real checkpoint can supply
/// the strict proof. No hook path may acknowledge a pre-reserved native
/// delivery while silently leaving its receipt pending.
pub(super) async fn settle_preapplied_without_checkpoint(
    conn: &sea_orm::DatabaseConnection,
    catalog_reservation: LiveCaptureReservation,
    catalog_request: &CaptureCatalogApplyRequest,
    finalizer_deadline_millis: Option<i64>,
    terminal_settlement_deadline: Option<CaptureCommitDeadline>,
) -> Result<()> {
    if catalog_request.mutation().is_terminal() {
        // The reservation that advanced the lifecycle state deliberately
        // carries the primary capture deadline. Do not use that expired port
        // to write a terminal recovery fence. Re-enter the opaque
        // coordinator protocol under the one dispatch-established grace pair;
        // `apply` can only resume the already-pending receipt here, then the
        // finalizer write remains content-free. This never authorizes source
        // or checkpoint work after the primary budget.
        let (catalog_reservation, effective_request) = if let Some(settlement_deadline) =
            terminal_settlement_deadline
        {
            drop(catalog_reservation);
            let resumed = match reserve_capture_catalog_until(
                conn.clone(),
                catalog_request,
                Some(settlement_deadline),
            )
            .await
            .context("resume terminal pending receipt within its settlement deadline")?
            {
                CaptureCoordinatorReserveOutcome::Reserved(reservation) => *reservation,
                CaptureCoordinatorReserveOutcome::AlreadyApplied => return Ok(()),
                CaptureCoordinatorReserveOutcome::ConflictUnchanged { conflict } => {
                    bail!(
                        "terminal pending receipt lost its catalog fence during settlement: {conflict:?}"
                    );
                }
            };
            let effective_request = resumed.effective_request().clone();
            (resumed, effective_request)
        } else {
            (catalog_reservation, catalog_request.clone())
        };
        let policy: CaptureFinalizePolicy = CaptureFinalizePolicy::new(
            finalizer_deadline_millis,
            CaptureFinalizeMode::Deferrable,
            effective_request.action().action_key(),
        )
        .map_err(|error| anyhow!("prepare no-checkpoint terminal finalizer policy: {error}"))?;
        match catalog_reservation
            .persist_terminal_pending_without_checkpoint(
                &effective_request,
                CaptureCoordinatorFinalizer::new(policy, finalizer_now_millis()),
            )
            .await
            .context("settle no-checkpoint terminal capture receipt")?
        {
            CaptureCoordinatorOutcome::FinalizerPending { .. }
            | CaptureCoordinatorOutcome::AlreadyApplied => Ok(()),
            CaptureCoordinatorOutcome::FinalizerQuarantined { reason } => {
                bail!("capture terminal finalizer requires repair: {reason:?}");
            }
            CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
                bail!("capture terminal finalizer lost its catalog fence: {conflict:?}");
            }
            outcome => bail!(
                "capture coordinator returned an invalid no-checkpoint terminal outcome: {outcome:?}"
            ),
        }
    } else {
        match catalog_reservation
            .complete_preapplied_without_checkpoint(catalog_request)
            .await
            .context("settle no-checkpoint capture receipt")?
        {
            CaptureCoordinatorOutcome::StateApplied | CaptureCoordinatorOutcome::AlreadyApplied => {
                Ok(())
            }
            CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
                bail!("no-checkpoint capture coordinator lost its catalog fence: {conflict:?}");
            }
            outcome => {
                bail!("capture coordinator returned an invalid no-checkpoint outcome: {outcome:?}");
            }
        }
    }
}

/// Finish the narrow cross-channel replay case where coverage has proven that
/// every turn is already durable and the catalog reservation began from an
/// existing terminal session. This is not a substitute for a terminal
/// checkpoint: the coordinator and catalog both require the prior stopped
/// state plus an unbound provisional finalizer before they will merely
/// acknowledge the new native receipt.
async fn settle_preapplied_covered_terminal_replay(
    catalog_reservation: LiveCaptureReservation,
    catalog_request: &CaptureCatalogApplyRequest,
) -> Result<()> {
    match catalog_reservation
        .complete_preapplied_terminal_coverage_replay(catalog_request)
        .await
        .context("settle covered terminal capture replay")?
    {
        CaptureCoordinatorOutcome::StateApplied | CaptureCoordinatorOutcome::AlreadyApplied => {
            Ok(())
        }
        CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
            bail!("covered terminal capture replay lost its catalog fence: {conflict:?}");
        }
        outcome => {
            bail!(
                "capture coordinator returned an invalid covered-terminal replay outcome: {outcome:?}"
            );
        }
    }
}

/// How the checkpoint store resolved a coverage no-op's own replay evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CoveredCheckpointReplay {
    /// The store classified (and, if needed, retired) this action's replay
    /// evidence; the no-op may settle its receipt.
    Settled,
    /// The capture deadline elapsed before the store could classify it. No
    /// recovery evidence was erased; the caller applies its deadline policy.
    DeadlineElapsed,
}

/// A coverage no-op settles its receipt without a checkpoint write. When this
/// action's own earlier delivery committed the covering checkpoint but could
/// not retire its ordinary writer marker, that marker must not outlive the
/// completed receipt until its TTL or `libra agent doctor`. Let the
/// checkpoint store — the same replay authority its write path consults —
/// settle this action's durable replay first, so a failure outcome resolves
/// identically whether or not the replay found a coverage-parseable source.
/// A marker that still owns repairable objects keeps the receipt pending,
/// exactly as the store-write replay does.
async fn settle_covered_checkpoint_replay(
    conn: &sea_orm::DatabaseConnection,
    scope: &CaptureScope,
    libra_session_id: &str,
    checkpoint_id: &str,
    capture_deadline: Option<CaptureCommitDeadline>,
) -> Result<CoveredCheckpointReplay> {
    match TracesCheckpointStore::settle_committed_replay_without_write(
        conn,
        scope,
        checkpoint_id,
        libra_session_id,
        capture_deadline,
    )
    .await
    {
        Ok(CheckpointReplayStatus::Missing | CheckpointReplayStatus::Complete) => {
            Ok(CoveredCheckpointReplay::Settled)
        }
        Ok(CheckpointReplayStatus::PendingCleanup { .. }) => {
            bail!("capture checkpoint write needs durable cleanup before completion")
        }
        Err(CheckpointStoreError::DeadlineExceeded) => Ok(CoveredCheckpointReplay::DeadlineElapsed),
        Err(CheckpointStoreError::ReplayPayloadMismatch) => bail!(
            "capture coordinator left the checkpoint unchanged: {:?}",
            CheckpointConflictReason::ReplayPayloadMismatch
        ),
        Err(error) => Err(anyhow!("settle covered capture checkpoint replay: {error}")),
    }
}

async fn settle_runtime_covered_checkpoint_replay(
    conn: &sea_orm::DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    checkpoint_id: &str,
    deadline: CaptureCommitDeadline,
    covered: &[CommittedTurnClaimSnapshot],
) -> Result<CoveredCheckpointReplay> {
    match TracesCheckpointStore::settle_runtime_covered_replay_without_write(
        conn,
        scope,
        checkpoint_id,
        session_id,
        Some(deadline),
        covered,
    )
    .await
    {
        Ok(CheckpointReplayStatus::Missing | CheckpointReplayStatus::Complete) => {
            Ok(CoveredCheckpointReplay::Settled)
        }
        Ok(CheckpointReplayStatus::PendingCleanup { .. }) => {
            bail!("capture checkpoint needs durable cleanup; run libra agent doctor")
        }
        Err(CheckpointStoreError::DeadlineExceeded) => Ok(CoveredCheckpointReplay::DeadlineElapsed),
        Err(error) => Err(anyhow!(
            "cannot settle versioned checkpoint replay: {error}; run libra agent doctor"
        )),
    }
}

async fn legacy_export_replay_exit(
    conn: &sea_orm::DatabaseConnection,
    scope: &CaptureScope,
    session_id: &str,
    checkpoint_id: &str,
    deadline: CaptureCommitDeadline,
) -> std::result::Result<Option<PreparedExportSource>, ExportStageExit> {
    let durable = TracesCheckpointStore::durable_legacy_replay_exists_for(
        conn,
        scope,
        checkpoint_id,
        session_id,
        Some(deadline),
    )
    .await
    .map_err(|error| {
        ExportStageExit::CoveredReplayFailed(anyhow!(
            "cannot verify legacy capture replay: {error}; run libra agent doctor"
        ))
    })?;
    if !durable {
        return Ok(None);
    }
    match settle_covered_checkpoint_replay(conn, scope, session_id, checkpoint_id, Some(deadline))
        .await
    {
        Ok(CoveredCheckpointReplay::Settled) => {
            Ok(Some(PreparedExportSource::DurableActionReplay {
                keep_export_dirty: true,
            }))
        }
        Ok(CoveredCheckpointReplay::DeadlineElapsed) => Err(ExportStageExit::CoveredReplayFailed(
            anyhow!("legacy capture replay exceeded its deadline"),
        )),
        Err(error) => Err(ExportStageExit::CoveredReplayFailed(error)),
    }
}

pub(super) fn finalizer_now_millis() -> i64 {
    Utc::now().timestamp_millis()
}

/// ADR-ACF-09a write trigger: only a terminal, exact-complete snapshot read
/// through the live channel becomes a replayable artifact. A transcript
/// export snapshot's claims belong to its export-job lease and the artifact
/// coverage codec accepts live claims only, so export SessionEnd keeps the
/// synchronous checkpoint path instead of failing closed at seal.
pub(super) fn terminal_snapshot_artifact_eligible(
    terminal: bool,
    exact_complete_snapshot: bool,
    source_channel: &str,
    export_lease_held: bool,
) -> bool {
    terminal && exact_complete_snapshot && source_channel == "live" && !export_lease_held
}

/// Content-free, retryable outcome for a terminal delivery whose stable
/// receipt is already owned by a retained recovery artifact.
fn retained_terminal_artifact_error() -> anyhow::Error {
    tracing::warn!(
        target: "agent.capture.recovery",
        reason = "terminal_artifact_retained",
        "terminal capture is already retained for local recovery; this delivery left it unchanged"
    );
    anyhow!(
        "the complete terminal snapshot for this event is already retained for local recovery; \
         start a new agent session to replay it automatically or run `libra agent doctor --repair`"
    )
}

/// Acquire the live transcript snapshot for a committed checkpoint
/// (ADR-ACF-10, ACF-18). The provider's live capability may derive a
/// deterministic candidate from the verified cwd and native session id; the
/// snapshot service alone opens, bounds and redacts it. Returns the safe
/// projection and, for a complete source, its redacted transcript; the
/// snapshot's redaction report is merged into `report_value` here.
#[derive(Debug, thiserror::Error)]
#[error(
    "reasoning source preparation has no ready capture consumer; keep the receipt pending and retry after the consumer is available"
)]
pub(super) struct SourcePreparationRejected;

async fn acquire_live_snapshot(
    conn: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    scope: &CaptureScope,
    binding: LiveCaptureBinding,
    context: &LiveCaptureContext<'_>,
    capture_deadline: Option<CaptureCommitDeadline>,
    report_value: &mut serde_json::Value,
) -> (
    CaptureSnapshotProjection,
    Option<crate::internal::ai::observed_agents::RedactedBytes>,
    crate::internal::ai::observed_agents::reasoning::ReasoningSourceDisposition,
) {
    let agent_kind = binding.agent_kind_db();
    let adapter = binding.observed();
    // Never use a provider-supplied transcript pointer. A provider whose
    // native layout is deterministically scoped by the verified cwd and
    // provider session id derives its candidate through its live capability;
    // other adapters remain source-absent until they can prove an equivalent
    // derivation.
    #[cfg(test)]
    let snapshot_deadline = (!test_support::live_capture_without_deadline())
        .then_some(capture_deadline)
        .flatten()
        .map(CaptureCommitDeadline::monotonic);
    #[cfg(not(test))]
    let snapshot_deadline = capture_deadline.map(CaptureCommitDeadline::monotonic);
    let locator = binding.live_transcript_locator();
    let candidate = if let Some(locator) = locator {
        locator.locate(context, snapshot_deadline).await
    } else {
        binding.live_transcript_candidate(context)
    };
    let locator_failed = locator.is_some()
        && candidate.is_err()
        && snapshot_deadline.is_some_and(|deadline| Instant::now() < deadline);
    let transcript_path = match candidate {
        Ok(path) => path,
        Err(LiveCandidateUnavailable) => {
            tracing::warn!(
                agent_kind,
                reason = "derived_transcript_path_unavailable",
                "skipping unverified live transcript source"
            );
            None
        }
    };
    let seam_ctx = crate::internal::ai::observed_agents::AgentSessionCtx {
        session_id: context.libra_session_id.to_string(),
        provider_session_id: context.provider_session_id.to_string(),
        working_dir: context.verified_cwd.to_path_buf(),
        transcript_path,
    };
    let mut snapshot = if locator_failed {
        CaptureSnapshotService::live_locator_unavailable()
    } else if let Some(source_capability) = locator {
        CaptureSnapshotService::capture_live_with_source_until(
            adapter,
            &seam_ctx,
            CaptureSnapshotPolicy::with_deadline(snapshot_deadline),
            source_capability,
        )
        .await
    } else {
        CaptureSnapshotService::capture_live_until(
            adapter,
            &seam_ctx,
            CaptureSnapshotPolicy::with_deadline(snapshot_deadline),
        )
        .await
    };
    if snapshot.rejects_capture_source_consumer() {
        return snapshot.into_checkpoint_parts();
    }
    // The helper's redacted SHA-256 is a transient preimage only. Before
    // a live snapshot can reach checkpoint metadata or a terminal
    // finalizer, bind it to this repository's scoped HMAC capability.
    // Production callbacks reuse their dispatch-established absolute
    // deadline; only unbounded in-process callers receive a small local
    // cap so tests and non-hook tooling cannot wait indefinitely.
    if snapshot.transcript().is_some() {
        let commitment_deadline = capture_deadline
            .map(CaptureCommitDeadline::monotonic)
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(5));
        let (commitment, commitment_failure_reason) = match (
            snapshot.redacted_digest_preimage(),
            util::try_get_storage_path(Some(repo_path.to_path_buf())),
        ) {
            (Some(preimage), Ok(storage_path)) => {
                match derive_snapshot_content_commitment_in_scope_until(
                    conn,
                    scope,
                    &storage_path,
                    repo_path,
                    &preimage,
                    commitment_deadline,
                )
                .await
                {
                    Ok(commitment) => (Some(commitment), None),
                    Err(_) if Instant::now() >= commitment_deadline => {
                        (None, Some(CaptureSnapshotPartialReason::DeadlineExceeded))
                    }
                    Err(_) => (
                        None,
                        Some(CaptureSnapshotPartialReason::SourceCommitmentUnavailable),
                    ),
                }
            }
            _ => (
                None,
                Some(CaptureSnapshotPartialReason::SourceCommitmentUnavailable),
            ),
        };
        if !commitment.is_some_and(|commitment| snapshot.bind_source_commitment(commitment)) {
            // Never persist a complete live source under the helper's
            // unkeyed redacted checksum. The typed event fallback stays
            // available below, but source provenance remains absent.
            snapshot.downgrade_for_commitment_failure(
                commitment_failure_reason
                    .unwrap_or(CaptureSnapshotPartialReason::SourceCommitmentUnavailable),
            );
            tracing::warn!(
                agent_kind,
                reason = "live_source_commitment_unavailable",
                "live transcript snapshot is partial because its scoped source commitment could not be established"
            );
        }
    } else {
        // A deadline may elapse after the helper has calculated its
        // transient redacted checksum but before it can return a
        // complete snapshot. Preserve that typed partial reason, while
        // removing the helper-only checksum before this projection can
        // enter fallback checkpoint metadata.
        snapshot.clear_non_durable_source_digest();
    }
    let projection = snapshot.safe_projection();
    if let Some(reason) = snapshot.partial_reason() {
        tracing::warn!(
            agent_kind,
            reason = ?reason,
            "live transcript snapshot is partial; using the redacted event fallback"
        );
    }
    merge_redaction_report_into(report_value, snapshot.redaction_report());
    let (_, transcript, reasoning) = snapshot.into_checkpoint_parts();
    (projection, transcript, reasoning)
}

/// Outcome of the DR-05c-0 live coverage stage (ADR-ACF-10, ACF-18).
enum LiveCoverageStage {
    /// The stage consumed the catalog reservation: an elapsed deadline left
    /// the receipt to its content-free settlement, or every turn was already
    /// covered and the no-op receipt settled. The writer returns `Ok(())`.
    Settled,
    /// Continue to the checkpoint with the still-owned reservation (boxed,
    /// as in `CaptureCoordinatorReserveOutcome::Reserved`) and the live claims
    /// this writer reserved (`None` when the source had no turns to claim).
    Proceed {
        catalog_reservation: Box<LiveCaptureReservation>,
        live_reservation: Option<CaptureClaimReservation>,
    },
}

#[allow(clippy::too_many_arguments)]
async fn reserve_reasoning_coverage(
    conn: &sea_orm::DatabaseConnection,
    scope: &CaptureScope,
    repo_path: &std::path::Path,
    session_id: &str,
    turns: &[crate::internal::ai::observed_agents::NormalizedTurn],
    reasoning: &ReasoningSourceDisposition,
    provider: &str,
    owner: &str,
    now_ms: i64,
    source_channel: &'static str,
    deadline: Option<CaptureCommitDeadline>,
) -> Result<(
    crate::internal::ai::coverage_gate::ReasoningReservationOutcome,
    Option<ReasoningCoveragePlan>,
)> {
    use crate::internal::ai::{
        capture::checkpoint::prepare_reasoning_coverage_plan_from_disposition, coverage_gate,
    };

    let storage_path = util::try_get_storage_path(Some(repo_path.to_path_buf()))
        .context("resolve storage for private reasoning coverage")?;
    let reasoning_deadline = deadline
        .map(CaptureCommitDeadline::monotonic)
        .unwrap_or_else(|| Instant::now() + Duration::from_secs(5));
    let reader = ReasoningIdentityReadContext::new(
        scope,
        session_id,
        &storage_path,
        repo_path,
        reasoning_deadline,
    );
    let candidate = prepare_reasoning_coverage_plan_from_disposition(
        reasoning,
        ReasoningIdentityPreparation {
            conn,
            scope,
            storage_path: &storage_path,
            authorized_root: repo_path,
            session_id,
            provider,
            turns,
            deadline: reasoning_deadline,
        },
    )
    .await?;
    let input = match candidate.as_ref() {
        Some(candidate) => ReasoningCoverageInput::native(candidate),
        None => ReasoningCoverageInput::ordinary(&reader),
    };
    let outcome = match deadline {
        Some(deadline) => {
            let execution = match source_channel {
                "live" => CaptureReservationExecution::live(deadline),
                "export" => CaptureReservationExecution::export(deadline),
                _ => bail!("unsupported reasoning coverage channel"),
            };
            // Readable Claude thinking selects coverage schema 2. The automatic
            // reservation stamps schema 1 and then rejects that typed turn, so
            // the checkpoint write never keeps the ordinary transcript.
            if turns.iter().any(|turn| {
                crate::internal::ai::observed_agents::select_coverage_schema(turn)
                    == crate::internal::ai::observed_agents::CoverageSchemaVersion::V2
            }) {
                coverage_gate::reserve_runtime_turn_claims_with_reasoning(
                    conn, scope, session_id, turns, owner, now_ms, execution, input, false,
                )
                .await
            } else {
                coverage_gate::reserve_turn_claims_with_reasoning(
                    conn, scope, session_id, turns, owner, now_ms, execution, input,
                )
                .await
            }
        }
        None => {
            coverage_gate::reserve_turn_claims_with_reasoning_no_deadline(
                conn,
                scope,
                session_id,
                turns,
                owner,
                now_ms,
                source_channel,
                input,
            )
            .await
        }
    }?;
    Ok((outcome, candidate))
}

/// DR-05c-0 live coverage gate (plan-20260713 ADR-DR-09/10) for a provider
/// whose live capability supplies a coverage-v1 normalizer: its logical
/// turns are reserved BEFORE any object is built.
/// - every turn already covered by equivalent-or-better content → the whole
///   write is a no-op (no duplicate checkpoint on repeated events);
/// - a reservation failure (DB gate unavailable) fails the write CLOSED — no
///   ungated append;
/// - reserved claims commit inside the writer's ref-CAS transaction.
///
/// The capture deadline is checked before the synchronous normalizer starts
/// and after each synchronous step; an elapsed deadline releases any claim
/// this attempt owns before the terminal receipt is settled as pending, and
/// never acknowledges a covered no-op late.
#[allow(clippy::too_many_arguments)]
async fn reserve_live_coverage(
    conn: &sea_orm::DatabaseConnection,
    scope: &CaptureScope,
    libra_session_id: &str,
    checkpoint_id: &str,
    catalog_reservation: LiveCaptureReservation,
    catalog_request: &CaptureCatalogApplyRequest,
    capture_deadline: Option<CaptureCommitDeadline>,
    terminal_settlement_deadline: Option<CaptureCommitDeadline>,
    capture_finalizer_deadline_millis: Option<i64>,
    preexisting_durable_terminal: bool,
    normalize: LiveCoverageNormalizer,
    transcript_redacted: &crate::internal::ai::observed_agents::RedactedBytes,
    reasoning: &ReasoningSourceDisposition,
    provider: &str,
    repo_path: &std::path::Path,
) -> Result<LiveCoverageStage> {
    use crate::internal::ai::coverage_gate;

    // Do not begin a synchronous parser after the snapshot boundary has
    // already consumed the managed hook window. The writer's outer check is
    // intentionally repeated at this local boundary so later edits to
    // the coverage branch cannot make expired capture normalize bytes.
    if capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation,
            catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await?;
        return Ok(LiveCoverageStage::Settled);
    }
    let mut turns = normalize(transcript_redacted);
    // Normalization is intentionally bounded by the snapshot cap but is
    // synchronous. Once it returns, observe the same absolute deadline
    // before doing typed-field redaction or opening a claim transaction.
    if capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation,
            catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await?;
        return Ok(LiveCoverageStage::Settled);
    }
    // The snapshot service has already scrubbed generic secrets. Preserve
    // coverage-v1's typed-field redaction before canonicalize/digest so
    // claims never hash (or store digests of) unredacted content.
    crate::internal::ai::observed_agents::coverage::redact_turns(&mut turns);
    // `redact_turns` is also synchronous. An elapsed terminal deadline
    // must persist only its content-free pending finalizer, never enter a
    // coverage no-op path that can acknowledge the native delivery.
    if capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation,
            catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await?;
        return Ok(LiveCoverageStage::Settled);
    }
    if turns.is_empty() {
        if !reasoning.is_ordinary() {
            bail!("reasoning source has no logical turns for identity coverage");
        }
        return Ok(LiveCoverageStage::Proceed {
            catalog_reservation: Box::new(catalog_reservation),
            live_reservation: None,
        });
    }
    let owner = format!("live:{}:{}", std::process::id(), uuid::Uuid::new_v4());
    let now_ms = Utc::now().timestamp_millis();
    let reservation = reserve_reasoning_coverage(
        conn,
        scope,
        repo_path,
        libra_session_id,
        &turns,
        reasoning,
        provider,
        &owner,
        now_ms,
        "live",
        capture_deadline,
    )
    .await;
    let (reasoning_outcome, identity_plan) = match reservation {
        Ok(outcome) => outcome,
        Err(error) => {
            if let Some(deadline) = capture_deadline
                && Instant::now() >= deadline.monotonic()
            {
                // A deadline check inside the single live transaction can
                // win after the outer preflight. The transaction rolls
                // back its own in-flight work; defensively clear this
                // attempt's opaque owner in case a driver reports an
                // error after durable ownership changed. Never turn that
                // late failure into a terminal completion.
                coverage_gate::abandon_reserved_turn_claims_with_capture_scope_until(
                    conn,
                    scope,
                    libra_session_id,
                    &owner,
                    "live",
                    Utc::now().timestamp_millis(),
                    deadline,
                )
                .await
                .context(
                    "release elapsed-deadline live coverage reservation after a failed claim transaction",
                )?;
                settle_preapplied_without_checkpoint(
                    conn,
                    catalog_reservation,
                    catalog_request,
                    capture_finalizer_deadline_millis,
                    terminal_settlement_deadline,
                )
                .await?;
                return Ok(LiveCoverageStage::Settled);
            }
            return Err(error).context(
                "coverage gate reservation failed; checkpoint write aborted (fail-closed)",
            );
        }
    };
    if reasoning_outcome.skipped_incomplete_reasoning != 0 {
        bail!("reasoning coverage is partial; run libra agent doctor before retrying capture");
    }
    let versioned_claims = reasoning_outcome.versioned_reserved;
    let outcome = reasoning_outcome.ordinary;
    // A claim transaction can finish just before its final internal
    // check while the clock expires before this writer acts on the
    // outcome. Release the still-owned plan before settling the
    // terminal receipt as pending; specifically do not let an
    // all-covered terminal replay acknowledge after its deadline.
    #[cfg(test)]
    test_support::delay_after_live_coverage_reservation();
    if let Some(deadline) = capture_deadline
        && Instant::now() >= deadline.monotonic()
    {
        if !outcome.reserved.is_empty() {
            coverage_gate::abandon_reserved_turn_claims_with_capture_scope_until(
                conn,
                scope,
                libra_session_id,
                &owner,
                "live",
                Utc::now().timestamp_millis(),
                deadline,
            )
            .await
            .context(
                "release elapsed-deadline live coverage reservation before settling terminal receipt",
            )?;
        }
        settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation,
            catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await?;
        return Ok(LiveCoverageStage::Settled);
    }
    // Turns retained by a durable pending artifact are never released
    // by lease expiry, so they must not make every later writer of a
    // resumed session retry forever. Only a genuinely running writer
    // makes this delivery retryable; retained turns stay owned by
    // their artifact and are simply not re-claimed here.
    let running_inflight = outcome
        .skipped_inflight
        .saturating_sub(outcome.skipped_retained);
    if outcome.reserved.is_empty() && running_inflight > 0 {
        bail!(
            "coverage gate reservation is held by another live writer for {} turn(s); \
             checkpoint was not appended; retry after that writer finishes or its lease expires",
            running_inflight
        );
    }
    let fully_covered = !turns.is_empty()
        && outcome.reserved.is_empty()
        && outcome.skipped_covered == turns.len()
        && outcome.skipped_inflight == 0
        && outcome.conflicted == 0;
    let covered_terminal_replay =
        catalog_request.mutation().is_terminal() && preexisting_durable_terminal && fully_covered;
    if outcome.is_noop() && (!catalog_request.mutation().is_terminal() || covered_terminal_replay) {
        tracing::info!(
            session_id = %redact_session_id(libra_session_id),
            skipped_covered = outcome.skipped_covered,
            skipped_inflight = outcome.skipped_inflight,
            skipped_retained = outcome.skipped_retained,
            conflicted = outcome.conflicted,
            terminal_coverage_replay = covered_terminal_replay,
            "coverage gate: every turn already covered; skipping checkpoint append"
        );
        let covered_replay = settle_covered_checkpoint_replay(
            conn,
            scope,
            libra_session_id,
            checkpoint_id,
            capture_deadline,
        )
        .await?;
        if covered_replay == CoveredCheckpointReplay::DeadlineElapsed {
            // Same policy as an elapsed deadline right after the
            // reservation above: never acknowledge a terminal
            // replay late; nothing was reserved to release.
            settle_preapplied_without_checkpoint(
                conn,
                catalog_reservation,
                catalog_request,
                capture_finalizer_deadline_millis,
                terminal_settlement_deadline,
            )
            .await?;
            return Ok(LiveCoverageStage::Settled);
        }
        if covered_terminal_replay {
            settle_preapplied_covered_terminal_replay(catalog_reservation, catalog_request).await?;
        } else {
            settle_preapplied_without_checkpoint(
                conn,
                catalog_reservation,
                catalog_request,
                capture_finalizer_deadline_millis,
                terminal_settlement_deadline,
            )
            .await?;
        }
        return Ok(LiveCoverageStage::Settled);
    }
    if outcome.is_noop() {
        // A distinct terminal lifecycle receipt cannot borrow the
        // earlier turn's marker/source proof unless coverage proved
        // it is a replay over an already durable terminal state.
        // Keep the empty claim plan and let the coordinator write a
        // strict completion checkpoint for every other terminal
        // case, including changed, partial, or conflicted sources.
        tracing::info!(
            session_id = %redact_session_id(libra_session_id),
            skipped_covered = outcome.skipped_covered,
            "coverage gate: terminal action has no new turns; writing its strict completion checkpoint"
        );
    }
    Ok(LiveCoverageStage::Proceed {
        catalog_reservation: Box::new(catalog_reservation),
        live_reservation: Some(CaptureClaimReservation {
            owner,
            now_ms,
            claims: outcome.reserved,
            versioned_claims,
            reasoning: identity_plan.map(Box::new),
        }),
    })
}

/// Write a `committed` checkpoint (for a `TurnEnd` or `SessionEnd` event):
/// materialise the E4-libra tree (metadata.json, manifest.json,
/// events/lifecycle.jsonl, transcript/<agent_kind>.jsonl,
/// redaction_report.json, content_hash.txt), append a commit on
/// `refs/libra/traces`, and insert the corresponding `agent_checkpoint`
/// row. Errors are surfaced verbatim — a failure here means the ingest cannot
/// acknowledge the checkpoint to the caller.
///
/// `event` is the (already-redacted) triggering lifecycle event; it feeds
/// both the canonical `events/lifecycle.jsonl` line and metadata.json's
/// `model` field, and its redacted prompt is the transcript fallback when
/// the adapter advertises no readable transcript.
///
/// Write sequence + crash windows (AG-20, see the write-sequence matrix in
/// `docs/development/tracing/agent.md`): an in-flight marker is registered
/// fail-closed with the session/tombstone/coverage fence BEFORE stage (a) and
/// cleared AFTER stage (d); between ref CAS
/// (c) and catalog INSERT (d) the catalog is probed by `traces_commit` so a
/// retry — or a doctor repair that already backfilled the row — never
/// double-inserts.
///
/// A provider whose live capability supplies a transcript exporter runs the
/// export stage (`run_export_stage`) between the live coverage stage and
/// the checkpoint stage (`run_checkpoint_stage`). An admitted export-job
/// runner token reaches the checkpoint stage only through
/// `ExportStage::Proceed` (ADR-ACF-10, ACF-20).
#[allow(clippy::too_many_arguments)]
pub(super) async fn write_committed_checkpoint<P: LiveExportLeasePort>(
    conn: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    catalog_request: CaptureCatalogApplyRequest,
    catalog_reservation: LiveCaptureReservation,
    libra_session_id: &str,
    envelope: &SessionHookEnvelope,
    binding: LiveCaptureBinding,
    event: &LifecycleEvent,
    ingress_event_id: uuid::Uuid,
    identity_scheme: LifecycleIdentityScheme,
    checkpoint_id: &str,
    capture_deadline: Option<CaptureCommitDeadline>,
    terminal_settlement_deadline: Option<CaptureCommitDeadline>,
    capture_finalizer_deadline_millis: Option<i64>,
    redaction_report_json: &str,
    redaction_matches: &[crate::internal::ai::observed_agents::RedactionMatch],
    now: i64,
    scope: &CaptureScope,
    preexisting_durable_terminal: bool,
    subagent_sources: Vec<crate::internal::ai::subagent_content::DiscoveredSubagentContent>,
    subagent_discovery_warning: Option<&str>,
    subagent_capture: Option<crate::internal::ai::subagent_content::SubagentUsageCapture>,
    export_lease: &P,
) -> Result<()> {
    let agent_kind = binding.agent_kind_db();
    // The coordinator request is consumed when its opaque reservation writes
    // the checkpoint. Keep only a typed clone for best-effort diagnostics;
    // this clone never performs lifecycle apply/complete itself.
    let diagnostic_request = catalog_request.clone();

    // ADR-ACF-09a "Write trigger": a retained authenticated SessionEnd
    // artifact is the single owner of its stable receipt until the
    // checkpoint is published. A native redelivery must not re-read the
    // provider source, reserve or take over coverage, spend the receipt's
    // replay budget, or seal a second payload: each would change the bytes or
    // fences the artifact replays under. Completion belongs to the next
    // SessionStart recovery worker or an explicit `libra agent doctor
    // --repair`. A published checkpoint row bypasses this so redelivery can
    // still finish its receipt through the durable-replay path.
    if catalog_request.mutation().is_terminal()
        && crate::internal::ai::capture::pending::retains_unpublished_artifact_for_checkpoint(
            conn,
            &scope.repo_id,
            checkpoint_id,
        )
        .await?
    {
        return Err(retained_terminal_artifact_error());
    }

    let redacted_prompt = event.prompt.as_deref();

    // The snapshot service is the only source reader/redactor on this path.
    // It consumes the descriptor-pinned source internally and leaves this
    // runtime with only `RedactedBytes` plus safe source metadata.
    let mut report_value = serde_json::from_str::<serde_json::Value>(redaction_report_json)
        .unwrap_or_else(|_| serde_json::json!({}));
    let prompt_fallback =
        || RedactedBytes::new_unchecked(redacted_prompt.unwrap_or("").as_bytes().to_vec());
    let live_context = LiveCaptureContext {
        libra_session_id,
        provider_session_id: &envelope.session_id,
        verified_cwd: std::path::Path::new(&envelope.cwd),
    };
    let (live_projection, live_transcript, mut live_reasoning) = acquire_live_snapshot(
        conn,
        repo_path,
        scope,
        binding,
        &live_context,
        capture_deadline,
        &mut report_value,
    )
    .await;
    if !live_reasoning.has_capture_consumer()
        || (!live_reasoning.is_ordinary()
            && (live_projection.completeness != CaptureSnapshotCompleteness::Complete
                || live_transcript.is_none()))
    {
        // The existing catalog reservation remains uncommitted. This happens
        // before deadline/no-op settlement or coverage-turn reservation, so
        // the wrapper retains its original Pending/NotTracked behavior.
        return Err(SourcePreparationRejected.into());
    }
    if !live_reasoning.is_ordinary()
        && binding.live_coverage_normalizer().is_none()
        && binding.transcript_exporter().is_none()
    {
        return Err(SourcePreparationRejected.into());
    }
    let mut transcript_is_authorized = live_transcript.is_some();
    let mut transcript_snapshot_projection = Some(live_projection);
    let mut transcript_redacted = live_transcript.unwrap_or_else(prompt_fallback);

    // A terminal capture whose source boundary consumed the remaining window
    // must not enter synchronous coverage normalization. Settle the durable
    // content-free pending receipt instead of starting a parser that Tokio
    // cannot preempt once it is running.
    if capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
        return settle_preapplied_without_checkpoint(
            conn,
            catalog_reservation,
            &catalog_request,
            capture_finalizer_deadline_millis,
            terminal_settlement_deadline,
        )
        .await;
    }

    // DR-05c-0 live coverage gate: a provider whose live capability supplies
    // a coverage-v1 normalizer reserves its logical turns before any object
    // is built (see `reserve_live_coverage`). Providers without a normalizer
    // keep the legacy ungated path.
    let (catalog_reservation, mut live_reservation) =
        if transcript_is_authorized && let Some(normalize) = binding.live_coverage_normalizer() {
            match reserve_live_coverage(
                conn,
                scope,
                libra_session_id,
                checkpoint_id,
                catalog_reservation,
                &catalog_request,
                capture_deadline,
                terminal_settlement_deadline,
                capture_finalizer_deadline_millis,
                preexisting_durable_terminal,
                normalize,
                &transcript_redacted,
                &live_reasoning,
                agent_kind,
                repo_path,
            )
            .await?
            {
                LiveCoverageStage::Settled => return Ok(()),
                LiveCoverageStage::Proceed {
                    catalog_reservation,
                    live_reservation,
                } => (*catalog_reservation, live_reservation),
            }
        } else {
            (catalog_reservation, None)
        };

    // DR-04b (M3): a provider without an on-disk transcript supplies its
    // content through a trusted, sandboxed transcript exporter, converged
    // through the per-session export job (ADR-DR-11) and gated per turn like
    // every other path. Exporter unavailability degrades to the legacy
    // metadata-only capture with a warning — never an unsandboxed or ungated
    // content write.
    let mut claim_channel: &'static str = "live";
    let mut export_runner = None;
    let mut verified_partial = None;
    let catalog_reservation = match binding.transcript_exporter() {
        None => catalog_reservation,
        Some(exporter) => match run_export_stage(
            conn,
            repo_path,
            scope,
            binding,
            &live_context,
            checkpoint_id,
            catalog_reservation,
            &catalog_request,
            capture_deadline,
            terminal_settlement_deadline,
            capture_finalizer_deadline_millis,
            preexisting_durable_terminal,
            &mut report_value,
            export_lease,
            exporter,
        )
        .await?
        {
            ExportStage::Settled => return Ok(()),
            ExportStage::MetadataOnly {
                catalog_reservation,
            } => *catalog_reservation,
            ExportStage::Proceed {
                catalog_reservation,
                runner,
                projection,
                transcript,
                claims,
                reasoning,
            } => {
                transcript_snapshot_projection = Some(*projection);
                transcript_is_authorized = true;
                transcript_redacted = transcript;
                live_reasoning = *reasoning;
                claim_channel = "export";
                live_reservation = Some(*claims);
                export_runner = Some(runner);
                *catalog_reservation
            }
            ExportStage::VerifiedPartial {
                catalog_reservation,
                runner,
                source,
            } => {
                // Storage-only partial content has no coverage-v1 or native
                // identity reservation. The checkpoint stage still builds
                // the existing empty scope/tombstone/catalog CAS companion.
                claim_channel = "export";
                live_reservation = None;
                verified_partial = Some(*source);
                export_runner = Some(runner);
                *catalog_reservation
            }
        },
    };

    // Suppress the unused-warning for redaction_matches; reserved for a
    // Phase 3 enhancement that adds per-rule counters to metadata.
    let _ = redaction_matches;
    run_checkpoint_stage(
        CheckpointStageInput {
            conn,
            repo_path,
            catalog_request,
            diagnostic_request,
            catalog_reservation,
            libra_session_id,
            envelope,
            agent_kind,
            event,
            ingress_event_id,
            identity_scheme,
            checkpoint_id,
            capture_deadline,
            terminal_settlement_deadline,
            capture_finalizer_deadline_millis,
            now,
            scope,
            subagent_sources,
            subagent_discovery_warning,
            subagent_capture,
            incomplete_parent_extraction: subagent_discovery_warning.is_some()
                && binding
                    .subagent_discovery()
                    .is_some_and(|port| port.marks_incomplete_parent_extraction()),
            report_value,
            source: match verified_partial {
                Some(source) => CheckpointStageSource::VerifiedPartial(source),
                None => CheckpointStageSource::Captured {
                    transcript_is_authorized,
                    projection: transcript_snapshot_projection,
                    transcript: transcript_redacted,
                    reasoning: live_reasoning,
                },
            },
            claim_channel,
            live_reservation,
        },
        export_runner,
    )
    .await
}

/// Outcome of the export stage (ADR-ACF-10, ACF-20).
pub(super) enum ExportStage<'a, P: LiveExportLeasePort> {
    /// The stage consumed the catalog reservation (another runner covers
    /// this idle, an in-flight skip, or a covered no-op settled its receipt).
    /// The writer returns `Ok(())`.
    Settled,
    /// The exporter was unavailable and its runner already settled `Failed`:
    /// continue with a metadata-only checkpoint on the live claim channel,
    /// without a runner token.
    MetadataOnly {
        catalog_reservation: Box<LiveCaptureReservation>,
    },
    /// An authorized, redacted export whose turns were reserved on the
    /// export claim channel. The runner token moves to the checkpoint stage.
    Proceed {
        catalog_reservation: Box<LiveCaptureReservation>,
        runner: LiveExportRunner<'a, P>,
        projection: Box<CaptureSnapshotProjection>,
        transcript: RedactedBytes,
        claims: Box<CaptureClaimReservation>,
        reasoning: Box<ReasoningSourceDisposition>,
    },
    /// Strictly scrubbed OpenCode ordinary content with an owning,
    /// scope-bound partial permit. It bypasses complete-turn reservation and
    /// can reach only the direct storage-only checkpoint consumer.
    VerifiedPartial {
        catalog_reservation: Box<LiveCaptureReservation>,
        runner: LiveExportRunner<'a, P>,
        source: Box<VerifiedOrdinaryPartialSnapshot>,
    },
}

/// The closed set of export-stage exits after runner admission (ADR-ACF-10
/// exit classes). `export_stage_lease_disposition` maps each variant to
/// its lease disposition; there is deliberately no `Proceed` variant.
pub(super) enum ExportStageExit {
    /// The exporter bridge is unavailable (source stage, `Failed`); the
    /// writer continues metadata-only.
    BridgeUnavailable,
    /// The exported source cannot yield a trusted redacted transcript
    /// (source stage, `Failed`). Claims reserved before the final redacted
    /// transcript check are left to expire, as before.
    SourceRejected(anyhow::Error),
    /// The export coverage reservation failed (coverage stage, `Dirty`).
    CoverageFailed(anyhow::Error),
    /// Another in-flight writer holds every turn (coverage stage, `Dirty`).
    InflightOnly,
    /// A covered no-op could not settle this action's own durable replay
    /// (coverage stage, `Dirty`).
    CoveredReplayFailed(anyhow::Error),
    /// Every turn is already covered and the replay settled (coverage stage,
    /// `AdvanceAndRelease`).
    CoveredNoop { covered_terminal_replay: bool },
}

/// An authorized export ready for the checkpoint stage.
struct ExportedSource {
    projection: CaptureSnapshotProjection,
    transcript: RedactedBytes,
    reservation: CaptureClaimReservation,
    reasoning: ReasoningSourceDisposition,
}

enum PreparedExportSource {
    /// Only the original action is acknowledged, never a replacement payload.
    DurableActionReplay {
        keep_export_dirty: bool,
    },
    Captured(ExportedSource),
    VerifiedPartial(VerifiedOrdinaryPartialSnapshot),
}

// Narrow success and settlement before matching the runner owner. The two
// content arms can only hand it off; all other exits share one settlement site.
enum ReadyExportSource {
    Captured(ExportedSource),
    VerifiedPartial(VerifiedOrdinaryPartialSnapshot),
}

enum ExportStageSettlement {
    DurableActionReplay { keep_export_dirty: bool },
    StageExit(ExportStageExit),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExportSnapshotRoute {
    Captured,
    VerifiedPartial,
    Rejected,
}

/// Select the owning source consumer without changing default readiness.
/// This pure boundary is tested without a provider exporter or Session.
fn export_snapshot_route(snapshot: &CaptureSnapshot) -> ExportSnapshotRoute {
    if snapshot.has_verified_ordinary_partial_permit() {
        ExportSnapshotRoute::VerifiedPartial
    } else if snapshot.rejects_capture_source_consumer() {
        ExportSnapshotRoute::Rejected
    } else {
        ExportSnapshotRoute::Captured
    }
}

/// The lease disposition, structured `reason=` and tracing fields of each
/// export-stage exit. Pure and exhaustive.
pub(super) fn export_stage_lease_disposition<'a>(
    exit: &ExportStageExit,
    libra_session_id: &'a str,
) -> (LeaseDisposition, &'static str, LeaseLogContext<'a>) {
    let (disposition, reason, message) = match exit {
        ExportStageExit::BridgeUnavailable | ExportStageExit::SourceRejected(_) => (
            LeaseDisposition::Failed,
            "release_failed_export_runner_failed",
            "failed to release unsuccessful export job",
        ),
        ExportStageExit::CoverageFailed(_) => (
            LeaseDisposition::Dirty,
            "release_export_job_after_coverage_error_failed",
            "failed to release export job after coverage error",
        ),
        ExportStageExit::InflightOnly => (
            LeaseDisposition::Dirty,
            "release_export_job_after_inflight_skip_failed",
            "failed to release export job after in-flight skip",
        ),
        ExportStageExit::CoveredReplayFailed(_) => (
            LeaseDisposition::Dirty,
            "release_export_job_after_covered_replay_failed",
            "failed to release export job after covered replay settlement",
        ),
        ExportStageExit::CoveredNoop { .. } => (
            LeaseDisposition::AdvanceAndRelease,
            "settle_noop_export_job_recovery_failed",
            "failed to retire timed-out no-op export job",
        ),
    };
    (
        disposition,
        reason,
        LeaseLogContext::Session {
            session_id: libra_session_id,
            message,
        },
    )
}

/// Shared authorized-snapshot route. The exporter owns process admission;
/// this stage consumes only an already authorized/redacted snapshot, so its
/// persistence and receipt replay can also be exercised without a provider.
#[allow(clippy::too_many_arguments)]
async fn prepare_export_snapshot(
    conn: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    scope: &CaptureScope,
    agent_kind: &str,
    libra_session_id: &str,
    checkpoint_id: &str,
    catalog_request: &CaptureCatalogApplyRequest,
    export_deadline: CaptureCommitDeadline,
    preexisting_durable_terminal: bool,
    report_value: &mut serde_json::Value,
    normalizer: crate::internal::ai::observed_agents::live_capture::LiveCoverageNormalizer,
    mut snapshot: CaptureSnapshot,
    owner: String,
    now_ms: i64,
) -> std::result::Result<PreparedExportSource, ExportStageExit> {
    use crate::internal::ai::observed_agents::coverage;
    let source_route = export_snapshot_route(&snapshot);
    let mut runtime = snapshot.take_coverage_runtime();
    if source_route == ExportSnapshotRoute::VerifiedPartial {
        // This consumer owns the source's strict pre-scrub permit. It
        // independently binds the actual redacted bytes to the current
        // scope/HMAC and deadline; a generic Unproven flag cannot enter.
        let storage = util::try_get_storage_path(Some(repo_path.to_path_buf())).map_err(|_| {
            ExportStageExit::CoverageFailed(anyhow!(
                "verified partial export storage scope is unavailable"
            ))
        })?;
        let source = snapshot
            .try_into_verified_ordinary_partial_in_scope_until(
                conn,
                scope,
                &storage,
                repo_path,
                export_deadline.monotonic(),
            )
            .await
            .map_err(|_| {
                ExportStageExit::CoverageFailed(anyhow!(
                    "verified partial export did not retain its source binding"
                ))
            })?;
        // The owning partial carrier has now revalidated source/scope/HMAC.
        if let Some(exit) = legacy_export_replay_exit(
            conn,
            scope,
            libra_session_id,
            checkpoint_id,
            export_deadline,
        )
        .await?
        {
            return Ok(exit);
        }
        crate::internal::ai::coverage_gate::ensure_runtime_source_admitted_until(
            conn,
            scope,
            libra_session_id,
            export_deadline,
            "export",
            runtime.classifier_rejected,
        )
        .await
        .map_err(ExportStageExit::CoverageFailed)?;
        merge_redaction_report_into(report_value, source.redaction_report());
        // Do not run the coverage-v1 normalizer: its all-incomplete skip
        // is not proof that this nonzero partial source was persisted.
        return Ok(PreparedExportSource::VerifiedPartial(source));
    }
    if source_route == ExportSnapshotRoute::Rejected {
        return Err(ExportStageExit::SourceRejected(
            SourcePreparationRejected.into(),
        ));
    }
    // Keep an incomplete source on its existing typed outcome. In
    // particular, do not replace a deadline result with a commitment
    // failure while no snapshot can reach a durable projection.
    if snapshot.transcript().is_none() {
        return Err(ExportStageExit::SourceRejected(anyhow!(
            "LBR-AGENT-005: export snapshot is incomplete; write aborted (fail-closed)"
        )));
    }
    // The export bridge's helper checksum is a transient preimage only.
    // Bind it before the snapshot can reach coverage, checkpoint
    // metadata, or a terminal finalizer; an export without that scoped
    // proof must not downgrade into a durable raw-SHA projection.
    let commitment = match (
        snapshot.redacted_digest_preimage(),
        util::try_get_storage_path(Some(repo_path.to_path_buf())),
    ) {
        (Some(preimage), Ok(storage_path)) => derive_snapshot_content_commitment_in_scope_until(
            conn,
            scope,
            &storage_path,
            repo_path,
            &preimage,
            export_deadline.monotonic(),
        )
        .await
        .ok(),
        _ => None,
    };
    if !commitment.is_some_and(|commitment| snapshot.bind_source_commitment(commitment)) {
        return Err(ExportStageExit::SourceRejected(anyhow!(
            "LBR-AGENT-005: export source commitment unavailable; \
             write aborted (fail-closed)"
        )));
    }
    let Some(snapshot_bytes) = snapshot.transcript() else {
        return Err(ExportStageExit::SourceRejected(anyhow!(
            "LBR-AGENT-005: export snapshot is incomplete; write aborted (fail-closed)"
        )));
    };
    merge_redaction_report_into(report_value, snapshot.redaction_report());
    let mut turns = runtime
        .turns
        .take()
        .unwrap_or_else(|| (normalizer)(snapshot_bytes));
    coverage::redact_turns(&mut turns);
    let (projection, transcript, reasoning) = snapshot.into_checkpoint_parts();
    let Some(transcript) = transcript else {
        return Err(ExportStageExit::SourceRejected(anyhow!(
            "LBR-AGENT-005: complete export snapshot lost its redacted transcript"
        )));
    };
    if turns.is_empty() && !reasoning.is_ordinary() {
        return Err(ExportStageExit::CoverageFailed(anyhow!(
            "reasoning source has no logical turns for identity coverage"
        )));
    }
    let storage_path =
        util::try_get_storage_path(Some(repo_path.to_path_buf())).map_err(|error| {
            ExportStageExit::CoverageFailed(
                anyhow!(error).context("resolve storage for runtime reasoning coverage"),
            )
        })?;
    let reader = ReasoningIdentityReadContext::new(
        scope,
        libra_session_id,
        &storage_path,
        repo_path,
        export_deadline.monotonic(),
    );
    let identity_plan =
        crate::internal::ai::capture::checkpoint::prepare_reasoning_coverage_plan_from_disposition(
            &reasoning,
            ReasoningIdentityPreparation {
                conn,
                scope,
                storage_path: &storage_path,
                authorized_root: repo_path,
                session_id: libra_session_id,
                provider: agent_kind,
                turns: &turns,
                deadline: export_deadline.monotonic(),
            },
        )
        .await
        .map_err(ExportStageExit::CoverageFailed)?;
    if let Some(exit) = legacy_export_replay_exit(
        conn,
        scope,
        libra_session_id,
        checkpoint_id,
        export_deadline,
    )
    .await?
    {
        return Ok(exit);
    }
    let input = match identity_plan.as_ref() {
        Some(plan) => ReasoningCoverageInput::native(plan),
        None => ReasoningCoverageInput::ordinary(&reader),
    };
    let reasoning_outcome =
        crate::internal::ai::coverage_gate::reserve_runtime_turn_claims_with_reasoning(
            conn,
            scope,
            libra_session_id,
            &turns,
            &owner,
            now_ms,
            CaptureReservationExecution::export(export_deadline),
            input,
            runtime.classifier_rejected,
        )
        .await
        .map_err(ExportStageExit::CoverageFailed)?;
    if reasoning_outcome.skipped_incomplete_reasoning != 0 {
        return Err(ExportStageExit::CoverageFailed(anyhow!(
            "reasoning coverage is partial; run libra agent doctor before retrying capture"
        )));
    }
    let versioned_claims = reasoning_outcome.versioned_reserved;
    let covered = reasoning_outcome.covered;
    let outcome = reasoning_outcome.ordinary;
    #[cfg(test)]
    test_support::delay_after_live_coverage_reservation();
    // Codex M3 R2 P1-2: a purely in-flight skip (another writer holds
    // the claim lease for every turn, nothing reserved here) must NOT
    // mark this export generation clean. If that writer then crashes, its
    // claim lease expires and only a DIRTY job lets a later idle retry —
    // advancing to clean would silently drop the transcript. Mirror the
    // live path: release dirty WITHOUT advancing processed, so
    // acquisition re-fires.
    if outcome.is_inflight_only_skip() {
        tracing::warn!(
            session_id = %redact_session_id(libra_session_id),
            skipped_inflight = outcome.skipped_inflight,
            "transcript export: coverage held by another in-flight writer; \
             releasing dirty for retry"
        );
        return Err(ExportStageExit::InflightOnly);
    }
    if runtime.source_partial && outcome.is_noop() {
        return Err(ExportStageExit::CoverageFailed(anyhow!(
            "partial capture source cannot acknowledge a covered replay; run libra agent doctor"
        )));
    }
    let fully_covered = !runtime.source_partial
        && !turns.is_empty()
        && outcome.reserved.is_empty()
        && outcome.skipped_covered == turns.len()
        && outcome.skipped_inflight == 0
        && outcome.conflicted == 0;
    if fully_covered
        && covered.iter().any(|claim| {
            claim.checkpoint_id() == checkpoint_id
                && claim.version()
                    == crate::internal::ai::observed_agents::CoverageSchemaVersion::V2
        })
    {
        match settle_runtime_covered_checkpoint_replay(
            conn,
            scope,
            libra_session_id,
            checkpoint_id,
            export_deadline,
            &covered,
        )
        .await
        {
            Ok(CoveredCheckpointReplay::Settled) => {
                return Ok(PreparedExportSource::DurableActionReplay {
                    keep_export_dirty: false,
                });
            }
            Ok(CoveredCheckpointReplay::DeadlineElapsed) => {
                return Err(ExportStageExit::CoveredReplayFailed(anyhow!(
                    "versioned capture replay exceeded its deadline"
                )));
            }
            Err(error) => return Err(ExportStageExit::CoveredReplayFailed(error)),
        }
    }
    let covered_terminal_replay =
        catalog_request.mutation().is_terminal() && preexisting_durable_terminal && fully_covered;
    if outcome.is_noop() && (!catalog_request.mutation().is_terminal() || covered_terminal_replay) {
        tracing::info!(
            session_id = %redact_session_id(libra_session_id),
            skipped_covered = outcome.skipped_covered,
            skipped_inflight = outcome.skipped_inflight,
            conflicted = outcome.conflicted,
            terminal_coverage_replay = covered_terminal_replay,
            "transcript export: every turn already covered; no append"
        );
        // Settle this action's own durable replay before the export
        // generation can advance clean. Any unresolved outcome keeps the
        // receipt pending and releases the runner dirty so a later idle
        // retries.
        return Err(
            match settle_runtime_covered_checkpoint_replay(
                conn,
                scope,
                libra_session_id,
                checkpoint_id,
                export_deadline,
                &covered,
            )
            .await
            {
                Ok(CoveredCheckpointReplay::Settled) => ExportStageExit::CoveredNoop {
                    covered_terminal_replay,
                },
                Ok(CoveredCheckpointReplay::DeadlineElapsed) => {
                    ExportStageExit::CoveredReplayFailed(anyhow!(
                        "covered export replay exceeded its deadline"
                    ))
                }
                Err(error) => ExportStageExit::CoveredReplayFailed(error),
            },
        );
    }
    if outcome.is_noop() {
        // See the live-source branch: a terminal receipt needs a durable
        // proof for its own action key unless all covered turns replay
        // an already durable terminal state.
        tracing::info!(
            session_id = %redact_session_id(libra_session_id),
            skipped_covered = outcome.skipped_covered,
            "transcript export: terminal action has no new turns; writing strict completion checkpoint"
        );
    }
    Ok(PreparedExportSource::Captured(ExportedSource {
        projection,
        transcript,
        reservation: CaptureClaimReservation {
            owner,
            now_ms,
            claims: outcome.reserved,
            versioned_claims,
            reasoning: identity_plan.map(Box::new),
        },
        reasoning,
    }))
}

/// Export stage (ADR-ACF-10, ACF-20): admit an export-job runner, export the
/// transcript through the provider's exporter, snapshot and bind it, and
/// reserve its turns on the export claim channel. Its token is settled here
/// exactly once on every exit, or moved to the checkpoint stage through
/// `ExportStage::Proceed`. The order is fixed: the inner future (source and
/// coverage work, including a covered no-op's durable replay settlement) →
/// `settle` → the continuation (catalog settlement or the stage's error).
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_export_stage<'a, P: LiveExportLeasePort>(
    conn: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    scope: &'a CaptureScope,
    binding: LiveCaptureBinding,
    context: &LiveCaptureContext<'a>,
    checkpoint_id: &str,
    catalog_reservation: LiveCaptureReservation,
    catalog_request: &CaptureCatalogApplyRequest,
    capture_deadline: Option<CaptureCommitDeadline>,
    terminal_settlement_deadline: Option<CaptureCommitDeadline>,
    capture_finalizer_deadline_millis: Option<i64>,
    preexisting_durable_terminal: bool,
    report_value: &mut serde_json::Value,
    export_lease: &'a P,
    exporter: &dyn LiveTranscriptExporter,
) -> Result<ExportStage<'a, P>> {
    let libra_session_id = context.libra_session_id;
    // `effective_capture_deadline` establishes an exporter's default budget
    // before catalog/snapshot work. Reuse that exact pair here; re-anchoring
    // it at export entry would let earlier durable runtime work borrow a
    // second execution window.
    let export_deadline = capture_deadline
        .context("export capture deadline was not established at runtime ingress")?;
    // Hermes acquires the export-job lease inside `read_hermes_live_source`.
    // That has to finish, including the release, before this stage admits
    // its own runner for the same session.
    let managed_source = if exporter.manages_export_lease() {
        let exported = match tokio::time::timeout_at(
            tokio::time::Instant::from_std(export_deadline.monotonic()),
            exporter.export(context, export_deadline.monotonic()),
        )
        .await
        {
            Ok(Ok(source)) => source,
            Ok(Err(_)) | Err(_) => {
                return Ok(ExportStage::MetadataOnly {
                    catalog_reservation: Box::new(catalog_reservation),
                });
            }
        };
        Some(exported)
    } else {
        None
    };
    let target = LiveExportTarget {
        agent_kind: binding.agent_kind_db(),
        provider_session_id: context.provider_session_id,
        scope,
    };
    let runner = match LiveExportRunner::admit(export_lease, target, export_deadline).await {
        // ADR-DR-10 fail-closed: a job/DB/schema failure is a GATE failure —
        // never proceed to an ungated append. (Only trusted-binary/sandbox/
        // export failures degrade to the metadata-only capture.)
        Err(error) => {
            return Err(error
                .context("export job gate unavailable; checkpoint write aborted (fail-closed)"));
        }
        Ok(None) => {
            tracing::info!(
                session_id = %redact_session_id(libra_session_id),
                "export idle recorded; an in-flight export runner will cover it"
            );
            settle_preapplied_without_checkpoint(
                conn,
                catalog_reservation,
                catalog_request,
                capture_finalizer_deadline_millis,
                terminal_settlement_deadline,
            )
            .await?;
            return Ok(ExportStage::Settled);
        }
        Ok(Some(runner)) => runner,
    };
    let owner = runner.owner().to_owned();
    let now_ms = runner.now_ms();
    let source = async {
        // Only source acquisition is cancellable here. Keep the original
        // paired deadline; runner settlement and dispatched SQL/ref COMMIT
        // acknowledgements below must still be awaited without cancellation.
        let exported = if let Some(source) = managed_source {
            source
        } else {
            match tokio::time::timeout_at(
                tokio::time::Instant::from_std(export_deadline.monotonic()),
                exporter.export(context, export_deadline.monotonic()),
            )
            .await
            {
                Ok(Ok(source)) => source,
                Ok(Err(_)) => return Err(ExportStageExit::BridgeUnavailable),
                Err(_) => {
                    tracing::info!(
                        session_id = %redact_session_id(libra_session_id),
                        reason = "export_deadline_elapsed",
                        "export source cancelled at capture deadline"
                    );
                    return Err(ExportStageExit::BridgeUnavailable);
                }
            }
        };
        let source = match exported {
            TranscriptSource::File { .. } => {
                // INVARIANT: a transcript exporter only constructs Bytes.
                return Err(ExportStageExit::SourceRejected(anyhow!(
                    "LBR-AGENT-005: export bridge returned a File source (internal invariant)"
                )));
            }
            source @ TranscriptSource::Bytes { .. } => source,
        };
        // The snapshot service re-verifies the exporter proof, enforces the
        // hard cap/deadline, digests the exact authorized source, then
        // returns redacted-only bytes. A partial export is a failed closed
        // checkpoint write, never an unredacted normalization fallback.
        let snapshot = CaptureSnapshotService::capture_authorized_with_reasoning_context(
            source,
            binding.observed(),
            crate::internal::ai::capture::snapshot::AuthorizedReasoningSourceContext::authorized_export(
                binding.agent_kind_db(), libra_session_id, context.provider_session_id,
            ),
            CaptureSnapshotPolicy::with_deadline(Some(export_deadline.monotonic())),
        );
        prepare_export_snapshot(
            conn, repo_path, scope, binding.agent_kind_db(), libra_session_id,
            checkpoint_id, catalog_request, export_deadline,
            preexisting_durable_terminal, report_value,
            exporter.export_coverage_normalizer(), snapshot, owner, now_ms,
        ).await
    }
    .await;
    let source = match source {
        Ok(PreparedExportSource::Captured(source)) => Ok(ReadyExportSource::Captured(source)),
        Ok(PreparedExportSource::VerifiedPartial(source)) => {
            Ok(ReadyExportSource::VerifiedPartial(source))
        }
        Ok(PreparedExportSource::DurableActionReplay { keep_export_dirty }) => {
            Err(ExportStageSettlement::DurableActionReplay { keep_export_dirty })
        }
        Err(exit) => Err(ExportStageSettlement::StageExit(exit)),
    };
    match source {
        Ok(ReadyExportSource::Captured(ExportedSource {
            projection,
            transcript,
            reservation,
            reasoning,
        })) => Ok(ExportStage::Proceed {
            catalog_reservation: Box::new(catalog_reservation),
            runner,
            projection: Box::new(projection),
            transcript,
            claims: Box::new(reservation),
            reasoning: Box::new(reasoning),
        }),
        Ok(ReadyExportSource::VerifiedPartial(source)) => Ok(ExportStage::VerifiedPartial {
            catalog_reservation: Box::new(catalog_reservation),
            runner,
            source: Box::new(source),
        }),
        Err(continuation) => {
            let (disposition, reason, log, settlement_context) = match &continuation {
                ExportStageSettlement::DurableActionReplay { keep_export_dirty } => (
                    if *keep_export_dirty {
                        LeaseDisposition::Dirty
                    } else {
                        LeaseDisposition::AdvanceAndRelease
                    },
                    "settle_durable_action_export_job_failed",
                    LeaseLogContext::Session {
                        session_id: libra_session_id,
                        message: "failed to settle original action export job",
                    },
                    "settle export runner for original durable action",
                ),
                ExportStageSettlement::StageExit(exit) => {
                    let (disposition, reason, log) =
                        export_stage_lease_disposition(exit, libra_session_id);
                    (
                        disposition,
                        reason,
                        log,
                        "settle no-op export job within its capture deadline",
                    )
                }
            };
            // Every non-writing exit consumes the token at this one site.
            // Failed advance already retires it dirty; keep the receipt pending.
            runner
                .settle(disposition, reason, log)
                .await
                .context(settlement_context)?;
            match continuation {
                ExportStageSettlement::DurableActionReplay { .. } => {
                    if !catalog_request.mutation().is_terminal() {
                        settle_preapplied_without_checkpoint(
                            conn,
                            catalog_reservation,
                            catalog_request,
                            capture_finalizer_deadline_millis,
                            terminal_settlement_deadline,
                        )
                        .await?;
                        return Ok(ExportStage::Settled);
                    }
                    match catalog_reservation
                        .complete_preapplied_durable_replay(catalog_request)
                        .await
                        .map_err(|error| {
                            anyhow!("complete original durable capture receipt: {error}")
                        })? {
                        CaptureCoordinatorOutcome::CheckpointAlreadyExists { .. }
                        | CaptureCoordinatorOutcome::AlreadyApplied => Ok(ExportStage::Settled),
                        outcome => Err(anyhow!(
                            "durable capture receipt remains pending: {outcome:?}; run libra agent doctor"
                        )),
                    }
                }
                ExportStageSettlement::StageExit(exit) => match exit {
                    ExportStageExit::BridgeUnavailable => Ok(ExportStage::MetadataOnly {
                        catalog_reservation: Box::new(catalog_reservation),
                    }),
                    ExportStageExit::SourceRejected(error)
                    | ExportStageExit::CoveredReplayFailed(error) => Err(error),
                    ExportStageExit::CoverageFailed(error) => Err(error.context(
                        "coverage gate reservation failed; export capture aborted (fail-closed)",
                    )),
                    ExportStageExit::InflightOnly => {
                        settle_preapplied_without_checkpoint(
                            conn,
                            catalog_reservation,
                            catalog_request,
                            capture_finalizer_deadline_millis,
                            terminal_settlement_deadline,
                        )
                        .await?;
                        Ok(ExportStage::Settled)
                    }
                    ExportStageExit::CoveredNoop {
                        covered_terminal_replay,
                    } => {
                        if covered_terminal_replay {
                            settle_preapplied_covered_terminal_replay(
                                catalog_reservation,
                                catalog_request,
                            )
                            .await?;
                        } else {
                            settle_preapplied_without_checkpoint(
                                conn,
                                catalog_reservation,
                                catalog_request,
                                capture_finalizer_deadline_millis,
                                terminal_settlement_deadline,
                            )
                            .await?;
                        }
                        Ok(ExportStage::Settled)
                    }
                },
            }
        }
    }
}

/// Source ownership at the checkpoint stage. The partial branch cannot be
/// rebuilt from a disposition, snapshot projection, or metadata flag.
enum CheckpointStageSource {
    Captured {
        transcript_is_authorized: bool,
        projection: Option<CaptureSnapshotProjection>,
        transcript: RedactedBytes,
        reasoning: ReasoningSourceDisposition,
    },
    VerifiedPartial(VerifiedOrdinaryPartialSnapshot),
}

impl CheckpointStageSource {
    fn is_verified_partial(&self) -> bool {
        matches!(self, Self::VerifiedPartial(_))
    }

    fn transcript_is_authorized(&self) -> bool {
        match self {
            Self::Captured {
                transcript_is_authorized,
                ..
            } => *transcript_is_authorized,
            Self::VerifiedPartial(_) => true,
        }
    }

    fn transcript(&self) -> &RedactedBytes {
        match self {
            Self::Captured { transcript, .. } => transcript,
            Self::VerifiedPartial(source) => source.transcript(),
        }
    }

    fn projection(&self) -> Option<CaptureSnapshotProjection> {
        match self {
            Self::Captured { projection, .. } => projection.clone(),
            Self::VerifiedPartial(source) => Some(source.projection()),
        }
    }
}

/// The checkpoint stage's inputs other than the export runner token.
pub(super) struct CheckpointStageInput<'a> {
    conn: &'a sea_orm::DatabaseConnection,
    repo_path: &'a std::path::Path,
    catalog_request: CaptureCatalogApplyRequest,
    diagnostic_request: CaptureCatalogApplyRequest,
    catalog_reservation: LiveCaptureReservation,
    libra_session_id: &'a str,
    envelope: &'a SessionHookEnvelope,
    agent_kind: &'static str,
    event: &'a LifecycleEvent,
    ingress_event_id: uuid::Uuid,
    identity_scheme: LifecycleIdentityScheme,
    checkpoint_id: &'a str,
    capture_deadline: Option<CaptureCommitDeadline>,
    terminal_settlement_deadline: Option<CaptureCommitDeadline>,
    capture_finalizer_deadline_millis: Option<i64>,
    now: i64,
    scope: &'a CaptureScope,
    subagent_sources: Vec<crate::internal::ai::subagent_content::DiscoveredSubagentContent>,
    subagent_discovery_warning: Option<&'a str>,
    subagent_capture: Option<crate::internal::ai::subagent_content::SubagentUsageCapture>,
    incomplete_parent_extraction: bool,
    report_value: serde_json::Value,
    source: CheckpointStageSource,
    claim_channel: &'static str,
    live_reservation: Option<CaptureClaimReservation>,
}

/// The closed set of checkpoint-stage exits (ADR-ACF-10 classes (a)–(d)).
/// Each variant is built only by its class helper: `uncommitted` /
/// `uncommitted_settle` (a), `post_checkpoint` (b), `authorized` (c)
/// and `expire` (d).
pub(super) enum CheckpointStageExit {
    /// (a) The capture deadline elapsed before checkpoint work. This
    /// invocation's claims are released; after the lease, the terminal
    /// receipt settles as content-free pending.
    UncommittedSettle {
        catalog_reservation: Box<LiveCaptureReservation>,
        catalog_request: Box<CaptureCatalogApplyRequest>,
    },
    /// (a) No durable checkpoint for this invocation; its claims were
    /// released by `claim_owner`.
    Uncommitted(Result<()>),
    /// (b) The checkpoint/ref transaction committed its companion claims,
    /// which are kept; only the lease is released.
    PostCheckpoint(Result<()>),
    /// (c) An authorized commit: advance the export generation.
    Authorized,
    /// (d) Leave the lease to expire; the error is returned unchanged.
    Expire(anyhow::Error),
}

/// The lease disposition, structured `reason=` and tracing fields of each
/// checkpoint-stage exit. Pure and exhaustive.
pub(super) fn checkpoint_stage_lease_disposition<'a>(
    exit: &CheckpointStageExit,
    libra_session_id: &'a str,
    checkpoint_id: &'a str,
) -> (LeaseDisposition, &'static str, LeaseLogContext<'a>) {
    match exit {
        CheckpointStageExit::UncommittedSettle { .. }
        | CheckpointStageExit::Uncommitted(_)
        | CheckpointStageExit::PostCheckpoint(_) => (
            LeaseDisposition::Dirty,
            "release_export_job_after_checkpoint_failed",
            LeaseLogContext::Checkpoint {
                checkpoint_id,
                message: "failed to release export job after checkpoint write",
            },
        ),
        CheckpointStageExit::Authorized => (
            LeaseDisposition::AdvanceAndRelease,
            "settle_completed_export_job_recovery_failed",
            LeaseLogContext::Session {
                session_id: libra_session_id,
                message: "failed to retire timed-out completed export job",
            },
        ),
        CheckpointStageExit::Expire(_) => (
            LeaseDisposition::LeaveToExpiry,
            "export_job_left_to_expiry",
            LeaseLogContext::Checkpoint {
                checkpoint_id,
                message: "export job lease left to expire",
            },
        ),
    }
}

/// Class (a): release this invocation's claims by `claim_owner` — after
/// recording a retryable checkpoint diagnostic for a direct store failure —
/// then build the exit. `result` is produced after the cleanup, matching
/// the order of every uncommitted exit.
async fn uncommitted(
    conn: &sea_orm::DatabaseConnection,
    cleanup: &CheckpointReservationCleanup<'_>,
    deadline: Option<CaptureCommitDeadline>,
    diagnostic: Option<&CaptureCatalogApplyRequest>,
    result: impl FnOnce() -> Result<()>,
) -> CheckpointStageExit {
    match diagnostic {
        Some(request) => {
            cleanup_failed_checkpoint_side_effects(conn, request, cleanup, deadline).await
        }
        None => release_reserved_checkpoint_side_effects(conn, cleanup, deadline).await,
    }
    CheckpointStageExit::Uncommitted(result())
}

/// Class (a) for an elapsed capture deadline: release this invocation's
/// claims, then hand the reservation to the pending-receipt continuation.
async fn uncommitted_settle(
    conn: &sea_orm::DatabaseConnection,
    cleanup: &CheckpointReservationCleanup<'_>,
    deadline: Option<CaptureCommitDeadline>,
    catalog_reservation: LiveCaptureReservation,
    catalog_request: CaptureCatalogApplyRequest,
) -> CheckpointStageExit {
    release_reserved_checkpoint_side_effects(conn, cleanup, deadline).await;
    CheckpointStageExit::UncommittedSettle {
        catalog_reservation: Box::new(catalog_reservation),
        catalog_request: Box::new(catalog_request),
    }
}

/// Class (b): the companion claims are durable and stay; only the lease is
/// released.
fn post_checkpoint(result: Result<()>) -> CheckpointStageExit {
    CheckpointStageExit::PostCheckpoint(result)
}

/// Class (c): an authorized commit. A held runner must still carry its
/// ingress capture deadline; otherwise the invariant failure is class (d).
fn authorized(
    runner_held: bool,
    capture_deadline: Option<CaptureCommitDeadline>,
) -> CheckpointStageExit {
    if runner_held && capture_deadline.is_none() {
        return expire(anyhow!(
            "export runner completed without its ingress capture deadline"
        ));
    }
    CheckpointStageExit::Authorized
}

/// Class (d): leave the lease to expire and return the error.
fn expire(error: anyhow::Error) -> CheckpointStageExit {
    CheckpointStageExit::Expire(error)
}

/// Checkpoint stage (ADR-ACF-10, ACF-20): build the checkpoint payload,
/// elect the terminal attempt, and hand the reservation to the coordinator.
/// Every exit is one `CheckpointStageExit` built by its class helper inside
/// the inner future (claim cleanup and diagnostics included); then an
/// admitted runner token is settled exactly once; then the continuation runs
/// (the pending-receipt settlement or the stage's own result). Catalog and
/// checkpoint completion inside `execute_preapplied` and a durable replay's
/// completion stay inside the inner future, before the lease advances.
pub(super) async fn run_checkpoint_stage<P: LiveExportLeasePort>(
    input: CheckpointStageInput<'_>,
    runner: Option<LiveExportRunner<'_, P>>,
) -> Result<()> {
    use crate::internal::ai::{coverage_gate, observed_agents::Redactor, traces};

    let CheckpointStageInput {
        conn,
        repo_path,
        catalog_request,
        diagnostic_request,
        catalog_reservation,
        libra_session_id,
        envelope,
        agent_kind,
        event,
        ingress_event_id,
        identity_scheme,
        checkpoint_id,
        capture_deadline,
        terminal_settlement_deadline,
        capture_finalizer_deadline_millis,
        now,
        scope,
        subagent_sources,
        subagent_discovery_warning,
        subagent_capture,
        incomplete_parent_extraction,
        mut report_value,
        source,
        claim_channel,
        live_reservation,
    } = input;
    let runner_held = runner.is_some();
    let verified_partial = source.is_verified_partial();
    let transcript_is_authorized = source.transcript_is_authorized();
    let transcript_snapshot_projection = source.projection();
    let exit = async {
        let transcript_redacted = source.transcript();
        // From here until the coordinator durably consumes the reservation,
        // every fallible preparation step and every non-committing outcome
        // must release the scope-fenced claims it may have acquired above.
        // These exits have not committed this invocation's checkpoint, so
        // they intentionally do not record a retryable checkpoint
        // diagnostic.
        let failure_claim_owner = live_reservation
            .as_ref()
            .map(|reservation| reservation.owner.clone());
        let mut cleanup = CheckpointReservationCleanup {
            session_id: libra_session_id,
            checkpoint_id,
            scope,
            claim_channel,
            claim_owner: failure_claim_owner.as_deref(),
        };
        if verified_partial && (claim_channel != "export" || live_reservation.is_some()) {
            return uncommitted(conn, &cleanup, capture_deadline, None, || {
                Err(anyhow!(
                    "verified partial source cannot carry coverage or native identity reservations"
                ))
            })
            .await;
        }

        // Coverage may have reserved work while the source helper was still
        // within budget. Do not launch the extraction worker after that
        // absolute deadline has elapsed; release those reservations and
        // retain the terminal receipt as pending instead.
        if capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
            return uncommitted_settle(
                conn,
                &cleanup,
                capture_deadline,
                catalog_reservation,
                catalog_request,
            )
            .await;
        }

        // Native reasoning is archiveable only when its authenticated
        // per-turn identity plan travels with the claim reservation into the
        // checkpoint transaction. Keep this invariant at the persistence
        // boundary as well as in the source adapters: a future metadata-only
        // caller must fail closed instead of silently dropping the identity.
        let has_identity_plan = live_reservation
            .as_ref()
            .is_some_and(|reservation| reservation.reasoning.is_some());
        if let CheckpointStageSource::Captured { reasoning, .. } = &source
            && let Err(error) =
                require_identity_plan_for_native_reasoning(reasoning, has_identity_plan)
        {
            return uncommitted(conn, &cleanup, capture_deadline, None, || Err(error)).await;
        }

        // Build metadata.json (external schema v2 — v1 fields plus `model`;
        // strictly additive so v1 readers keep working). `model` mirrors the
        // E4-entire tolerance: taken from the triggering lifecycle event when
        // present, else the literal "unknown".
        // Child source persistence above intentionally operates on the
        // securely discovered native bytes. Parent extraction must not:
        // consume each child into the snapshot service and expose only typed
        // redacted output to the extractor and parent metadata.
        let subagent_snapshots = if capture_deadline.is_some() {
            omit_subagent_sources_for_deadline(subagent_sources)
        } else {
            snapshot_subagent_sources_for_extraction(
                subagent_sources,
                libra_session_id,
                CaptureSnapshotPolicy::default(),
            )
        };
        for report in &subagent_snapshots.redaction_reports {
            merge_redaction_report_into(&mut report_value, report);
        }
        let subagent_extraction_warnings = subagent_extraction_warnings(
            subagent_discovery_warning,
            subagent_snapshots.partial_source_count(),
        );
        // AG-21 transcript intelligence consumes snapshot-owned redacted
        // bytes. A child that could not make that boundary is omitted from
        // attribution and reported as an explicit, safe partial rather than
        // falling back to its native content.
        let mut extraction_value = match capture_deadline {
            Some(deadline) => match CaptureSnapshotService::build_extraction_projection_until(
                agent_kind,
                transcript_is_authorized.then_some(transcript_redacted),
                subagent_snapshots.children_omitted_for_deadline()
                    || incomplete_parent_extraction,
                deadline.monotonic(),
            )
            .await
            {
                DeadlineExtractionResult::Complete(value) => value,
                DeadlineExtractionResult::DeadlineExceeded => {
                    CaptureSnapshotService::deadline_extraction_partial(true)
                }
                DeadlineExtractionResult::Failed => {
                    CaptureSnapshotService::deadline_extraction_partial(false)
                }
            },
            None => build_extraction_metadata(
                agent_kind,
                transcript_is_authorized.then_some(transcript_redacted),
                &subagent_snapshots.transcripts,
                &subagent_extraction_warnings,
            ),
        };
        attach_subagent_snapshot_metadata(&mut extraction_value, &subagent_snapshots);
        // Attribution is auxiliary. Keep its window inside both original
        // pairs, and run it before the parent's existing final edge check.
        let usage_attribution = if let Some(capture) = subagent_capture.as_ref() {
            let deadline = capture_deadline.map(|parent| capture.deadline
                .filter(|child| child.monotonic() < parent.monotonic()).unwrap_or(parent));
            let parent_snapshot_complete = !verified_partial && transcript_is_authorized
                && transcript_snapshot_projection.as_ref().is_some_and(|projection|
                    projection.completeness == CaptureSnapshotCompleteness::Complete
                    && projection.source.as_ref().is_some_and(|source|
                        source.kind == crate::internal::ai::capture::snapshot::CaptureSnapshotSourceKind::ProviderFile));
            capture.summary.project_usage_attribution(conn, repo_path,
                crate::internal::ai::subagent_content::SubagentUsageAttributionInput {
                    scope, capture_session: libra_session_id, current_thread: &envelope.session_id,
                    verified_cwd: std::path::Path::new(&envelope.cwd), agent_kind,
                    as_of: "live_capture", parent_extraction: &extraction_value,
                    parent_snapshot_complete, deadline,
                }).await
        } else {
            None
        };
        // The helper lifecycle owns its deadline, but a response can become
        // ready at the same instant the capture window expires. Do not begin
        // synchronous metadata/report construction after that edge; no raw
        // or redacted transcript fallback is permitted here.
        if capture_deadline.is_some_and(|deadline| Instant::now() >= deadline.monotonic()) {
            return uncommitted_settle(
                conn,
                &cleanup,
                capture_deadline,
                catalog_reservation,
                catalog_request,
            )
            .await;
        }
        // Clone only after every transcript/child redaction report has been
        // merged, so metadata.json and redaction_report.json carry the same
        // bounded aggregate diagnostics.
        let redaction_report_value = report_value.clone();
        let metadata = serde_json::json!({
            "schema_version": traces::CHECKPOINT_METADATA_SCHEMA_VERSION,
            "checkpoint_id": null, // filled in below once we have the UUID
            "session_id": libra_session_id,
            "agent_kind": agent_kind,
            "scope": "committed",
            "provider_session_id": envelope.session_id,
            "working_dir": envelope.cwd,
            "model": checkpoint_model_field(event.model.as_ref()),
            "redaction_report": report_value,
            "created_at": now,
            "extraction": extraction_value,
        });

        // The caller supplies a stable replay ID. It names both the catalog
        // row and tree path, so a pending receipt retry addresses the same
        // durable checkpoint rather than creating a second one.
        let checkpoint_id = checkpoint_id.to_string();
        let mut metadata = metadata;
        if let Some(attribution) = usage_attribution {
            metadata["usage_attribution"] = attribution;
        }
        if let Some(obj) = metadata.as_object_mut() {
            obj.insert(
                "checkpoint_id".to_string(),
                serde_json::Value::String(checkpoint_id.clone()),
            );
            if let Some(snapshot) = transcript_snapshot_projection.as_ref() {
                let snapshot_value = match serde_json::to_value(snapshot) {
                    Ok(value) => value,
                    Err(error) => {
                        return uncommitted(conn, &cleanup, capture_deadline, None, || {
                            Err(error).context("serialize safe transcript snapshot projection")
                        })
                        .await;
                    }
                };
                obj.insert("transcript_snapshot".to_string(), snapshot_value);
            }
        }
        let metadata_bytes = match serde_json::to_vec_pretty(&normalize_json_value(metadata)) {
            Ok(bytes) => bytes,
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(error).context("serialize checkpoint metadata")
                })
                .await;
            }
        };
        // AG-19 / G4 redaction-before-persist: every blob entering the traces
        // writer is a `RedactedBytes`, so no `&[u8]` can reach the checkpoint
        // sink. The pass is idempotent defense-in-depth — these buffers are
        // already built from redacted extraction outputs / rule-hit
        // statistics, and the redactor skips existing `<REDACTED:…>` spans.
        let (metadata_redacted, _) = Redactor::new_default().redact(&metadata_bytes);

        // Canonical E3-JSONL evidence line(s) for events/lifecycle.jsonl —
        // exactly the redacted triggering event today; the slice API keeps
        // multi-event batches source-compatible.
        let canonical_ctx = CanonicalEventContext {
            agent_kind,
            session_id: libra_session_id,
            provider_session_id: &envelope.session_id,
            identity_scheme,
            provenance: serde_json::json!({
                "channel": "hook",
                "hook_event_name": envelope.hook_event_name,
            }),
        };
        let canonical_event = lifecycle_event_canonical_json_with_identity(
            event,
            &canonical_ctx,
            ingress_event_id,
            false,
        );
        let mut lifecycle_events_jsonl = match serde_json::to_vec(&canonical_event) {
            Ok(bytes) => bytes,
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(error).context("serialize canonical lifecycle evidence")
                })
                .await;
            }
        };
        lifecycle_events_jsonl.push(b'\n');
        let (lifecycle_events_redacted, _) =
            Redactor::new_default().redact(&lifecycle_events_jsonl);
        let redaction_report_bytes =
            match serde_json::to_vec_pretty(&normalize_json_value(redaction_report_value)) {
            Ok(bytes) => bytes,
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(error).context("serialize checkpoint redaction_report.json")
                })
                .await;
            }
        };
        let (redaction_report_redacted, _) =
            Redactor::new_default().redact(&redaction_report_bytes);

        // AG-20 observability (`agent.md` §6): one span per checkpoint write.
        // Required fields: checkpoint_id, session_id, stage (progression),
        // cas_retries, object_count. The transcript body is deliberately
        // never recorded.
        let write_span = tracing::info_span!(
            "agent.checkpoint.write",
            checkpoint_id = %checkpoint_id,
            session_id = %redact_session_id(libra_session_id),
            stage = tracing::field::Empty,
            cas_retries = tracing::field::Empty,
            object_count = tracing::field::Empty,
        );
        write_span.record("stage", "marker");

        // Resolve the user-branch HEAD via the typed helper so we can
        // distinguish "no HEAD yet (unborn)" from real storage errors. The
        // empty-string conflation produced by the lossy wrapper was flagged
        // in the Codex Phase-2 round-1 review.
        //
        // Three semantic cases land in `parent_commit`:
        // - `Some(hash)` — repo has at least one commit and HEAD resolves.
        // - `None` from typed `Ok(None)` — HEAD is born but the branch is
        //   commit-less (e.g. immediately after `libra init`).
        // - `None` from `BranchStoreError::Corrupt { "HEAD reference is missing" }`
        //   — the schema is wired but the HEAD row was never seeded. This
        //   shows up in test fixtures that bootstrap the migrations without
        //   running `initialize_refs`. Functionally equivalent to "unborn" for
        //   the traces writer, so we coerce to `None` rather than
        //   failing the whole ingest.
        let parent_commit: Option<String> =
            match crate::internal::head::Head::current_commit_result_with_conn(conn).await {
                Ok(commit) => commit.map(|h| h.to_string()),
                Err(crate::internal::branch::BranchStoreError::Corrupt { detail, .. })
                    if detail.contains("HEAD reference is missing") =>
                {
                    None
                }
                Err(err) => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(anyhow!(
                            "failed to resolve HEAD while writing checkpoint: {err}"
                        ))
                    })
                    .await;
                }
            };

        // DR-05c-0: reserved claims + the catalog row commit INSIDE the
        // ref-CAS transaction (ADR-DR-10) — ref, catalog, revisions and claim
        // advances are atomic; a fence violation rolls all of them back.
        // Even a metadata-only/fallback checkpoint carries an empty claim
        // plan: the plan injects both the tombstone barrier and catalog
        // INSERT into the same transaction as the traces-ref CAS. No live
        // writer may use the old ref-first/catalog-later path after
        // ADR-DR-19.
        let (owner, claim_now_ms, claims, versioned_claims, identity_plan) = match live_reservation {
            Some(reservation) => (
                reservation.owner,
                reservation.now_ms,
                reservation.claims,
                reservation.versioned_claims,
                reservation.reasoning,
            ),
            None => (
                format!("live-barrier:{}", uuid::Uuid::new_v4()),
                Utc::now().timestamp_millis(),
                Vec::new(),
                Vec::new(),
                None,
            ),
        };
        let claim_plan = coverage_gate::LiveClaimCommitPlan {
            source_channel: claim_channel,
            session_id: libra_session_id.to_string(),
            checkpoint_id: checkpoint_id.clone(),
            owner,
            parent_commit: parent_commit.clone(),
            created_at: now,
            now_ms: claim_now_ms,
            claims,
            import_session: None,
            import_identity: None,
            capture_scope: Some(scope.clone()),
        };
        // Validate the ordinal sidecar before marker registration. A typed
        // capture may not reinterpret a missing or mismatched version as v1.
        if !versioned_claims.is_empty()
            && let Err(error) = coverage_gate::VersionedLiveClaimCommitPlan::new(
                claim_plan.clone(), &versioned_claims,
            )
        {
            return uncommitted(conn, &cleanup, capture_deadline, None, || Err(error)).await;
        }
        let runtime_versioned = versioned_claims.iter().any(|claim| {
            claim.coverage_schema_version == crate::internal::ai::observed_agents::CoverageSchemaVersion::V2
        });
        let registration_fences = claim_plan
            .claims
            .iter()
            .enumerate()
            .map(|(ordinal, claim)| traces::TracesCoverageFence {
                coverage_schema_version: versioned_claims.get(ordinal).map_or(
                    crate::internal::ai::observed_agents::CoverageSchemaVersion::V1,
                    |bound| bound.coverage_schema_version,
                ),
                logical_turn_key: &claim.logical_turn_key,
                coverage_digest: &claim.coverage_digest,
                completeness: claim.completeness,
                next_revision: claim.next_revision,
                owner: &claim_plan.owner,
                fence_token: claim.fence_token,
                reservation_state: "reserved_live",
            })
            .collect::<Vec<_>>();

        // The sealed payload is the only value handed to the checkpoint
        // store. Every byte was already redacted above; the optional safe
        // projection makes a metadata-only compatibility fallback explicitly
        // partial rather than falsely claiming an authorized source snapshot.
        let prepared_payload = match source {
            CheckpointStageSource::Captured {
                transcript,
                projection,
                reasoning,
                ..
            } => CheckpointRedactedPayload::from_redacted_capture_with_reasoning(
                transcript,
                projection,
                metadata_redacted,
                lifecycle_events_redacted,
                redaction_report_redacted,
                reasoning,
            ),
            CheckpointStageSource::VerifiedPartial(source) => {
                CheckpointRedactedPayload::from_verified_ordinary_partial(
                    source,
                    metadata_redacted,
                    lifecycle_events_redacted,
                    redaction_report_redacted,
                )
            }
        };
        let checkpoint_payload = match prepared_payload {
            Ok(payload) => payload,
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!(
                        "prepare redacted capture checkpoint payload: {error}"
                    ))
                })
                .await;
            }
        };
        // Elect/adopt the one durable terminal attempt after the sealed
        // payload has supplied its source digest but before marker
        // registration. This closes the stale-reservation race: two
        // deliveries that both observed an unbound receipt cannot each
        // manufacture a marker generation.
        let replay_key = catalog_request.action().action_key().to_string();
        let terminal_attempt = if catalog_request.mutation().is_terminal() {
            let policy = match CaptureFinalizePolicy::new(
                capture_finalizer_deadline_millis,
                CaptureFinalizeMode::Deferrable,
                replay_key.clone(),
            ) {
                Ok(policy) => policy,
                Err(error) => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(anyhow!(
                            "prepare terminal capture finalizer policy: {error}"
                        ))
                    })
                    .await;
                }
            };
            let finalizer = CaptureCoordinatorFinalizer::new(policy, finalizer_now_millis());
            let source_digest = checkpoint_payload
                .snapshot()
                .source
                .as_ref()
                .and_then(|source| source.digest_sha256.clone());
            let candidate = TracesCheckpointStore::new_terminal_attempt_generation();
            let claim = match catalog_reservation
                .claim_terminal_checkpoint_attempt(
                    &catalog_request,
                    finalizer.clone(),
                    candidate,
                    source_digest,
                )
                .await
            {
                Ok(claim) => claim,
                Err(error) => {
                    let waiting = child_checkpoint_wait_error(&error).is_some();
                    let error = anyhow::Error::new(error).context("claim terminal capture checkpoint attempt");
                    if waiting {
                        return expire(error);
                    }
                    return uncommitted(conn, &cleanup, capture_deadline, None, || Err(error)).await;
                }
            };
            match claim {
                CaptureCatalogTerminalAttempt::Bound {
                    marker_generation,
                    registration_fence,
                    ..
                } => Some((finalizer, marker_generation, registration_fence)),
                CaptureCatalogTerminalAttempt::DurableReplay => {
                    // The catalog saw the original deterministic checkpoint
                    // after its marker was retired. Do not let this newer
                    // snapshot reach marker registration: the reservation
                    // rechecks the durable row and completes only the
                    // persisted receipt in one writer transaction.
                    let outcome = match catalog_reservation
                        .complete_preapplied_durable_replay(&catalog_request)
                        .await
                    {
                        Ok(outcome) => outcome,
                        Err(error) => {
                            let waiting = child_checkpoint_wait_error(&error).is_some();
                            let error = anyhow::Error::new(error).context("complete durable terminal replay under catalog fence");
                            if waiting {
                                return expire(error);
                            }
                            return uncommitted(conn, &cleanup, capture_deadline, None, || Err(error)).await;
                        }
                    };
                    return match outcome {
                        CaptureCoordinatorOutcome::CheckpointAlreadyExists { .. }
                        | CaptureCoordinatorOutcome::AlreadyApplied => {
                            uncommitted(conn, &cleanup, capture_deadline, None, || Ok(())).await
                        }
                        CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
                            uncommitted(conn, &cleanup, capture_deadline, None, || {
                                Err(anyhow!(
                                    "durable terminal checkpoint evidence changed before receipt completion: {conflict:?}; retry the same native event"
                                ))
                            })
                            .await
                        }
                        CaptureCoordinatorOutcome::FinalizerQuarantined { reason } => {
                            let exit =
                                uncommitted(conn, &cleanup, capture_deadline, None, || Ok(()))
                                    .await;
                            tracing::warn!(
                                target: "agent.capture.finalize",
                                finalizer_outcome = "quarantined",
                                finalizer_reason = ?reason,
                                "durable terminal replay is repair-required"
                            );
                            exit
                        }
                        CaptureCoordinatorOutcome::FinalizerPending { .. }
                        | CaptureCoordinatorOutcome::CheckpointInFlight { .. }
                        | CaptureCoordinatorOutcome::CheckpointConflict { .. }
                        | CaptureCoordinatorOutcome::PendingCleanup { .. }
                        | CaptureCoordinatorOutcome::DurableFinalizer { .. }
                        | CaptureCoordinatorOutcome::CheckpointCommitted { .. }
                        | CaptureCoordinatorOutcome::StateApplied => {
                            uncommitted(conn, &cleanup, capture_deadline, None, || {
                                Err(anyhow!(
                                    "durable terminal replay did not complete the persisted receipt; retry the same native event"
                                ))
                            })
                            .await
                        }
                    };
                }
                CaptureCatalogTerminalAttempt::Adopted { .. } => {
                    // The same native event may be delivered after the
                    // elected writer has snapshotted an earlier transcript
                    // revision. A changed source is an observer of that
                    // durable attempt, not authority to rewrite or quarantine
                    // its finalizer fence. Return retryable failure so the
                    // host redelivers after the elected source reaches a
                    // durable checkpoint.
                    tracing::info!(
                        target: "agent.capture.finalize",
                        finalizer_outcome = "adopted_different_source",
                        "terminal capture attempt is owned by an earlier source snapshot"
                    );
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(anyhow!(
                            "capture terminal checkpoint attempt is owned by an earlier source snapshot; retry the same native event"
                        ))
                    })
                    .await;
                }
                CaptureCatalogTerminalAttempt::AlreadyComplete => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || Ok(())).await;
                }
                CaptureCatalogTerminalAttempt::Quarantined { reason } => {
                    tracing::warn!(
                        target: "agent.capture.finalize",
                        finalizer_outcome = "quarantined",
                        finalizer_reason = ?reason,
                        "terminal capture attempt is already repair-required"
                    );
                    return uncommitted(conn, &cleanup, capture_deadline, None, || Ok(())).await;
                }
                CaptureCatalogTerminalAttempt::ConflictUnchanged { conflict } => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(anyhow!(
                            "terminal capture attempt lost its catalog fence: {conflict:?}"
                        ))
                    })
                    .await;
                }
            }
        } else {
            None
        };
        // The entry probe can race a sibling delivery that retained the
        // artifact after it ran. Recheck once this delivery holds the elected
        // attempt so it never seals a second payload or publishes different
        // bytes over the retained one; its own fresh reservations are
        // released first.
        if terminal_attempt.is_some() {
            match crate::internal::ai::capture::pending::retains_unpublished_artifact_for_checkpoint(
                conn,
                &scope.repo_id,
                &checkpoint_id,
            )
            .await
            {
                Ok(false) => {}
                Ok(true) => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(retained_terminal_artifact_error())
                    })
                    .await;
                }
                Err(error) => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || Err(error))
                        .await;
                }
            }
        }
        // A complete, authenticated SessionEnd snapshot is made durable
        // before marker registration/ref CAS. If any later checkpoint stage
        // fails, the artifact owns its coverage reservation so doctor/worker
        // replay can finish the exact elected receipt instead of observing
        // released claims. Incomplete snapshots deliberately stay on the
        // existing synchronous path and never become replayable artifacts.
        // An admitted export runner makes this branch unreachable.
        if terminal_snapshot_artifact_eligible(
            catalog_request.mutation().is_terminal(),
            checkpoint_payload.is_exact_complete_snapshot(),
            claim_plan.source_channel,
            runner_held,
        ) && let (Some((_, _, registration_fence)), Some(deadline)) =
            (terminal_attempt.as_ref(), capture_deadline)
        {
            let storage = match util::try_get_storage_path(Some(repo_path.to_path_buf())) {
                Ok(storage) => storage,
                Err(error) => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(error).context("resolve verified storage for capture recovery artifact")
                    })
                    .await;
                }
            };
            if let Err(error) = CaptureCatalogStore::new(conn.clone())
                .persist_pending_artifact_with_reasoning(
                    registration_fence,
                    &checkpoint_payload,
                    crate::internal::ai::capture::catalog::PendingArtifactPlans {
                        coverage: &claim_plan,
                        reasoning: identity_plan.as_deref(),
                    },
                    &storage,
                    repo_path,
                    deadline,
                )
                .await
            {
                // Only an unacknowledged COMMIT can have published the
                // artifact together with its claim retention, so those
                // claims stay with the possibly durable artifact. Every
                // other failure (capacity, binding/alias conflict, seal,
                // deadline, database) rolled the artifact transaction back
                // before commit: keep the pre-artifact contract and release
                // this delivery's fresh reservations rather than leaving
                // them to block the next redelivery for a lease.
                let retain_failure = || {
                    anyhow!(
                        "could not durably retain the complete terminal snapshot; retry the same native event or run `libra agent doctor`"
                    )
                };
                if error != CaptureCatalogError::CommitFailed {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(retain_failure())
                    })
                    .await;
                }
                return expire(retain_failure());
            }
            cleanup.claim_owner = None;
        }
        let checkpoint_store = match terminal_attempt.as_ref() {
            Some((_, marker_generation, _)) => {
                TracesCheckpointStore::new_with_persisted_marker_generation(
                    conn,
                    repo_path,
                    libra_session_id,
                    &checkpoint_id,
                    marker_generation,
                    &registration_fences,
                )
            }
            None => TracesCheckpointStore::new(
                conn,
                repo_path,
                libra_session_id,
                &checkpoint_id,
                &registration_fences,
            ),
        };
        let checkpoint_store = match checkpoint_store {
            Ok(store) => store.with_capture_scope(scope.clone()),
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!("prepare capture checkpoint store: {error}"))
                })
                .await;
            }
        };
        let checkpoint_store = match terminal_attempt.as_ref() {
            Some((_, _, registration_fence)) => {
                checkpoint_store.with_terminal_attempt_fence(registration_fence.as_ref().clone())
            }
            None => checkpoint_store,
        };
        let marker_generation = checkpoint_store.marker_generation().to_string();
        // Replay identity belongs to the lifecycle receipt, while
        // checkpoint_id remains the stable traces/catalog identity. Keeping
        // these distinct prevents a checkpoint from being completed under an
        // unrelated event.
        let transaction_plan = match if runtime_versioned {
            coverage_gate::LiveCaptureClaimCommitPlan::new_runtime_versioned(
                claim_plan, &versioned_claims, identity_plan.as_deref(),
            )
        } else {
            coverage_gate::LiveCaptureClaimCommitPlan::new(claim_plan, identity_plan.as_deref())
        } {
            Ok(plan) => plan,
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(error.context("prepare live reasoning claim transaction companion"))
                })
                .await;
            }
        };
        let checkpoint_request = match CheckpointWriteRequest::new(
            &replay_key,
            &checkpoint_id,
            libra_session_id,
            agent_kind,
            parent_commit.as_deref(),
            crate::internal::ai::traces::CheckpointScope::Committed,
            &marker_generation,
            None,
            &checkpoint_payload,
            if runtime_versioned { None } else { Some(&transaction_plan) },
            capture_deadline,
        ) {
            Ok(request) => request,
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!("prepare capture checkpoint write request: {error}"))
                })
                .await;
            }
        };

        let checkpoint_request = if runtime_versioned {
            match checkpoint_request.with_runtime_versioned_claims(&transaction_plan) {
                Ok(request) => request,
                Err(error) => {
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(anyhow!("bind runtime versioned checkpoint: {error}"))
                    }).await;
                }
            }
        } else { checkpoint_request };

        // The coordinator owns stages (a)–(d) and the only receipt-completion
        // edge. A terminal policy is persisted before checkpoint work;
        // recovery retains its original absolute deadline rather than
        // re-anchoring it.
        write_span.record("stage", "append");
        let terminal_checkpoint = catalog_request.mutation().is_terminal();
        let coordinator_request = if let Some((finalizer, _, _)) = terminal_attempt {
            CaptureCoordinatorRequest::with_claimed_finalizer(
                catalog_request,
                Some(checkpoint_request),
                finalizer,
            )
        } else {
            CaptureCoordinatorRequest::new(catalog_request, Some(checkpoint_request))
        };
        let coordinator_request = match coordinator_request {
            Ok(request) => request,
            Err(error) => {
                return uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!("prepare capture coordinator request: {error}"))
                })
                .await;
            }
        };
        let coordinator: CaptureCoordinator<_, _> =
            catalog_reservation.into_coordinator(checkpoint_store);
        let coordinator_outcome = match coordinator.execute_preapplied(coordinator_request).await
        {
            Ok(outcome) => outcome,
            Err(error) => {
                // Only a direct checkpoint-store failure is known to have
                // left no durable checkpoint and records a retryable
                // diagnostic; a typed store conflict left this invocation
                // unchanged without one. Completion failures after the
                // ref/catalog transaction keep their durable claims, and an
                // unclassified error leaves every reservation to expire.
                let waiting = child_checkpoint_wait_error(&error).is_some();
                let class = error.reservation_class();
                let error = anyhow::Error::new(error).context("capture coordinator execution failed");
                if waiting {
                    return expire(error);
                }
                return match class {
                    CoordinatorReservationClass::Uncommitted { diagnostic } => {
                        uncommitted(
                            conn,
                            &cleanup,
                            capture_deadline,
                            diagnostic.then_some(&diagnostic_request),
                            || Err(error),
                        )
                        .await
                    }
                    CoordinatorReservationClass::PostCheckpoint => post_checkpoint(Err(error)),
                    CoordinatorReservationClass::Unclassified => expire(error),
                };
            }
        };
        match coordinator_outcome {
            CaptureCoordinatorOutcome::CheckpointCommitted {
                cas_retries,
                object_count,
                ..
            } => {
                // The coordinator returns this outcome only after the
                // checkpoint store has durably completed its ref CAS and the
                // receipt catalog completion has succeeded. Preserve those
                // two completed phases and their content-free write metrics
                // on the owning span.
                write_span.record("cas_retries", cas_retries);
                write_span.record("object_count", object_count);
                write_span.record("stage", "ref_cas_done");
                write_span.record("stage", "catalog");
                write_span.record("stage", "done");
                authorized(runner_held, capture_deadline)
            }
            CaptureCoordinatorOutcome::CheckpointAlreadyExists { .. }
            | CaptureCoordinatorOutcome::AlreadyApplied => {
                write_span.record("stage", "already_exists");
                // This delivery reserved fresh coverage/export work, but the
                // durable checkpoint belongs to an earlier attempt. Do not
                // let its reservation survive as a lease leak or advance an
                // export generation that this invocation did not commit.
                uncommitted(conn, &cleanup, capture_deadline, None, || Ok(())).await
            }
            CaptureCoordinatorOutcome::CheckpointInFlight { .. } => {
                // A duplicate native delivery shares the persisted terminal
                // marker with the elected writer. It must not be treated as a
                // failed attempt (or consume finalizer budget); replay
                // observes the durable result once that writer completes.
                write_span.record("stage", "inflight");
                uncommitted(conn, &cleanup, capture_deadline, None, || {
                    if terminal_checkpoint {
                        // A terminal acknowledgement is a durability promise.
                        // The elected writer may still crash before the
                        // ref/catalog CAS, so a sibling cannot return success
                        // merely because it saw a live marker. Leave the
                        // persisted finalizer untouched and make the host
                        // redeliver this native event.
                        Err(anyhow!(
                            "capture terminal checkpoint attempt is still in flight; retry the same native event"
                        ))
                    } else {
                        Ok(())
                    }
                })
                .await
            }
            CaptureCoordinatorOutcome::CheckpointConflict { reason } => {
                // A typed conflict is not a checkpoint-store failure.
                // Preserve its durable evidence, but release only this
                // invocation's uncommitted reservations without changing
                // retry diagnostics.
                uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!(
                        "capture coordinator left the checkpoint unchanged: {reason:?}"
                    ))
                })
                .await
            }
            CaptureCoordinatorOutcome::PendingCleanup { .. } => {
                // A durable marker remains for doctor/GC. Do not complete a
                // terminal catalog receipt while that cleanup is unresolved.
                // The ref/CAS and coverage companion already committed, so
                // only the export lease is released; abandoning claims would
                // erase durable checkpoint evidence.
                post_checkpoint(Err(anyhow!(
                    "capture checkpoint write needs durable cleanup before completion"
                )))
            }
            CaptureCoordinatorOutcome::DurableFinalizer {
                checkpoint_id: durable_checkpoint_id,
                cleanup_pending,
                disposition,
            } => match disposition {
                DurableFinalizerDisposition::ReceiptAlreadyApplied => {
                    // A sibling completed this exact terminal receipt after
                    // the checkpoint/ref transaction committed. Preserve its
                    // coverage evidence; a successful Writer path still
                    // advances the export generation.
                    write_span.record("stage", "done");
                    if cleanup_pending {
                        post_checkpoint(Ok(()))
                    } else {
                        authorized(runner_held, capture_deadline)
                    }
                }
                DurableFinalizerDisposition::Quarantined { reason } => {
                    // The catalog has durable repair evidence and this
                    // checkpoint's companion claims are already committed. Do
                    // not recast it as an uncommitted write failure.
                    write_span.record("stage", "finalizer_quarantined");
                    tracing::warn!(
                        target: "agent.capture.finalize",
                        checkpoint_id = %durable_checkpoint_id,
                        finalizer_outcome = "quarantined_after_durable_checkpoint",
                        finalizer_reason = ?reason,
                        "terminal capture was durably quarantined for repair"
                    );
                    if cleanup_pending {
                        post_checkpoint(Ok(()))
                    } else {
                        authorized(runner_held, capture_deadline)
                    }
                }
                DurableFinalizerDisposition::Conflict { conflict } => {
                    // The checkpoint/ref transaction is durable even though
                    // the receipt fence moved. Keep companion claims intact
                    // and force a replay after releasing only the transient
                    // export lease.
                    post_checkpoint(Err(anyhow!(
                        "capture checkpoint {durable_checkpoint_id} is durable but its terminal receipt lost a catalog fence: {conflict:?}; retry the same native event"
                    )))
                }
            },
            CaptureCoordinatorOutcome::FinalizerQuarantined { reason } => {
                // The catalog has durably recorded a content-free, repairable
                // quarantine and deliberately did not publish `stopped`. Hook
                // hosts may acknowledge this terminal delivery only after
                // that durable evidence exists; a later doctor/replay owns
                // recovery.
                write_span.record("stage", "finalizer_quarantined");
                tracing::warn!(
                    target: "agent.capture.finalize",
                    finalizer_outcome = "quarantined",
                    finalizer_reason = ?reason,
                    "terminal capture was durably quarantined for repair"
                );
                uncommitted(conn, &cleanup, capture_deadline, None, || Ok(())).await
            }
            CaptureCoordinatorOutcome::FinalizerPending { .. } => {
                // A terminal receipt remains deliberately replayable until
                // the checkpoint/finalizer reaches a durable terminal proof.
                uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!(
                        "capture terminal finalizer remains pending durable replay"
                    ))
                })
                .await
            }
            CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
                uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!(
                        "capture coordinator lost its catalog fence: {conflict:?}"
                    ))
                })
                .await
            }
            CaptureCoordinatorOutcome::StateApplied => {
                uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!(
                        "capture coordinator returned a state-only outcome for checkpoint work"
                    ))
                })
                .await
            }
        }
    }
    .await;
    // A storage-only partial may not inherit a catalog-only successful
    // outcome (AlreadyApplied, adopted, quarantined, or deferred). Confirm
    // the actual scoped checkpoint/ref evidence before any success can
    // advance the export generation. No partial payload is queued here.
    let exit = if verified_partial
        && matches!(
            &exit,
            CheckpointStageExit::Authorized
                | CheckpointStageExit::Uncommitted(Ok(()))
                | CheckpointStageExit::PostCheckpoint(Ok(()))
        ) {
        match TracesCheckpointStore::replay_status_for(
            conn,
            Some(scope),
            checkpoint_id,
            libra_session_id,
            crate::internal::ai::traces::CheckpointScope::Committed,
            capture_deadline,
        )
        .await
        {
            Ok(CheckpointReplayStatus::Complete) => exit,
            Ok(CheckpointReplayStatus::PendingCleanup { .. }) => post_checkpoint(Err(anyhow!(
                "verified partial checkpoint needs durable cleanup before completion"
            ))),
            Ok(CheckpointReplayStatus::Missing) => {
                let cleanup = CheckpointReservationCleanup {
                    session_id: libra_session_id,
                    checkpoint_id,
                    scope,
                    claim_channel,
                    claim_owner: None,
                };
                uncommitted(conn, &cleanup, capture_deadline, None, || {
                    Err(anyhow!(
                        "verified partial capture has no durable checkpoint; keep the receipt pending"
                    ))
                })
                .await
            }
            Err(_) => post_checkpoint(Err(anyhow!(
                "verified partial capture could not confirm its durable checkpoint"
            ))),
        }
    } else {
        exit
    };
    let (disposition, reason, log) =
        checkpoint_stage_lease_disposition(&exit, libra_session_id, checkpoint_id);
    let settled = if let Some(runner) = runner {
        runner.settle(disposition, reason, log).await
    } else {
        Ok(())
    };
    // DR-04b: an authorized runner advances its processed generation and
    // releases the lease HONESTLY — `dirty` when more idles arrived during
    // the run (the next idle picks them up; no unbounded loop on the hook
    // path), `idle` when clean; a fenced-out runner touches nothing. Only a
    // failed advance returns `Err`: the runner is already retired dirty, so
    // skip the continuation.
    settled.context("settle completed export job within its capture deadline")?;
    match exit {
        CheckpointStageExit::UncommittedSettle {
            catalog_reservation,
            catalog_request,
        } => {
            if verified_partial && !catalog_request.mutation().is_terminal() {
                // A nonterminal no-checkpoint completion would retire this
                // receipt despite losing its actual source. Keep the
                // original reservation pending instead.
                drop(catalog_reservation);
                bail!("verified partial capture deadline elapsed before its checkpoint");
            }
            settle_preapplied_without_checkpoint(
                conn,
                *catalog_reservation,
                &catalog_request,
                capture_finalizer_deadline_millis,
                terminal_settlement_deadline,
            )
            .await?;
            if verified_partial {
                bail!("verified partial terminal capture remains content-free pending");
            }
            Ok(())
        }
        CheckpointStageExit::Uncommitted(result) | CheckpointStageExit::PostCheckpoint(result) => {
            result
        }
        CheckpointStageExit::Authorized => Ok(()),
        CheckpointStageExit::Expire(error) => Err(error),
    }
}

fn require_identity_plan_for_native_reasoning(
    reasoning: &ReasoningSourceDisposition,
    has_identity_plan: bool,
) -> Result<()> {
    if reasoning.state()
        == crate::internal::ai::observed_agents::reasoning::ReasoningSourceState::NativeProven
        && !has_identity_plan
    {
        bail!(
            "native reasoning cannot be committed without authenticated per-turn identity coverage"
        );
    }
    Ok(())
}

struct SubagentAssociationCommitPlan<'a> {
    base: &'a dyn crate::internal::ai::history::TracesTxnExtra,
    associations: &'a crate::internal::ai::subagent_content::SubagentBoundaryAssociations,
    scope: &'a CaptureScope,
    capture_session: &'a str,
    checkpoint_id: &'a str,
    deadline: Option<CaptureCommitDeadline>,
}

#[async_trait::async_trait]
impl crate::internal::ai::history::TracesTxnExtra for SubagentAssociationCommitPlan<'_> {
    async fn apply(
        &self,
        txn: &sea_orm::DatabaseTransaction,
        ctx: &crate::internal::ai::history::TracesCommitCtx,
    ) -> Result<()> {
        self.base.apply(txn, ctx).await?;
        self.associations
            .apply(
                txn,
                ctx,
                crate::internal::ai::subagent_content::SubagentBoundaryAssociationContext {
                    scope: self.scope,
                    capture_session: self.capture_session,
                    boundary_checkpoint_id: self.checkpoint_id,
                    deadline: self.deadline,
                },
            )
            .await
    }

    fn requires_versioned_coverage_binding(&self) -> bool {
        self.base.requires_versioned_coverage_binding()
    }
}

/// A0-02: materialise an independent `scope='subagent'` checkpoint at a
/// `SubagentStart` / `SubagentEnd` boundary.
///
/// Unlike [`write_committed_checkpoint`], this path deliberately does NOT
/// re-read the parent agent's on-disk transcript — a subagent boundary event
/// carries no separate transcript, and the parent session's `committed`
/// checkpoints already capture the full transcript. The subagent checkpoint's
/// tree carries a compact `metadata.json` describing the boundary (kind,
/// tool, source, timestamp, parent linkage) with an empty transcript blob, so
/// it is fully `list/show/export/prune/doctor`-able while staying cheap to
/// write. Parent linkage (`parent_checkpoint_id`) points at the session's
/// most recent `committed` checkpoint so `checkpoint show` can walk from a
/// nested run back to the enclosing turn/session checkpoint.
///
/// Crash-safety mirrors the committed path: an in-flight marker is persisted
/// before the traces commit and cleared after the catalog INSERT, so a
/// concurrent prune in the ref-CAS→catalog window cannot drop the commit.
#[allow(clippy::too_many_arguments)]
pub(super) async fn write_subagent_checkpoint(
    conn: &sea_orm::DatabaseConnection,
    repo_path: &std::path::Path,
    catalog_request: CaptureCatalogApplyRequest,
    catalog_reservation: LiveCaptureReservation,
    libra_session_id: &str,
    envelope: &SessionHookEnvelope,
    agent_kind: &str,
    event: &LifecycleEvent,
    ingress_event_id: uuid::Uuid,
    identity_scheme: LifecycleIdentityScheme,
    checkpoint_id: &str,
    redaction_report_json: &str,
    now: i64,
    scope: &CaptureScope,
    capture_deadline: Option<CaptureCommitDeadline>,
    child_capture: Option<&crate::internal::ai::subagent_content::SubagentCaptureSummary>,
    child_capture_incomplete: bool,
    child_context: &LiveCaptureContext<'_>,
    child_hints: &crate::internal::ai::observed_agents::live_capture::UntrustedChildSourceHints,
) -> Result<()> {
    use crate::internal::ai::observed_agents::{RedactedBytes, Redactor};

    // Resolve the parent `committed` checkpoint (if any) for linkage.
    let parent_checkpoint_id = latest_committed_checkpoint_id(conn, libra_session_id).await?;

    let boundary = match event.kind {
        LifecycleEventKind::SubagentStart => "start",
        _ => "end",
    };
    // Generic boundary id columns stay NULL. Source identities and exact
    // revision/turn proofs are owned by the source collector and link table;
    // an untrusted hook hint cannot authorize these columns. In particular,
    // `event.session_ref` is filled from the transcript PATH by
    // `build_lifecycle_event`, so persisting it as a subagent id would both
    // mislabel the column and leak a filesystem path. The tool NAME (if the
    // provider ever surfaces one) is recorded in the metadata / description
    // for context only; the id columns stay reserved for a future provider
    // that emits a real subagent/tool-use id.
    let tool_name = event.tool_name.clone();
    let subagent_session_id: Option<String> = None;
    let tool_use_id: Option<String> = None;
    // Redact the description before it lands in either the catalog row or
    // metadata.json — `tool_name` is event-derived and could carry a path.
    // Redacting once keeps the row's `description` column and the
    // metadata.json `description` (which doctor rebuilds the row from) byte
    // -identical.
    let description = redact_extracted_string(&match tool_name.as_deref() {
        Some(t) => format!("subagent {boundary} via {t}"),
        None => format!("subagent {boundary}"),
    });

    let checkpoint_id = checkpoint_id.to_string();

    // Compact metadata.json. Boundary/source/tool are event-derived and may
    // reference file paths, so the whole object passes the default redactor
    // before persist (same discipline as the committed writer's metadata).
    let source_redacted = event.source.clone().map(redact_extracted_json);
    let mut metadata = serde_json::json!({
        "schema_version": 1,
        "checkpoint_id": checkpoint_id,
        "session_id": libra_session_id,
        "agent_kind": agent_kind,
        "scope": "subagent",
        "provider_session_id": envelope.session_id,
        "working_dir": envelope.cwd,
        "created_at": now,
        // Flat linkage fields (A0-02): the subagent-scope class-2 doctor
        // repair rebuilds the catalog row straight from these, mirroring the
        // committed path's metadata-driven repair.
        "parent_checkpoint_id": parent_checkpoint_id,
        "subagent_session_id": subagent_session_id,
        "tool_use_id": tool_use_id,
        "description": description,
        "subagent": {
            "boundary": boundary,
            "kind": event.kind.to_string(),
            "tool": tool_name,
            "source": source_redacted,
            "timestamp": event.timestamp.to_rfc3339(),
        },
    });
    if let Some(summary) = child_capture {
        metadata["subagent"]["content_capture"] = serde_json::json!({
            "state": if child_capture_incomplete { "partial" } else { "complete" },
            "discovered": summary.discovered,
            "checkpoints_written": summary.checkpoints_written,
            "skipped_unchanged": summary.skipped_unchanged,
            "partial_sources": summary.partial_sources,
        });
    }
    let metadata = normalize_json_value(metadata);
    let metadata_bytes =
        serde_json::to_vec_pretty(&metadata).context("serialize subagent checkpoint metadata")?;
    let (metadata_redacted, _) = Redactor::new_default().redact(&metadata_bytes);

    // Empty transcript — the boundary carries no separate transcript body.
    let transcript_redacted = RedactedBytes::new_unchecked(Vec::new());

    // One canonical lifecycle evidence line for the boundary event.
    let canonical_ctx = CanonicalEventContext {
        agent_kind,
        session_id: libra_session_id,
        provider_session_id: &envelope.session_id,
        identity_scheme,
        provenance: serde_json::json!({
            "channel": "hook",
            "hook_event_name": envelope.hook_event_name,
        }),
    };
    // A boundary marker needs neither prompt body nor legacy session reference.
    let mut sidecar_event = event.clone();
    sidecar_event.session_ref = None;
    sidecar_event.prompt = None;
    let canonical_event = lifecycle_event_canonical_json_with_identity(
        &sidecar_event,
        &canonical_ctx,
        ingress_event_id,
        false,
    );
    let mut lifecycle_events_jsonl = serde_json::to_vec(&canonical_event)
        .context("serialize canonical subagent lifecycle evidence")?;
    lifecycle_events_jsonl.push(b'\n');
    let (lifecycle_events_redacted, _) = Redactor::new_default().redact(&lifecycle_events_jsonl);

    let report_value = normalize_json_value(
        serde_json::from_str::<serde_json::Value>(redaction_report_json)
            .unwrap_or_else(|_| serde_json::json!({})),
    );
    let redaction_report_bytes = serde_json::to_vec_pretty(&report_value)
        .context("serialize subagent checkpoint redaction_report.json")?;
    let (redaction_report_redacted, _) = Redactor::new_default().redact(&redaction_report_bytes);

    // Parent user-branch HEAD (same semantics/tolerance as committed path).
    let parent_commit: Option<String> =
        match crate::internal::head::Head::current_commit_result_with_conn(conn).await {
            Ok(commit) => commit.map(|h| h.to_string()),
            Err(crate::internal::branch::BranchStoreError::Corrupt { detail, .. })
                if detail.contains("HEAD reference is missing") =>
            {
                None
            }
            Err(err) => {
                return Err(anyhow!(
                    "failed to resolve HEAD while writing subagent checkpoint: {err}"
                ));
            }
        };

    let commit_plan = SubagentCheckpointCommitPlan::new(
        checkpoint_id.clone(),
        libra_session_id.to_string(),
        parent_checkpoint_id.clone(),
        parent_commit.clone(),
        tool_use_id.clone(),
        subagent_session_id.clone(),
        Some(description.clone()),
        now,
        scope.clone(),
    )
    .map_err(|error| anyhow!("prepare subagent checkpoint transaction companion: {error}"))?;
    let associations = child_capture
        .map(|summary| {
            summary.bind_boundary_associations(
                event.kind,
                ingress_event_id,
                child_context,
                child_hints,
            )
        })
        .unwrap_or_default();
    let association_plan = SubagentAssociationCommitPlan {
        base: &commit_plan,
        associations: &associations,
        scope,
        capture_session: libra_session_id,
        checkpoint_id: &checkpoint_id,
        deadline: capture_deadline,
    };
    // A boundary has no separate provider transcript. Its typed empty
    // `RedactedBytes` remains an explicitly partial capture projection,
    // preserving the historical compact boundary layout without a raw-byte
    // escape hatch.
    let checkpoint_payload = CheckpointRedactedPayload::from_redacted_capture(
        transcript_redacted,
        None,
        metadata_redacted,
        lifecycle_events_redacted,
        redaction_report_redacted,
    )
    .map_err(|error| anyhow!("prepare redacted subagent checkpoint payload: {error}"))?;
    let checkpoint_store =
        TracesCheckpointStore::new(conn, repo_path, libra_session_id, &checkpoint_id, &[])
            .map_err(|error| anyhow!("prepare subagent checkpoint store: {error}"))?
            .with_capture_scope(scope.clone());
    let marker_generation = checkpoint_store.marker_generation().to_string();
    let replay_key = catalog_request.action().action_key().to_string();
    let replay_fence = if child_capture.is_some()
        && let Some(deadline) = capture_deadline
    {
        crate::internal::ai::subagent_content::prepare_child_boundary_replay(
            conn,
            &catalog_request,
            deadline,
        )
        .await?
    } else {
        None
    };
    let checkpoint_request = CheckpointWriteRequest::new(
        &replay_key,
        &checkpoint_id,
        libra_session_id,
        agent_kind,
        parent_commit.as_deref(),
        crate::internal::ai::traces::CheckpointScope::Subagent,
        &marker_generation,
        tool_use_id.as_deref(),
        &checkpoint_payload,
        // Boundary checkpoints stay outside per-turn coverage, but their
        // tombstone barrier and catalog row remain transaction-injected.
        Some(&association_plan),
        capture_deadline,
    )
    .map_err(|error| anyhow!("prepare subagent checkpoint write request: {error}"))?;
    let coordinator_request =
        CaptureCoordinatorRequest::new(catalog_request, Some(checkpoint_request))
            .map_err(|error| anyhow!("prepare subagent capture coordinator request: {error}"))?;
    let coordinator: CaptureCoordinator<_, _> =
        catalog_reservation.into_coordinator(checkpoint_store);
    match coordinator
        .execute_preapplied(coordinator_request)
        .await
        .map_err(|error| anyhow!("subagent capture coordinator execution failed: {error}"))?
    {
        CaptureCoordinatorOutcome::CheckpointCommitted { .. }
        | CaptureCoordinatorOutcome::CheckpointInFlight { .. } => Ok(()),
        CaptureCoordinatorOutcome::CheckpointAlreadyExists { .. }
        | CaptureCoordinatorOutcome::AlreadyApplied => {
            if let (Some(fence), Some(deadline)) = (replay_fence.as_ref(), capture_deadline) {
                crate::internal::ai::subagent_content::replay_child_boundary_associations(
                    conn,
                    repo_path,
                    &associations,
                    fence,
                    deadline,
                )
                .await?;
            }
            Ok(())
        }
        CaptureCoordinatorOutcome::CheckpointConflict { reason } => {
            bail!("subagent capture coordinator left the write unchanged: {reason:?}")
        }
        CaptureCoordinatorOutcome::PendingCleanup { .. } => {
            bail!("subagent checkpoint write needs durable cleanup before completion")
        }
        CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
            bail!("subagent capture coordinator lost its catalog fence: {conflict:?}")
        }
        CaptureCoordinatorOutcome::FinalizerQuarantined { .. }
        | CaptureCoordinatorOutcome::FinalizerPending { .. }
        | CaptureCoordinatorOutcome::DurableFinalizer { .. }
        | CaptureCoordinatorOutcome::StateApplied => {
            bail!("subagent capture coordinator returned an invalid terminal outcome")
        }
    }
}

/// Extract metadata.json's `model` field from a lifecycle event's `model`
/// value, mirroring the E4-entire missing-`model` tolerance: absent or
/// unrecognisable shapes become the literal `"unknown"` rather than an
/// error. Providers emit either a plain string or an object carrying an
/// id/name-ish key.
/// Redact a single extracted string (model id, file path) through the
/// default `Redactor` before it lands in checkpoint metadata. Extraction
/// fields are derived from the redacted snapshot transcript, then pass a
/// second defensive scrubbing before they land in checkpoint metadata.
fn redact_extracted_string(value: &str) -> String {
    use crate::internal::ai::observed_agents::Redactor;
    let (bytes, _report) = Redactor::new_default().redact(value.as_bytes());
    String::from_utf8_lossy(bytes.as_ref()).into_owned()
}

/// Recursively redact every string leaf of a JSON value (used for the
/// skill-events array so anchors / names are scrubbed uniformly).
fn redact_extracted_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::String(text) => {
            serde_json::Value::String(redact_extracted_string(&text))
        }
        serde_json::Value::Array(items) => {
            serde_json::Value::Array(items.into_iter().map(redact_extracted_json).collect())
        }
        serde_json::Value::Object(map) => serde_json::Value::Object(
            map.into_iter()
                // Provider event JSON can carry transcript-derived dynamic
                // object keys as well as values. Scrub both before durable
                // projection; a redaction collision only drops redundant
                // diagnostic detail, never restores sensitive text.
                .map(|(key, value)| (redact_extracted_string(&key), redact_extracted_json(value)))
                .collect(),
        ),
        other => other,
    }
}

/// The parent checkpoint's child-extraction input after every discovered
/// child has crossed the capture snapshot boundary. This is intentionally a
/// typed redacted collection: runtime extraction has no raw child-byte API.
#[derive(Default)]
pub(super) struct SubagentExtractionSnapshots {
    pub(super) transcripts: Vec<crate::internal::ai::observed_agents::RedactedBytes>,
    pub(super) projections: Vec<CaptureSnapshotProjection>,
    pub(super) redaction_reports: Vec<crate::internal::ai::observed_agents::RedactionReport>,
    pub(super) deadline_omitted_source_count: usize,
}

impl SubagentExtractionSnapshots {
    pub(super) fn partial_source_count(&self) -> usize {
        self.projections
            .iter()
            .filter(|projection| projection.completeness == CaptureSnapshotCompleteness::Partial)
            .count()
            .saturating_add(self.deadline_omitted_source_count)
    }

    fn source_count(&self) -> usize {
        self.projections
            .len()
            .saturating_add(self.deadline_omitted_source_count)
    }

    fn children_omitted_for_deadline(&self) -> bool {
        self.deadline_omitted_source_count != 0
    }
}

/// Drop discovered native child bytes without inspecting, parsing, or
/// redacting them in the deadline-owning hook process. The extraction worker
/// receives an explicit safe partial flag instead; this avoids turning a
/// deadline path into a synchronous raw-child redaction path.
pub(super) fn omit_subagent_sources_for_deadline(
    sources: Vec<crate::internal::ai::subagent_content::DiscoveredSubagentContent>,
) -> SubagentExtractionSnapshots {
    SubagentExtractionSnapshots {
        deadline_omitted_source_count: sources
            .iter()
            .filter(|source| source.parent_extraction_enabled())
            .count(),
        ..Default::default()
    }
}

/// Consume securely discovered child sources into snapshot-owned redacted
/// output. A child source is never borrowed as raw bytes by the parent
/// runtime: `DiscoveredSubagentContent` moves it directly into its
/// authorization-bound snapshot source.
pub(super) fn snapshot_subagent_sources_for_extraction(
    sources: Vec<crate::internal::ai::subagent_content::DiscoveredSubagentContent>,
    parent_session_id: &str,
    policy: CaptureSnapshotPolicy,
) -> SubagentExtractionSnapshots {
    let mut snapshots = SubagentExtractionSnapshots {
        transcripts: Vec::with_capacity(sources.len()),
        projections: Vec::with_capacity(sources.len()),
        redaction_reports: Vec::with_capacity(sources.len()),
        deadline_omitted_source_count: 0,
    };
    for source in sources {
        let Some(kind) = source.observed_kind() else {
            // Discovery tags are not native authority. Do not introduce an
            // unknown-provider child into the parent's extraction input.
            snapshots.projections.push(CaptureSnapshotProjection {
                completeness:
                    crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Partial,
                partial_reason: Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch),
                source: None,
                transcript_redacted_bytes: 0,
                redaction_match_count: 0,
                redaction_bytes_scanned: 0,
                redaction_bytes_redacted: 0,
            });
            snapshots.redaction_reports.push(Default::default());
            continue;
        };
        let Some(snapshot) = source.into_parent_extraction_snapshot(
            parent_session_id,
            crate::internal::ai::observed_agents::live_capture::selected_snapshot_adapter(kind),
            policy,
        ) else {
            continue;
        };
        snapshots.projections.push(snapshot.safe_projection());
        snapshots
            .redaction_reports
            .push(snapshot.redaction_report().clone());
        if let Ok(Some(transcript)) = snapshot.try_into_ordinary() {
            snapshots.transcripts.push(transcript);
        }
    }
    snapshots
}

/// Build only static/count-based warnings for child snapshots. These are
/// durable parent metadata diagnostics, so they must not carry a provider
/// path, source key, child bytes, or lower-layer error string.
pub(super) fn subagent_extraction_warnings(
    discovery_warning: Option<&str>,
    partial_source_count: usize,
) -> Vec<String> {
    let mut warnings = discovery_warning
        .map(str::to_owned)
        .into_iter()
        .collect::<Vec<_>>();
    if partial_source_count > 0 {
        warnings.push(format!(
            "{partial_source_count} child source(s) could not be safely snapshotted; child attribution is incomplete"
        ));
    }
    warnings
}

/// Attach the bounded, content-free child snapshot status to extraction
/// metadata. `source_count` reflects children eligible for this aggregate;
/// `complete_source_count` reflects only the subset allowed into aggregate
/// extraction, so a partial snapshot can never be mistaken for attribution.
pub(super) fn attach_subagent_snapshot_metadata(
    extraction: &mut serde_json::Value,
    snapshots: &SubagentExtractionSnapshots,
) {
    if snapshots.source_count() == 0 {
        return;
    }
    let Some(object) = extraction.as_object_mut() else {
        return;
    };
    let mut partial_reasons = snapshots
        .projections
        .iter()
        .filter_map(|projection| projection.partial_reason)
        .collect::<Vec<_>>();
    partial_reasons.extend(std::iter::repeat_n(
        crate::internal::ai::capture::snapshot::CaptureSnapshotPartialReason::DeadlineExceeded,
        snapshots.deadline_omitted_source_count,
    ));
    object.insert(
        "subagent_snapshot".into(),
        serde_json::json!({
            "schema_version": 1,
            "source_count": snapshots.source_count(),
            "complete_source_count": snapshots.transcripts.len(),
            "partial_source_count": snapshots.partial_source_count(),
            "partial_reasons": partial_reasons,
            "deadline_omitted_source_count": snapshots.deadline_omitted_source_count,
        }),
    );
}

/// Runtime compatibility wrapper. Extraction/projection ownership lives in
/// `CaptureSnapshotService`; deadline callers use its helper-backed sibling
/// rather than composing parser work in this module.
pub(super) fn build_extraction_metadata(
    agent_kind: &str,
    redacted_parent: Option<&crate::internal::ai::observed_agents::RedactedBytes>,
    redacted_subagents: &[crate::internal::ai::observed_agents::RedactedBytes],
    subagent_snapshot_warnings: &[String],
) -> serde_json::Value {
    CaptureSnapshotService::build_extraction_projection(
        agent_kind,
        redacted_parent,
        redacted_subagents,
        subagent_snapshot_warnings,
    )
}

fn checkpoint_model_field(model: Option<&serde_json::Value>) -> String {
    match model {
        Some(serde_json::Value::String(name)) if !name.trim().is_empty() => name.clone(),
        Some(serde_json::Value::Object(obj)) => ["id", "model", "name", "display_name"]
            .iter()
            .find_map(|key| obj.get(*key).and_then(serde_json::Value::as_str))
            .filter(|name| !name.trim().is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| "unknown".to_string()),
        _ => "unknown".to_string(),
    }
}

/// Merge a [`RedactionReport`](crate::internal::ai::observed_agents::RedactionReport)
/// produced while redacting the captured transcript into the checkpoint's
/// existing `redaction_report` JSON object (built from the event payload's
/// prompt / tool-input matches). Appends the transcript's `matches` and adds
/// its `bytes_scanned` / `bytes_redacted` counters so the stored report stays
/// consistent with the stored (redacted) transcript blob. A non-object
/// `report` (only possible from a malformed input string) is left untouched.
///
/// (DR-04a) The provider-root trust gate that used to live here moved to
/// `observed_agents::transcript_source::transcript_path_within_provider_root`,
/// the single source of truth behind the `TranscriptSource` seam.
fn merge_redaction_report_into(
    report: &mut serde_json::Value,
    extra: &crate::internal::ai::observed_agents::RedactionReport,
) {
    let Some(obj) = report.as_object_mut() else {
        return;
    };
    use crate::internal::ai::observed_agents::MAX_REDACTION_MATCH_SAMPLES;

    // Reports are durable diagnostics. Preserve exact aggregate counts but
    // cap detailed samples across event and transcript redaction alike, so a
    // multi-field event cannot bypass the per-redactor bound.
    let mut dropped_matches = obj
        .remove("dropped_matches")
        .and_then(|value| value.as_u64())
        .and_then(|value| usize::try_from(value).ok())
        .unwrap_or(0)
        .saturating_add(extra.dropped_matches);
    let mut matches = match obj.remove("matches") {
        Some(serde_json::Value::Array(matches)) => matches,
        _ => Vec::new(),
    };
    if matches.len() > MAX_REDACTION_MATCH_SAMPLES {
        dropped_matches = dropped_matches
            .saturating_add(matches.len().saturating_sub(MAX_REDACTION_MATCH_SAMPLES));
        matches.truncate(MAX_REDACTION_MATCH_SAMPLES);
    }
    for matched in &extra.matches {
        if matches.len() >= MAX_REDACTION_MATCH_SAMPLES {
            dropped_matches = dropped_matches.saturating_add(1);
            continue;
        }
        match serde_json::to_value(matched) {
            Ok(value) => matches.push(value),
            Err(_) => dropped_matches = dropped_matches.saturating_add(1),
        }
    }
    obj.insert("matches".to_string(), serde_json::Value::Array(matches));
    if dropped_matches == 0 {
        obj.remove("dropped_matches");
    } else {
        obj.insert("dropped_matches".to_string(), json!(dropped_matches));
    }
    for (key, added) in [
        ("bytes_scanned", extra.bytes_scanned),
        ("bytes_redacted", extra.bytes_redacted),
    ] {
        let current = obj
            .get(key)
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        obj.insert(
            key.to_string(),
            serde_json::json!(current.saturating_add(added as u64)),
        );
    }
}

/// Retain a bounded redaction-match sample while preserving the exact count
/// across independently redacted hook fields.
pub(super) fn append_redaction_report_bounded(
    matches: &mut Vec<crate::internal::ai::observed_agents::RedactionMatch>,
    dropped_matches: &mut usize,
    report: crate::internal::ai::observed_agents::RedactionReport,
) {
    use crate::internal::ai::observed_agents::MAX_REDACTION_MATCH_SAMPLES;

    *dropped_matches = dropped_matches.saturating_add(report.dropped_matches);
    for matched in report.matches {
        if matches.len() < MAX_REDACTION_MATCH_SAMPLES {
            matches.push(matched);
        } else {
            *dropped_matches = dropped_matches.saturating_add(1);
        }
    }
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod codex_child_wait_error_local_tests {
    use super::*;

    fn catalog_wait(capacity: bool) -> CaptureCatalogError {
        if capacity {
            CaptureCatalogError::ChildGateCapacity
        } else {
            CaptureCatalogError::ChildCheckpointsPending
        }
    }

    fn wrap(kind: u8, cause: CaptureCoordinatorError) -> CaptureCoordinatorError {
        match kind {
            0 => CaptureCoordinatorError::DurableCheckpointCompletion {
                checkpoint_id: "owned-checkpoint".into(),
                cause: Box::new(cause),
            },
            1 => CaptureCoordinatorError::CheckpointReplayCompletion {
                checkpoint_id: "owned-checkpoint".into(),
                cause: Box::new(cause),
            },
            2 => CaptureCoordinatorError::CheckpointWriteFinalizer {
                checkpoint: CheckpointStoreError::DeadlineExceeded,
                finalizer: Box::new(cause),
            },
            _ => CaptureCoordinatorError::CheckpointConflictFinalizer {
                reason: CheckpointConflictReason::ReplayPayloadMismatch,
                finalizer: Box::new(cause),
            },
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_wait_error_typed_catalog_and_context() {
        for capacity in [false, true] {
            assert!(child_checkpoint_wait_error(&catalog_wait(capacity)).is_some());
            for boundary in 0..3 {
                let error = match boundary {
                    0 => CaptureCoordinatorError::CatalogApply(catalog_wait(capacity)),
                    1 => CaptureCoordinatorError::CatalogComplete(catalog_wait(capacity)),
                    _ => CaptureCoordinatorError::CatalogFinalize(catalog_wait(capacity)),
                };
                let error = anyhow::Error::new(error)
                    .context("checkpoint stage")
                    .context("live capture");
                assert_eq!(
                    child_checkpoint_wait_error(error.as_ref())
                        .unwrap()
                        .to_string(),
                    catalog_wait(capacity).to_string()
                );
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_wait_error_follows_non_source_coordinator_fields() {
        for capacity in [false, true] {
            for outer in 0..4 {
                for inner in 0..4 {
                    let error = wrap(
                        outer,
                        wrap(
                            inner,
                            CaptureCoordinatorError::CatalogFinalize(catalog_wait(capacity)),
                        ),
                    );
                    // The two durable wrappers omit Error::source; chain-only
                    // code cannot discover their inner catalog wait.
                    if outer < 2 {
                        let chain = anyhow::Error::new(error);
                        assert!(!chain.chain().any(|cause| cause.is::<CaptureCatalogError>()));
                        assert_eq!(
                            child_checkpoint_wait_error(chain.as_ref()).is_some(),
                            inner < 2
                        );
                    } else {
                        assert!(child_checkpoint_wait_error(&error).is_none());
                    }
                }
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_error_progress_real_checkpoint_failure_wins_over_child_wait() {
        for capacity in [false, true] {
            for kind in [2, 3] {
                let error = wrap(
                    kind,
                    CaptureCoordinatorError::CatalogFinalize(catalog_wait(capacity)),
                );
                assert!(
                    child_checkpoint_wait_error(&error).is_none(),
                    "a real checkpoint failure/conflict remains causal even if finalizer also waits"
                );
            }
        }
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_wait_error_does_not_classify_strings_or_other_failures() {
        for error in [
            anyhow!(CaptureCatalogError::ChildCheckpointsPending.to_string()),
            anyhow!(CaptureCatalogError::ChildGateCapacity.to_string()),
            anyhow::Error::new(CaptureCatalogError::MalformedReceiptLedger),
            anyhow::Error::new(CaptureCatalogError::Database),
            anyhow::Error::new(wrap(
                0,
                CaptureCoordinatorError::CatalogComplete(CaptureCatalogError::ScopeRejected),
            )),
            anyhow::Error::new(wrap(
                2,
                CaptureCoordinatorError::CatalogFinalize(CaptureCatalogError::DeadlineExceeded),
            )),
        ] {
            assert!(child_checkpoint_wait_error(error.as_ref()).is_none());
        }
    }
}

#[cfg(test)]
#[cfg(feature = "test-live-agent")]
mod codex_child_parent_extraction_local_tests {
    use super::*;
    use crate::internal::ai::subagent_content::DiscoveredSubagentContent;

    fn source(kind: &str, text: &str, usage: usize) -> DiscoveredSubagentContent {
        DiscoveredSubagentContent::fixture(
            kind,
            "source/sha256/local-parent-extraction",
            format!(
                "{{\"type\":\"assistant\",\"message\":{{\"role\":\"assistant\",\"content\":\"{text}\",\"usage\":{{\"input_tokens\":{usage},\"output_tokens\":1}}}}}}\n"
            )
            .as_bytes(),
            None,
        )
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_parent_extraction_skips_content_usage_and_status() {
        let snapshots = snapshot_subagent_sources_for_extraction(
            vec![source("codex", "CODEX_CHILD_STAYS_IN_ITS_CHECKPOINT", 999)],
            "codex__parent",
            CaptureSnapshotPolicy::default(),
        );
        assert!(snapshots.transcripts.is_empty());
        assert!(snapshots.projections.is_empty());
        assert!(snapshots.redaction_reports.is_empty());
        assert_eq!(snapshots.source_count(), 0);
        assert_eq!(snapshots.partial_source_count(), 0);
        let mut extraction = serde_json::json!({"parent": "unchanged"});
        attach_subagent_snapshot_metadata(&mut extraction, &snapshots);
        assert_eq!(extraction, serde_json::json!({"parent": "unchanged"}));
        assert!(subagent_extraction_warnings(None, snapshots.partial_source_count()).is_empty());

        let snapshots = snapshot_subagent_sources_for_extraction(
            vec![
                source("codex", "CODEX_CHILD_STAYS_IN_ITS_CHECKPOINT", 999),
                source("claude_code", "CLAUDE_CHILD_STILL_AGGREGATES", 3),
            ],
            "claude__parent",
            CaptureSnapshotPolicy::default(),
        );
        assert_eq!(snapshots.transcripts.len(), 1);
        assert_eq!(snapshots.projections.len(), 1);
        assert_eq!(snapshots.redaction_reports.len(), 1);
        assert_eq!(snapshots.source_count(), 1);
        assert_eq!(snapshots.partial_source_count(), 0);
        let mut extraction =
            build_extraction_metadata("claude_code", None, &snapshots.transcripts, &[]);
        assert_eq!(extraction["subagent_token_usage"]["input_tokens"], 3);
        attach_subagent_snapshot_metadata(&mut extraction, &snapshots);
        assert_eq!(extraction["subagent_snapshot"]["source_count"], 1);
        assert_eq!(extraction["subagent_snapshot"]["complete_source_count"], 1);
        assert!(
            !serde_json::to_string(&extraction)
                .expect("safe status")
                .contains("CODEX_CHILD")
        );
    }

    #[test]
    #[ignore = "operator-local Codex acceptance; never run in CI"]
    fn codex_child_parent_extraction_deadline_counts_only_eligible_sources() {
        let snapshots = omit_subagent_sources_for_deadline(vec![source(
            "codex",
            "CODEX_CHILD_STAYS_IN_ITS_CHECKPOINT",
            999,
        )]);
        assert_eq!(snapshots.source_count(), 0);
        assert_eq!(snapshots.partial_source_count(), 0);
        assert!(!snapshots.children_omitted_for_deadline());
        let mut extraction = serde_json::json!({"parent": "unchanged"});
        attach_subagent_snapshot_metadata(&mut extraction, &snapshots);
        assert_eq!(extraction, serde_json::json!({"parent": "unchanged"}));

        let snapshots = omit_subagent_sources_for_deadline(vec![
            source("codex", "CODEX_CHILD_STAYS_IN_ITS_CHECKPOINT", 999),
            source("claude_code", "CLAUDE_CHILD_OMITTED_BY_DEADLINE", 3),
        ]);
        assert!(snapshots.transcripts.is_empty());
        assert_eq!(snapshots.deadline_omitted_source_count, 1);
        assert_eq!(snapshots.source_count(), 1);
        assert_eq!(snapshots.partial_source_count(), 1);
        assert!(snapshots.children_omitted_for_deadline());
        attach_subagent_snapshot_metadata(&mut extraction, &snapshots);
        assert_eq!(extraction["subagent_snapshot"]["source_count"], 1);
        assert_eq!(
            extraction["subagent_snapshot"]["deadline_omitted_source_count"],
            1
        );
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use sea_orm::{ConnectionTrait, Statement};
    use serial_test::serial;

    use super::*;
    use crate::internal::ai::observed_agents::{
        AgentKind, AgentSessionCtx, live_capture::scoped_test_snapshot_adapter,
        reasoning::carrier_test_support as fixture,
    };

    struct TestHomeScope(Option<std::ffi::OsString>);
    impl TestHomeScope {
        fn set(path: &std::path::Path) -> Self {
            let old = std::env::var_os("LIBRA_TEST_HOME");
            // SAFETY: callers hold the serial env lane and Drop restores it.
            unsafe {
                std::env::set_var("LIBRA_TEST_HOME", path);
            }
            Self(old)
        }
    }
    impl Drop for TestHomeScope {
        fn drop(&mut self) {
            // SAFETY: paired with set, under the same serial env lane.
            unsafe {
                match &self.0 {
                    Some(v) => std::env::set_var("LIBRA_TEST_HOME", v),
                    None => std::env::remove_var("LIBRA_TEST_HOME"),
                }
            }
        }
    }
    async fn live_fixture() -> (tempfile::TempDir, sea_orm::DatabaseConnection) {
        let dir = tempfile::tempdir().unwrap();
        let conn =
            crate::internal::db::create_database(dir.path().join(util::DATABASE).to_str().unwrap())
                .await
                .unwrap();
        crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
            .await
            .unwrap();
        std::fs::create_dir_all(dir.path().join("objects")).unwrap();
        (dir, conn)
    }

    #[cfg(unix)]
    const VERIFIED_PARTIAL_CAPTURE: &str = "opencode__pure-partial-store";
    #[cfg(unix)]
    const VERIFIED_PARTIAL_PROVIDER: &str = "pure-partial-store";
    #[cfg(unix)]
    const VERIFIED_PARTIAL_TEXT: &str = "safe ordinary reply";
    #[cfg(unix)]
    const VERIFIED_PARTIAL_CANARY: &str = "PRIVATE_ASSOCIATED_REASONING_CANARY";

    /// Pure source/storage fixture. It never runs an exporter, CLI, provider
    /// Session, hook parser, account lookup, or fake collection service.
    #[cfg(unix)]
    struct VerifiedPartialStoreFixture {
        _directory: tempfile::TempDir,
        repo: std::path::PathBuf,
        storage: std::path::PathBuf,
        conn: sea_orm::DatabaseConnection,
        scope: CaptureScope,
    }

    #[cfg(unix)]
    impl VerifiedPartialStoreFixture {
        async fn new() -> Self {
            let directory = tempfile::tempdir().expect("create partial store fixture");
            let repo = directory
                .path()
                .canonicalize()
                .expect("canonical fixture root");
            let storage = repo.join(util::ROOT_DIR);
            std::fs::create_dir_all(storage.join("objects")).expect("create local object store");
            let conn = crate::internal::db::create_database(
                storage
                    .join(util::DATABASE)
                    .to_str()
                    .expect("UTF-8 fixture database"),
            )
            .await
            .expect("create source/storage database");
            crate::internal::workspace::RepoIdentity::resolve_or_init(&conn)
                .await
                .expect("initialize isolated repository identity");
            let scope = CaptureScope::resolve(&conn, &repo)
                .await
                .expect("resolve own scope");
            Self {
                _directory: directory,
                repo,
                storage,
                conn,
                scope,
            }
        }

        fn snapshot(&self) -> CaptureSnapshot {
            crate::internal::ai::capture::snapshot::verified_partial_fixture_snapshot(
                VERIFIED_PARTIAL_CAPTURE,
                VERIFIED_PARTIAL_PROVIDER,
                &self.repo,
            )
        }

        async fn carrier(&self) -> VerifiedOrdinaryPartialSnapshot {
            let snapshot = self.snapshot();
            assert_eq!(
                export_snapshot_route(&snapshot),
                ExportSnapshotRoute::VerifiedPartial
            );
            assert!(
                snapshot.rejects_capture_source_consumer(),
                "default readiness must stay closed"
            );
            snapshot
                .try_into_verified_ordinary_partial_in_scope_until(
                    &self.conn,
                    &self.scope,
                    &self.storage,
                    &self.repo,
                    Instant::now() + Duration::from_secs(30),
                )
                .await
                .expect("consume strict source into scope-bound partial carrier")
        }

        async fn scalar(&self, sql: &str) -> i64 {
            self.conn
                .query_one_raw(Statement::from_string(
                    self.conn.get_database_backend(),
                    sql.to_owned(),
                ))
                .await
                .expect("read fixture count")
                .expect("count row")
                .try_get_by("n")
                .expect("decode fixture count")
        }

        async fn current(
            &self,
        ) -> Option<crate::internal::ai::capture::state::DurableCaptureState> {
            use crate::internal::ai::capture::state::{CapturePhase, DurableCaptureState};
            self.conn.query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT state, stopped_at, sync_revision FROM agent_session WHERE session_id = ?",
                [VERIFIED_PARTIAL_CAPTURE.into()],
            )).await.expect("read store state").map(|row| DurableCaptureState {
                phase: CapturePhase::from_db(&row.try_get_by::<String,_>("state").expect("phase")).expect("typed phase"),
                stopped_at: row.try_get_by("stopped_at").expect("stopped timestamp"),
                sync_revision: row.try_get_by("sync_revision").expect("revision"),
            })
        }

        async fn write(
            &self,
            action: u128,
            terminal: bool,
            deadline: Option<CaptureCommitDeadline>,
        ) -> Result<String> {
            self.write_with_tombstone(action, terminal, deadline, false)
                .await
        }

        async fn write_with_tombstone(
            &self,
            action: u128,
            terminal: bool,
            deadline: Option<CaptureCommitDeadline>,
            tombstone_before_store: bool,
        ) -> Result<String> {
            use crate::internal::ai::capture::{
                catalog::{CaptureCatalogAction, CaptureCatalogMutation, CaptureCatalogSession},
                checkpoint::checkpoint_id_for_capture_action,
                live::ExportJobLeaseStore,
                state::{LifecycleReducerInput, reduce_lifecycle},
            };
            let source = self.carrier().await;
            let kind = if terminal {
                LifecycleEventKind::SessionEnd
            } else {
                LifecycleEventKind::TurnEnd
            };
            let event_id = uuid::Uuid::from_u128(action);
            let current = self.current().await;
            let reduced = reduce_lifecycle(LifecycleReducerInput {
                current,
                event_kind: kind,
                event_id,
                occurred_at: 1,
                deadline: None,
            })?;
            let checkpoint_id = checkpoint_id_for_capture_action(event_id, reduced.checkpoint);
            let request = CaptureCatalogApplyRequest::new(
                self.scope.clone(),
                CaptureCatalogSession::new(
                    VERIFIED_PARTIAL_CAPTURE,
                    "opencode",
                    VERIFIED_PARTIAL_PROVIDER,
                    self.repo.to_string_lossy().into_owned(),
                )?,
                CaptureCatalogAction::from_ingress(event_id, None, kind)?,
                CaptureCatalogMutation::from_reducer(current, &reduced, 1)?,
            )?;
            let reservation =
                match reserve_capture_catalog_until(self.conn.clone(), &request, None).await? {
                    CaptureCoordinatorReserveOutcome::Reserved(reservation) => *reservation,
                    CaptureCoordinatorReserveOutcome::AlreadyApplied => {
                        assert!(
                            TracesCheckpointStore::durable_replay_exists_for(
                                &self.conn,
                                &checkpoint_id,
                                VERIFIED_PARTIAL_CAPTURE,
                                crate::internal::ai::traces::CheckpointScope::Committed,
                                None,
                            )
                            .await?
                        );
                        return Ok(checkpoint_id);
                    }
                    CaptureCoordinatorReserveOutcome::ConflictUnchanged { conflict } => {
                        bail!("source/storage reservation did not proceed: {conflict:?}")
                    }
                };
            let request = reservation.effective_request().clone();
            if tombstone_before_store {
                self.conn.execute_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "INSERT INTO agent_import_tombstone (tombstone_id, agent_kind, provider_session_id, erased_session_id, erased_at) VALUES ('partial-store-fence','opencode',?,?,1)",
                    [VERIFIED_PARTIAL_PROVIDER.into(), VERIFIED_PARTIAL_CAPTURE.into()],
                )).await.expect("insert the real concurrent tombstone fence");
            }
            let envelope = SessionHookEnvelope {
                hook_event_name: "pure-storage-action".to_owned(),
                session_id: VERIFIED_PARTIAL_PROVIDER.to_owned(),
                cwd: self.repo.to_string_lossy().into_owned(),
                transcript_path: None,
                extra: serde_json::Map::new(),
            };
            let event = LifecycleEvent {
                kind,
                session_id: VERIFIED_PARTIAL_PROVIDER.to_owned(),
                session_ref: None,
                prompt: None,
                model: None,
                source: None,
                tool_name: None,
                tool_input: None,
                tool_response: None,
                assistant_message: None,
                timestamp: Utc::now(),
            };
            run_checkpoint_stage::<ExportJobLeaseStore<'_>>(
                CheckpointStageInput {
                    conn: &self.conn,
                    // The checkpoint-stage API receives the Libra storage
                    // directory; scope and source authorization use the
                    // distinct worktree root above.
                    repo_path: &self.storage,
                    catalog_request: request.clone(),
                    diagnostic_request: request,
                    catalog_reservation: reservation,
                    libra_session_id: VERIFIED_PARTIAL_CAPTURE,
                    envelope: &envelope,
                    agent_kind: "opencode",
                    event: &event,
                    ingress_event_id: event_id,
                    identity_scheme: LifecycleIdentityScheme::FallbackActionHmacV1,
                    checkpoint_id: &checkpoint_id,
                    capture_deadline: deadline,
                    terminal_settlement_deadline: None,
                    capture_finalizer_deadline_millis: deadline
                        .map(CaptureCommitDeadline::sqlite_not_after_millis),
                    now: 1,
                    scope: &self.scope,
                    subagent_sources: Vec::new(),
                    subagent_discovery_warning: None,
                    subagent_capture: None,
                    incomplete_parent_extraction: false,
                    report_value: json!({}),
                    source: CheckpointStageSource::VerifiedPartial(source),
                    claim_channel: "export",
                    live_reservation: None,
                },
                None,
            )
            .await?;
            Ok(checkpoint_id)
        }

        /// Read the same manifest/role-proof primitives used by ordinary
        /// checkpoint export, without invoking its CLI or account surface.
        async fn read_back(&self, checkpoint_id: &str) -> (Vec<u8>, serde_json::Value) {
            use crate::internal::ai::checkpoint_reader::{
                checkpoint_plain_files, checkpoint_plain_manifest, checkpoint_transcript_oids,
                read_checkpoint_object_bounded, read_tree_object, reasoning_artifact_tree_oids,
                subtree,
            };
            let row = self.conn.query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT tree_oid, metadata_blob_oid FROM agent_checkpoint WHERE checkpoint_id = ?",
                [checkpoint_id.into()],
            )).await.expect("read actual catalog row").expect("checkpoint row exists");
            let tree_oid: String = row.try_get_by("tree_oid").expect("tree oid");
            let root =
                read_tree_object(&self.storage, &tree_oid).expect("read committed traces tree");
            let checkpoints = subtree(&self.storage, &root, "checkpoint").expect("checkpoint root");
            let prefix = subtree(&self.storage, &checkpoints, &checkpoint_id[..2])
                .expect("checkpoint prefix");
            let inner =
                subtree(&self.storage, &prefix, &checkpoint_id[2..]).expect("checkpoint leaf");
            let artifacts = reasoning_artifact_tree_oids(&self.storage, &inner)
                .expect("validate reasoning roles");
            assert!(
                artifacts.is_empty(),
                "partial storage adds no native artifact role"
            );
            let ordinary = checkpoint_plain_files(&self.storage, &inner, &artifacts)
                .expect("validate ordinary roles");
            let manifest = checkpoint_plain_manifest(&self.storage, &inner, &artifacts, &ordinary)
                .expect("validate manifest")
                .expect("modern checkpoint manifest");
            let oids = checkpoint_transcript_oids(&self.storage, &inner, &manifest, &artifacts)
                .expect("resolve transcript roles");
            let mut bytes = Vec::new();
            for oid in oids {
                let (part, truncated) =
                    read_checkpoint_object_bounded(&self.storage, &oid, 1024 * 1024)
                        .expect("read stored ordinary transcript");
                assert!(!truncated);
                bytes.extend(part);
            }
            let metadata_oid: String = row.try_get_by("metadata_blob_oid").expect("metadata oid");
            let metadata_oid = metadata_oid.parse().expect("typed metadata oid");
            let (metadata, truncated) =
                read_checkpoint_object_bounded(&self.storage, &metadata_oid, 1024 * 1024)
                    .expect("read actual metadata blob");
            assert!(!truncated);
            (
                bytes,
                serde_json::from_slice(&metadata).expect("decode stored metadata"),
            )
        }

        /// Pure SQL lease seam, separate from the no-deadline direct object
        /// store seam. It does not start a provider exporter or Session.
        async fn settle_real_lease(&self, exit: CheckpointStageExit) -> (String, i64) {
            use crate::internal::ai::capture::live::ExportJobLeaseStore;
            let port = ExportJobLeaseStore::new(&self.conn);
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30))
                .expect("bounded lease deadline");
            let target = LiveExportTarget {
                agent_kind: "opencode",
                provider_session_id: VERIFIED_PARTIAL_PROVIDER,
                scope: &self.scope,
            };
            let runner = LiveExportRunner::admit(&port, target, deadline)
                .await
                .expect("real lease admission")
                .expect("fixture owns runner");
            let (disposition, reason, log) = checkpoint_stage_lease_disposition(
                &exit,
                VERIFIED_PARTIAL_CAPTURE,
                "pure-partial-lease",
            );
            runner
                .settle(disposition, reason, log)
                .await
                .expect("settle real SQL lease");
            let row = self.conn.query_one_raw(Statement::from_sql_and_values(
                self.conn.get_database_backend(),
                "SELECT state, processed_generation, owner FROM agent_export_job WHERE agent_kind = 'opencode' AND provider_session_id = ?",
                [VERIFIED_PARTIAL_PROVIDER.into()],
            )).await.expect("read actual export job").expect("export job exists");
            assert!(
                row.try_get_by::<Option<String>, _>("owner")
                    .expect("lease owner")
                    .is_none()
            );
            (
                row.try_get_by("state").expect("lease state"),
                row.try_get_by("processed_generation")
                    .expect("processed generation"),
            )
        }

        async fn assert_no_native_or_partial_payload(&self) {
            assert_eq!(
                self.scalar("SELECT COUNT(*) AS n FROM agent_coverage_claim")
                    .await,
                0
            );
            assert_eq!(
                self.scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision")
                    .await,
                0
            );
            assert_eq!(
                self.scalar(
                    "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_reasoning_identity'"
                )
                .await,
                0
            );
            assert_eq!(self.scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope IN ('agent_capture_session_alias','agent_capture_pending_chunk')").await, 0);
            let rows = self.conn.query_all_raw(Statement::from_string(self.conn.get_database_backend(),
                "SELECT value FROM metadata_kv WHERE scope IN ('agent_capture_pending','agent_capture_quarantine')".to_owned())).await.expect("read content-free pending intents");
            for row in rows {
                let value: String = row.try_get_by("value").expect("pending value");
                assert!(!value.contains(VERIFIED_PARTIAL_TEXT));
                assert!(!value.contains(VERIFIED_PARTIAL_CANARY));
            }
        }
    }

    #[cfg(unix)]
    const COV2_RUNTIME_CANONICAL: &str = r#"[{"role":"user","text":"hi"},{"provider":"opencode","role":"reasoning","source_kind":"reasoning","text":"before <REDACTED:anthropic-api-key> after"},{"role":"assistant","text":"hello"}]"#;
    #[cfg(unix)]
    const COV2_RUNTIME_SHA: &str =
        "7973604f43b515e1af6b1206e85c4fdc88447eed5a7822ca0ec3a77224b792a9";

    #[cfg(unix)]
    #[derive(Debug, PartialEq, Eq)]
    enum Cov2RuntimeDelivery {
        Written(String),
        Covered(String),
        LegacyReplay(String),
        AlreadyApplied(String),
    }

    #[cfg(unix)]
    fn cov2_runtime_normalizer(
        bytes: &RedactedBytes,
    ) -> Vec<crate::internal::ai::observed_agents::coverage::NormalizedTurn> {
        crate::internal::ai::observed_agents::coverage::normalize_opencode_export(bytes.bytes())
    }

    #[cfg(unix)]
    impl VerifiedPartialStoreFixture {
        fn cov2_visible_source(&self, user: &str, extra_ordinary_turn: bool) -> Vec<u8> {
            let mut messages = vec![
                json!({"id":user,"type":"user","text":"hi"}),
                json!({"id":format!("a-{user}"),"type":"assistant","content":[
                    {"id":format!("r-{user}"),"type":"reasoning","text":"before sk-ant-EXAMPLE0000000000000000 after"},
                    {"id":format!("t-{user}"),"type":"text","text":"hello"}]}),
                json!({"id":format!("i-{user}"),"type":"idle","outcome":"succeeded"}),
            ];
            if extra_ordinary_turn {
                messages.extend([
                    json!({"id":"ordinary-new-key","type":"user","text":"ordinary next"}),
                    json!({"id":"ordinary-answer","type":"assistant","content":[{"id":"ordinary-text","type":"text","text":"ordinary reply"}]}),
                    json!({"id":"ordinary-idle","type":"idle","outcome":"succeeded"}),
                ]);
            }
            serde_json::to_vec(
                &json!({"info":{"id":VERIFIED_PARTIAL_PROVIDER},"messages":messages}),
            )
            .expect("bounded source fixture")
        }

        fn cov2_runtime_snapshot(&self, bytes: Vec<u8>) -> CaptureSnapshot {
            use crate::internal::ai::{
                capture::snapshot::AuthorizedReasoningSourceContext,
                observed_agents::{ExportAuthorized, agent_for},
            };
            CaptureSnapshotService::capture_authorized_with_reasoning_context(
                TranscriptSource::Bytes {
                    auth: ExportAuthorized::issue("opencode", VERIFIED_PARTIAL_CAPTURE, &bytes),
                    bytes,
                },
                agent_for(AgentKind::OpenCode),
                AuthorizedReasoningSourceContext::authorized_export(
                    "opencode",
                    VERIFIED_PARTIAL_CAPTURE,
                    VERIFIED_PARTIAL_PROVIDER,
                ),
                CaptureSnapshotPolicy::default(),
            )
        }

        /// Same post-export production stages and real SQL lease, with an
        /// already authorized source instead of an exporter process. The
        /// stage retains its paired deadline and runs the real private Libra
        /// extraction/object-I/O helpers, without a provider or exporter.
        async fn cov2_deliver(
            &self,
            action: u128,
            terminal: bool,
            mut snapshot: CaptureSnapshot,
            pre_upgrade_v1: bool,
            interrupt_receipt: bool,
        ) -> Result<Cov2RuntimeDelivery> {
            use crate::internal::ai::capture::{
                catalog::{CaptureCatalogAction, CaptureCatalogMutation, CaptureCatalogSession},
                checkpoint::checkpoint_id_for_capture_action,
                live::ExportJobLeaseStore,
                state::{LifecycleReducerInput, reduce_lifecycle},
            };
            if pre_upgrade_v1 {
                // Model the pre-activation producer's ordinary-only snapshot;
                // no native authority or public typed-text issuer is involved.
                drop(snapshot.take_coverage_runtime());
            }
            let kind = if terminal {
                LifecycleEventKind::SessionEnd
            } else {
                LifecycleEventKind::TurnEnd
            };
            let event_id = uuid::Uuid::from_u128(action);
            let current = self.current().await;
            let reduced = reduce_lifecycle(LifecycleReducerInput {
                current,
                event_kind: kind,
                event_id,
                occurred_at: 1,
                deadline: None,
            })?;
            let checkpoint_id = checkpoint_id_for_capture_action(event_id, reduced.checkpoint);
            let request = CaptureCatalogApplyRequest::new(
                self.scope.clone(),
                CaptureCatalogSession::new(
                    VERIFIED_PARTIAL_CAPTURE,
                    "opencode",
                    VERIFIED_PARTIAL_PROVIDER,
                    self.repo.to_string_lossy().into_owned(),
                )?,
                CaptureCatalogAction::from_ingress(
                    event_id,
                    Some(&format!("capture-dedup-v2:{action:064x}")),
                    kind,
                )?,
                CaptureCatalogMutation::from_reducer(current, &reduced, 1)?,
            )?;
            let reservation =
                match reserve_capture_catalog_until(self.conn.clone(), &request, None).await? {
                    CaptureCoordinatorReserveOutcome::Reserved(reservation) => *reservation,
                    CaptureCoordinatorReserveOutcome::AlreadyApplied => {
                        return Ok(Cov2RuntimeDelivery::AlreadyApplied(checkpoint_id));
                    }
                    CaptureCoordinatorReserveOutcome::ConflictUnchanged { conflict } => {
                        bail!("runtime test catalog fence: {conflict:?}")
                    }
                };
            let request = reservation.effective_request().clone();
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30))
                .expect("bounded runtime test deadline");
            let lease = ExportJobLeaseStore::new(&self.conn);
            let runner = LiveExportRunner::admit(
                &lease,
                LiveExportTarget {
                    agent_kind: "opencode",
                    provider_session_id: VERIFIED_PARTIAL_PROVIDER,
                    scope: &self.scope,
                },
                deadline,
            )
            .await?
            .expect("own actual SQL runner");
            let mut report = json!({});
            let prepared = prepare_export_snapshot(
                &self.conn,
                &self.repo,
                &self.scope,
                "opencode",
                VERIFIED_PARTIAL_CAPTURE,
                &checkpoint_id,
                &request,
                deadline,
                current.is_some_and(|state| {
                    state.phase == crate::internal::ai::capture::state::CapturePhase::Stopped
                }),
                &mut report,
                cov2_runtime_normalizer,
                snapshot,
                runner.owner().to_owned(),
                runner.now_ms(),
            )
            .await;
            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(exit) => {
                    let replay = match &exit {
                        ExportStageExit::CoveredNoop {
                            covered_terminal_replay,
                        } => Some((false, *covered_terminal_replay)),
                        _ => None,
                    };
                    let (disposition, reason, log) =
                        export_stage_lease_disposition(&exit, VERIFIED_PARTIAL_CAPTURE);
                    runner.settle(disposition, reason, log).await?;
                    if let Some((legacy, terminal_replay)) = replay {
                        if terminal_replay {
                            settle_preapplied_covered_terminal_replay(reservation, &request)
                                .await?;
                        } else {
                            settle_preapplied_without_checkpoint(
                                &self.conn,
                                reservation,
                                &request,
                                None,
                                None,
                            )
                            .await?;
                        }
                        return Ok(if legacy {
                            Cov2RuntimeDelivery::LegacyReplay(checkpoint_id)
                        } else {
                            Cov2RuntimeDelivery::Covered(checkpoint_id)
                        });
                    }
                    return Err(match exit {
                        ExportStageExit::SourceRejected(error)
                        | ExportStageExit::CoverageFailed(error)
                        | ExportStageExit::CoveredReplayFailed(error) => error,
                        ExportStageExit::BridgeUnavailable => {
                            anyhow!("unexpected exporter-only outcome")
                        }
                        ExportStageExit::InflightOnly => {
                            anyhow!("unexpected concurrent coverage owner")
                        }
                        ExportStageExit::CoveredNoop { .. } => {
                            unreachable!("replay already consumed")
                        }
                    });
                }
            };
            let (source, live_reservation) = match prepared {
                PreparedExportSource::DurableActionReplay { keep_export_dirty } => {
                    runner
                        .settle(
                            if keep_export_dirty {
                                LeaseDisposition::Dirty
                            } else {
                                LeaseDisposition::AdvanceAndRelease
                            },
                            "settle_durable_action_export_job_failed",
                            LeaseLogContext::Session {
                                session_id: VERIFIED_PARTIAL_CAPTURE,
                                message: "failed to settle original action export job",
                            },
                        )
                        .await?;
                    if request.mutation().is_terminal() {
                        match reservation
                            .complete_preapplied_durable_replay(&request)
                            .await?
                        {
                            CaptureCoordinatorOutcome::CheckpointAlreadyExists { .. }
                            | CaptureCoordinatorOutcome::AlreadyApplied => {}
                            outcome => {
                                bail!("durable terminal receipt remains pending: {outcome:?}")
                            }
                        }
                    } else {
                        settle_preapplied_without_checkpoint(
                            &self.conn,
                            reservation,
                            &request,
                            None,
                            None,
                        )
                        .await?;
                    }
                    return Ok(if keep_export_dirty {
                        Cov2RuntimeDelivery::LegacyReplay(checkpoint_id)
                    } else {
                        Cov2RuntimeDelivery::Covered(checkpoint_id)
                    });
                }

                PreparedExportSource::Captured(source) => (
                    CheckpointStageSource::Captured {
                        transcript_is_authorized: true,
                        projection: Some(source.projection),
                        transcript: source.transcript,
                        reasoning: source.reasoning,
                    },
                    Some(source.reservation),
                ),
                PreparedExportSource::VerifiedPartial(source) => {
                    (CheckpointStageSource::VerifiedPartial(source), None)
                }
            };
            let envelope = SessionHookEnvelope {
                hook_event_name: "pure-cov2-runtime".to_owned(),
                session_id: VERIFIED_PARTIAL_PROVIDER.to_owned(),
                cwd: self.repo.to_string_lossy().into_owned(),
                transcript_path: None,
                extra: serde_json::Map::new(),
            };
            let event = LifecycleEvent {
                kind,
                session_id: VERIFIED_PARTIAL_PROVIDER.to_owned(),
                session_ref: None,
                prompt: None,
                model: None,
                source: None,
                tool_name: None,
                tool_input: None,
                tool_response: None,
                assistant_message: None,
                timestamp: Utc::now(),
            };
            if interrupt_receipt {
                use crate::internal::ai::capture::checkpoint::test_support::{
                    CheckpointFaultPoint, fail_once_at,
                };
                fail_once_at(CheckpointFaultPoint::MarkerCleanup);
            }
            let helper = std::env::current_exe()
                .expect("library test executable")
                .parent()
                .and_then(std::path::Path::parent)
                .expect("Cargo debug directory")
                .join("libra");
            assert!(
                helper.is_file(),
                "build the real Libra private-helper executable before this library test"
            );
            crate::internal::ai::authorized_read::with_test_helper_program(
                helper,
                run_checkpoint_stage(
                    CheckpointStageInput {
                        conn: &self.conn,
                        repo_path: &self.storage,
                        catalog_request: request.clone(),
                        diagnostic_request: request,
                        catalog_reservation: reservation,
                        libra_session_id: VERIFIED_PARTIAL_CAPTURE,
                        envelope: &envelope,
                        agent_kind: "opencode",
                        event: &event,
                        ingress_event_id: event_id,
                        identity_scheme: LifecycleIdentityScheme::FallbackActionHmacV1,
                        checkpoint_id: &checkpoint_id,
                        capture_deadline: Some(deadline),
                        terminal_settlement_deadline: Some(deadline),
                        capture_finalizer_deadline_millis: Some(deadline.sqlite_not_after_millis()),
                        now: 1,
                        scope: &self.scope,
                        subagent_sources: Vec::new(),
                        subagent_discovery_warning: None,
                        subagent_capture: None,
                        incomplete_parent_extraction: false,
                        report_value: report,
                        source,
                        claim_channel: "export",
                        live_reservation,
                    },
                    Some(runner),
                ),
            )
            .await?;
            Ok(Cov2RuntimeDelivery::Written(checkpoint_id))
        }

        async fn cov2_receipt(&self, action: u128) -> Option<String> {
            let row = self
                .conn
                .query_one_raw(Statement::from_sql_and_values(
                    self.conn.get_database_backend(),
                    "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                    [VERIFIED_PARTIAL_CAPTURE.into()],
                ))
                .await
                .expect("receipt metadata read")?;
            let metadata: String = row.try_get_by("metadata_json").expect("receipt metadata");
            let value: serde_json::Value =
                serde_json::from_str(&metadata).expect("catalog metadata JSON");
            value["capture_catalog_receipts_v1"]["entries"]
                .as_array()
                .expect("bounded real receipt ledger")
                .iter()
                .find(|entry| entry["event_id"] == uuid::Uuid::from_u128(action).to_string())
                .map(|entry| entry["status"].as_str().expect("receipt status").to_owned())
        }

        async fn cov2_claim_rows(&self) -> Vec<serde_json::Value> {
            self.conn.query_all_raw(Statement::from_string(self.conn.get_database_backend(),"SELECT coverage_schema_version,logical_turn_key,coverage_digest,completeness,revision,checkpoint_id,fence_token,state,owner FROM agent_coverage_claim ORDER BY coverage_schema_version,logical_turn_key".to_owned())).await.expect("actual coverage rows").into_iter().map(|row|json!({
                "version":row.try_get_by::<i64,_>("coverage_schema_version").unwrap(),"key":row.try_get_by::<String,_>("logical_turn_key").unwrap(),"digest":row.try_get_by::<String,_>("coverage_digest").unwrap(),"completeness":row.try_get_by::<String,_>("completeness").unwrap(),"revision":row.try_get_by::<i64,_>("revision").unwrap(),"checkpoint":row.try_get_by::<String,_>("checkpoint_id").unwrap(),"fence":row.try_get_by::<i64,_>("fence_token").unwrap(),"state":row.try_get_by::<String,_>("state").unwrap(),"owner":row.try_get_by::<Option<String>,_>("owner").unwrap(),
            })).collect()
        }

        async fn cov2_job(&self) -> (String, i64) {
            let row=self.conn.query_one_raw(Statement::from_sql_and_values(self.conn.get_database_backend(),"SELECT state,processed_generation,owner FROM agent_export_job WHERE agent_kind='opencode' AND provider_session_id=?",[VERIFIED_PARTIAL_PROVIDER.into()])).await.expect("real lease read").expect("admitted lease row");
            assert!(
                row.try_get_by::<Option<String>, _>("owner")
                    .unwrap()
                    .is_none()
            );
            (
                row.try_get_by("state").unwrap(),
                row.try_get_by("processed_generation").unwrap(),
            )
        }

        async fn cov2_covered(&self, bytes: Vec<u8>) -> Vec<CommittedTurnClaimSnapshot> {
            let mut snapshot = self.cov2_runtime_snapshot(bytes);
            let turns = snapshot
                .take_coverage_runtime()
                .turns
                .expect("authorized redacted typed turns");
            let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap();
            let reader = ReasoningIdentityReadContext::new(
                &self.scope,
                VERIFIED_PARTIAL_CAPTURE,
                &self.storage,
                &self.repo,
                deadline.monotonic(),
            );
            let outcome =
                crate::internal::ai::coverage_gate::reserve_runtime_turn_claims_with_reasoning(
                    &self.conn,
                    &self.scope,
                    VERIFIED_PARTIAL_CAPTURE,
                    &turns,
                    "coverage-replay-read",
                    Utc::now().timestamp_millis(),
                    CaptureReservationExecution::export(deadline),
                    ReasoningCoverageInput::ordinary(&reader),
                    false,
                )
                .await
                .expect("actual committed-row observations");
            assert!(outcome.versioned_reserved.is_empty());
            assert!(outcome.ordinary.reserved.is_empty());
            outcome.covered
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_direct_visible_turn_claim_version() {
        use crate::internal::ai::observed_agents::coverage::encode_coverage_turn;
        let fixture = VerifiedPartialStoreFixture::new().await;
        let bytes = fixture.cov2_visible_source("u", false);
        let mut snapshot = fixture.cov2_runtime_snapshot(bytes.clone());
        let turns = snapshot
            .take_coverage_runtime()
            .turns
            .expect("production typed companion");
        assert_eq!(turns.len(), 1);
        let encoded = encode_coverage_turn(&turns[0]).unwrap();
        assert_eq!(encoded.schema_version.as_i64(), 2);
        assert_eq!(encoded.canonical_bytes, COV2_RUNTIME_CANONICAL.as_bytes());
        assert_eq!(encoded.digest_hex, COV2_RUNTIME_SHA);
        let Cov2RuntimeDelivery::Written(checkpoint) = fixture
            .cov2_deliver(
                0xc201,
                false,
                fixture.cov2_runtime_snapshot(bytes),
                false,
                false,
            )
            .await
            .expect("actual runtime checkpoint")
        else {
            panic!("first typed delivery must write")
        };
        let rows = fixture.cov2_claim_rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["version"], 2);
        assert_eq!(rows[0]["digest"], COV2_RUNTIME_SHA);
        assert_eq!(rows[0]["completeness"], "complete");
        assert_eq!(rows[0]["revision"], 1);
        assert_eq!(rows[0]["checkpoint"], checkpoint);
        assert!(rows[0]["fence"].as_i64().unwrap() > 0);
        assert_eq!(rows[0]["state"], "catalog_committed");
        assert!(rows[0]["owner"].is_null());
        assert_eq!(fixture.scalar("SELECT COUNT(*) AS n FROM agent_coverage_revision WHERE coverage_schema_version=2 AND revision=1 AND completeness='complete'").await,1);
        assert_eq!(
            fixture.cov2_receipt(0xc201).await.as_deref(),
            Some("complete")
        );
        let (stored, metadata) = fixture.read_back(&checkpoint).await;
        assert!(!String::from_utf8_lossy(&stored).contains("sk-ant-EXAMPLE"));
        assert_eq!(metadata["transcript_snapshot"]["completeness"], "complete");
        assert_eq!(
            fixture
                .scalar(
                    "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope='agent_reasoning_identity'"
                )
                .await,
            0
        );

        // Ordinary text remains in the frozen v1 canonical domain.
        let ordinary = VerifiedPartialStoreFixture::new().await;
        let bytes=serde_json::to_vec(&json!({"info":{"id":VERIFIED_PARTIAL_PROVIDER},"messages":[{"id":"u","type":"user","text":"hi"},{"id":"a","type":"assistant","content":[{"id":"t","type":"text","text":"hello"}]},{"id":"i","type":"idle","outcome":"succeeded"}]})).unwrap();
        const ORDINARY_CANONICAL: &str =
            r#"[{"role":"user","text":"hi"},{"role":"assistant","text":"hello"}]"#;
        const ORDINARY_SHA: &str =
            "df991cd9a1ac5c12c32b8cdf0254c3dfbbf26485b505a5afc83a90d1128ebc54";
        let ordinary_turns =
            crate::internal::ai::observed_agents::coverage::normalize_opencode_export(&bytes);
        let ordinary_encoded = encode_coverage_turn(&ordinary_turns[0]).unwrap();
        assert_eq!(ordinary_encoded.schema_version.as_i64(), 1);
        assert_eq!(
            ordinary_encoded.canonical_bytes,
            ORDINARY_CANONICAL.as_bytes()
        );
        assert_eq!(ordinary_encoded.digest_hex, ORDINARY_SHA);
        ordinary
            .cov2_deliver(
                0xc202,
                false,
                ordinary.cov2_runtime_snapshot(bytes),
                false,
                false,
            )
            .await
            .expect("ordinary runtime write");
        let rows = ordinary.cov2_claim_rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["version"], 1);
        assert_eq!(rows[0]["digest"], ORDINARY_SHA);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_direct_candidate_turn_stays_partial() {
        let fixture = VerifiedPartialStoreFixture::new().await;
        let Cov2RuntimeDelivery::Written(checkpoint) = fixture
            .cov2_deliver(0xc251, false, fixture.snapshot(), false, false)
            .await
            .expect("owning partial still writes explicit partial")
        else {
            panic!("partial cannot take covered shortcut")
        };
        let (_, metadata) = fixture.read_back(&checkpoint).await;
        assert_eq!(metadata["transcript_snapshot"]["completeness"], "partial");
        fixture.assert_no_native_or_partial_payload().await;
        assert!(matches!(
            fixture
                .cov2_deliver(0xc252, false, fixture.snapshot(), false, false)
                .await
                .unwrap(),
            Cov2RuntimeDelivery::Written(_)
        ));
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            2,
            "incomplete turns never become covered ACK authority"
        );
        fixture.assert_no_native_or_partial_payload().await;

        let candidate = VerifiedPartialStoreFixture::new().await;
        let source=serde_json::to_vec(&json!({"info":{"id":VERIFIED_PARTIAL_PROVIDER},"messages":[{"id":"u","type":"user","text":"hi"},{"id":"a","type":"assistant","content":[{"id":"npart_0123456789abcdef0123456789abcdef","type":"reasoning","text":"unverified private text","state":{"signature":"unverified signature"},"originProof":{"nonce":"unverified receipt"}}]}]})).unwrap();
        let snapshot = candidate.cov2_runtime_snapshot(source);
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Partial
        );
        assert!(
            candidate
                .cov2_deliver(0xc253, false, snapshot, false, false)
                .await
                .is_err()
        );
        assert_eq!(
            candidate
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            0
        );
        assert_eq!(
            candidate.cov2_receipt(0xc253).await.as_deref(),
            Some("pending")
        );
        candidate.assert_no_native_or_partial_payload().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_structure_redaction_rejects_typed_and_keeps_legacy_receipt() {
        use crate::internal::ai::observed_agents::Redactor;

        const EMPTY_V1_SHA: &str =
            "4f53cda18c2baa0c0354bb5f9a3ecbe5ed12ab4d8e11ba873c2f11161202b945";
        let fixture = VerifiedPartialStoreFixture::new().await;
        let mut source: serde_json::Value =
            serde_json::from_slice(&fixture.cov2_visible_source("u", false)).unwrap();
        source["messages"][0]["text"] = "postgres://user:pass@host/db".into();
        let bytes = serde_json::to_vec(&source).unwrap();
        let (legacy, report) = Redactor::new_default().redact(&bytes);
        assert!(serde_json::from_slice::<serde_json::Value>(legacy.bytes()).is_err());
        let mut probe = fixture.cov2_runtime_snapshot(bytes.clone());
        assert_eq!(probe.completeness(), CaptureSnapshotCompleteness::Complete);
        assert!(!probe.rejects_capture_source_consumer());
        assert_eq!(probe.transcript().unwrap(), &legacy);
        assert_eq!(probe.redaction_report(), &report);
        let runtime = probe.take_coverage_runtime();
        assert!(runtime.turns.is_none());
        assert!(runtime.classifier_rejected);
        assert!(!runtime.source_partial);
        assert!(
            fixture
                .cov2_deliver(
                    0xc351,
                    true,
                    fixture.cov2_runtime_snapshot(bytes.clone()),
                    false,
                    true
                )
                .await
                .is_err()
        );
        let rows = fixture.cov2_claim_rows().await;
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["version"], 1);
        assert_eq!(rows[0]["digest"], EMPTY_V1_SHA);
        assert_eq!(rows[0]["completeness"], "incomplete");
        assert_eq!(
            fixture.cov2_receipt(0xc351).await.as_deref(),
            Some("pending")
        );
        assert!(matches!(
            fixture
                .cov2_deliver(
                    0xc351,
                    true,
                    fixture.cov2_runtime_snapshot(bytes),
                    false,
                    false
                )
                .await
                .unwrap(),
            Cov2RuntimeDelivery::LegacyReplay(_)
        ));
        assert_eq!(fixture.cov2_claim_rows().await, rows);
        assert_eq!(
            fixture.cov2_receipt(0xc351).await.as_deref(),
            Some("complete")
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );

        let history = VerifiedPartialStoreFixture::new().await;
        history
            .cov2_deliver(
                0xc352,
                false,
                history.cov2_runtime_snapshot(history.cov2_visible_source("prior-v2-key", false)),
                false,
                false,
            )
            .await
            .unwrap();
        let before = history.cov2_claim_rows().await;
        let mut source: serde_json::Value =
            serde_json::from_slice(&history.cov2_visible_source("unrelated-new-key", true))
                .unwrap();
        source["messages"][0]["text"] = "postgres://user:pass@host/db".into();
        assert!(
            history
                .cov2_deliver(
                    0xc353,
                    false,
                    history.cov2_runtime_snapshot(serde_json::to_vec(&source).unwrap()),
                    false,
                    false
                )
                .await
                .is_err()
        );
        assert_eq!(history.cov2_claim_rows().await, before);
        assert_eq!(
            history.cov2_receipt(0xc353).await.as_deref(),
            Some("pending")
        );
        assert_eq!(
            history
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        assert_eq!(history.cov2_job().await.0, "dirty");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cov2_ordinary_partial_fragment_keeps_v1_and_terminal_noop() {
        const V1_SHA: &str = "df991cd9a1ac5c12c32b8cdf0254c3dfbbf26485b505a5afc83a90d1128ebc54";
        for text in ["", " \t"] {
            let fixture = VerifiedPartialStoreFixture::new().await;
            let mut source: serde_json::Value =
                serde_json::from_slice(&fixture.cov2_visible_source("u", false)).unwrap();
            source["messages"][1]["content"][0]["text"] = text.into();
            let bytes = serde_json::to_vec(&source).unwrap();
            let mut probe = fixture.cov2_runtime_snapshot(bytes.clone());
            assert_eq!(
                probe.completeness(),
                crate::internal::ai::capture::snapshot::CaptureSnapshotCompleteness::Complete
            );
            assert!(!probe.rejects_capture_source_consumer());
            assert!(!probe.has_verified_ordinary_partial_permit());
            let runtime = probe.take_coverage_runtime();
            assert!(runtime.turns.is_none());
            assert!(runtime.classifier_rejected);
            assert!(!runtime.source_partial);
            assert_eq!(probe.transcript().unwrap().bytes(), bytes);
            assert!(matches!(
                fixture
                    .cov2_deliver(
                        0xc301,
                        false,
                        fixture.cov2_runtime_snapshot(bytes.clone()),
                        false,
                        false,
                    )
                    .await
                    .unwrap(),
                Cov2RuntimeDelivery::Written(_)
            ));
            let rows = fixture.cov2_claim_rows().await;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["version"], 1);
            assert_eq!(rows[0]["digest"], V1_SHA);
            assert_eq!(rows[0]["completeness"], "incomplete");
            assert!(matches!(
                fixture
                    .cov2_deliver(
                        0xc302,
                        true,
                        fixture.cov2_runtime_snapshot(bytes),
                        false,
                        false,
                    )
                    .await
                    .expect("a supported incomplete V1 terminal capture must settle"),
                Cov2RuntimeDelivery::Written(_)
            ));
            // The legacy ordinary normalizer deliberately keeps this turn
            // incomplete; a new terminal action writes its next revision.
            let terminal_rows = fixture.cov2_claim_rows().await;
            assert_eq!(terminal_rows.len(), 1);
            assert_eq!(terminal_rows[0]["version"], 1);
            assert_eq!(terminal_rows[0]["digest"], V1_SHA);
            assert_eq!(terminal_rows[0]["completeness"], "incomplete");
            assert_eq!(
                fixture.cov2_receipt(0xc302).await.as_deref(),
                Some("complete")
            );
            assert_eq!(
                fixture
                    .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                    .await,
                2
            );
        }

        // The legacy normalizer deliberately retains an incomplete turn
        // even when reasoning is empty. Interrupt a real terminal V1 write
        // after its CAS, then settle that same durable receipt read-only.
        for text in ["", " \t"] {
            let fixture = VerifiedPartialStoreFixture::new().await;
            let mut source: serde_json::Value =
                serde_json::from_slice(&fixture.cov2_visible_source("u", false)).unwrap();
            source["messages"][1]["content"][0]["text"] = text.into();
            let bytes = serde_json::to_vec(&source).unwrap();
            assert!(
                fixture
                    .cov2_deliver(
                        0xc303,
                        true,
                        fixture.cov2_runtime_snapshot(bytes.clone()),
                        false,
                        true
                    )
                    .await
                    .is_err()
            );
            let rows = fixture.cov2_claim_rows().await;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["version"], 1);
            assert_eq!(rows[0]["digest"], V1_SHA);
            assert_eq!(rows[0]["completeness"], "incomplete");
            assert_eq!(
                fixture.cov2_receipt(0xc303).await.as_deref(),
                Some("pending")
            );
            assert!(matches!(
                fixture
                    .cov2_deliver(
                        0xc303,
                        true,
                        fixture.cov2_runtime_snapshot(bytes.clone()),
                        false,
                        false
                    )
                    .await
                    .expect("a committed V1 terminal receipt must settle read-only"),
                Cov2RuntimeDelivery::LegacyReplay(_)
            ));
            assert_eq!(fixture.cov2_claim_rows().await, rows);
            assert_eq!(
                fixture.cov2_receipt(0xc303).await.as_deref(),
                Some("complete")
            );
            assert_eq!(
                fixture
                    .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                    .await,
                1
            );
            assert!(matches!(
                fixture
                    .cov2_deliver(
                        0xc303,
                        true,
                        fixture.cov2_runtime_snapshot(bytes),
                        false,
                        false
                    )
                    .await
                    .unwrap(),
                Cov2RuntimeDelivery::AlreadyApplied(_)
            ));
        }

        let history = VerifiedPartialStoreFixture::new().await;
        history
            .cov2_deliver(
                0xc311,
                false,
                history.cov2_runtime_snapshot(history.cov2_visible_source("u", false)),
                false,
                false,
            )
            .await
            .unwrap();
        let rows = history.cov2_claim_rows().await;
        let mut rejected: serde_json::Value =
            serde_json::from_slice(&history.cov2_visible_source("u", true)).unwrap();
        rejected["messages"][1]["content"][0]["text"] = "".into();
        assert!(
            history
                .cov2_deliver(
                    0xc312,
                    false,
                    history.cov2_runtime_snapshot(serde_json::to_vec(&rejected).unwrap()),
                    false,
                    false,
                )
                .await
                .is_err()
        );
        assert_eq!(history.cov2_claim_rows().await, rows);
        assert_eq!(
            history
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        assert_eq!(
            history.cov2_receipt(0xc312).await.as_deref(),
            Some("pending")
        );
        assert_eq!(history.cov2_job().await.0, "dirty");
    }

    #[cfg(unix)]
    pub(crate) async fn assert_cov2_runtime_claim_replay_version() {
        // Completeness belongs to the immutable committed turn. A pending
        // receipt may settle its exact incomplete v2 set without upgrading it.
        for terminal in [false, true] {
            let incomplete = VerifiedPartialStoreFixture::new().await;
            let mut source: serde_json::Value =
                serde_json::from_slice(&incomplete.cov2_visible_source("incomplete-u", false))
                    .unwrap();
            source["messages"].as_array_mut().unwrap().pop();
            let bytes = serde_json::to_vec(&source).unwrap();
            let action = if terminal { 0xc3f1 } else { 0xc3f0 };
            assert!(
                incomplete
                    .cov2_deliver(
                        action,
                        terminal,
                        incomplete.cov2_runtime_snapshot(bytes.clone()),
                        false,
                        true,
                    )
                    .await
                    .is_err()
            );
            let rows = incomplete.cov2_claim_rows().await;
            assert_eq!(rows.len(), 1);
            assert_eq!(rows[0]["version"], 2);
            assert_eq!(rows[0]["completeness"], "incomplete");
            assert_eq!(
                incomplete.cov2_receipt(action).await.as_deref(),
                Some("pending")
            );
            assert!(matches!(
                incomplete
                    .cov2_deliver(
                        action,
                        terminal,
                        incomplete.cov2_runtime_snapshot(bytes.clone()),
                        false,
                        false,
                    )
                    .await
                    .expect("the exact committed incomplete V2 set can settle its receipt"),
                Cov2RuntimeDelivery::Covered(_)
            ));
            assert_eq!(incomplete.cov2_claim_rows().await, rows);
            assert_eq!(
                incomplete.cov2_receipt(action).await.as_deref(),
                Some("complete")
            );
            assert_eq!(
                incomplete
                    .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                    .await,
                1
            );
            assert!(matches!(
                incomplete
                    .cov2_deliver(
                        action,
                        terminal,
                        incomplete.cov2_runtime_snapshot(bytes),
                        false,
                        false,
                    )
                    .await
                    .unwrap(),
                Cov2RuntimeDelivery::AlreadyApplied(_)
            ));
            assert_eq!(incomplete.cov2_claim_rows().await, rows);
        }
        // Export SessionEnd retains an actual catalog receipt after a
        // post-CAS interruption; it must recover without an ordinary pending
        // artifact or a second checkpoint/reservation.
        let terminal = VerifiedPartialStoreFixture::new().await;
        let terminal_bytes = terminal.cov2_visible_source("terminal-u", false);
        assert!(
            terminal
                .cov2_deliver(
                    0xc400,
                    true,
                    terminal.cov2_runtime_snapshot(terminal_bytes.clone()),
                    false,
                    true
                )
                .await
                .is_err()
        );
        assert_eq!(
            terminal.cov2_receipt(0xc400).await.as_deref(),
            Some("pending")
        );
        assert_eq!(terminal.cov2_job().await, ("dirty".to_owned(), 0));
        let terminal_rows = terminal.cov2_claim_rows().await;
        assert_eq!(terminal_rows.len(), 1);
        assert_eq!(terminal_rows[0]["version"], 2);
        let replay = terminal
            .cov2_deliver(
                0xc400,
                true,
                terminal.cov2_runtime_snapshot(terminal_bytes.clone()),
                false,
                false,
            )
            .await
            .expect("actual terminal receipt replay");
        assert!(matches!(replay, Cov2RuntimeDelivery::Covered(_)));
        assert_eq!(
            terminal.cov2_receipt(0xc400).await.as_deref(),
            Some("complete")
        );
        assert_eq!(terminal.cov2_claim_rows().await, terminal_rows);
        assert_eq!(
            terminal
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        assert_eq!(terminal.scalar("SELECT COUNT(*) AS n FROM metadata_kv WHERE scope='agent_capture_pending_chunk'").await, 0);
        let terminal_job = terminal.cov2_job().await;
        assert_eq!(terminal_job.0, "idle");
        assert!(matches!(
            terminal
                .cov2_deliver(
                    0xc400,
                    true,
                    terminal.cov2_runtime_snapshot(terminal_bytes),
                    false,
                    false
                )
                .await
                .unwrap(),
            Cov2RuntimeDelivery::AlreadyApplied(_)
        ));
        assert_eq!(terminal.cov2_job().await, terminal_job);

        let fixture = VerifiedPartialStoreFixture::new().await;
        let bytes = fixture.cov2_visible_source("u", false);
        assert!(
            fixture
                .cov2_deliver(
                    0xc401,
                    false,
                    fixture.cov2_runtime_snapshot(bytes.clone()),
                    false,
                    true
                )
                .await
                .is_err()
        );
        assert_eq!(
            fixture.cov2_receipt(0xc401).await.as_deref(),
            Some("pending")
        );
        assert_eq!(fixture.cov2_job().await, ("dirty".to_owned(), 0));
        let committed = fixture.cov2_claim_rows().await;
        assert_eq!(committed.len(), 1);
        assert_eq!(committed[0]["version"], 2);
        assert_eq!(committed[0]["digest"], COV2_RUNTIME_SHA);
        assert!(matches!(
            fixture
                .cov2_deliver(
                    0xc401,
                    false,
                    fixture.cov2_runtime_snapshot(bytes.clone()),
                    false,
                    false
                )
                .await
                .unwrap(),
            Cov2RuntimeDelivery::Covered(_)
        ));
        assert_eq!(
            fixture.cov2_receipt(0xc401).await.as_deref(),
            Some("complete")
        );
        assert_eq!(fixture.cov2_claim_rows().await, committed);
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        let job = fixture.cov2_job().await;
        assert_eq!(job.0, "idle");
        assert!(job.1 > 0);
        assert!(matches!(
            fixture
                .cov2_deliver(
                    0xc401,
                    false,
                    fixture.cov2_runtime_snapshot(bytes.clone()),
                    false,
                    false
                )
                .await
                .unwrap(),
            Cov2RuntimeDelivery::AlreadyApplied(_)
        ));
        assert_eq!(
            fixture.cov2_job().await,
            job,
            "one receipt ACK advances once"
        );

        // A retained over-limit source after v2 blocks the entire new action,
        // including an unrelated ordinary key, before any claim reservation.
        let parts = (0..=4096)
            .map(|n| json!({"id":format!("r{n}"),"type":"reasoning","text":"visible"}))
            .collect::<Vec<_>>();
        let rejected=serde_json::to_vec(&json!({"info":{"id":VERIFIED_PARTIAL_PROVIDER},"messages":[{"id":"new-unrelated-key","type":"user","text":"ordinary new"},{"id":"a-over-limit","type":"assistant","content":parts}]})).unwrap();
        assert!(
            fixture
                .cov2_deliver(
                    0xc402,
                    false,
                    fixture.cov2_runtime_snapshot(rejected),
                    false,
                    false
                )
                .await
                .is_err()
        );
        assert_eq!(fixture.cov2_claim_rows().await, committed);
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        assert_eq!(
            fixture.cov2_receipt(0xc402).await.as_deref(),
            Some("pending")
        );
        assert_eq!(fixture.cov2_job().await.0, "dirty");

        let covered = fixture.cov2_covered(bytes.clone()).await;
        assert_eq!(covered.len(), 1);
        let checkpoint = covered[0].checkpoint_id().to_owned();
        assert!(
            TracesCheckpointStore::settle_committed_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                &checkpoint,
                VERIFIED_PARTIAL_CAPTURE,
                None
            )
            .await
            .is_err(),
            "original None expectation must still reject actual v2 revisions"
        );
        assert!(
            TracesCheckpointStore::settle_runtime_covered_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                &checkpoint,
                VERIFIED_PARTIAL_CAPTURE,
                None,
                &[]
            )
            .await
            .is_err(),
            "an empty expectation cannot authorize v2"
        );
        let other = fixture.cov2_visible_source("foreign-key", false);
        fixture
            .cov2_deliver(
                0xc403,
                false,
                fixture.cov2_runtime_snapshot(other.clone()),
                false,
                false,
            )
            .await
            .unwrap();
        let foreign = fixture.cov2_covered(other).await;
        assert!(
            TracesCheckpointStore::settle_runtime_covered_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                &checkpoint,
                VERIFIED_PARTIAL_CAPTURE,
                None,
                &foreign
            )
            .await
            .is_err(),
            "foreign checkpoint observations cannot authorize this v2 checkpoint"
        );
        fixture.conn.execute_raw(Statement::from_sql_and_values(fixture.conn.get_database_backend(),"UPDATE agent_coverage_claim SET revision=revision+1 WHERE checkpoint_id=? AND coverage_schema_version=2",[checkpoint.clone().into()])).await.unwrap();
        assert!(
            TracesCheckpointStore::settle_runtime_covered_replay_without_write(
                &fixture.conn,
                &fixture.scope,
                &checkpoint,
                VERIFIED_PARTIAL_CAPTURE,
                None,
                &covered
            )
            .await
            .is_err(),
            "superseded current rows invalidate old observed fences"
        );
        assert_eq!(
            fixture.cov2_receipt(0xc402).await.as_deref(),
            Some("pending")
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            2
        );

        // Pre-upgrade C1 is immutable: settle its interrupted receipt before
        // selecting v2, then let the next action capture typed reasoning once.
        let old = VerifiedPartialStoreFixture::new().await;
        let source = old.cov2_visible_source("u", false);
        assert!(
            old.cov2_deliver(0xc411, false, old.cov2_runtime_snapshot(source), true, true)
                .await
                .is_err()
        );
        let v1 = old.cov2_claim_rows().await;
        assert_eq!(v1.len(), 1);
        assert_eq!(v1[0]["version"], 1);
        let fresh = old.cov2_visible_source("u", true);
        assert!(matches!(
            old.cov2_deliver(
                0xc411,
                false,
                old.cov2_runtime_snapshot(fresh.clone()),
                false,
                false
            )
            .await
            .unwrap(),
            Cov2RuntimeDelivery::LegacyReplay(_)
        ));
        assert_eq!(
            old.cov2_claim_rows().await,
            v1,
            "old action makes zero v1/v2 reservations, including fresh ordinary keys"
        );
        assert_eq!(old.cov2_receipt(0xc411).await.as_deref(), Some("complete"));
        assert_eq!(old.cov2_job().await, ("dirty".to_owned(), 0));
        assert_eq!(
            old.scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        assert!(matches!(
            old.cov2_deliver(
                0xc412,
                false,
                old.cov2_runtime_snapshot(fresh.clone()),
                false,
                false
            )
            .await
            .unwrap(),
            Cov2RuntimeDelivery::Written(_)
        ));
        assert_eq!(
            old.scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2"
            )
            .await,
            1
        );
        assert_eq!(
            old.scalar(
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=1"
            )
            .await,
            2
        );
        assert_eq!(
            old.scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            2
        );
        let rows = old.cov2_claim_rows().await;
        assert!(
            rows.contains(&v1[0]),
            "historical v1 row remains byte-for-byte semantically unchanged"
        );
        assert!(matches!(
            old.cov2_deliver(
                0xc412,
                false,
                old.cov2_runtime_snapshot(fresh.clone()),
                false,
                false
            )
            .await
            .unwrap(),
            Cov2RuntimeDelivery::AlreadyApplied(_)
        ));
        assert!(matches!(
            old.cov2_deliver(
                0xc413,
                false,
                old.cov2_runtime_snapshot(fresh),
                false,
                false
            )
            .await
            .unwrap(),
            Cov2RuntimeDelivery::Covered(_)
        ));
        assert_eq!(old.cov2_claim_rows().await, rows);
        assert_eq!(
            old.scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            2
        );
        // The same-action legacy exception also precedes the session's v2
        // history barrier: a sibling action may already have created v2
        // history while this old, authenticated receipt was still pending.
        let mixed_history = VerifiedPartialStoreFixture::new().await;
        assert!(
            mixed_history
                .cov2_deliver(
                    0xc421,
                    false,
                    mixed_history
                        .cov2_runtime_snapshot(mixed_history.cov2_visible_source("u", false)),
                    true,
                    true
                )
                .await
                .is_err()
        );
        mixed_history
            .cov2_deliver(
                0xc422,
                false,
                mixed_history.cov2_runtime_snapshot(
                    mixed_history.cov2_visible_source("prior-v2-key", false),
                ),
                false,
                false,
            )
            .await
            .expect("real sibling action commits v2 history");
        let before = mixed_history.cov2_claim_rows().await;
        assert_eq!(before.len(), 2);
        assert_eq!(
            mixed_history
                .scalar(
                    "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE coverage_schema_version=2"
                )
                .await,
            1
        );
        assert!(matches!(
            mixed_history
                .cov2_deliver(
                    0xc421,
                    false,
                    mixed_history
                        .cov2_runtime_snapshot(mixed_history.cov2_visible_source("u", true)),
                    false,
                    false
                )
                .await
                .expect("old action read-only exception precedes v2 history"),
            Cov2RuntimeDelivery::LegacyReplay(_)
        ));
        assert_eq!(
            mixed_history.cov2_claim_rows().await,
            before,
            "legacy same-action replay makes no v1/v2 reservation even with fresh keys and v2 history"
        );
        assert_eq!(
            mixed_history
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            2
        );
        assert_eq!(
            mixed_history.cov2_receipt(0xc421).await.as_deref(),
            Some("complete")
        );
        let job = mixed_history.cov2_job().await;
        assert_eq!(
            job.0, "dirty",
            "uncaptured fresh keys stay available to a new action"
        );
        assert!(matches!(
            mixed_history
                .cov2_deliver(
                    0xc421,
                    false,
                    mixed_history
                        .cov2_runtime_snapshot(mixed_history.cov2_visible_source("u", true)),
                    false,
                    false
                )
                .await
                .unwrap(),
            Cov2RuntimeDelivery::AlreadyApplied(_)
        ));
        assert_eq!(
            mixed_history.cov2_job().await,
            job,
            "one ACK, no replay loop"
        );
        assert_eq!(mixed_history.cov2_claim_rows().await, before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn verified_partial_export_commits_nonzero_empty_claim_checkpoint() {
        let fixture = VerifiedPartialStoreFixture::new().await;
        let snapshot = fixture.snapshot();
        assert_eq!(
            export_snapshot_route(&snapshot),
            ExportSnapshotRoute::VerifiedPartial
        );
        assert!(
            snapshot.try_into_ordinary().is_err(),
            "default consuming route remains closed"
        );
        let rejected = crate::internal::ai::capture::snapshot::carrier_test_snapshot(
            &fixture::UNMAPPED,
            fixture::CANARY.to_vec(),
        );
        assert_eq!(
            export_snapshot_route(&rejected),
            ExportSnapshotRoute::Rejected
        );
        let mut stale_scope = fixture.scope.clone();
        stale_scope.repo_id = "foreign-source-scope".to_owned();
        let rejected = match fixture
            .snapshot()
            .try_into_verified_ordinary_partial_in_scope_until(
                &fixture.conn,
                &stale_scope,
                &fixture.storage,
                &fixture.repo,
                Instant::now() + Duration::from_secs(30),
            )
            .await
        {
            Ok(_) => panic!("foreign scope cannot bind a partial source"),
            Err(rejected) => rejected,
        };
        assert_eq!(
            export_snapshot_route(&rejected),
            ExportSnapshotRoute::Rejected
        );
        assert!(
            rejected.transcript().is_none(),
            "binding failure erases content and permit"
        );
        let checkpoint = fixture
            .write(0x101, false, None)
            .await
            .expect("real partial store commit");
        let (bytes, metadata) = fixture.read_back(&checkpoint).await;
        assert!(!bytes.is_empty());
        let stored = String::from_utf8(bytes).expect("stored JSON is UTF-8");
        assert!(stored.contains(VERIFIED_PARTIAL_TEXT));
        assert!(stored.contains(VERIFIED_PARTIAL_CANARY));
        assert!(stored.contains("reasoningField"));
        assert!(stored.contains("PRIVATE_STATE_CANARY"));
        assert_eq!(metadata["transcript_snapshot"]["completeness"], "partial");
        assert_eq!(
            metadata["transcript_snapshot"]["partial_reason"],
            "source_authorization_mismatch"
        );
        assert!(
            metadata["transcript_snapshot"]["transcript_redacted_bytes"]
                .as_u64()
                .unwrap()
                > 0
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'")
                .await,
            1
        );
        fixture.assert_no_native_or_partial_payload().await;

        // A terminal action uses the same strict partial export carrier, but
        // must publish stopped only after the nonzero checkpoint/ref commit.
        let terminal = VerifiedPartialStoreFixture::new().await;
        let checkpoint = terminal
            .write(0x102, true, None)
            .await
            .expect("terminal partial commits before acknowledgement");
        let (bytes, metadata) = terminal.read_back(&checkpoint).await;
        assert!(!bytes.is_empty());
        let stored = String::from_utf8(bytes).expect("stored terminal JSON is UTF-8");
        assert!(stored.contains(VERIFIED_PARTIAL_TEXT));
        assert!(stored.contains(VERIFIED_PARTIAL_CANARY));
        assert!(stored.contains("reasoningField"));
        assert_eq!(metadata["transcript_snapshot"]["completeness"], "partial");
        let completed = terminal.current().await.expect("terminal catalog state");
        assert_eq!(
            completed.phase,
            crate::internal::ai::capture::state::CapturePhase::Stopped
        );
        assert!(completed.stopped_at.is_some());
        assert_eq!(
            terminal
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        assert_eq!(
            terminal
                .scalar("SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'")
                .await,
            1
        );
        assert_eq!(
            terminal
                .write(0x102, true, None)
                .await
                .expect("same terminal action replays durable checkpoint"),
            checkpoint
        );
        assert_eq!(
            terminal.current().await,
            Some(completed),
            "same-action terminal replay does not mutate the completed catalog"
        );
        assert_eq!(
            terminal
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        let scan = CaptureCatalogStore::new(terminal.conn.clone())
            .pending_finalizer_recoveries_for_doctor(Utc::now().timestamp_millis())
            .await
            .expect("read terminal finalizer receipts");
        assert!(
            scan.recoveries.is_empty(),
            "durable terminal has no pending finalizer"
        );
        assert_eq!(scan.malformed_rows, 0);
        terminal.assert_no_native_or_partial_payload().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn verified_partial_does_not_settle_as_all_skipped_or_complete_coverage() {
        let fixture = VerifiedPartialStoreFixture::new().await;
        let source = fixture.carrier().await;
        let turns = crate::internal::ai::observed_agents::coverage::normalize_opencode_export(
            source.transcript().bytes(),
        );
        assert!(!turns.is_empty(), "fixture has real logical ordinary turns");
        assert!(turns.iter().all(|turn| turn.completeness
            == crate::internal::ai::observed_agents::Completeness::Incomplete));
        drop(source);
        let first = fixture
            .write(0x201, false, None)
            .await
            .expect("all-incomplete source still writes");
        fixture
            .write(0x201, false, None)
            .await
            .expect("same action replays its durable checkpoint");
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        let second = fixture
            .write(0x202, false, None)
            .await
            .expect("a distinct idle remains a bounded direct write");
        assert_ne!(first, second);
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            2,
            "distinct idle actions may retain duplicate full transcripts; no cross-idle coverage claim is fabricated"
        );
        let (first_bytes, _) = fixture.read_back(&first).await;
        let (second_bytes, _) = fixture.read_back(&second).await;
        assert_eq!(
            first_bytes, second_bytes,
            "distinct idles retain bounded duplicate source bytes"
        );
        fixture.assert_no_native_or_partial_payload().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn verified_partial_replay_and_failure_remain_direct_only() {
        use crate::internal::ai::capture::checkpoint::test_support::{
            self as faults, CheckpointFaultPoint,
        };
        for fault in CheckpointFaultPoint::ALL {
            let fixture = VerifiedPartialStoreFixture::new().await;
            faults::fail_once_at(fault);
            let failure = fixture.write(0x301, false, None).await;
            assert!(failure.is_err(), "{fault:?}");
            let (job_state, processed) = fixture
                .settle_real_lease(CheckpointStageExit::Uncommitted(failure.map(|_| ())))
                .await;
            assert_eq!(
                job_state, "dirty",
                "failed partial store never cleans lease: {fault:?}"
            );
            assert_eq!(
                processed, 0,
                "failed partial store never advances processed: {fault:?}"
            );
            assert!(
                faults::armed_fault().is_none(),
                "fault must be reached: {fault:?}"
            );
            let count = fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await;
            let traces_refs = fixture
                .scalar("SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'")
                .await;
            if fault == CheckpointFaultPoint::MarkerCleanup {
                // This fault fires after the checkpoint/ref transaction.
                // Its retained cleanup marker must keep the failed export
                // dirty, while the single durable checkpoint remains intact.
                assert_eq!(count, 1, "post-CAS cleanup retains checkpoint: {fault:?}");
                assert_eq!(traces_refs, 1, "post-CAS cleanup retains ref: {fault:?}");
                let checkpoint_id =
                    crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                        uuid::Uuid::from_u128(0x301),
                        crate::internal::ai::capture::state::CheckpointWrite::Committed,
                    );
                let entry = crate::internal::metadata::MetadataKv::get_with_conn(
                    &fixture.conn,
                    crate::internal::metadata::MetadataScope::AgentTracesInflight,
                    VERIFIED_PARTIAL_CAPTURE,
                    &checkpoint_id,
                )
                .await
                .expect("read retained post-CAS marker")
                .expect("post-CAS cleanup failure retains marker");
                let marker =
                    crate::internal::ai::traces::decode_and_validate_traces_inflight_marker(
                        &entry.value,
                        VERIFIED_PARTIAL_CAPTURE,
                        &checkpoint_id,
                    )
                    .expect("decode retained post-CAS marker");
                assert!(
                    !marker.cleanup_pending && marker.commit.is_some(),
                    "post-CAS cleanup retains committed ordinary marker: {fault:?}"
                );
                assert_eq!(
                    fixture.scalar(
                        "SELECT COUNT(*) AS n FROM agent_checkpoint c JOIN reference r ON r.\"commit\" = c.traces_commit WHERE r.name = 'traces' AND r.kind = 'Branch' AND r.remote IS NULL"
                    ).await,
                    1,
                    "durable catalog and traces ref agree: {fault:?}"
                );
            } else {
                assert_eq!(count, 0, "precommit failure must not publish: {fault:?}");
                if fault == CheckpointFaultPoint::RefCasExhausted {
                    // The real fault seam commits a competing traces writer
                    // on every CAS attempt. Its ref is not this checkpoint.
                    assert_eq!(
                        traces_refs, 1,
                        "competing writer owns traces ref: {fault:?}"
                    );
                } else {
                    assert_eq!(
                        traces_refs, 0,
                        "pre-CAS fault leaves no traces ref: {fault:?}"
                    );
                }
            }
            fixture.assert_no_native_or_partial_payload().await;
        }
        let fixture = VerifiedPartialStoreFixture::new().await;
        assert!(
            fixture
                .write_with_tombstone(0x399, false, None, true)
                .await
                .is_err()
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            0
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'")
                .await,
            0
        );
        fixture.assert_no_native_or_partial_payload().await;
        let fixture = VerifiedPartialStoreFixture::new().await;
        let expired =
            CaptureCommitDeadline::from_test_pair(Instant::now() - Duration::from_secs(1), 0);
        assert!(fixture.write(0x401, false, Some(expired)).await.is_err());
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            0
        );
        fixture.assert_no_native_or_partial_payload().await;

        let terminal = VerifiedPartialStoreFixture::new().await;
        let terminal_failure = terminal
            .write(0x402, true, Some(expired))
            .await
            .expect_err("expired terminal must never acknowledge an unwritten partial checkpoint");
        assert!(
            terminal_failure
                .to_string()
                .contains("verified partial terminal capture remains content-free pending")
        );
        assert_eq!(
            terminal
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            0
        );
        assert_eq!(
            terminal
                .scalar("SELECT COUNT(*) AS n FROM reference WHERE name = 'traces'")
                .await,
            0
        );
        let current = terminal
            .current()
            .await
            .expect("pending terminal remains in catalog");
        assert_ne!(
            current.phase,
            crate::internal::ai::capture::state::CapturePhase::Stopped
        );
        assert!(
            current.stopped_at.is_none(),
            "deadline miss never publishes stopped_at"
        );
        let scan = CaptureCatalogStore::new(terminal.conn.clone())
            .pending_finalizer_recoveries_for_doctor(Utc::now().timestamp_millis())
            .await
            .expect("read content-free terminal recovery receipt");
        assert_eq!(scan.malformed_rows, 0);
        assert_eq!(
            scan.recoveries.len(),
            1,
            "deadline miss retains exactly one original receipt"
        );
        let pending = &scan.recoveries[0];
        assert_eq!(pending.scope(), &terminal.scope);
        assert!(
            !pending.artifact_present(),
            "partial bytes are never sealed for replay"
        );
        assert!(!pending.artifact_pending());
        assert!(!pending.quarantined());
        let (state, processed) = terminal
            .settle_real_lease(CheckpointStageExit::Uncommitted(Err(terminal_failure)))
            .await;
        assert_eq!(state, "dirty");
        assert_eq!(
            processed, 0,
            "expired terminal never advances the export generation"
        );
        let row = terminal
            .conn
            .query_one_raw(Statement::from_sql_and_values(
                terminal.conn.get_database_backend(),
                "SELECT metadata_json FROM agent_session WHERE session_id = ?",
                [VERIFIED_PARTIAL_CAPTURE.into()],
            ))
            .await
            .expect("read terminal receipt metadata")
            .expect("pending terminal session");
        let receipt_metadata: String = row.try_get_by("metadata_json").expect("receipt metadata");
        assert!(!receipt_metadata.contains(VERIFIED_PARTIAL_TEXT));
        assert!(!receipt_metadata.contains(VERIFIED_PARTIAL_CANARY));
        terminal.assert_no_native_or_partial_payload().await;

        let fixture = VerifiedPartialStoreFixture::new().await;
        crate::internal::ai::capture::coordinator::test_support::interrupt_after_checkpoint_once();
        assert!(
            fixture.write(0x501, false, None).await.is_err(),
            "post-CAS interruption stays retryable"
        );
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        fixture
            .write(0x501, false, None)
            .await
            .expect("same action recovers durable checkpoint once");
        assert_eq!(
            fixture
                .scalar("SELECT COUNT(*) AS n FROM agent_checkpoint")
                .await,
            1
        );
        fixture.assert_no_native_or_partial_payload().await;
        let fixture = VerifiedPartialStoreFixture::new().await;
        let checkpoint = fixture
            .write(0x601, false, None)
            .await
            .expect("real durable partial source");
        let (bytes, metadata) = fixture.read_back(&checkpoint).await;
        assert!(!bytes.is_empty());
        assert_eq!(metadata["transcript_snapshot"]["completeness"], "partial");
        let (state, processed) = fixture.settle_real_lease(authorized(false, None)).await;
        assert_eq!(state, "idle");
        assert_eq!(
            processed, 1,
            "real SQL lease advances only after an independently confirmed durable store result"
        );
        fixture.assert_no_native_or_partial_payload().await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn legacy_v1_covered_noop_settles_nonempty_prior_revision() {
        use crate::internal::ai::{
            coverage_gate::tests::ReasoningFixture,
            observed_agents::reasoning::carrier_test_support as native,
            traces::{self, TracesInflightMarker},
        };

        let fixture = ReasoningFixture::new().await;
        let checkpoint_id = "legacy-v1-covered-noop";
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
            .expect("seed old no-op's ordinary marker");
        assert_eq!(
            settle_covered_checkpoint_replay(
                &fixture.conn,
                &fixture.scope,
                native::CAPTURE,
                checkpoint_id,
                None,
            )
            .await
            .expect("settle old v1 no-op with a committed revision"),
            CoveredCheckpointReplay::Settled,
        );
        assert!(
            crate::internal::metadata::MetadataKv::get_with_conn(
                &fixture.conn,
                crate::internal::metadata::MetadataScope::AgentTracesInflight,
                native::CAPTURE,
                checkpoint_id,
            )
            .await
            .expect("read old no-op marker")
            .is_none(),
            "the old no-op's ordinary marker must be retired"
        );
    }

    #[cfg(unix)]
    #[test]
    fn native_reasoning_checkpoint_requires_authenticated_identity_plan() {
        let native =
            crate::internal::ai::capture::checkpoint::reasoning_coverage_test_disposition(vec![
                b"synthetic native reasoning".to_vec(),
            ]);
        let error = require_identity_plan_for_native_reasoning(&native, false).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("authenticated per-turn identity")
        );
        assert!(require_identity_plan_for_native_reasoning(&native, true).is_ok());
        assert!(
            require_identity_plan_for_native_reasoning(
                &ReasoningSourceDisposition::Ordinary,
                false,
            )
            .is_ok()
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_and_export_reasoning_reader_preserves_all_skip_and_mixed_partial() {
        use crate::internal::ai::{
            coverage_gate::{ReasoningReservationOutcome, tests::ReasoningFixture},
            observed_agents::coverage::{Completeness, NormalizedTurn},
        };

        let fixture = ReasoningFixture::new().await;
        let incumbent = crate::internal::ai::coverage_gate::tests::turn(
            "synthetic-turn-0",
            "ordinary transcript turn",
            Completeness::Complete,
        );
        let plan = fixture
            .plan(
                std::slice::from_ref(&incumbent),
                vec![b"typed private bytes".to_vec()],
            )
            .await;
        let first = fixture
            .reserve(std::slice::from_ref(&incumbent), Some(&plan), "seed-owner")
            .await;
        assert_eq!(first.ordinary.reserved.len(), 1);
        fixture
            .commit(
                &first.ordinary.reserved,
                &plan,
                "seed-owner",
                "seed-checkpoint",
            )
            .await;
        let identity_before = fixture.identity_text().await;
        let mixed = vec![
            incumbent.clone(),
            NormalizedTurn {
                logical_turn_key: "ordinary-fresh-turn".to_owned(),
                ordinal: 1,
                completeness: Completeness::Complete,
                started_at: None,
                ended_at: None,
                records: vec![crate::internal::ai::observed_agents::SemanticRecord::User {
                    text: "fresh ordinary turn".to_owned(),
                }],
            },
        ];

        for channel in ["live", "export"] {
            let owner = format!("{channel}-all-skip");
            let ordinary_reasoning = ReasoningSourceDisposition::Ordinary;
            let (all_skip, _): (ReasoningReservationOutcome, Option<ReasoningCoveragePlan>) =
                reserve_reasoning_coverage(
                    &fixture.conn,
                    &fixture.scope,
                    fixture.dir.path(),
                    fixture::CAPTURE,
                    std::slice::from_ref(&incumbent),
                    &ordinary_reasoning,
                    "claude_code",
                    &owner,
                    2,
                    channel,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(all_skip.skipped_incomplete_reasoning, 1, "{channel}");
            assert_eq!(all_skip.ordinary.skipped_covered, 0, "{channel}");
            assert!(all_skip.ordinary.reserved.is_empty(), "{channel}");

            let owner = format!("{channel}-mixed");
            let (mixed_outcome, _): (ReasoningReservationOutcome, Option<ReasoningCoveragePlan>) =
                reserve_reasoning_coverage(
                    &fixture.conn,
                    &fixture.scope,
                    fixture.dir.path(),
                    fixture::CAPTURE,
                    &mixed,
                    &ordinary_reasoning,
                    "claude_code",
                    &owner,
                    3,
                    channel,
                    None,
                )
                .await
                .unwrap();
            assert_eq!(mixed_outcome.skipped_incomplete_reasoning, 1, "{channel}");
            assert_eq!(mixed_outcome.ordinary.skipped_covered, 0, "{channel}");
            assert!(mixed_outcome.ordinary.reserved.is_empty(), "{channel}");
            assert_eq!(fixture.identity_text().await, identity_before, "{channel}");
        }

        let row = fixture
            .conn
            .query_one_raw(Statement::from_string(
                fixture.conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_coverage_claim".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get_by::<i64, _>("n").unwrap(), 1);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn live_and_export_reservations_commit_source_identity_atomically() {
        use crate::internal::ai::{
            capture::checkpoint::reasoning_coverage_test_disposition,
            coverage_gate::{LiveCaptureClaimCommitPlan, LiveClaimCommitPlan},
            observed_agents::coverage::Completeness,
        };

        for channel in ["live", "export"] {
            let fixture = crate::internal::ai::coverage_gate::tests::ReasoningFixture::new().await;
            let turn = crate::internal::ai::coverage_gate::tests::turn(
                "synthetic-turn-0",
                "ordinary transcript turn",
                Completeness::Complete,
            );
            let disposition = reasoning_coverage_test_disposition(vec![
                b"synthetic native wire for source identity".to_vec(),
            ]);
            let owner = format!("{channel}-identity-owner");
            let (reserved, candidate) = reserve_reasoning_coverage(
                &fixture.conn,
                &fixture.scope,
                fixture.dir.path(),
                fixture::CAPTURE,
                std::slice::from_ref(&turn),
                &disposition,
                "claude_code",
                &owner,
                100,
                channel,
                None,
            )
            .await
            .unwrap();
            assert_eq!(reserved.skipped_incomplete_reasoning, 0, "{channel}");
            assert_eq!(reserved.ordinary.reserved.len(), 1, "{channel}");
            let candidate = candidate.expect("native disposition yields an identity plan");

            let checkpoint_id = format!("{channel}-identity-checkpoint");
            let transaction_plan = LiveCaptureClaimCommitPlan::new(
                LiveClaimCommitPlan {
                    source_channel: channel,
                    session_id: fixture::CAPTURE.to_owned(),
                    checkpoint_id: checkpoint_id.clone(),
                    owner: owner.clone(),
                    parent_commit: None,
                    created_at: 100,
                    now_ms: 100,
                    claims: reserved.ordinary.reserved,
                    import_session: None,
                    import_identity: None,
                    capture_scope: Some(fixture.scope.clone()),
                },
                Some(&candidate),
            )
            .unwrap();
            let txn = sea_orm::TransactionTrait::begin(&fixture.conn)
                .await
                .unwrap();
            crate::internal::ai::history::TracesTxnExtra::apply(
                &transaction_plan,
                &txn,
                &crate::internal::ai::history::TracesCommitCtx {
                    commit_hash: format!("commit-{channel}"),
                    tree_oid: "tree".to_owned(),
                    metadata_blob_oid: "metadata".to_owned(),
                },
            )
            .await
            .unwrap();
            txn.commit().await.unwrap();

            let redelivery = reserve_reasoning_coverage(
                &fixture.conn,
                &fixture.scope,
                fixture.dir.path(),
                fixture::CAPTURE,
                std::slice::from_ref(&turn),
                &disposition,
                "claude_code",
                &format!("{channel}-redelivery"),
                200,
                channel,
                None,
            )
            .await
            .unwrap()
            .0;
            assert_eq!(redelivery.skipped_incomplete_reasoning, 0, "{channel}");
            assert_eq!(redelivery.ordinary.skipped_covered, 1, "{channel}");
            assert!(redelivery.ordinary.reserved.is_empty(), "{channel}");
        }
    }

    fn source_context(repo: &std::path::Path, bytes: &[u8]) -> AgentSessionCtx {
        let directory = crate::internal::ai::observed_agents::claude_session_dir(repo).unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join(format!("{}.jsonl", fixture::SOURCE));
        std::fs::write(&path, bytes).unwrap();
        AgentSessionCtx {
            session_id: fixture::CAPTURE.to_owned(),
            provider_session_id: fixture::SOURCE.to_owned(),
            working_dir: repo.to_path_buf(),
            transcript_path: Some(path),
        }
    }

    #[tokio::test]
    #[serial(env)]
    async fn reasoning_none_live_bytes_unchanged() {
        let home = tempfile::tempdir().unwrap();
        let _home = TestHomeScope::set(home.path());
        let (dir, conn) = live_fixture().await;
        let repo = dir.path().canonicalize().unwrap();
        let bytes = b"ordinary live transcript remains byte identical\n";
        let ctx = source_context(&repo, bytes);
        let snapshot =
            CaptureSnapshotService::capture_live(&fixture::NONE, &ctx, Default::default());
        assert_eq!(
            snapshot.completeness(),
            CaptureSnapshotCompleteness::Complete
        );
        assert_eq!(
            snapshot.try_into_ordinary().unwrap().unwrap().bytes(),
            bytes
        );
        let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::NONE);
        let binding =
            LiveCaptureBinding::resolve(crate::internal::ai::hooks::providers::claude_provider());
        let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
        let context = LiveCaptureContext {
            libra_session_id: fixture::CAPTURE,
            provider_session_id: fixture::SOURCE,
            verified_cwd: &repo,
        };
        let (projection, transcript, reasoning) = acquire_live_snapshot(
            &conn,
            &repo,
            &scope,
            binding,
            &context,
            None,
            &mut json!({}),
        )
        .await;
        assert_eq!(
            projection.completeness,
            CaptureSnapshotCompleteness::Complete
        );
        assert_eq!(transcript.unwrap().bytes(), bytes);
        assert!(reasoning.is_ordinary());
    }

    pub(crate) async fn assert_no_deadline_reasoning_handoff() {
        let home = tempfile::tempdir().unwrap();
        let _home = TestHomeScope::set(home.path());
        let (dir, conn) = live_fixture().await;
        let repo = dir.path().canonicalize().unwrap();
        let bytes = [b"ordinary ".as_slice(), fixture::CANARY].concat();
        source_context(&repo, &bytes);
        let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::STRIP);
        let binding =
            LiveCaptureBinding::resolve(crate::internal::ai::hooks::providers::claude_provider());
        let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
        let context = LiveCaptureContext {
            libra_session_id: fixture::CAPTURE,
            provider_session_id: fixture::SOURCE,
            verified_cwd: &repo,
        };
        let (projection, transcript, reasoning) = acquire_live_snapshot(
            &conn,
            &repo,
            &scope,
            binding,
            &context,
            None,
            &mut json!({}),
        )
        .await;
        assert_eq!(
            projection.partial_reason,
            Some(CaptureSnapshotPartialReason::SourceAuthorizationMismatch)
        );
        assert_eq!(transcript.unwrap().bytes(), b"ordinary ");
        assert!(!reasoning.is_ordinary());
        assert!(
            projection
                .source
                .as_ref()
                .unwrap()
                .digest_sha256
                .as_ref()
                .unwrap()
                .starts_with("sha256:")
        );
    }

    async fn deliver(
        conn: &sea_orm::DatabaseConnection,
        repo: &std::path::Path,
        session: &str,
        hook: &str,
        command: crate::internal::ai::hooks::provider::ProviderHookCommand,
        kind: LifecycleEventKind,
    ) -> Result<()> {
        use crate::internal::ai::capture::ingress::CaptureDeadline;
        deliver_with_deadline(
            conn,
            repo,
            session,
            hook,
            command,
            kind,
            Some(CaptureDeadline::from_budget_millis(5000)?),
        )
        .await
    }

    #[cfg(unix)]
    async fn deliver_without_deadline(
        conn: &sea_orm::DatabaseConnection,
        repo: &std::path::Path,
        session: &str,
        hook: &str,
        command: crate::internal::ai::hooks::provider::ProviderHookCommand,
        kind: LifecycleEventKind,
    ) -> Result<()> {
        deliver_with_deadline(conn, repo, session, hook, command, kind, None).await
    }

    async fn deliver_with_deadline(
        conn: &sea_orm::DatabaseConnection,
        repo: &std::path::Path,
        session: &str,
        hook: &str,
        command: crate::internal::ai::hooks::provider::ProviderHookCommand,
        kind: LifecycleEventKind,
        deadline: Option<crate::internal::ai::capture::ingress::CaptureDeadline>,
    ) -> Result<()> {
        let storage_path = repo.join(crate::utils::util::ROOT_DIR);
        use crate::internal::ai::capture::ingress::{
            CaptureIngressBinding, CaptureIngressCommand, CaptureRuntimeScope,
        };
        let provider = crate::internal::ai::hooks::providers::claude_provider();
        let bytes=serde_json::to_vec(&json!({"hook_event_name":hook,"session_id":session,"cwd":repo,"prompt":"ordinary prompt"})).unwrap();
        let outcome =
            CaptureIngressCommand::from_payload(&bytes, command, kind, provider, deadline, |_| {
                Ok(CaptureIngressBinding::new(
                    repo.to_string_lossy().into_owned(),
                    [0xA5; 32],
                    CaptureRuntimeScope::new(storage_path.clone(), repo.to_path_buf()),
                ))
            });
        crate::internal::ai::capture::test_support::ingest_agent_traces_ingress_outcome_for_test(
            outcome,
            command,
            provider,
            conn,
            Some(&storage_path),
        )
        .await
    }
    #[cfg(unix)]
    async fn session_row(
        conn: &sea_orm::DatabaseConnection,
        session: &str,
    ) -> (i64, serde_json::Value) {
        let row=conn.query_one_raw(Statement::from_sql_and_values(conn.get_database_backend(),
            "SELECT sync_revision, metadata_json FROM agent_session WHERE provider_session_id = ?",[session.to_owned().into()])).await.unwrap().unwrap();
        (
            row.try_get_by("sync_revision").unwrap(),
            serde_json::from_str(&row.try_get_by::<String, _>("metadata_json").unwrap()).unwrap(),
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(env)]
    async fn reasoning_unready_pending_rejection_preserves_actual_terminal_attempt() {
        use crate::internal::ai::hooks::provider::ProviderHookCommand;
        let home = tempfile::tempdir().unwrap();
        let _home = TestHomeScope::set(home.path());
        let (dir, conn) = live_fixture().await;
        let repo = dir.path().canonicalize().unwrap();
        let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::STRIP);
        for (session, hook, command, kind, pending) in [
            (
                "og16-terminal",
                "SessionEnd",
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
                true,
            ),
            (
                "og16-not-tracked",
                "Stop",
                ProviderHookCommand::Stop,
                LifecycleEventKind::TurnEnd,
                false,
            ),
        ] {
            deliver(
                &conn,
                &repo,
                session,
                "SessionStart",
                ProviderHookCommand::SessionStart,
                LifecycleEventKind::SessionStart,
            )
            .await
            .unwrap();
            let (before, _) = session_row(&conn, session).await;
            let error = deliver(&conn, &repo, session, hook, command, kind)
                .await
                .expect_err("actual writer must reject before fallback/coverage");
            assert!(
                error.downcast_ref::<SourcePreparationRejected>().is_some(),
                "{error:#}"
            );
            let (after, metadata) = session_row(&conn, session).await;
            assert_eq!(
                after,
                before + 1,
                "only catalog reservation may advance sync_revision"
            );
            let rendered = metadata.to_string();
            assert!(!rendered.contains(std::str::from_utf8(fixture::CANARY).unwrap()));
            if pending {
                assert!(rendered.contains("pending"));
                assert!(
                    !rendered.contains("checkpoint_write"),
                    "Pending suppresses wrapper checkpoint-write diagnosis"
                );
            } else {
                assert!(
                    rendered.contains("checkpoint_write"),
                    "NotTracked retains its bounded fixed diagnosis"
                );
            }
            let n: i64 = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM agent_checkpoint".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("n")
                .unwrap();
            assert_eq!(n, 0);
            let n: i64 = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM agent_coverage_claim".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("n")
                .unwrap();
            assert_eq!(n, 0);
        }
        // Exercise the real pending seal preflight for both an owning native
        // batch and a zero-artifact partial, before envelope/MAC writes.
        for adapter in [
            &fixture::NATIVE,
            &fixture::STRIP,
            &fixture::UNMAPPED,
            &fixture::HELPER,
        ] {
            let snapshot = crate::internal::ai::capture::snapshot::carrier_test_snapshot(
                adapter,
                fixture::CANARY.to_vec(),
            );
            let redacted = crate::internal::ai::observed_agents::Redactor::new_default()
                .redact(b"{}")
                .0;
            let payload = CheckpointRedactedPayload::from_snapshot(
                snapshot,
                redacted.clone(),
                redacted.clone(),
                redacted,
            )
            .unwrap();
            super::super::pending_payload::assert_reasoning_pending_seal_rejected(&payload).await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn reasoning_partial_outcome_reaches_actual_live_handlers() {
        use crate::internal::ai::hooks::provider::ProviderHookCommand;

        let home = tempfile::tempdir().unwrap();
        let _home = TestHomeScope::set(home.path());
        let dir = tempfile::tempdir().unwrap();
        crate::utils::test::setup_with_new_libra_in(dir.path()).await;
        let db_path = dir
            .path()
            .join(crate::utils::util::ROOT_DIR)
            .join(crate::utils::util::DATABASE);
        let conn = sea_orm::Database::connect(format!("sqlite://{}", db_path.display()))
            .await
            .unwrap();
        let repo = dir.path().canonicalize().unwrap();
        let transcript = concat!(
            r#"{"type":"user","uuid":"synthetic-turn-0","message":{"role":"user","content":"ordinary OG16_CANDIDATE_PRIVATE_CANARY"}}"#,
            "\n",
            r#"{"type":"assistant","uuid":"assistant-0","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
            "\n",
        );
        source_context(&repo, transcript.as_bytes());

        // First actual Stop commits a synthetic-only source identity through
        // the live writer. The fixture does not exercise a provider archive
        // claim or prove any real native field origin.
        let _snapshot_helper_bypass =
            crate::internal::ai::capture::live_pipeline::test_support::capture_live_without_deadline();
        let _native = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::NATIVE);
        // The lib test host has no main-registered object-I/O helper. Keep
        // this integration on the real in-process live path without a
        // deadline so checkpoint writes use the supported direct test route;
        // deadline/helper behavior has separate bounded-process coverage.
        deliver_without_deadline(
            &conn,
            &repo,
            fixture::SOURCE,
            "SessionStart",
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
        )
        .await
        .unwrap_or_else(|error| panic!("synthetic live source commit failed: {error:#}"));
        deliver_without_deadline(
            &conn,
            &repo,
            fixture::SOURCE,
            "Stop",
            ProviderHookCommand::Stop,
            LifecycleEventKind::TurnEnd,
        )
        .await
        .unwrap_or_else(|error| panic!("synthetic live source commit failed: {error:#}"));

        async fn checkpoint_count(conn: &sea_orm::DatabaseConnection) -> i64 {
            conn.query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_checkpoint".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by::<i64, _>("n")
            .unwrap()
        }
        let committed_checkpoints = checkpoint_count(&conn).await;
        assert_eq!(committed_checkpoints, 1, "live source should commit once");
        let identity_before: String = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT value FROM metadata_kv WHERE scope = 'agent_reasoning_identity' AND target = ?",
                [fixture::CAPTURE.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("value")
            .unwrap();

        drop(_native);
        let _unproven = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::STRIP);
        let error = deliver_without_deadline(
            &conn,
            &repo,
            fixture::SOURCE,
            "SessionEnd",
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
        )
        .await
        .expect_err("unproven repeat must not complete the terminal receipt");
        assert!(
            error.downcast_ref::<SourcePreparationRejected>().is_some(),
            "actual live handler should preserve a fixed source-preparation rejection: {error:#}"
        );
        assert_eq!(checkpoint_count(&conn).await, committed_checkpoints);
        let identity_after: String = conn
            .query_one_raw(Statement::from_sql_and_values(
                conn.get_database_backend(),
                "SELECT value FROM metadata_kv WHERE scope = 'agent_reasoning_identity' AND target = ?",
                [fixture::CAPTURE.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("value")
            .unwrap();
        assert_eq!(identity_after, identity_before);
        let (_, metadata) = session_row(&conn, fixture::SOURCE).await;
        assert!(metadata.to_string().contains("pending"));
        let reserved: i64 = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE state LIKE 'reserved%'"
                    .to_owned(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by("n")
            .unwrap();
        assert_eq!(reserved, 0, "partial must release only fresh reservations");
    }

    #[cfg(unix)]
    #[tokio::test]
    #[serial(cwd, env)]
    async fn reasoning_actual_terminal_seal_then_store_preserves_owned_carrier() {
        use crate::internal::ai::{
            capture::{
                catalog::CaptureCatalogStore,
                checkpoint::test_support::{CheckpointFaultPoint, armed_fault, fail_once_at},
                recovery::{ArtifactRecoveryOutcome, DoctorReplayRequest, replay_for_doctor},
            },
            capture_scope::CaptureCommitDeadline,
            hooks::provider::ProviderHookCommand,
        };

        for point in [
            CheckpointFaultPoint::AfterMarkerRegistration,
            CheckpointFaultPoint::AfterObjectWrite,
            CheckpointFaultPoint::CompanionTransaction,
            CheckpointFaultPoint::MarkerCleanup,
        ] {
            let home = tempfile::tempdir().unwrap();
            let _home = TestHomeScope::set(home.path());
            let dir = tempfile::tempdir().unwrap();
            crate::utils::test::setup_with_new_libra_in(dir.path()).await;
            let db_path = dir
                .path()
                .join(crate::utils::util::ROOT_DIR)
                .join(crate::utils::util::DATABASE);
            let conn = sea_orm::Database::connect(format!("sqlite://{}", db_path.display()))
                .await
                .unwrap();
            let repo = dir.path().canonicalize().unwrap();
            let transcript = concat!(
                r#"{"type":"user","uuid":"synthetic-turn-0","message":{"role":"user","content":"ordinary OG21_LIVE_PRIVATE_CANARY"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"assistant-0","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
                "\n",
            );
            source_context(&repo, transcript.as_bytes());
            let _native = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::NATIVE);
            let _snapshot_helper_bypass =
            crate::internal::ai::capture::live_pipeline::test_support::capture_live_without_deadline();
            let executable = std::env::current_exe().unwrap();
            let helper = executable
                .parent()
                .and_then(std::path::Path::parent)
                .unwrap()
                .join("libra");
            assert!(helper.is_file(), "built Libra helper is unavailable");
            crate::internal::ai::authorized_read::with_test_helper_program(helper, async {
            deliver(
                &conn,
                &repo,
                fixture::SOURCE,
                "SessionStart",
                ProviderHookCommand::SessionStart,
                LifecycleEventKind::SessionStart,
            )
            .await
            .unwrap();
            deliver(
                &conn,
                &repo,
                fixture::SOURCE,
                "Stop",
                ProviderHookCommand::Stop,
                LifecycleEventKind::TurnEnd,
            )
            .await
            .unwrap();
            let extended = concat!(
                r#"{"type":"user","uuid":"synthetic-turn-0","message":{"role":"user","content":"ordinary OG21_LIVE_PRIVATE_CANARY"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"assistant-0","message":{"role":"assistant","content":[{"type":"text","text":"done"}]}}"#,
                "\n",
                r#"{"type":"user","uuid":"synthetic-turn-1","message":{"role":"user","content":"ordinary second turn"}}"#,
                "\n",
                r#"{"type":"assistant","uuid":"assistant-1","message":{"role":"assistant","content":[{"type":"text","text":"second done"}]}}"#,
                "\n",
            );
            source_context(&repo, extended.as_bytes());
            fail_once_at(point);
            let first = deliver(
                &conn,
                &repo,
                fixture::SOURCE,
                "SessionEnd",
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
            )
            .await;
            assert_eq!(armed_fault(), None, "{point:?}: terminal delivery must reach checkpoint store");
            assert!(first.is_err(), "{point:?}: injected failure must not acknowledge completion");
            let pending: i64 = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_capture_pending'"
                        .to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("n")
                .unwrap();
            {
                assert_eq!(pending, 1, "{point:?}: actual terminal must retain typed pending evidence");
                let catalog = CaptureCatalogStore::new(conn.clone());
                let recovery = catalog
                    .pending_finalizer_recoveries_for_doctor(chrono::Utc::now().timestamp_millis())
                    .await
                    .unwrap()
                    .recoveries
                    .into_iter()
                    .find(|item| item.artifact_present())
                    .unwrap();
                let storage = repo.join(crate::utils::util::ROOT_DIR);
                if point != CheckpointFaultPoint::MarkerCleanup {
                    // Object-write and companion-transaction failures leave
                    // cleanup-owned markers. Doctor repairs those before
                    // retrying the retained terminal artifact.
                    let history = crate::internal::ai::history::HistoryManager::for_traces(
                        std::sync::Arc::new(crate::utils::client_storage::ClientStorage::try_init(
                            storage.join("objects"),
                        ).expect("storage")),
                        storage.clone(),
                        std::sync::Arc::new(conn.clone()),
                    );
                    assert!(
                        history
                            .repair_expired_traces_inflight_marker_for_test(
                                fixture::CAPTURE,
                                recovery.checkpoint_id(),
                                chrono::Utc::now().timestamp_millis(),
                            )
                            .await
                            .unwrap(),
                        "{point:?}: doctor must retire failed attempt marker"
                    );
                }
                let outcome = replay_for_doctor(DoctorReplayRequest {
                    conn: &conn,
                    recovery: &recovery,
                    storage: &storage,
                    root: &repo,
                    repo_path: &storage,
                    now_millis: chrono::Utc::now().timestamp_millis(),
                    deadline: CaptureCommitDeadline::from_budget(Duration::from_secs(60)).unwrap(),
                    manual: false,
                })
                .await
                .unwrap_or_else(|error| panic!("{point:?}: actual replay failed: {error:#}"));
                assert_eq!(outcome, ArtifactRecoveryOutcome::Completed, "{point:?}");
            }
            let identity: i64 = conn
                .query_one_raw(Statement::from_sql_and_values(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM metadata_kv WHERE scope = 'agent_reasoning_identity' AND target = ?",
                    [fixture::CAPTURE.into()],
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("n")
                .unwrap();
            assert_eq!(identity, 1, "same native carrier must reach identity commit");
            let committed_claim: i64 = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM agent_coverage_claim WHERE logical_turn_key = 'synthetic-turn-1' AND state = 'catalog_committed'".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("n")
                .unwrap();
            assert_eq!(committed_claim, 1, "{point:?}: original claim must commit once");
            deliver(
                &conn,
                &repo,
                fixture::SOURCE,
                "SessionEnd",
                ProviderHookCommand::SessionEnd,
                LifecycleEventKind::SessionEnd,
            )
            .await
            .unwrap();
            let checkpoints: i64 = conn
                .query_one_raw(Statement::from_string(
                    conn.get_database_backend(),
                    "SELECT COUNT(*) AS n FROM agent_checkpoint".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get_by("n")
                .unwrap();
            assert_eq!(checkpoints, 2, "{point:?}: successful retry must not append twice");
        })
        .await;
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn reasoning_pending_covered_prefix_keeps_reserved_identity_subset() {
        super::super::pending::tests::assert_covered_prefix_keeps_reserved_identity_subset().await;
    }
    #[cfg(target_os = "macos")]
    fn assert_source_descriptor_closed(path: &std::path::Path) {
        let expected = path.canonicalize().unwrap();
        for fd in 0..4096 {
            let mut buffer = [0_i8; libc::PATH_MAX as usize];
            // SAFETY: F_GETPATH writes at most PATH_MAX bytes into this live
            // writable buffer; invalid descriptors simply return an error.
            if unsafe { libc::fcntl(fd, libc::F_GETPATH, buffer.as_mut_ptr()) } == 0 {
                // SAFETY: successful F_GETPATH produces a NUL-terminated path.
                let name = unsafe { std::ffi::CStr::from_ptr(buffer.as_ptr()) };
                if let Ok(name) = name.to_str() {
                    assert_ne!(
                        std::path::Path::new(name),
                        expected,
                        "source descriptor remained open on a failure exit"
                    );
                }
            }
        }
    }

    #[cfg(target_os = "macos")]
    async fn source_resource_failure_matrix() {
        use std::os::unix::fs::PermissionsExt;
        for failure in ["timeout", "cancel", "error"] {
            let home = tempfile::tempdir().unwrap();
            let _home = TestHomeScope::set(home.path());
            let repo = tempfile::tempdir().unwrap();
            let ctx = source_context(repo.path(), b"ordinary resource test");
            let source = ctx.transcript_path.as_ref().unwrap().clone();
            let helper = tempfile::tempdir().unwrap();
            let pid_file = helper.path().join("helper.pid");
            let script = helper.path().join("helper.sh");
            let pid_path = pid_file.to_string_lossy().replace('\'', "'\\''");
            let tail = if failure == "error" {
                "exit 2"
            } else {
                "exec /bin/sleep 60"
            };
            std::fs::write(
                &script,
                format!("#!/bin/sh\nprintf '%s' \"$$\" > '{pid_path}'\n{tail}\n"),
            )
            .unwrap();
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
            let capture = tokio::spawn(async move {
                crate::internal::ai::authorized_read::with_test_helper_program(
                    script,
                    CaptureSnapshotService::capture_live_until(
                        &fixture::NONE,
                        &ctx,
                        CaptureSnapshotPolicy::with_deadline(Some(
                            Instant::now() + Duration::from_secs(2),
                        )),
                    ),
                )
                .await
            });
            let pid_deadline = Instant::now() + Duration::from_secs(1);
            let mut pid_contents = String::new();
            while Instant::now() < pid_deadline {
                if let Ok(contents) = std::fs::read_to_string(&pid_file)
                    && !contents.trim().is_empty()
                {
                    pid_contents = contents;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(
                !pid_contents.trim().is_empty(),
                "actual helper must write its PID before the injected exit"
            );
            let pid: i32 = pid_contents.parse().expect("helper PID must be an integer");
            if failure == "cancel" {
                capture.abort();
                assert!(capture.await.unwrap_err().is_cancelled());
            } else {
                let snapshot = capture.await.unwrap();
                let expected = if failure == "timeout" {
                    CaptureSnapshotPartialReason::DeadlineExceeded
                } else {
                    CaptureSnapshotPartialReason::SourceReadError
                };
                assert_eq!(snapshot.partial_reason(), Some(expected));
                assert!(snapshot.transcript().is_none());
            }
            let reap_deadline = Instant::now() + Duration::from_secs(5);
            loop {
                // SAFETY: signal zero only checks this known fixture PID.
                if unsafe { libc::kill(pid, 0) } != 0 {
                    break;
                }
                assert!(
                    Instant::now() < reap_deadline,
                    "helper process group was not reaped after {failure}"
                );
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_source_descriptor_closed(&source);
        }
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[serial(env)]
    async fn reasoning_carrier_failures_release_descriptors() {
        source_resource_failure_matrix().await;
        let home = tempfile::tempdir().unwrap();
        let _home = TestHomeScope::set(home.path());
        let repo = tempfile::tempdir().unwrap();
        let ctx = source_context(repo.path(), fixture::CANARY);
        let source = ctx.transcript_path.as_ref().unwrap().clone();
        // A malformed in-process preparation owns and drops its descriptor;
        // an enabled deadline route rejects before any source FD is opened.
        let snapshot =
            CaptureSnapshotService::capture_live(&fixture::MALFORMED, &ctx, Default::default());
        assert!(snapshot.try_into_ordinary().is_err());
        assert_source_descriptor_closed(&source);
        let snapshot = CaptureSnapshotService::capture_live_until(
            &fixture::STRIP,
            &ctx,
            CaptureSnapshotPolicy::with_deadline(Some(Instant::now() + Duration::from_secs(1))),
        )
        .await;
        assert!(snapshot.try_into_ordinary().is_err());
        assert_source_descriptor_closed(&source);
    }

    #[cfg(target_os = "macos")]
    #[tokio::test]
    #[serial(env)]
    async fn reasoning_carrier_failures_reap_helpers() {
        source_resource_failure_matrix().await;
    }

    #[tokio::test]
    #[serial(env)]
    async fn reasoning_carrier_failures_preserve_lease_ownership() {
        use crate::internal::ai::{
            capture::live::{ExportJobLeaseStore, LiveExportAdmission},
            hooks::provider::ProviderHookCommand,
        };
        let home = tempfile::tempdir().unwrap();
        let _home = TestHomeScope::set(home.path());
        let (dir, conn) = live_fixture().await;
        let repo = dir.path().canonicalize().unwrap();
        deliver(
            &conn,
            &repo,
            fixture::SOURCE,
            "SessionStart",
            ProviderHookCommand::SessionStart,
            LifecycleEventKind::SessionStart,
        )
        .await
        .unwrap();
        let scope = CaptureScope::main_for_connection(&conn).await.unwrap();
        let bytes=serde_json::to_vec(&json!({"type":"user","uuid":"foreign-turn","message":{"role":"user","content":"foreign ordinary claim"}})).unwrap();
        let mut turns = crate::internal::ai::observed_agents::normalize_claude_transcript(&bytes);
        crate::internal::ai::observed_agents::coverage::redact_turns(&mut turns);
        let held =
            crate::internal::ai::coverage_gate::reserve_turn_claims_for_channel_with_capture_scope(
                &conn,
                &scope,
                fixture::CAPTURE,
                &turns,
                "foreign-owner",
                Utc::now().timestamp_millis(),
                "live",
            )
            .await
            .unwrap();
        assert_eq!(held.reserved.len(), 1);
        let lease = ExportJobLeaseStore::new(&conn);
        let target = LiveExportTarget {
            agent_kind: "claude_code",
            provider_session_id: fixture::SOURCE,
            scope: &scope,
        };
        let deadline = CaptureCommitDeadline::from_budget(Duration::from_secs(30)).unwrap();
        let LiveExportAdmission::Runner { fence_token, .. } = lease
            .observe_idle(
                &target,
                "foreign-runner",
                Utc::now().timestamp_millis(),
                deadline,
            )
            .await
            .unwrap()
        else {
            panic!("fixture owns its runner")
        };
        let claim = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT owner, fence_token, state FROM agent_coverage_claim".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        let before: (String, i64, String) = (
            claim.try_get_by("owner").unwrap(),
            claim.try_get_by("fence_token").unwrap(),
            claim.try_get_by("state").unwrap(),
        );
        let _selected = scoped_test_snapshot_adapter(AgentKind::ClaudeCode, &fixture::STRIP);
        let error = deliver(
            &conn,
            &repo,
            fixture::SOURCE,
            "SessionEnd",
            ProviderHookCommand::SessionEnd,
            LifecycleEventKind::SessionEnd,
        )
        .await
        .unwrap_err();
        assert!(error.downcast_ref::<SourcePreparationRejected>().is_some());
        let claim = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT owner, fence_token, state FROM agent_coverage_claim".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        let after: (String, i64, String) = (
            claim.try_get_by("owner").unwrap(),
            claim.try_get_by("fence_token").unwrap(),
            claim.try_get_by("state").unwrap(),
        );
        assert_eq!(
            before, after,
            "unsupported source must not release a foreign/durable coverage owner"
        );
        let runner = conn
            .query_one_raw(Statement::from_string(
                conn.get_database_backend(),
                "SELECT owner, fence_token, state FROM agent_export_job".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            runner.try_get_by::<String, _>("owner").unwrap(),
            "foreign-runner"
        );
        assert_eq!(
            runner.try_get_by::<i64, _>("fence_token").unwrap(),
            fence_token
        );
        assert_eq!(runner.try_get_by::<String, _>("state").unwrap(), "inflight");
        // Cleanup uses the owner's actual fence, never a blanket release.
        lease
            .release_failed(&target, "foreign-runner", fence_token, deadline)
            .await
            .unwrap();
    }
}
