//! Hook system for event-driven automation around the AI agent.
//!
//! Hooks are external shell commands triggered by lifecycle events (tool use,
//! session start/end). They receive a JSON payload on stdin and can optionally
//! block operations (`PreToolUse` only, signalled via exit code 129).
//!
//! Hook configuration is loaded from:
//! 1. `{working_dir}/.libra/hooks.json` (project-local)
//! 2. `~/.config/libra/hooks.json` (user-global)
//!
//! Both tiers are merged — hooks from all tiers are collected and executed.
//!
//! Submodule overview:
//! - [`config`]: deserialise hook definitions from `hooks.json`.
//! - [`event`]: wire-format types for the hook stdin/stdout contract.
//! - [`lifecycle`]: agent-agnostic lifecycle event normalisation.
//! - [`provider`] + [`providers`]: per-LLM-provider hook adapters that translate the
//!   provider's native hook taxonomy into Libra's canonical events.
//! - [`runner`]: spawns hook commands and translates their exit codes into
//!   [`event::HookAction`].
//! - [`runtime`]: turns stdin envelopes into recorded session updates.
//! - `setup`: helper for materialising hook scripts on disk during `libra code`
//!   bootstrap.

pub mod config;
pub mod event;
pub mod lifecycle;
pub mod provider;
pub mod providers;
pub mod runner;
pub mod runtime;
mod setup;

pub use config::{HookConfig, HookDefinition, load_hook_config};
pub use event::{HookAction, HookEvent, HookInput};
pub use lifecycle::{LifecycleEvent, LifecycleEventKind, SessionHookEnvelope};
pub use provider::{HookProvider, ProviderHookCommand, ProviderInstallOptions};
pub use providers::{claude_provider, gemini_provider};
pub use runner::HookRunner;
pub use runtime::{
    AI_SESSION_SCHEMA, AI_SESSION_TYPE, HookEnvelopeInvalid, HookTarget, build_ai_session_id,
    process_hook_event_from_stdin, process_hook_event_with_target,
};
