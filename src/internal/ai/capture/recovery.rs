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
            CaptureCatalogStore, CaptureCatalogTerminalAttempt,
        },
        checkpoint::{CheckpointStore, CheckpointWriteOutcome, TracesCheckpointStore},
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
        Ok(verified) => verified,
        Err(error) if pending::retryable_load_failure(&error, deadline.monotonic()) => {
            return Ok(ArtifactRecoveryOutcome::RetryLater);
        }
        Err(_) => return Ok(ArtifactRecoveryOutcome::ManualRequired),
    };

    let catalog = CaptureCatalogStore::new_until(conn.clone(), deadline);
    let terminal_fence = if manual {
        match catalog
            .claim_manual_pending_artifact(&verified, &identity, &header, now_millis, deadline)
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
            .claim_pending_artifact_attempt(recovery, &verified, &identity, now_millis, deadline)
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
                return complete_durable_receipt(catalog, recovery, now_millis).await;
            }
            CaptureCatalogTerminalAttempt::Quarantined { .. }
            | CaptureCatalogTerminalAttempt::Adopted { .. }
            | CaptureCatalogTerminalAttempt::ConflictUnchanged { .. } => {
                return Ok(ArtifactRecoveryOutcome::ManualRequired);
            }
        }
    };

    let claims = &verified.coverage().claims;
    let owner = &verified.coverage().owner;
    let registration_fences = claims
        .iter()
        .map(|claim| TracesCoverageFence {
            logical_turn_key: &claim.logical_turn_key,
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
    let outcome = checkpoint_store
        .write(request)
        .await
        .context("write authenticated capture checkpoint; run `libra agent doctor`")?;
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
