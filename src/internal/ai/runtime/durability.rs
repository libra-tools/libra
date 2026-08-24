//! Recovery-critical runtime command durability.
//!
//! This service is the only runtime owner for the narrow sequence
//! `durable intent -> one dispatch -> durable terminal result`. It delegates
//! event storage to the existing session JSONL store; it does not create a
//! second Code event log or decide a projection/resume view (W1-06).

use crate::internal::ai::session::{
    CodeCommandAdmission, CodeCommandIntent, CodeCommandRecovery, CodeCommandStatus,
    CodeCommandStoreError, IntentRevisionRecovery, Phase1RetryIntentReview, SessionJsonlStore,
};

/// Deterministic crash boundaries used by the runtime regression harness.
///
/// Production callers pass `None`. Each point models process loss at a
/// durable boundary rather than trying to catch or recover from a real crash
/// in-process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DurableCommandCrashPoint {
    BeforeIntentFsync,
    AfterIntentFsyncBeforeDispatch,
    AfterDispatchBeforeTerminalFsync,
    AfterTerminalFsync,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeCommandDurabilityError {
    #[error(transparent)]
    Store(#[from] CodeCommandStoreError),
    #[error("injected runtime crash at {0:?}")]
    InjectedCrash(DurableCommandCrashPoint),
}

/// Runtime-owned facade over a single session's append-only command log.
#[derive(Debug, Clone)]
pub struct RuntimeCommandDurability {
    session_store: SessionJsonlStore,
}

impl RuntimeCommandDurability {
    pub fn new(session_store: SessionJsonlStore) -> Self {
        Self { session_store }
    }

    pub fn session_store(&self) -> &SessionJsonlStore {
        &self.session_store
    }

    /// Return the committed status for this exact durable request without
    /// admitting a new command. This is used when another live turn already
    /// owns the session: a terminal retry may be acknowledged, but a new
    /// intent must not be appended behind that owner.
    pub fn existing_status_for_intent(
        &self,
        intent: &CodeCommandIntent,
    ) -> Result<Option<CodeCommandStatus>, RuntimeCommandDurabilityError> {
        match self
            .session_store
            .code_command_intent_status(&intent.identity)?
        {
            Some((existing, status)) if existing == *intent => Ok(Some(status)),
            Some(_) => Err(CodeCommandStoreError::PayloadConflict {
                repo_id: intent.identity.repo_id.clone(),
                session_id: intent.identity.session_id.clone(),
                principal_id: intent.identity.principal_id.clone(),
                command_id: intent.identity.command_id.clone(),
            }
            .into()),
            None => Ok(None),
        }
    }

    pub fn checkpoint_pending_interaction_resolutions(
        &self,
        intent: &CodeCommandIntent,
        resolutions: &[(String, String)],
    ) -> Result<(), RuntimeCommandDurabilityError> {
        #[cfg(any(test, feature = "test-provider"))]
        if self
            .session_store
            .take_pending_interaction_checkpoint_failure_for_test()
        {
            return Err(RuntimeCommandDurabilityError::InjectedCrash(
                DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync,
            ));
        }
        Ok(self
            .session_store
            .checkpoint_pending_interaction_resolutions(&intent.identity, resolutions)?)
    }

    /// Durably admit an asynchronous runtime command before its executor is
    /// allowed to cross a side-effect boundary. Async adapters use this
    /// narrow half of [`Self::execute`] when their dispatch cannot be held in
    /// a synchronous closure; they must call exactly one terminal method
    /// below once the executor reaches a determinate result.
    pub fn admit(
        &self,
        intent: CodeCommandIntent,
    ) -> Result<CodeCommandAdmission, RuntimeCommandDurabilityError> {
        Ok(self.session_store.admit_code_command(intent)?)
    }

    /// Record a durable successful terminal result for an admitted command.
    pub fn complete_success(
        &self,
        intent: &CodeCommandIntent,
        summary: impl Into<String>,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        Ok(self
            .session_store
            .complete_code_command_success(&intent.identity, summary)?)
    }

    /// Terminal success plus `InteractionResolved` in one durable append batch.
    pub fn complete_success_with_interaction_resolved(
        &self,
        intent: &CodeCommandIntent,
        summary: impl Into<String>,
        interaction_id: impl Into<String>,
        resolution: impl Into<String>,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        let resolutions = [(interaction_id.into(), resolution.into())];
        self.complete_success_with_interaction_resolutions_and_intent_revision(
            intent,
            summary,
            &resolutions,
            None,
        )
    }

    /// Terminal success plus zero-or-more interaction resolutions in one
    /// durable append batch (W2-05 multi-approval / multi-input turns).
    pub fn complete_success_with_interaction_resolutions(
        &self,
        intent: &CodeCommandIntent,
        summary: impl Into<String>,
        resolutions: &[(String, String)],
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        self.complete_success_with_interaction_resolutions_and_intent_revision(
            intent,
            summary,
            resolutions,
            None,
        )
    }

    /// Terminal success plus ordered interaction resolutions and optional
    /// IntentSpec Modify recovery data in one durable append.
    pub fn complete_success_with_interaction_resolutions_and_intent_revision(
        &self,
        intent: &CodeCommandIntent,
        summary: impl Into<String>,
        resolutions: &[(String, String)],
        intent_revision: Option<&IntentRevisionRecovery>,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        #[cfg(any(test, feature = "test-provider"))]
        if !resolutions.is_empty() {
            let inject = self
                .session_store
                .take_combined_terminal_append_failure_for_test();
            if inject {
                return Err(RuntimeCommandDurabilityError::InjectedCrash(
                    DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync,
                ));
            }
        }
        Ok(self
            .session_store
            .complete_code_command_success_with_interaction_resolutions_and_intent_revision(
                &intent.identity,
                summary,
                resolutions,
                intent_revision,
            )?)
    }

    /// Record a durable failed terminal result for an admitted command.
    pub fn complete_failure(
        &self,
        intent: &CodeCommandIntent,
        reason: impl Into<String>,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        Ok(self
            .session_store
            .complete_code_command_failure(&intent.identity, reason)?)
    }

    pub fn complete_failure_with_interaction_resolutions(
        &self,
        intent: &CodeCommandIntent,
        reason: impl Into<String>,
        interaction_resolutions: &[(String, String)],
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        self.complete_failure_with_interaction_resolutions_and_retry_intent_review(
            intent,
            reason,
            interaction_resolutions,
            None,
        )
    }

    pub fn complete_failure_with_interaction_resolutions_and_retry_intent_review(
        &self,
        intent: &CodeCommandIntent,
        reason: impl Into<String>,
        interaction_resolutions: &[(String, String)],
        retry_intent_review: Option<&Phase1RetryIntentReview>,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        Ok(self
            .session_store
            .complete_code_command_failure_with_interaction_resolutions_and_retry_intent_review(
                &intent.identity,
                reason,
                interaction_resolutions,
                retry_intent_review,
            )?)
    }

    /// Record an explicit reconciliation requirement when a started mutation
    /// cannot be proven to have reached a determinate result.
    pub fn mark_indeterminate(
        &self,
        intent: &CodeCommandIntent,
        effect: impl Into<String>,
        reason: impl Into<String>,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        Ok(self
            .session_store
            .mark_code_command_indeterminate(&intent.identity, effect, reason)?)
    }

    /// Run one command exactly once after its intent reaches durable storage.
    ///
    /// A previously admitted command returns its durable state without
    /// dispatching again. A post-intent crash deliberately leaves recovery to
    /// [`Self::recover`], which turns mutating pending entries into
    /// `Indeterminate` instead of replaying their side effect.
    pub fn execute<F>(
        &self,
        intent: CodeCommandIntent,
        crash_point: Option<DurableCommandCrashPoint>,
        dispatch: F,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError>
    where
        F: FnOnce() -> Result<String, String>,
    {
        if crash_point == Some(DurableCommandCrashPoint::BeforeIntentFsync) {
            return Err(RuntimeCommandDurabilityError::InjectedCrash(
                DurableCommandCrashPoint::BeforeIntentFsync,
            ));
        }

        match self.session_store.admit_code_command(intent.clone())? {
            CodeCommandAdmission::Existing { status } => return Ok(status),
            CodeCommandAdmission::Execute { .. } => {}
        }

        if crash_point == Some(DurableCommandCrashPoint::AfterIntentFsyncBeforeDispatch) {
            return Err(RuntimeCommandDurabilityError::InjectedCrash(
                DurableCommandCrashPoint::AfterIntentFsyncBeforeDispatch,
            ));
        }

        let dispatch_result = dispatch();
        if crash_point == Some(DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync) {
            return Err(RuntimeCommandDurabilityError::InjectedCrash(
                DurableCommandCrashPoint::AfterDispatchBeforeTerminalFsync,
            ));
        }

        let status = self.persist_dispatch_result(&intent, dispatch_result)?;
        if crash_point == Some(DurableCommandCrashPoint::AfterTerminalFsync) {
            return Err(RuntimeCommandDurabilityError::InjectedCrash(
                DurableCommandCrashPoint::AfterTerminalFsync,
            ));
        }
        Ok(status)
    }

    /// Return the explicit restart decision for a previously admitted command.
    pub fn recover(
        &self,
        intent: &CodeCommandIntent,
    ) -> Result<CodeCommandRecovery, RuntimeCommandDurabilityError> {
        Ok(self.session_store.recover_code_command(&intent.identity)?)
    }

    /// Fence every pending mutating command in this session during runtime
    /// startup. Callers must not accept another turn until this completes.
    pub fn recover_pending_mutations(
        &self,
    ) -> Result<Vec<crate::internal::ai::session::CodeCommandIdentity>, RuntimeCommandDurabilityError>
    {
        Ok(self
            .session_store
            .recover_pending_mutating_code_commands()?)
    }

    /// Like [`Self::recover_pending_mutations`], but when `phase0_turn_id`
    /// identifies the IntentSpec draft turn, that pending mutation is
    /// completed as success while other pending mutations stay fenced.
    pub fn recover_pending_mutations_for_intent_review(
        &self,
        phase0_turn_id: Option<&str>,
    ) -> Result<Vec<crate::internal::ai::session::CodeCommandIdentity>, RuntimeCommandDurabilityError>
    {
        Ok(self
            .session_store
            .recover_pending_mutating_code_commands_for_intent_review(phase0_turn_id)?)
    }

    pub fn recover_pending_mutations_for_review_and_phase1_prewrite(
        &self,
        phase0_turn_id: Option<&str>,
        phase1_prewrite_intent: Option<&crate::internal::ai::session::CodeCommandIntent>,
        intent_revision_consumer: Option<&crate::internal::ai::session::IntentRevisionConsumption>,
    ) -> Result<
        crate::internal::ai::session::jsonl::PendingMutationRecoveryOutcome,
        RuntimeCommandDurabilityError,
    > {
        Ok(self
            .session_store
            .recover_pending_mutating_code_commands_for_review_and_phase1_prewrite(
                phase0_turn_id,
                phase1_prewrite_intent,
                intent_revision_consumer,
            )?)
    }

    /// Dispatch an explicitly recovered read-only command. This refuses to
    /// replay a pending mutating command: `recover` will instead have made it
    /// durable `Indeterminate` and return that state here.
    pub fn retry_recovered_read_only<F>(
        &self,
        intent: CodeCommandIntent,
        dispatch: F,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError>
    where
        F: FnOnce() -> Result<String, String>,
    {
        match self.session_store.admit_code_command(intent.clone())? {
            CodeCommandAdmission::Execute { .. } => {
                self.persist_dispatch_result(&intent, dispatch())
            }
            CodeCommandAdmission::Existing {
                status: CodeCommandStatus::Pending,
            } => match self.recover(&intent)? {
                CodeCommandRecovery::RetryReadOnly { .. } => {
                    self.persist_dispatch_result(&intent, dispatch())
                }
                CodeCommandRecovery::Existing { status } => Ok(status),
            },
            CodeCommandAdmission::Existing { status } => Ok(status),
        }
    }

    fn persist_dispatch_result(
        &self,
        intent: &CodeCommandIntent,
        dispatch_result: Result<String, String>,
    ) -> Result<CodeCommandStatus, RuntimeCommandDurabilityError> {
        match dispatch_result {
            Ok(summary) => Ok(self
                .session_store
                .complete_code_command_success(&intent.identity, summary)?),
            Err(reason) => Ok(self
                .session_store
                .complete_code_command_failure(&intent.identity, reason)?),
        }
    }
}
