//! External-Agent capture (CEX-EntireIO).
//!
//! This module owns the runtime that observes lifecycle events from
//! externally-hosted AI agents (Claude Code, Gemini CLI, Cursor, …) and
//! materialises them into Libra's catalog (`agent_session`, `agent_checkpoint`)
//! plus the `refs/libra/traces` orphan ref. See
//! `docs/development/commands/_general.md` (sections 5–8) for the design.
//!
//! Sub-modules:
//!
//! - [`adapter`]: the small core trait [`adapter::ObservedAgent`] every captured
//!   agent must implement (with `as_*` capability accessors), plus the
//!   optional capability traits ([`adapter::TranscriptTruncator`],
//!   [`adapter::TranscriptChunker`]).
//! - [`capability`]: the frozen E1 8-bool wire contract
//!   ([`capability::DeclaredAgentCaps`]) and the optional capability traits
//!   (AG-16; implementations land in AG-19/AG-21).
//! - [`registry`]: the static capability matrix and roster fact source
//!   ([`registry::AgentRegistration`], first-batch supported roster,
//!   unknown-slug quarantine).
//! - [`redaction`]: the [`redaction::Redactor`] engine and the
//!   [`redaction::RedactedBytes`] compile-time contract — only redacted bytes
//!   may flow into checkpoint storage.
//!
//! Phase 1 (this module's first cut) only ships traits, the redaction engine,
//! and the migration that backs the catalog. Phase 2 wires checkpoint
//! generation; Phase 3 wires the cloud-sync hooks.

pub mod adapter;
pub mod builtin;
pub mod capability;
pub mod compliance;
pub mod derived;
pub mod extract;
pub mod preview;
pub mod redaction;
pub mod registry;
pub mod rpc;
pub mod skill_projection;
pub mod trust;

pub use adapter::{
    AgentKind, AgentSessionCtx, AgentStability, ObservedAgent, TranscriptChunker,
    TranscriptTruncator,
};
use builtin::stable_promoted::{
    CODEX_STABLE_PROMOTED_SPEC, COPILOT_STABLE_PROMOTED_SPEC, CURSOR_STABLE_PROMOTED_SPEC,
    FACTORY_AI_STABLE_PROMOTED_SPEC, OPENCODE_STABLE_PROMOTED_SPEC,
};
pub use builtin::{
    ClaudeCodeObservedAgent, GeminiObservedAgent, STABLE_PROMOTED_SPECS, StablePromotedAgent,
    rfc3339_boundary_for_unix_seconds, stable_promoted_spec_for, write_truncated_transcript,
};
pub use capability::{
    CapabilityDeclarer, DeclaredAgentCaps, HookResponseWriter, ModelExtractor, PromptExtractor,
    SkillEvent, SkillEventExtractor, SkillEventSignal, SkillEventSource, SkillEventType, SkillRef,
    SubagentAwareExtractor, TextGenerator, TokenCalculator, TranscriptAnalyzer,
    TranscriptCompactor, TranscriptPreparer,
};
pub use derived::derive_tool_call_records;
pub use preview::{PREVIEW_SPECS, PreviewAgent, PreviewSpec, is_preview, preview_spec_for};
pub use redaction::{
    RedactedBytes, RedactedSink, RedactionMatch, RedactionReport, RedactionRule, Redactor,
};
pub use registry::{
    AgentRegistration, FIRST_BATCH_WAVE, SlugLookup, launchable_investigate_slugs,
    launchable_review_slugs, lookup_cli_slug, registration_for, registry, supported_slugs,
};
pub use rpc::{
    AgentInfo, RPC_BINARY_PREFIX, RPC_DEFAULT_TIMEOUT, RPC_MAX_STDERR_BYTES, RPC_PROTOCOL_VERSION,
    RpcAgent, RpcAgentBinary, RpcError, RpcRequest, RpcResponse, discover_rpc_agents,
};
pub use skill_projection::{
    DiscoveredSkill, IndexedSkillEvent, SKILL_PROJECTION_SCHEMA_VERSION, SkillEventProjection,
    SkillQuery, discover_skills,
};
pub use trust::{
    DEFAULT_TRUSTED_DIRS, ENV_ALLOWLIST_EXTRA_KEY, EXTERNAL_AGENTS_ENABLED_KEY, Provenance,
    TRUSTED_DIRS_KEY, TrustRecord, add_trusted_dir, compute_provenance,
    ensure_dir_not_world_writable, ensure_parent_not_world_writable, env_allowlist_extra,
    env_name_is_forbidden, external_agents_enabled, path_within_trusted_dirs, read_trust,
    read_trusted_dirs, record_trust, revalidate_trust, revoke_trust,
};

/// Borrow the static [`ObservedAgent`] for the supplied [`AgentKind`].
///
/// This is the single dispatch entry point downstream callers (the
/// hook runtime, `libra agent` subcommands, the checkpoint writer)
/// use to find the adapter for a kind without hard-coding the
/// dedicated-vs-promoted split. The two original stable kinds
/// (`ClaudeCode`, `Gemini`) resolve to their hand-written struct;
/// the five Phase 4.4-promoted kinds (`Cursor`, `Codex`, `OpenCode`,
/// `Copilot`, `FactoryAi`) resolve to a `&'static StablePromotedAgent`
/// borrowed from a per-kind static cell so the function can return a
/// `&'static dyn ObservedAgent` for every kind without per-call
/// allocation.
///
/// The function is total over [`AgentKind`]: the exhaustive `match`
/// arms force a future variant to add its own registration in the
/// same patch, which is the same compile-time guard the v0.17.660+
/// `*::all()` enumerators established.
pub fn agent_for(kind: AgentKind) -> &'static dyn ObservedAgent {
    static CURSOR: StablePromotedAgent = StablePromotedAgent(&CURSOR_STABLE_PROMOTED_SPEC);
    static CODEX: StablePromotedAgent = StablePromotedAgent(&CODEX_STABLE_PROMOTED_SPEC);
    static OPENCODE: StablePromotedAgent = StablePromotedAgent(&OPENCODE_STABLE_PROMOTED_SPEC);
    static COPILOT: StablePromotedAgent = StablePromotedAgent(&COPILOT_STABLE_PROMOTED_SPEC);
    static FACTORY_AI: StablePromotedAgent = StablePromotedAgent(&FACTORY_AI_STABLE_PROMOTED_SPEC);
    static CLAUDE_CODE: ClaudeCodeObservedAgent = ClaudeCodeObservedAgent::new();
    static GEMINI: GeminiObservedAgent = GeminiObservedAgent::new();

    match kind {
        AgentKind::ClaudeCode => &CLAUDE_CODE,
        AgentKind::Gemini => &GEMINI,
        AgentKind::Cursor => &CURSOR,
        AgentKind::Codex => &CODEX,
        AgentKind::OpenCode => &OPENCODE,
        AgentKind::Copilot => &COPILOT,
        AgentKind::FactoryAi => &FACTORY_AI,
    }
}

/// Return the static [`TranscriptTruncator`] adapter for the supplied
/// kind, or `None` when the adapter does not implement that optional
/// capability.
///
/// Companion to [`agent_for`] for the
/// `libra agent checkpoint rewind --apply` dispatch path. As of
/// v0.17.677 only [`ClaudeCodeObservedAgent`] implements the
/// truncator trait — the other six kinds return `None` so the caller
/// can branch cleanly without inspecting the source-of-truth match.
///
/// Adding a second truncator capability is a two-step process:
/// 1. Implement `TranscriptTruncator` on the adapter struct.
/// 2. Add a `match` arm here returning `Some(&STATIC_INSTANCE)`.
///
/// The exhaustive match below makes step 2 a compile-time obligation
/// — a new variant added to `AgentKind` without a corresponding arm
/// here fails to build, which prevents the silent
/// "adapter exists but its truncator isn't wired" bug class.
pub fn truncator_for(kind: AgentKind) -> Option<&'static dyn TranscriptTruncator> {
    static CLAUDE_CODE_TRUNCATOR: ClaudeCodeObservedAgent = ClaudeCodeObservedAgent::new();

    match kind {
        AgentKind::ClaudeCode => Some(&CLAUDE_CODE_TRUNCATOR),
        AgentKind::Cursor
        | AgentKind::Codex
        | AgentKind::Gemini
        | AgentKind::OpenCode
        | AgentKind::Copilot
        | AgentKind::FactoryAi => None,
    }
}

#[cfg(test)]
mod registry_tests {
    use super::*;

    /// `agent_for` must return an adapter for every [`AgentKind`], and
    /// the adapter's `provider_kind()` must match the requested kind.
    /// The exhaustive `match` in `agent_for` already forces a future
    /// variant to add a registration; this test pins the
    /// kind-round-trip invariant so a refactor that wires a new
    /// variant to the wrong adapter fails here.
    #[test]
    fn agent_for_returns_matching_kind_for_every_variant() {
        for kind in AgentKind::all() {
            let agent = agent_for(*kind);
            assert_eq!(
                agent.provider_kind(),
                *kind,
                "agent_for({kind:?}) returned wrong kind",
            );
            assert_eq!(
                agent.stability(),
                AgentStability::Stable,
                "agent_for({kind:?}) must report Stable tier — \
                 preview specs are not registered here",
            );
        }
    }

    /// Multiple calls to `agent_for` for the same kind must return the
    /// same `'static` reference so callers can cheaply cache an
    /// adapter handle without indirection.
    #[test]
    fn agent_for_returns_stable_static_references_across_calls() {
        for kind in AgentKind::all() {
            let a = agent_for(*kind);
            let b = agent_for(*kind);
            assert!(
                std::ptr::eq(
                    a as *const dyn ObservedAgent as *const (),
                    b as *const dyn ObservedAgent as *const (),
                ),
                "agent_for({kind:?}) must return the same &'static reference on every call",
            );
        }
    }

    /// `truncator_for` is the optional-capability companion to
    /// `agent_for`. It returns `Some(&dyn TranscriptTruncator)` for
    /// kinds whose adapter implements `TranscriptTruncator`, `None`
    /// otherwise. Today only `ClaudeCode` qualifies; the other six
    /// kinds must return `None`. Pin the per-kind expectation so a
    /// future second truncator implementation lands a passing arm
    /// here (and a refactor that drops the ClaudeCode arm fails the
    /// test rather than silently disabling
    /// `libra agent checkpoint rewind --apply`).
    #[test]
    fn truncator_for_returns_some_only_for_claude_code_today() {
        for kind in AgentKind::all() {
            let truncator = truncator_for(*kind);
            let should_have_truncator = matches!(*kind, AgentKind::ClaudeCode);
            assert_eq!(
                truncator.is_some(),
                should_have_truncator,
                "truncator_for({kind:?}) returned {:?}; expected Some={should_have_truncator}",
                truncator.is_some(),
            );
        }
    }

    /// When `truncator_for` returns `Some`, the returned adapter's
    /// `provider_kind` must match the requested kind — the same
    /// kind-round-trip invariant `agent_for` enforces on the broader
    /// adapter surface.
    #[test]
    fn truncator_for_some_arm_reports_matching_kind() {
        for kind in AgentKind::all() {
            if let Some(truncator) = truncator_for(*kind) {
                assert_eq!(
                    truncator.provider_kind(),
                    *kind,
                    "truncator_for({kind:?}) returned adapter with wrong kind",
                );
            }
        }
    }

    /// Every adapter returned by `agent_for` must declare at least one
    /// protected directory, and the directory name must start with `.`
    /// (Libra's convention for hidden agent storage roots:
    /// `.claude`, `.gemini`, `.cursor`, …). The list is consumed by
    /// `rewind` / `clean` to leave the agent's local storage alone
    /// during destructive worktree operations — an empty list would
    /// mean those operations silently scrub the agent's state.
    ///
    /// Pin both invariants per kind so:
    ///   * a future variant added without populating `protected_dirs`
    ///     fails this test (vs. silently making `rewind` destructive
    ///     for that kind).
    ///   * a future refactor that drops the leading `.` from one of
    ///     the directory names (e.g. `claude` instead of `.claude`)
    ///     fails here (vs. having `rewind` either match nothing
    ///     because Unix tooling treats dotfiles specially, or
    ///     accidentally scrubbing a top-level non-hidden directory
    ///     with the same name).
    #[test]
    fn agent_for_protected_dirs_are_dot_prefixed_and_non_empty() {
        for kind in AgentKind::all() {
            let agent = agent_for(*kind);
            let dirs = agent.protected_dirs();
            assert!(
                !dirs.is_empty(),
                "agent_for({kind:?}) returned an adapter with empty protected_dirs; \
                 rewind / clean would scrub the agent's local storage",
            );
            for dir in dirs {
                assert!(
                    dir.starts_with('.'),
                    "agent_for({kind:?}) protected_dir '{dir}' must start with '.' \
                     (Libra's hidden-agent-storage convention)",
                );
            }
        }
    }
}
