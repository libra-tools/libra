//! `libra agent status` — read-only summary of captured external-agent state.
//!
//! V1 emits the count of `agent_session` rows by `state`, plus the timestamp
//! of the most recent `agent_checkpoint`. Designed to be safe on a fresh repo
//! (zero rows is a valid response, not an error).

use clap::Args;
#[cfg(feature = "storage-sqlite")]
use sea_orm::{ConnectionTrait, Statement};
use serde::Serialize;

#[cfg(feature = "storage-sqlite")]
use crate::internal::ai::capture::store;
use crate::{
    internal::ai::capture::diagnostics::{self, CaptureDiagnostics},
    storage::StorageBackendKind,
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[derive(Args, Debug)]
pub struct StatusArgs {}

#[derive(Debug, Serialize)]
struct StatusOutput {
    sessions_active: i64,
    sessions_stopped: i64,
    sessions_other: i64,
    last_checkpoint_at: Option<i64>,
    capture_diagnostics: CaptureDiagnostics,
}

pub async fn execute_safe(_args: StatusArgs, output: &OutputConfig) -> CliResult<()> {
    let storage = util::try_get_storage_path(None).map_err(|_| CliError::repo_not_found())?;
    let capture_diagnostics = diagnostics::summarize(&storage);

    match crate::storage::runtime::detect_repo_backend(&storage).map_err(CliError::from)? {
        StorageBackendKind::Redb => {
            #[cfg(feature = "storage-redb")]
            {
                use crate::storage::capture_store::CaptureSessionState;

                let capture = crate::storage::runtime::open_capture_store_read_only(&storage)
                    .await
                    .map_err(CliError::from)?;
                let sessions = capture
                    .list_sessions_by_started_desc()
                    .await
                    .map_err(CliError::from)?;

                let mut active: i64 = 0;
                let mut stopped: i64 = 0;
                let mut other: i64 = 0;
                let mut last: Option<i64> = None;

                for session in &sessions {
                    match session.state {
                        CaptureSessionState::Active => active += 1,
                        CaptureSessionState::Stopped => stopped += 1,
                        CaptureSessionState::Pending
                        | CaptureSessionState::Condensed
                        | CaptureSessionState::Quarantined => other += 1,
                    }
                    let checkpoints = capture
                        .list_checkpoints_by_session(&session.session_id)
                        .await
                        .map_err(CliError::from)?;
                    for cp in checkpoints {
                        last = Some(last.map_or(cp.created_at, |cur| cur.max(cp.created_at)));
                    }
                }

                emit(
                    &StatusOutput {
                        sessions_active: active,
                        sessions_stopped: stopped,
                        sessions_other: other,
                        last_checkpoint_at: last,
                        capture_diagnostics,
                    },
                    output,
                )
            }
            #[cfg(not(feature = "storage-redb"))]
            {
                let _ = (capture_diagnostics, output);
                Err(
                    crate::storage::error::StorageBackendError::UnsupportedBackend {
                        requested: "redb".to_string(),
                        detail: "this Libra binary was compiled without `storage-redb`".to_string(),
                    }
                    .into_cli_error(),
                )
            }
        }
        StorageBackendKind::Sqlite => {
            #[cfg(feature = "storage-sqlite")]
            {
                let conn = store::open_current().await.map_err(|error| {
                    CliError::fatal(format!("failed to open the capture database: {error}"))
                })?;
                let backend = conn.get_database_backend();

                // The migration that creates these tables ships in `2026050303_agent_capture`.
                // Older databases may not have run it yet (e.g. a `libra init` from before
                // this version); in that case sqlite_master returns nothing and we report
                // an empty status rather than failing.
                if !table_exists(&conn, "agent_session").await? {
                    return emit(
                        &StatusOutput {
                            sessions_active: 0,
                            sessions_stopped: 0,
                            sessions_other: 0,
                            last_checkpoint_at: None,
                            capture_diagnostics,
                        },
                        output,
                    );
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
                        capture_diagnostics,
                    },
                    output,
                )
            }
            #[cfg(not(feature = "storage-sqlite"))]
            {
                let _ = (capture_diagnostics, output);
                Err(
                    crate::storage::error::StorageBackendError::UnsupportedBackend {
                        requested: "sqlite".to_string(),
                        detail: "this Libra binary was compiled without `storage-sqlite`"
                            .to_string(),
                    }
                    .into_cli_error(),
                )
            }
        }
    }
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
    println!(
        "Codex diagnostics: {}",
        payload.capture_diagnostics.describe()
    );
    Ok(())
}

#[cfg(feature = "storage-sqlite")]
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

#[cfg(feature = "storage-sqlite")]
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
