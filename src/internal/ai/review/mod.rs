//! AG-22 read-only agent review workflow engine (plan.md Task A7).
//!
//! This module owns the *engine* half of `libra review`: everything below
//! the CLI surface. It provides:
//!
//! - [`store`] — the `.libra/sessions/agent-runs/<run_id>/` run directory
//!   store (`state.json`, `manifest.json` with the exact E8 key set,
//!   `findings.md`, `reviewers/<slug>.stdout.redacted.log` +
//!   `.stderr.redacted.log`), including keyset-ordered listing
//!   (`created_at DESC, run_id DESC`) for the CLI pagination contract.
//! - [`launcher`] — the production real-CLI argv builder per
//!   `docs/development/tracing/plan.md` §0.3.2 (codex / claude-code /
//!   opencode read-only spawn shapes; forbidden flags never present;
//!   non-first-batch slugs are a structured unsupported error, never a
//!   spawn) plus the minimal-allowlist spawn skeleton.
//! - [`sink`] — bounded per-reviewer capture buffers (64 KiB per sink,
//!   `agent.md` 强制补强项 #12), redaction + control-character scrubbing
//!   for everything persisted, and the ANSI-strip render helper for
//!   untrusted `findings.md` content.
//! - [`runner`] — the concurrent fan-in → serial-sink run loop with the
//!   five terminal states (`success` / `error` / `cancelled` / `timeout`
//!   / `partial`), the shared cancel/cleanup path (used by both
//!   `review cancel` and foreground SIGINT/SIGTERM), and the
//!   `agent.review.run` span (`agent.md` §6).
//!
//! # Mandatory isolation
//!
//! Reviewers always run inside an isolated workspace materialized through
//! [`materialize_isolated_workspace`] (the public seam extracted from the
//! sub-agent dispatcher per plan.md:946) — never the main worktree. The
//! copy backend's ignore-aware walk keeps gitignored secret files (e.g.
//! `.env.test`) out of the workspace; the reviewer environment is cleared
//! down to a documented allowlist; redaction of persisted output is only
//! the last line of defense.
//!
//! # Provenance
//!
//! Reviewer stdout is free text and **untrusted**. `findings.md` stores
//! the raw-redacted text inside explicit spotlighting delimiters; callers
//! that display it (the CLI slice's `review show`) must render it through
//! [`sink::render_untrusted_findings`], which strips ANSI/terminal
//! control sequences.

pub mod launcher;
pub mod runner;
pub mod sink;
pub mod store;

// The reviewer-facing path to the mandatory isolation seam (declared and
// documented at its owning site in the sub-agent dispatcher).
pub use launcher::{
    CODEX_LAST_MESSAGE_FILE, DEFAULT_CLAUDE_REVIEW_MAX_BUDGET_USD, DEFAULT_REVIEWER_TIMEOUT,
    FORBIDDEN_REVIEWER_FLAGS, REVIEWER_ENV_ALLOWLIST, ReviewerCommand, ReviewerLaunchError,
    ReviewerLaunchPlan, SpawnedReviewer, build_reviewer_command, is_launchable_reviewer,
    process_start_ticks, reviewer_env_allowlist, spawn_reviewer, unsupported_reviewer_error,
};
pub use runner::{
    DEFAULT_MAX_CONCURRENT_REVIEWERS, OrphanedRunCancel, OrphanedWorkspaceAction,
    ReviewCancelHandle, ReviewRunError, ReviewRunOutcome, ReviewRunRequest, ReviewerReport,
    ReviewerSource, cancel_orphaned_run, run_review,
};
pub use sink::{
    BoundedSinkBuffer, REVIEW_SINK_BUFFER_BYTES, REVIEW_SINK_TRUNCATION_MARKER,
    UNTRUSTED_FINDINGS_CLOSE, UNTRUSTED_FINDINGS_OPEN_PREFIX, drain_capped, findings_section,
    redact_for_log, redact_untrusted, render_untrusted_findings, scrub_controls,
};
pub use store::{
    REVIEW_MANIFEST_SCHEMA_VERSION, REVIEW_RUN_KIND, RedactionReportSummary, ReviewManifest,
    ReviewRunCursor, ReviewRunPage, ReviewRunState, ReviewRunStore, ReviewRunSummary,
    ReviewTerminalState, ReviewerOutcome, ReviewerStateEntry, aggregate_terminal_state,
    is_valid_run_id, sanitize_reviewer_name,
};

pub use crate::internal::ai::agent::runtime::sub_agent_dispatcher::materialize_isolated_workspace;
