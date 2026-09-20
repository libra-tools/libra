//! Step 2 sub-agent contracts (CEX-S2-10 schema-first runtime contracts).
//!
//! # Status
//!
//! This module is **schema-only**: it defines the data types Step 2 will use,
//! and the event vocabulary now used by the OC-Phase 3 sub-agent dispatcher.
//! The richer task / patchset / merge schemas remain schema-first contracts;
//! production runtime wiring still lives in `agent::runtime`.
//!
//! # CP-4 gate violation
//!
//! Per `docs/development/tracing/agent.md` "Step 2 audit closure (CEX-S2-00 / 01 / 02)",
//! all CEX-S2-10..18 Runtime task cards are gated on **CP-4** (Step 1 single-
//! agent gate). Step 1 is currently incomplete (multiple `未开始` cards in the
//! milestone index). This file ships the schema scaffold ahead of CP-4 by
//! explicit user request to unblock parallel design work; production wiring
//! must wait for Step 1 to finish. The feature gate keeps the violation
//! invisible to default builds.
//!
//! # Schema-ownership notes
//!
//! - `AgentEvidence` (in `evidence.rs`) **wraps** the persistent
//!   `git_internal::internal::object::evidence::Evidence` Snapshot rather than
//!   forking a parallel schema. The wrapper adds the raw-fact-chain fields
//!   required by S2-INV-12. See R-A4 in the audit closure for why we don't
//!   touch the runtime `EvidenceKind` enum here.
//! - `AgentTask` / `AgentRun` / `AgentPatchSet` reference (not copy) the
//!   existing `IntentSpec` / `Plan` / `Task` / `Run` / `PatchSet` Snapshots
//!   from `git-internal`.
//! - `MergeDecision` event payload starts as `MergeDecisionPayloadV0` stub;
//!   CEX-S2-13 fills the real payload. We only freeze the **field shape** of
//!   `risk_score` / `conflict_list` / `test_evidence` /
//!   `distillable_evidence_ids` here per CEX-S2-13 ownership rule.
//! - `Event` / `Snapshot` traits owned by CEX-00.5 are **not** introduced in
//!   this scaffold; types here implement only `Serialize` + `Deserialize` and
//!   will pick up the trait bound when CEX-00.5 lands.
//!
//! # Unknown-event-safe pattern
//!
//! Two layers, satisfying S2-INV-10:
//! - `AgentRunEvent` uses `#[serde(tag = "kind", content = "payload")]` for
//!   the recognized variants.
//! - `AgentRunEventEnvelope` is the wire-level wrapper readers should parse;
//!   it is `#[serde(untagged)]` over `Known(AgentRunEvent)` and
//!   `Unknown(Value)` so unknown future tags fall through cleanly without
//!   losing the raw payload.
//!
//! This is the canonical pattern CEX-00.5 will lift to the `Event` trait.
//! `#[serde(other)]` on the inner enum cannot do this on its own because
//! future variants will carry payloads (maps), and `#[serde(other)]` requires
//! a unit catch-all.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub mod budget;
pub mod context_pack;
pub mod decision;
pub mod event;
pub mod event_store;
pub mod evidence;
pub mod evidence_query;
pub mod parallel;
pub mod patchset;
pub mod permission;
pub mod run;
pub mod task;
pub mod workspace_strategy;

// ----------------------------------------------------------------------------
// Newtype IDs
// ----------------------------------------------------------------------------

macro_rules! uuid_newtype {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                Self(uuid)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

uuid_newtype!(
    /// Identifier for an `AgentTask` (Phase 2 dispatch unit derived from a
    /// confirmed `Task`).
    AgentTaskId
);
uuid_newtype!(
    /// Identifier for an `AgentRun` (one sub-agent execution attempt).
    AgentRunId
);
uuid_newtype!(
    /// Identifier for an `AgentPatchSet` (sub-agent output staged in isolated
    /// workspace).
    AgentPatchSetId
);
uuid_newtype!(
    /// Identifier for a `MergeCandidate` (Layer 1 aggregate of one or more
    /// `AgentPatchSet`s).
    MergeCandidateId
);
uuid_newtype!(
    /// Identifier for an `ApprovalRequest` raised by a sub-agent. Approver
    /// `agent_run_id` MUST differ from request originator (S2-INV-06).
    ApprovalRequestId
);
uuid_newtype!(
    /// Identifier for an `AgentEvidence` event.
    EvidenceId
);
uuid_newtype!(
    /// Identifier for any append-only event in the JSONL stream. Backreferenced
    /// by `AgentEvidence::source_event_id`.
    EventId
);
uuid_newtype!(
    /// Identifier for one tool call dispatch. Component of the trace id chain
    /// `thread_id → agent_run_id → tool_call_id → source_call_id`.
    ToolCallId
);
uuid_newtype!(
    /// Identifier for one Source Pool call. Trailing component of the trace id
    /// chain.
    SourceCallId
);
uuid_newtype!(
    /// Identifier for a `Decision[E]` event (final merge / phase-4 decision).
    DecisionId
);

// ----------------------------------------------------------------------------
// Forward-declared cross-CEX types
// ----------------------------------------------------------------------------

/// Capability package identifier.
///
/// Forward-declared per CEX-S2-10 (5): CEX-S2-17 will replace the inner shape
/// with the real manifest-derived id but must keep the public type signature
/// compatible. Today this is a wrapper over a `String` slug.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct PackageId(pub String);

/// SHA-256 digest carried in `HookInvocationPayload::hook_checksum` and other
/// integrity fields. Stored as the 64-character lowercase hex string to keep
/// JSON serialization stable and human-readable.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Sha256(pub String);

/// Anchor scope for evidence, mirroring Step 1.9 `MemoryAnchor` scope so
/// distillation downstream (Step 3.D) can consume `AgentEvidence` directly.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AnchorScope {
    Session,
    AgentRun,
    Project,
}

/// Confidence score attached to evidence (sub-agent self-assessment +
/// verification result, range `0.0..=1.0`).
///
/// The inner `f32` is **private**: the only ways to build a
/// `Confidence` are [`Confidence::new`] and deserialization, both of
/// which route through the clamping in `new` so the `0.0..=1.0`
/// invariant cannot be bypassed by a struct literal or an out-of-range
/// JSON value.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
#[serde(transparent)]
pub struct Confidence(f32);

impl Confidence {
    /// Construct a `Confidence`, clamping into the documented
    /// `0.0..=1.0` range.
    ///
    /// `NaN` is mapped to `0.0` rather than passed through: `f32::clamp`
    /// *propagates* `NaN`, which would leave a confidence outside the
    /// invariant range and silently break the ordering / thresholding
    /// that Phase 3 / Phase 4 evidence ranking relies on. Treating a
    /// non-numeric self-assessment as the lowest confidence is the
    /// conservative choice — a garbage score must never read as
    /// high-confidence evidence.
    pub fn new(value: f32) -> Self {
        if value.is_nan() {
            return Self(0.0);
        }
        Self(value.clamp(0.0, 1.0))
    }

    /// The clamped score in `0.0..=1.0`.
    pub fn value(self) -> f32 {
        self.0
    }
}

impl<'de> Deserialize<'de> for Confidence {
    /// Route deserialization through [`Confidence::new`] so an
    /// out-of-range or `NaN` value persisted in a JSONL transcript is
    /// clamped on read, not trusted verbatim. Without this a corrupt
    /// `"confidence": 2.5` (or `-1.0`) would reconstruct an
    /// out-of-invariant `Confidence` that the `pub` constructor would
    /// never have produced.
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Confidence::new(f32::deserialize(deserializer)?))
    }
}

// ----------------------------------------------------------------------------
// Re-exports for downstream consumers
// ----------------------------------------------------------------------------

pub use budget::{AgentBudget, BudgetDimension};
pub use context_pack::AgentContextPack;
pub use decision::{
    Conflict, MergeCandidate, MergeDecision, MergeDecisionPayloadV0, ReviewState, RiskScore,
};
pub use event::{
    AgentRunEvent, AgentRunEventEnvelope, CancellationReason, FailureReason, HookFailureReason,
    HookInvocationPayload, HookKind, HookPhase, PostToolReason, RunUsage, WorkspaceMaterialized,
    WorkspaceStrategy,
};
pub use evidence::{AgentEvidence, AgentType};
pub use evidence_query::{
    EvidenceFilter, evidence_query_by_scope, evidence_stream, merge_decision_distillable_evidence,
};
pub use parallel::{
    ParallelAdmissionConfig, ParallelAdmissionDecision, ParallelQueueReason, ParallelRunState,
    ParallelSchedulerState, ParallelTaskRequest,
};
pub use patchset::AgentPatchSet;
pub use permission::AgentPermissionProfile;
pub use run::{AgentRun, AgentRunStatus};
pub use task::AgentTask;
pub use workspace_strategy::{
    SPARSE_FILE_COUNT_THRESHOLD, SPARSE_REPO_SIZE_THRESHOLD_BYTES, WorkspaceSizing,
    WriteScopeViolation, check_write_in_scope, record_materialization, resolve_full_copy_fallback,
    select_preferred_strategy,
};

#[cfg(test)]
mod tests {
    use super::Confidence;

    /// `Confidence::new` clamps into the documented `0.0..=1.0` range:
    /// values above 1.0 saturate to 1.0, below 0.0 to 0.0, and an
    /// in-range value is preserved exactly.
    #[test]
    fn confidence_new_clamps_into_unit_range() {
        assert_eq!(Confidence::new(2.5).value(), 1.0);
        assert_eq!(Confidence::new(1.0).value(), 1.0);
        assert_eq!(Confidence::new(0.42).value(), 0.42);
        assert_eq!(Confidence::new(0.0).value(), 0.0);
        assert_eq!(Confidence::new(-3.0).value(), 0.0);
    }

    /// `NaN` must be mapped to `0.0`, not propagated. `f32::clamp`
    /// returns `NaN` for a `NaN` input, which would leave a confidence
    /// outside `0.0..=1.0` and break evidence ranking comparisons —
    /// pin the conservative NaN→0.0 mapping so a refactor back to a
    /// bare `clamp` regresses here.
    #[test]
    fn confidence_new_maps_nan_to_zero() {
        let c = Confidence::new(f32::NAN);
        assert!(!c.value().is_nan(), "confidence must never store NaN");
        assert_eq!(c.value(), 0.0);
    }

    /// Infinities clamp to the range bounds (`+inf` → 1.0, `-inf` →
    /// 0.0) — `f32::clamp` handles these correctly (unlike NaN), but
    /// pin them so the documented invariant covers every non-finite
    /// input.
    #[test]
    fn confidence_new_clamps_infinities() {
        assert_eq!(Confidence::new(f32::INFINITY).value(), 1.0);
        assert_eq!(Confidence::new(f32::NEG_INFINITY).value(), 0.0);
    }

    /// Deserialization routes through `new`, so an out-of-range value
    /// persisted in a transcript is clamped on read rather than trusted
    /// verbatim. Pins the defense-in-depth that the private field +
    /// custom `Deserialize` provide (a struct literal can't bypass the
    /// invariant, and neither can a corrupt JSON value).
    #[test]
    fn confidence_deserialize_clamps_out_of_range() {
        let over: Confidence = serde_json::from_str("2.5").expect("deserialize over-range");
        assert_eq!(over.value(), 1.0);

        let under: Confidence = serde_json::from_str("-1.0").expect("deserialize under-range");
        assert_eq!(under.value(), 0.0);

        let in_range: Confidence = serde_json::from_str("0.5").expect("deserialize in-range");
        assert_eq!(in_range.value(), 0.5);

        // Serialize round-trips the clamped value transparently.
        assert_eq!(serde_json::to_string(&in_range).unwrap(), "0.5");
    }
}
