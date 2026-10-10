//! OpenCode hook provider and audited 2.0.26 event registry.
//!
//! Parsing and recognition consume the same OpenCode-only registry. The
//! managed plugin uses the current subscription API and Node/Bun transport,
//! retaining bounded legacy input compatibility. The
//! six existing lifecycle verbs and install/status interfaces are unchanged.

pub mod events;

mod parser;
mod settings;

use anyhow::Result;

use super::super::{
    lifecycle::{LifecycleEvent, SessionHookEnvelope},
    provider::{
        CANONICAL_DEDUP_IDENTITY_KEYS, HookProvider, ProviderHookCommand, ProviderInstallOptions,
    },
};

/// Singleton instance intended to back an `opencode_provider()` typed
/// accessor in [`super`] (mirroring `CLAUDE_PROVIDER` / `GEMINI_PROVIDER`).
pub static OPENCODE_PROVIDER: OpenCodeProvider = OpenCodeProvider;

/// Hook commands the OpenCode provider can install and parse. Order matters
/// only for documentation/listing; lookup is by value. `ModelUpdate` is
/// intentionally absent: OpenCode exposes no model-change plugin event.
const SUPPORTED_COMMANDS: &[ProviderHookCommand] = &[
    ProviderHookCommand::SessionStart,
    ProviderHookCommand::Prompt,
    ProviderHookCommand::ToolUse,
    ProviderHookCommand::Stop,
    ProviderHookCommand::SessionEnd,
    ProviderHookCommand::Compaction,
];

/// Zero-sized provider type. All state lives in the submodules.
#[derive(Debug, Clone, Copy)]
pub struct OpenCodeProvider;

impl HookProvider for OpenCodeProvider {
    fn provider_name(&self) -> &'static str {
        "opencode"
    }

    fn source_name(&self) -> &'static str {
        "opencode_hook"
    }

    fn supported_commands(&self) -> &'static [ProviderHookCommand] {
        SUPPORTED_COMMANDS
    }

    fn parse_hook_event(
        &self,
        hook_event_name: &str,
        envelope: &SessionHookEnvelope,
    ) -> Result<LifecycleEvent> {
        parser::parse_opencode_hook_event(hook_event_name, envelope)
    }

    fn recognizes_event(&self, hook_event_name: &str) -> bool {
        events::recognizes_event(hook_event_name)
    }

    fn dedup_identity_keys(&self) -> &'static [&'static str] {
        CANONICAL_DEDUP_IDENTITY_KEYS
    }

    fn install_hooks(&self, options: &ProviderInstallOptions) -> Result<()> {
        settings::install_opencode_hooks(options)
    }

    fn uninstall_hooks(&self) -> Result<()> {
        settings::uninstall_opencode_hooks()
    }

    fn hooks_are_installed(&self) -> Result<bool> {
        settings::opencode_hooks_are_installed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Scenario: the singleton exposes the canonical AG-19 provider surface.
    #[test]
    fn opencode_provider_exposes_canonical_surface() {
        let provider: &dyn HookProvider = &OPENCODE_PROVIDER;
        assert_eq!(provider.provider_name(), "opencode");
        assert_eq!(provider.source_name(), "opencode_hook");
        assert_eq!(
            provider.dedup_identity_keys(),
            CANONICAL_DEDUP_IDENTITY_KEYS
        );
        assert_eq!(provider.supported_commands(), SUPPORTED_COMMANDS);
        assert_eq!(provider.supported_commands().len(), 6);

        // recognizes_event follows the parser's name table: mapped events are
        // recognized, streaming/unmapped events are skip-and-logged upstream.
        for name in [
            "session.created",
            "message.updated",
            "tool.execute.after",
            "session.idle",
            "session.deleted",
            "session.compacted",
        ] {
            assert!(provider.recognizes_event(name), "must recognize '{name}'");
        }
        for name in [
            "message.part.updated",
            "session.inbox.enqueued",
            "session.diff",
        ] {
            assert!(
                !provider.recognizes_event(name),
                "must not recognize '{name}'",
            );
        }
    }
}
