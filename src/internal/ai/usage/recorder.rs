use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, Value};
use uuid::Uuid;

use crate::internal::ai::{
    completion::CompletionUsageSummary,
    usage::{pricing::UsagePriceTable, query::UsageQuery},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageContext {
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub agent_run_id: Option<String>,
    pub run_id: Option<String>,
    pub provider: String,
    pub model: String,
    pub request_kind: String,
    pub intent: Option<String>,
    /// Declarative agent profile name from the multi-agent runtime
    /// (`planner` / `explorer` / `reviewer` / …). `None` for the
    /// single-agent legacy path; the `agent_usage_stats` row stores
    /// NULL in that case so existing aggregation continues to match
    /// the original (provider, model) grain. See OC-Phase 5 P5.2.
    pub agent_name: Option<String>,
}

#[derive(Clone, Debug)]
pub struct UsageRecorder {
    conn: DatabaseConnection,
    pricing: UsagePriceTable,
}

impl UsageRecorder {
    pub fn new(conn: DatabaseConnection) -> Self {
        Self {
            conn,
            pricing: UsagePriceTable::new(),
        }
    }

    pub fn with_pricing(conn: DatabaseConnection, pricing: UsagePriceTable) -> Self {
        Self { conn, pricing }
    }

    pub fn query(&self) -> UsageQuery {
        UsageQuery::new(self.conn.clone())
    }

    pub async fn record_optional_summary(
        &self,
        context: &UsageContext,
        summary: Option<&CompletionUsageSummary>,
        wall_clock_ms: Option<u64>,
    ) -> Result<(), DbErr> {
        let Some(summary) = summary else {
            return Ok(());
        };
        self.record_summary(context, summary, wall_clock_ms).await
    }

    pub async fn record_summary(
        &self,
        context: &UsageContext,
        summary: &CompletionUsageSummary,
        wall_clock_ms: Option<u64>,
    ) -> Result<(), DbErr> {
        self.record_summary_with_tool_count(context, summary, wall_clock_ms, 0)
            .await
    }

    pub async fn record_summary_with_tool_count(
        &self,
        context: &UsageContext,
        summary: &CompletionUsageSummary,
        wall_clock_ms: Option<u64>,
        tool_call_count: u64,
    ) -> Result<(), DbErr> {
        if summary.is_zero() {
            return Ok(());
        }
        self.insert_row(UsageInsert {
            context,
            summary: Some(summary),
            wall_clock_ms: wall_clock_ms.unwrap_or(0),
            tool_call_count,
            usage_estimated: false,
            success: true,
            error_kind: None,
        })
        .await
    }

    pub async fn record_missing_usage(
        &self,
        context: &UsageContext,
        wall_clock_ms: Option<u64>,
        tool_call_count: u64,
    ) -> Result<(), DbErr> {
        self.insert_row(UsageInsert {
            context,
            summary: None,
            wall_clock_ms: wall_clock_ms.unwrap_or(0),
            tool_call_count,
            usage_estimated: true,
            success: true,
            error_kind: None,
        })
        .await
    }

    pub async fn record_failure(
        &self,
        context: &UsageContext,
        error_kind: &str,
        wall_clock_ms: Option<u64>,
    ) -> Result<(), DbErr> {
        self.insert_row(UsageInsert {
            context,
            summary: None,
            wall_clock_ms: wall_clock_ms.unwrap_or(0),
            tool_call_count: 0,
            usage_estimated: false,
            success: false,
            error_kind: Some(error_kind),
        })
        .await
    }

    pub async fn prune_before(&self, cutoff_rfc3339: &str) -> Result<u64, DbErr> {
        let backend = self.conn.get_database_backend();
        let result = self
            .conn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "DELETE FROM agent_usage_stats \
                 WHERE COALESCE(started_at, created_at) < ?",
                vec![cutoff_rfc3339.to_string().into()],
            ))
            .await?;
        Ok(result.rows_affected())
    }

    async fn insert_row(&self, input: UsageInsert<'_>) -> Result<(), DbErr> {
        let now = Utc::now().to_rfc3339();
        let summary = input.summary.cloned().unwrap_or_default();
        let total_tokens = summary.total_tokens.unwrap_or_else(|| {
            summary
                .input_tokens
                .saturating_add(summary.output_tokens)
                .saturating_add(summary.reasoning_tokens.unwrap_or(0))
        });
        let cost_micro_dollars = cost_micro_dollars(summary.cost_usd).or_else(|| {
            self.pricing.estimate_micro_dollars(
                &input.context.provider,
                &input.context.model,
                &summary,
            )
        });
        let backend = self.conn.get_database_backend();
        self.conn
            .execute_raw(Statement::from_sql_and_values(
                backend,
                "INSERT INTO agent_usage_stats \
                 (id, session_id, thread_id, agent_run_id, run_id, provider, model, agent_name, request_kind, intent, prompt_tokens, completion_tokens, cached_tokens, reasoning_tokens, total_tokens, tool_call_count, wall_clock_ms, provider_latency_ms, cost_estimate_micro_dollars, cost_usd, usage_estimated, started_at, finished_at, success, error_kind, schema_version, created_at) \
                 VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
                vec![
                    Uuid::new_v4().to_string().into(),
                    input.context.session_id.clone().into(),
                    input.context.thread_id.clone().into(),
                    input.context.agent_run_id.clone().into(),
                    input.context.run_id.clone().into(),
                    input.context.provider.clone().into(),
                    input.context.model.clone().into(),
                    input.context.agent_name.clone().into(),
                    input.context.request_kind.clone().into(),
                    input.context.intent.clone().into(),
                    u64_to_i64_value(summary.input_tokens),
                    u64_to_i64_value(summary.output_tokens),
                    u64_to_i64_value(summary.cached_tokens.unwrap_or(0)),
                    u64_to_i64_value(summary.reasoning_tokens.unwrap_or(0)),
                    u64_to_i64_value(total_tokens),
                    u64_to_i64_value(input.tool_call_count),
                    u64_to_i64_value(input.wall_clock_ms),
                    Value::BigInt(None),
                    optional_i64_value(cost_micro_dollars),
                    summary.cost_usd.into(),
                    bool_to_i64_value(input.usage_estimated),
                    now.clone().into(),
                    now.clone().into(),
                    bool_to_i64_value(input.success),
                    input.error_kind.map(str::to_string).into(),
                    1_i64.into(),
                    now.into(),
                ],
            ))
            .await?;
        Ok(())
    }
}

struct UsageInsert<'a> {
    context: &'a UsageContext,
    summary: Option<&'a CompletionUsageSummary>,
    wall_clock_ms: u64,
    tool_call_count: u64,
    usage_estimated: bool,
    success: bool,
    error_kind: Option<&'a str>,
}

fn u64_to_i64_value(value: u64) -> Value {
    i64::try_from(value).unwrap_or(i64::MAX).into()
}

fn optional_i64_value(value: Option<i64>) -> Value {
    value.into()
}

fn bool_to_i64_value(value: bool) -> Value {
    i64::from(value).into()
}

fn cost_micro_dollars(cost_usd: Option<f64>) -> Option<i64> {
    let cost = cost_usd?;
    if !cost.is_finite() || cost < 0.0 {
        return None;
    }
    let micro_dollars = cost * 1_000_000.0;
    if micro_dollars > i64::MAX as f64 {
        None
    } else {
        Some(micro_dollars.round() as i64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `u64_to_i64_value` must clamp values exceeding `i64::MAX` to
    /// `i64::MAX` rather than wrapping or panicking. Pin both the
    /// happy path (`u64::MAX -> i64::MAX`) and a representative
    /// in-range value.
    #[test]
    fn u64_to_i64_value_clamps_overflow_to_i64_max() {
        // In-range value passes through unchanged.
        match u64_to_i64_value(42) {
            Value::BigInt(Some(v)) => assert_eq!(v, 42),
            other => panic!("expected BigInt(Some), got {other:?}"),
        }
        match u64_to_i64_value(0) {
            Value::BigInt(Some(v)) => assert_eq!(v, 0),
            other => panic!("expected BigInt(Some), got {other:?}"),
        }
        // i64::MAX is the boundary — exactly representable.
        match u64_to_i64_value(i64::MAX as u64) {
            Value::BigInt(Some(v)) => assert_eq!(v, i64::MAX),
            other => panic!("expected BigInt(Some), got {other:?}"),
        }
        // u64::MAX overflows -> clamped to i64::MAX.
        match u64_to_i64_value(u64::MAX) {
            Value::BigInt(Some(v)) => assert_eq!(v, i64::MAX),
            other => panic!("expected BigInt(Some), got {other:?}"),
        }
    }

    /// `bool_to_i64_value` maps `true -> 1` and `false -> 0`. Pin
    /// against a future "true→-1" sentinel encoding refactor.
    #[test]
    fn bool_to_i64_value_maps_true_one_false_zero() {
        match bool_to_i64_value(true) {
            Value::BigInt(Some(v)) => assert_eq!(v, 1),
            other => panic!("expected BigInt(Some), got {other:?}"),
        }
        match bool_to_i64_value(false) {
            Value::BigInt(Some(v)) => assert_eq!(v, 0),
            other => panic!("expected BigInt(Some), got {other:?}"),
        }
    }

    /// `optional_i64_value` round-trips both Some and None.
    #[test]
    fn optional_i64_value_threads_some_and_none() {
        match optional_i64_value(Some(42)) {
            Value::BigInt(Some(v)) => assert_eq!(v, 42),
            other => panic!("expected BigInt(Some), got {other:?}"),
        }
        match optional_i64_value(None) {
            Value::BigInt(None) => {}
            other => panic!("expected BigInt(None), got {other:?}"),
        }
    }

    /// `cost_micro_dollars` happy path: USD * 1e6 rounded to i64
    /// using `f64::round()` (round-half-away-from-zero).
    #[test]
    fn cost_micro_dollars_converts_usd_to_micro_dollars() {
        assert_eq!(cost_micro_dollars(Some(0.0)), Some(0));
        assert_eq!(cost_micro_dollars(Some(1.0)), Some(1_000_000));
        // 0.0012345 USD = 1234.5 micro-dollars → 1235 via round-
        // half-away-from-zero. Pin the rounding direction.
        assert_eq!(cost_micro_dollars(Some(0.0012345)), Some(1235));
        // 0.0000005 USD = 0.5 → rounds to 1; 0.0000004 → 0.
        assert_eq!(cost_micro_dollars(Some(0.0000005)), Some(1));
        assert_eq!(cost_micro_dollars(Some(0.0000004)), Some(0));
    }

    /// `cost_micro_dollars` rejects negative / NaN / ±Inf inputs.
    /// Pin so a future "saturate to 0" refactor catches a wider net.
    #[test]
    fn cost_micro_dollars_rejects_invalid_inputs() {
        assert_eq!(cost_micro_dollars(None), None);
        assert_eq!(cost_micro_dollars(Some(-0.01)), None);
        assert_eq!(cost_micro_dollars(Some(f64::NAN)), None);
        assert_eq!(cost_micro_dollars(Some(f64::INFINITY)), None);
        assert_eq!(cost_micro_dollars(Some(f64::NEG_INFINITY)), None);
    }

    /// `cost_micro_dollars` rejects values that would overflow i64
    /// after the 1e6 multiplication. Pin so a future "saturate at
    /// MAX" refactor breaks this test loudly (i.e. the caller would
    /// see Some(MAX) instead of None — a behaviour change).
    #[test]
    fn cost_micro_dollars_returns_none_on_i64_overflow() {
        // i64::MAX micros ≈ 9.22e12 USD. A USD value 100x larger
        // overflows.
        let huge = (i64::MAX as f64 / 1_000_000.0) * 10.0;
        assert_eq!(cost_micro_dollars(Some(huge)), None);
    }

    /// `UsageContext` clones cleanly (the recorder clones the context
    /// per insert to thread fields into the SQL statement).
    #[test]
    fn usage_context_derives_clone_and_eq() {
        let ctx = UsageContext {
            session_id: Some("s1".to_string()),
            thread_id: Some("t1".to_string()),
            agent_run_id: Some("r1".to_string()),
            run_id: Some("run1".to_string()),
            provider: "openai".to_string(),
            model: "gpt-4".to_string(),
            request_kind: "chat".to_string(),
            intent: Some("fix".to_string()),
            agent_name: Some("coder".to_string()),
        };
        let cloned = ctx.clone();
        assert_eq!(cloned, ctx);
        // Subtle: agent_name=None must be distinguishable from
        // agent_name=Some("") for the per-agent grouping path.
        let mut anon = ctx.clone();
        anon.agent_name = None;
        assert_ne!(anon, ctx);
        let mut empty = ctx.clone();
        empty.agent_name = Some(String::new());
        assert_ne!(empty, ctx);
        assert_ne!(empty.agent_name, anon.agent_name);
    }
}
