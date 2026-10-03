//! Deadline-bounded terminal finalization policy for Agent Capture.
//!
//! This is deliberately a pure policy layer.  It decides whether a terminal
//! lifecycle action may be made visible, must remain a replayable pending
//! receipt, or has exhausted its bounded recovery budget.  Stores persist the
//! receipt; this module never reads a clock, filesystem, provider, or ref.

use thiserror::Error;

/// Maximum attempts for one replay key before the session must require
/// operator repair.  The number is shared by live hooks and later doctor
/// replay, preventing an unbounded retry loop on either path.
pub const MAX_FINALIZE_ATTEMPTS: u8 = 5;
/// Maximum wall-clock span for attempts under one replay key.
pub const MAX_FINALIZE_WINDOW_MILLIS: i64 = 30 * 60 * 1000;

/// Whether the host is allowed to defer a terminal write after its synchronous
/// budget expires.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CaptureFinalizeMode {
    Synchronous,
    Deferrable,
}

/// Provider-neutral finalization policy.
///
/// `deadline_millis` is an absolute time supplied by the caller.  It is not a
/// timeout duration, so replay cannot extend a host's original budget by
/// repeatedly starting new timers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureFinalizePolicy {
    deadline_millis: Option<i64>,
    mode: CaptureFinalizeMode,
    replay_key: String,
}

impl CaptureFinalizePolicy {
    pub fn new(
        deadline_millis: Option<i64>,
        mode: CaptureFinalizeMode,
        replay_key: impl Into<String>,
    ) -> Result<Self, FinalizerError> {
        let replay_key = replay_key.into();
        validate_replay_key(&replay_key)?;
        Ok(Self {
            deadline_millis,
            mode,
            replay_key,
        })
    }

    pub fn deadline_millis(&self) -> Option<i64> {
        self.deadline_millis
    }

    pub fn mode(&self) -> CaptureFinalizeMode {
        self.mode
    }

    pub fn replay_key(&self) -> &str {
        &self.replay_key
    }
}

/// Content-free persisted evidence for a deferred terminal operation.
///
/// It holds only stable identities, an opaque source digest, the stage, and
/// bounded retry accounting.  Raw transcript/path/error content must never be
/// stored in this receipt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingFinalizeReceipt {
    replay_key: String,
    marker_generation: String,
    source_digest: Option<String>,
    // Preserve the original policy with the receipt.  A retry must not be
    // able to extend a host deadline simply by constructing a fresh policy.
    deadline_millis: Option<i64>,
    mode: CaptureFinalizeMode,
    first_attempt_millis: i64,
    attempts: u8,
    stage: FinalizePendingStage,
}

impl PendingFinalizeReceipt {
    pub fn new(
        policy: &CaptureFinalizePolicy,
        marker_generation: impl Into<String>,
        source_digest: Option<String>,
        now_millis: i64,
        stage: FinalizePendingStage,
    ) -> Result<Self, FinalizerError> {
        let marker_generation = marker_generation.into();
        validate_marker_generation(&marker_generation)?;
        validate_new_source_digest(source_digest.as_deref())?;
        Ok(Self {
            replay_key: policy.replay_key.clone(),
            marker_generation,
            source_digest,
            deadline_millis: policy.deadline_millis,
            mode: policy.mode,
            first_attempt_millis: now_millis,
            attempts: 1,
            stage,
        })
    }

    /// Rebuild a receipt read from a durable catalog ledger.  The ledger
    /// decoder calls this rather than constructing fields directly so old or
    /// malformed state cannot bypass the replay/window invariants.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn restore(
        replay_key: impl Into<String>,
        marker_generation: impl Into<String>,
        source_digest: Option<String>,
        deadline_millis: Option<i64>,
        mode: CaptureFinalizeMode,
        first_attempt_millis: i64,
        attempts: u8,
        stage: FinalizePendingStage,
    ) -> Result<Self, FinalizerError> {
        let replay_key = replay_key.into();
        validate_replay_key(&replay_key)?;
        let marker_generation = marker_generation.into();
        validate_marker_generation(&marker_generation)?;
        // Historical ledgers may contain the original bare hexadecimal
        // spelling. Keep that immutable evidence readable, but never let a
        // new mutation manufacture another ambiguous bare digest.
        validate_stored_source_digest(source_digest.as_deref())?;
        if attempts == 0 || attempts > MAX_FINALIZE_ATTEMPTS {
            return Err(FinalizerError::InvalidAttempts);
        }
        Ok(Self {
            replay_key,
            marker_generation,
            source_digest,
            deadline_millis,
            mode,
            first_attempt_millis,
            attempts,
            stage,
        })
    }

    pub fn replay_key(&self) -> &str {
        &self.replay_key
    }

    pub fn marker_generation(&self) -> &str {
        &self.marker_generation
    }

    pub fn source_digest(&self) -> Option<&str> {
        self.source_digest.as_deref()
    }

    pub(crate) fn deadline_millis(&self) -> Option<i64> {
        self.deadline_millis
    }

    pub(crate) fn mode(&self) -> CaptureFinalizeMode {
        self.mode
    }

    pub(crate) fn first_attempt_millis(&self) -> i64 {
        self.first_attempt_millis
    }

    pub fn attempts(&self) -> u8 {
        self.attempts
    }

    pub fn stage(&self) -> FinalizePendingStage {
        self.stage
    }
}

/// Safe terminal-write stages that can be persisted in a pending receipt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalizePendingStage {
    Snapshot,
    Marker,
    Checkpoint,
    Cleanup,
}

/// Typed checkpoint progress presented to the finalizer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalizeCheckpointProgress {
    /// No checkpoint has been committed yet.
    NotStarted,
    /// A checkpoint (including a replayed existing checkpoint) is durable.
    Durable,
    /// A conflict or failed store attempt left no terminal-safe checkpoint.
    Retryable(FinalizePendingStage),
    /// A failed cleanup has durable checkpoint data but needs repair evidence.
    PendingCleanup,
}

/// Input to the pure finalization decision.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FinalizeDecisionInput<'a> {
    pub policy: &'a CaptureFinalizePolicy,
    pub current_receipt: Option<&'a PendingFinalizeReceipt>,
    pub marker_generation: &'a str,
    pub source_digest: Option<&'a str>,
    pub now_millis: i64,
    pub checkpoint: FinalizeCheckpointProgress,
}

/// Decision a coordinator must execute exactly once through its stores.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FinalizeDecision {
    /// It is now safe to atomically publish `stopped` plus `stopped_at`.
    CommitTerminal,
    /// Preserve or update a replayable receipt; terminal state remains hidden.
    PersistPending(PendingFinalizeReceipt),
    /// Recovery budget is exhausted.  Mark the capture quarantined/repairable,
    /// never falsely terminal.
    Quarantine { reason: FinalizeQuarantineReason },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FinalizeQuarantineReason {
    AttemptLimit,
    WindowLimit,
    SourceDigestConflict,
    MarkerGenerationConflict,
    SynchronousDeadline,
}

/// Compute the terminal/pending/quarantine transition without side effects.
pub fn decide_finalization(
    input: FinalizeDecisionInput<'_>,
) -> Result<FinalizeDecision, FinalizerError> {
    validate_marker_generation(input.marker_generation)?;
    validate_new_source_digest(input.source_digest)?;

    if let Some(receipt) = input.current_receipt {
        if receipt.replay_key != input.policy.replay_key {
            return Err(FinalizerError::ReplayKeyMismatch);
        }
        if receipt.deadline_millis != input.policy.deadline_millis
            || receipt.mode != input.policy.mode
        {
            return Err(FinalizerError::PolicyMismatch);
        }
        if receipt.marker_generation != input.marker_generation {
            return Ok(FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::MarkerGenerationConflict,
            });
        }
        if receipt.source_digest.as_deref() != input.source_digest {
            return Ok(FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::SourceDigestConflict,
            });
        }
        // A checkpoint can be durable only for the exact reserved generation
        // and source commitment. Validate those fences before permitting the
        // terminal publication; otherwise a stale finalizer could turn a
        // newer session terminal merely because an older write completed.
        if matches!(input.checkpoint, FinalizeCheckpointProgress::Durable) {
            return Ok(FinalizeDecision::CommitTerminal);
        }
        let deadline_elapsed = input
            .policy
            .deadline_millis()
            .is_some_and(|deadline| input.now_millis >= deadline);
        if deadline_elapsed && matches!(input.policy.mode(), CaptureFinalizeMode::Synchronous) {
            return Ok(FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::SynchronousDeadline,
            });
        }
        let attempts = receipt.attempts.saturating_add(1);
        if attempts > MAX_FINALIZE_ATTEMPTS {
            return Ok(FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::AttemptLimit,
            });
        }
        if input
            .now_millis
            .saturating_sub(receipt.first_attempt_millis)
            > MAX_FINALIZE_WINDOW_MILLIS
        {
            return Ok(FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::WindowLimit,
            });
        }
        return Ok(FinalizeDecision::PersistPending(PendingFinalizeReceipt {
            replay_key: receipt.replay_key.clone(),
            marker_generation: receipt.marker_generation.clone(),
            source_digest: receipt.source_digest.clone(),
            deadline_millis: receipt.deadline_millis,
            mode: receipt.mode,
            first_attempt_millis: receipt.first_attempt_millis,
            attempts,
            stage: match input.checkpoint {
                FinalizeCheckpointProgress::NotStarted => FinalizePendingStage::Snapshot,
                FinalizeCheckpointProgress::Retryable(stage) => stage,
                FinalizeCheckpointProgress::PendingCleanup => FinalizePendingStage::Cleanup,
                FinalizeCheckpointProgress::Durable => {
                    return Err(FinalizerError::InconsistentCheckpointProgress);
                }
            },
        }));
    }

    if matches!(input.checkpoint, FinalizeCheckpointProgress::Durable) {
        return Err(FinalizerError::MissingPendingReceipt);
    }

    let deadline_elapsed = input
        .policy
        .deadline_millis()
        .is_some_and(|deadline| input.now_millis >= deadline);
    if deadline_elapsed && matches!(input.policy.mode(), CaptureFinalizeMode::Synchronous) {
        return Ok(FinalizeDecision::Quarantine {
            reason: FinalizeQuarantineReason::SynchronousDeadline,
        });
    }

    let stage = match input.checkpoint {
        FinalizeCheckpointProgress::NotStarted => FinalizePendingStage::Snapshot,
        FinalizeCheckpointProgress::Retryable(stage) => stage,
        FinalizeCheckpointProgress::PendingCleanup => FinalizePendingStage::Cleanup,
        FinalizeCheckpointProgress::Durable => {
            return Err(FinalizerError::InconsistentCheckpointProgress);
        }
    };
    Ok(FinalizeDecision::PersistPending(
        PendingFinalizeReceipt::new(
            input.policy,
            input.marker_generation,
            input.source_digest.map(str::to_owned),
            input.now_millis,
            stage,
        )?,
    ))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FinalizerError {
    #[error("capture finalizer replay key is empty or too long")]
    InvalidReplayKey,
    #[error("capture finalizer marker generation is empty or too long")]
    InvalidMarkerGeneration,
    #[error("capture finalizer source digest must use source/hmac-v2/<64 lowercase hex>")]
    InvalidSourceDigest,
    #[error("capture finalizer receipt replay key does not match the operation")]
    ReplayKeyMismatch,
    #[error("capture finalizer policy does not match the durable pending receipt")]
    PolicyMismatch,
    #[error("capture finalizer receipt attempt count is invalid")]
    InvalidAttempts,
    #[error("capture finalizer received an inconsistent durable checkpoint state")]
    InconsistentCheckpointProgress,
    #[error("capture finalizer cannot publish a terminal state without its pending receipt")]
    MissingPendingReceipt,
}

fn validate_replay_key(replay_key: &str) -> Result<(), FinalizerError> {
    if replay_key.is_empty() || replay_key.len() > 512 {
        return Err(FinalizerError::InvalidReplayKey);
    }
    Ok(())
}

fn validate_marker_generation(marker_generation: &str) -> Result<(), FinalizerError> {
    if marker_generation.is_empty() || marker_generation.len() > 512 {
        return Err(FinalizerError::InvalidMarkerGeneration);
    }
    Ok(())
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'))
}

fn valid_source_hmac_v2(value: &str) -> bool {
    value
        .strip_prefix("source/hmac-v2/")
        .is_some_and(valid_sha256_hex)
}

fn validate_new_source_digest(source_digest: Option<&str>) -> Result<(), FinalizerError> {
    if source_digest.is_some_and(|digest| !valid_source_hmac_v2(digest)) {
        return Err(FinalizerError::InvalidSourceDigest);
    }
    Ok(())
}

fn validate_stored_source_digest(source_digest: Option<&str>) -> Result<(), FinalizerError> {
    if source_digest.is_some_and(|digest| {
        !valid_source_hmac_v2(digest)
            && !digest.strip_prefix("sha256:").is_some_and(valid_sha256_hex)
            && !valid_sha256_hex(digest)
    }) {
        return Err(FinalizerError::InvalidSourceDigest);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE_DIGEST: &str = concat!(
        "source/hmac-v2/",
        "0123456789abcdef",
        "0123456789abcdef",
        "0123456789abcdef",
        "0123456789abcdef",
    );
    const OTHER_SOURCE_DIGEST: &str = concat!(
        "source/hmac-v2/",
        "fedcba9876543210",
        "fedcba9876543210",
        "fedcba9876543210",
        "fedcba9876543210",
    );

    fn policy(mode: CaptureFinalizeMode) -> CaptureFinalizePolicy {
        CaptureFinalizePolicy::new(Some(1_000), mode, "capture-lifecycle-v1:event")
            .expect("valid policy")
    }

    #[test]
    fn terminal_is_never_committed_before_checkpoint_is_durable() {
        let decision = decide_finalization(FinalizeDecisionInput {
            policy: &policy(CaptureFinalizeMode::Deferrable),
            current_receipt: None,
            marker_generation: "generation",
            source_digest: Some(SOURCE_DIGEST),
            now_millis: 1,
            checkpoint: FinalizeCheckpointProgress::NotStarted,
        })
        .expect("decision");
        assert!(matches!(decision, FinalizeDecision::PersistPending(_)));

        let receipt = PendingFinalizeReceipt::new(
            &policy(CaptureFinalizeMode::Deferrable),
            "generation",
            Some(SOURCE_DIGEST.to_string()),
            1,
            FinalizePendingStage::Checkpoint,
        )
        .expect("receipt");
        let committed = decide_finalization(FinalizeDecisionInput {
            policy: &policy(CaptureFinalizeMode::Deferrable),
            current_receipt: Some(&receipt),
            marker_generation: "generation",
            source_digest: Some(SOURCE_DIGEST),
            now_millis: 1,
            checkpoint: FinalizeCheckpointProgress::Durable,
        })
        .expect("decision");
        assert_eq!(committed, FinalizeDecision::CommitTerminal);
    }

    #[test]
    fn stale_marker_or_source_cannot_take_over_a_pending_receipt() {
        let policy = policy(CaptureFinalizeMode::Deferrable);
        let receipt = PendingFinalizeReceipt::new(
            &policy,
            "old-generation",
            Some(SOURCE_DIGEST.to_string()),
            1,
            FinalizePendingStage::Checkpoint,
        )
        .expect("receipt");
        let decision = decide_finalization(FinalizeDecisionInput {
            policy: &policy,
            current_receipt: Some(&receipt),
            marker_generation: "new-generation",
            source_digest: Some(OTHER_SOURCE_DIGEST),
            now_millis: 2,
            checkpoint: FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
        })
        .expect("decision");
        assert_eq!(
            decision,
            FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::MarkerGenerationConflict
            }
        );
    }

    #[test]
    fn retries_are_bounded_by_attempts_and_wall_clock() {
        let policy = policy(CaptureFinalizeMode::Deferrable);
        let mut receipt = PendingFinalizeReceipt::new(
            &policy,
            "generation",
            None,
            1,
            FinalizePendingStage::Checkpoint,
        )
        .expect("receipt");
        receipt.attempts = MAX_FINALIZE_ATTEMPTS;
        let attempts = decide_finalization(FinalizeDecisionInput {
            policy: &policy,
            current_receipt: Some(&receipt),
            marker_generation: "generation",
            source_digest: None,
            now_millis: 2,
            checkpoint: FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
        })
        .expect("decision");
        assert!(matches!(
            attempts,
            FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::AttemptLimit
            }
        ));

        receipt.attempts = 1;
        let wall_clock = decide_finalization(FinalizeDecisionInput {
            policy: &policy,
            current_receipt: Some(&receipt),
            marker_generation: "generation",
            source_digest: None,
            now_millis: 1 + MAX_FINALIZE_WINDOW_MILLIS + 1,
            checkpoint: FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
        })
        .expect("decision");
        assert!(matches!(
            wall_clock,
            FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::WindowLimit
            }
        ));
    }

    #[test]
    fn durable_receipt_rejects_policy_reanchoring_but_can_finish_after_deadline() {
        let original = CaptureFinalizePolicy::new(
            Some(10),
            CaptureFinalizeMode::Deferrable,
            "capture-lifecycle-v1:event",
        )
        .expect("original policy");
        let receipt = PendingFinalizeReceipt::new(
            &original,
            "generation",
            Some(SOURCE_DIGEST.to_string()),
            1,
            FinalizePendingStage::Checkpoint,
        )
        .expect("receipt");
        let reanchored = CaptureFinalizePolicy::new(
            Some(999),
            CaptureFinalizeMode::Deferrable,
            "capture-lifecycle-v1:event",
        )
        .expect("reanchored policy");
        assert_eq!(
            decide_finalization(FinalizeDecisionInput {
                policy: &reanchored,
                current_receipt: Some(&receipt),
                marker_generation: "generation",
                source_digest: Some(SOURCE_DIGEST),
                now_millis: 2,
                checkpoint: FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
            }),
            Err(FinalizerError::PolicyMismatch)
        );
        assert_eq!(
            decide_finalization(FinalizeDecisionInput {
                policy: &original,
                current_receipt: Some(&receipt),
                marker_generation: "generation",
                source_digest: Some(SOURCE_DIGEST),
                now_millis: 11,
                checkpoint: FinalizeCheckpointProgress::Durable,
            }),
            Ok(FinalizeDecision::CommitTerminal),
            "a checkpoint that is already durable is a valid terminal completion even if the host deadline passed"
        );
    }

    #[test]
    fn expired_synchronous_policy_quarantines_a_fresh_attempt_without_a_receipt() {
        let synchronous = policy(CaptureFinalizeMode::Synchronous);
        for (now_millis, checkpoint) in [
            (1_000, FinalizeCheckpointProgress::NotStarted),
            (
                1_001,
                FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
            ),
            (i64::MAX, FinalizeCheckpointProgress::PendingCleanup),
        ] {
            assert_eq!(
                decide_finalization(FinalizeDecisionInput {
                    policy: &synchronous,
                    current_receipt: None,
                    marker_generation: "generation",
                    source_digest: Some(SOURCE_DIGEST),
                    now_millis,
                    checkpoint,
                }),
                Ok(FinalizeDecision::Quarantine {
                    reason: FinalizeQuarantineReason::SynchronousDeadline,
                }),
                "an expired synchronous host budget at {now_millis} must not mint a pending receipt"
            );
        }

        // Controls: the same expired deadline is still pending when the host
        // may defer, and a synchronous attempt inside its budget is pending.
        for (mode, now_millis) in [
            (CaptureFinalizeMode::Deferrable, 1_000),
            (CaptureFinalizeMode::Synchronous, 999),
        ] {
            match decide_finalization(FinalizeDecisionInput {
                policy: &policy(mode),
                current_receipt: None,
                marker_generation: "generation",
                source_digest: Some(SOURCE_DIGEST),
                now_millis,
                checkpoint: FinalizeCheckpointProgress::NotStarted,
            }) {
                Ok(FinalizeDecision::PersistPending(receipt)) => {
                    assert_eq!(receipt.attempts(), 1);
                    assert_eq!(receipt.mode(), mode);
                    assert_eq!(receipt.first_attempt_millis(), now_millis);
                }
                other => panic!("{mode:?} at {now_millis} must stay pending, got {other:?}"),
            }
        }
    }

    #[test]
    fn expired_synchronous_receipt_quarantines_before_attempt_accounting() {
        let synchronous = policy(CaptureFinalizeMode::Synchronous);
        let mut receipt = PendingFinalizeReceipt::new(
            &synchronous,
            "generation",
            Some(SOURCE_DIGEST.to_string()),
            1,
            FinalizePendingStage::Checkpoint,
        )
        .expect("receipt");
        let decide = |receipt: &PendingFinalizeReceipt, now_millis: i64| {
            decide_finalization(FinalizeDecisionInput {
                policy: &synchronous,
                current_receipt: Some(receipt),
                marker_generation: "generation",
                source_digest: Some(SOURCE_DIGEST),
                now_millis,
                checkpoint: FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
            })
        };
        let synchronous_deadline = Ok(FinalizeDecision::Quarantine {
            reason: FinalizeQuarantineReason::SynchronousDeadline,
        });

        // Inside the budget a retry consumes one attempt; once the deadline
        // elapses, the same retry is repair-required instead.
        assert!(matches!(
            decide(&receipt, 999),
            Ok(FinalizeDecision::PersistPending(next)) if next.attempts() == 2
        ));
        assert_eq!(decide(&receipt, 1_000), synchronous_deadline);

        // The deadline is checked before attempt/window accounting, so an
        // exhausted budget still reports the synchronous contract rather than
        // a deferrable retry limit.
        receipt.attempts = MAX_FINALIZE_ATTEMPTS;
        assert_eq!(decide(&receipt, 1_000), synchronous_deadline);
        receipt.attempts = 1;
        assert_eq!(
            decide(&receipt, 1 + MAX_FINALIZE_WINDOW_MILLIS + 1),
            synchronous_deadline
        );

        // A checkpoint that already became durable for the exact fence still
        // completes: the deadline only prevents further hidden retries.
        assert_eq!(
            decide_finalization(FinalizeDecisionInput {
                policy: &synchronous,
                current_receipt: Some(&receipt),
                marker_generation: "generation",
                source_digest: Some(SOURCE_DIGEST),
                now_millis: 1_000,
                checkpoint: FinalizeCheckpointProgress::Durable,
            }),
            Ok(FinalizeDecision::CommitTerminal)
        );
        // A stale marker past the deadline is still classified as a takeover:
        // the deadline never hides a fence conflict.
        assert_eq!(
            decide_finalization(FinalizeDecisionInput {
                policy: &synchronous,
                current_receipt: Some(&receipt),
                marker_generation: "other-generation",
                source_digest: Some(SOURCE_DIGEST),
                now_millis: 1_000,
                checkpoint: FinalizeCheckpointProgress::Retryable(FinalizePendingStage::Checkpoint),
            }),
            Ok(FinalizeDecision::Quarantine {
                reason: FinalizeQuarantineReason::MarkerGenerationConflict,
            })
        );
    }

    #[test]
    fn new_source_digest_requires_a_repository_keyed_hmac_v2_value() {
        for malformed in [
            "raw transcript content",
            "0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
            "sha256:0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF0123456789ABCDEF",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdeg",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcde",
        ] {
            assert_eq!(
                PendingFinalizeReceipt::new(
                    &policy(CaptureFinalizeMode::Deferrable),
                    "generation",
                    Some(malformed.to_string()),
                    1,
                    FinalizePendingStage::Checkpoint,
                ),
                Err(FinalizerError::InvalidSourceDigest),
                "receipt rejects {malformed:?} before durable persistence"
            );
            assert_eq!(
                PendingFinalizeReceipt::restore(
                    "capture-lifecycle-v1:event",
                    "generation",
                    Some(malformed.to_string()),
                    Some(1_000),
                    CaptureFinalizeMode::Deferrable,
                    1,
                    1,
                    FinalizePendingStage::Checkpoint,
                ),
                Err(FinalizerError::InvalidSourceDigest),
                "ledger restore rejects malformed {malformed:?}"
            );
            assert_eq!(
                decide_finalization(FinalizeDecisionInput {
                    policy: &policy(CaptureFinalizeMode::Deferrable),
                    current_receipt: None,
                    marker_generation: "generation",
                    source_digest: Some(malformed),
                    now_millis: 1,
                    checkpoint: FinalizeCheckpointProgress::NotStarted,
                }),
                Err(FinalizerError::InvalidSourceDigest),
                "incoming finalizer input rejects {malformed:?}"
            );
        }

        let legacy_bare = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        let legacy_tagged = concat!(
            "sha256:",
            "0123456789abcdef",
            "0123456789abcdef",
            "0123456789abcdef",
            "0123456789abcdef",
        );
        assert_eq!(
            PendingFinalizeReceipt::new(
                &policy(CaptureFinalizeMode::Deferrable),
                "generation",
                Some(legacy_bare.to_string()),
                1,
                FinalizePendingStage::Checkpoint,
            ),
            Err(FinalizerError::InvalidSourceDigest),
            "new receipts must not manufacture ambiguous bare digest evidence"
        );
        assert_eq!(
            decide_finalization(FinalizeDecisionInput {
                policy: &policy(CaptureFinalizeMode::Deferrable),
                current_receipt: None,
                marker_generation: "generation",
                source_digest: Some(legacy_bare),
                now_millis: 1,
                checkpoint: FinalizeCheckpointProgress::NotStarted,
            }),
            Err(FinalizerError::InvalidSourceDigest),
            "new finalizer decisions must reject bare source digests"
        );
        assert_eq!(
            PendingFinalizeReceipt::new(
                &policy(CaptureFinalizeMode::Deferrable),
                "generation",
                Some(legacy_tagged.to_string()),
                1,
                FinalizePendingStage::Checkpoint,
            ),
            Err(FinalizerError::InvalidSourceDigest),
            "new receipts must not manufacture an unkeyed tagged digest"
        );
        assert_eq!(
            decide_finalization(FinalizeDecisionInput {
                policy: &policy(CaptureFinalizeMode::Deferrable),
                current_receipt: None,
                marker_generation: "generation",
                source_digest: Some(legacy_tagged),
                now_millis: 1,
                checkpoint: FinalizeCheckpointProgress::NotStarted,
            }),
            Err(FinalizerError::InvalidSourceDigest),
            "new finalizer decisions must reject tagged unkeyed source digests"
        );

        assert!(
            PendingFinalizeReceipt::new(
                &policy(CaptureFinalizeMode::Deferrable),
                "generation",
                Some(SOURCE_DIGEST.to_string()),
                1,
                FinalizePendingStage::Checkpoint,
            )
            .is_ok()
        );
        assert!(
            PendingFinalizeReceipt::restore(
                "capture-lifecycle-v1:event",
                "generation",
                Some(legacy_bare.to_string()),
                Some(1_000),
                CaptureFinalizeMode::Deferrable,
                1,
                1,
                FinalizePendingStage::Checkpoint,
            )
            .is_ok(),
            "legacy immutable receipts retain readable bare-hex evidence"
        );
        assert!(
            PendingFinalizeReceipt::restore(
                "capture-lifecycle-v1:event",
                "generation",
                Some(legacy_tagged.to_string()),
                Some(1_000),
                CaptureFinalizeMode::Deferrable,
                1,
                1,
                FinalizePendingStage::Checkpoint,
            )
            .is_ok(),
            "legacy immutable receipts retain readable tagged SHA evidence"
        );
        assert!(
            PendingFinalizeReceipt::new(
                &policy(CaptureFinalizeMode::Deferrable),
                "generation",
                None,
                1,
                FinalizePendingStage::Checkpoint,
            )
            .is_ok()
        );
    }
}
