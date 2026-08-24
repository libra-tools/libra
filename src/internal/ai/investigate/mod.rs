//! AG-23 read-only agent investigate workflow engine (plan.md Task A8).
//!
//! This module owns the *engine* half of `libra investigate`: everything
//! below the CLI surface. It is the strict-round-robin sibling of
//! [`crate::internal::ai::review`] and deliberately reuses A7's proven
//! machinery rather than duplicating it:
//!
//! - the isolated-workspace seam (`materialize_isolated_workspace`),
//! - the §0.3.2 read-only real-CLI argv builder + minimal-allowlist spawn
//!   ([`crate::internal::ai::review::build_reviewer_command`] /
//!   [`crate::internal::ai::review::spawn_reviewer`]),
//! - the bounded-sink capture, redaction pipeline, control scrub, and the
//!   untrusted-findings render/spotlight helpers
//!   ([`crate::internal::ai::review::redact_untrusted`],
//!   [`crate::internal::ai::review::render_untrusted_findings`],
//!   [`crate::internal::ai::review::findings_section`]),
//! - the run-directory path-traversal guard and filesystem-safe log
//!   naming ([`store::is_valid_run_id`] / [`store::sanitize_reviewer_name`],
//!   re-exported from the review store).
//!
//! What is genuinely different from review (and therefore lives here):
//!
//! - [`store`] — the round-robin run state (`turn`, `next_agent_idx`,
//!   `completed_rounds`, `quorum`, `stances`, `pending_turn`, …), the
//!   `kind = "investigate"` E8 manifest, single-writer `findings.md`, and
//!   the OS-level per-run lock ([`store::RunLock`]).
//! - [`runner`] — the strict serial turn loop (one investigator at a time,
//!   in agent order), quorum / max-turns terminal classification, stall /
//!   agent-failure PAUSES (resumable via `investigate continue`), the
//!   run-level timeout, the shared cancel/cleanup path, and the
//!   `agent.investigate.run` span (`agent.md` §6).
//!
//! # Provenance
//!
//! The investigation topic is an **untrusted seed** (issue-link / operator
//! text; plan.md:998). It — and every prior investigator stance injected
//! as context — is redacted and wrapped in explicit spotlighting
//! delimiters before it reaches any turn prompt, and rendered through the
//! ANSI-stripping sanitizer before display. A mutating `investigate fix`
//! is refused fail-closed at the CLI (`LBR-AGENT-010` /
//! `LBR-AGENT-011`) — the engine here is strictly read-only.

pub mod runner;
pub mod store;

pub use runner::{
    DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD, DEFAULT_INVESTIGATOR_TIMEOUT, InvestigateCancelHandle,
    InvestigateRunError, InvestigateRunOutcome, InvestigateRunRequest, InvestigatorSource,
    continue_investigate, continue_investigate_with_sources, is_launchable_investigator,
    run_investigate,
};
pub use store::{
    INVESTIGATE_FINDINGS_DOC, INVESTIGATE_MANIFEST_SCHEMA_VERSION, INVESTIGATE_RUN_KIND,
    InvestigateManifest, InvestigateRunCursor, InvestigateRunPage, InvestigateRunState,
    InvestigateRunStore, InvestigateRunSummary, InvestigateTerminalState, PauseReason, PendingTurn,
    RedactionReportSummary, RunLock, StanceDisposition, StanceEntry, classify_stance_disposition,
    is_valid_run_id, sanitize_reviewer_name,
};

// The untrusted-findings render helper is the review pipeline's; re-export
// it here so the CLI slice imports every display/redaction primitive from
// one investigate-facing path.
pub use crate::internal::ai::review::render_untrusted_findings;

/// RFC 3339 UTC micro-precision timestamp — the fixed-width format the run
/// store uses so lexicographic order equals chronological order (shared
/// with the review store's keyset contract).
pub(crate) fn store_now() -> String {
    crate::internal::ai::review::store::utc_timestamp()
}
