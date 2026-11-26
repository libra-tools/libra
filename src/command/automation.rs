//! `libra automation` command surface for CEX-15.

use std::path::PathBuf;

use chrono::{DateTime, Utc};
use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    info_println,
    internal::{
        ai::automation::{
            AutomationConfig, AutomationExecutor, AutomationHistory, AutomationRunResult,
            AutomationScheduler,
        },
        db::get_db_conn_instance_for_path,
    },
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
        util::{DATABASE, try_get_storage_path},
    },
};

/// `--help` examples shown in `libra automation --help` output.
///
/// `automation` exposes three sub-commands (`list` / `run` / `history`)
/// for inspecting and exercising the rule-based automation runtime. The
/// banner pins the canonical invocation per sub-command plus a named
/// rule run, a simulated-time run, the `--live` opt-in for actually
/// spawning shell actions, and JSON variants for agents so users can
/// map intent to invocation without reading the design doc.
/// Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const AUTOMATION_EXAMPLES: &str = "\
EXAMPLES:
    libra automation list                                List configured automation rules
    libra automation run                                 Run all due cron rules in dry-run mode
    libra automation run --rule my-rule                  Run a single rule regardless of cron schedule
    libra automation run --now 2026-05-23T12:00:00Z      Simulate the current time when evaluating cron triggers
    libra automation run --live                          Actually spawn shell actions that pass safety preflight
    libra automation history --limit 50                  Show the 50 most recent automation history rows
    libra automation --json list                         Structured JSON output for agents
    libra automation --json run                          Structured JSON output for a run (includes per-rule results[])";

#[derive(Parser, Debug)]
#[command(after_help = AUTOMATION_EXAMPLES)]
pub struct AutomationArgs {
    #[command(subcommand)]
    pub command: AutomationSubcommand,
}

#[derive(Subcommand, Debug)]
pub enum AutomationSubcommand {
    /// List configured automation rules.
    List,
    /// Run due cron rules, or one named rule.
    Run {
        /// Run one rule by name regardless of whether its cron trigger is due
        #[arg(long, value_name = "NAME")]
        rule: Option<String>,
        /// Simulated current time as RFC3339 (default: actual now)
        #[arg(long, value_name = "DATE")]
        now: Option<String>,
        /// Actually spawn shell actions that pass safety preflight
        #[arg(long)]
        live: bool,
    },
    /// Show recent automation history rows.
    History {
        /// Maximum number of history rows to return (default: 20)
        #[arg(long, value_name = "N", default_value_t = 20)]
        limit: u64,
    },
}

#[derive(Serialize)]
struct AutomationListOutput<'a> {
    rules: &'a [crate::internal::ai::automation::AutomationRule],
}

#[derive(Serialize)]
struct AutomationRunOutput {
    results: Vec<AutomationRunResult>,
}

pub async fn execute_safe(args: AutomationArgs, output: &OutputConfig) -> CliResult<()> {
    // W4-08: linked worktrees load automations.toml through the W4-06/W4-12
    // resolver (repository layer visible; overlay-wins on the same name).
    // Unregistered/damaged scope still fail-closes.
    let cwd = std::env::current_dir()
        .map_err(|error| CliError::io(format!("failed to resolve current directory: {error}")))?;
    crate::command::require_registered_worktree_scope("libra automation", &cwd)?;
    match args.command {
        AutomationSubcommand::List => list_rules(output).await,
        AutomationSubcommand::Run { rule, now, live } => run_rules(rule, now, live, output).await,
        AutomationSubcommand::History { limit } => list_history(limit, output).await,
    }
}

async fn list_rules(output: &OutputConfig) -> CliResult<()> {
    let config = load_config()?;
    config
        .validate()
        .map_err(|error| CliError::failure(error.to_string()))?;
    if output.is_json() {
        return emit_json_data(
            "automation.list",
            &AutomationListOutput {
                rules: &config.rules,
            },
            output,
        );
    }

    if config.rules.is_empty() {
        info_println!(output, "No automation rules configured.");
        return Ok(());
    }
    for rule in &config.rules {
        info_println!(
            output,
            "{}\t{}\t{}",
            rule.id,
            rule.trigger.kind(),
            rule.action.kind()
        );
    }
    Ok(())
}

async fn run_rules(
    rule_id: Option<String>,
    now: Option<String>,
    live: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let config = load_config()?;
    config
        .validate()
        .map_err(|error| CliError::failure(error.to_string()))?;
    let now = parse_now(now)?;
    let executor = if live {
        AutomationExecutor::live(current_working_dir()?)
    } else {
        AutomationExecutor::dry_run()
    };
    let results = if let Some(rule_id) = rule_id {
        let rule = config
            .rules
            .iter()
            .find(|rule| rule.id == rule_id)
            .ok_or_else(|| {
                CliError::failure(format!("automation rule `{rule_id}` was not found"))
            })?;
        vec![executor.execute_rule(rule, rule.trigger.clone()).await]
    } else {
        AutomationScheduler::new(config)
            .run_due_at(now, &executor)
            .await
            .map_err(|error| CliError::failure(error.to_string()))?
    };

    match crate::storage::runtime::detect_current_repo_backend() {
        Ok(crate::storage::StorageBackendKind::Redb) => {
            let state_store = crate::storage::runtime::open_current_git_state_store_read_write()
                .await
                .map_err(|error| CliError::failure(error.to_string()))?;
            for result in &results {
                let record = crate::storage::git_state_store::AutomationLogRecord {
                    id: result.id.clone(),
                    rule_id: result.rule_id.clone(),
                    trigger_kind: result.trigger_kind.clone(),
                    action_kind: result.action_kind.clone(),
                    status: result.status.as_str().to_string(),
                    message: result.message.clone(),
                    started_at: result.started_at.to_rfc3339(),
                    finished_at: result.finished_at.to_rfc3339(),
                    details_json: serde_json::to_string(&result.details)
                        .unwrap_or_else(|_| "{}".to_string()),
                };
                state_store
                    .append_automation_log(&record)
                    .await
                    .map_err(|error| CliError::failure(error.to_string()))?;
            }
        }
        Ok(crate::storage::StorageBackendKind::Sqlite) | Err(_) => {
            let db = open_repo_db().await?;
            for result in &results {
                AutomationHistory::append(&db, result)
                    .await
                    .map_err(|error| CliError::failure(error.to_string()))?;
            }
        }
    }

    if output.is_json() {
        return emit_json_data("automation.run", &AutomationRunOutput { results }, output);
    }
    for result in &results {
        info_println!(
            output,
            "{}\t{}\t{}",
            result.rule_id,
            result.status.as_str(),
            result.message
        );
    }
    Ok(())
}

async fn list_history(limit: u64, output: &OutputConfig) -> CliResult<()> {
    let rows = match crate::storage::runtime::detect_current_repo_backend() {
        Ok(crate::storage::StorageBackendKind::Redb) => {
            let state_store = crate::storage::runtime::open_current_git_state_store_read_only()
                .await
                .map_err(|error| CliError::failure(error.to_string()))?;
            let cap = usize::try_from(limit).unwrap_or(usize::MAX);
            let logs = state_store
                .list_automation_logs(None, Some(cap))
                .await
                .map_err(|error| CliError::failure(error.to_string()))?;
            logs.into_iter()
                .map(|row| {
                    let status = match row.status.as_str() {
                        "succeeded" => {
                            crate::internal::ai::automation::AutomationRunStatus::Succeeded
                        }
                        "failed" => crate::internal::ai::automation::AutomationRunStatus::Failed,
                        "approval_required" => {
                            crate::internal::ai::automation::AutomationRunStatus::ApprovalRequired
                        }
                        _ => crate::internal::ai::automation::AutomationRunStatus::Skipped,
                    };
                    let started_at = DateTime::parse_from_rfc3339(&row.started_at)
                        .map(|v| v.with_timezone(&Utc))
                        .unwrap_or_else(|_| Utc::now());
                    let finished_at = DateTime::parse_from_rfc3339(&row.finished_at)
                        .map(|v| v.with_timezone(&Utc))
                        .unwrap_or(started_at);
                    let details = serde_json::from_str(&row.details_json)
                        .unwrap_or_else(|_| serde_json::json!({}));
                    AutomationRunResult {
                        id: row.id,
                        rule_id: row.rule_id,
                        trigger_kind: row.trigger_kind,
                        action_kind: row.action_kind,
                        status,
                        message: row.message,
                        details,
                        started_at,
                        finished_at,
                    }
                })
                .collect()
        }
        Ok(crate::storage::StorageBackendKind::Sqlite) | Err(_) => {
            let db = open_repo_db().await?;
            AutomationHistory::list_recent(&db, limit)
                .await
                .map_err(|error| CliError::failure(error.to_string()))?
        }
    };
    if output.is_json() {
        return emit_json_data(
            "automation.history",
            &AutomationRunOutput { results: rows },
            output,
        );
    }
    if rows.is_empty() {
        info_println!(output, "No automation history.");
        return Ok(());
    }
    for row in &rows {
        info_println!(
            output,
            "{}\t{}\t{}\t{}",
            row.finished_at.to_rfc3339(),
            row.rule_id,
            row.status.as_str(),
            row.message
        );
    }
    Ok(())
}

fn load_config() -> CliResult<AutomationConfig> {
    let working_dir = current_working_dir()?;
    AutomationConfig::load_from_working_dir(&working_dir)
        .map_err(|error| CliError::failure(error.to_string()))
}

fn current_working_dir() -> CliResult<PathBuf> {
    std::env::current_dir()
        .map_err(|error| CliError::io(format!("failed to resolve current directory: {error}")))
}

fn parse_now(now: Option<String>) -> CliResult<DateTime<Utc>> {
    match now {
        Some(raw) => DateTime::parse_from_rfc3339(&raw)
            .map(|value| value.with_timezone(&Utc))
            .map_err(|error| CliError::command_usage(format!("invalid --now timestamp: {error}"))),
        None => Ok(Utc::now()),
    }
}

async fn open_repo_db() -> CliResult<sea_orm::DatabaseConnection> {
    let db_path = try_get_storage_path(None)
        .map(|storage| storage.join(DATABASE))
        .map_err(|error| {
            CliError::repo_not_found()
                .with_hint(format!("failed to resolve repository storage: {error}"))
        })?;
    get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| {
            CliError::failure(format!(
                "failed to open repository database {}: {error}",
                db_path.display()
            ))
        })
}
