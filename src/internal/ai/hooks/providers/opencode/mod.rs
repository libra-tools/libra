//! OpenCode lifecycle hook provider facade (AG-19).
//!
//! Wires the canonical [`HookProvider`] trait to the OpenCode-specific parser
//! (`parser`) and the `.opencode/plugin/libra-hooks.js` installer
//! (`settings`). All OpenCode-specific decoding logic lives in those
//! submodules; this module contains only the singleton plumbing.
//!
//! # Verified upstream contract (opencode 1.17.13, probed live 2026-07-05)
//!
//! - **Plugin file**: a JS module in `<project>/.opencode/plugin/*.js`
//!   (singular directory). The plural `.opencode/plugins/` directory and
//!   `opencode.json` `"plugin"` array entries also load, but Libra writes only
//!   `.opencode/plugin/libra-hooks.js` as its managed file; uninstall/status
//!   additionally detect a stray Libra-managed copy under
//!   `.opencode/plugins/` to warn about / clean duplicates.
//! - **Plugin API**: `export const LibraHooks = async ({ project, client,
//!   directory, worktree, serverUrl, $ }) => ({ event: async ({ event }) =>
//!   { ... }, "tool.execute.after": async (input, output) => { ... } })`,
//!   where `$` is a BunShell.
//! - **Bus events observed live**: `session.created` (properties.sessionID +
//!   info.directory…), `session.updated`, `message.updated`
//!   (properties.info.role user/assistant), `message.part.updated`/delta
//!   (streaming — never forwarded), `session.status`, `session.idle` (fires at
//!   the end of each headless run — the reliable turn-complete marker),
//!   `session.diff`. Declared in the SDK but not observed headless:
//!   `session.deleted`, `session.error`, `session.compacted`.
//! - **Load errors** are per-plugin and non-fatal, visible only with
//!   `opencode --print-logs`.
//! - **`--pure` caveat**: `opencode --pure` / `OPENCODE_PURE=1` disables all
//!   external plugins, including the Libra forwarder — no lifecycle events are
//!   captured in that mode.

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
        parser::OPENCODE_HOOK_EVENT_NAMES.contains(&hook_event_name)
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
        for name in ["message.part.updated", "session.status", "session.diff"] {
            assert!(
                !provider.recognizes_event(name),
                "must not recognize '{name}'",
            );
        }
    }
}
