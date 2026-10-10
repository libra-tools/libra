//! OG-00/01 pinned source, fixture, registry and parser conformance.

use std::collections::BTreeSet;

use libra::internal::ai::hooks::{
    SessionHookEnvelope,
    providers::{
        opencode::events::{
            OPENCODE_COMMIT, OPENCODE_DEPRECATED_ALIASES, OPENCODE_FILTERED_STATUSES,
            OPENCODE_HOOK_EVENT_SPECS, OPENCODE_IDLE_STATUS, OPENCODE_LEGACY_EVENT_SPECS,
            OPENCODE_PIN, OPENCODE_TERMINAL_INTERRUPTION_REASONS, OpenCodeForwarding,
        },
        opencode_provider,
    },
};
use serde_json::Value;

const FIXTURE: &str = include_str!("fixtures/agent_transcripts/opencode/hook_events.json");
const MANIFEST: &str = include_str!("fixtures/agent_transcripts/opencode/MANIFEST.md");
const PLAN: &str = include_str!("../docs/development/plan/plan-20260902.md");

fn source_record() -> &'static str {
    let start = "\n<!-- OG-00-PROBE-START -->\n";
    let end = "\n<!-- OG-00-PROBE-END -->\n";
    let starts: Vec<_> = PLAN.match_indices(start).collect();
    let ends: Vec<_> = PLAN.match_indices(end).collect();
    assert_eq!(
        starts.len(),
        1,
        "exactly one standalone source start marker"
    );
    assert_eq!(ends.len(), 1, "exactly one standalone source end marker");
    let begin = starts[0].0 + start.len();
    let finish = ends[0].0;
    assert!(
        finish > begin,
        "source markers must be ordered and non-empty"
    );
    &PLAN[begin..finish]
}

fn declared<'a>(text: &'a str, prefix: &str) -> Vec<&'a str> {
    text.lines()
        .filter_map(|line| line.strip_prefix(prefix))
        .collect()
}

#[test]
fn source_record_matches_fixture_manifest() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("valid synthetic fixture");
    for document in [source_record(), MANIFEST] {
        assert_eq!(declared(document, "opencode_pin: "), [OPENCODE_PIN]);
        assert_eq!(declared(document, "opencode_commit: "), [OPENCODE_COMMIT]);
        for stale in ["1.17.13", "1.18.29"] {
            assert!(!document.contains(stale), "mixed pin {stale}");
        }
    }
    assert_eq!(fixture["opencode_pin"], OPENCODE_PIN);
    assert_eq!(fixture["opencode_commit"], OPENCODE_COMMIT);
    assert_eq!(fixture["payload_member"], "data");
    for (prefix, specs, forwarding) in [
        (
            "standalone_events: ",
            OPENCODE_HOOK_EVENT_SPECS,
            OpenCodeForwarding::Standalone,
        ),
        (
            "plugin_hook_events: ",
            OPENCODE_HOOK_EVENT_SPECS,
            OpenCodeForwarding::PluginHook,
        ),
        (
            "plugin_merged_events: ",
            OPENCODE_HOOK_EVENT_SPECS,
            OpenCodeForwarding::PluginMerged,
        ),
        (
            "compatibility_events: ",
            OPENCODE_LEGACY_EVENT_SPECS,
            OpenCodeForwarding::LegacyCompatibility,
        ),
        (
            "deprecated_aliases: ",
            OPENCODE_DEPRECATED_ALIASES,
            OpenCodeForwarding::DeprecatedAlias,
        ),
    ] {
        let names = specs
            .iter()
            .filter(|spec| spec.forwarding == forwarding)
            .map(|spec| spec.name)
            .collect::<Vec<_>>()
            .join(", ");
        for document in [source_record(), MANIFEST] {
            assert_eq!(declared(document, prefix), [names.as_str()]);
        }
    }
    for (prefix, names) in [
        (
            "terminal_interruption_reasons: ",
            OPENCODE_TERMINAL_INTERRUPTION_REASONS,
        ),
        ("status_filtered: ", OPENCODE_FILTERED_STATUSES),
    ] {
        let expected = names.join(", ");
        for document in [source_record(), MANIFEST] {
            assert_eq!(declared(document, prefix), [expected.as_str()]);
        }
    }
    assert_eq!(
        fixture["terminal_interruption_reasons"],
        serde_json::json!(OPENCODE_TERMINAL_INTERRUPTION_REASONS)
    );
    assert_eq!(
        fixture["status_filtered"],
        serde_json::json!(OPENCODE_FILTERED_STATUSES)
    );
    assert_eq!(fixture["idle_status"], OPENCODE_IDLE_STATUS);
}

#[test]
fn fixture_contains_no_real_content() {
    let forbidden =
        regex::Regex::new(r"sk-[a-zA-Z0-9]|ANTHROPIC_API_KEY|OPENAI_API_KEY|/Users/|/home/")
            .expect("fixed privacy guard");
    for document in [source_record(), MANIFEST, FIXTURE] {
        assert!(!forbidden.is_match(document));
    }
}

#[test]
fn registry_matches_fixture_manifest() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture");
    let expected = fixture["events"].as_array().expect("event list");
    let advertised: Vec<_> = OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .filter(|spec| spec.kind.is_some())
        .collect();
    assert_eq!(advertised.len(), expected.len());
    let mut unique = BTreeSet::new();
    for spec in OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .chain(OPENCODE_LEGACY_EVENT_SPECS)
        .chain(OPENCODE_DEPRECATED_ALIASES)
    {
        assert!(
            unique.insert(spec.name),
            "duplicate registry name across current/compatibility/alias tables"
        );
    }
    for (spec, fixture) in advertised.iter().zip(expected) {
        assert_eq!(spec.name, fixture["name"].as_str().expect("name"));
        let forwarding = match spec.forwarding {
            OpenCodeForwarding::Standalone => "standalone",
            OpenCodeForwarding::PluginHook => "plugin-hook",
            other => panic!("unexpected advertised forwarding: {other:?}"),
        };
        assert_eq!(fixture["forwarding"], forwarding);
        assert_eq!(
            format!("{:?}", spec.kind.expect("kind")),
            fixture["lifecycle"]
        );
        assert_eq!(spec.command.expect("verb").to_string(), fixture["verb"]);
        assert_eq!(
            spec.command.expect("verb").lifecycle_event_kind(),
            spec.kind.expect("kind")
        );
        let envelope: SessionHookEnvelope =
            serde_json::from_value(fixture["envelope"].clone()).expect("envelope");
        assert_eq!(
            opencode_provider()
                .parse_hook_event(spec.name, &envelope)
                .expect("parse current event")
                .kind,
            spec.kind.expect("kind")
        );
    }
    let merged: Vec<_> = OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .filter(|spec| spec.forwarding == OpenCodeForwarding::PluginMerged)
        .map(|spec| spec.name)
        .collect();
    assert_eq!(fixture["plugin_merged_events"], serde_json::json!(merged));
    assert!(
        !OPENCODE_HOOK_EVENT_SPECS
            .iter()
            .any(|spec| spec.name == "session.idle")
    );
    for name in ["session.error", "message.part.updated", "unknown.synthetic"] {
        assert!(!opencode_provider().recognizes_event(name));
    }
    for spec in OPENCODE_HOOK_EVENT_SPECS
        .iter()
        .filter(|spec| spec.kind.is_none())
    {
        assert!(!opencode_provider().recognizes_event(spec.name));
    }
}

#[test]
fn fallback_events_are_a_subset_of_advertised_names() {
    let fixture: Value = serde_json::from_str(FIXTURE).expect("fixture");
    for (specs, field) in [
        (OPENCODE_LEGACY_EVENT_SPECS, "compatibility_events"),
        (OPENCODE_DEPRECATED_ALIASES, "deprecated_aliases"),
    ] {
        let expected: Vec<_> = fixture[field]
            .as_array()
            .expect("compatibility names")
            .iter()
            .map(|value| value.as_str().expect("name"))
            .collect();
        assert_eq!(
            specs.iter().map(|spec| spec.name).collect::<Vec<_>>(),
            expected
        );
        for spec in specs {
            assert!(opencode_provider().recognizes_event(spec.name));
            assert!(
                opencode_provider()
                    .supported_commands()
                    .contains(&spec.command.expect("command"))
            );
            let envelope: SessionHookEnvelope = serde_json::from_value(serde_json::json!({
                "hook_event_name": spec.name, "session_id": "ses_synthetic_001",
                "cwd": "/tmp/synthetic-project", "prompt": "", "role": "user",
                "status": {"type":"idle"}
            }))
            .expect("legacy envelope");
            assert_eq!(
                opencode_provider()
                    .parse_hook_event(spec.name, &envelope)
                    .expect("legacy parse")
                    .kind,
                spec.kind.expect("kind")
            );
        }
    }
}
