//! `AgentEvidence[E]` event — wraps the persistent `Evidence` snapshot with
//! sub-agent provenance fields required by S2-INV-12 (raw fact chain) and
//! Step 2.8 evidence query.
//!
//! # R-A4 disambiguation note
//!
//! There are two `EvidenceKind` types in this codebase, both legitimate but
//! semantically different:
//!
//! - `git_internal::internal::object::evidence::EvidenceKind` — the
//!   persistent classification stored on the `Evidence` snapshot
//!   (`Test` / `Lint` / `Build` / `Other(String)`). This is what we wrap.
//! - A Code-era runtime-side `EvidenceKind` (deleted with RC-23) used to tag
//!   Phase 3/4 dispatch (`Test` / `Lint` / `Build` / `Security` /
//!   `Performance` / various internal failure variants). It was never the
//!   schema source.
//!
//! Per the audit closure, callers that need both types in the same file
//! MUST `use ... as ...` to avoid name collisions. We follow that pattern
//! below.

use serde::{Deserialize, Serialize};

use super::{AgentRunId, AnchorScope, Confidence, EventId, EvidenceId, SourceCallId, ToolCallId};

/// Three sub-agent role types per S2-INV-05 / Step 2.2 tool policy.
/// Recorded on every `AgentEvidence` so Phase 3 / Phase 4 can filter by role
/// (e.g., reviewer evidence does not count toward production patch test
/// coverage).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AgentType {
    Explorer,
    Worker,
    Reviewer,
}

/// Sub-agent evidence event.
///
/// Wraps — does not extend — the persistent `Evidence` snapshot. The wrapper
/// adds the raw-fact-chain fields (`source_event_id` / `tool_call_id` /
/// `source_call_id` / `confidence` / `applies_to_scope` / `distillable` /
/// `agent_run_id` / `source_agent_type`) so a future Step 3.D Memory
/// Distillation can consume these events without re-parsing transcripts.
///
/// # Why a wrapper instead of `serde(flatten)`
///
/// `git_internal::Evidence` carries `#[serde(deny_unknown_fields)]`; flattening
/// extra fields would cause runtime deserialization failures. A wrapper keeps
/// the upstream schema intact and the extension fields cleanly attributed to
/// the agent layer.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentEvidence {
    /// Stable id for this evidence event.
    pub id: EvidenceId,

    /// Owning sub-agent run.
    pub agent_run_id: AgentRunId,

    /// Sub-agent type that produced this evidence.
    pub source_agent_type: AgentType,

    /// JSONL event id this evidence derives from. Required by S2-INV-12.
    pub source_event_id: EventId,

    /// Tool call id, if the evidence comes from a specific tool invocation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<ToolCallId>,

    /// Source Pool call id, if the evidence comes from a Source Pool fetch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_call_id: Option<SourceCallId>,

    /// Sub-agent self-reported confidence + verification adjustment.
    pub confidence: Confidence,

    /// Anchor scope this evidence applies to. Aligns with Step 1.9
    /// `MemoryAnchor` scope per `AnchorScope`.
    pub applies_to_scope: AnchorScope,

    /// Sub-agent's recommendation about whether this evidence is worth
    /// distilling into long-lived memory. Step 2.8 read API surfaces this;
    /// Step 3.D may consume it.
    #[serde(default)]
    pub distillable: bool,

    /// `Evidence` snapshot id (UUID assigned by the persistent layer; resolves
    /// to a `git_internal::Evidence` via the AI orphan branch). Consumers that
    /// need the persistent kind / tool / report should resolve the snapshot
    /// rather than caching a copy here, to avoid drift (R-A4).
    pub evidence_snapshot_id: uuid::Uuid,
}
