//! Claude Code lifecycle hook provider facade.
//!
//! Wires the canonical [`HookProvider`] trait to the Claude-specific parser
//! (`parser`) and Claude settings.json installer (`settings`). All Claude-specific
//! decoding logic lives in those submodules; this module contains only the
//! singleton plumbing.

mod parser;
mod settings;

use anyhow::Result;

use super::super::{
    lifecycle::{LifecycleEvent, SessionHookEnvelope},
    provider::{
        CANONICAL_DEDUP_IDENTITY_KEYS, HookProvider, ProviderHookCommand, ProviderInstallOptions,
    },
};

/// Singleton instance referenced by [`super::claude_provider`].
pub static CLAUDE_PROVIDER: ClaudeProvider = ClaudeProvider;

/// Hook commands the Claude provider can install and parse. Order matters only
/// for documentation/listing; lookup is by value.
const SUPPORTED_COMMANDS: &[ProviderHookCommand] = &[
    ProviderHookCommand::SessionStart,
    ProviderHookCommand::Prompt,
    ProviderHookCommand::ToolUse,
    ProviderHookCommand::ModelUpdate,
    ProviderHookCommand::Compaction,
    ProviderHookCommand::Stop,
    ProviderHookCommand::SessionEnd,
];

/// Zero-sized provider type. All state lives in the submodules.
#[derive(Debug, Clone, Copy)]
pub struct ClaudeProvider;

impl HookProvider for ClaudeProvider {
    fn provider_name(&self) -> &'static str {
        "claude"
    }

    fn source_name(&self) -> &'static str {
        "claude_code_hook"
    }

    fn supported_commands(&self) -> &'static [ProviderHookCommand] {
        SUPPORTED_COMMANDS
    }

    fn parse_hook_event(
        &self,
        hook_event_name: &str,
        envelope: &SessionHookEnvelope,
    ) -> Result<LifecycleEvent> {
        parser::parse_claude_hook_event(hook_event_name, envelope)
    }

    fn recognizes_event(&self, hook_event_name: &str) -> bool {
        parser::CLAUDE_HOOK_EVENT_NAMES.contains(&hook_event_name)
    }

    fn dedup_identity_keys(&self) -> &'static [&'static str] {
        CANONICAL_DEDUP_IDENTITY_KEYS
    }

    fn install_hooks(&self, options: &ProviderInstallOptions) -> Result<()> {
        settings::install_claude_hooks(options)
    }

    fn uninstall_hooks(&self) -> Result<()> {
        settings::uninstall_claude_hooks()
    }

    fn hooks_are_installed(&self) -> Result<bool> {
        settings::claude_hooks_are_installed()
    }
}
