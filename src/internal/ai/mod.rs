//! AI capture, review, investigate, sandbox, and session infrastructure.
//!
//! The Code UI / AgentRuntime executor SCC was deleted in plan-20260920
//! RC-23. This module now exports KEEP surfaces: hooks, observed agents,
//! review, investigate, sandbox, automation, session, completion types,
//! and source security/resolver helpers.

/// DeepSeek Harness bridge protocol/transport authority (plan-20260818 LB-01).
pub mod agent_bridge;
// Rule-driven automation MVP for hooks, cron, and source-triggered workflows.
pub mod automation;
// Step 2 sub-agent contracts. Runtime wiring lived in `agent/runtime` (RC-23).
pub mod agent_run;
// Historical external-agent transcript import orchestration (M4 / DR-05).
pub mod agent_import;
// Provider-root subagent transcript discovery and source-scoped content
// revisions (plan-20260713 M5 / DR-06).
pub mod subagent_content;
// Canonical repo/worktree/workspace ownership for capture/import/export rows.
pub mod capture_scope;
// PD-02 checkpoint-scoped review/investigate input materialization.
pub mod checkpoint_input;
// Completion-model trait and request/response types.
pub mod completion;
// Per-turn coverage claim gate for external-agent checkpoint writers.
pub mod coverage_gate;
// Append-only event trait (plan-20260920 RC-01).
pub mod event;
// OpenCode export-bridge job coordination (plan-20260713 DR-04b, ADR-DR-11).
pub mod export_job;
// Conversation history datastructures (compaction, persistence, replay).
pub mod history;
// `refs/libra/traces` persistence API (plan-20260920 RC-02).
pub mod traces;
// Isolated workspace helper (plan-20260920 RC-03 / RC-23).
pub mod workspace_isolation;
// Capture-side tool-call projection (plan-20260920 RC-04).
pub mod tool_call_record;
// Runtime-owned AI causality identifiers (plan-20260920 RC-06).
pub mod operation_context;
// Authorization / tool-boundary / audit contracts (plan-20260920 RC-09).
pub mod hardening;
// Shell command safety classification (plan-20260920 RC-09).
pub mod command_safety;
// KEEP permission / tool schema (plan-20260920 RC-19).
pub mod permission_spec;
pub mod tool_definition;
// Git hooks integration that lets the agent observe commit events.
pub mod hooks;
// External-Agent capture (CEX-EntireIO): contracts and redaction engine.
pub mod observed_agents;
// Permission ruleset machinery (OC-Phase 2 P2.3).
pub mod permission;
// AG-22 read-only agent review workflow engine.
pub mod review;
pub mod run_admission;
// AG-23 read-only agent investigate workflow engine.
pub mod investigate;
// Filesystem/network sandbox shared by capture and review.
pub mod sandbox;
// Source security + config resolver (SourcePool deleted in RC-23).
pub mod sources;
// Per-session persistent state.
pub mod session;
// Misc utilities used across the AI module.
pub mod util;

pub use completion::{Chat, CompletionModel, Message, Prompt};
