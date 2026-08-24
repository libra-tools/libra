//! Plain-text formatters for the historical OC-Phase 5 P5.4 interactive surfaces
//! (`/agents`, `/budget`, `/usage --by=agent`).
//!
//! The retired TUI (removed in W5-03) used to dispatch a slash command
//! and render the returned string into its AssistantHistoryCell. Keeping
//! the formatters in the agent layer (not a UI layer) means the
//! same renderers can be reused by `libra agent status` (CLI), an
//! external observability pipe, or a future JSON projection without
//! coupling to terminal-renderer types.
//!
//! All renderers are pure functions of their input (no I/O, no clock
//! reads) so callers can snapshot them in tests without mocking.
//!
//! ## Output format
//!
//! Stable, monospace-friendly tables with a one-line title. Columns
//! are aligned with `width` formatting so the output looks correct in
//! both a 100-col UI cell and a piped-to-grep terminal. Rows are
//! sorted by the leading column (agent name / provider) so
//! consecutive runs produce identical output for the same data.

use std::collections::BTreeMap;

use super::{
    budget::{BudgetTracker, BudgetWarning},
    profile::config::{AgentsConfig, PerAgentBudgetConfig, PermissionPolicy},
};
use crate::internal::ai::usage::UsageAggregate;

/// Render the contents of `[code.agents.<name>]` as a small monospace
/// table. Every declared agent appears on its own row. When the
/// section is empty the function returns a documented placeholder so
/// the operator sees actionable guidance instead of a blank pane.
pub fn format_agents_table(config: &AgentsConfig) -> String {
    if config.agents.is_empty() {
        return "No `[code.agents.<name>]` entries declared. Add them to `agents.toml` to \
                enable the multi-agent runtime."
            .to_string();
    }

    let mut out = String::new();
    out.push_str("Agents (declarative):\n");
    let header = format!(
        "  {:<14} {:<10} {:<48} {:<24} {:>5}",
        "name", "mode", "model", "permission", "steps"
    );
    out.push_str(&header);
    out.push('\n');
    out.push_str("  ");
    out.push_str(&"-".repeat(header.len() - 2));
    out.push('\n');
    for (name, agent) in &config.agents {
        out.push_str(&format!(
            "  {:<14} {:<10} {:<48} {:<24} {:>5}\n",
            name,
            agent.mode,
            truncate(&agent.model, 48),
            format_permission_summary(&agent.permission),
            agent
                .steps
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".to_string()),
        ));
    }
    out
}

/// Render the running session / per-agent / goal budget totals
/// against the configured caps. Formatter mirrors the doc's
/// `[code.budget]` ordering so an operator can correlate the output
/// row-by-row with their `agents.toml` tree.
pub fn format_budget_status(
    config: &AgentsConfig,
    tracker: &BudgetTracker,
    pending_warnings: &[BudgetWarning],
) -> String {
    let mut out = String::new();
    out.push_str("Budget:\n");

    // ----- Session -----
    out.push_str("  session:\n");
    push_budget_axis(
        &mut out,
        "cost",
        format!("${:.4}", tracker.session_cost_usd()),
        config
            .budget
            .max_session_cost_usd
            .map(|v| format!("${v:.4}")),
        config
            .budget
            .warn_session_cost_usd
            .map(|v| format!("${v:.4}")),
    );
    push_budget_axis(
        &mut out,
        "tokens",
        tracker.session_total_tokens().to_string(),
        config.budget.max_session_tokens.map(|v| v.to_string()),
        None,
    );
    push_budget_axis(
        &mut out,
        "wall_clock_ms",
        tracker.session_wall_clock_ms().to_string(),
        None,
        None,
    );

    // ----- Goal -----
    if config.goal.enabled || has_goal_caps(&config.budget.goal) {
        out.push_str("  goal:\n");
        push_budget_axis(
            &mut out,
            "cost",
            format!("${:.4}", tracker.session_cost_usd()),
            config.budget.goal.max_cost_usd.map(|v| format!("${v:.4}")),
            config.budget.goal.warn_cost_usd.map(|v| format!("${v:.4}")),
        );
        push_budget_axis(
            &mut out,
            "wall_clock_min",
            format!("{:.2}", tracker.session_wall_clock_ms() as f64 / 60_000.0),
            config
                .budget
                .goal
                .max_wall_clock_minutes
                .map(|v| v.to_string()),
            config
                .budget
                .goal
                .warn_wall_clock_minutes
                .map(|v| v.to_string()),
        );
    }

    // ----- Per-agent -----
    if !config.budget.per_agent.is_empty() {
        out.push_str("  per_agent:\n");
        for (name, cap) in &config.budget.per_agent {
            push_per_agent_caps(&mut out, name, cap, tracker);
        }
    }

    // ----- Pending warnings -----
    if !pending_warnings.is_empty() {
        out.push_str("\n  warnings (one-shot, not yet at hard cap):\n");
        for warning in pending_warnings {
            out.push_str(&format!(
                "    {:<14} {:<10} actual {:>10}  warn-at {:>10}\n",
                budget_scope_label(&warning.scope),
                warning.axis.as_str(),
                warning.actual,
                warning.threshold
            ));
        }
    }

    out
}

/// Render a `Vec<UsageAggregate>` rows as a small monospace table.
/// Caller picks the [`UsageGrouping`](
/// crate::internal::ai::usage::UsageGrouping) before calling
/// [`UsageQuery::aggregate_filtered`](
/// crate::internal::ai::usage::UsageQuery::aggregate_filtered); this
/// function only renders. Empty rows produce a documented placeholder
/// so the UI never shows a confusing blank cell.
pub fn format_usage_table(rows: &[UsageAggregate]) -> String {
    if rows.is_empty() {
        return "No usage recorded yet.".to_string();
    }
    let mut out = String::new();
    out.push_str("Usage:\n");
    let header = format!(
        "  {:<14} {:<10} {:<24} {:>8} {:>10} {:>10} {:>10}",
        "agent", "provider", "model", "requests", "in_tok", "out_tok", "cost_usd"
    );
    out.push_str(&header);
    out.push('\n');
    out.push_str("  ");
    out.push_str(&"-".repeat(header.len() - 2));
    out.push('\n');

    for row in rows {
        out.push_str(&format!(
            "  {:<14} {:<10} {:<24} {:>8} {:>10} {:>10} {:>10}\n",
            row.agent_name.as_deref().unwrap_or("—"),
            if row.provider.is_empty() {
                "—"
            } else {
                &row.provider
            },
            if row.model.is_empty() {
                "—"
            } else {
                &row.model
            },
            row.request_count,
            row.prompt_tokens,
            row.completion_tokens,
            row.cost_usd
                .map(|v| format!("${v:.4}"))
                .unwrap_or_else(|| "—".to_string()),
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn truncate(s: &str, max: usize) -> String {
    // Char-count based, NOT byte-length based, so a UTF-8 string with
    // multi-byte codepoints (e.g. a CJK model name slug) cannot panic
    // by slicing mid-codepoint. The 48-char column limit in the
    // agents-table renderer counts characters, which is also closer to
    // what the operator sees in a monospace UI cell than byte count.
    let char_count = s.chars().count();
    if char_count <= max {
        return s.to_string();
    }
    if max <= 1 {
        return s.chars().take(max).collect();
    }
    let truncated: String = s.chars().take(max - 1).collect();
    format!("{truncated}…")
}

fn format_permission_summary(perm: &BTreeMap<String, PermissionPolicy>) -> String {
    if perm.is_empty() {
        return "(default)".to_string();
    }
    let mut entries: Vec<String> = perm
        .iter()
        .map(|(k, v)| format!("{}={}", k, policy_label(*v)))
        .collect();
    entries.sort();
    entries.join(", ")
}

fn policy_label(p: PermissionPolicy) -> &'static str {
    match p {
        PermissionPolicy::Allow => "allow",
        PermissionPolicy::Deny => "deny",
        PermissionPolicy::Ask => "ask",
    }
}

fn push_budget_axis(
    out: &mut String,
    label: &str,
    actual: String,
    max: Option<String>,
    warn: Option<String>,
) {
    let max_label = max.unwrap_or_else(|| "—".to_string());
    let warn_label = warn.unwrap_or_else(|| "—".to_string());
    out.push_str(&format!(
        "    {:<14} actual {:>12}  warn-at {:>12}  cap {:>12}\n",
        label, actual, warn_label, max_label
    ));
}

fn push_per_agent_caps(
    out: &mut String,
    name: &str,
    cap: &PerAgentBudgetConfig,
    tracker: &BudgetTracker,
) {
    out.push_str(&format!("    {name}:\n"));
    if let Some(max_usd) = cap.max_cost_usd {
        out.push_str(&format!(
            "      {:<14} actual {:>12}  cap {:>12}\n",
            "cost",
            format!("${:.4}", tracker.agent_cost_usd(name)),
            format!("${max_usd:.4}"),
        ));
    }
    if let Some(max_steps) = cap.max_steps {
        out.push_str(&format!(
            "      {:<14} actual {:>12}  cap {:>12}\n",
            "steps",
            tracker.agent_steps(name),
            max_steps,
        ));
    }
}

fn budget_scope_label(scope: &super::budget::BudgetScope) -> String {
    match scope {
        super::budget::BudgetScope::Session => "session".to_string(),
        super::budget::BudgetScope::Goal => "goal".to_string(),
        super::budget::BudgetScope::Agent { name } => name.clone(),
    }
}

fn has_goal_caps(goal: &super::profile::config::GoalBudgetConfig) -> bool {
    goal.warn_cost_usd.is_some()
        || goal.max_cost_usd.is_some()
        || goal.warn_wall_clock_minutes.is_some()
        || goal.max_wall_clock_minutes.is_some()
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::internal::ai::agent::profile::config::{
        AgentConfigEntry, AgentsConfig, BudgetConfig, GoalConfig, PerAgentBudgetConfig,
    };

    fn config_with(agents: Vec<(&str, AgentConfigEntry)>) -> AgentsConfig {
        let mut map = BTreeMap::new();
        for (name, entry) in agents {
            map.insert(name.to_string(), entry);
        }
        AgentsConfig {
            agents: map,
            ..AgentsConfig::default()
        }
    }

    fn entry(model: &str, mode: &str) -> AgentConfigEntry {
        AgentConfigEntry {
            model: model.to_string(),
            mode: mode.to_string(),
            tools: Vec::new(),
            permission: BTreeMap::new(),
            steps: None,
        }
    }

    #[test]
    fn agents_table_empty_yields_actionable_placeholder() {
        let out = format_agents_table(&AgentsConfig::default());
        assert!(out.contains("No `[code.agents.<name>]`"));
        assert!(out.contains("agents.toml"));
    }

    #[test]
    fn agents_table_lists_each_declared_agent_in_sorted_order() {
        let cfg = config_with(vec![
            ("explorer", entry("deepseek/deepseek-chat", "subagent")),
            (
                "planner",
                entry("anthropic/claude-3-5-sonnet-latest", "primary"),
            ),
        ]);
        let out = format_agents_table(&cfg);
        // Header present.
        assert!(out.contains("Agents (declarative):"));
        assert!(out.contains("name") && out.contains("mode") && out.contains("model"));
        // BTreeMap iteration order: explorer < planner.
        let explorer_pos = out.find("explorer").expect("explorer row");
        let planner_pos = out.find("planner").expect("planner row");
        assert!(explorer_pos < planner_pos);
        assert!(out.contains("anthropic/claude-3-5-sonnet-latest"));
        assert!(out.contains("deepseek/deepseek-chat"));
    }

    #[test]
    fn agents_table_truncates_long_model_names() {
        // Model id longer than the 48-char column should end with `…`.
        let long_model = format!("acmecorp/{}", "x".repeat(80));
        let cfg = config_with(vec![("solo", entry(&long_model, "primary"))]);
        let out = format_agents_table(&cfg);
        assert!(out.contains('…'), "expected ellipsis in truncated cell");
    }

    /// Char-boundary safety: a model name containing multi-byte UTF-8
    /// codepoints (e.g. a CJK slug) must not panic when truncated.
    /// Regression guard against the byte-index slicing bug Codex
    /// flagged on the first version of `truncate`.
    #[test]
    fn agents_table_truncate_handles_multibyte_codepoints_without_panic() {
        // `测试` is two CJK codepoints (3 bytes each in UTF-8); chain
        // many of them so the resulting string crosses the 48-char
        // column limit and forces the truncate path.
        let long_cjk: String = "测试".repeat(60);
        let cfg = config_with(vec![("cn", entry(&long_cjk, "primary"))]);
        let out = format_agents_table(&cfg);
        assert!(out.contains('…'));
        // The truncated cell still ends on a valid char boundary —
        // proven by the fact that we got here without a panic and
        // the output is valid UTF-8 (it must be, it's a `String`).
        assert!(out.is_char_boundary(out.len()));
    }

    #[test]
    fn agents_table_renders_permission_categories_sorted() {
        let mut perm = BTreeMap::new();
        perm.insert("write".to_string(), PermissionPolicy::Deny);
        perm.insert("shell".to_string(), PermissionPolicy::Ask);
        let cfg = config_with(vec![(
            "tight",
            AgentConfigEntry {
                model: "openai/gpt-4o".to_string(),
                mode: "subagent".to_string(),
                tools: Vec::new(),
                permission: perm,
                steps: Some(15),
            },
        )]);
        let out = format_agents_table(&cfg);
        // Categories sorted alphabetically; values rendered as the
        // canonical lower-case label.
        assert!(out.contains("shell=ask, write=deny"));
        // Steps shown.
        assert!(out.contains("15"));
    }

    #[test]
    fn budget_status_renders_session_caps_and_running_totals() {
        let cfg = AgentsConfig {
            budget: BudgetConfig {
                max_session_cost_usd: Some(5.0),
                warn_session_cost_usd: Some(2.0),
                max_session_tokens: Some(100_000),
                ..BudgetConfig::default()
            },
            ..AgentsConfig::default()
        };
        let mut tracker = BudgetTracker::new();
        tracker.accumulate(
            &crate::internal::ai::completion::CompletionUsageSummary {
                input_tokens: 100,
                output_tokens: 50,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(150),
                cost_usd: Some(1.25),
            },
            Some(45_000),
            None,
        );
        let out = format_budget_status(&cfg, &tracker, &[]);
        assert!(out.contains("Budget:"));
        assert!(out.contains("session:"));
        assert!(out.contains("$1.2500"));
        assert!(out.contains("$5.0000"));
        assert!(out.contains("$2.0000"));
        // Tokens row.
        assert!(out.contains("150"));
        assert!(out.contains("100000"));
    }

    #[test]
    fn budget_status_omits_goal_section_when_neither_enabled_nor_capped() {
        let cfg = AgentsConfig::default();
        let tracker = BudgetTracker::new();
        let out = format_budget_status(&cfg, &tracker, &[]);
        assert!(
            !out.contains("goal:"),
            "goal section must hide when default"
        );
    }

    #[test]
    fn budget_status_renders_goal_section_when_caps_present() {
        let cfg = AgentsConfig {
            goal: GoalConfig {
                enabled: false,
                ..GoalConfig::default()
            },
            budget: BudgetConfig {
                goal: super::super::profile::config::GoalBudgetConfig {
                    max_cost_usd: Some(3.0),
                    warn_cost_usd: Some(1.0),
                    max_wall_clock_minutes: Some(60),
                    warn_wall_clock_minutes: Some(30),
                },
                ..BudgetConfig::default()
            },
            ..AgentsConfig::default()
        };
        let tracker = BudgetTracker::new();
        let out = format_budget_status(&cfg, &tracker, &[]);
        assert!(out.contains("goal:"));
        assert!(out.contains("$3.0000"));
        assert!(out.contains("60"));
    }

    #[test]
    fn budget_status_renders_per_agent_section() {
        let mut agents = BTreeMap::new();
        agents.insert(
            "explorer".to_string(),
            entry("deepseek/deepseek-chat", "subagent"),
        );
        let mut per_agent = BTreeMap::new();
        per_agent.insert(
            "explorer".to_string(),
            PerAgentBudgetConfig {
                max_cost_usd: Some(1.0),
                max_steps: Some(20),
            },
        );
        let cfg = AgentsConfig {
            agents,
            budget: BudgetConfig {
                per_agent,
                ..BudgetConfig::default()
            },
            ..AgentsConfig::default()
        };
        let mut tracker = BudgetTracker::new();
        for _ in 0..7 {
            tracker.record_step(Some("explorer"));
        }
        let out = format_budget_status(&cfg, &tracker, &[]);
        assert!(out.contains("per_agent:"));
        assert!(out.contains("explorer:"));
        assert!(out.contains("steps"));
        assert!(out.contains("7"));
        assert!(out.contains("20"));
    }

    #[test]
    fn budget_status_renders_warnings_section() {
        let cfg = AgentsConfig::default();
        let tracker = BudgetTracker::new();
        let warnings = vec![BudgetWarning {
            axis: super::super::budget::BudgetAxis::Cost,
            scope: super::super::budget::BudgetScope::Session,
            threshold: "$2.0000".to_string(),
            actual: "$2.5000".to_string(),
        }];
        let out = format_budget_status(&cfg, &tracker, &warnings);
        assert!(out.contains("warnings"));
        assert!(out.contains("$2.5000"));
        assert!(out.contains("$2.0000"));
    }

    #[test]
    fn usage_table_empty_yields_placeholder() {
        let out = format_usage_table(&[]);
        assert!(out.contains("No usage recorded"));
    }

    #[test]
    fn usage_table_renders_aggregates_with_optional_agent_dimension() {
        let rows = vec![
            UsageAggregate {
                agent_name: Some("planner".to_string()),
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
                request_count: 3,
                prompt_tokens: 100,
                completion_tokens: 60,
                cached_tokens: 0,
                reasoning_tokens: 0,
                total_tokens: 160,
                tool_call_count: 5,
                wall_clock_ms: 4_000,
                cost_usd: Some(0.0123),
                cost_estimate_micro_dollars: None,
                failed_count: 0,
                unknown_usage_count: 0,
                unknown_cost_count: 0,
            },
            UsageAggregate {
                agent_name: None,
                provider: "anthropic".to_string(),
                model: "claude-3-5-sonnet-latest".to_string(),
                request_count: 1,
                prompt_tokens: 20,
                completion_tokens: 30,
                cached_tokens: 0,
                reasoning_tokens: 0,
                total_tokens: 50,
                tool_call_count: 0,
                wall_clock_ms: 1_000,
                cost_usd: None,
                cost_estimate_micro_dollars: None,
                failed_count: 0,
                unknown_usage_count: 0,
                unknown_cost_count: 0,
            },
        ];
        let out = format_usage_table(&rows);
        assert!(out.contains("Usage:"));
        assert!(out.contains("planner"));
        assert!(out.contains("openai"));
        assert!(out.contains("$0.0123"));
        // Legacy row (agent_name=None) renders as the documented em-dash.
        assert!(out.contains("—"));
    }
}
