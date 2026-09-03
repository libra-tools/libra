//! Provider-aware context budget planning and allocation.
//!
//! CEX-13a defined the segment budget contract and deterministic allocation
//! behavior. CEX-13b layers append-only context frames, attachment references,
//! and compaction replay records on top of that core. CEX-13c adds reviewed
//! memory anchors for cross-turn semantic constraints.

pub mod allocator;
pub mod budget;
pub mod compaction;
pub mod compaction_agent;
pub mod frame;
pub mod handoff;
// Replay/inspection accessors remain staged for the M2-13 command adapter.
#[allow(dead_code)]
pub(crate) mod memory;
pub mod memory_anchor;
pub mod projection;
// M2-02R freezes the shared receipt seam before M2-12 wires retrieval into
// prompt assembly. Keep the staged crate-private implementation lint-clean
// without exposing it as a public API prematurely.
#[allow(dead_code)]
pub(crate) mod receipt;
#[allow(dead_code)]
pub(crate) mod receipt_store;

pub use allocator::{
    AllocationOmissionReason, ContextAllocation, ContextAllocationOmission, ContextBudgetAllocator,
    ContextBudgetCandidate,
};
pub use budget::{
    ContextBudget, ContextBudgetError, ContextPriority, ContextSegmentBudget, ContextSegmentKind,
    ProviderContextCapability, SAFETY_MARGIN_TOKENS, TruncationPolicy,
};
pub use compaction::{
    CompactionEvent, CompactionReason, DEFAULT_TAIL_TURNS, MAX_PRESERVE_RECENT_TOKENS,
    MIN_PRESERVE_RECENT_TOKENS, PRUNE_MINIMUM, PRUNE_PROTECT, PRUNE_PROTECTED_TOOLS,
    TOOL_OUTPUT_MAX_CHARS, preserve_recent_budget,
};
pub use compaction_agent::{
    COMPACTION_AGENT_NAME, CompactionAgentError, EMBEDDED_COMPACTION_PROFILE,
    compaction_event_for_handoff, embedded_compaction_system_prompt, run_compaction,
};
pub use frame::{
    ContextAttachmentRef, ContextAttachmentStore, ContextFrameBuilder, ContextFrameCandidate,
    ContextFrameEvent, ContextFrameKind, ContextFrameOmission, ContextFrameSegment,
    ContextFrameSource, ContextFrameSourceKind, ContextTrustLevel,
};
pub use handoff::{
    ContextHandoff, ContextHandoffBuilder, ContextHandoffParseError, ParsedSection, ParsedSummary,
    parse_handoff_template,
};
pub(crate) use memory::AuditedMemoryContextBundleV1;
pub use memory_anchor::{
    MemoryAnchor, MemoryAnchorAction, MemoryAnchorConfidence, MemoryAnchorDraft, MemoryAnchorEvent,
    MemoryAnchorKind, MemoryAnchorLookupError, MemoryAnchorReplay, MemoryAnchorReviewState,
    MemoryAnchorScope, build_memory_anchor_prompt_section,
};
pub use projection::{
    MessageProjection, ProjectionKind, PruneResult, compaction_event_to_projection,
    filter_compacted, prune_inline_tool_output,
};
