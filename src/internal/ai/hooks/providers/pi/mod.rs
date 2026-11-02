//! The observe-only Pi hook (PI-04).
//!
//! A Libra-owned project extension (`.pi/extensions/libra-hooks.ts`)
//! forwards the ADR-PI-06 envelope to `libra agent hooks pi observe`. The
//! receiver lowers that camelCase frame to the shared envelope before the
//! generic checks and records it through the existing agent-traces ingest.
//! Installing the asset through `libra agent enable`, the subprocess
//! resource contract, event correlation and the run terminal belong to
//! PI-14, PI-13, PI-12 and PI-19. Pi stays unsupported and not
//! hook-installable.

pub mod parser;
pub mod settings;

use anyhow::{Result, bail};

use super::super::{
    lifecycle::{LifecycleEvent, LifecycleEventKind, SessionHookEnvelope},
    provider::{HookProvider, ProviderHookCommand, ProviderInstallOptions},
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

    /// Stable native keys arrive with PI-12. Until then every delivery is
    /// its own action.
    fn dedup_identity_keys(&self) -> &'static [&'static str] {
        &[]
    }

    fn dedup_identity_keys_for_event(
        &self,
        _hook_event_name: &str,
        _event_kind: LifecycleEventKind,
    ) -> &'static [&'static str] {
        &[]
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
