//! CEX-01 command safety contract tests.
//!
//! These tests pin the provider-neutral safety decision shape and load the
//! fixture corpus that later CEX-02 / CEX-03 enforcement work must preserve.

use libra::internal::ai::{
    command_safety::classify_ai_command_safety,
    hardening::{BlastRadius, CommandSafetySurface, SafetyDecision, SafetyDisposition},
};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    name: String,
    surface: CommandSafetySurface,
    command: String,
    #[serde(default)]
    args: Vec<String>,
    expected: SafetyDisposition,
    rule_name: String,
    blast_radius: String,
}

fn fixtures() -> Vec<Fixture> {
    include_str!("data/ai_safety/command_safety.jsonl")
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| serde_json::from_str(line).expect("fixture line must be valid JSON"))
        .collect()
}

#[test]
fn ai_command_safety_decision_constructors_pin_three_way_contract() {
    let allow = SafetyDecision::allow("fixture.allow", "read-only fixture", BlastRadius::Workspace);
    assert!(allow.is_allow());
    assert!(!allow.is_deny());
    assert!(!allow.is_needs_human());

    let deny = SafetyDecision::deny("fixture.deny", "destructive fixture", BlastRadius::System);
    assert!(deny.is_deny());
    assert!(!deny.is_allow());
    assert!(!deny.is_needs_human());

    let needs_human = SafetyDecision::needs_human(
        "fixture.needs_human",
        "ambiguous fixture",
        BlastRadius::Unknown,
    );
    assert!(needs_human.is_needs_human());
    assert!(!needs_human.is_allow());
    assert!(!needs_human.is_deny());
}

#[test]
fn ai_command_safety_fixture_corpus_has_required_coverage() {
    let fixtures = fixtures();

    assert!(
        fixtures.len() >= 50,
        "CEX-01 requires at least 50 command safety fixtures"
    );
    assert!(fixtures.iter().any(|fixture| fixture.expected.is_allow()));
    assert!(fixtures.iter().any(|fixture| fixture.expected.is_deny()));
    assert!(
        fixtures
            .iter()
            .any(|fixture| fixture.expected.needs_human())
    );
    assert!(
        fixtures
            .iter()
            .any(|fixture| fixture.surface == CommandSafetySurface::Shell)
    );
    assert!(
        fixtures
            .iter()
            .any(|fixture| fixture.surface == CommandSafetySurface::LibraVcs)
    );
}

#[test]
fn ai_command_safety_fixtures_match_contract_classifier() {
    for fixture in fixtures() {
        let decision = classify_ai_command_safety(fixture.surface, &fixture.command, &fixture.args);

        assert_eq!(
            decision.disposition, fixture.expected,
            "{}: wrong disposition for {} {:?}",
            fixture.name, fixture.command, fixture.args
        );
        assert_eq!(
            decision.rule_name, fixture.rule_name,
            "{}: wrong rule for {} {:?}",
            fixture.name, fixture.command, fixture.args
        );
        assert_eq!(
            decision.blast_radius.to_string(),
            fixture.blast_radius,
            "{}: wrong blast radius for {} {:?}",
            fixture.name,
            fixture.command,
            fixture.args
        );
        assert!(
            !decision.reason.trim().is_empty(),
            "{}: safety decisions must carry a user-facing reason",
            fixture.name
        );
    }
}
