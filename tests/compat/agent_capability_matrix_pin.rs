//! Schema pin for the observed-agent capability contract (AG-16 / E1 / E9).
//!
//! Guards `docs/development/tracing/agent.md` E1's frozen 8-bool wire keys
//! and the first-batch supported roster against drift. Changing either is a
//! public wire break: update the doc, the registry, and this pin in the same
//! PR (plus `docs/commands/agent.md` once the CLI surfaces the matrix).

use std::collections::BTreeSet;

use libra::internal::ai::observed_agents::{
    AgentKind, DeclaredAgentCaps, FIRST_BATCH_WAVE, SlugLookup, agent_for, discover_skills,
    launchable_investigate_slugs, launchable_review_slugs, lookup_cli_slug, registration_for,
    registry, supported_slugs,
};

/// E1: `DeclaredAgentCaps` serializes to exactly the 8 frozen snake_case
/// keys — no more, no fewer. (The second half of the E1 pin — external
/// `methods[]` unlock for the three non-declared capabilities and the
/// `PromptExtractor` gate reusing `transcript_analyzer` — becomes testable
/// when the AG-18 external shim lands and must be added here then.)
#[test]
fn declared_agent_caps_wire_keys_are_exactly_e1() {
    let value = serde_json::to_value(DeclaredAgentCaps::default()).unwrap();
    let object = value.as_object().expect("caps serialize to an object");
    let keys: BTreeSet<&str> = object.keys().map(String::as_str).collect();
    let expected: BTreeSet<&str> = [
        "hooks",
        "transcript_analyzer",
        "transcript_preparer",
        "token_calculator",
        "compact_transcript",
        "text_generator",
        "hook_response_writer",
        "subagent_aware_extractor",
    ]
    .into_iter()
    .collect();
    assert_eq!(
        keys, expected,
        "DeclaredAgentCaps wire keys drifted from E1 (docs/development/tracing/agent.md)"
    );
    assert_eq!(object.len(), 8, "E1 pins exactly 8 keys");
}

/// E9 / 第一批支持项目: the static matrix covers every `AgentKind`, the
/// supported roster is exactly `claude-code`/`codex`/`opencode` at wave
/// `first_batch`, and no unsupported row advertises installability or
/// launchability.
#[test]
fn known_agent_capability_matrix_matches_current_roster() {
    // One row per kind, slug/db_value in sync with the enum mappings.
    assert_eq!(registry().len(), AgentKind::all().len());
    for kind in AgentKind::all() {
        let row = registration_for(*kind);
        assert_eq!(row.slug, kind.as_cli_slug());
        assert_eq!(row.db_value, kind.as_db_str());
        assert_eq!(row.agent_kind, kind.as_db_str());
        assert!(
            row.registered,
            "{}: every static row is registered",
            row.slug
        );
        assert!(
            !row.external_binary,
            "{}: static rows are built-in adapters only",
            row.slug
        );
        assert!(
            !row.installed,
            "{}: the static matrix never claims a runtime install state",
            row.slug
        );
    }

    // Frozen first-batch roster.
    assert_eq!(supported_slugs(), ["claude-code", "codex", "opencode"]);
    for row in registry() {
        if row.supported {
            assert_eq!(row.support_wave, Some(FIRST_BATCH_WAVE), "{}", row.slug);
            assert!(row.transcript_readable, "{}", row.slug);
        } else {
            assert_eq!(row.support_wave, None, "{}", row.slug);
        }
    }

    // AG-19: the whole first batch — Claude Code, Codex, OpenCode — is
    // hook-installable, each with its verified upstream config target
    // (Claude `.claude/settings.json`; Codex user-level hooks.json with
    // the project-visible `.codex/hooks.json` load path pinned here;
    // OpenCode Libra-managed plugin file).
    let claude = registration_for(AgentKind::ClaudeCode);
    assert!(claude.hook_installable);
    assert!(claude.capabilities.hooks);
    assert_eq!(claude.config_paths, [".claude/settings.json"]);
    let codex = registration_for(AgentKind::Codex);
    assert!(codex.hook_installable, "codex: AG-19 HookProvider landed");
    assert!(codex.capabilities.hooks);
    assert_eq!(codex.config_paths, [".codex/hooks.json"]);
    let opencode = registration_for(AgentKind::OpenCode);
    assert!(
        opencode.hook_installable,
        "opencode: AG-19 HookProvider landed"
    );
    assert!(opencode.capabilities.hooks);
    assert_eq!(opencode.config_paths, [".opencode/plugin/libra-hooks.js"]);

    // AG-22 / AG-23: the first batch is BOTH review- and
    // investigate-launchable — `libra review` / `libra investigate` gate
    // on these exact (independent) flags, so the matrix rows and the
    // launchers can never disagree.
    assert_eq!(
        launchable_review_slugs(),
        ["claude-code", "codex", "opencode"],
        "the review-launchable roster is exactly the first-batch trio"
    );
    assert_eq!(
        launchable_investigate_slugs(),
        ["claude-code", "codex", "opencode"],
        "the investigate-launchable roster is exactly the first-batch trio"
    );
    for kind in [AgentKind::ClaudeCode, AgentKind::Codex, AgentKind::OpenCode] {
        let row = registration_for(kind);
        assert!(
            row.launchable_review,
            "{}: first-batch rows are review-launchable since AG-22",
            row.slug
        );
        assert!(
            row.launchable_investigate,
            "{}: first-batch rows are investigate-launchable since AG-23",
            row.slug
        );
    }
    for row in registry() {
        if row.launchable_review {
            assert!(
                row.supported,
                "{}: launchable_review implies supported, never the reverse",
                row.slug
            );
        }
        if row.launchable_investigate {
            assert!(
                row.supported,
                "{}: launchable_investigate implies supported, never the reverse",
                row.slug
            );
        }
    }

    // Non-first-batch agents must never be exposed as installable or
    // launchable (E9 quarantine/unsupported rule).
    for kind in [
        AgentKind::Gemini,
        AgentKind::Cursor,
        AgentKind::Copilot,
        AgentKind::FactoryAi,
    ] {
        let row = registration_for(kind);
        assert!(!row.supported, "{}", row.slug);
        assert!(!row.hook_installable, "{}", row.slug);
        assert!(!row.launchable_review, "{}", row.slug);
        assert!(!row.launchable_investigate, "{}", row.slug);
        assert!(!row.capabilities.hooks, "{}", row.slug);
    }

    // The static rows and the adapters' `as_*` introspection must agree —
    // a row must not advertise a capability its adapter cannot produce
    // (and vice versa), and installability is exactly
    // `supported && as_hooks().is_some()`. (The gemini HookProvider exists
    // for the AG-17 uninstall-only channel but is deliberately NOT wired
    // through `as_hooks()` — E9 forbids advertising it as a capability.)
    for kind in AgentKind::all() {
        let row = registration_for(*kind);
        let agent = agent_for(*kind);
        assert_eq!(
            row.capabilities,
            agent.declared_capabilities(),
            "{}: registry row and adapter introspection drifted",
            row.slug
        );
        assert_eq!(
            row.hook_installable,
            row.supported && agent.as_hooks().is_some(),
            "{}: hook_installable must equal supported && as_hooks().is_some()",
            row.slug
        );
    }
}

/// A0-07: the curated per-agent skill-discovery registry
/// (`discover_skills`, the public SkillDiscoverer surface) matches the frozen
/// first-batch roster — Claude Code exposes the three E7 skills, Codex and
/// OpenCode the single `/review`, and every non-first-batch agent exposes
/// none. Skill discovery is an OPTIONAL capability (unlocked via the v1
/// `methods[]` `skill_events` negotiation), deliberately NOT a 9th
/// `DeclaredAgentCaps` bool, so the frozen 8-key wire contract above is
/// untouched.
#[test]
fn skill_discovery_registry_matches_roster() {
    let claude = discover_skills(AgentKind::ClaudeCode);
    assert_eq!(
        claude.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["/review", "/security-review", "/simplify"],
        "claude-code curated skills drifted from E7"
    );
    assert!(
        claude.iter().all(|s| s.provider == "claude-code"),
        "discovered skills carry the agent's CLI slug as provider"
    );
    assert_eq!(
        discover_skills(AgentKind::Codex)
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["/review"]
    );
    assert_eq!(
        discover_skills(AgentKind::OpenCode)
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        ["/review"]
    );
    // Non-first-batch agents expose no discoverable skills.
    for kind in [
        AgentKind::Gemini,
        AgentKind::Cursor,
        AgentKind::Copilot,
        AgentKind::FactoryAi,
    ] {
        assert!(
            discover_skills(kind).is_empty(),
            "{}: unsupported agents expose no discoverable skills",
            kind.as_cli_slug()
        );
    }
    // Every supported agent's discovery set is non-empty and provider-tagged.
    for kind in AgentKind::all() {
        for skill in discover_skills(*kind) {
            assert_eq!(skill.provider, kind.as_cli_slug());
            assert!(
                skill.name.starts_with('/'),
                "skill names are slash commands"
            );
        }
    }
}

/// E9: slugs outside the known `AgentKind` set are quarantined fail-closed —
/// they never resolve to a registration row. Known-but-unsupported slugs
/// resolve to their row with `supported=false` (needed for the AG-17
/// gemini uninstall-only channel).
#[test]
fn unsupported_external_agent_kind_is_quarantined() {
    for slug in ["pi", "vogon", "copilot-cli", "factoryai-droid", "", "wat"] {
        assert_eq!(
            lookup_cli_slug(slug),
            SlugLookup::UnknownQuarantined,
            "slug {slug:?} must be quarantined"
        );
    }
    match lookup_cli_slug("gemini") {
        SlugLookup::Known(row) => {
            assert!(!row.supported);
            assert!(!row.hook_installable);
        }
        SlugLookup::UnknownQuarantined => {
            panic!("gemini stays registered (read-only/uninstall channel)")
        }
    }
}
