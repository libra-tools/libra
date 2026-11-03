//! The observe-only Pi hook (PI-04).
//!
//! A Libra-owned project extension (`.pi/extensions/libra-hooks.ts`)
//! forwards the ADR-PI-06 envelope to `libra agent hooks pi observe`. The
//! receiver lowers that camelCase frame to the shared envelope before the
//! generic checks and records it through the existing agent-traces ingest.
//! The extension bounds its child processes (PI-13) and the receiver keys
//! every recorded event by a stable native field (PI-12, see [`parser`]).
//! Installing the asset through `libra agent enable` and the run terminal
//! belong to PI-14 and PI-19. Pi stays unsupported and not hook-installable.

pub mod parser;
pub mod settings;

use anyhow::{Result, bail};

use super::super::{
    lifecycle::{LifecycleEvent, LifecycleEventKind, SessionHookEnvelope},
    provider::{HookProvider, ProviderHookCommand, ProviderInstallOptions, ToolLinkKeys},
};

pub static PI_PROVIDER: PiProvider = PiProvider;

const SUPPORTED_COMMANDS: &[ProviderHookCommand] = &[ProviderHookCommand::Observe];

#[derive(Debug, Clone, Copy)]
pub struct PiProvider;

impl HookProvider for PiProvider {
    fn provider_name(&self) -> &'static str {
        "pi"
    }

    fn source_name(&self) -> &'static str {
        "pi_extension"
    }

    fn supported_commands(&self) -> &'static [ProviderHookCommand] {
        SUPPORTED_COMMANDS
    }

    fn lower_native_frame(&self, frame: &str) -> Option<Result<SessionHookEnvelope>> {
        Some(parser::lower_pi_frame(frame))
    }

    fn parse_hook_event(
        &self,
        hook_event_name: &str,
        envelope: &SessionHookEnvelope,
    ) -> Result<LifecycleEvent> {
        parser::parse_pi_hook_event(hook_event_name, envelope)
    }

    fn recognizes_event(&self, hook_event_name: &str) -> bool {
        parser::recognizes_pi_event(hook_event_name)
    }

    fn admit_ingress_frame(&self, hook_event_name: &str, _session_id: &str) -> bool {
        parser::pi_event_is_ingested(hook_event_name)
    }

    /// Every replay identity field the Pi envelope carries. Which one
    /// applies depends on the event, see [`Self::dedup_identity_keys_for_event`].
    fn dedup_identity_keys(&self) -> &'static [&'static str] {
        &["toolCallId", "frameId"]
    }

    /// Keyed by event name rather than lifecycle kind: `tool_call` and
    /// `tool_result` are both `ToolUse` but keep separate phases, and an
    /// event without a key here stays its own action on every delivery.
    fn dedup_identity_keys_for_event(
        &self,
        hook_event_name: &str,
        _event_kind: LifecycleEventKind,
    ) -> &'static [&'static str] {
        parser::pi_replay_identity_keys(hook_event_name)
    }

    /// A tool event's `toolCallId` and `parentToolCallId` (PI-37). A nested
    /// call's parent identity is built from the parent's own `toolCallId`,
    /// so it equals the identity the parent call is recorded with.
    fn tool_link_keys(&self, hook_event_name: &str) -> Option<ToolLinkKeys> {
        matches!(hook_event_name, "tool_call" | "tool_result").then_some(ToolLinkKeys {
            tool: "toolCallId",
            parent: "parentToolCallId",
        })
    }

    fn install_hooks(&self, _options: &ProviderInstallOptions) -> Result<()> {
        bail!("Pi hook installation is not available yet; Pi is not a supported agent")
    }

    fn uninstall_hooks(&self) -> Result<()> {
        bail!("Pi hook installation is not available yet; Pi is not a supported agent")
    }

    fn hooks_are_installed(&self) -> Result<bool> {
        Ok(false)
    }
}

pub fn pi_provider() -> &'static dyn HookProvider {
    &PI_PROVIDER
}
