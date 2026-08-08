use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement, Value};
use serde::Serialize;

#[derive(Clone)]
pub struct UsageQuery {
    conn: DatabaseConnection,
}

impl UsageQuery {
    pub fn new(conn: DatabaseConnection) -> Self {
        Self { conn }
    }

    pub async fn aggregate_by_model(&self) -> Result<Vec<UsageAggregate>, DbErr> {
        self.aggregate_by_model_filtered(&UsageQueryFilter::default())
            .await
    }

    pub async fn aggregate_by_model_filtered(
        &self,
        filter: &UsageQueryFilter,
    ) -> Result<Vec<UsageAggregate>, DbErr> {
        self.aggregate_filtered(UsageGrouping::ProviderModel, filter)
            .await
    }

    /// Aggregate filtered usage rows at one of the three documented
    /// grains:
    ///
    /// - [`UsageGrouping::ProviderModel`] — original `(provider, model)`
    ///   shape (back-compat with `aggregate_by_model_filtered`).
    /// - [`UsageGrouping::Agent`] — `(agent_name)`-only, the
    ///   `/usage --by=agent` surface (P5.4) consumes this.
    /// - [`UsageGrouping::AgentProviderModel`] — full
    ///   `(agent_name, provider, model)` join, the most informative
    ///   shape for the multi-agent runtime where one agent may exercise
    ///   several models.
    ///
    /// `agent_name` columns surface as `None` in the result when the
    /// row has no recorded agent (legacy single-agent path).
    pub async fn aggregate_filtered(
        &self,
        grouping: UsageGrouping,
        filter: &UsageQueryFilter,
    ) -> Result<Vec<UsageAggregate>, DbErr> {
        let backend = self.conn.get_database_backend();
        let (where_sql, values) = usage_where_clause(filter);
        let group_cols = grouping.group_columns();
        let order_cols = grouping.order_columns();
        let select_prefix = grouping.select_prefix();
        let rows = self
            .conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                format!(
                    "SELECT {select_prefix}, COUNT(*), \
                        COALESCE(SUM(prompt_tokens), 0), \
                        COALESCE(SUM(completion_tokens), 0), \
                        COALESCE(SUM(cached_tokens), 0), \
                        COALESCE(SUM(reasoning_tokens), 0), \
                        COALESCE(SUM(total_tokens), 0), \
                        COALESCE(SUM(tool_call_count), 0), \
                        COALESCE(SUM(wall_clock_ms), 0), \
                        SUM(cost_usd), \
                        SUM(cost_estimate_micro_dollars), \
                        COALESCE(SUM(CASE WHEN success = 0 THEN 1 ELSE 0 END), 0) \
                 FROM agent_usage_stats \
                 {where_sql} \
                 GROUP BY {group_cols} \
                 ORDER BY {order_cols}"
                ),
                values,
            ))
            .await?;

        rows.into_iter()
            .map(|row| decode_aggregate(grouping, row))
            .collect()
    }
}

/// Grain at which the query layer aggregates `agent_usage_stats`. The
/// three variants map 1:1 to the surfaces the TUI `/usage` slash
/// command (P5.4) supports.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UsageGrouping {
    /// `(provider, model)` — pre-OC-Phase-5 shape; preserved for
    /// back-compat callers and tests that pre-date `agent_name`.
    ProviderModel,
    /// `(agent_name)` only.
    Agent,
    /// `(agent_name, provider, model)` — the most informative grain
    /// for a multi-agent session.
    AgentProviderModel,
}

impl UsageGrouping {
    fn select_prefix(self) -> &'static str {
        match self {
            Self::ProviderModel => "NULL, provider, model",
            Self::Agent => "agent_name, NULL, NULL",
            Self::AgentProviderModel => "agent_name, provider, model",
        }
    }

    fn group_columns(self) -> &'static str {
        match self {
            Self::ProviderModel => "provider, model",
            Self::Agent => "agent_name",
            Self::AgentProviderModel => "agent_name, provider, model",
        }
    }

    fn order_columns(self) -> &'static str {
        match self {
            Self::ProviderModel => "provider, model",
            // SQLite's default NULL ordering puts NULL first under
            // ASC; spell the policy explicitly so the result order is
            // stable regardless of the underlying engine's defaults.
            Self::Agent => "agent_name IS NULL, agent_name",
            Self::AgentProviderModel => "agent_name IS NULL, agent_name, provider, model",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UsageQueryFilter {
    pub since: Option<String>,
    pub until: Option<String>,
    pub session_id: Option<String>,
    pub thread_id: Option<String>,
    pub include_failed: bool,
}

impl Default for UsageQueryFilter {
    fn default() -> Self {
        Self {
            since: None,
            until: None,
            session_id: None,
            thread_id: None,
            include_failed: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct UsageAggregate {
    /// Populated only when [`UsageGrouping::Agent`] or
    /// [`UsageGrouping::AgentProviderModel`] was used. `None` for the
    /// `(provider, model)` grain or when the row had no recorded
    /// agent (legacy single-agent path).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// Empty when grouping is [`UsageGrouping::Agent`] (no
    /// per-provider breakdown).
    pub provider: String,
    /// Empty when grouping is [`UsageGrouping::Agent`].
    pub model: String,
    pub request_count: u64,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cached_tokens: u64,
    pub reasoning_tokens: u64,
    pub total_tokens: u64,
    pub tool_call_count: u64,
    pub wall_clock_ms: u64,
    pub cost_usd: Option<f64>,
    pub cost_estimate_micro_dollars: Option<u64>,
    pub failed_count: u64,
}

fn decode_aggregate(
    grouping: UsageGrouping,
    row: sea_orm::QueryResult,
) -> Result<UsageAggregate, DbErr> {
    // Columns 0..=2 are SELECT-prefix slots whose typed shape depends
    // on the grouping (agent_name vs provider vs model). Columns
    // 3..=14 are the constant aggregation tail.
    let agent_name: Option<String> = row.try_get_by_index(0)?;
    let provider: Option<String> = row.try_get_by_index(1)?;
    let model: Option<String> = row.try_get_by_index(2)?;
    let request_count: i64 = row.try_get_by_index(3)?;
    let prompt_tokens: i64 = row.try_get_by_index(4)?;
    let completion_tokens: i64 = row.try_get_by_index(5)?;
    let cached_tokens: i64 = row.try_get_by_index(6)?;
    let reasoning_tokens: i64 = row.try_get_by_index(7)?;
    let total_tokens: i64 = row.try_get_by_index(8)?;
    let tool_call_count: i64 = row.try_get_by_index(9)?;
    let wall_clock_ms: i64 = row.try_get_by_index(10)?;
    let cost_usd: Option<f64> = row.try_get_by_index(11)?;
    let cost_estimate_micro_dollars: Option<i64> = row.try_get_by_index(12)?;
    let failed_count: i64 = row.try_get_by_index(13)?;

    // Reify the SELECT-prefix shape into the documented public form
    // so a caller using `aggregate_by_model_filtered` (the back-compat
    // entry point) does not see a `None` provider / model where the
    // pre-P5.2 contract guaranteed a `String`.
    let (agent_name_out, provider_out, model_out) = match grouping {
        UsageGrouping::ProviderModel => (
            None,
            provider.unwrap_or_default(),
            model.unwrap_or_default(),
        ),
        UsageGrouping::Agent => (agent_name, String::new(), String::new()),
        UsageGrouping::AgentProviderModel => (
            agent_name,
            provider.unwrap_or_default(),
            model.unwrap_or_default(),
        ),
    };

    Ok(UsageAggregate {
        agent_name: agent_name_out,
        provider: provider_out,
        model: model_out,
        request_count: non_negative_u64(request_count),
        prompt_tokens: non_negative_u64(prompt_tokens),
        completion_tokens: non_negative_u64(completion_tokens),
        cached_tokens: non_negative_u64(cached_tokens),
        reasoning_tokens: non_negative_u64(reasoning_tokens),
        total_tokens: non_negative_u64(total_tokens),
        tool_call_count: non_negative_u64(tool_call_count),
        wall_clock_ms: non_negative_u64(wall_clock_ms),
        cost_usd,
        cost_estimate_micro_dollars: cost_estimate_micro_dollars.map(non_negative_u64),
        failed_count: non_negative_u64(failed_count),
    })
}

fn non_negative_u64(value: i64) -> u64 {
    u64::try_from(value.max(0)).unwrap_or(0)
}

fn usage_where_clause(filter: &UsageQueryFilter) -> (String, Vec<Value>) {
    let mut clauses = Vec::new();
    let mut values = Vec::new();

    if let Some(since) = filter.since.as_ref() {
        clauses.push("COALESCE(started_at, created_at) >= ?");
        values.push(since.clone().into());
    }
    if let Some(until) = filter.until.as_ref() {
        clauses.push("COALESCE(started_at, created_at) <= ?");
        values.push(until.clone().into());
    }
    if let Some(session_id) = filter.session_id.as_ref() {
        clauses.push("session_id = ?");
        values.push(session_id.clone().into());
    }
    if let Some(thread_id) = filter.thread_id.as_ref() {
        clauses.push("thread_id = ?");
        values.push(thread_id.clone().into());
    }
    if !filter.include_failed {
        clauses.push("success = 1");
    }

    if clauses.is_empty() {
        (String::new(), values)
    } else {
        (format!("WHERE {}", clauses.join(" AND ")), values)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `UsageGrouping::select_prefix` pins the SQL SELECT projection
    /// for each grain. Pin so the column-index decoder in
    /// `decode_aggregate` (which reads positions 0..=2) stays in
    /// lock-step with what the SELECT actually returns.
    #[test]
    fn usage_grouping_select_prefix_pins_three_grain_shapes() {
        assert_eq!(
            UsageGrouping::ProviderModel.select_prefix(),
            "NULL, provider, model",
        );
        assert_eq!(
            UsageGrouping::Agent.select_prefix(),
            "agent_name, NULL, NULL",
        );
        assert_eq!(
            UsageGrouping::AgentProviderModel.select_prefix(),
            "agent_name, provider, model",
        );
    }

    /// `group_columns` pins the GROUP BY clause for each grain.
    #[test]
    fn usage_grouping_group_columns_match_grain_shape() {
        assert_eq!(
            UsageGrouping::ProviderModel.group_columns(),
            "provider, model"
        );
        assert_eq!(UsageGrouping::Agent.group_columns(), "agent_name");
        assert_eq!(
            UsageGrouping::AgentProviderModel.group_columns(),
            "agent_name, provider, model",
        );
    }

    /// `order_columns` for `Agent` / `AgentProviderModel` must use the
    /// explicit `IS NULL` prefix so NULL agent_name rows sort
    /// deterministically regardless of the backing engine's default
    /// NULL ordering. Pin the explicit-policy form so a future
    /// "let the engine decide" refactor breaks here.
    #[test]
    fn usage_grouping_order_columns_carry_explicit_null_policy() {
        assert_eq!(
            UsageGrouping::ProviderModel.order_columns(),
            "provider, model"
        );
        // Agent grouping must surface NULL-handling explicitly.
        assert!(
            UsageGrouping::Agent.order_columns().contains("IS NULL"),
            "Agent ordering must declare NULL policy explicitly; got: {}",
            UsageGrouping::Agent.order_columns(),
        );
        assert!(
            UsageGrouping::AgentProviderModel
                .order_columns()
                .contains("IS NULL"),
            "AgentProviderModel ordering must declare NULL policy explicitly",
        );
    }

    /// `UsageQueryFilter::default()` enables failed-row inclusion by
    /// default. Pin the canonical "show everything by default" rule —
    /// a flip to `include_failed=false` would silently hide error
    /// rows from rollups.
    #[test]
    fn usage_query_filter_default_includes_failed_rows() {
        let filter = UsageQueryFilter::default();
        assert!(filter.include_failed);
        assert!(filter.since.is_none());
        assert!(filter.until.is_none());
        assert!(filter.session_id.is_none());
        assert!(filter.thread_id.is_none());
    }

    /// `non_negative_u64` clamps negatives to 0, passes non-negatives
    /// through, and handles `i64::MAX` without panic.
    #[test]
    fn non_negative_u64_clamps_negatives() {
        assert_eq!(non_negative_u64(0), 0);
        assert_eq!(non_negative_u64(42), 42);
        assert_eq!(non_negative_u64(i64::MAX), i64::MAX as u64);
        assert_eq!(non_negative_u64(-1), 0);
        assert_eq!(non_negative_u64(i64::MIN), 0);
    }

    /// `usage_where_clause` with the default filter (no time bounds,
    /// no session/thread filter, include_failed=true) produces an
    /// empty SQL fragment and no bound values.
    #[test]
    fn usage_where_clause_default_filter_produces_empty_clause() {
        let filter = UsageQueryFilter::default();
        let (sql, values) = usage_where_clause(&filter);
        assert!(sql.is_empty(), "got SQL: {sql}");
        assert!(values.is_empty(), "got {} values", values.len());
    }

    /// `since` / `until` / `session_id` / `thread_id` filters each
    /// contribute a `?` placeholder and a corresponding bound value.
    /// All combined under `AND` with the `WHERE` prefix.
    #[test]
    fn usage_where_clause_all_filters_combined_with_and() {
        let filter = UsageQueryFilter {
            since: Some("2026-01-01T00:00:00Z".to_string()),
            until: Some("2026-12-31T23:59:59Z".to_string()),
            session_id: Some("s1".to_string()),
            thread_id: Some("t1".to_string()),
            include_failed: false,
        };
        let (sql, values) = usage_where_clause(&filter);
        // Should be 4 bound values; `include_failed=false` adds a
        // clause with no parameter.
        assert_eq!(
            values.len(),
            4,
            "expected 4 bound values; got {}",
            values.len()
        );
        assert!(sql.starts_with("WHERE "));
        // 4 placeholders + 1 literal `success = 1` clause → joined by
        // exactly 4 `" AND "` separators.
        assert_eq!(
            sql.matches(" AND ").count(),
            4,
            "expected 4 AND separators; got: {sql}",
        );
        // Time-range clauses use COALESCE to fall back to created_at.
        assert!(sql.contains("COALESCE(started_at, created_at) >= ?"));
        assert!(sql.contains("COALESCE(started_at, created_at) <= ?"));
        assert!(sql.contains("session_id = ?"));
        assert!(sql.contains("thread_id = ?"));
        assert!(sql.contains("success = 1"));
    }

    /// `include_failed = false` alone (without any other filter)
    /// produces a `WHERE success = 1` clause with no bound values.
    /// Pin the value count so a future refactor that binds the `1`
    /// as a parameter breaks here.
    #[test]
    fn usage_where_clause_include_failed_false_adds_inline_literal_no_value() {
        let filter = UsageQueryFilter {
            include_failed: false,
            ..Default::default()
        };
        let (sql, values) = usage_where_clause(&filter);
        assert_eq!(sql, "WHERE success = 1");
        assert!(values.is_empty(), "success=1 must be inline, not bound");
    }
}
