//! `libra agent status` — read-only summary of captured external-agent state.
//!
//! V1 emits the count of `agent_session` rows by `state`, plus the timestamp
//! of the most recent `agent_checkpoint`. Designed to be safe on a fresh repo
//! (zero rows is a valid response, not an error).

use clap::Args;
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

use crate::{
    internal::db::get_db_conn_instance,
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
    },
};

#[derive(Args, Debug)]
pub struct StatusArgs {}

#[derive(Debug, Default, Serialize)]
struct StatusOutput {
    sessions_active: i64,
    sessions_stopped: i64,
    sessions_other: i64,
    last_checkpoint_at: Option<i64>,
}

pub async fn execute_safe(_args: StatusArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    let backend = conn.get_database_backend();

    // The migration that creates these tables ships in `2026050303_agent_capture`.
    // Older databases may not have run it yet (e.g. a `libra init` from before
    // this version); in that case sqlite_master returns nothing and we report
    // an empty status rather than failing.
    if !table_exists(&conn, "agent_session").await? {
        return emit(&StatusOutput::default(), output);
    }

    let active = scalar_count(
        &conn,
        backend,
        "SELECT COUNT(*) AS n FROM agent_session WHERE state = 'active'",
    )
    .await?;
    let stopped = scalar_count(
        &conn,
        backend,
        "SELECT COUNT(*) AS n FROM agent_session WHERE state = 'stopped'",
    )
    .await?;
    let other = scalar_count(
        &conn,
        backend,
        "SELECT COUNT(*) AS n FROM agent_session WHERE state NOT IN ('active','stopped')",
    )
    .await?;

    let last = if table_exists(&conn, "agent_checkpoint").await? {
        let stmt = Statement::from_sql_and_values(
            backend,
            "SELECT MAX(created_at) AS last FROM agent_checkpoint",
            [],
        );
        match conn.query_one_raw(stmt).await {
            Ok(Some(row)) => row.try_get_by::<Option<i64>, _>("last").ok().flatten(),
            Ok(None) => None,
            Err(e) => {
                return Err(CliError::fatal(format!(
                    "failed to read agent_checkpoint summary: {e}"
                )));
            }
        }
    } else {
        None
    };

    emit(
        &StatusOutput {
            sessions_active: active,
            sessions_stopped: stopped,
            sessions_other: other,
            last_checkpoint_at: last,
        },
        output,
    )
}

fn emit(payload: &StatusOutput, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("agent_status", payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("Active sessions:   {}", payload.sessions_active);
    println!("Stopped sessions:  {}", payload.sessions_stopped);
    println!("Other states:      {}", payload.sessions_other);
    match payload.last_checkpoint_at {
        Some(ts) => println!("Last checkpoint:   {ts} (unix epoch seconds)"),
        None => println!("Last checkpoint:   (none)"),
    }
    Ok(())
}

async fn table_exists(conn: &(impl ConnectionTrait + ?Sized), name: &str) -> CliResult<bool> {
    let backend = conn.get_database_backend();
    let stmt = Statement::from_sql_and_values(
        backend,
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ? LIMIT 1",
        [name.into()],
    );
    conn.query_one_raw(stmt)
        .await
        .map(|row| row.is_some())
        .map_err(|e| CliError::fatal(format!("failed to query sqlite_master: {e}")))
}

async fn scalar_count(
    conn: &(impl ConnectionTrait + ?Sized),
    backend: sea_orm::DatabaseBackend,
    sql: &str,
) -> CliResult<i64> {
    let stmt = Statement::from_sql_and_values(backend, sql, []);
    let row = conn
        .query_one_raw(stmt)
        .await
        .map_err(|e| CliError::fatal(format!("failed to read agent_session count: {e}")))?
        .ok_or_else(|| {
            CliError::fatal("expected one row from COUNT(*) but got none".to_string())
        })?;
    row.try_get_by::<i64, _>("n")
        .map_err(|e| CliError::fatal(format!("failed to decode COUNT(*) row: {e}")))
}
