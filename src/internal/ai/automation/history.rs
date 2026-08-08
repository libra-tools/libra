use sea_orm::{ConnectionTrait, DatabaseConnection, DbErr, Statement};

use crate::internal::ai::automation::events::{
    AutomationError, AutomationRunResult, AutomationRunStatus,
};

pub struct AutomationHistory;

impl AutomationHistory {
    pub async fn append(
        conn: &DatabaseConnection,
        result: &AutomationRunResult,
    ) -> Result<(), AutomationError> {
        let backend = conn.get_database_backend();
        conn.execute_raw(Statement::from_sql_and_values(
            backend,
            "INSERT INTO automation_log \
             (id, rule_id, trigger_kind, action_kind, status, message, started_at, finished_at, details_json) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            [
                result.id.clone().into(),
                result.rule_id.clone().into(),
                result.trigger_kind.clone().into(),
                result.action_kind.clone().into(),
                result.status.as_str().into(),
                result.message.clone().into(),
                result.started_at.to_rfc3339().into(),
                result.finished_at.to_rfc3339().into(),
                result.details.to_string().into(),
            ],
        ))
        .await
        .map_err(database_error)?;
        Ok(())
    }

    pub async fn list_recent(
        conn: &DatabaseConnection,
        limit: u64,
    ) -> Result<Vec<AutomationRunResult>, AutomationError> {
        let backend = conn.get_database_backend();
        let rows = conn
            .query_all_raw(Statement::from_sql_and_values(
                backend,
                "SELECT id, rule_id, trigger_kind, action_kind, status, message, started_at, finished_at, details_json \
                 FROM automation_log ORDER BY finished_at DESC LIMIT ?",
                [i64::try_from(limit).unwrap_or(i64::MAX).into()],
            ))
            .await
            .map_err(database_error)?;

        rows.into_iter().map(decode_row).collect()
    }
}

fn decode_row(row: sea_orm::QueryResult) -> Result<AutomationRunResult, AutomationError> {
    let id: String = row.try_get_by_index(0).map_err(database_error)?;
    let rule_id: String = row.try_get_by_index(1).map_err(database_error)?;
    let trigger_kind: String = row.try_get_by_index(2).map_err(database_error)?;
    let action_kind: String = row.try_get_by_index(3).map_err(database_error)?;
    let status_raw: String = row.try_get_by_index(4).map_err(database_error)?;
    let message: String = row.try_get_by_index(5).map_err(database_error)?;
    let started_at_raw: String = row.try_get_by_index(6).map_err(database_error)?;
    let finished_at_raw: String = row.try_get_by_index(7).map_err(database_error)?;
    let details_raw: String = row.try_get_by_index(8).map_err(database_error)?;

    let started_at = chrono::DateTime::parse_from_rfc3339(&started_at_raw)
        .map_err(|error| AutomationError::Database(format!("invalid started_at: {error}")))?
        .with_timezone(&chrono::Utc);
    let finished_at = chrono::DateTime::parse_from_rfc3339(&finished_at_raw)
        .map_err(|error| AutomationError::Database(format!("invalid finished_at: {error}")))?
        .with_timezone(&chrono::Utc);
    let details = serde_json::from_str(&details_raw)
        .map_err(|error| AutomationError::Database(format!("invalid details_json: {error}")))?;

    Ok(AutomationRunResult {
        id,
        rule_id,
        trigger_kind,
        action_kind,
        status: AutomationRunStatus::parse(&status_raw)?,
        message,
        details,
        started_at,
        finished_at,
    })
}

fn database_error(error: impl Into<DbErr>) -> AutomationError {
    AutomationError::Database(error.into().to_string())
}
