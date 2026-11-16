//! Static capability matrix for observed external agents (AG-16 / E9).
//!
//! This is the single fact source for which agents Libra supports, at what
//! wave, and with which frozen capability rows. The CLI roster derives from
//! [`supported_slugs`] (the old CLI roster constant was deleted in AG-17);
//! hook providers converge in AG-19. The compat pin
//! (`compat_agent_capability_matrix_pin`) guards the matrix against drift.
//!
//! Roster rules (E9 / "第一批支持项目"):
//!
//! - First-batch supported roster is exactly `claude-code` / `codex` /
//!   `opencode`. Nothing else may be exposed as `supported=true`.
//! - Since AG-22 the first-batch trio is also `launchable_review` (the
//!   `libra review` launcher gates on this flag via
//!   [`launchable_review_slugs`], never on `supported` alone). Since AG-23
//!   the same trio is also `launchable_investigate` (the
//!   `libra investigate` launcher gates on [`launchable_investigate_slugs`]),
//!   so `review` and `investigate` launchability are declared and gated
//!   independently even though the first-batch roster is identical.
//! - `gemini`, `cursor`, `copilot`, `factory-ai` stay registered (their
//!   adapters exist and historical data must remain readable) but are
//!   unsupported: never hook-installable, never launchable.
//! - Unknown external slugs are quarantined fail-closed — they never enter
//!   this static registry; external registration requires the AG-18
//!   `info`/trust flow.

use serde::Serialize;

use super::{adapter::AgentKind, capability::DeclaredAgentCaps};

/// Wave tag carried by every supported roster row.
pub const FIRST_BATCH_WAVE: &str = "first_batch";
/// GK-05 support wave. It is not `first_batch`.
pub const GROK_WAVE: &str = "grok";
/// HM-06 support wave. It is not `first_batch`.
pub const HERMES_WAVE: &str = "hermes";
pub const PI_WAVE: &str = "pi";

/// One frozen capability-matrix row (AG-16 field set).
///
/// Field semantics:
///
/// - `registered`: the kind resolves to a static adapter via `agent_for`.
/// - `supported`: the agent is in the current supported roster (E9).
/// - `installed` in this *static* matrix is always `false`; the CLI layer
///   (AG-17) overlays the runtime installation state when rendering
///   `list --json`.
/// - `external_binary`: whether the row describes an external
///   `libra-agent-*` binary. Static rows are always built-in (`false`) —
///   external agents go through AG-18 discovery/trust and never enter the
///   static matrix.
/// - `config_paths`: provider config entry points Libra manages during hook
///   install/uninstall. Empty when the upstream config surface has not been
///   verified yet (codex/opencode — fixed in AG-19 per upstream testing).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct AgentRegistration {
    pub slug: &'static str,
    pub agent_kind: &'static str,
    pub db_value: &'static str,
    pub supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub support_wave: Option<&'static str>,
    pub registered: bool,
    pub transcript_readable: bool,
    pub hook_installable: bool,
    pub installed: bool,
    pub launchable_review: bool,
    pub launchable_investigate: bool,
    pub external_binary: bool,
    pub config_paths: &'static [&'static str],
    pub capabilities: DeclaredAgentCaps,
}

impl AgentRegistration {
    /// The [`AgentKind`] this row describes.
    ///
    /// Stored as the CLI slug string for serialization; this accessor
    /// resolves it back. The registry construction guarantees the lookup
    /// succeeds for every row.
    pub fn kind(&self) -> Option<AgentKind> {
        AgentKind::from_cli_slug(self.slug)
    }
}

/// Hook-only capability set; no transcript intelligence is advertised.
const HOOKS_ONLY: DeclaredAgentCaps = DeclaredAgentCaps {
    hooks: true,
    transcript_analyzer: false,
    transcript_preparer: false,
    token_calculator: false,
    compact_transcript: false,
    text_generator: false,
    hook_response_writer: false,
    subagent_aware_extractor: false,
};

const NO_CAPS: DeclaredAgentCaps = DeclaredAgentCaps {
    hooks: false,
    transcript_analyzer: false,
    transcript_preparer: false,
    token_calculator: false,
    compact_transcript: false,
    text_generator: false,
    hook_response_writer: false,
    subagent_aware_extractor: false,
};

/// Claude Code capability set (AG-19 hooks, AG-21 transcript intelligence,
/// and M2 transcript flush preparation: analyzer, preparer, token calculator,
/// subagent-aware extractor; prompt extraction rides the analyzer gate,
/// model/skill extraction are deliberately outside the 8-bool set).
const CLAUDE_CODE_CAPS: DeclaredAgentCaps = DeclaredAgentCaps {
    hooks: true,
    transcript_analyzer: true,
    transcript_preparer: true,
    token_calculator: true,
    subagent_aware_extractor: true,
    ..NO_CAPS
};

/// Shared hook/token capability base (AG-19 hooks + AG-21 best-effort
/// token calculation over their rollout/export formats).
const HOOKS_AND_TOKENS: DeclaredAgentCaps = DeclaredAgentCaps {
    hooks: true,
    token_calculator: true,
    ..NO_CAPS
};

/// Codex's source-pinned parser also supplies the modified-file analyzer.
const CODEX_ROLLOUT_CAPS: DeclaredAgentCaps = DeclaredAgentCaps {
    transcript_analyzer: true,
    ..HOOKS_AND_TOKENS
};

/// The frozen static matrix, one row per [`AgentKind`] variant, in
/// registration order. Guarded by `compat_agent_capability_matrix_pin`.
static REGISTRY: [AgentRegistration; 10] = [
    AgentRegistration {
        slug: "claude-code",
        agent_kind: "claude_code",
        db_value: "claude_code",
        supported: true,
        support_wave: Some(FIRST_BATCH_WAVE),
        registered: true,
        transcript_readable: true,
        hook_installable: true,
        installed: false,
        // AG-22/AG-23: review- AND investigate-launchable (read-only
        // spawn shape per plan.md §0.3.2, gated independently).
        launchable_review: true,
        launchable_investigate: true,
        external_binary: false,
        config_paths: &[".claude/settings.json"],
        capabilities: CLAUDE_CODE_CAPS,
    },
    AgentRegistration {
        slug: "cursor",
        agent_kind: "cursor",
        db_value: "cursor",
        supported: false,
        support_wave: None,
        registered: true,
        transcript_readable: true,
        hook_installable: false,
        installed: false,
        launchable_review: false,
        launchable_investigate: false,
        external_binary: false,
        config_paths: &[],
        capabilities: NO_CAPS,
    },
    AgentRegistration {
        slug: "codex",
        agent_kind: "codex",
        db_value: "codex",
        supported: true,
        support_wave: Some(FIRST_BATCH_WAVE),
        registered: true,
        transcript_readable: true,
        // AG-19: Codex HookProvider landed. Install target verified
        // against codex-cli 0.142.4 (2026-07-05): USER-level
        // `$CODEX_HOME/hooks.json` + Libra-managed `[hooks.state]` trust
        // entries in `$CODEX_HOME/config.toml`; project `.codex/hooks.json`
        // only loads for user-trusted projects, so Libra does not write it.
        hook_installable: true,
        installed: false,
        // AG-22/AG-23: review- AND investigate-launchable (read-only
        // spawn shape per plan.md §0.3.2, gated independently).
        launchable_review: true,
        launchable_investigate: true,
        external_binary: false,
        config_paths: &[".codex/hooks.json"],
        capabilities: CODEX_ROLLOUT_CAPS,
    },
    AgentRegistration {
        slug: "gemini",
        agent_kind: "gemini",
        db_value: "gemini",
        // Demoted out of the supported roster (E9). Historical hook installs
        // keep an uninstall-only channel (AG-17) and captured sessions stay
        // readable, but the row must never advertise installability.
        supported: false,
        support_wave: None,
        registered: true,
        transcript_readable: true,
        hook_installable: false,
        installed: false,
        launchable_review: false,
        launchable_investigate: false,
        external_binary: false,
        config_paths: &[".gemini/settings.json"],
        capabilities: NO_CAPS,
    },
    AgentRegistration {
        slug: "opencode",
        agent_kind: "opencode",
        db_value: "opencode",
        supported: true,
        support_wave: Some(FIRST_BATCH_WAVE),
        registered: true,
        transcript_readable: true,
        // AG-19: OpenCode HookProvider landed. Install target verified
        // against opencode 1.17.13 (2026-07-05): Libra-managed plugin file
        // `.opencode/plugin/libra-hooks.js` (project-local).
        hook_installable: true,
        installed: false,
        // AG-22/AG-23: review- AND investigate-launchable (read-only
        // spawn shape per plan.md §0.3.2, gated independently).
        launchable_review: true,
        launchable_investigate: true,
        external_binary: false,
        config_paths: &[".opencode/plugin/libra-hooks.js"],
        capabilities: HOOKS_AND_TOKENS,
    },
    AgentRegistration {
        slug: "copilot",
        agent_kind: "copilot",
        db_value: "copilot",
        supported: false,
        support_wave: None,
        registered: true,
        transcript_readable: true,
        hook_installable: false,
        installed: false,
        launchable_review: false,
        launchable_investigate: false,
        external_binary: false,
        config_paths: &[],
        capabilities: NO_CAPS,
    },
    AgentRegistration {
        slug: "factory-ai",
        agent_kind: "factory_ai",
        db_value: "factory_ai",
        supported: false,
        support_wave: None,
        registered: true,
        transcript_readable: true,
        hook_installable: false,
        installed: false,
        launchable_review: false,
        launchable_investigate: false,
        external_binary: false,
        config_paths: &[],
        capabilities: NO_CAPS,
    },
    AgentRegistration {
        slug: "grok",
        agent_kind: "grok",
        db_value: "grok",
        // GK-05: supported and hook-installable, but not a first-batch
        // launcher. Review and investigate stay closed.
        supported: true,
        support_wave: Some(GROK_WAVE),
        registered: true,
        transcript_readable: true,
        hook_installable: true,
        installed: false,
        launchable_review: false,
        launchable_investigate: false,
        external_binary: false,
        config_paths: &[],
        capabilities: HOOKS_ONLY,
    },
    AgentRegistration {
        slug: "hermes",
        agent_kind: "hermes",
        db_value: "hermes",
        // HM-06: supported after a real isolated session emitted non-empty
        // reasoning. Review and investigate stay closed. Cloud sync stays
        // refused by the capture allowlist.
        supported: true,
        support_wave: Some(HERMES_WAVE),
        registered: true,
        transcript_readable: true,
        hook_installable: true,
        installed: false,
        launchable_review: false,
        launchable_investigate: false,
        external_binary: false,
        config_paths: &["plugins/libra-hermes-capture"],
        capabilities: HOOKS_ONLY,
    },
    AgentRegistration {
        slug: "pi",
        agent_kind: "pi",
        db_value: "pi",
        // PI-06: local capture is supported and hook-installable through
        // the Libra-owned project extension. Pi is not a launcher; the
        // generic transcript reader stays closed (import is explicit).
        supported: true,
        support_wave: Some(PI_WAVE),
        registered: true,
        transcript_readable: false,
        hook_installable: true,
        installed: false,
        launchable_review: false,
        launchable_investigate: false,
        external_binary: false,
        config_paths: &[".pi/extensions/libra-hooks.ts"],
        capabilities: HOOKS_ONLY,
    },
];

/// Borrow the full static matrix in registration order.
pub fn registry() -> &'static [AgentRegistration] {
    &REGISTRY
}

/// Borrow the matrix row for a known [`AgentKind`].
pub fn registration_for(kind: AgentKind) -> &'static AgentRegistration {
    // INVARIANT: REGISTRY holds one row per AgentKind variant in
    // registration order; the pin test asserts full coverage, so the
    // linear scan always finds a row.
    REGISTRY
        .iter()
        .find(|row| row.db_value == kind.as_db_str())
        .expect("static registry covers every AgentKind variant")
}

/// Slugs of the currently supported roster, in registration order.
pub fn supported_slugs() -> Vec<&'static str> {
    REGISTRY
        .iter()
        .filter(|row| row.supported)
        .map(|row| row.slug)
        .collect()
}

/// Slugs launchable as read-only reviewers (`launchable_review`,
/// AG-22), in registration order. This — not [`supported_slugs`] — is
/// the launcher gate: supported ≠ launchable, and using the same matrix
/// flag `agent list --json` renders keeps the CLI roster and the
/// `libra review` launcher from ever disagreeing.
pub fn launchable_review_slugs() -> Vec<&'static str> {
    REGISTRY
        .iter()
        .filter(|row| row.launchable_review)
        .map(|row| row.slug)
        .collect()
}

/// Slugs launchable as read-only investigators (`launchable_investigate`,
/// AG-23), in registration order. This — not [`supported_slugs`] and not
/// [`launchable_review_slugs`] — is the `libra investigate` launcher
/// gate: investigate launchability is declared independently of review
/// launchability, so the CLI roster and the investigate launcher never
/// disagree even if the two rosters diverge later.
pub fn launchable_investigate_slugs() -> Vec<&'static str> {
    REGISTRY
        .iter()
        .filter(|row| row.launchable_investigate)
        .map(|row| row.slug)
        .collect()
}

/// Result of resolving a user-supplied slug against the roster.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SlugLookup {
    /// The slug maps to a registered kind — the row says whether it is
    /// supported/installable.
    Known(&'static AgentRegistration),
    /// The slug is not a known [`AgentKind`]: quarantined fail-closed.
    /// It is never registered, never invoked, and never written into
    /// `agent_session.agent_kind`. External `libra-agent-*` binaries only
    /// become callable through the AG-18 `info`/trust flow.
    UnknownQuarantined,
}

/// Resolve a CLI slug (permissive form, see `AgentKind::from_cli_slug`)
/// against the static roster.
pub fn lookup_cli_slug(slug: &str) -> SlugLookup {
    match AgentKind::from_cli_slug(slug) {
        Some(kind) => SlugLookup::Known(registration_for(kind)),
        None => SlugLookup::UnknownQuarantined,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_has_one_row_per_agent_kind_in_order() {
        assert_eq!(REGISTRY.len(), AgentKind::all().len());
        for (row, kind) in REGISTRY.iter().zip(AgentKind::all()) {
            assert_eq!(row.slug, kind.as_cli_slug(), "row order mismatch");
            assert_eq!(row.db_value, kind.as_db_str());
            assert_eq!(row.agent_kind, kind.as_db_str());
            assert_eq!(row.kind(), Some(*kind));
        }
    }

    #[test]
    fn first_batch_roster_is_exactly_claude_code_codex_opencode() {
        let first_batch: Vec<_> = registry()
            .iter()
            .filter(|row| row.support_wave == Some(FIRST_BATCH_WAVE))
            .map(|row| row.slug)
            .collect();
        assert_eq!(first_batch, ["claude-code", "codex", "opencode"]);
        assert_eq!(
            supported_slugs(),
            ["claude-code", "codex", "opencode", "grok", "hermes", "pi"]
        );
        let grok = registration_for(AgentKind::Grok);
        assert_eq!(grok.support_wave, Some(GROK_WAVE));
        assert_ne!(grok.support_wave, Some(FIRST_BATCH_WAVE));
        let hermes = registration_for(AgentKind::Hermes);
        assert_eq!(hermes.support_wave, Some(HERMES_WAVE));
        let pi = registration_for(AgentKind::Pi);
        assert!(pi.supported);
        assert!(pi.hook_installable);
        assert_eq!(pi.support_wave, Some(PI_WAVE));
        assert!(!pi.launchable_review && !pi.launchable_investigate);
        assert_ne!(hermes.support_wave, Some(FIRST_BATCH_WAVE));
        assert!(!hermes.launchable_review);
        assert!(!hermes.launchable_investigate);
        for row in registry() {
            if row.slug == "grok" || row.slug == "hermes" || row.slug == "pi" {
                continue;
            }
            if row.supported {
                assert_eq!(row.support_wave, Some(FIRST_BATCH_WAVE), "{}", row.slug);
            } else {
                assert_eq!(row.support_wave, None, "{}", row.slug);
            }
        }
    }

    /// AG-22/AG-23: the review- and investigate-launchable rosters are
    /// each exactly the first-batch trio; launchability implies supported.
    #[test]
    fn launchable_review_roster_is_the_first_batch_trio() {
        assert_eq!(
            launchable_review_slugs(),
            ["claude-code", "codex", "opencode"]
        );
        assert_eq!(
            launchable_investigate_slugs(),
            ["claude-code", "codex", "opencode"]
        );
        for row in registry() {
            if row.launchable_review {
                assert!(
                    row.supported,
                    "{}: launchable_review implies supported",
                    row.slug
                );
            }
            if row.launchable_investigate {
                assert!(
                    row.supported,
                    "{}: launchable_investigate implies supported",
                    row.slug
                );
            }
        }
    }

    #[test]
    fn unsupported_rows_are_never_installable_or_launchable() {
        for row in registry().iter().filter(|row| !row.supported) {
            assert!(!row.hook_installable, "{}", row.slug);
            assert!(!row.installed, "{}", row.slug);
            assert!(!row.launchable_review, "{}", row.slug);
            assert!(!row.launchable_investigate, "{}", row.slug);
            assert!(!row.capabilities.hooks, "{}", row.slug);
        }
    }

    #[test]
    fn unknown_slug_is_quarantined_and_gemini_resolves_unsupported() {
        assert_eq!(lookup_cli_slug("vogon"), SlugLookup::UnknownQuarantined);
        assert_eq!(lookup_cli_slug(""), SlugLookup::UnknownQuarantined);
        match lookup_cli_slug("gemini") {
            SlugLookup::Known(row) => {
                assert!(!row.supported);
                assert!(!row.hook_installable);
            }
            SlugLookup::UnknownQuarantined => panic!("gemini must stay registered"),
        }
        match lookup_cli_slug("pi") {
            SlugLookup::Known(row) => {
                assert!(row.supported);
                assert!(!row.transcript_readable);
                assert!(row.hook_installable);
                assert_eq!(row.support_wave, Some(PI_WAVE));
            }
            SlugLookup::UnknownQuarantined => panic!("pi is registered by PI-01"),
        }
    }

    #[test]
    fn static_matrix_rows_are_all_built_in() {
        for row in registry() {
            assert!(row.registered, "{}", row.slug);
            assert!(!row.external_binary, "{}", row.slug);
            // Static matrix never claims a runtime install state.
            assert!(!row.installed, "{}", row.slug);
        }
    }
}
