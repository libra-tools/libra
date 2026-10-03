//! Codex CLI lifecycle hook provider facade (AG-19).
//!
//! Wires the canonical [`HookProvider`] trait to the Codex-specific parser
//! (`parser`) and the `$CODEX_HOME/hooks.json` + `config.toml` trust-state
//! installer (`settings`). All Codex-specific decoding logic lives in those
//! submodules; this module contains only the singleton plumbing and the
//! trust-gap accessor used by the dispatcher's SessionStart banner.
//!
//! # Verified upstream contract (codex-cli 0.142.4)
//!
//! Probed live 2026-07-05 and cross-checked byte-exact against source
//! `rust-v0.142.4` @57d253ad — treat as ground truth:
//!
//! - **Hooks are default-enabled** in 0.142.4. Each config layer accepts two
//!   forms: a `hooks.json` next to that layer's config (user:
//!   `$CODEX_HOME/hooks.json`, project: `<repo>/.codex/hooks.json`) with
//!   shape `{"hooks": {"<EventName>": [{"matcher": "<optional regex>",
//!   "hooks": [{"type": "command", "command": "<cmd run via $SHELL -lc>",
//!   "timeout": <secs, default 600>, "statusMessage": "…"}]}]}}` — the
//!   top-level key MUST be `"hooks"` (`deny_unknown_fields`) — or
//!   `[[hooks.<EventName>]]` tables in that layer's `config.toml` (using
//!   both in one layer warns).
//! - **Event names** (PascalCase in config): `PreToolUse`,
//!   `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`,
//!   `SessionStart`, `UserPromptSubmit`, `SubagentStart`, `SubagentStop`,
//!   `Stop`, `SessionEnd`.
//! - **Hook stdin payload is Claude Code-compatible** single-line JSON:
//!   `{"session_id", "transcript_path", "cwd", "hook_event_name", "model",
//!   "permission_mode", …}`; `UserPromptSubmit` adds `prompt` + `turn_id`;
//!   `PreToolUse`/`PostToolUse` add `tool_name`, `tool_input`
//!   (`tool_response` on Post), `tool_use_id`; `Stop` adds
//!   `stop_hook_active`, `last_assistant_message`; `SessionStart` adds
//!   `source` (startup|resume|clear|compact).
//! - **Trust double gate** (user config only): a hook runs iff its
//!   `[hooks.state."<abs source path>:<event_snake>:<matcher_group_index>:
//!   <handler_index>"]` entry in `$CODEX_HOME/config.toml` has
//!   `enabled != false` **and** `trusted_hash` equal to `"sha256:" +
//!   sha256hex(<compact JSON with recursively sorted keys of the hook's
//!   canonical identity>)`. Untrusted hooks are skipped **silently** by
//!   `codex exec` — hence [`codex_hook_trust_gaps`] for the dispatcher's
//!   SessionStart banner.
//! - **Positional-key hazard**: the state keys embed group/handler indices
//!   (upstream TODO: durable ids), so reordering `hooks.json` entries
//!   invalidates trust. The installer recomputes indices from the final
//!   file on every (re)install and removes its own stale keys; see
//!   `settings` for the full strategy.
//! - **Project layer is trust-gated**: `<repo>/.codex/hooks.json` only loads
//!   when the user config carries `[projects."<abs path>"] trust_level =
//!   "trusted"`, which Libra cannot arrange non-interactively for arbitrary
//!   repos — the installer therefore targets the user level
//!   (`$CODEX_HOME`), the proven fully non-interactive path.
//! - **Operational note**: `codex exec` hangs reading non-tty stdin —
//!   irrelevant to hook forwarding (Codex feeds each hook a JSON line on its
//!   stdin), but relevant when driving Codex itself from scripts.

mod parser;
mod settings;

use anyhow::Result;

use super::super::{
    lifecycle::{LifecycleEvent, SessionHookEnvelope},
    provider::{
        CANONICAL_DEDUP_IDENTITY_KEYS, HookProvider, ProviderHookCommand, ProviderInstallOptions,
    },
};

/// Singleton instance intended to back a `codex_provider()` typed accessor
/// in [`super`] (mirroring `CLAUDE_PROVIDER` / `GEMINI_PROVIDER`).
pub static CODEX_PROVIDER: CodexProvider = CodexProvider;

/// Hook commands the Codex provider can install and parse. Events that share a
/// lifecycle kind intentionally share a command: both `PreToolUse` and
/// `PostToolUse` use `tool-use`, while `PreCompact` and `PostCompact` use
/// `compaction`; the original provider event name remains in the envelope.
const SUPPORTED_COMMANDS: &[ProviderHookCommand] = &[
    ProviderHookCommand::SessionStart,
    ProviderHookCommand::Prompt,
    ProviderHookCommand::ToolUse,
    ProviderHookCommand::PermissionRequest,
    ProviderHookCommand::Compaction,
    ProviderHookCommand::Stop,
    ProviderHookCommand::SessionEnd,
    ProviderHookCommand::SubagentStart,
    ProviderHookCommand::SubagentEnd,
];

/// Zero-sized provider type. All state lives in the submodules.
#[derive(Debug, Clone, Copy)]
pub struct CodexProvider;

/// Count Libra-managed Codex handlers lacking a matching + current
/// `trusted_hash` state entry (AG-19 trust-gap banner support). Codex skips
/// untrusted hooks silently, so the dispatcher surfaces a SessionStart
/// banner whenever this is non-zero.
pub fn codex_hook_trust_gaps() -> Result<usize> {
    settings::codex_hook_trust_gaps()
}

impl HookProvider for CodexProvider {
    fn provider_name(&self) -> &'static str {
        "codex"
    }

    fn source_name(&self) -> &'static str {
        "codex_hook"
    }

    fn supported_commands(&self) -> &'static [ProviderHookCommand] {
        SUPPORTED_COMMANDS
    }

    fn parse_hook_event(
        &self,
        hook_event_name: &str,
        envelope: &SessionHookEnvelope,
    ) -> Result<LifecycleEvent> {
        parser::parse_codex_hook_event(hook_event_name, envelope)
    }

    fn recognizes_event(&self, hook_event_name: &str) -> bool {
        parser::CODEX_HOOK_EVENT_NAMES.contains(&hook_event_name)
    }

    fn dedup_identity_keys(&self) -> &'static [&'static str] {
        CANONICAL_DEDUP_IDENTITY_KEYS
    }

    fn install_hooks(&self, options: &ProviderInstallOptions) -> Result<()> {
        settings::install_codex_hooks(options)
    }

    fn uninstall_hooks(&self) -> Result<()> {
        settings::uninstall_codex_hooks()
    }

    fn hooks_are_installed(&self) -> Result<bool> {
        settings::codex_hooks_are_installed()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Scenario: the singleton exposes the canonical AG-19 provider surface.
    #[test]
    fn codex_provider_exposes_canonical_surface() {
        let provider: &dyn HookProvider = &CODEX_PROVIDER;
        assert_eq!(provider.provider_name(), "codex");
        assert_eq!(provider.source_name(), "codex_hook");
        assert_eq!(
            provider.dedup_identity_keys(),
            CANONICAL_DEDUP_IDENTITY_KEYS
        );
        assert_eq!(provider.supported_commands(), SUPPORTED_COMMANDS);
        assert_eq!(provider.supported_commands().len(), 9);

        // recognizes_event follows the parser's 11-event name table; names
        // Codex never emits are skip-and-logged upstream.
        for name in [
            "SessionStart",
            "UserPromptSubmit",
            "PreToolUse",
            "PostToolUse",
            "PreCompact",
            "PostCompact",
            "SubagentStart",
            "SubagentStop",
            "PermissionRequest",
            "Stop",
            "SessionEnd",
        ] {
            assert!(provider.recognizes_event(name), "must recognize '{name}'");
        }
        for name in ["ModelUpdate", "session.created", "stop"] {
            assert!(
                !provider.recognizes_event(name),
                "must not recognize '{name}'",
            );
        }
    }
}
