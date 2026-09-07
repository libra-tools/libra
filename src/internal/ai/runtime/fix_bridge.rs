//! Authenticated bridge for `libra review --fix`.
//!
//! The review CLI never starts its own runtime worker. Instead it discovers an
//! already running, write-enabled `libra code` session and submits one fixed
//! plain-text planning request through that session's authenticated control
//! surface. Plain messages use the existing Phase 0 path, which cannot apply a
//! patch before the Code runtime's normal review gates. The execution driver in
//! [`super::fix_execution`] keeps that same controller lease while it relays
//! explicit user decisions back through the existing Code runtime.

use thiserror::Error;

/// Fixed, trusted request text admitted through the running Code runtime.
///
/// No reviewer stdout, finding, environment value, or user-supplied seed is
/// included here. The Code runtime must still obtain each normal plan, network,
/// tool-approval, sandbox, and ACL decision before it may apply a patch.
pub const REVIEW_FIX_ADMISSION_MESSAGE: &str = "Prepare a controlled review-fix plan for the current working tree. This request comes from `libra review --fix`; do not consume external reviewer findings. Do not apply a patch or run mutating tools until the user explicitly confirms the normal Code plan and every applicable approval, sandbox, network, and ACL gate. Then execute only through the Code runtime's normal controlled plan-execution path.";

/// Fixed request used by `libra investigate fix`.
///
/// The run id, topic, findings, stance text, and attachments are deliberately
/// absent. They are untrusted observed-agent content; the active Code runtime
/// independently inspects the current worktree and still asks for every normal
/// decision before mutation.
pub const INVESTIGATE_FIX_ADMISSION_MESSAGE: &str = "Prepare a controlled investigation-fix plan for the current working tree. This request comes from `libra investigate fix`; do not consume or infer from any stored investigation topic, findings, stance, attachment, or run identifier. Inspect the worktree independently. Do not apply a patch or run mutating tools until the user explicitly confirms the normal Code plan and every applicable approval, sandbox, network, and ACL gate. Then execute only through the Code runtime's normal controlled plan-execution path.";

/// Provenance classification required before any request can enter the
/// mutating-capable Code runtime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewFixInput {
    /// The CLI's fixed admission request; it contains no observed-agent data.
    TrustedAdmission,
    /// The investigate CLI's fixed request; no run content is included.
    TrustedInvestigateAdmission,
    /// An issue, transcript, finding, or other observed external-agent seed.
    UntrustedSeed,
}

impl ReviewFixInput {
    pub(super) fn admission_message(self) -> Result<&'static str, ReviewFixBridgeError> {
        match self {
            Self::TrustedAdmission => Ok(REVIEW_FIX_ADMISSION_MESSAGE),
            Self::TrustedInvestigateAdmission => Ok(INVESTIGATE_FIX_ADMISSION_MESSAGE),
            Self::UntrustedSeed => Err(ReviewFixBridgeError::UntrustedSeed),
        }
    }
}

/// Fail-closed result from the review-fix admission bridge.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ReviewFixBridgeError {
    #[error("no authorized active Code runtime is available")]
    Unavailable,
    #[error("untrusted seed content cannot enter a mutating workflow")]
    UntrustedSeed,
    #[error("review-fix tool approval was denied before a patch was applied")]
    ApprovalDenied,
    #[error("review-fix sandbox approval was denied before a patch was applied")]
    SandboxDenied,
    #[error("review-fix execution stopped without a clean success outcome")]
    ExecutionFailed,
    #[error("the Code runtime exposed a patch before a denial gate completed")]
    MutationBeforeDenial,
    #[error("review-fix execution exceeded its controlled wait limit")]
    TimedOut,
    #[error("review-fix interaction response was invalid")]
    InvalidInteractionResponse,
}
