//! Stable [`ObservedAgent`](super::ObservedAgent) adapters built into the
//! libra binary. Distinct from `super::preview` which holds the v1
//! preview stubs that return `Err(AgentNotYetImplemented)` for the
//! transcript / hook code paths.
//!
//! Phase 4 (entire.md §14.4) populates this module incrementally; the
//! first entrant is `claude_code`, paired with a `TranscriptTruncator`
//! capability that `libra agent checkpoint rewind --apply` calls.

pub mod claude_code;
pub mod gemini;
pub mod stable_promoted;

pub use claude_code::{
    ClaudeCodeObservedAgent, FlushOutcome, claude_project_slug, claude_session_dir,
    claude_session_file_candidate, claude_session_id_is_safe_path_component, flush_wait,
    resolve_session_file, rfc3339_boundary_for_unix_seconds, write_truncated_transcript,
};
pub use gemini::GeminiObservedAgent;
pub use stable_promoted::{
    STABLE_PROMOTED_SPECS, StablePromotedAgent, find_codex_rollout, stable_promoted_spec_for,
};
