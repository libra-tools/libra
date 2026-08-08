//! Recovery-critical runtime command durability.
//!
//! This service is the only runtime owner for the narrow sequence
//! `durable intent -> one dispatch -> durable terminal result`. It delegates
//! event storage to the existing session JSONL store; it does not create a
//! second Code event log or decide a projection/resume view (W1-06).

use crate::internal::ai::session::{
    CodeCommandAdmission, CodeCommandIntent, CodeCommandRecovery, CodeCommandStatus,
    CodeCommandStoreError, SessionJsonlStore,
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
