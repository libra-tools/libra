//! Claude Code lifecycle hook provider facade.
//!
//! Wires the canonical [`HookProvider`] trait to the Claude-specific parser
//! (`parser`) and Claude settings.json installer (`settings`). All Claude-specific
//! decoding logic lives in those submodules; this module contains only the
//! singleton plumbing.

mod events;
mod parser;
mod settings;

use anyhow::Result;
pub use events::{
    ClaudeEventClass, ClaudeFieldReport, MESSAGE_DISPLAY_COALESCE_LIMIT, admit_message_display,
    classify_claude_fields, control_sensitive_names, deprecated_alias_names, event_class,
    known_event_names, reset_message_display_counts_for_test, safely_installable_names,
};
pub use settings::ClaudeHookDoctorEvent;

/// Event-level Claude hook report for `libra agent doctor`.
pub fn claude_hook_doctor_events() -> Result<Vec<ClaudeHookDoctorEvent>> {
    settings::claude_hook_doctor_events()
}

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
        events::on_current_ingress(hook_event_name)
    }

    fn admit_ingress_frame(&self, hook_event_name: &str, session_id: &str) -> bool {
        if hook_event_name == "MessageDisplay" {
            events::admit_message_display(session_id)
        } else {
            true
        }
    }

    fn display_batch_limit(&self, hook_event_name: &str) -> Option<u32> {
        (hook_event_name == "MessageDisplay").then_some(events::MESSAGE_DISPLAY_COALESCE_LIMIT)
    }

    fn dedup_identity_keys(&self) -> &'static [&'static str] {
        CANONICAL_DEDUP_IDENTITY_KEYS
    }

    fn supports_host_timeout(&self) -> bool {
        true
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
