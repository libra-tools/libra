//! Cross-phase revision-chain helper (schema-only landing).
//!
//! Phase 0 (Intent), Phase 1 (Plan) and Phase 2 (Execution) all participate
//! in **revision chains**: every modification produces a new immutable
//! revision rather than overwriting the previous one, so the formal history
//! stays append-only and downstream verifiers / observers can reconstruct
//! the decision path.
//!
//! This module hosts the shared helpers for that chain — currently
//! schema-only — so the eventual implementations can sit beside the data
//! types instead of being scattered across `orchestrator/`, `intentspec/`
//! and `runtime/phase{0,1,2}.rs`.
//!
//! # Schema vs. wiring
//!
//! The current revision-chain logic is implicit:
//!
//! - `intentspec::resolve_intentspec` is invoked on every draft to produce
//!   a fresh `IntentSpec`; downstream code passes the new spec into
//!   [`super::phase0::write_intent`] and a new persisted Intent revision
//!   is created.
//! - `orchestrator::persistence::ExecutionAuditSession::record_plan_compiled`
//!   either reuses an existing preview plan id (when revision 1) or calls
//!   `create_plan_set_revision`, threading `parent_execution_plan_id` /
//!   `parent_test_plan_id` to keep the chain explicit.
//!
//! What's missing is a **shared** helper that captures the rules below
//! (per [`docs/development/tracing/agent.md`](../../../../docs/development/tracing/agent.md)
//! Part B revision chain section):
//!
//! 1. `Modify Plan` requests must not edit `Plan` / `Task` in place; they
//!    must derive a new revision skeleton from the previous one.
//! 2. `step_id` values are stable across plan revisions when the step's
//!    intent is unchanged, so observers can correlate metrics across
//!    revisions.
//! 3. `plan` and `test-plan` always rev together — the chain must enforce
//!    that the (n)-th execution-plan revision pairs with the (n)-th
//!    test-plan revision, never (n−1) or (n+1).
//!
//! Once these rules graduate from prose to code (`handle_modify_request()`
//! + `derive_next_revision_skeleton()`), they will land in this module.

use uuid::Uuid;

/// Identifies the kind of revision chain a modify request walks.
///
/// Each variant maps to a distinct AI object family in the formal history:
/// Intent ↔ `git-internal Intent`, ExecutionPlan ↔ persisted plan
/// revisions with `role = "execution"`, TestPlan ↔ same family with
/// `role = "test"`. Keeping the discriminator on the entry point means
/// downstream helpers can switch on a single value rather than re-deriving
/// the chain kind from request shape.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RevisionKind {
    Intent,
    ExecutionPlan,
    TestPlan,
}

impl RevisionKind {
    /// Stable label used in audit / log lines so a future grep pipeline can
    /// correlate revision events across phases.
    pub fn label(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::ExecutionPlan => "execution_plan",
            Self::TestPlan => "test_plan",
        }
    }
}

/// The parent reference and ordinal of a new revision in a chain.
///
/// `previous_id` is the persisted id of the immediately-preceding revision
/// (or `None` for the first link in a chain); `revision` is the 1-based
/// ordinal so the (n)-th plan revision can be paired with the (n)-th
/// test-plan revision per the cross-phase rule.
///
/// **Stability contract:** field names are part of the public Runtime
/// surface; once `handle_modify_request()` ships, downstream observers will
/// key off `previous_id` / `revision`. New fields may be added as
/// `Option<...>`; existing fields cannot be renamed or removed without a
/// parallel deprecation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RevisionChainEntry {
    pub kind: RevisionKind,
    pub previous_id: Option<String>,
    pub revision: u32,
    /// Logical entity id (e.g. `task_id` for plan / test-plan revisions).
    /// Stable across revisions of the same chain — observers correlate
    /// time-series metrics by this value.
    pub logical_id: Uuid,
}

impl RevisionChainEntry {
    /// `true` for the first link in a chain (no `previous_id`).
    pub fn is_first(&self) -> bool {
        self.previous_id.is_none()
    }

    /// `true` when this entry is a continuation (rev > 1) of an existing
    /// chain. Helpers like `handle_modify_request` will branch on this to
    /// either create the first revision or derive a skeleton from the
    /// `previous_id` link.
    pub fn is_continuation(&self) -> bool {
        self.revision > 1 && self.previous_id.is_some()
    }

    /// Stable single-line audit label for this entry, suitable for
    /// `tracing` field values or grep pipelines that join revision events
    /// across phases.
    ///
    /// Format (positional, stable):
    ///
    /// ```text
    /// <kind_label> rev <n> (head) for logical_id <uuid>
    /// <kind_label> rev <n> (continuation from <previous_id>) for logical_id <uuid>
    /// <kind_label> rev <n> (forked from <previous_id>) for logical_id <uuid>
    /// ```
    ///
    /// The three forms correspond to `is_first()` (head), `is_continuation()`
    /// (rev>1 with parent), and the degenerate "revision 1 with a parent"
    /// case (`first_and_continuation_flag_chain_position_correctly` test
    /// describes when this happens) which represents a forced re-derive.
    pub fn audit_label(&self) -> String {
        let kind = self.kind.label();
        match (&self.previous_id, self.revision) {
            (None, _) => format!(
                "{kind} rev {} (head) for logical_id {}",
                self.revision, self.logical_id
            ),
            (Some(prev), rev) if rev > 1 => format!(
                "{kind} rev {} (continuation from {}) for logical_id {}",
                self.revision, prev, self.logical_id
            ),
            (Some(prev), _) => format!(
                "{kind} rev {} (forked from {}) for logical_id {}",
                self.revision, prev, self.logical_id
            ),
        }
    }
}

/// User-facing payload for a "modify the current revision" request.
///
/// `kind` and `logical_id` identify which chain entry the modify is
/// against (they must match the chain's existing identity — see
/// [`handle_modify_request`] for the rule). `reason` is a free-form
/// human-readable string that gets audited alongside the resulting
/// formal-write event so downstream review can reconstruct *why* the
/// modify was requested.
///
/// **Why not include the modify *payload* here yet:** the actual
/// modification (which fields of `IntentSpec` / `ExecutionPlanSpec` change
/// between revisions) is per-`kind` and not yet uniformly typed. Future
/// patches will either (a) add `kind`-specific payload variants here, or
/// (b) keep this type as the audit envelope and pair it with a separate
/// payload argument at the call site.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModifyRequest {
    /// Which chain this modify targets — must match the input
    /// [`RevisionChainEntry::kind`] passed to [`handle_modify_request`].
    pub kind: RevisionKind,
    /// Stable logical entity id of the chain — must match the input
    /// [`RevisionChainEntry::logical_id`].
    pub logical_id: Uuid,
    /// Free-form human-readable rationale for the modify. Audited
    /// verbatim (subject to [`SecretRedactor`](crate::internal::ai::runtime::hardening::SecretRedactor)
    /// at the audit-write boundary, not here).
    pub reason: String,
}

/// Errors that prevent [`handle_modify_request`] from producing a valid
/// next-revision skeleton.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum ModifyRequestError {
    /// The request's `kind` doesn't match the chain's `kind`. Chains
    /// cannot change kind — a Plan modify request cannot retarget a
    /// Test-Plan chain, etc.
    #[error(
        "modify request kind mismatch: request says {request_kind:?} but chain is {chain_kind:?}"
    )]
    KindMismatch {
        request_kind: RevisionKind,
        chain_kind: RevisionKind,
    },
    /// The request's `logical_id` doesn't match the chain's
    /// `logical_id`. Each chain corresponds to one logical entity; a
    /// modify request must target that entity explicitly.
    #[error(
        "modify request logical_id mismatch: request {request_logical_id} ≠ chain {chain_logical_id}"
    )]
    LogicalIdMismatch {
        request_logical_id: Uuid,
        chain_logical_id: Uuid,
    },
}

/// Coordinator for the modify path of a revision chain.
///
/// Validates that the `request` actually targets the supplied `previous`
/// chain entry (kind + logical_id must match), then returns the next
/// revision's skeleton via [`derive_next_revision_skeleton`]. The
/// caller is responsible for invoking the appropriate Phase 0 / Phase 1
/// formal-write helper to persist the skeleton; this function does NOT
/// touch the persistence layer (mirrors the design rule "the formal-write
/// helper owns persisted-id assignment" called out at
/// [`derive_next_revision_skeleton`]).
///
/// # Errors
///
/// Returns [`ModifyRequestError::KindMismatch`] or
/// [`ModifyRequestError::LogicalIdMismatch`] if the request targets a
/// different chain than `previous` describes — these are caller bugs and
/// the function fails-closed to prevent cross-chain id leakage.
pub fn handle_modify_request(
    previous: &RevisionChainEntry,
    previous_persisted_id: String,
    request: &ModifyRequest,
) -> Result<RevisionChainEntry, ModifyRequestError> {
    if request.kind != previous.kind {
        return Err(ModifyRequestError::KindMismatch {
            request_kind: request.kind,
            chain_kind: previous.kind,
        });
    }
    if request.logical_id != previous.logical_id {
        return Err(ModifyRequestError::LogicalIdMismatch {
            request_logical_id: request.logical_id,
            chain_logical_id: previous.logical_id,
        });
    }
    Ok(derive_next_revision_skeleton(
        previous,
        previous_persisted_id,
    ))
}

/// Derive the metadata for the **next** revision in a chain, given the
/// previous link's own persisted id.
///
/// This is the pure half of [`handle_modify_request`] (still TBD): it
/// answers the question "if I just persisted revision N as
/// `previous_persisted_id`, what should the metadata for revision N+1
/// look like?" without touching the actual persistence layer. The
/// resulting skeleton:
///
/// - inherits `kind` and `logical_id` from `previous` (chain identity is
///   stable),
/// - points `previous_id` at the just-persisted id (so the chain stays
///   linked),
/// - sets `revision = previous.revision + 1` (1-based ordinal, see the
///   [`RevisionChainEntry`] docs).
///
/// Callers that need the **persisted** version of the next revision can
/// pass the skeleton into the appropriate Phase 0 / Phase 1 formal-write
/// helper ([`super::phase0::write_intent`] /
/// [`super::phase1::write_plan_set`]).
///
/// # Why not infer `previous_persisted_id` from `previous.previous_id`
///
/// `RevisionChainEntry.previous_id` points at the parent of `previous`,
/// not at `previous` itself; the persisted id of `previous` is owned by
/// the formal-write helper that produced it. Requiring the caller to pass
/// that id explicitly keeps this function pure and side-effect free, and
/// makes the rule "the formal-write helper owns assignment of persisted
/// ids" explicit at the type system level.
pub fn derive_next_revision_skeleton(
    previous: &RevisionChainEntry,
    previous_persisted_id: String,
) -> RevisionChainEntry {
    RevisionChainEntry {
        kind: previous.kind,
        previous_id: Some(previous_persisted_id),
        revision: previous.revision + 1,
        logical_id: previous.logical_id,
    }
}

/// Errors that prevent [`validate_paired_plan_revisions`] from succeeding.
///
/// The execution plan and test plan are sibling chains that must always
/// move in lockstep (rule 3 of the revision-chain design — see the module
/// docstring). When the rule is violated, this enum's variants distinguish
/// the three failure shapes so callers can surface a precise error message
/// rather than a generic "not paired".
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum PairingError {
    /// The supposed-execution-plan argument actually carries a different
    /// `kind`. Caught first because the rest of the checks assume the
    /// argument really is an execution plan.
    #[error("expected execution-plan entry, got {kind:?}")]
    NotExecutionPlan { kind: RevisionKind },
    /// The supposed-test-plan argument actually carries a different
    /// `kind`.
    #[error("expected test-plan entry, got {kind:?}")]
    NotTestPlan { kind: RevisionKind },
    /// The two entries carry different `revision` ordinals. Revision
    /// numbers must match exactly — never (n−1) or (n+1).
    #[error(
        "execution-plan revision {execution_revision} does not pair with test-plan revision {test_revision}"
    )]
    RevisionMismatch {
        execution_revision: u32,
        test_revision: u32,
    },
}

/// Enforce the "plans and test-plans always rev together" rule across a
/// sibling pair of [`RevisionChainEntry`] values.
///
/// Per rule 3 of the revision-chain design, the (n)-th execution-plan
/// revision must pair with the (n)-th test-plan revision — never (n−1) or
/// (n+1). This helper validates that constraint as a pure check so call
/// sites like [`super::phase1::write_plan_set`] can fail-closed before
/// persisting a desynchronised pair.
///
/// # Errors
///
/// * [`PairingError::NotExecutionPlan`] if `execution.kind` is not
///   [`RevisionKind::ExecutionPlan`].
/// * [`PairingError::NotTestPlan`] if `test.kind` is not
///   [`RevisionKind::TestPlan`].
/// * [`PairingError::RevisionMismatch`] if the two entries carry different
///   `revision` ordinals.
///
/// Kind errors are returned in priority order: execution-plan kind is
/// checked first, then test-plan kind, then revision parity. Callers that
/// pass two malformed inputs will see the execution-side error first; this
/// is deterministic so audit logs stay stable.
pub fn validate_paired_plan_revisions(
    execution: &RevisionChainEntry,
    test: &RevisionChainEntry,
) -> Result<(), PairingError> {
    if execution.kind != RevisionKind::ExecutionPlan {
        return Err(PairingError::NotExecutionPlan {
            kind: execution.kind,
        });
    }
    if test.kind != RevisionKind::TestPlan {
        return Err(PairingError::NotTestPlan { kind: test.kind });
    }
    if execution.revision != test.revision {
        return Err(PairingError::RevisionMismatch {
            execution_revision: execution.revision,
            test_revision: test.revision,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Labels must be stable so audit consumers can grep across phases.
    #[test]
    fn revision_kind_labels_are_stable() {
        assert_eq!(RevisionKind::Intent.label(), "intent");
        assert_eq!(RevisionKind::ExecutionPlan.label(), "execution_plan");
        assert_eq!(RevisionKind::TestPlan.label(), "test_plan");
    }

    /// `is_first()` and `is_continuation()` are mutually exclusive on
    /// well-formed chains: revision 1 + no parent is "first", revision >=2
    /// + parent set is "continuation". Tests both directions plus the
    ///   degenerate case (revision 1 with a parent — represents a forced
    ///   re-derive and is NOT continuation by our convention).
    #[test]
    fn first_and_continuation_flag_chain_position_correctly() {
        let logical_id = Uuid::new_v4();

        let first = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id,
        };
        assert!(first.is_first());
        assert!(!first.is_continuation());

        let continuation = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: Some("plan-prev".to_string()),
            revision: 2,
            logical_id,
        };
        assert!(!continuation.is_first());
        assert!(continuation.is_continuation());

        // Degenerate: revision 1 with a parent set — neither flag fires
        // continuation, so the caller can branch into a "first link of a
        // forked chain" code path.
        let forked = RevisionChainEntry {
            kind: RevisionKind::TestPlan,
            previous_id: Some("plan-prev".to_string()),
            revision: 1,
            logical_id,
        };
        assert!(!forked.is_first());
        assert!(!forked.is_continuation());
    }

    /// `RevisionChainEntry` must derive `Clone` so observer / audit
    /// handlers can keep a snapshot while the caller continues mutating
    /// the chain head.
    #[test]
    fn entry_is_clone() {
        let entry = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: Some("intent-prev".to_string()),
            revision: 3,
            logical_id: Uuid::new_v4(),
        };
        let cloned = entry.clone();
        assert_eq!(cloned, entry);
    }

    /// `derive_next_revision_skeleton` must preserve chain identity
    /// (`kind` + `logical_id`), increment the 1-based ordinal, and point
    /// `previous_id` at the just-persisted id of the input.
    #[test]
    fn derive_next_revision_skeleton_increments_and_links() {
        let logical_id = Uuid::new_v4();
        let previous = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: Some("plan-rev-1".to_string()),
            revision: 2,
            logical_id,
        };

        let next = derive_next_revision_skeleton(&previous, "plan-rev-2".to_string());

        assert_eq!(next.kind, RevisionKind::ExecutionPlan);
        assert_eq!(next.logical_id, logical_id);
        assert_eq!(next.revision, 3);
        assert_eq!(next.previous_id.as_deref(), Some("plan-rev-2"));
        // The skeleton is itself a continuation now (rev > 1 + parent set).
        assert!(!next.is_first());
        assert!(next.is_continuation());
    }

    /// Deriving the next skeleton from a `is_first()` head must still set
    /// `previous_id` (to the persisted id of the head) and produce a
    /// `revision == 2` continuation, with the original first head's
    /// `previous_id: None` left intact.
    #[test]
    fn derive_next_revision_skeleton_from_first_link_promotes_to_continuation() {
        let logical_id = Uuid::new_v4();
        let head = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id,
        };

        let next = derive_next_revision_skeleton(&head, "intent-rev-1".to_string());

        assert_eq!(next.revision, 2);
        assert_eq!(next.previous_id.as_deref(), Some("intent-rev-1"));
        // The original head must still flag as `is_first` — derivation is
        // a pure function.
        assert!(head.is_first());
        // The new skeleton is a continuation.
        assert!(next.is_continuation());
    }

    /// Happy path: request targets the matching chain → returns Ok with
    /// the derived skeleton. Equivalent to calling
    /// `derive_next_revision_skeleton` directly.
    #[test]
    fn handle_modify_request_matching_chain_derives_next_skeleton() {
        let logical_id = Uuid::new_v4();
        let previous = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: None,
            revision: 1,
            logical_id,
        };
        let request = ModifyRequest {
            kind: RevisionKind::ExecutionPlan,
            logical_id,
            reason: "user requested narrower scope".to_string(),
        };

        let next = handle_modify_request(&previous, "plan-rev-1".to_string(), &request)
            .expect("matching request should derive next skeleton");

        assert_eq!(next.revision, 2);
        assert_eq!(next.previous_id.as_deref(), Some("plan-rev-1"));
        assert_eq!(next.kind, RevisionKind::ExecutionPlan);
        assert_eq!(next.logical_id, logical_id);
    }

    /// Cross-kind modify request: chain is `ExecutionPlan` but the
    /// request targets `TestPlan`. Must fail-closed with
    /// `KindMismatch` rather than silently retarget — chains cannot
    /// change kind across revisions.
    #[test]
    fn handle_modify_request_rejects_kind_mismatch() {
        let logical_id = Uuid::new_v4();
        let previous = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: Some("plan-rev-1".to_string()),
            revision: 2,
            logical_id,
        };
        let request = ModifyRequest {
            kind: RevisionKind::TestPlan,
            logical_id,
            reason: "tried to retarget".to_string(),
        };

        let error = handle_modify_request(&previous, "plan-rev-2".to_string(), &request)
            .expect_err("kind mismatch must fail-closed");
        assert_eq!(
            error,
            ModifyRequestError::KindMismatch {
                request_kind: RevisionKind::TestPlan,
                chain_kind: RevisionKind::ExecutionPlan,
            }
        );
    }

    /// `validate_paired_plan_revisions` happy path: an execution-plan
    /// revision N and a test-plan revision N pair cleanly.
    #[test]
    fn validate_paired_plan_revisions_matching_revisions_succeeds() {
        let execution_logical = Uuid::new_v4();
        let test_logical = Uuid::new_v4();
        let execution = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: Some("exec-rev-2".to_string()),
            revision: 3,
            logical_id: execution_logical,
        };
        let test = RevisionChainEntry {
            kind: RevisionKind::TestPlan,
            previous_id: Some("test-rev-2".to_string()),
            revision: 3,
            logical_id: test_logical,
        };

        assert!(validate_paired_plan_revisions(&execution, &test).is_ok());
    }

    /// First-link pair (rev 1 / rev 1) is the canonical "plan compiled
    /// the first time" case — must pair without error.
    #[test]
    fn validate_paired_plan_revisions_accepts_first_link_pair() {
        let execution = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        let test = RevisionChainEntry {
            kind: RevisionKind::TestPlan,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        assert!(validate_paired_plan_revisions(&execution, &test).is_ok());
    }

    /// Off-by-one drift in either direction must fail-closed with
    /// `RevisionMismatch`.
    #[test]
    fn validate_paired_plan_revisions_rejects_drift_by_one() {
        let execution = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: None,
            revision: 2,
            logical_id: Uuid::new_v4(),
        };
        let test_behind = RevisionChainEntry {
            kind: RevisionKind::TestPlan,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        let test_ahead = RevisionChainEntry {
            kind: RevisionKind::TestPlan,
            previous_id: None,
            revision: 3,
            logical_id: Uuid::new_v4(),
        };

        assert_eq!(
            validate_paired_plan_revisions(&execution, &test_behind).unwrap_err(),
            PairingError::RevisionMismatch {
                execution_revision: 2,
                test_revision: 1,
            }
        );
        assert_eq!(
            validate_paired_plan_revisions(&execution, &test_ahead).unwrap_err(),
            PairingError::RevisionMismatch {
                execution_revision: 2,
                test_revision: 3,
            }
        );
    }

    /// A non-execution-plan kind in the first argument must fail-closed
    /// with `NotExecutionPlan` regardless of the second argument.
    #[test]
    fn validate_paired_plan_revisions_rejects_non_execution_plan_first() {
        let intent_as_first = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        let test = RevisionChainEntry {
            kind: RevisionKind::TestPlan,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        assert_eq!(
            validate_paired_plan_revisions(&intent_as_first, &test).unwrap_err(),
            PairingError::NotExecutionPlan {
                kind: RevisionKind::Intent,
            }
        );
    }

    /// A non-test-plan kind in the second argument must fail-closed with
    /// `NotTestPlan` (provided the first argument is a valid execution
    /// plan — execution-side errors take priority).
    #[test]
    fn validate_paired_plan_revisions_rejects_non_test_plan_second() {
        let execution = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        let intent_as_second = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        assert_eq!(
            validate_paired_plan_revisions(&execution, &intent_as_second).unwrap_err(),
            PairingError::NotTestPlan {
                kind: RevisionKind::Intent,
            }
        );
    }

    /// Priority rule: when both kinds are wrong, the execution-side error
    /// must surface first so audit logs stay deterministic.
    #[test]
    fn validate_paired_plan_revisions_execution_kind_error_takes_priority() {
        let intent_as_first = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        let intent_as_second = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id: Uuid::new_v4(),
        };
        assert_eq!(
            validate_paired_plan_revisions(&intent_as_first, &intent_as_second).unwrap_err(),
            PairingError::NotExecutionPlan {
                kind: RevisionKind::Intent,
            }
        );
    }

    /// `audit_label()` happy paths: head, continuation, forked.
    ///
    /// The exact format is part of the public contract (audit consumers
    /// grep on it), so this test pins the expected strings verbatim.
    #[test]
    fn audit_label_format_per_chain_position() {
        // Pinned logical_id so the assertion strings are deterministic.
        let logical_id = "0192345f-1c12-7e8a-9abc-d0c0c0c0c0c0"
            .parse::<Uuid>()
            .expect("valid UUID literal");

        let head = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id,
        };
        assert_eq!(
            head.audit_label(),
            "intent rev 1 (head) for logical_id 0192345f-1c12-7e8a-9abc-d0c0c0c0c0c0",
        );

        let continuation = RevisionChainEntry {
            kind: RevisionKind::ExecutionPlan,
            previous_id: Some("plan-rev-2".to_string()),
            revision: 3,
            logical_id,
        };
        assert_eq!(
            continuation.audit_label(),
            "execution_plan rev 3 (continuation from plan-rev-2) for logical_id 0192345f-1c12-7e8a-9abc-d0c0c0c0c0c0",
        );

        let forked = RevisionChainEntry {
            kind: RevisionKind::TestPlan,
            previous_id: Some("plan-prev".to_string()),
            revision: 1,
            logical_id,
        };
        assert_eq!(
            forked.audit_label(),
            "test_plan rev 1 (forked from plan-prev) for logical_id 0192345f-1c12-7e8a-9abc-d0c0c0c0c0c0",
        );
    }

    /// `audit_label()` must carry the kind label so audit consumers can
    /// grep by phase. The format pins the kind label as the first token.
    #[test]
    fn audit_label_starts_with_kind_label() {
        let logical_id = Uuid::new_v4();
        for (kind, expected_prefix) in [
            (RevisionKind::Intent, "intent rev "),
            (RevisionKind::ExecutionPlan, "execution_plan rev "),
            (RevisionKind::TestPlan, "test_plan rev "),
        ] {
            let entry = RevisionChainEntry {
                kind,
                previous_id: None,
                revision: 7,
                logical_id,
            };
            assert!(
                entry.audit_label().starts_with(expected_prefix),
                "expected audit_label to start with {expected_prefix:?}, got {}",
                entry.audit_label(),
            );
        }
    }

    /// `PairingError` must derive `Clone` + `PartialEq` so callers can
    /// match-and-compare without re-deriving the error from text.
    #[test]
    fn pairing_error_is_clone_and_eq() {
        let err = PairingError::RevisionMismatch {
            execution_revision: 3,
            test_revision: 4,
        };
        let cloned = err.clone();
        assert_eq!(cloned, err);
    }

    /// Cross-entity modify request: request targets a different
    /// `logical_id` than the chain. Must fail-closed with
    /// `LogicalIdMismatch` to prevent cross-chain id leakage.
    #[test]
    fn handle_modify_request_rejects_logical_id_mismatch() {
        let chain_logical_id = Uuid::new_v4();
        let stranger_logical_id = Uuid::new_v4();
        let previous = RevisionChainEntry {
            kind: RevisionKind::Intent,
            previous_id: None,
            revision: 1,
            logical_id: chain_logical_id,
        };
        let request = ModifyRequest {
            kind: RevisionKind::Intent,
            logical_id: stranger_logical_id,
            reason: "tried to modify someone else's chain".to_string(),
        };

        let error = handle_modify_request(&previous, "intent-rev-1".to_string(), &request)
            .expect_err("logical_id mismatch must fail-closed");
        assert_eq!(
            error,
            ModifyRequestError::LogicalIdMismatch {
                request_logical_id: stranger_logical_id,
                chain_logical_id,
            }
        );
    }
}
