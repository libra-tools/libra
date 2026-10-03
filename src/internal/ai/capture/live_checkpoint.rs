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
                CheckpointStoreError, CheckpointWriteRequest, SubagentCheckpointCommitPlan,
                TracesCheckpointStore,
            },
            coordinator::{
                CaptureCoordinator, CaptureCoordinatorFinalizer, CaptureCoordinatorOutcome,
                CaptureCoordinatorRequest, CaptureCoordinatorReserveOutcome,
                CoordinatorReservationClass, DurableFinalizerDisposition,
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
                CaptureSnapshotCompleteness, CaptureSnapshotPartialReason, CaptureSnapshotPolicy,
                CaptureSnapshotProjection, CaptureSnapshotService, DeadlineExtractionResult,
            },
        },
        capture_scope::{CaptureCommitDeadline, CaptureScope},
        coverage_gate::ReservedTurnClaim,
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
        },
    },
    utils::util,
};

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
            .map_err(|error| {
                anyhow!("resume terminal pending receipt within its settlement deadline: {error}")
            })? {
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
            .map_err(|error| anyhow!("settle no-checkpoint terminal capture receipt: {error}"))?
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
            .map_err(|error| anyhow!("settle no-checkpoint capture receipt: {error}"))?
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
        .map_err(|error| anyhow!("settle covered terminal capture replay: {error}"))?
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
) {
    let agent_kind = binding.agent_kind_db();
    let adapter = binding.observed();
    // Never use a provider-supplied transcript pointer. A provider whose
    // native layout is deterministically scoped by the verified cwd and
    // provider session id derives its candidate through its live capability;
    // other adapters remain source-absent until they can prove an equivalent
    // derivation.
    let transcript_path = match binding.live_transcript_candidate(context) {
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
    #[cfg(test)]
    let snapshot_deadline = (!test_support::live_capture_without_deadline())
        .then_some(capture_deadline)
        .flatten()
        .map(CaptureCommitDeadline::monotonic);
    #[cfg(not(test))]
    let snapshot_deadline = capture_deadline.map(CaptureCommitDeadline::monotonic);
    let mut snapshot = CaptureSnapshotService::capture_live_until(
        adapter,
        &seam_ctx,
        CaptureSnapshotPolicy::with_deadline(snapshot_deadline),
    )
    .await;
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
    (projection, snapshot.into_redacted_transcript())
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
        live_reservation: Option<(
            String,
            i64,
            Vec<crate::internal::ai::coverage_gate::ReservedTurnClaim>,
        )>,
    },
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
        return Ok(LiveCoverageStage::Proceed {
            catalog_reservation: Box::new(catalog_reservation),
            live_reservation: None,
        });
    }
    let owner = format!("live:{}:{}", std::process::id(), uuid::Uuid::new_v4());
    let now_ms = Utc::now().timestamp_millis();
    let reservation = match capture_deadline {
        Some(deadline) => {
            coverage_gate::reserve_live_turn_claims_until(
                conn,
                scope,
                libra_session_id,
                &turns,
                &owner,
                now_ms,
                deadline,
            )
            .await
        }
        None => {
            coverage_gate::reserve_live_turn_claims_with_capture_scope(
                conn,
                scope,
                libra_session_id,
                &turns,
                &owner,
                now_ms,
            )
            .await
        }
    };
    let outcome = match reservation {
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
        live_reservation: Some((owner, now_ms, outcome.reserved)),
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
    let (live_projection, live_transcript) = acquire_live_snapshot(
        conn,
        repo_path,
        scope,
        binding,
        &live_context,
        capture_deadline,
        &mut report_value,
    )
    .await;
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
            } => {
                transcript_snapshot_projection = Some(*projection);
                transcript_is_authorized = true;
                transcript_redacted = transcript;
                claim_channel = "export";
                live_reservation = Some(*claims);
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
            report_value,
            transcript_is_authorized,
            transcript_snapshot_projection,
            transcript_redacted,
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
        claims: Box<(String, i64, Vec<ReservedTurnClaim>)>,
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
    reserved: Vec<ReservedTurnClaim>,
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
    use crate::internal::ai::{coverage_gate, observed_agents::coverage};

    let libra_session_id = context.libra_session_id;
    // `effective_capture_deadline` establishes an exporter's default budget
    // before catalog/snapshot work. Reuse that exact pair here; re-anchoring
    // it at export entry would let earlier durable runtime work borrow a
    // second execution window.
    let export_deadline = capture_deadline
        .context("export capture deadline was not established at runtime ingress")?;
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
        let exported = match exporter.export(context, export_deadline.monotonic()).await {
            Ok(source) => source,
            Err(_) => return Err(ExportStageExit::BridgeUnavailable),
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
        let mut snapshot = CaptureSnapshotService::capture_authorized(
            source,
            binding.agent_kind_db(),
            libra_session_id,
            CaptureSnapshotPolicy::with_deadline(Some(export_deadline.monotonic())),
        );
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
            (Some(preimage), Ok(storage_path)) => {
                derive_snapshot_content_commitment_in_scope_until(
                    conn,
                    scope,
                    &storage_path,
                    repo_path,
                    &preimage,
                    export_deadline.monotonic(),
                )
                .await
                .ok()
            }
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
        let mut turns = (exporter.export_coverage_normalizer())(snapshot_bytes);
        coverage::redact_turns(&mut turns);
        let outcome = match coverage_gate::reserve_turn_claims_for_channel_with_capture_scope_until(
            conn,
            scope,
            libra_session_id,
            &turns,
            &owner,
            now_ms,
            coverage_gate::CaptureReservationExecution::export(export_deadline),
        )
        .await
        {
            Ok(outcome) => outcome,
            // Fail-closed: no ungated append; the runner is released dirty
            // so the next idle retries.
            Err(error) => return Err(ExportStageExit::CoverageFailed(error)),
        };
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
        let fully_covered = !turns.is_empty()
            && outcome.reserved.is_empty()
            && outcome.skipped_covered == turns.len()
            && outcome.skipped_inflight == 0
            && outcome.conflicted == 0;
        let covered_terminal_replay = catalog_request.mutation().is_terminal()
            && preexisting_durable_terminal
            && fully_covered;
        if outcome.is_noop()
            && (!catalog_request.mutation().is_terminal() || covered_terminal_replay)
        {
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
                match settle_covered_checkpoint_replay(
                    conn,
                    scope,
                    libra_session_id,
                    checkpoint_id,
                    Some(export_deadline),
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
        // The snapshot owns the only raw-source boundary; its content and
        // report are already redacted here.
        let projection = snapshot.safe_projection();
        merge_redaction_report_into(report_value, snapshot.redaction_report());
        let Some(transcript) = snapshot.into_redacted_transcript() else {
            return Err(ExportStageExit::SourceRejected(anyhow!(
                "LBR-AGENT-005: complete export snapshot lost its redacted transcript"
            )));
        };
        Ok(ExportedSource {
            projection,
            transcript,
            reserved: outcome.reserved,
        })
    }
    .await;
    match source {
        Ok(ExportedSource {
            projection,
            transcript,
            reserved,
        }) => Ok(ExportStage::Proceed {
            catalog_reservation: Box::new(catalog_reservation),
            runner,
            projection: Box::new(projection),
            transcript,
            claims: Box::new((owner, now_ms, reserved)),
        }),
        Err(exit) => {
            let (disposition, reason, log) =
                export_stage_lease_disposition(&exit, libra_session_id);
            let settled = runner.settle(disposition, reason, log).await;
            // Only a failed `AdvanceAndRelease` advance returns `Err`; the
            // runner is already retired dirty, so skip the continuation and
            // make the host retry the catalog receipt.
            settled.context("settle no-op export job within its capture deadline")?;
            match exit {
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
            }
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
    report_value: serde_json::Value,
    transcript_is_authorized: bool,
    transcript_snapshot_projection: Option<CaptureSnapshotProjection>,
    transcript_redacted: RedactedBytes,
    claim_channel: &'static str,
    live_reservation: Option<(String, i64, Vec<ReservedTurnClaim>)>,
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
        mut report_value,
        transcript_is_authorized,
        transcript_snapshot_projection,
        transcript_redacted,
        claim_channel,
        live_reservation,
    } = input;
    let runner_held = runner.is_some();
    let exit = async {
        // From here until the coordinator durably consumes the reservation,
        // every fallible preparation step and every non-committing outcome
        // must release the scope-fenced claims it may have acquired above.
        // These exits have not committed this invocation's checkpoint, so
        // they intentionally do not record a retryable checkpoint
        // diagnostic.
        let failure_claim_owner = live_reservation.as_ref().map(|(owner, _, _)| owner.clone());
        let mut cleanup = CheckpointReservationCleanup {
            session_id: libra_session_id,
            checkpoint_id,
            scope,
            claim_channel,
            claim_owner: failure_claim_owner.as_deref(),
        };

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
                transcript_is_authorized.then_some(&transcript_redacted),
                subagent_snapshots.children_omitted_for_deadline(),
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
                transcript_is_authorized.then_some(&transcript_redacted),
                &subagent_snapshots.transcripts,
                &subagent_extraction_warnings,
            ),
        };
        attach_subagent_snapshot_metadata(&mut extraction_value, &subagent_snapshots);
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
        let checkpoint_snapshot_projection = transcript_snapshot_projection.clone();
        let mut metadata = metadata;
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
        let metadata_bytes = match serde_json::to_vec_pretty(&metadata) {
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
        let redaction_report_bytes = match serde_json::to_vec_pretty(&redaction_report_value) {
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
        let (owner, claim_now_ms, claims) = live_reservation.unwrap_or_else(|| {
            (
                format!("live-barrier:{}", uuid::Uuid::new_v4()),
                Utc::now().timestamp_millis(),
                Vec::new(),
            )
        });
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
        let registration_fences = claim_plan
            .claims
            .iter()
            .map(|claim| traces::TracesCoverageFence {
                logical_turn_key: &claim.logical_turn_key,
                owner: &claim_plan.owner,
                fence_token: claim.fence_token,
                reservation_state: "reserved_live",
            })
            .collect::<Vec<_>>();

        // The sealed payload is the only value handed to the checkpoint
        // store. Every byte was already redacted above; the optional safe
        // projection makes a metadata-only compatibility fallback explicitly
        // partial rather than falsely claiming an authorized source snapshot.
        let checkpoint_payload = match CheckpointRedactedPayload::from_redacted_capture(
            transcript_redacted,
            checkpoint_snapshot_projection,
            metadata_redacted,
            lifecycle_events_redacted,
            redaction_report_redacted,
        ) {
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
                    return uncommitted(conn, &cleanup, capture_deadline, None, || {
                        Err(anyhow!(
                            "claim terminal capture checkpoint attempt: {error}"
                        ))
                    })
                    .await;
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
                            return uncommitted(conn, &cleanup, capture_deadline, None, || {
                                Err(anyhow!(
                                    "complete durable terminal replay under catalog fence: {error}"
                                ))
                            })
                            .await;
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
                .persist_pending_artifact(
                    registration_fence,
                    &checkpoint_payload,
                    &claim_plan,
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
            Some(&claim_plan),
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
                let failure = || Err(anyhow!("capture coordinator execution failed: {error}"));
                return match error.reservation_class() {
                    CoordinatorReservationClass::Uncommitted { diagnostic } => {
                        uncommitted(
                            conn,
                            &cleanup,
                            capture_deadline,
                            diagnostic.then_some(&diagnostic_request),
                            failure,
                        )
                        .await
                    }
                    CoordinatorReservationClass::PostCheckpoint => post_checkpoint(failure()),
                    CoordinatorReservationClass::Unclassified => expire(anyhow!(
                        "capture coordinator execution failed: {error}"
                    )),
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
            settle_preapplied_without_checkpoint(
                conn,
                *catalog_reservation,
                &catalog_request,
                capture_finalizer_deadline_millis,
                terminal_settlement_deadline,
            )
            .await
        }
        CheckpointStageExit::Uncommitted(result) | CheckpointStageExit::PostCheckpoint(result) => {
            result
        }
        CheckpointStageExit::Authorized => Ok(()),
        CheckpointStageExit::Expire(error) => Err(error),
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
) -> Result<()> {
    use crate::internal::ai::observed_agents::{RedactedBytes, Redactor};

    // Resolve the parent `committed` checkpoint (if any) for linkage.
    let parent_checkpoint_id = latest_committed_checkpoint_id(conn, libra_session_id).await?;

    let boundary = match event.kind {
        LifecycleEventKind::SubagentStart => "start",
        _ => "end",
    };
    // The subagent boundary hook envelope carries only `session_id` + `cwd`
    // (see the Codex `SubagentStop` row in agent.md) — no distinct subagent
    // id and no tool-use id. We therefore leave `subagent_session_id` /
    // `tool_use_id` NULL rather than inventing them: in particular
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
    let metadata = serde_json::json!({
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

    let report_value = serde_json::from_str::<serde_json::Value>(redaction_report_json)
        .unwrap_or_else(|_| serde_json::json!({}));
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
        Some(&commit_plan),
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
        | CaptureCoordinatorOutcome::CheckpointAlreadyExists { .. }
        | CaptureCoordinatorOutcome::CheckpointInFlight { .. }
        | CaptureCoordinatorOutcome::AlreadyApplied => Ok(()),
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
        deadline_omitted_source_count: sources.len(),
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
        let snapshot = source.into_parent_extraction_snapshot(parent_session_id, policy);
        snapshots.projections.push(snapshot.safe_projection());
        snapshots
            .redaction_reports
            .push(snapshot.redaction_report().clone());
        if let Some(transcript) = snapshot.into_redacted_transcript() {
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
/// metadata. `source_count` reflects every independently durable child;
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
