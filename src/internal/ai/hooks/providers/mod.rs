//! Statically registered lifecycle hook providers.
//!
//! Each provider lives in its own submodule and exposes a singleton
//! `&'static dyn HookProvider` behind a typed accessor. There is
//! deliberately **no name-string lookup** (AG-19 removed the
//! `find_provider` bridge): runtime dispatch goes `AgentKind` ->
//! observed-agents registry -> `as_hooks()`, and the few places that need
//! a provider outside that path (gemini's uninstall-only channel, the
//! hidden `libra agent hooks <provider>` entry) call the typed accessors
//! directly.
//!
//! The builtin providers' typed catalog identities
//! ([`HookProviderIdentity`]) are declared together below (ADR-ACF-10), so
//! the capture runtime never maps a provider name onto an `AgentKind`.

pub mod claude;
pub mod codex;
pub mod gemini;
pub mod grok;
pub mod hermes;
pub mod opencode;
pub mod pi;

pub use hermes::hermes_provider;
pub use pi::pi_provider;

use super::provider::{HookProvider, HookProviderIdentity};
use crate::internal::ai::observed_agents::AgentKind;

impl HookProviderIdentity for claude::ClaudeProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::ClaudeCode
    }
}

impl HookProviderIdentity for codex::CodexProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Codex
    }
}

impl HookProviderIdentity for gemini::GeminiProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Gemini
    }
}

impl HookProviderIdentity for opencode::OpenCodeProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::OpenCode
    }
}

impl HookProviderIdentity for grok::GrokProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Grok
    }
}

impl HookProviderIdentity for hermes::HermesProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Hermes
    }
}

impl HookProviderIdentity for pi::PiProvider {
    fn agent_kind(&self) -> AgentKind {
        AgentKind::Pi
    }
}

/// Singleton accessor for the Claude hook provider.
pub fn claude_provider() -> &'static dyn HookProvider {
    &claude::CLAUDE_PROVIDER
}

/// Singleton accessor for the Codex hook provider (AG-19).
pub fn codex_provider() -> &'static dyn HookProvider {
    &codex::CODEX_PROVIDER
}

/// Singleton accessor for the Gemini hook provider.
pub fn gemini_provider() -> &'static dyn HookProvider {
    &gemini::GEMINI_PROVIDER
}

/// Singleton accessor for the OpenCode hook provider (AG-19).
pub fn opencode_provider() -> &'static dyn HookProvider {
    &opencode::OPENCODE_PROVIDER
}

/// Singleton accessor for the Grok observe-only hook provider.
///
/// `AgentKind::Grok::as_hooks()` stays `None` until GK-05 opens
/// `hook_installable`. Callers that already hold this provider use it directly.
pub fn grok_provider() -> &'static dyn HookProvider {
    &grok::GROK_PROVIDER
}

#[cfg(test)]
mod tests {
    use super::*;

    // Scenario: the typed singletons expose their canonical names.
    #[test]
    fn typed_singletons_expose_canonical_names() {
        assert_eq!(claude_provider().provider_name(), "claude");
        assert_eq!(codex_provider().provider_name(), "codex");
        assert_eq!(gemini_provider().provider_name(), "gemini");
        assert_eq!(opencode_provider().provider_name(), "opencode");
        assert_eq!(grok_provider().provider_name(), "grok");
        assert_eq!(grok_provider().agent_kind(), AgentKind::Grok);
        assert_eq!(pi_provider().provider_name(), "pi");
        assert_eq!(pi_provider().agent_kind(), AgentKind::Pi);
    }
}
