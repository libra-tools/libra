//! CEX-16 usage stats persistence and aggregation tests.

use libra::internal::{
    ai::{
        completion::CompletionUsageSummary,
        usage::{
            UsageContext, UsagePrice, UsagePriceTable, UsageQuery, UsageQueryFilter, UsageRecorder,
        },
    },
    db::migration::run_builtin_migrations,
};
use sea_orm::{ConnectionTrait, Database, Statement};

fn usage_context(provider: &str, model: &str) -> UsageContext {
    UsageContext {
        session_id: Some("session-1".to_string()),
        thread_id: Some("thread-1".to_string()),
        agent_run_id: None,
        run_id: Some("run-1".to_string()),
        provider: provider.to_string(),
        model: model.to_string(),
        request_kind: "completion".to_string(),
        intent: Some("feature".to_string()),
        agent_name: None,
    }
}

fn usage_context_with_agent(provider: &str, model: &str, agent: &str) -> UsageContext {
    UsageContext {
        agent_name: Some(agent.to_string()),
        ..usage_context(provider, model)
    }
}

#[tokio::test]
async fn usage_recorder_persists_and_aggregates_by_model() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let recorder = UsageRecorder::new(conn.clone());
    let context = usage_context("openai", "gpt-test");

    recorder
        .record_summary_with_tool_count(
            &context,
            &CompletionUsageSummary {
                input_tokens: 10,
                output_tokens: 5,
                cached_tokens: Some(2),
                reasoning_tokens: Some(1),
                total_tokens: Some(16),
                cost_usd: Some(0.25),
            },
            Some(1200),
            2,
        )
        .await
        .expect("record first usage");
    recorder
        .record_summary(
            &context,
            &CompletionUsageSummary {
                input_tokens: 7,
                output_tokens: 3,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(10),
                cost_usd: None,
            },
            Some(800),
        )
        .await
        .expect("record second usage");

    let rows = UsageQuery::new(conn)
        .aggregate_by_model()
        .await
        .expect("aggregate usage");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].provider, "openai");
    assert_eq!(rows[0].model, "gpt-test");
    assert_eq!(rows[0].request_count, 2);
    assert_eq!(rows[0].prompt_tokens, 17);
    assert_eq!(rows[0].completion_tokens, 8);
    assert_eq!(rows[0].cached_tokens, 2);
    assert_eq!(rows[0].reasoning_tokens, 1);
    assert_eq!(rows[0].total_tokens, 26);
    assert_eq!(rows[0].tool_call_count, 2);
    assert_eq!(rows[0].wall_clock_ms, 2000);
    assert_eq!(rows[0].cost_usd, Some(0.25));
    assert_eq!(rows[0].cost_estimate_micro_dollars, Some(250_000));
    assert_eq!(rows[0].failed_count, 0);
}

#[tokio::test]
async fn usage_recorder_ignores_absent_or_zero_usage() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let recorder = UsageRecorder::new(conn.clone());
    let context = usage_context("ollama", "local-test");

    recorder
        .record_optional_summary(&context, None, Some(40))
        .await
        .expect("missing usage is tolerated");
    recorder
        .record_summary(&context, &CompletionUsageSummary::default(), Some(40))
        .await
        .expect("zero usage is tolerated");

    let rows = UsageQuery::new(conn)
        .aggregate_by_model()
        .await
        .expect("aggregate usage");
    assert!(rows.is_empty());
}

#[tokio::test]
async fn usage_recorder_estimates_cost_from_builtin_price_table() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let recorder = UsageRecorder::new(conn.clone());
    let context = usage_context("openai", "gpt-4o-mini");

    recorder
        .record_summary(
            &context,
            &CompletionUsageSummary {
                input_tokens: 1_000_000,
                output_tokens: 2_000_000,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(3_000_000),
                cost_usd: None,
            },
            Some(100),
        )
        .await
        .expect("record estimated cost usage");

    let rows = UsageQuery::new(conn)
        .aggregate_by_model()
        .await
        .expect("aggregate usage");

    assert_eq!(rows[0].cost_usd, None);
    assert_eq!(rows[0].cost_estimate_micro_dollars, Some(1_350_000));
}

#[tokio::test]
async fn usage_recorder_allows_project_price_overrides() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let pricing = UsagePriceTable::new().with_override(
        "custom",
        "model",
        UsagePrice::new(10, 20)
            .with_cached_micro_dollars_per_mtok(2)
            .with_reasoning_micro_dollars_per_mtok(30),
    );
    let recorder = UsageRecorder::with_pricing(conn.clone(), pricing);
    let context = usage_context("custom", "model");

    recorder
        .record_summary(
            &context,
            &CompletionUsageSummary {
                input_tokens: 2_000_000,
                output_tokens: 1_000_000,
                cached_tokens: Some(500_000),
                reasoning_tokens: Some(1_000_000),
                total_tokens: Some(4_000_000),
                cost_usd: None,
            },
            Some(100),
        )
        .await
        .expect("record override cost usage");

    let rows = UsageQuery::new(conn)
        .aggregate_by_model()
        .await
        .expect("aggregate usage");

    assert_eq!(rows[0].cost_estimate_micro_dollars, Some(66));
}

#[tokio::test]
async fn usage_recorder_records_missing_usage_and_failures() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let recorder = UsageRecorder::new(conn.clone());
    let context = usage_context("gemini", "gemini-test");

    recorder
        .record_missing_usage(&context, Some(250), 1)
        .await
        .expect("record estimated zero-token usage");
    recorder
        .record_failure(&context, "provider_error", Some(750))
        .await
        .expect("record failed usage");

    let rows = UsageQuery::new(conn)
        .aggregate_by_model()
        .await
        .expect("aggregate usage");

    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].request_count, 2);
    assert_eq!(rows[0].total_tokens, 0);
    assert_eq!(rows[0].tool_call_count, 1);
    assert_eq!(rows[0].wall_clock_ms, 1000);
    assert_eq!(rows[0].failed_count, 1);
}

#[tokio::test]
async fn usage_query_filter_excludes_failures_by_default_when_requested() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let recorder = UsageRecorder::new(conn.clone());
    let context = usage_context("openai", "gpt-test");

    recorder
        .record_summary(
            &context,
            &CompletionUsageSummary {
                input_tokens: 4,
                output_tokens: 6,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(10),
                cost_usd: None,
            },
            Some(100),
        )
        .await
        .expect("record success");
    recorder
        .record_failure(&context, "provider_error", Some(900))
        .await
        .expect("record failure");

    let success_rows = UsageQuery::new(conn.clone())
        .aggregate_by_model_filtered(&UsageQueryFilter {
            include_failed: false,
            ..UsageQueryFilter::default()
        })
        .await
        .expect("aggregate successes");
    assert_eq!(success_rows[0].request_count, 1);
    assert_eq!(success_rows[0].failed_count, 0);
    assert_eq!(success_rows[0].wall_clock_ms, 100);

    let all_rows = UsageQuery::new(conn)
        .aggregate_by_model_filtered(&UsageQueryFilter {
            include_failed: true,
            ..UsageQueryFilter::default()
        })
        .await
        .expect("aggregate all rows");
    assert_eq!(all_rows[0].request_count, 2);
    assert_eq!(all_rows[0].failed_count, 1);
    assert_eq!(all_rows[0].wall_clock_ms, 1000);
}

#[tokio::test]
async fn usage_recorder_prunes_rows_before_cutoff() {
    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let recorder = UsageRecorder::new(conn.clone());
    let context = usage_context("openai", "gpt-test");

    recorder
        .record_summary(
            &context,
            &CompletionUsageSummary {
                input_tokens: 1,
                output_tokens: 1,
                cached_tokens: None,
                reasoning_tokens: None,
                total_tokens: Some(2),
                cost_usd: None,
            },
            Some(10),
        )
        .await
        .expect("record usage");

    conn.execute_raw(Statement::from_sql_and_values(
        conn.get_database_backend(),
        "UPDATE agent_usage_stats SET started_at = ?, created_at = ?",
        vec![
            "2020-01-01T00:00:00+00:00".into(),
            "2020-01-01T00:00:00+00:00".into(),
        ],
    ))
    .await
    .expect("age usage row");

    let deleted = recorder
        .prune_before("2021-01-01T00:00:00+00:00")
        .await
        .expect("prune old rows");
    assert_eq!(deleted, 1);

    let rows = UsageQuery::new(conn)
        .aggregate_by_model()
        .await
        .expect("aggregate usage");
    assert!(rows.is_empty());
}

/// OC-Phase 5 P5.2: the recorder persists `agent_name` and the
/// query layer can aggregate at three documented grains:
/// `(provider, model)` (legacy), `(agent_name)`, and
/// `(agent_name, provider, model)`. Mixing rows with and without
/// `agent_name` exercises the legacy back-compat path: the
/// `(provider, model)` aggregation collapses every row regardless of
/// agent, while the `(agent_name, provider, model)` aggregation
/// surfaces the agent dimension and keeps `agent_name = None` for
/// the legacy row.
#[tokio::test]
async fn usage_query_aggregates_by_agent_name_grouping() {
    use libra::internal::ai::usage::UsageGrouping;

    let conn = Database::connect("sqlite::memory:")
        .await
        .expect("connect sqlite");
    run_builtin_migrations(&conn).await.expect("run migrations");
    let recorder = UsageRecorder::new(conn.clone());

    let summary = CompletionUsageSummary {
        input_tokens: 4,
        output_tokens: 2,
        cached_tokens: None,
        reasoning_tokens: None,
        total_tokens: Some(6),
        cost_usd: Some(0.10),
    };

    // Two `planner` rows on the same model — should fold into one
    // row of the agent-grain aggregation.
    let planner = usage_context_with_agent("openai", "gpt-4o", "planner");
    recorder
        .record_summary(&planner, &summary, Some(100))
        .await
        .expect("planner row 1");
    recorder
        .record_summary(&planner, &summary, Some(150))
        .await
        .expect("planner row 2");

    // One `explorer` row on a different model.
    let explorer = usage_context_with_agent("deepseek", "deepseek-chat", "explorer");
    recorder
        .record_summary(&explorer, &summary, Some(200))
        .await
        .expect("explorer row");

    // One legacy single-agent row (agent_name = None).
    let legacy = usage_context("openai", "gpt-4o");
    recorder
        .record_summary(&legacy, &summary, Some(50))
        .await
        .expect("legacy row");

    let query = UsageQuery::new(conn.clone());

    // Grain 1: legacy (provider, model). All four rows fold into two
    // groups; agent_name is None on every result.
    let by_pm = query
        .aggregate_filtered(UsageGrouping::ProviderModel, &UsageQueryFilter::default())
        .await
        .expect("by-provider-model");
    assert_eq!(by_pm.len(), 2, "two (provider, model) groups");
    assert!(by_pm.iter().all(|r| r.agent_name.is_none()));
    let openai_pm = by_pm
        .iter()
        .find(|r| r.provider == "openai" && r.model == "gpt-4o")
        .expect("openai/gpt-4o group");
    // 2 planner rows + 1 legacy row = 3 requests.
    assert_eq!(openai_pm.request_count, 3);

    // Grain 2: agent only. Three groups: planner / explorer / legacy(None).
    let by_agent = query
        .aggregate_filtered(UsageGrouping::Agent, &UsageQueryFilter::default())
        .await
        .expect("by-agent");
    assert_eq!(by_agent.len(), 3);
    let planner_total = by_agent
        .iter()
        .find(|r| r.agent_name.as_deref() == Some("planner"))
        .expect("planner group");
    assert_eq!(planner_total.request_count, 2);
    let legacy_total = by_agent
        .iter()
        .find(|r| r.agent_name.is_none())
        .expect("legacy group");
    assert_eq!(legacy_total.request_count, 1);
    assert!(legacy_total.provider.is_empty());

    // Grain 3: full (agent_name, provider, model). Three groups
    // because (planner, openai, gpt-4o) folds two rows; the others
    // each contribute one row.
    let by_apm = query
        .aggregate_filtered(
            UsageGrouping::AgentProviderModel,
            &UsageQueryFilter::default(),
        )
        .await
        .expect("by-agent-provider-model");
    assert_eq!(by_apm.len(), 3);
    let planner_apm = by_apm
        .iter()
        .find(|r| {
            r.agent_name.as_deref() == Some("planner")
                && r.provider == "openai"
                && r.model == "gpt-4o"
        })
        .expect("planner/openai/gpt-4o group");
    assert_eq!(planner_apm.request_count, 2);
}
