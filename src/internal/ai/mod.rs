//! AI Agent Infrastructure for Libra.
//!
//! This module is the root of every AI-related capability used by `libra code`,
//! the MCP server, and the workflow orchestrator. It is organised as a small set
//! of focused submodules that snap together at runtime:
//!
//! - **Agent framework** ([`agent`]): the [`Agent`] / [`AgentBuilder`] /
//!   [`ChatAgent`] types that wrap a provider's completion model with tools,
//!   preamble injection, and message history.
//! - **Providers** ([`providers`]): one submodule per LLM backend (`gemini`,
//!   `openai`, `anthropic`, `deepseek`, `kimi`, `zhipu`, `ollama`, ...) — each
//!   implements [`CompletionModel`] so the rest of the stack stays
//!   provider-agnostic.
//! - **Completion contracts** ([`completion`]): the [`CompletionModel`], [`Chat`],
//!   [`Prompt`], and [`Message`] traits/types that providers must satisfy.
//! - **Tools** ([`tools`]): the registry and handlers (apply patch, shell, read
//!   file, grep, ...) plus the [`tools::ToolOutput`] type returned to callers.
//! - **Orchestrator** ([`orchestrator`]): the multi-phase IntentSpec / plan /
//!   execute pipeline with DAG-aware task scheduling.
//! - **Codex** ([`codex`]) and **MCP** ([`mcp`]): managed-runtime adapter and
//!   Model Context Protocol server, respectively, both built on top of the
//!   common [`Agent`] abstraction.
//! - **Prompt engineering** ([`prompt`]) and **commands** ([`commands`]): YAML
//!   slash-command parsing, prompt template rendering, and built-in agent
//!   commands.
//! - **Hooks** ([`hooks`]) and **sandbox** ([`sandbox`]): git-hook integration
//!   plus filesystem/network sandboxing primitives shared by tool handlers.
//! - **Session / history / projections** ([`session`], [`history`],
//!   [`projection`]): durable state on disk, message history compaction, and
//!   read-side projections for the TUI.
//! - **Runtime / web / VCS adapters** ([`runtime`], [`web`], [`libra_vcs`],
//!   [`node_adapter`]): glue that connects the agent to the surrounding
//!   environment (process supervisor, web UI, Libra repo, workflow DAG nodes).
//! - **IntentSpec types** ([`intent`], [`intentspec`], [`workflow_objects`],
//!   [`workspace_snapshot`], [`generated_artifacts`]): structured plan/intent
//!   specifications and their persisted representations.
//!
//! # Example
//! ```no_run
//! use libra::internal::ai::{AgentBuilder, providers::gemini::Client};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let client = Client::from_env()?;
//! let model = client.completion_model("gemini-2.5-flash");
//! let agent = AgentBuilder::new(model)
//!     .preamble("You are a helpful assistant")
//!     .temperature(0.7)?
//!     .build();
//! # Ok(())
//! # }
//! ```

// Agent framework: Agent, AgentBuilder, ChatAgent and their builders.
pub mod agent;
// Rule-driven automation MVP for hooks, cron, and source-triggered workflows.
pub mod automation;
// Step 2 sub-agent contracts (CEX-S2-10 schema-only scaffold) plus the
// runtime extensions that landed with the OC orchestration runtime
// (`feat(code): land opencode orchestration runtime`). Originally
// gated behind the `subagent-scaffold` Cargo feature, but the
// runtime entrypoints (`agent_run::AgentRunId`, `AgentRunEvent`,
// `AgentRunEventEnvelope`) are now referenced ungated by
// `agent/runtime/sub_agent.rs` and `session/jsonl.rs`, so the module
// is unconditionally available.
pub mod agent_run;
// Capability packages (CEX-S2-17): auditable, checksum-verified bundles of
// skills / commands / sources / sub-agent definitions plus the per-repo
// installed-package store the session bootstrap reads.
pub mod capability_package;
// Generic LLM client helpers shared across providers.
pub mod client;
// Adapter for the managed Codex provider runtime.
pub mod codex;
// YAML-defined slash commands and dispatcher.
pub mod commands;
// Completion-model trait and request/response types every provider implements.
pub mod completion;
// Provider-aware prompt context budget planning and allocation.
pub mod context_budget;
// OC-Phase 6 Goal mode runtime contract (P6.1 schema only).
// Schema lives here; supervisor / verifier / tools / CLI land in P6.2-P6.7.
pub mod goal;
// Crate-private helpers for capturing artifacts produced by tool calls.
pub(crate) mod generated_artifacts;
// Per-turn coverage claim gate for external-agent checkpoint writers
// (plan-20260713 DR-05c-0).
pub mod coverage_gate;
// Conversation history datastructures (compaction, persistence, replay).
pub mod history;
// Git hooks integration that lets the agent observe commit events.
pub mod hooks;
// IntentSpec primitive types (Phase 0 / "what does the user want?").
pub mod intent;
// Structured IntentSpec schema, parsing, and review flow.
pub mod intentspec;
// VCS-side helpers used by tools that touch the repository.
pub mod libra_vcs;
// Model Context Protocol server exposing Libra to MCP-aware clients.
pub mod mcp;
// Adapter that lets agents participate as nodes in the workflow DAG.
pub mod node_adapter;
// Phase 0/1/2 orchestrator: intent -> plan -> execute pipeline.
pub mod orchestrator;
// External-Agent capture (CEX-EntireIO): contracts and redaction engine for
// observing externally-hosted agents (Claude Code, Gemini CLI, …).
pub mod observed_agents;
// Read-only projections of session state for UI consumers.
pub mod projection;
// Permission ruleset machinery (OC-Phase 2 P2.3): types + evaluate / disabled algorithms.
pub mod permission;
// Prompt templates and rendering helpers.
pub mod prompt;
// One submodule per LLM backend; each implements CompletionModel.
pub mod providers;
// AG-22 read-only agent review workflow engine (run store, reviewer
// launcher, fan-in sink, terminal states).
pub mod review;

pub mod run_admission;
// AG-23 read-only agent investigate workflow engine (strict round-robin
// run store, turn loop, quorum/max-turns/pause states) — reuses review's
// launcher/sink/isolation machinery.
pub mod investigate;
// Process-level runtime for long-running agents.
pub mod runtime;
// Filesystem/network sandbox shared by every tool handler.
pub mod sandbox;
// Markdown skills with tool-policy metadata and scanner warnings.
pub mod skills;
// Source Pool for MCP / REST / local-doc capability providers.
pub mod sources;
// Per-session persistent state.
pub mod session;
// Tool registry + handlers (ApplyPatch, Shell, ReadFile, ...).
pub mod tools;
// Provider-neutral usage persistence, aggregation, and display helpers.
pub mod usage;
// Misc utilities used across the AI module.
pub mod util;
// Optional embedded web UI for collaboration.
pub mod web;
// Persisted workflow object types (plans, executions, results).
pub mod workflow_objects;
// Snapshot of the workspace consumed by tool calls and validators.
pub mod workspace_snapshot;

// Curated public surface: re-exports kept stable across patch releases.
pub use agent::{Agent, AgentBuilder, ChatAgent};
pub use completion::{Chat, CompletionModel, Message, Prompt};
pub use node_adapter::{AgentAction, ToolLoopAction};
