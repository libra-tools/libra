//! CEX-16 usage display formatting tests.

use libra::internal::ai::usage::{
    UsageAggregate, UsageDisplaySnapshot, format_usage_badge, format_usage_detail_panel,
};

#[test]
fn usage_badge_shows_model_tokens_and_wall_clock() {
    let snapshot = UsageDisplaySnapshot {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        prompt_tokens: 1200,
        completion_tokens: 345,
        wall_clock_ms: 12_345,
        cost_usd: Some(0.42),
    };

    let badge = format_usage_badge(&snapshot);

    assert!(badge.contains("openai/gpt-test"));
    assert!(badge.contains("1.5k tok"));
    assert!(badge.contains("12.3s"));
    assert!(badge.contains("$0.4200"));
}

#[test]
fn usage_detail_panel_formats_sqlite_aggregates() {
    let snapshot = UsageDisplaySnapshot {
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        prompt_tokens: 1200,
        completion_tokens: 345,
        wall_clock_ms: 12_345,
        cost_usd: Some(0.42),
    };
    let aggregate = UsageAggregate {
        agent_name: Some("planner".to_string()),
        provider: "openai".to_string(),
        model: "gpt-test".to_string(),
        request_count: 2,
        prompt_tokens: 2000,
        completion_tokens: 500,
        cached_tokens: 100,
        reasoning_tokens: 50,
        total_tokens: 2650,
        tool_call_count: 3,
        wall_clock_ms: 20_000,
        cost_usd: None,
        cost_estimate_micro_dollars: Some(1234),
        failed_count: 1,
        unknown_usage_count: 0,
        unknown_cost_count: 0,
    };

    let panel = format_usage_detail_panel(&snapshot, "agent/provider/model", &[aggregate]);

    assert!(panel.contains("Usage Details"));
    assert!(panel.contains("Grouped by: agent/provider/model"));
    assert!(panel.contains("planner openai/gpt-test"));
    assert!(panel.contains("req 2"));
    assert!(panel.contains("tools 3"));
    assert!(panel.contains("~$0.0012"));
    assert!(panel.contains("failed 1"));
}
