//! Provider-neutral capture orchestration.
//!
//! This layer owns the fixed ordering between the pure reducer's catalog
//! mutation and the checkpoint store.  It intentionally knows nothing about
//! provider names, transcript paths, database handles, or ref layout: entry
//! adapters construct typed requests and the two ports enforce their own
//! scope/redaction/transaction contracts.

use thiserror::Error;

use crate::internal::ai::{
    capture::{
        catalog::{
            CaptureCatalogApplyRequest, CaptureCatalogApplyResult, CaptureCatalogCompleteRequest,
            CaptureCatalogCompleteResult, CaptureCatalogConflict, CaptureCatalogError,
            CaptureCatalogFinalizeProof, CaptureCatalogFinalizeRequest,
            CaptureCatalogFinalizeResult, CaptureCatalogPort, CaptureCatalogTerminalAttempt,
            CaptureReceiptDisposition,
        },
        checkpoint::{
            CheckpointStore, CheckpointStoreError, CheckpointWriteOutcome, CheckpointWriteRequest,
        },
        finalizer::{
            CaptureFinalizePolicy, FinalizeCheckpointProgress, FinalizePendingStage,
            FinalizeQuarantineReason,
        },
        state::CheckpointWrite,
    },
    traces::CheckpointScope,
};

#[cfg(test)]
pub(crate) mod test_support {
    use std::cell::Cell;

    thread_local! {
        static INTERRUPT_AFTER_CHECKPOINT: Cell<bool> = const { Cell::new(false) };
    }

    pub(crate) fn interrupt_after_checkpoint_once() {
        INTERRUPT_AFTER_CHECKPOINT.with(|armed| armed.set(true));
    }

    pub(crate) fn take_interrupt_after_checkpoint() -> bool {
        INTERRUPT_AFTER_CHECKPOINT.with(|armed| armed.replace(false))
    }
}

/// A fully typed capture operation assembled by a hook or import entrypoint.
///
/// The coordinator validates the relationship between the reducer's
/// checkpoint action and the sealed checkpoint request before it lets either
/// store mutate.  This prevents a caller from committing a snapshot under an
/// unrelated catalog receipt.
pub(crate) struct CaptureCoordinatorRequest<'a> {
    catalog: CaptureCatalogApplyRequest,
    checkpoint: Option<CheckpointWriteRequest<'a>>,
    finalizer: Option<CaptureCoordinatorFinalizer>,
    /// Set only after the reservation has atomically elected/adopted the
    /// terminal marker/source pair. `execute` must not count another
    /// NotStarted attempt for this exact handoff.
    finalizer_attempt_claimed: bool,
}

/// Caller-injected timing policy for one terminal operation. The absolute
/// timestamp is captured once by the entrypoint and persisted by the catalog;
/// retries cannot replace it with a longer window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CaptureCoordinatorFinalizer {
    policy: CaptureFinalizePolicy,
    now_millis: i64,
}

impl CaptureCoordinatorFinalizer {
    pub(crate) fn new(policy: CaptureFinalizePolicy, now_millis: i64) -> Self {
        Self { policy, now_millis }
    }
}

/// Content-free binding taken from a sealed checkpoint request before it is
/// moved into the store. It prevents a retry from swapping its source digest
/// or marker generation between finalizer stages.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CoordinatorFinalizeBinding {
    policy: CaptureFinalizePolicy,
    now_millis: i64,
    marker_generation: String,
    source_digest: Option<String>,
}

/// The catalog's completion decision after a checkpoint has become durable.
///
/// `AlreadyComplete` is distinct from the absence of a terminal binding:
/// conflating the two would send a terminal request into the legacy
/// proof-less completion path (or report a false finalizer error) when a
/// sibling completed the receipt in the narrow write-to-completion interval.
enum TerminalCompletionDecision {
    Nonterminal,
    Proof(CaptureCatalogFinalizeProof),
    Classified(DurableFinalizerDisposition),
}

/// A terminal receipt disposition observed after the checkpoint/ref
/// transaction is already durable. Entry adapters must distinguish these
/// from their pre-write twins: releasing coverage as though no checkpoint had
/// committed would erase companion evidence or strand an export lease.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DurableFinalizerDisposition {
    ReceiptAlreadyApplied,
    Quarantined { reason: FinalizeQuarantineReason },
    Conflict { conflict: CaptureCatalogConflict },
}

impl<'a> CaptureCoordinatorRequest<'a> {
    pub(crate) fn new(
        catalog: CaptureCatalogApplyRequest,
        checkpoint: Option<CheckpointWriteRequest<'a>>,
    ) -> Result<Self, CaptureCoordinatorError> {
        Self::build(catalog, checkpoint, None, false)
    }

    /// Port-level regression tests construct a terminal request whose
    /// finalizer has not been elected yet. Production terminal requests are
    /// always built by [`Self::with_claimed_finalizer`] after the
    /// reservation's atomic marker/source election.
    #[cfg(test)]
    pub(crate) fn with_finalizer(
        catalog: CaptureCatalogApplyRequest,
        checkpoint: Option<CheckpointWriteRequest<'a>>,
        finalizer: CaptureCoordinatorFinalizer,
    ) -> Result<Self, CaptureCoordinatorError> {
        Self::build(catalog, checkpoint, Some(finalizer), false)
    }

    /// Construct a terminal execution after
    /// [`CaptureCoordinatorReservation::claim_terminal_checkpoint_attempt`]
    /// has atomically selected the marker/source fence.
    pub(crate) fn with_claimed_finalizer(
        catalog: CaptureCatalogApplyRequest,
        checkpoint: Option<CheckpointWriteRequest<'a>>,
        finalizer: CaptureCoordinatorFinalizer,
    ) -> Result<Self, CaptureCoordinatorError> {
        Self::build(catalog, checkpoint, Some(finalizer), true)
    }

    fn build(
        catalog: CaptureCatalogApplyRequest,
        checkpoint: Option<CheckpointWriteRequest<'a>>,
        finalizer: Option<CaptureCoordinatorFinalizer>,
        finalizer_attempt_claimed: bool,
    ) -> Result<Self, CaptureCoordinatorError> {
        let required = catalog.mutation().checkpoint();
        match (required, checkpoint.as_ref()) {
            (CheckpointWrite::None, None) => {}
            (CheckpointWrite::None, Some(_)) => {
                return Err(CaptureCoordinatorError::UnexpectedCheckpoint);
            }
            (CheckpointWrite::Committed, Some(request))
                if request.scope() == CheckpointScope::Committed => {}
            (CheckpointWrite::SubagentBoundary, Some(request))
                if request.scope() == CheckpointScope::Subagent => {}
            (CheckpointWrite::Committed | CheckpointWrite::SubagentBoundary, None) => {
                return Err(CaptureCoordinatorError::MissingCheckpoint);
            }
            _ => return Err(CaptureCoordinatorError::CheckpointScopeMismatch),
        }
        if let Some(checkpoint) = checkpoint.as_ref()
            && (checkpoint.replay_key() != catalog.action().action_key()
                || checkpoint.session_id() != catalog.session().session_id())
        {
            return Err(CaptureCoordinatorError::CheckpointReceiptMismatch);
        }
        if catalog.mutation().is_terminal() {
            let Some(finalizer) = finalizer.as_ref() else {
                return Err(CaptureCoordinatorError::MissingTerminalFinalizer);
            };
            if finalizer.policy.replay_key() != catalog.action().action_key() {
                return Err(CaptureCoordinatorError::FinalizerReceiptMismatch);
            }
        } else if finalizer.is_some() {
            return Err(CaptureCoordinatorError::UnexpectedFinalizer);
        }
        Ok(Self {
            catalog,
            checkpoint,
            finalizer,
            finalizer_attempt_claimed,
        })
    }
}

/// Typed, dependency-injected coordinator shared by live and import entry
/// adapters.
pub(crate) struct CaptureCoordinator<C, S> {
    catalog: C,
    checkpoint: S,
    preapplied: Option<(CaptureCatalogApplyRequest, CaptureCatalogApplyResult)>,
}

/// Marker checkpoint port for lifecycle actions whose reducer explicitly
/// requires no checkpoint. `execute_preapplied` returns before touching this
/// port; an attempted write is therefore an invariant violation reported as a
/// typed request error instead of a panic or a hidden success.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct NoCheckpointStore;

#[async_trait::async_trait]
impl CheckpointStore for NoCheckpointStore {
    async fn write(
        &self,
        _: CheckpointWriteRequest<'_>,
    ) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
        Err(CheckpointStoreError::InvalidRequest {
            field: "no-checkpoint lifecycle action".to_string(),
        })
    }
}

/// Opaque reservation retained by the coordinator between the cheap catalog
/// phase and a later redacted checkpoint preparation. An entry adapter may
/// inspect only checkpoint necessity; it cannot access the catalog port or
/// recreate completion sequencing.
pub(crate) struct CaptureCoordinatorReservation<C> {
    catalog: C,
    request: CaptureCatalogApplyRequest,
    result: CaptureCatalogApplyResult,
}

/// Coordinator-owned result of the early catalog reservation phase.
pub(crate) enum CaptureCoordinatorReserveOutcome<C> {
    Reserved(Box<CaptureCoordinatorReservation<C>>),
    AlreadyApplied,
    ConflictUnchanged { conflict: CaptureCatalogConflict },
}

impl<C> CaptureCoordinatorReservation<C> {
    /// Bind a pre-applied port result to its catalog instance without exposing
    /// either field to an entry adapter. Concrete store factories live outside
    /// this provider-neutral module.
    pub(crate) fn from_preapplied(
        catalog: C,
        request: CaptureCatalogApplyRequest,
        result: CaptureCatalogApplyResult,
    ) -> Self {
        let request = effective_catalog_request(request, &result);
        Self {
            catalog,
            request,
            result,
        }
    }

    /// Turn an opaque reservation into the actual coordinator that performs
    /// the write/complete sequence. Entry adapters construct this value only
    /// after their provider-specific source preparation is ready.
    pub(crate) fn into_coordinator<S>(self, checkpoint: S) -> CaptureCoordinator<C, S>
    where
        C: CaptureCatalogPort,
        S: CheckpointStore,
    {
        CaptureCoordinator::with_preapplied(self.catalog, checkpoint, self.request, self.result)
    }

    pub(crate) fn checkpoint_kind(&self) -> CheckpointWrite {
        match &self.result {
            CaptureCatalogApplyResult::Applied { checkpoint, .. }
            | CaptureCatalogApplyResult::ResumePending { checkpoint, .. } => *checkpoint,
            CaptureCatalogApplyResult::AlreadyApplied
            | CaptureCatalogApplyResult::ConflictUnchanged { .. } => CheckpointWrite::None,
        }
    }

    /// The catalog may atomically replace an ID-less terminal delivery with
    /// the action stored in an eligible pending receipt. Entry adapters must
    /// derive every later checkpoint/finalizer identity from this effective
    /// request, never from the fresh ingress UUID.
    pub(crate) fn effective_request(&self) -> &CaptureCatalogApplyRequest {
        &self.request
    }

    pub(crate) fn receipt_is_pending(&self) -> bool {
        match &self.result {
            CaptureCatalogApplyResult::Applied { receipt, .. } => {
                *receipt == CaptureReceiptDisposition::Pending
            }
            CaptureCatalogApplyResult::ResumePending { .. } => true,
            CaptureCatalogApplyResult::AlreadyApplied
            | CaptureCatalogApplyResult::ConflictUnchanged { .. } => false,
        }
    }
}

/// A catalog can atomically adopt the action identity from an earlier local
/// terminal receipt when an ID-less `SessionEnd` is redelivered.  Every
/// follow-on operation must address that adopted receipt, so normalize the
/// request at the shared reservation boundary rather than relying on each
/// live/import adapter to remember this protocol rule.
fn effective_catalog_request(
    request: CaptureCatalogApplyRequest,
    result: &CaptureCatalogApplyResult,
) -> CaptureCatalogApplyRequest {
    if let CaptureCatalogApplyResult::ResumePending {
        adopted_action: Some(action),
        ..
    } = result
    {
        request.with_effective_action(action.clone())
    } else {
        request
    }
}

impl<C> CaptureCoordinatorReservation<C>
where
    C: CaptureCatalogPort,
{
    /// Finish a pre-reserved nonterminal receipt when an adapter has proven
    /// that no checkpoint work is required (for example, every logical turn
    /// is already covered by an equivalent durable checkpoint). This consumes
    /// the opaque reservation so a native replay cannot be stranded in the
    /// pending receipt ledger merely because the append was a safe no-op.
    ///
    /// Terminal actions are deliberately rejected: only a durable
    /// checkpoint/finalizer proof may publish their receipt.
    pub(crate) async fn complete_preapplied_without_checkpoint(
        self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        let Self {
            catalog,
            request: reserved_request,
            result,
        } = self;
        if reserved_request != *request {
            return Err(CaptureCoordinatorError::PreappliedRequestMismatch);
        }
        if request.mutation().is_terminal() {
            return Err(CaptureCoordinatorError::TerminalCompletionWithoutCheckpoint);
        }
        match result {
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
            | CaptureCatalogApplyResult::ResumePending { .. } => {
                let completion = CaptureCatalogCompleteRequest::from_apply(request)
                    .map_err(CaptureCoordinatorError::CatalogComplete)?;
                match catalog
                    .complete(&completion)
                    .await
                    .map_err(CaptureCoordinatorError::CatalogComplete)?
                {
                    CaptureCatalogCompleteResult::Completed => {
                        Ok(CaptureCoordinatorOutcome::StateApplied)
                    }
                    CaptureCatalogCompleteResult::AlreadyComplete => {
                        Ok(CaptureCoordinatorOutcome::AlreadyApplied)
                    }
                    CaptureCatalogCompleteResult::ConflictUnchanged { conflict } => {
                        Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict })
                    }
                }
            }
            CaptureCatalogApplyResult::Applied { .. } => {
                Ok(CaptureCoordinatorOutcome::StateApplied)
            }
            CaptureCatalogApplyResult::AlreadyApplied => {
                Ok(CaptureCoordinatorOutcome::AlreadyApplied)
            }
            CaptureCatalogApplyResult::ConflictUnchanged { conflict } => {
                Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict })
            }
        }
    }

    /// Acknowledge a fully covered terminal replay without creating a second
    /// checkpoint. This is intentionally narrower than the ordinary no-op
    /// path: the request must have been reserved over an already durable
    /// terminal state, and the catalog rechecks that its finalizer is still
    /// only the provisional, content-free marker. A new terminal transition,
    /// a changed source, or an in-flight writer remains on the strict
    /// checkpoint/finalizer path.
    pub(crate) async fn complete_preapplied_terminal_coverage_replay(
        self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        let Self {
            catalog,
            request: reserved_request,
            result,
        } = self;
        if reserved_request != *request {
            return Err(CaptureCoordinatorError::PreappliedRequestMismatch);
        }
        if !request.mutation().is_terminal()
            || request.mutation().checkpoint() != CheckpointWrite::Committed
            || !request.mutation().is_durable_terminal_replay_candidate()
        {
            return Err(CaptureCoordinatorError::MissingTerminalFinalizer);
        }
        match result {
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
            | CaptureCatalogApplyResult::ResumePending { .. } => {
                let completion =
                    CaptureCatalogCompleteRequest::from_covered_terminal_replay(request)
                        .map_err(CaptureCoordinatorError::CatalogComplete)?;
                match catalog
                    .complete(&completion)
                    .await
                    .map_err(CaptureCoordinatorError::CatalogComplete)?
                {
                    CaptureCatalogCompleteResult::Completed => {
                        Ok(CaptureCoordinatorOutcome::StateApplied)
                    }
                    CaptureCatalogCompleteResult::AlreadyComplete => {
                        Ok(CaptureCoordinatorOutcome::AlreadyApplied)
                    }
                    CaptureCatalogCompleteResult::ConflictUnchanged { conflict } => {
                        Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict })
                    }
                }
            }
            CaptureCatalogApplyResult::Applied { .. } => {
                Err(CaptureCoordinatorError::MissingTerminalFinalizer)
            }
            CaptureCatalogApplyResult::AlreadyApplied => {
                Ok(CaptureCoordinatorOutcome::AlreadyApplied)
            }
            CaptureCatalogApplyResult::ConflictUnchanged { conflict } => {
                Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict })
            }
        }
    }

    /// Complete a terminal receipt only after the catalog has rechecked its
    /// deterministic checkpoint in the same writer transaction. This is the
    /// replay-only counterpart to `execute_preapplied`: a changed source
    /// never reaches a checkpoint store or registers a marker merely because
    /// the original writer retired its marker before receipt completion.
    pub(crate) async fn complete_preapplied_durable_replay(
        self,
        request: &CaptureCatalogApplyRequest,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        let Self {
            catalog,
            request: reserved_request,
            result,
        } = self;
        if reserved_request != *request {
            return Err(CaptureCoordinatorError::PreappliedRequestMismatch);
        }
        if !request.mutation().is_terminal()
            || request.mutation().checkpoint() != CheckpointWrite::Committed
        {
            return Err(CaptureCoordinatorError::MissingTerminalFinalizer);
        }
        match result {
            CaptureCatalogApplyResult::Applied {
                receipt: CaptureReceiptDisposition::Pending,
                ..
            }
            | CaptureCatalogApplyResult::ResumePending { .. } => {
                match catalog
                    .complete_durable_replay(request)
                    .await
                    .map_err(CaptureCoordinatorError::CatalogComplete)?
                {
                    CaptureCatalogCompleteResult::Completed => {
                        let checkpoint_id =
                            crate::internal::ai::capture::checkpoint::checkpoint_id_for_capture_action(
                                request.action().event_id(),
                                CheckpointWrite::Committed,
                            );
                        Ok(CaptureCoordinatorOutcome::CheckpointAlreadyExists { checkpoint_id })
                    }
                    CaptureCatalogCompleteResult::AlreadyComplete => {
                        Ok(CaptureCoordinatorOutcome::AlreadyApplied)
                    }
                    CaptureCatalogCompleteResult::ConflictUnchanged { conflict } => {
                        Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict })
                    }
                }
            }
            CaptureCatalogApplyResult::Applied { .. } => {
                Err(CaptureCoordinatorError::MissingTerminalFinalizer)
            }
            CaptureCatalogApplyResult::AlreadyApplied => {
                Ok(CaptureCoordinatorOutcome::AlreadyApplied)
            }
            CaptureCatalogApplyResult::ConflictUnchanged { conflict } => {
                Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict })
            }
        }
    }

    /// Ensure a terminal receipt has durable finalizer evidence before any
    /// fallible source, coverage, lock, or checkpoint work begins. Fresh and
    /// legacy pending receipts receive one provisional content-free marker;
    /// an already-bound attempt is observed without replacing its fence.
    ///
    /// The method deliberately borrows the reservation so runtime can later
    /// hand the same opaque token to `execute_preapplied`; no adapter gains a
    /// direct catalog completion path.
    pub(crate) async fn prepare_terminal_finalizer(
        &self,
        request: &CaptureCatalogApplyRequest,
        finalizer: CaptureCoordinatorFinalizer,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        if self.request != *request {
            return Err(CaptureCoordinatorError::PreappliedRequestMismatch);
        }
        if !request.mutation().is_terminal() || self.checkpoint_kind() == CheckpointWrite::None {
            return Err(CaptureCoordinatorError::MissingTerminalFinalizer);
        }
        let needs_binding = matches!(
            &self.result,
            CaptureCatalogApplyResult::Applied { .. }
                | CaptureCatalogApplyResult::ResumePending {
                    terminal_finalizer_needs_binding: true,
                    ..
                }
        );
        if !needs_binding {
            // A pre-existing pending receipt may already carry a concrete
            // marker. Preparation must never replace it with the provisional
            // marker below.
            tracing::info!(
                target: "agent.capture.finalize",
                replay_key = %request.action().action_key(),
                finalizer_outcome = "pending",
                finalizer_stage = "checkpoint_unavailable",
                "terminal capture remains replayable until a repository checkpoint is available"
            );
            return Ok(CaptureCoordinatorOutcome::FinalizerPending {
                attempts: None,
                stage: None,
            });
        }
        let provisional_marker = format!(
            "capture-finalizer-unbound-v1:{}",
            request.action().action_key()
        );
        let finalize = CaptureCatalogFinalizeRequest::from_apply(
            request,
            finalizer.policy,
            provisional_marker,
            None,
            finalizer.now_millis,
            FinalizeCheckpointProgress::NotStarted,
        )
        .map_err(CaptureCoordinatorError::CatalogFinalize)?;
        match self
            .catalog
            .claim_terminal_attempt(&finalize, false)
            .await
            .map_err(CaptureCoordinatorError::CatalogFinalize)?
        {
            CaptureCatalogTerminalAttempt::Bound { attempts, .. } => {
                tracing::info!(
                    target: "agent.capture.finalize",
                    replay_key = %request.action().action_key(),
                    finalizer_outcome = "pending",
                    finalizer_stage = ?FinalizePendingStage::Snapshot,
                    finalizer_attempts = attempts,
                    "terminal capture is pending because no repository checkpoint is available"
                );
                Ok(CaptureCoordinatorOutcome::FinalizerPending {
                    attempts: Some(attempts),
                    stage: Some(FinalizePendingStage::Snapshot),
                })
            }
            CaptureCatalogTerminalAttempt::Adopted { .. }
            | CaptureCatalogTerminalAttempt::DurableReplay => {
                tracing::info!(
                    target: "agent.capture.finalize",
                    replay_key = %request.action().action_key(),
                    finalizer_outcome = "pending",
                    finalizer_stage = ?FinalizePendingStage::Snapshot,
                    "terminal capture is pending because no repository checkpoint is available"
                );
                Ok(CaptureCoordinatorOutcome::FinalizerPending {
                    attempts: None,
                    stage: Some(FinalizePendingStage::Snapshot),
                })
            }
            CaptureCatalogTerminalAttempt::Quarantined { reason } => {
                tracing::warn!(
                    target: "agent.capture.finalize",
                    replay_key = %request.action().action_key(),
                    finalizer_outcome = "quarantined",
                    finalizer_reason = ?reason,
                    "terminal capture cannot be retried without repair"
                );
                Ok(CaptureCoordinatorOutcome::FinalizerQuarantined { reason })
            }
            CaptureCatalogTerminalAttempt::AlreadyComplete => {
                Ok(CaptureCoordinatorOutcome::AlreadyApplied)
            }
            CaptureCatalogTerminalAttempt::ConflictUnchanged { conflict } => {
                Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict })
            }
        }
    }

    /// Atomically elect the real marker/source fence immediately before a
    /// terminal checkpoint store is built. The returned marker is durable
    /// catalog state, so duplicate native deliveries cannot create an
    /// independent process-local writer attempt.
    pub(crate) async fn claim_terminal_checkpoint_attempt(
        &self,
        request: &CaptureCatalogApplyRequest,
        finalizer: CaptureCoordinatorFinalizer,
        candidate_marker_generation: String,
        source_digest: Option<String>,
    ) -> Result<CaptureCatalogTerminalAttempt, CaptureCoordinatorError> {
        if self.request != *request {
            return Err(CaptureCoordinatorError::PreappliedRequestMismatch);
        }
        if !request.mutation().is_terminal() || self.checkpoint_kind() == CheckpointWrite::None {
            return Err(CaptureCoordinatorError::MissingTerminalFinalizer);
        }
        let finalize = CaptureCatalogFinalizeRequest::from_apply(
            request,
            finalizer.policy,
            candidate_marker_generation,
            source_digest,
            finalizer.now_millis,
            FinalizeCheckpointProgress::NotStarted,
        )
        .map_err(CaptureCoordinatorError::CatalogFinalize)?;
        self.catalog
            .claim_terminal_attempt(&finalize, true)
            .await
            .map_err(CaptureCoordinatorError::CatalogFinalize)
    }

    /// Record a terminal receipt as explicitly pending when the hook has no
    /// repository path and therefore cannot construct a checkpoint store.
    /// This path never publishes `stopped`; it writes (or observes) the
    /// provisional, content-free finalizer fence which a later real
    /// checkpoint attempt can bind exactly once.
    pub(crate) async fn persist_terminal_pending_without_checkpoint(
        self,
        request: &CaptureCatalogApplyRequest,
        finalizer: CaptureCoordinatorFinalizer,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        self.prepare_terminal_finalizer(request, finalizer).await
    }
}

/// Reserve the reducer mutation before an adapter performs expensive source
/// preparation.  This is still part of the coordinator protocol: callers
/// must feed the returned result into [`CaptureCoordinator::execute_preapplied`]
/// rather than recreating checkpoint/receipt completion sequencing locally.
pub(crate) async fn reserve_capture_catalog<C>(
    catalog: &C,
    request: &CaptureCatalogApplyRequest,
) -> Result<CaptureCatalogApplyResult, CaptureCoordinatorError>
where
    C: CaptureCatalogPort,
{
    catalog
        .apply(request)
        .await
        .map_err(CaptureCoordinatorError::CatalogApply)
}

impl<C, S> CaptureCoordinator<C, S>
where
    C: CaptureCatalogPort,
    S: CheckpointStore,
{
    /// One-step construction for catalog/checkpoint port regression tests.
    /// Production adapters obtain a coordinator only from an opaque
    /// [`CaptureCoordinatorReservation::into_coordinator`] handoff.
    #[cfg(test)]
    pub(crate) fn new(catalog: C, checkpoint: S) -> Self {
        Self {
            catalog,
            checkpoint,
            preapplied: None,
        }
    }

    fn with_preapplied(
        catalog: C,
        checkpoint: S,
        request: CaptureCatalogApplyRequest,
        preapplied: CaptureCatalogApplyResult,
    ) -> Self {
        Self {
            catalog,
            checkpoint,
            preapplied: Some((request, preapplied)),
        }
    }

    /// Execute exactly one lifecycle action in the only safe order:
    ///
    /// `scope/fence + dedup/reducer mutation → checkpoint → receipt
    /// completion`.
    ///
    /// A checkpoint failure deliberately leaves a pending catalog receipt;
    /// retries re-enter through `ResumePending` and never apply the lifecycle
    /// mutation a second time.  A terminal receipt is completed only after a
    /// durable checkpoint outcome.
    ///
    /// This one-step apply-and-execute form exists only for port regression
    /// tests; it shares `execute_with_preapplied_result` with production.
    /// Live hooks use `into_coordinator(...).execute_preapplied`.
    #[cfg(test)]
    pub(crate) async fn execute(
        &self,
        request: CaptureCoordinatorRequest<'_>,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        let catalog_result = reserve_capture_catalog(&self.catalog, &request.catalog).await?;
        if matches!(
            &catalog_result,
            CaptureCatalogApplyResult::ResumePending {
                adopted_action: Some(_),
                ..
            }
        ) {
            // The generic one-step API has already sealed a checkpoint and
            // finalizer around the fresh ingress action. Rebinding it after
            // `apply` could complete a different receipt, so require callers
            // to use the split reservation protocol, which exposes the
            // effective action before those values are constructed.
            return Err(CaptureCoordinatorError::AdoptedActionRequiresReservation);
        }
        self.execute_with_preapplied_result(request, catalog_result)
            .await
    }

    /// Continue a catalog reservation obtained through
    /// [`reserve_capture_catalog`].  Live adapters use this split form to
    /// avoid reading a transcript or reserving coverage when the lifecycle
    /// receipt has already lost a scope/state fence.
    pub(crate) async fn execute_preapplied(
        &self,
        request: CaptureCoordinatorRequest<'_>,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        let (preapplied_request, result) = self
            .preapplied
            .clone()
            .ok_or(CaptureCoordinatorError::MissingPreappliedReservation)?;
        if preapplied_request != request.catalog {
            return Err(CaptureCoordinatorError::PreappliedRequestMismatch);
        }
        self.execute_with_preapplied_result(request, result).await
    }

    async fn execute_with_preapplied_result(
        &self,
        request: CaptureCoordinatorRequest<'_>,
        catalog_result: CaptureCatalogApplyResult,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        let (checkpoint_kind, receipt, resuming_pending, terminal_finalizer_needs_binding) =
            match catalog_result {
                CaptureCatalogApplyResult::Applied {
                    checkpoint,
                    receipt,
                    ..
                } => (checkpoint, receipt, false, false),
                CaptureCatalogApplyResult::ResumePending {
                    checkpoint,
                    terminal_finalizer_needs_binding,
                    ..
                } => (
                    checkpoint,
                    CaptureReceiptDisposition::Pending,
                    true,
                    terminal_finalizer_needs_binding,
                ),
                CaptureCatalogApplyResult::AlreadyApplied => {
                    return Ok(CaptureCoordinatorOutcome::AlreadyApplied);
                }
                CaptureCatalogApplyResult::ConflictUnchanged { conflict } => {
                    return Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict });
                }
            };

        if checkpoint_kind == CheckpointWrite::None {
            return Ok(CaptureCoordinatorOutcome::StateApplied);
        }

        let finalizer_attempt_claimed = request.finalizer_attempt_claimed;
        let checkpoint_request = request
            .checkpoint
            .ok_or(CaptureCoordinatorError::MissingCheckpoint)?;
        let finalizer_binding = if request.catalog.mutation().is_terminal() {
            let finalizer = request
                .finalizer
                .as_ref()
                .ok_or(CaptureCoordinatorError::MissingTerminalFinalizer)?;
            Some(CoordinatorFinalizeBinding {
                policy: finalizer.policy.clone(),
                now_millis: finalizer.now_millis,
                marker_generation: checkpoint_request.marker_generation().to_string(),
                source_digest: checkpoint_request
                    .payload()
                    .snapshot()
                    .source
                    .as_ref()
                    .and_then(|source| source.digest_sha256.clone()),
            })
        } else {
            None
        };

        // A resumed terminal receipt normally carries an earlier inflight
        // marker. Do not let a newly-constructed request take it over before
        // the checkpoint facade has told us whether the stable checkpoint is
        // already durable; `AlreadyExists` below then obtains the original
        // strict proof directly from the receipt ledger. The one exception
        // is a provisional no-repository receipt: the catalog explicitly
        // marks it unbound, and its first real checkpoint must atomically
        // bind marker and source *before* the write. That hint is derived
        // from the persisted receipt; ordinary pending markers always remain
        // fenced and cannot take this path.
        if !finalizer_attempt_claimed
            && (!resuming_pending || terminal_finalizer_needs_binding)
            && let Some(binding) = finalizer_binding.as_ref()
            && let Some(outcome) = self
                .persist_or_classify_finalizer(
                    &request.catalog,
                    binding,
                    FinalizeCheckpointProgress::NotStarted,
                )
                .await?
        {
            return Ok(outcome);
        }

        let requested_checkpoint_id = checkpoint_request.checkpoint_id().to_string();
        let checkpoint_outcome = match self.checkpoint.write(checkpoint_request).await {
            Ok(outcome) => outcome,
            Err(error) => {
                if let Some(binding) = finalizer_binding.as_ref() {
                    match self
                        .persist_or_classify_finalizer(
                            &request.catalog,
                            binding,
                            FinalizeCheckpointProgress::Retryable(
                                finalizer_stage_for_checkpoint_error(&error),
                            ),
                        )
                        .await
                    {
                        Ok(Some(outcome)) => return Ok(outcome),
                        Ok(None) => {}
                        Err(finalizer) => {
                            // The checkpoint write is still the causal
                            // failure: a finalizer persistence error must not
                            // disguise it as a catalog-only error, or live
                            // adapters will retain their scope-fenced
                            // coverage/export reservations indefinitely.
                            return Err(CaptureCoordinatorError::CheckpointWriteFinalizer {
                                checkpoint: error,
                                finalizer: Box::new(finalizer),
                            });
                        }
                    }
                }
                return Err(CaptureCoordinatorError::CheckpointWrite(error));
            }
        };

        match checkpoint_outcome {
            CheckpointWriteOutcome::TerminalReceiptAlreadyApplied => {
                // A duplicate terminal delivery lost the marker-registration
                // race only because its sibling already completed the exact
                // receipt. Do not re-enter finalizer accounting here: that
                // would turn a successful acknowledgement into a retryable
                // marker failure and could quarantine the settled session.
                Ok(CaptureCoordinatorOutcome::AlreadyApplied)
            }
            CheckpointWriteOutcome::AttemptInFlight { checkpoint_id, .. } => {
                // The catalog elected this exact marker generation for a
                // sibling delivery. This is neither a checkpoint failure nor
                // a retry: advancing the finalizer here could quarantine the
                // live elected writer under duplicate hook pressure.
                Ok(CaptureCoordinatorOutcome::CheckpointInFlight { checkpoint_id })
            }
            CheckpointWriteOutcome::ConflictUnchanged { reason } => {
                if let Some(binding) = finalizer_binding.as_ref() {
                    match self
                        .persist_or_classify_finalizer(
                            &request.catalog,
                            binding,
                            FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
                        )
                        .await
                    {
                        Ok(Some(outcome)) => return Ok(outcome),
                        Ok(None) => {}
                        Err(finalizer) => {
                            // This outcome guarantees that the store left
                            // the request unchanged. Preserve that causal
                            // fact so the runtime releases only its fresh
                            // reservations instead of leaking them behind a
                            // bare catalog-finalizer error.
                            return Err(CaptureCoordinatorError::CheckpointConflictFinalizer {
                                reason,
                                finalizer: Box::new(finalizer),
                            });
                        }
                    }
                }
                Ok(CaptureCoordinatorOutcome::CheckpointConflict { reason })
            }
            CheckpointWriteOutcome::PendingCleanup { checkpoint_id, .. } => {
                if let Some(binding) = finalizer_binding.as_ref() {
                    // `PendingCleanup` is returned only after the ref/CAS
                    // transaction (including companion claims) committed.
                    // A later finalizer catalog failure must not make the
                    // runtime abandon those durable claims. Keep the
                    // recovery outcome explicit and content-free; retry or
                    // doctor can revisit the finalizer independently.
                    match self
                        .persist_or_classify_finalizer(
                            &request.catalog,
                            binding,
                            FinalizeCheckpointProgress::PendingCleanup,
                        )
                        .await
                    {
                        Ok(Some(outcome)) => {
                            return Ok(CaptureCoordinatorOutcome::DurableFinalizer {
                                checkpoint_id,
                                cleanup_pending: true,
                                disposition: Self::durable_finalizer_disposition(outcome)?,
                            });
                        }
                        Ok(None) => {}
                        Err(_) => {
                            tracing::warn!(
                                target: "agent.capture.finalize",
                                replay_key = %request.catalog.action().action_key(),
                                finalizer_outcome = "pending_cleanup_finalizer_error",
                                finalizer_reason = "finalizer_update_failed",
                                "durable checkpoint cleanup remains pending after finalizer update failed"
                            );
                        }
                    }
                }
                Ok(CaptureCoordinatorOutcome::PendingCleanup { checkpoint_id })
            }
            CheckpointWriteOutcome::Written {
                commit_hash,
                marker_generation,
                cas_retries,
                object_count,
            } => {
                #[cfg(test)]
                if test_support::take_interrupt_after_checkpoint() {
                    return Err(CaptureCoordinatorError::InterruptedAfterCheckpoint);
                }
                let proof = self
                    .terminal_completion_proof(
                        &request.catalog,
                        finalizer_binding.as_ref(),
                        Some(&marker_generation),
                    )
                    .await
                    .map_err(
                        |cause| CaptureCoordinatorError::DurableCheckpointCompletion {
                            checkpoint_id: requested_checkpoint_id.clone(),
                            cause: Box::new(cause),
                        },
                    )?;
                let proof = match proof {
                    TerminalCompletionDecision::Nonterminal => None,
                    TerminalCompletionDecision::Proof(proof) => Some(proof),
                    // Return before `complete_if_pending`: no caller may
                    // reinterpret a sibling's completed receipt as a
                    // proof-less legacy terminal completion. The durable
                    // checkpoint needs distinct cleanup handling from an
                    // ordinary pre-write `AlreadyApplied` replay.
                    TerminalCompletionDecision::Classified(disposition) => {
                        return Ok(CaptureCoordinatorOutcome::DurableFinalizer {
                            checkpoint_id: requested_checkpoint_id,
                            cleanup_pending: false,
                            disposition,
                        });
                    }
                };
                self.complete_if_pending(&request.catalog, receipt, proof)
                    .await
                    .map_err(
                        |cause| CaptureCoordinatorError::DurableCheckpointCompletion {
                            checkpoint_id: requested_checkpoint_id.clone(),
                            cause: Box::new(cause),
                        },
                    )?;
                Ok(CaptureCoordinatorOutcome::CheckpointCommitted {
                    commit_hash,
                    cas_retries,
                    object_count,
                })
            }
            CheckpointWriteOutcome::AlreadyExists { checkpoint_id } => {
                #[cfg(test)]
                if test_support::take_interrupt_after_checkpoint() {
                    return Err(CaptureCoordinatorError::InterruptedAfterCheckpoint);
                }
                let proof = if finalizer_binding.is_some() {
                    let recovery = CaptureCatalogCompleteRequest::from_apply(&request.catalog)
                        .map_err(
                            |cause| CaptureCoordinatorError::CheckpointReplayCompletion {
                                checkpoint_id: checkpoint_id.clone(),
                                cause: Box::new(CaptureCoordinatorError::CatalogComplete(cause)),
                            },
                        )?;
                    match self
                        .catalog
                        .prove_durable_replay(&recovery)
                        .await
                        .map_err(
                            |cause| CaptureCoordinatorError::CheckpointReplayCompletion {
                                checkpoint_id: checkpoint_id.clone(),
                                cause: Box::new(CaptureCoordinatorError::CatalogFinalize(cause)),
                            },
                        )? {
                        CaptureCatalogFinalizeResult::ReadyToComplete { proof } => {
                            tracing::info!(
                                target: "agent.capture.finalize",
                                replay_key = %request.catalog.action().action_key(),
                                finalizer_outcome = "durable_replay",
                                "capture terminal finalizer recovered durable checkpoint"
                            );
                            Some(proof)
                        }
                        CaptureCatalogFinalizeResult::Quarantined { reason } => {
                            tracing::warn!(
                                target: "agent.capture.finalize",
                                replay_key = %request.catalog.action().action_key(),
                                finalizer_outcome = "quarantined",
                                finalizer_reason = ?reason,
                                "capture terminal replay requires repair"
                            );
                            return Ok(CaptureCoordinatorOutcome::FinalizerQuarantined { reason });
                        }
                        CaptureCatalogFinalizeResult::AlreadyComplete => {
                            tracing::info!(
                                target: "agent.capture.finalize",
                                replay_key = %request.catalog.action().action_key(),
                                finalizer_outcome = "already_complete",
                                "capture terminal replay observed completed receipt"
                            );
                            return Ok(CaptureCoordinatorOutcome::AlreadyApplied);
                        }
                        CaptureCatalogFinalizeResult::ConflictUnchanged { conflict } => {
                            tracing::warn!(
                                target: "agent.capture.finalize",
                                replay_key = %request.catalog.action().action_key(),
                                finalizer_outcome = "conflict",
                                finalizer_conflict = ?conflict,
                                "capture terminal replay lost its fence"
                            );
                            return Ok(CaptureCoordinatorOutcome::ConflictUnchanged { conflict });
                        }
                        CaptureCatalogFinalizeResult::Pending { .. } => {
                            return Err(CaptureCoordinatorError::CheckpointReplayCompletion {
                                checkpoint_id,
                                cause: Box::new(CaptureCoordinatorError::FinalizerNotDurable),
                            });
                        }
                    }
                } else {
                    None
                };
                self.complete_if_pending(&request.catalog, receipt, proof)
                    .await
                    .map_err(
                        |cause| CaptureCoordinatorError::CheckpointReplayCompletion {
                            checkpoint_id: checkpoint_id.clone(),
                            cause: Box::new(cause),
                        },
                    )?;
                Ok(CaptureCoordinatorOutcome::CheckpointAlreadyExists { checkpoint_id })
            }
        }
    }

    /// Persist a non-durable terminal attempt before returning a checkpoint
    /// error/outcome. A quarantine is an explicit result, never a success.
    async fn persist_or_classify_finalizer(
        &self,
        catalog: &CaptureCatalogApplyRequest,
        binding: &CoordinatorFinalizeBinding,
        progress: FinalizeCheckpointProgress,
    ) -> Result<Option<CaptureCoordinatorOutcome>, CaptureCoordinatorError> {
        let request = CaptureCatalogFinalizeRequest::from_apply(
            catalog,
            binding.policy.clone(),
            binding.marker_generation.clone(),
            binding.source_digest.clone(),
            binding.now_millis,
            progress,
        )
        .map_err(CaptureCoordinatorError::CatalogFinalize)?;
        let result = self
            .catalog
            .finalize(&request)
            .await
            .map_err(CaptureCoordinatorError::CatalogFinalize)?;
        match result {
            CaptureCatalogFinalizeResult::Pending { attempts, stage } => {
                tracing::info!(
                    target: "agent.capture.finalize",
                    replay_key = %catalog.action().action_key(),
                    finalizer_outcome = "pending",
                    finalizer_stage = ?stage,
                    finalizer_attempts = attempts,
                    "capture terminal finalizer remains pending"
                );
                Ok(None)
            }
            CaptureCatalogFinalizeResult::Quarantined { reason } => {
                tracing::warn!(
                    target: "agent.capture.finalize",
                    replay_key = %catalog.action().action_key(),
                    finalizer_outcome = "quarantined",
                    finalizer_reason = ?reason,
                    "capture terminal finalizer requires repair"
                );
                Ok(Some(CaptureCoordinatorOutcome::FinalizerQuarantined {
                    reason,
                }))
            }
            CaptureCatalogFinalizeResult::AlreadyComplete => {
                tracing::info!(
                    target: "agent.capture.finalize",
                    replay_key = %catalog.action().action_key(),
                    finalizer_outcome = "already_complete",
                    "capture terminal finalizer observed completed receipt"
                );
                Ok(Some(CaptureCoordinatorOutcome::AlreadyApplied))
            }
            CaptureCatalogFinalizeResult::ConflictUnchanged { conflict } => {
                tracing::warn!(
                    target: "agent.capture.finalize",
                    replay_key = %catalog.action().action_key(),
                    finalizer_outcome = "conflict",
                    finalizer_conflict = ?conflict,
                    "capture terminal finalizer lost its fence"
                );
                Ok(Some(CaptureCoordinatorOutcome::ConflictUnchanged {
                    conflict,
                }))
            }
            CaptureCatalogFinalizeResult::ReadyToComplete { .. } => {
                Err(CaptureCoordinatorError::FinalizerUnexpectedReady)
            }
        }
    }

    /// Reclassify a catalog-only finalizer disposition once the checkpoint
    /// store has already committed its ref/catalog transaction. This keeps
    /// runtime cleanup honest: an `AlreadyApplied` receipt or quarantine here
    /// is not the same as a pre-write no-op.
    fn durable_finalizer_disposition(
        outcome: CaptureCoordinatorOutcome,
    ) -> Result<DurableFinalizerDisposition, CaptureCoordinatorError> {
        match outcome {
            CaptureCoordinatorOutcome::AlreadyApplied => {
                Ok(DurableFinalizerDisposition::ReceiptAlreadyApplied)
            }
            CaptureCoordinatorOutcome::FinalizerQuarantined { reason } => {
                Ok(DurableFinalizerDisposition::Quarantined { reason })
            }
            CaptureCoordinatorOutcome::ConflictUnchanged { conflict } => {
                Ok(DurableFinalizerDisposition::Conflict { conflict })
            }
            _ => Err(CaptureCoordinatorError::UnexpectedDurableFinalizerOutcome),
        }
    }

    /// Return a strict completion proof only after the catalog has validated
    /// the same persisted marker/source fence under a durable checkpoint.
    async fn terminal_completion_proof(
        &self,
        catalog: &CaptureCatalogApplyRequest,
        binding: Option<&CoordinatorFinalizeBinding>,
        written_marker_generation: Option<&str>,
    ) -> Result<TerminalCompletionDecision, CaptureCoordinatorError> {
        let Some(binding) = binding else {
            return Ok(TerminalCompletionDecision::Nonterminal);
        };
        if let Some(written_marker_generation) = written_marker_generation
            && written_marker_generation != binding.marker_generation
        {
            if let Some(outcome) = self
                .persist_or_classify_finalizer(
                    catalog,
                    binding,
                    FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Marker),
                )
                .await?
            {
                return Ok(TerminalCompletionDecision::Classified(
                    Self::durable_finalizer_disposition(outcome)?,
                ));
            }
            return Err(CaptureCoordinatorError::CheckpointMarkerMismatch);
        }
        let request = CaptureCatalogFinalizeRequest::from_apply(
            catalog,
            binding.policy.clone(),
            binding.marker_generation.clone(),
            binding.source_digest.clone(),
            binding.now_millis,
            FinalizeCheckpointProgress::Durable,
        )
        .map_err(CaptureCoordinatorError::CatalogFinalize)?;
        let result = self
            .catalog
            .finalize(&request)
            .await
            .map_err(CaptureCoordinatorError::CatalogFinalize)?;
        match result {
            CaptureCatalogFinalizeResult::ReadyToComplete { proof } => {
                tracing::info!(
                    target: "agent.capture.finalize",
                    replay_key = %catalog.action().action_key(),
                    finalizer_outcome = "durable",
                    "capture terminal finalizer observed durable checkpoint"
                );
                Ok(TerminalCompletionDecision::Proof(proof))
            }
            CaptureCatalogFinalizeResult::Quarantined { reason } => {
                Ok(TerminalCompletionDecision::Classified(
                    DurableFinalizerDisposition::Quarantined { reason },
                ))
            }
            CaptureCatalogFinalizeResult::AlreadyComplete => {
                Ok(TerminalCompletionDecision::Classified(
                    DurableFinalizerDisposition::ReceiptAlreadyApplied,
                ))
            }
            CaptureCatalogFinalizeResult::ConflictUnchanged { conflict } => {
                Ok(TerminalCompletionDecision::Classified(
                    DurableFinalizerDisposition::Conflict { conflict },
                ))
            }
            CaptureCatalogFinalizeResult::Pending { .. } => {
                Err(CaptureCoordinatorError::FinalizerNotDurable)
            }
        }
    }

    async fn complete_if_pending(
        &self,
        catalog: &CaptureCatalogApplyRequest,
        receipt: CaptureReceiptDisposition,
        proof: Option<CaptureCatalogFinalizeProof>,
    ) -> Result<(), CaptureCoordinatorError> {
        if receipt != CaptureReceiptDisposition::Pending {
            return Ok(());
        }
        let completion = match (catalog.mutation().is_terminal(), proof) {
            // A terminal receipt is never allowed to fall back to the legacy
            // apply-only completion constructor. That path has no durable
            // marker/source proof and would let a stale replay publish
            // `stopped` after another writer took over the receipt.
            (true, Some(proof)) => CaptureCatalogCompleteRequest::from_finalizer(catalog, proof),
            (true, None) => return Err(CaptureCoordinatorError::FinalizerNotDurable),
            (false, _) => CaptureCatalogCompleteRequest::from_apply(catalog),
        }
        .map_err(CaptureCoordinatorError::CatalogComplete)?;
        match self
            .catalog
            .complete(&completion)
            .await
            .map_err(CaptureCoordinatorError::CatalogComplete)?
        {
            CaptureCatalogCompleteResult::Completed
            | CaptureCatalogCompleteResult::AlreadyComplete => Ok(()),
            CaptureCatalogCompleteResult::ConflictUnchanged { conflict } => {
                Err(CaptureCoordinatorError::CompletionConflict { conflict })
            }
        }
    }
}

fn finalizer_stage_for_checkpoint_error(error: &CheckpointStoreError) -> FinalizePendingStage {
    match error {
        CheckpointStoreError::IncompleteSnapshot { .. }
        | CheckpointStoreError::DeadlineExceeded => FinalizePendingStage::Snapshot,
        CheckpointStoreError::StoreFailure { stage } => match stage {
            crate::internal::ai::capture::checkpoint::CheckpointStoreStage::ScopeFence => {
                FinalizePendingStage::Checkpoint
            }
            crate::internal::ai::capture::checkpoint::CheckpointStoreStage::Marker => {
                FinalizePendingStage::Marker
            }
            crate::internal::ai::capture::checkpoint::CheckpointStoreStage::Cleanup => {
                FinalizePendingStage::Cleanup
            }
            crate::internal::ai::capture::checkpoint::CheckpointStoreStage::ObjectWrite
            | crate::internal::ai::capture::checkpoint::CheckpointStoreStage::CatalogTransaction => {
                FinalizePendingStage::Checkpoint
            }
        },
        CheckpointStoreError::InvalidRequest { .. }
        | CheckpointStoreError::ReplayPayloadMismatch => FinalizePendingStage::Checkpoint,
    }
}

/// Observable stage/outcome for entrypoints.  These values contain only
/// opaque IDs and typed conflict classes; they are safe for spans and host
/// response policy.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum CaptureCoordinatorOutcome {
    StateApplied,
    AlreadyApplied,
    ConflictUnchanged {
        conflict: CaptureCatalogConflict,
    },
    CheckpointCommitted {
        commit_hash: String,
        /// Content-free ref-CAS retry count propagated from the checkpoint
        /// port for the owning runtime span.
        cas_retries: u64,
        /// Content-free object count propagated from the checkpoint port for
        /// the owning runtime span.
        object_count: u64,
    },
    CheckpointAlreadyExists {
        checkpoint_id: String,
    },
    /// A sibling delivery owns the same persisted writer marker. The caller
    /// made no mutation and must leave finalizer retry state untouched.
    CheckpointInFlight {
        checkpoint_id: String,
    },
    CheckpointConflict {
        reason: crate::internal::ai::capture::checkpoint::CheckpointConflictReason,
    },
    PendingCleanup {
        checkpoint_id: String,
    },
    /// The checkpoint/ref transaction committed before the finalizer saw a
    /// sibling's completion, quarantine, or fence change. Callers must keep
    /// its durable coverage evidence and settle only transient export state.
    DurableFinalizer {
        checkpoint_id: String,
        cleanup_pending: bool,
        disposition: DurableFinalizerDisposition,
    },
    /// A terminal receipt was deliberately left pending because no checkpoint
    /// store could be constructed. Optional fields are absent only for a
    /// legacy pre-finalizer receipt that must not be rebound without a store.
    FinalizerPending {
        attempts: Option<u8>,
        stage: Option<FinalizePendingStage>,
    },
    /// The bounded finalizer exhausted its replay/deadline budget. The
    /// catalog session is repair-required, never falsely terminal.
    FinalizerQuarantined {
        reason: FinalizeQuarantineReason,
    },
}

#[derive(Debug, Error)]
pub(crate) enum CaptureCoordinatorError {
    #[error("capture coordinator received a checkpoint for a no-checkpoint lifecycle action")]
    UnexpectedCheckpoint,
    #[error("capture coordinator lifecycle action requires a checkpoint request")]
    MissingCheckpoint,
    #[error("capture coordinator checkpoint scope does not match the lifecycle action")]
    CheckpointScopeMismatch,
    #[error("capture coordinator checkpoint does not match its catalog replay receipt")]
    CheckpointReceiptMismatch,
    #[error("capture coordinator terminal lifecycle action requires a finalizer policy")]
    MissingTerminalFinalizer,
    #[error("capture coordinator finalizer policy does not match its catalog replay receipt")]
    FinalizerReceiptMismatch,
    #[error("capture coordinator received a finalizer for a nonterminal lifecycle action")]
    UnexpectedFinalizer,
    #[error("capture coordinator pre-reserved execution is missing its catalog reservation")]
    MissingPreappliedReservation,
    #[error("capture coordinator request does not match its pre-reserved catalog receipt")]
    PreappliedRequestMismatch,
    /// Raised only by the test-only one-step `execute` form; production
    /// reservations normalize an adopted action before any request exists.
    #[cfg(test)]
    #[error(
        "capture coordinator adopted an ID-less terminal action; reserve the catalog before constructing checkpoint and finalizer requests"
    )]
    AdoptedActionRequiresReservation,
    #[error("capture coordinator cannot complete a terminal receipt without a durable checkpoint")]
    TerminalCompletionWithoutCheckpoint,
    #[error("capture coordinator catalog apply failed: {0}")]
    CatalogApply(#[source] CaptureCatalogError),
    #[error("capture coordinator checkpoint write failed: {0}")]
    CheckpointWrite(#[source] CheckpointStoreError),
    #[error(
        "capture coordinator checkpoint write failed; terminal finalizer transition also failed: {finalizer}"
    )]
    CheckpointWriteFinalizer {
        #[source]
        checkpoint: CheckpointStoreError,
        finalizer: Box<CaptureCoordinatorError>,
    },
    #[error(
        "capture coordinator checkpoint was unchanged ({reason:?}); terminal finalizer transition also failed: {finalizer}"
    )]
    CheckpointConflictFinalizer {
        reason: crate::internal::ai::capture::checkpoint::CheckpointConflictReason,
        finalizer: Box<CaptureCoordinatorError>,
    },
    #[error(
        "capture coordinator checkpoint {checkpoint_id} is durable but receipt completion failed: {cause}"
    )]
    DurableCheckpointCompletion {
        checkpoint_id: String,
        cause: Box<CaptureCoordinatorError>,
    },
    #[error(
        "capture coordinator replay of existing checkpoint {checkpoint_id} could not finish its receipt: {cause}"
    )]
    CheckpointReplayCompletion {
        checkpoint_id: String,
        cause: Box<CaptureCoordinatorError>,
    },
    #[error("capture coordinator catalog completion failed: {0}")]
    CatalogComplete(#[source] CaptureCatalogError),
    #[error("capture coordinator catalog finalizer operation failed: {0}")]
    CatalogFinalize(#[source] CaptureCatalogError),
    #[error("capture coordinator finalizer was ready before a durable checkpoint")]
    FinalizerUnexpectedReady,
    #[error("capture coordinator terminal finalizer did not observe a durable checkpoint")]
    FinalizerNotDurable,
    #[error("capture coordinator received an invalid finalizer outcome after a durable checkpoint")]
    UnexpectedDurableFinalizerOutcome,
    #[error("capture coordinator checkpoint writer returned a different marker generation")]
    CheckpointMarkerMismatch,
    #[error("capture coordinator checkpoint committed but its catalog receipt lost a fence")]
    CompletionConflict { conflict: CaptureCatalogConflict },
    #[cfg(test)]
    #[error("capture checkpoint interrupted after durable write before catalog completion")]
    InterruptedAfterCheckpoint,
}

/// What a pre-reserved execution error says about this invocation's own
/// scope-fenced reservations (ADR-ACF-10, ACF-20).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CoordinatorReservationClass {
    /// The checkpoint store left this invocation without a durable
    /// checkpoint: release its fresh claim/export reservations. `diagnostic`
    /// marks a direct store failure that also records a retryable
    /// checkpoint diagnostic, even when persisting finalizer evidence failed.
    Uncommitted { diagnostic: bool },
    /// The checkpoint/ref transaction already committed its companion
    /// claims: keep them, and retire only the transient export lease so a
    /// replay or doctor can finish the receipt.
    PostCheckpoint,
    /// No reservation decision can be derived from the error.
    Unclassified,
}

impl CaptureCoordinatorError {
    /// The single exhaustive reservation classifier for an error returned by
    /// `execute_preapplied`. Every variant is named, so a new error cannot
    /// silently inherit a release decision.
    pub(crate) fn reservation_class(&self) -> CoordinatorReservationClass {
        match self {
            Self::CheckpointWrite(_) | Self::CheckpointWriteFinalizer { .. } => {
                CoordinatorReservationClass::Uncommitted { diagnostic: true }
            }
            Self::CheckpointConflictFinalizer { .. } | Self::CheckpointReplayCompletion { .. } => {
                CoordinatorReservationClass::Uncommitted { diagnostic: false }
            }
            Self::DurableCheckpointCompletion { .. } => CoordinatorReservationClass::PostCheckpoint,
            Self::UnexpectedCheckpoint
            | Self::MissingCheckpoint
            | Self::CheckpointScopeMismatch
            | Self::CheckpointReceiptMismatch
            | Self::MissingTerminalFinalizer
            | Self::FinalizerReceiptMismatch
            | Self::UnexpectedFinalizer
            | Self::MissingPreappliedReservation
            | Self::PreappliedRequestMismatch
            | Self::TerminalCompletionWithoutCheckpoint
            | Self::CatalogApply(_)
            | Self::CatalogComplete(_)
            | Self::CatalogFinalize(_)
            | Self::FinalizerUnexpectedReady
            | Self::FinalizerNotDurable
            | Self::UnexpectedDurableFinalizerOutcome
            | Self::CheckpointMarkerMismatch
            | Self::CompletionConflict { .. } => CoordinatorReservationClass::Unclassified,
            #[cfg(test)]
            Self::AdoptedActionRequiresReservation | Self::InterruptedAfterCheckpoint => {
                CoordinatorReservationClass::Unclassified
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use uuid::Uuid;

    use super::*;
    use crate::internal::ai::{
        capture::{
            catalog::{
                CaptureCatalogAction, CaptureCatalogError, CaptureCatalogMetadataPatch,
                CaptureCatalogMutation, CaptureCatalogSession, FakeCaptureCatalogStore,
            },
            checkpoint::{CheckpointRedactedPayload, CheckpointStoreStage},
            finalizer::{CaptureFinalizeMode, CaptureFinalizePolicy},
            snapshot::CaptureSnapshotService,
            state::{CapturePhase, DurableCaptureState, LifecycleActionPlan, StoppedAtMutation},
        },
        capture_scope::CaptureScope,
        observed_agents::{ExportAuthorized, Redactor, TranscriptSource},
    };

    #[derive(Clone)]
    struct FakeCatalog {
        apply: CaptureCatalogApplyResult,
        completed: Arc<Mutex<usize>>,
        finalize_result: Option<CaptureCatalogFinalizeResult>,
    }

    #[derive(Clone, Default)]
    struct CompletionConflictCatalog {
        inner: FakeCaptureCatalogStore,
        fail_next_completion: Arc<Mutex<bool>>,
    }

    impl CompletionConflictCatalog {
        fn fail_next_completion(&self) {
            *self
                .fail_next_completion
                .lock()
                .expect("completion fault lock") = true;
        }
    }

    #[async_trait]
    impl CaptureCatalogPort for FakeCatalog {
        async fn apply(
            &self,
            _: &CaptureCatalogApplyRequest,
        ) -> Result<CaptureCatalogApplyResult, CaptureCatalogError> {
            Ok(self.apply.clone())
        }

        async fn complete(
            &self,
            _: &CaptureCatalogCompleteRequest,
        ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
            let mut completed = self
                .completed
                .lock()
                .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
            *completed += 1;
            Ok(CaptureCatalogCompleteResult::Completed)
        }

        async fn finalize(
            &self,
            _: &crate::internal::ai::capture::catalog::CaptureCatalogFinalizeRequest,
        ) -> Result<
            crate::internal::ai::capture::catalog::CaptureCatalogFinalizeResult,
            CaptureCatalogError,
        > {
            self.finalize_result
                .clone()
                .ok_or(CaptureCatalogError::InvalidRequest)
        }

        async fn prove_durable_replay(
            &self,
            _: &CaptureCatalogCompleteRequest,
        ) -> Result<
            crate::internal::ai::capture::catalog::CaptureCatalogFinalizeResult,
            CaptureCatalogError,
        > {
            Err(CaptureCatalogError::InvalidRequest)
        }

        async fn update_diagnostic(
            &self,
            _: &CaptureCatalogApplyRequest,
            _: crate::internal::ai::capture::catalog::CaptureCatalogDiagnostic,
        ) -> Result<bool, CaptureCatalogError> {
            Err(CaptureCatalogError::InvalidRequest)
        }
    }

    #[async_trait]
    impl CaptureCatalogPort for CompletionConflictCatalog {
        async fn apply(
            &self,
            request: &CaptureCatalogApplyRequest,
        ) -> Result<CaptureCatalogApplyResult, CaptureCatalogError> {
            self.inner.apply(request).await
        }

        async fn complete(
            &self,
            request: &CaptureCatalogCompleteRequest,
        ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
            let fail_now = {
                let mut fail = self
                    .fail_next_completion
                    .lock()
                    .map_err(|_| CaptureCatalogError::FakeStoreUnavailable)?;
                let fail_now = *fail;
                *fail = false;
                fail_now
            };
            if fail_now {
                return Ok(CaptureCatalogCompleteResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ConditionalWrite,
                });
            }
            self.inner.complete(request).await
        }

        async fn finalize(
            &self,
            request: &crate::internal::ai::capture::catalog::CaptureCatalogFinalizeRequest,
        ) -> Result<
            crate::internal::ai::capture::catalog::CaptureCatalogFinalizeResult,
            CaptureCatalogError,
        > {
            self.inner.finalize(request).await
        }

        async fn prove_durable_replay(
            &self,
            request: &CaptureCatalogCompleteRequest,
        ) -> Result<
            crate::internal::ai::capture::catalog::CaptureCatalogFinalizeResult,
            CaptureCatalogError,
        > {
            self.inner.prove_durable_replay(request).await
        }

        async fn complete_durable_replay(
            &self,
            request: &CaptureCatalogApplyRequest,
        ) -> Result<CaptureCatalogCompleteResult, CaptureCatalogError> {
            self.inner.complete_durable_replay(request).await
        }

        async fn update_diagnostic(
            &self,
            request: &CaptureCatalogApplyRequest,
            diagnostic: crate::internal::ai::capture::catalog::CaptureCatalogDiagnostic,
        ) -> Result<bool, CaptureCatalogError> {
            self.inner.update_diagnostic(request, diagnostic).await
        }
    }

    #[derive(Clone)]
    struct FakeCheckpoint(CheckpointWriteOutcome);

    #[async_trait]
    impl CheckpointStore for FakeCheckpoint {
        async fn write(
            &self,
            _: CheckpointWriteRequest<'_>,
        ) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
            Ok(self.0.clone())
        }
    }

    struct FailingCheckpoint;

    #[async_trait]
    impl CheckpointStore for FailingCheckpoint {
        async fn write(
            &self,
            _: CheckpointWriteRequest<'_>,
        ) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
            Err(CheckpointStoreError::StoreFailure {
                stage: CheckpointStoreStage::ObjectWrite,
            })
        }
    }

    /// Records every sealed request the coordinator hands to the store and
    /// acknowledges it as freshly written under the request's own marker
    /// generation, as the concrete store does after a successful ref CAS.
    #[derive(Clone, Default)]
    struct RecordingCheckpoint {
        writes: Arc<Mutex<Vec<(CheckpointScope, String, String)>>>,
    }

    impl RecordingCheckpoint {
        fn writes(&self) -> Vec<(CheckpointScope, String, String)> {
            self.writes.lock().expect("recorded writes lock").clone()
        }
    }

    #[async_trait]
    impl CheckpointStore for RecordingCheckpoint {
        async fn write(
            &self,
            request: CheckpointWriteRequest<'_>,
        ) -> Result<CheckpointWriteOutcome, CheckpointStoreError> {
            let mut writes =
                self.writes
                    .lock()
                    .map_err(|_| CheckpointStoreError::InvalidRequest {
                        field: "recording checkpoint lock".to_string(),
                    })?;
            writes.push((
                request.scope(),
                request.checkpoint_id().to_string(),
                request.replay_key().to_string(),
            ));
            Ok(CheckpointWriteOutcome::Written {
                commit_hash: format!("opaque-commit-{}", writes.len()),
                marker_generation: request.marker_generation().to_string(),
                cas_retries: 0,
                object_count: 1,
            })
        }
    }

    /// Drive one sealed request through the production live protocol: an
    /// opaque reservation over the catalog port, then
    /// `into_coordinator(...).execute_preapplied`.
    async fn execute_through_reservation<S: CheckpointStore>(
        catalog: &FakeCaptureCatalogStore,
        checkpoint: S,
        request: CaptureCoordinatorRequest<'_>,
    ) -> Result<CaptureCoordinatorOutcome, CaptureCoordinatorError> {
        let reserved = reserve_capture_catalog(catalog, &request.catalog).await?;
        CaptureCoordinatorReservation::from_preapplied(
            catalog.clone(),
            request.catalog.clone(),
            reserved,
        )
        .into_coordinator(checkpoint)
        .execute_preapplied(request)
        .await
    }

    fn request<'a>(payload: &'a CheckpointRedactedPayload) -> CaptureCoordinatorRequest<'a> {
        const ACTION_KEY: &str = "capture-lifecycle-v1:80155d27-7f57-46a7-a102-9db203501201";
        let event_id = Uuid::from_u128(0x8015_5d27_7f57_46a7_a102_9db2_0350_1201);
        let action_plan = LifecycleActionPlan {
            // Keep the request's replay-key borrow independent from this
            // local reducer plan. Production entrypoints hold the ingress
            // action key for the whole coordinator call; the test mirrors
            // that ownership with a static opaque key.
            action_key: ACTION_KEY.to_string(),
            next_phase: CapturePhase::Active,
            stopped_at: StoppedAtMutation::Preserve,
            checkpoint: CheckpointWrite::Committed,
            expected_sync_revision: Some(7),
        };
        let catalog = CaptureCatalogApplyRequest::new(
            CaptureScope {
                repo_id: "repo".to_string(),
                worktree_id: String::new(),
                workspace_id: None,
                workspace_fence: None,
            },
            CaptureCatalogSession::new("session", "claude_code", "provider", "/repo")
                .expect("session"),
            CaptureCatalogAction::lifecycle(event_id, None),
            CaptureCatalogMutation::from_reducer(
                Some(DurableCaptureState {
                    phase: CapturePhase::Active,
                    stopped_at: None,
                    sync_revision: 7,
                }),
                &action_plan,
                1,
            )
            .expect("mutation"),
        )
        .expect("catalog request")
        .with_metadata(CaptureCatalogMetadataPatch::default());
        let checkpoint = CheckpointWriteRequest::new(
            ACTION_KEY,
            "checkpoint",
            "session",
            "claude_code",
            None,
            CheckpointScope::Committed,
            "marker",
            None,
            payload,
            None,
            None,
        )
        .expect("checkpoint request");
        CaptureCoordinatorRequest::new(catalog, Some(checkpoint)).expect("coordinator request")
    }

    fn terminal_request_with_marker<'a>(
        payload: &'a CheckpointRedactedPayload,
        marker_generation: &'a str,
    ) -> CaptureCoordinatorRequest<'a> {
        const ACTION_KEY: &str = "capture-lifecycle-v1:ec20f8ca-d232-437e-b4fe-331ac906e5cd";
        let event_id = Uuid::from_u128(0xec20_f8ca_d232_437e_b4fe_331a_c906_e5cd);
        let action_plan = LifecycleActionPlan {
            action_key: ACTION_KEY.to_string(),
            next_phase: CapturePhase::Stopped,
            stopped_at: StoppedAtMutation::Set(5),
            checkpoint: CheckpointWrite::Committed,
            expected_sync_revision: None,
        };
        let catalog = CaptureCatalogApplyRequest::new(
            CaptureScope {
                repo_id: "repo".to_string(),
                worktree_id: String::new(),
                workspace_id: None,
                workspace_fence: None,
            },
            CaptureCatalogSession::new("terminal", "claude_code", "terminal-provider", "/repo")
                .expect("session"),
            CaptureCatalogAction::lifecycle(event_id, None),
            CaptureCatalogMutation::from_reducer(None, &action_plan, 5).expect("terminal mutation"),
        )
        .expect("terminal catalog request");
        let checkpoint = CheckpointWriteRequest::new(
            ACTION_KEY,
            "terminal-checkpoint",
            "terminal",
            "claude_code",
            None,
            CheckpointScope::Committed,
            marker_generation,
            None,
            payload,
            None,
            None,
        )
        .expect("terminal checkpoint request");
        let finalizer = CaptureCoordinatorFinalizer::new(
            CaptureFinalizePolicy::new(Some(1_000), CaptureFinalizeMode::Deferrable, ACTION_KEY)
                .expect("terminal policy"),
            1,
        );
        CaptureCoordinatorRequest::with_finalizer(catalog, Some(checkpoint), finalizer)
            .expect("terminal coordinator request")
    }

    fn terminal_request<'a>(
        payload: &'a CheckpointRedactedPayload,
    ) -> CaptureCoordinatorRequest<'a> {
        terminal_request_with_marker(payload, "marker")
    }

    fn claimed_terminal_request<'a>(
        payload: &'a CheckpointRedactedPayload,
    ) -> CaptureCoordinatorRequest<'a> {
        let request = terminal_request(payload);
        let finalizer = request
            .finalizer
            .clone()
            .expect("terminal request carries finalizer");
        CaptureCoordinatorRequest::with_claimed_finalizer(
            request.catalog,
            request.checkpoint,
            finalizer,
        )
        .expect("claimed terminal coordinator request")
    }

    fn payload() -> CheckpointRedactedPayload {
        let raw = b"assistant text".to_vec();
        let auth = ExportAuthorized::issue("claude_code", "session", &raw);
        let mut snapshot = CaptureSnapshotService::capture_authorized(
            TranscriptSource::Bytes { bytes: raw, auth },
            "claude_code",
            "session",
            Default::default(),
        );
        // Terminal-finalizer tests exercise the same durable boundary as the
        // runtime. A test-only payload still needs a syntactically valid
        // repository-scoped commitment rather than the helper's transient
        // redacted-content checksum.
        assert!(snapshot.bind_source_commitment(format!("source/hmac-v2/{}", "a".repeat(64))));
        let redactor = Redactor::new_default();
        CheckpointRedactedPayload::from_snapshot(
            snapshot,
            redactor.redact(b"{}").0,
            redactor.redact(b"{}\n").0,
            redactor.redact(b"{}").0,
        )
        .expect("payload")
    }

    #[tokio::test]
    async fn durable_checkpoint_completes_its_pending_catalog_receipt_once() {
        let payload = payload();
        let completed = Arc::new(Mutex::new(0));
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 8,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::clone(&completed),
                finalize_result: None,
            },
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "opaque-commit".to_string(),
                marker_generation: "marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );

        let outcome = coordinator
            .execute(request(&payload))
            .await
            .expect("execute");
        assert_eq!(
            outcome,
            CaptureCoordinatorOutcome::CheckpointCommitted {
                commit_hash: "opaque-commit".to_string(),
                cas_retries: 0,
                object_count: 1,
            }
        );
        assert_eq!(*completed.lock().expect("completed lock"), 1);
    }

    #[test]
    fn reservation_rebinds_an_adopted_idless_terminal_action_once() {
        let payload = payload();
        let original = request(&payload).catalog;
        let adopted = CaptureCatalogAction::lifecycle(Uuid::from_u128(0xfeed), None);
        let reservation = CaptureCoordinatorReservation::from_preapplied(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::AlreadyApplied,
                completed: Arc::new(Mutex::new(0)),
                finalize_result: None,
            },
            original,
            CaptureCatalogApplyResult::ResumePending {
                state: DurableCaptureState {
                    phase: CapturePhase::Pending,
                    stopped_at: None,
                    sync_revision: 1,
                },
                checkpoint: CheckpointWrite::Committed,
                terminal_finalizer_needs_binding: true,
                terminal_marker_generation: None,
                adopted_action: Some(adopted.clone()),
            },
        );
        assert_eq!(reservation.effective_request().action(), &adopted);
    }

    #[tokio::test]
    async fn one_step_coordinator_refuses_an_adopted_action_before_writing_a_new_receipt() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::ResumePending {
                    state: DurableCaptureState {
                        phase: CapturePhase::Pending,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    terminal_finalizer_needs_binding: true,
                    terminal_marker_generation: None,
                    adopted_action: Some(CaptureCatalogAction::lifecycle(
                        Uuid::from_u128(0xbeef),
                        None,
                    )),
                },
                completed: Arc::new(Mutex::new(0)),
                finalize_result: None,
            },
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "must-not-write".to_string(),
                marker_generation: "marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );
        assert!(matches!(
            coordinator.execute(request(&payload)).await,
            Err(CaptureCoordinatorError::AdoptedActionRequiresReservation)
        ));
    }

    #[tokio::test]
    async fn no_checkpoint_noop_completes_a_pre_reserved_nonterminal_receipt() {
        let payload = payload();
        let request = request(&payload);
        let completed = Arc::new(Mutex::new(0));
        let reservation = CaptureCoordinatorReservation::from_preapplied(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 8,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::clone(&completed),
                finalize_result: None,
            },
            request.catalog.clone(),
            CaptureCatalogApplyResult::Applied {
                state: DurableCaptureState {
                    phase: CapturePhase::Active,
                    stopped_at: None,
                    sync_revision: 8,
                },
                checkpoint: CheckpointWrite::Committed,
                receipt: CaptureReceiptDisposition::Pending,
            },
        );

        assert_eq!(
            reservation
                .complete_preapplied_without_checkpoint(&request.catalog)
                .await
                .expect("nonterminal no-op settlement"),
            CaptureCoordinatorOutcome::StateApplied
        );
        assert_eq!(*completed.lock().expect("completed lock"), 1);
    }

    #[tokio::test]
    async fn no_checkpoint_noop_cannot_complete_a_pre_reserved_terminal_receipt() {
        let payload = payload();
        let request = terminal_request(&payload);
        let completed = Arc::new(Mutex::new(0));
        let reservation = CaptureCoordinatorReservation::from_preapplied(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Pending,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::clone(&completed),
                finalize_result: None,
            },
            request.catalog.clone(),
            CaptureCatalogApplyResult::Applied {
                state: DurableCaptureState {
                    phase: CapturePhase::Pending,
                    stopped_at: None,
                    sync_revision: 1,
                },
                checkpoint: CheckpointWrite::Committed,
                receipt: CaptureReceiptDisposition::Pending,
            },
        );

        assert!(matches!(
            reservation
                .complete_preapplied_without_checkpoint(&request.catalog)
                .await,
            Err(CaptureCoordinatorError::TerminalCompletionWithoutCheckpoint)
        ));
        assert_eq!(*completed.lock().expect("completed lock"), 0);
    }

    /// Neither an unchanged-checkpoint conflict nor a durable write whose
    /// marker cleanup is still pending may publish a terminal receipt. The
    /// in-memory catalog proves it through its durable ledger rather than a
    /// call counter: the identical delivery must re-enter as `ResumePending`
    /// on the unpublished revision with the bound marker fence, never as
    /// `AlreadyApplied`.
    #[tokio::test]
    async fn conflict_or_pending_cleanup_never_completes_terminal_receipt() {
        use crate::internal::ai::capture::checkpoint::CheckpointConflictReason;

        let payload = payload();
        for (label, store_outcome, expected) in [
            (
                "pending-cleanup",
                CheckpointWriteOutcome::PendingCleanup {
                    checkpoint_id: "terminal-checkpoint".to_string(),
                    marker_generation: "marker".to_string(),
                },
                CaptureCoordinatorOutcome::PendingCleanup {
                    checkpoint_id: "terminal-checkpoint".to_string(),
                },
            ),
            (
                "conflict-unchanged",
                CheckpointWriteOutcome::ConflictUnchanged {
                    reason: CheckpointConflictReason::ScopeFence,
                },
                CaptureCoordinatorOutcome::CheckpointConflict {
                    reason: CheckpointConflictReason::ScopeFence,
                },
            ),
        ] {
            let catalog = FakeCaptureCatalogStore::default();
            let terminal = terminal_request(&payload);
            assert!(
                terminal.catalog.mutation().is_terminal(),
                "{label}: the fixture must exercise a terminal receipt"
            );
            assert_eq!(
                execute_through_reservation(&catalog, FakeCheckpoint(store_outcome), terminal)
                    .await
                    .expect("a typed store outcome is not a coordinator error"),
                expected,
                "{label}"
            );

            let replay = reserve_capture_catalog(&catalog, &terminal_request(&payload).catalog)
                .await
                .expect("replay reservation");
            assert_eq!(
                replay,
                CaptureCatalogApplyResult::ResumePending {
                    state: DurableCaptureState {
                        phase: CapturePhase::Pending,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    terminal_finalizer_needs_binding: false,
                    terminal_marker_generation: Some("marker".to_string()),
                    adopted_action: None,
                },
                "{label}: the terminal receipt must stay pending and unpublished"
            );
        }
    }

    #[tokio::test]
    async fn terminal_coordinator_persists_finalizer_then_uses_strict_completion_proof() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCaptureCatalogStore::default(),
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "opaque-terminal-commit".to_string(),
                marker_generation: "marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );
        assert_eq!(
            coordinator
                .execute(terminal_request(&payload))
                .await
                .expect("strict terminal coordinator execution"),
            CaptureCoordinatorOutcome::CheckpointCommitted {
                commit_hash: "opaque-terminal-commit".to_string(),
                cas_retries: 0,
                object_count: 1,
            }
        );
    }

    #[tokio::test]
    async fn checkpoint_write_error_remains_causal_when_finalizer_persist_fails() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::new(Mutex::new(0)),
                // `None` makes the fake finalizer return its typed catalog
                // error after the checkpoint store has failed.
                finalize_result: None,
            },
            FailingCheckpoint,
        );

        let error = coordinator
            .execute(claimed_terminal_request(&payload))
            .await
            .expect_err("checkpoint failure must not be masked by finalizer failure");
        assert_eq!(
            error.reservation_class(),
            CoordinatorReservationClass::Uncommitted { diagnostic: true },
            "runtime must still release its uncommitted scoped reservations"
        );
        assert!(matches!(
            error,
            CaptureCoordinatorError::CheckpointWriteFinalizer {
                checkpoint: CheckpointStoreError::StoreFailure {
                    stage: CheckpointStoreStage::ObjectWrite,
                },
                finalizer,
            } if matches!(
                finalizer.as_ref(),
                CaptureCoordinatorError::CatalogFinalize(CaptureCatalogError::InvalidRequest)
            )
        ));
    }

    #[tokio::test]
    async fn checkpoint_conflict_remains_uncommitted_when_finalizer_persist_fails() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::new(Mutex::new(0)),
                finalize_result: None,
            },
            FakeCheckpoint(CheckpointWriteOutcome::ConflictUnchanged {
                reason:
                    crate::internal::ai::capture::checkpoint::CheckpointConflictReason::ScopeFence,
            }),
        );

        let error = coordinator
            .execute(claimed_terminal_request(&payload))
            .await
            .expect_err("unchanged checkpoint conflict must retain its causal classification");
        assert_eq!(
            error.reservation_class(),
            CoordinatorReservationClass::Uncommitted { diagnostic: false },
            "runtime must release fresh scoped reservations after an unchanged conflict"
        );
        assert!(matches!(
            error,
            CaptureCoordinatorError::CheckpointConflictFinalizer {
                reason: crate::internal::ai::capture::checkpoint::CheckpointConflictReason::ScopeFence,
                finalizer,
            } if matches!(
                finalizer.as_ref(),
                CaptureCoordinatorError::CatalogFinalize(CaptureCatalogError::InvalidRequest)
            )
        ));
    }

    #[tokio::test]
    async fn pending_cleanup_keeps_durable_outcome_when_finalizer_persist_fails() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::new(Mutex::new(0)),
                finalize_result: None,
            },
            FakeCheckpoint(CheckpointWriteOutcome::PendingCleanup {
                checkpoint_id: "terminal-checkpoint".to_string(),
                marker_generation: "marker".to_string(),
            }),
        );

        assert_eq!(
            coordinator
                .execute(claimed_terminal_request(&payload))
                .await
                .expect("durable cleanup must not be recast as checkpoint failure"),
            CaptureCoordinatorOutcome::PendingCleanup {
                checkpoint_id: "terminal-checkpoint".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn claimed_terminal_pending_cleanup_propagates_concurrent_receipt_completion() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::new(Mutex::new(0)),
                finalize_result: Some(CaptureCatalogFinalizeResult::AlreadyComplete),
            },
            FakeCheckpoint(CheckpointWriteOutcome::PendingCleanup {
                checkpoint_id: "terminal-checkpoint".to_string(),
                marker_generation: "marker".to_string(),
            }),
        );

        assert_eq!(
            coordinator
                .execute(claimed_terminal_request(&payload))
                .await
                .expect("already-complete receipt remains a durable no-op"),
            CaptureCoordinatorOutcome::DurableFinalizer {
                checkpoint_id: "terminal-checkpoint".to_string(),
                cleanup_pending: true,
                disposition: DurableFinalizerDisposition::ReceiptAlreadyApplied,
            }
        );
    }

    #[tokio::test]
    async fn claimed_terminal_pending_cleanup_propagates_durable_quarantine() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::new(Mutex::new(0)),
                finalize_result: Some(CaptureCatalogFinalizeResult::Quarantined {
                    reason: FinalizeQuarantineReason::SourceDigestConflict,
                }),
            },
            FakeCheckpoint(CheckpointWriteOutcome::PendingCleanup {
                checkpoint_id: "terminal-checkpoint".to_string(),
                marker_generation: "marker".to_string(),
            }),
        );

        assert_eq!(
            coordinator
                .execute(claimed_terminal_request(&payload))
                .await
                .expect("durable quarantine remains explicit"),
            CaptureCoordinatorOutcome::DurableFinalizer {
                checkpoint_id: "terminal-checkpoint".to_string(),
                cleanup_pending: true,
                disposition: DurableFinalizerDisposition::Quarantined {
                    reason: FinalizeQuarantineReason::SourceDigestConflict,
                },
            }
        );
    }

    #[tokio::test]
    async fn terminal_never_falls_back_to_legacy_complete_without_durable_proof() {
        let payload = payload();
        let completed = Arc::new(Mutex::new(0));
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Stopped,
                        stopped_at: Some(1),
                        sync_revision: 8,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::clone(&completed),
                // A terminal finalizer that does not return this attempt's
                // strict proof must never trigger legacy `from_apply`
                // completion, even if another writer reports completion.
                finalize_result: Some(CaptureCatalogFinalizeResult::AlreadyComplete),
            },
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "opaque-terminal-commit".to_string(),
                marker_generation: "marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );

        assert_eq!(
            coordinator
                .execute(terminal_request(&payload))
                .await
                .expect("already-complete finalizer is a safe no-op"),
            CaptureCoordinatorOutcome::AlreadyApplied
        );
        assert_eq!(
            *completed.lock().expect("completed lock"),
            0,
            "terminal completion must require CaptureCatalogCompleteRequest::from_finalizer"
        );
    }

    #[tokio::test]
    async fn claimed_terminal_written_checkpoint_observes_concurrent_receipt_completion() {
        let payload = payload();
        let completed = Arc::new(Mutex::new(0));
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::clone(&completed),
                // The request has already claimed its finalizer, so this
                // result is observed only after the fake checkpoint reports
                // Written. It models a sibling completing the receipt in
                // that narrow interval.
                finalize_result: Some(CaptureCatalogFinalizeResult::AlreadyComplete),
            },
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "opaque-terminal-commit".to_string(),
                marker_generation: "marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );

        assert_eq!(
            coordinator
                .execute(claimed_terminal_request(&payload))
                .await
                .expect("concurrently completed terminal receipt is a no-op"),
            CaptureCoordinatorOutcome::DurableFinalizer {
                checkpoint_id: "terminal-checkpoint".to_string(),
                cleanup_pending: false,
                disposition: DurableFinalizerDisposition::ReceiptAlreadyApplied,
            }
        );
        assert_eq!(
            *completed.lock().expect("completed lock"),
            0,
            "the already-complete outcome must return before legacy completion"
        );
    }

    #[tokio::test]
    async fn claimed_terminal_marker_mismatch_propagates_concurrent_receipt_completion() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCatalog {
                apply: CaptureCatalogApplyResult::Applied {
                    state: DurableCaptureState {
                        phase: CapturePhase::Active,
                        stopped_at: None,
                        sync_revision: 1,
                    },
                    checkpoint: CheckpointWrite::Committed,
                    receipt: CaptureReceiptDisposition::Pending,
                },
                completed: Arc::new(Mutex::new(0)),
                finalize_result: Some(CaptureCatalogFinalizeResult::AlreadyComplete),
            },
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "opaque-terminal-commit".to_string(),
                marker_generation: "different-marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );

        assert_eq!(
            coordinator
                .execute(claimed_terminal_request(&payload))
                .await
                .expect("completed receipt wins over the stale marker mismatch"),
            CaptureCoordinatorOutcome::DurableFinalizer {
                checkpoint_id: "terminal-checkpoint".to_string(),
                cleanup_pending: false,
                disposition: DurableFinalizerDisposition::ReceiptAlreadyApplied,
            }
        );
    }

    #[tokio::test]
    async fn durable_replay_uses_the_persisted_marker_proof_without_takeover() {
        let payload = payload();
        let catalog = CompletionConflictCatalog::default();
        // Simulate the crash window after the durable checkpoint/finalizer
        // decision but before catalog completion. The next writer receives a
        // new marker, so it can only finish by recovering the old proof.
        let first_request = terminal_request_with_marker(&payload, "marker");
        let reservation = reserve_capture_catalog(&catalog, &first_request.catalog)
            .await
            .expect("reserve terminal receipt");
        catalog.fail_next_completion();
        let first = CaptureCoordinator::new(
            catalog.clone(),
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "opaque-terminal-commit".to_string(),
                marker_generation: "marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );
        let first_result = first
            .execute_with_preapplied_result(first_request, reservation)
            .await;
        assert!(
            matches!(
                &first_result,
                Err(CaptureCoordinatorError::DurableCheckpointCompletion {
                    cause,
                    ..
                }) if matches!(
                    cause.as_ref(),
                    CaptureCoordinatorError::CompletionConflict { .. }
                )
            ),
            "the injected completion conflict must remain visible, got {first_result:?}"
        );

        let replay = CaptureCoordinator::new(
            catalog,
            FakeCheckpoint(CheckpointWriteOutcome::AlreadyExists {
                checkpoint_id: "terminal-checkpoint".to_string(),
            }),
        );
        assert_eq!(
            replay
                .execute(terminal_request_with_marker(&payload, "fresh-marker"))
                .await
                .expect("durable replay must reuse persisted proof"),
            CaptureCoordinatorOutcome::CheckpointAlreadyExists {
                checkpoint_id: "terminal-checkpoint".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn typed_fault_interrupts_only_between_durable_write_and_completion() {
        let payload = payload();
        let coordinator = CaptureCoordinator::new(
            FakeCaptureCatalogStore::default(),
            FakeCheckpoint(CheckpointWriteOutcome::Written {
                commit_hash: "opaque-terminal-commit".to_string(),
                marker_generation: "marker".to_string(),
                cas_retries: 0,
                object_count: 1,
            }),
        );
        test_support::interrupt_after_checkpoint_once();
        assert!(matches!(
            coordinator.execute(terminal_request(&payload)).await,
            Err(CaptureCoordinatorError::InterruptedAfterCheckpoint)
        ));
    }

    #[test]
    fn terminal_request_cannot_omit_its_finalizer_policy() {
        let payload = payload();
        let terminal = terminal_request(&payload);
        let finalizer = terminal
            .finalizer
            .clone()
            .expect("terminal fixture carries a finalizer");
        assert!(matches!(
            CaptureCoordinatorRequest::new(terminal.catalog, terminal.checkpoint),
            Err(CaptureCoordinatorError::MissingTerminalFinalizer)
        ));

        let nonterminal = request(&payload);
        let foreign_policy = CaptureCoordinatorFinalizer::new(
            CaptureFinalizePolicy::new(
                Some(1_000),
                CaptureFinalizeMode::Deferrable,
                nonterminal.catalog.action().action_key(),
            )
            .expect("foreign policy"),
            1,
        );
        let terminal = terminal_request(&payload);
        assert!(matches!(
            CaptureCoordinatorRequest::with_claimed_finalizer(
                terminal.catalog,
                terminal.checkpoint,
                foreign_policy,
            ),
            Err(CaptureCoordinatorError::FinalizerReceiptMismatch)
        ));
        assert!(matches!(
            CaptureCoordinatorRequest::with_claimed_finalizer(
                nonterminal.catalog,
                nonterminal.checkpoint,
                finalizer,
            ),
            Err(CaptureCoordinatorError::UnexpectedFinalizer)
        ));
    }

    /// Result of one lifecycle delivery through [`deliver_lifecycle`].
    struct LifecycleDelivery {
        reserved: CaptureCatalogApplyResult,
        terminal: bool,
        /// `None` when the reservation itself proved the receipt complete;
        /// the live reservation factory never builds a coordinator then.
        outcome: Option<CaptureCoordinatorOutcome>,
    }

    const MATRIX_OCCURRED_AT: i64 = 1_700_000_000;

    /// Reduce one canonical lifecycle event over `current` and drive it
    /// through the production protocol: reserve → (terminal only:
    /// provisional finalizer, then the atomic marker/source election) →
    /// sealed request → `into_coordinator(...).execute_preapplied`.
    async fn deliver_lifecycle(
        catalog: &FakeCaptureCatalogStore,
        store: &RecordingCheckpoint,
        payload: &CheckpointRedactedPayload,
        current: Option<DurableCaptureState>,
        kind: crate::internal::ai::hooks::LifecycleEventKind,
        event: u128,
    ) -> LifecycleDelivery {
        use crate::internal::ai::capture::{
            checkpoint::checkpoint_id_for_capture_action,
            state::{LifecycleReducerInput, reduce_lifecycle},
        };

        let event_id = Uuid::from_u128(event);
        let plan = reduce_lifecycle(LifecycleReducerInput {
            current,
            event_kind: kind,
            event_id,
            occurred_at: MATRIX_OCCURRED_AT,
            deadline: None,
        })
        .expect("matrix event is reducible");
        let receipt_key = format!("capture-dedup-v2:{event:064x}");
        let catalog_request = CaptureCatalogApplyRequest::new(
            CaptureScope {
                repo_id: "repo".to_string(),
                worktree_id: String::new(),
                workspace_id: None,
                workspace_fence: None,
            },
            CaptureCatalogSession::new("matrix", "matrix_agent", "matrix-provider", "/repo")
                .expect("matrix session"),
            CaptureCatalogAction::from_ingress(event_id, Some(receipt_key.as_str()), kind)
                .expect("matrix action"),
            CaptureCatalogMutation::from_reducer(current, &plan, MATRIX_OCCURRED_AT)
                .expect("matrix mutation"),
        )
        .expect("matrix catalog request");
        let terminal = catalog_request.mutation().is_terminal();
        let reserved = reserve_capture_catalog(catalog, &catalog_request)
            .await
            .expect("matrix reservation");
        if matches!(
            reserved,
            CaptureCatalogApplyResult::AlreadyApplied
                | CaptureCatalogApplyResult::ConflictUnchanged { .. }
        ) {
            return LifecycleDelivery {
                reserved,
                terminal,
                outcome: None,
            };
        }
        let reservation = CaptureCoordinatorReservation::from_preapplied(
            catalog.clone(),
            catalog_request,
            reserved.clone(),
        );
        let request = reservation.effective_request().clone();
        let checkpoint_kind = reservation.checkpoint_kind();
        let replay_key = request.action().action_key().to_string();
        let checkpoint_id =
            checkpoint_id_for_capture_action(request.action().event_id(), checkpoint_kind);
        let mut marker_generation = "matrix-marker".to_string();
        let finalizer = if terminal {
            let finalizer = CaptureCoordinatorFinalizer::new(
                CaptureFinalizePolicy::new(
                    Some(1_000),
                    CaptureFinalizeMode::Deferrable,
                    replay_key.clone(),
                )
                .expect("matrix terminal policy"),
                1,
            );
            assert!(matches!(
                reservation
                    .prepare_terminal_finalizer(&request, finalizer.clone())
                    .await
                    .expect("provisional terminal finalizer"),
                CaptureCoordinatorOutcome::FinalizerPending { .. }
            ));
            let source_digest = payload
                .snapshot()
                .source
                .as_ref()
                .and_then(|source| source.digest_sha256.clone());
            match reservation
                .claim_terminal_checkpoint_attempt(
                    &request,
                    finalizer.clone(),
                    "matrix-terminal-marker".to_string(),
                    source_digest,
                )
                .await
                .expect("terminal marker election")
            {
                CaptureCatalogTerminalAttempt::Bound {
                    marker_generation: elected,
                    ..
                } => marker_generation = elected,
                other => panic!("a fresh terminal receipt must elect its marker, got {other:?}"),
            }
            Some(finalizer)
        } else {
            None
        };
        let checkpoint_scope = match checkpoint_kind {
            CheckpointWrite::SubagentBoundary => CheckpointScope::Subagent,
            CheckpointWrite::None | CheckpointWrite::Committed => CheckpointScope::Committed,
        };
        let checkpoint = (checkpoint_kind != CheckpointWrite::None).then(|| {
            CheckpointWriteRequest::new(
                &replay_key,
                &checkpoint_id,
                "matrix",
                "matrix_agent",
                None,
                checkpoint_scope,
                &marker_generation,
                None,
                payload,
                None,
                None,
            )
            .expect("matrix checkpoint request")
        });
        let coordinator_request = match finalizer {
            Some(finalizer) => {
                CaptureCoordinatorRequest::with_claimed_finalizer(request, checkpoint, finalizer)
            }
            None => CaptureCoordinatorRequest::new(request, checkpoint),
        }
        .expect("matrix coordinator request");
        let outcome = reservation
            .into_coordinator(store.clone())
            .execute_preapplied(coordinator_request)
            .await
            .expect("matrix coordinator execution");
        LifecycleDelivery {
            reserved,
            terminal,
            outcome: Some(outcome),
        }
    }

    /// ACF-06 AC6: the SessionStart/TurnStart/ToolUse/Compaction/TurnEnd/
    /// Subagent/SessionEnd matrix runs end to end over fake ports through
    /// the production reservation → `execute_preapplied` handoff. Each row
    /// pins the reducer's checkpoint class and terminality, the number and
    /// scope of store writes, receipt completion, the exact durable state
    /// (proved by the catalog's expected-state fence), and a replay that
    /// neither writes nor mutates again.
    #[tokio::test]
    async fn lifecycle_matrix_runs_through_fake_ports_and_production_handoff() {
        use crate::internal::ai::{
            capture::checkpoint::checkpoint_id_for_capture_action, hooks::LifecycleEventKind,
        };

        let payload = payload();
        let seeded = DurableCaptureState {
            phase: CapturePhase::Active,
            stopped_at: None,
            sync_revision: 1,
        };
        // (event, checkpoint class, terminal, durable state after completion)
        let rows = [
            (
                LifecycleEventKind::SessionStart,
                CheckpointWrite::None,
                false,
                CapturePhase::Active,
            ),
            (
                LifecycleEventKind::TurnStart,
                CheckpointWrite::None,
                false,
                CapturePhase::Active,
            ),
            (
                LifecycleEventKind::ToolUse,
                CheckpointWrite::None,
                false,
                CapturePhase::Active,
            ),
            (
                LifecycleEventKind::Compaction,
                CheckpointWrite::None,
                false,
                CapturePhase::Condensed,
            ),
            (
                LifecycleEventKind::TurnEnd,
                CheckpointWrite::Committed,
                false,
                CapturePhase::Active,
            ),
            (
                LifecycleEventKind::SubagentStart,
                CheckpointWrite::SubagentBoundary,
                false,
                CapturePhase::Active,
            ),
            (
                LifecycleEventKind::SubagentEnd,
                CheckpointWrite::SubagentBoundary,
                false,
                CapturePhase::Active,
            ),
            (
                LifecycleEventKind::SessionEnd,
                CheckpointWrite::Committed,
                true,
                CapturePhase::Stopped,
            ),
        ];
        for (index, (kind, checkpoint_class, terminal, phase_after)) in rows.into_iter().enumerate()
        {
            let row = u128::try_from(index).expect("small matrix index");
            let catalog = FakeCaptureCatalogStore::default();
            let store = RecordingCheckpoint::default();

            let seed = deliver_lifecycle(
                &catalog,
                &store,
                &payload,
                None,
                LifecycleEventKind::SessionStart,
                0x5eed_0000 + row,
            )
            .await;
            assert_eq!(
                seed.reserved,
                CaptureCatalogApplyResult::Applied {
                    state: seeded,
                    checkpoint: CheckpointWrite::None,
                    receipt: CaptureReceiptDisposition::Complete,
                },
                "{kind}: seed"
            );

            let event = 0xacf0_6000 + row;
            let delivered =
                deliver_lifecycle(&catalog, &store, &payload, Some(seeded), kind, event).await;
            assert_eq!(delivered.terminal, terminal, "{kind}: terminality");
            let reserved_state = DurableCaptureState {
                // A terminal reservation keeps the prior phase until a durable
                // checkpoint completes its receipt.
                phase: if terminal {
                    CapturePhase::Active
                } else {
                    phase_after
                },
                stopped_at: None,
                sync_revision: 2,
            };
            assert_eq!(
                delivered.reserved,
                CaptureCatalogApplyResult::Applied {
                    state: reserved_state,
                    checkpoint: checkpoint_class,
                    receipt: if checkpoint_class == CheckpointWrite::None {
                        CaptureReceiptDisposition::Complete
                    } else {
                        CaptureReceiptDisposition::Pending
                    },
                },
                "{kind}: reservation"
            );
            let event_id = Uuid::from_u128(event);
            let expected_writes = match checkpoint_class {
                CheckpointWrite::None => Vec::new(),
                CheckpointWrite::Committed | CheckpointWrite::SubagentBoundary => vec![(
                    if checkpoint_class == CheckpointWrite::SubagentBoundary {
                        CheckpointScope::Subagent
                    } else {
                        CheckpointScope::Committed
                    },
                    checkpoint_id_for_capture_action(event_id, checkpoint_class),
                    format!("capture-lifecycle-v1:{event_id}"),
                )],
            };
            assert_eq!(
                delivered.outcome,
                Some(if checkpoint_class == CheckpointWrite::None {
                    CaptureCoordinatorOutcome::StateApplied
                } else {
                    CaptureCoordinatorOutcome::CheckpointCommitted {
                        commit_hash: "opaque-commit-1".to_string(),
                        cas_retries: 0,
                        object_count: 1,
                    }
                }),
                "{kind}: coordinator outcome"
            );
            assert_eq!(store.writes(), expected_writes, "{kind}: store writes");

            let replay =
                deliver_lifecycle(&catalog, &store, &payload, Some(seeded), kind, event).await;
            assert_eq!(
                replay.reserved,
                CaptureCatalogApplyResult::AlreadyApplied,
                "{kind}: a completed receipt replays as an acknowledgement"
            );
            assert_eq!(replay.outcome, None, "{kind}: replay builds no coordinator");
            assert_eq!(
                store.writes(),
                expected_writes,
                "{kind}: replay must not write again"
            );

            // The catalog applies a later action only over its exact durable
            // state, so these probes pin phase, stopped_at, and revision.
            let stale = deliver_lifecycle(
                &catalog,
                &store,
                &payload,
                Some(seeded),
                LifecycleEventKind::TurnStart,
                0x0b5e_0000 + row,
            )
            .await;
            assert_eq!(
                stale.reserved,
                CaptureCatalogApplyResult::ConflictUnchanged {
                    conflict: CaptureCatalogConflict::ExpectedState,
                },
                "{kind}: the seeded state must have advanced"
            );
            let completed = DurableCaptureState {
                phase: phase_after,
                stopped_at: terminal.then_some(MATRIX_OCCURRED_AT),
                // A terminal completion publishes one revision past the one
                // its receipt reserved.
                sync_revision: if terminal { 3 } else { 2 },
            };
            let probe = deliver_lifecycle(
                &catalog,
                &store,
                &payload,
                Some(completed),
                LifecycleEventKind::TurnStart,
                0x9e0b_0000 + row,
            )
            .await;
            assert!(
                matches!(probe.reserved, CaptureCatalogApplyResult::Applied { .. }),
                "{kind}: durable state must be exactly {completed:?}, got {:?}",
                probe.reserved
            );
        }

        // The reducer never pairs a terminal transition with no checkpoint,
        // and the catalog refuses to persist one: without a checkpoint there
        // would be no proof that could ever publish `stopped`.
        let terminal_without_checkpoint = LifecycleActionPlan {
            action_key: "capture-lifecycle-v1:acf06".to_string(),
            next_phase: CapturePhase::Stopped,
            stopped_at: StoppedAtMutation::Set(MATRIX_OCCURRED_AT),
            checkpoint: CheckpointWrite::None,
            expected_sync_revision: Some(1),
        };
        assert_eq!(
            CaptureCatalogMutation::from_reducer(
                Some(seeded),
                &terminal_without_checkpoint,
                MATRIX_OCCURRED_AT,
            ),
            Err(CaptureCatalogError::InvalidRequest)
        );
    }
}
