//! Operation (op) command group for viewing and restoring command-level operation history.

use std::collections::HashSet;

use clap::{Parser, Subcommand};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::Serialize;

use crate::{
    command::status,
    internal::{
        config::ConfigKv,
        db::get_db_conn_instance,
        operation::{
            DoctorEngine, DoctorReport, OperationKind, OperationStoreV2, ReconcileEngine,
            ReconcileError, ReconcileOutcome, RestoreEngine, RestoreError, RestoreReceipt,
            RestoreWhat, UndoEngine, UndoError,
        },
        worktree_scope::RequestScope,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

#[derive(Parser, Debug)]
#[command(about = "View and restore command-level operation history")]
/// Parsed arguments for the `libra op` command group.
pub struct OpArgs {
    /// Selected `libra op` subcommand.
    #[command(subcommand)]
    pub command: OpCommand,
}

#[derive(Subcommand, Debug)]
/// Supported `libra op` subcommands.
pub enum OpCommand {
    /// List operation history with pagination
    Log {
        /// Number of operations to show (default: 50)
        #[clap(short = 'n', long)]
        number: Option<u64>,

        /// Page number for pagination (default: 1)
        #[clap(long)]
        page: Option<u64>,

        /// Filter by command name (e.g., commit, merge)
        #[clap(long)]
        command: Option<String>,

        /// Show detailed metadata
        #[clap(long)]
        verbose: bool,
    },

    /// Show detailed operation information
    Show {
        /// Operation ID or index (e.g., @{0} for latest)
        #[arg(help = "Operation ID (UUID) or index like @{0}, @{1}")]
        op_ref: String,

        /// Show view snapshot details
        #[clap(long)]
        view: bool,
    },

    /// Restore repository to a previous operation's view state
    Restore {
        /// Operation ID or index to restore to
        #[arg(help = "Operation ID (UUID) or index like @{0}, @{1}")]
        op_ref: String,

        /// Force restoration even with uncommitted changes
        #[clap(long)]
        force: bool,

        /// Only show what would be done
        #[clap(long)]
        dry_run: bool,

        /// Facet selection for an operation-log v2 restore.
        #[clap(long, value_enum, default_value_t = RestoreWhat::All)]
        what: RestoreWhat,

        /// Explicitly acknowledge a repository-wide/multi-worktree target.
        #[clap(long)]
        confirm_repo_wide: bool,
    },

    /// Append an operation that moves the current state back to a prior operation's parent.
    Undo {
        op_ref: String,
        #[clap(long)]
        force: bool,
        #[clap(long)]
        dry_run: bool,
        #[clap(long)]
        confirm_repo_wide: bool,
    },

    /// Re-apply the operation that was undone by the selected undo operation.
    Redo {
        op_ref: String,
        #[clap(long)]
        force: bool,
        #[clap(long)]
        dry_run: bool,
        #[clap(long)]
        confirm_repo_wide: bool,
    },

    /// Apply the inverse of an operation relative to an explicit parent.
    Revert {
        op_ref: String,
        #[arg(long)]
        parent: String,
        #[clap(long)]
        force: bool,
        #[clap(long)]
        dry_run: bool,
        #[clap(long)]
        confirm_repo_wide: bool,
    },

    /// Converge concurrent operation heads when their states are provably unambiguous.
    Reconcile {
        /// Only report what would happen
        #[clap(long)]
        dry_run: bool,
    },

    /// Diagnose operation state; repair is opt-in with --fix.
    Doctor {
        #[clap(long)]
        fix: bool,
        #[clap(long)]
        dry_run: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "action")]
/// Structured output payload emitted by `libra op`.
pub enum OpOutput {
    #[serde(rename = "log")]
    Log {
        /// Operation entries returned for the requested page.
        operations: Vec<OpLogEntry>,
        /// 1-based page number after normalization.
        page: u64,
        /// Effective page size after normalization.
        per_page: u64,
        /// Total number of matching operations.
        total: u64,
    },
    #[serde(rename = "show")]
    Show {
        /// Resolved operation identifier.
        op_id: String,
        /// Command name recorded for the operation.
        command_name: String,
        /// Human-readable operation description.
        description: String,
        /// Actor recorded on the operation.
        actor: String,
        /// Stable text label for the operation status.
        status: String,
        /// Operation start timestamp in unix seconds.
        start_ts: i64,
        /// Operation end timestamp in unix seconds, when present.
        end_ts: Option<i64>,
        /// View identifier associated with the operation.
        view_id: String,
    },
    #[serde(rename = "restore")]
    Restore {
        /// Operation id that the restore targeted.
        target_op_id: String,
        /// Newly recorded `op restore` operation id.
        new_op_id: String,
        /// Human-readable restore confirmation.
        message: String,
    },
    #[serde(rename = "restore_v2")]
    RestoreV2 { receipt: RestoreReceipt },
    #[serde(rename = "undo")]
    Undo { receipt: RestoreReceipt },
    #[serde(rename = "redo")]
    Redo { receipt: RestoreReceipt },
    #[serde(rename = "revert")]
    Revert { receipt: RestoreReceipt },
    #[serde(rename = "reconcile")]
    Reconcile { outcome: ReconcileOutcome },
    #[serde(rename = "doctor")]
    Doctor { report: DoctorReport },
}

#[derive(Debug, Clone, Serialize)]
/// One entry rendered by `op log`.
pub struct OpLogEntry {
    /// Zero-based index in the complete, unfiltered operation history.
    pub index: usize,
    /// Operation identifier.
    pub op_id: String,
    /// Recorded command name.
    pub command_name: String,
    /// Human-readable operation description.
    pub description: String,
    /// Actor recorded for the operation.
    pub actor: String,
    /// Stable text label for the operation status.
    pub status: String,
    /// Completion timestamp in unix seconds, if the operation finished.
    pub end_ts: Option<i64>,
}

#[derive(Clone, Debug)]
struct OperationHistoryEntry {
    index: usize,
    op_id: String,
    command_name: String,
    description: String,
    actor: String,
    status: String,
    start_ts: i64,
    end_ts: Option<i64>,
    view_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct OperationQueryPage {
    page: u64,
    per_page: u64,
}

impl OperationQueryPage {
    const DEFAULT_PER_PAGE: u64 = 50;
    const MAX_PER_PAGE: u64 = 200;

    fn normalized(self) -> Self {
        let per_page = if self.per_page == 0 {
            Self::DEFAULT_PER_PAGE
        } else {
            self.per_page.clamp(1, Self::MAX_PER_PAGE)
        };
        Self {
            page: self.page.max(1),
            per_page,
        }
    }

    fn offset(self) -> u64 {
        let normalized = self.normalized();
        (normalized.page - 1).saturating_mul(normalized.per_page)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OperationPage<T> {
    items: Vec<T>,
    page: u64,
    per_page: u64,
    total: u64,
}

/// Execute `libra op` using default CLI output settings.
pub async fn execute(args: OpArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Execute `libra op` and emit results through the caller-provided output mode.
pub async fn execute_safe(args: OpArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    match args.command {
        OpCommand::Log {
            number,
            page,
            command,
            verbose,
        } => handle_op_log(number, page, command, verbose, output).await,
        OpCommand::Show { op_ref, view } => handle_op_show(op_ref, view, output).await,
        OpCommand::Restore {
            op_ref,
            force,
            dry_run,
            what,
            confirm_repo_wide,
        } => handle_op_restore(op_ref, force, dry_run, what, confirm_repo_wide, output).await,
        OpCommand::Undo {
            op_ref,
            force,
            dry_run,
            confirm_repo_wide,
        } => handle_op_undo(op_ref, force, dry_run, confirm_repo_wide, output).await,
        OpCommand::Redo {
            op_ref,
            force,
            dry_run,
            confirm_repo_wide,
        } => handle_op_redo(op_ref, force, dry_run, confirm_repo_wide, output).await,
        OpCommand::Revert {
            op_ref,
            parent,
            force,
            dry_run,
            confirm_repo_wide,
        } => handle_op_revert(op_ref, parent, force, dry_run, confirm_repo_wide, output).await,
        OpCommand::Reconcile { dry_run } => handle_op_reconcile(dry_run, output).await,
        OpCommand::Doctor { fix, dry_run } => handle_op_doctor(fix, dry_run, output).await,
    }
}

async fn v2_engine_for_repo(repo_id: &str) -> CliResult<RestoreEngine> {
    let Some(scope) = RequestScope::try_resolve(util::cur_dir())
        .map_err(|error| CliError::fatal(format!("failed to resolve repository scope: {error}")))?
    else {
        return Err(CliError::fatal(
            "v2 operation state is unavailable in this repository",
        ));
    };
    let storage = ClientStorage::init_local(scope.storage.join("objects"));
    let db = get_db_conn_instance().await;
    Ok(RestoreEngine::new(scope, repo_id, db, storage))
}

async fn v2_engine_and_ref(repo_id: &str, op_ref: &str) -> CliResult<(RestoreEngine, String)> {
    let engine = v2_engine_for_repo(repo_id).await?;
    let entry = resolve_v2_history_ref(engine.store().db(), repo_id, op_ref).await?;
    Ok((engine, entry.op_id))
}

async fn handle_op_undo(
    op_ref: String,
    force: bool,
    dry_run: bool,
    confirm_repo_wide: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    ensure_transition_clean(force).await?;
    let repo_id = current_repo_id().await?;
    let (restore, op_id) = v2_engine_and_ref(&repo_id, &op_ref).await?;
    let receipt = UndoEngine::new(restore)
        .undo(op_id, dry_run, confirm_repo_wide)
        .await
        .map_err(undo_cli_error)?;
    emit_transition_output("undo", receipt, output)
}

async fn handle_op_redo(
    op_ref: String,
    force: bool,
    dry_run: bool,
    confirm_repo_wide: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    ensure_transition_clean(force).await?;
    let repo_id = current_repo_id().await?;
    let (restore, op_id) = v2_engine_and_ref(&repo_id, &op_ref).await?;
    let receipt = UndoEngine::new(restore)
        .redo(op_id, dry_run, confirm_repo_wide)
        .await
        .map_err(undo_cli_error)?;
    emit_transition_output("redo", receipt, output)
}

async fn handle_op_revert(
    op_ref: String,
    parent: String,
    force: bool,
    dry_run: bool,
    confirm_repo_wide: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    ensure_transition_clean(force).await?;
    let repo_id = current_repo_id().await?;
    let (restore, op_id) = v2_engine_and_ref(&repo_id, &op_ref).await?;
    let parent_id = resolve_v2_history_ref(restore.store().db(), &repo_id, &parent)
        .await?
        .op_id;
    let receipt = UndoEngine::new(restore)
        .revert(op_id, parent_id, dry_run, confirm_repo_wide)
        .await
        .map_err(undo_cli_error)?;
    emit_transition_output("revert", receipt, output)
}

async fn handle_op_reconcile(dry_run: bool, output: &OutputConfig) -> CliResult<()> {
    let repo_id = current_repo_id().await?;
    let scope = RequestScope::try_resolve(util::cur_dir())
        .map_err(|error| CliError::fatal(format!("failed to resolve repository scope: {error}")))?
        .ok_or_else(|| CliError::fatal("v2 operation state is unavailable in this repository"))?;
    let storage = ClientStorage::init_local(scope.storage.join("objects"));
    let database = get_db_conn_instance().await;
    let store = OperationStoreV2::new_for_repo(&repo_id, database, storage);
    let engine = ReconcileEngine::new(scope, &repo_id, store);
    let outcome = engine
        .reconcile(dry_run)
        .await
        .map_err(|error| match &error {
            ReconcileError::Cas(message) => CliError::fatal(format!(
                "reconcile failed because the head set changed concurrently: {message}"
            ))
            .with_hint("re-run 'libra op reconcile' to converge the new head set"),
            other => CliError::fatal(other.to_string()),
        })?;
    let payload = OpOutput::Reconcile { outcome };
    let conflicted = matches!(
        &payload,
        OpOutput::Reconcile {
            outcome: ReconcileOutcome::Conflicted { .. }
        }
    );
    if output.is_json() {
        emit_json_data("op", &payload, output)?;
    } else if !output.quiet {
        let OpOutput::Reconcile { outcome } = &payload else {
            return Err(CliError::fatal(
                "internal error: reconcile produced an unexpected output payload",
            ));
        };
        match outcome {
            ReconcileOutcome::NothingToReconcile => {
                println!("Nothing to reconcile: the operation log has a single head.");
            }
            ReconcileOutcome::DryRunConverged { parents } => {
                println!(
                    "Would converge {} concurrent operation heads:\n  {}",
                    parents.len(),
                    parents.join("\n  ")
                );
            }
            ReconcileOutcome::Converged {
                reconcile_op_id,
                parents,
                generation,
            } => {
                println!(
                    "Converged {} concurrent operation heads into reconcile operation {reconcile_op_id} (generation {generation}).",
                    parents.len()
                );
            }
            ReconcileOutcome::Conflicted { conflicts } => {
                eprintln!(
                    "Cannot reconcile: concurrent heads disagree on {} reference(s). The head set is preserved; resolve the conflicts and retry.",
                    conflicts.len()
                );
                for conflict in conflicts {
                    eprintln!("  {} {}:", conflict.kind, conflict.name);
                    for (head, target) in &conflict.targets {
                        eprintln!("    {head} -> {target}");
                    }
                }
            }
        }
    }
    if conflicted {
        return Err(CliError::fatal(
            "reconcile conflicts must be resolved before the head set can converge",
        )
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint(
            "inspect the conflict targets above, resolve the reference disagreement, then retry",
        ));
    }
    Ok(())
}

async fn handle_op_doctor(fix: bool, dry_run: bool, output: &OutputConfig) -> CliResult<()> {
    let repo_id = current_repo_id().await?;
    let restore = v2_engine_for_repo(&repo_id).await?;
    let report = DoctorEngine::new(
        RequestScope::try_resolve(util::cur_dir())
            .map_err(|error| {
                CliError::fatal(format!("failed to resolve repository scope: {error}"))
            })?
            .ok_or_else(|| {
                CliError::fatal("v2 operation state is unavailable in this repository")
            })?,
        repo_id,
        restore.store().clone(),
    )
    .inspect(dry_run, fix)
    .await
    .map_err(|error| CliError::fatal(error.to_string()))?;
    if output.is_json() {
        return emit_json_data("op", &OpOutput::Doctor { report }, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("Operation doctor: {} issue(s)", report.issues.len());
    for issue in &report.issues {
        println!("{}: {}", issue.code, issue.message);
    }
    for fixed in &report.fixed {
        println!("fixed: {fixed}");
    }
    Ok(())
}

fn emit_transition_output(
    action: &str,
    receipt: RestoreReceipt,
    output: &OutputConfig,
) -> CliResult<()> {
    let print = |receipt: &RestoreReceipt| {
        println!(
            "{} {} facet(s), {} path(s)",
            action,
            receipt.restored_facets.len(),
            receipt.changed_paths
        );
        if let Some(op_id) = &receipt.new_op_id {
            println!("New operation recorded: {}", &op_id[..8.min(op_id.len())]);
        } else {
            println!("Dry run: no operation was published.");
        }
    };
    if output.is_json() {
        let payload = match action {
            "undo" => OpOutput::Undo { receipt },
            "redo" => OpOutput::Redo { receipt },
            "revert" => OpOutput::Revert { receipt },
            _ => return Err(CliError::fatal("unknown operation transition")),
        };
        return emit_json_data("op", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    print(&receipt);
    Ok(())
}

fn undo_cli_error(error: UndoError) -> CliError {
    match error {
        UndoError::NotUndo(_) => {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::CliInvalidTarget)
        }
        UndoError::Restore(RestoreError::HeadConfirmationRequired)
        | UndoError::Restore(RestoreError::WrongWorkspace(_))
        | UndoError::Restore(RestoreError::WrongScope { .. })
        | UndoError::Restore(RestoreError::Cas(_)) => CliError::fatal(error.to_string())
            .with_stable_code(StableErrorCode::ConflictOperationBlocked),
        UndoError::Restore(RestoreError::NonRestorableOperation) => {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::CliInvalidTarget)
        }
        UndoError::Restore(RestoreError::Storage(message))
            if message.contains("not found") || message.contains("not a completed") =>
        {
            CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidTarget)
        }
        UndoError::Restore(_) => {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}

async fn ensure_transition_clean(force: bool) -> CliResult<()> {
    if !force && !status::is_clean().await {
        return Err(CliError::fatal("working tree has uncommitted changes")
            .with_stable_code(StableErrorCode::ConflictUnresolved)
            .with_hint("use --force to transition anyway, or commit/stash changes first"));
    }
    Ok(())
}

/// Render one `op log` request, including optional command filtering and paging.
async fn handle_op_log(
    number: Option<u64>,
    page: Option<u64>,
    command_filter: Option<String>,
    verbose: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let db = get_db_conn_instance().await;
    let repo_id = current_repo_id().await?;
    let query_page = OperationQueryPage {
        page: page.unwrap_or(1),
        per_page: number.unwrap_or(OperationQueryPage::DEFAULT_PER_PAGE),
    };

    let result =
        query_operation_log_page(&db, &repo_id, query_page, command_filter.as_deref()).await?;

    let entries: Vec<OpLogEntry> = result.items.iter().map(log_entry_from_item).collect();
    let op_output = OpOutput::Log {
        operations: entries.clone(),
        page: result.page,
        per_page: result.per_page,
        total: result.total,
    };

    if output.is_json() {
        return emit_json_data("op", &op_output, output);
    }
    if output.quiet {
        return Ok(());
    }

    println!(
        "Operations (page {}, {} per page, shown {}):",
        result.page,
        result.per_page,
        entries.len()
    );
    println!();

    for op in &entries {
        let short_id = &op.op_id[..8.min(op.op_id.len())];
        let timestamp = op
            .end_ts
            .map(format_timestamp)
            .unwrap_or_else(|| "running".to_string());

        if verbose {
            println!("{short_id}@{{{}}}", op.index);
            println!("  command: {}", op.command_name);
            println!("  description: {}", op.description);
            println!("  actor: {}", op.actor);
            println!("  status: {}", op.status);
            println!("  time: {timestamp}");
            println!();
        } else {
            println!(
                "{short_id}@{{{}}} {} {} - {} [{}]",
                op.index, op.command_name, op.description, timestamp, op.status
            );
        }
    }

    Ok(())
}

const OPERATION_HISTORY_CTE: &str = r#"
WITH ranked AS (
    SELECT op_id,
           COALESCE(command_name, kind) AS command_name,
           COALESCE(description, kind) AS description,
           COALESCE(actor, '') AS actor,
           start_ts, end_ts, status,
           post_view_oid AS view_id,
           ROW_NUMBER() OVER (
               ORDER BY end_ts DESC, start_ts DESC, op_id DESC
           ) - 1 AS history_index
      FROM operation
     WHERE repo_id = ?
)
"#;

const OPERATION_HISTORY_FIELDS: &str =
    "op_id, command_name, description, actor, start_ts, end_ts, status, view_id, history_index";

/// Query the Operation v2 history in its canonical newest-first order.
async fn query_operation_log_page<C: ConnectionTrait>(
    db: &C,
    repo_id: &str,
    query_page: OperationQueryPage,
    command_filter: Option<&str>,
) -> CliResult<OperationPage<OperationHistoryEntry>> {
    let query_page = query_page.normalized();
    let command_filter = command_filter
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let count_sql = format!(
        "{OPERATION_HISTORY_CTE} SELECT COUNT(*) AS total FROM ranked \
         WHERE (? IS NULL OR command_name = ?)"
    );
    let count_row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            count_sql,
            [
                repo_id.to_string().into(),
                command_filter.clone().into(),
                command_filter.clone().into(),
            ],
        ))
        .await
        .map_err(|error| CliError::fatal(format!("failed to count operations: {error}")))?
        .ok_or_else(|| CliError::fatal("operation history count returned no row"))?;
    let total = count_row
        .try_get::<i64>("", "total")
        .map_err(|error| CliError::fatal(format!("failed to read operation count: {error}")))?;
    let offset = i64::try_from(query_page.offset()).unwrap_or(i64::MAX);
    let list_sql = format!(
        "{OPERATION_HISTORY_CTE} SELECT {OPERATION_HISTORY_FIELDS} FROM ranked \
         WHERE (? IS NULL OR command_name = ?) ORDER BY history_index LIMIT ? OFFSET ?"
    );
    let rows = db
        .query_all_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            list_sql,
            [
                repo_id.to_string().into(),
                command_filter.clone().into(),
                command_filter.into(),
                i64::try_from(query_page.per_page)
                    .unwrap_or(i64::MAX)
                    .into(),
                offset.into(),
            ],
        ))
        .await
        .map_err(|error| CliError::fatal(format!("failed to query operations: {error}")))?;
    let items = rows
        .iter()
        .map(operation_history_entry_from_row)
        .collect::<CliResult<Vec<_>>>()?;

    Ok(OperationPage {
        items,
        page: query_page.page,
        per_page: query_page.per_page,
        total: u64::try_from(total).unwrap_or(0),
    })
}

fn operation_history_entry_from_row(
    row: &sea_orm::QueryResult,
) -> CliResult<OperationHistoryEntry> {
    macro_rules! field {
        ($name:literal, $ty:ty) => {
            row.try_get::<$ty>("", $name).map_err(|error| {
                CliError::fatal(format!(
                    "failed to read operation history field '{}': {error}",
                    $name
                ))
            })?
        };
    }

    let raw_start_ts = field!("start_ts", i64);
    let raw_end_ts = field!("end_ts", Option<i64>);
    let (start_ts, end_ts) = (
        raw_start_ts.div_euclid(1000),
        raw_end_ts.map(|timestamp| timestamp.div_euclid(1000)),
    );
    let stored_status = field!("status", String);
    let status = match stored_status.as_str() {
        "success" => "succeeded".to_string(),
        "aborted" => "canceled".to_string(),
        status => status.to_string(),
    };
    let history_index = field!("history_index", i64);
    let index = usize::try_from(history_index)
        .map_err(|_| CliError::fatal("operation history index is out of range"))?;

    Ok(OperationHistoryEntry {
        index,
        op_id: field!("op_id", String),
        command_name: field!("command_name", String),
        description: field!("description", String),
        actor: field!("actor", String),
        status,
        start_ts,
        end_ts,
        view_id: field!("view_id", String),
    })
}

/// Render one `op show` request after resolving the supplied operation reference.
async fn handle_op_show(op_ref: String, show_view: bool, output: &OutputConfig) -> CliResult<()> {
    let db = get_db_conn_instance().await;
    let repo_id = current_repo_id().await?;
    let entry = resolve_op_ref(&db, &repo_id, &op_ref).await?;
    let op_output = OpOutput::Show {
        op_id: entry.op_id.clone(),
        command_name: entry.command_name.clone(),
        description: entry.description.clone(),
        actor: entry.actor.clone(),
        status: entry.status.clone(),
        start_ts: entry.start_ts,
        end_ts: entry.end_ts,
        view_id: entry.view_id.clone(),
    };

    if output.is_json() {
        return emit_json_data("op", &op_output, output);
    }

    let short_id = &entry.op_id[..8.min(entry.op_id.len())];
    println!("Operation: {short_id}");
    println!("Command: {}", entry.command_name);
    println!("Description: {}", entry.description);
    println!("Actor: {}", entry.actor);
    println!("Status: {}", entry.status);
    println!("Started: {}", format_timestamp(entry.start_ts));
    if let Some(end_ts) = entry.end_ts {
        println!("Completed: {}", format_timestamp(end_ts));
        println!(
            "Duration: {}ms",
            end_ts.saturating_sub(entry.start_ts) * 1000
        );
    }
    println!("View ID: {}", entry.view_id);

    if show_view {
        print_v2_view_snapshot(&repo_id, &entry.op_id).await?;
    }

    Ok(())
}

async fn print_v2_view_snapshot(repo_id: &str, op_id: &str) -> CliResult<()> {
    let Some(scope) = RequestScope::try_resolve(util::cur_dir())
        .map_err(|error| CliError::fatal(format!("failed to resolve repository scope: {error}")))?
    else {
        return Err(CliError::fatal(
            "v2 operation state is unavailable in this repository",
        ));
    };
    let storage = ClientStorage::init_local(scope.storage.join("objects"));
    let db = get_db_conn_instance().await;
    let store = OperationStoreV2::new_for_repo(repo_id, db, storage);
    let operation = store
        .load_operation(op_id)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to load v2 operation '{op_id}': {error}"))
        })?
        .ok_or_else(|| CliError::fatal(format!("v2 operation '{op_id}' not found")))?;
    let view = store.load_view(&operation.post_view_oid).map_err(|error| {
        CliError::fatal(format!("failed to load v2 view for '{op_id}': {error}"))
    })?;

    println!();
    println!("View Snapshot:");
    if let Some(snapshot_oid) = view.workspaces.values().next() {
        let snapshot = store.load_snapshot(snapshot_oid).map_err(|error| {
            CliError::fatal(format!(
                "failed to load v2 workspace snapshot for '{op_id}': {error}"
            ))
        })?;
        use crate::internal::operation::HeadState;
        match snapshot.head {
            HeadState::Symbolic { reference } => {
                if let Some(branch) = reference.strip_prefix("refs/heads/") {
                    println!("  HEAD: {branch} (branch)");
                } else {
                    println!("  HEAD: {reference} (symbolic)");
                }
            }
            HeadState::Detached { oid } => {
                let oid = oid.to_string();
                println!("  HEAD: {} (detached)", &oid[..7.min(oid.len())]);
            }
        }
    } else {
        println!("  HEAD: (not captured)");
    }

    println!("  Refs:");
    let refs_bytes = store.load_object(&view.refs_facet_oid).map_err(|error| {
        CliError::fatal(format!("failed to load v2 refs for '{op_id}': {error}"))
    })?;
    let refs_value: serde_json::Value = serde_json::from_slice(&refs_bytes).map_err(|error| {
        CliError::fatal(format!("failed to decode v2 refs for '{op_id}': {error}"))
    })?;
    if let Some(references) = refs_value
        .get("references")
        .and_then(serde_json::Value::as_array)
    {
        for reference in references {
            let Some(kind) = reference.get("kind").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(name) = reference.get("name").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let ref_name = reference
                .get("remote")
                .and_then(serde_json::Value::as_str)
                .map(|remote| format!("{kind}/{remote}/{name}"))
                .unwrap_or_else(|| format!("{kind} {name}"));
            if let Some(target) = reference.get("commit").and_then(serde_json::Value::as_str) {
                println!("    {ref_name}: {}", &target[..7.min(target.len())]);
            }
        }
    }
    Ok(())
}

/// Restore the repository view referenced by one prior operation.
async fn handle_op_restore(
    op_ref: String,
    force: bool,
    dry_run: bool,
    what: RestoreWhat,
    confirm_repo_wide: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let db = get_db_conn_instance().await;
    let repo_id = current_repo_id().await?;
    let target_entry = resolve_op_ref(&db, &repo_id, &op_ref).await?;
    handle_v2_restore(
        &db,
        &repo_id,
        &target_entry.op_id,
        force,
        what,
        dry_run,
        confirm_repo_wide,
        output,
    )
    .await
}

/// Restore through the Operation v2 engine and never synthesize a v2 view from
/// historical storage.
#[allow(clippy::too_many_arguments)]
async fn handle_v2_restore(
    db: &sea_orm::DatabaseConnection,
    repo_id: &str,
    op_id: &str,
    force: bool,
    what: RestoreWhat,
    dry_run: bool,
    confirm_repo_wide: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let Some(operation_scope) = RequestScope::try_resolve(util::cur_dir())
        .map_err(|error| CliError::fatal(format!("failed to resolve repository scope: {error}")))?
    else {
        return Err(CliError::fatal(
            "v2 operation state is unavailable in this repository",
        ));
    };
    let object_storage = ClientStorage::init_local(operation_scope.storage.join("objects"));
    let store = OperationStoreV2::new_for_repo(repo_id, db.clone(), object_storage.clone());
    let operation = store
        .load_operation(op_id)
        .await
        .map_err(|error| CliError::fatal(format!("failed to load v2 operation: {error}")))?
        .ok_or_else(|| CliError::fatal(format!("v2 operation '{op_id}' not found")))?;
    let engine = RestoreEngine::new(operation_scope, repo_id, db.clone(), object_storage);
    engine
        .validate_target(
            op_id.to_string(),
            operation.post_view_oid,
            OperationKind::Restore,
            confirm_repo_wide,
        )
        .await
        .map_err(|error| {
            CliError::fatal(format!("v2 restore failed: {error}"))
                .with_stable_code(restore_error_code(&error))
        })?;
    if !force && !status::is_clean().await {
        return Err(CliError::fatal("working tree has uncommitted changes")
            .with_stable_code(StableErrorCode::ConflictUnresolved)
            .with_hint("use --force to restore anyway, or commit/stash changes first"));
    }
    let receipt = engine
        .restore(
            op_id.to_string(),
            operation.post_view_oid,
            what,
            dry_run,
            confirm_repo_wide,
        )
        .await
        .map_err(|error| {
            CliError::fatal(format!("v2 restore failed: {error}"))
                .with_stable_code(restore_error_code(&error))
        })?;
    if output.is_json() {
        if receipt.dry_run {
            emit_json_data("op", &OpOutput::RestoreV2 { receipt }, output)?;
        } else {
            let new_op_id = receipt.new_op_id.clone().unwrap_or_default();
            let target_short =
                receipt.target_op_id[..8.min(receipt.target_op_id.len())].to_string();
            emit_json_data(
                "op",
                &OpOutput::Restore {
                    target_op_id: receipt.target_op_id,
                    new_op_id,
                    message: format!("Restored to operation {target_short}"),
                },
                output,
            )?;
        }
    } else if !output.quiet {
        let target_short = &receipt.target_op_id[..8.min(receipt.target_op_id.len())];
        let description = operation
            .metadata
            .description
            .as_deref()
            .unwrap_or("operation view");
        if receipt.dry_run {
            render_restore_preview(&store, &operation, db).await?;
        } else {
            println!("Restored to operation {target_short} ({description})");
            if let Some(new_op_id) = &receipt.new_op_id {
                println!(
                    "New operation recorded: {}",
                    &new_op_id[..8.min(new_op_id.len())]
                );
            }
        }
    }
    Ok(())
}

/// Render the stable human restore preview used by the legacy command
/// surface. The v2 receipt remains the machine-facing source of counts; this
/// preview adds the target HEAD/ref names and the local branches that an
/// all-facets restore would prune.
async fn render_restore_preview(
    store: &OperationStoreV2,
    operation: &crate::internal::operation::OperationV2,
    db: &sea_orm::DatabaseConnection,
) -> CliResult<()> {
    let target_short = &operation.op_id[..8.min(operation.op_id.len())];
    let description = operation
        .metadata
        .description
        .as_deref()
        .unwrap_or("operation view");
    let view = store.load_view(&operation.post_view_oid).map_err(|error| {
        CliError::fatal(format!(
            "failed to load restore preview view for '{}': {error}",
            operation.op_id
        ))
    })?;
    let workspace_id =
        match crate::internal::worktree_scope::RequestScope::try_resolve(util::cur_dir()).map_err(
            |error| CliError::fatal(format!("failed to resolve repository scope: {error}")),
        )? {
            Some(scope) => scope
                .scope
                .worktree_id()
                .map(str::to_string)
                .unwrap_or_else(|| "main".to_string()),
            None => "main".to_string(),
        };
    let snapshot_oid = view.workspaces.get(&workspace_id).ok_or_else(|| {
        CliError::fatal(format!(
            "restore preview has no workspace snapshot for '{workspace_id}'"
        ))
    })?;
    let snapshot = store.load_snapshot(snapshot_oid).map_err(|error| {
        CliError::fatal(format!("failed to load restore preview snapshot: {error}"))
    })?;

    println!("Would restore to operation {target_short} ({description})");
    match snapshot.head {
        crate::internal::operation::HeadState::Symbolic { reference } => {
            let branch = reference
                .strip_prefix("refs/heads/")
                .unwrap_or(reference.as_str());
            println!("  HEAD would become: {branch} (branch)");
        }
        crate::internal::operation::HeadState::Detached { oid } => {
            println!("  HEAD would become: {oid} (detached)");
        }
    }

    let refs_bytes = store.load_object(&view.refs_facet_oid).map_err(|error| {
        CliError::fatal(format!("failed to load restore preview refs: {error}"))
    })?;
    let refs_value: serde_json::Value = serde_json::from_slice(&refs_bytes).map_err(|error| {
        CliError::fatal(format!("failed to decode restore preview refs: {error}"))
    })?;
    let references = refs_value
        .get("references")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| CliError::fatal("restore preview refs have no references array"))?;
    println!("Refs that would be restored:");
    for reference in references {
        let Some(kind) = reference.get("kind").and_then(serde_json::Value::as_str) else {
            continue;
        };
        let name = reference
            .get("name")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("HEAD");
        let ref_name = reference
            .get("remote")
            .and_then(serde_json::Value::as_str)
            .map(|remote| format!("{kind}/{remote}/{name}"))
            .unwrap_or_else(|| format!("{kind} {name}"));
        if let Some(target) = reference.get("commit").and_then(serde_json::Value::as_str) {
            println!("  {ref_name}: {}", &target[..7.min(target.len())]);
        } else {
            println!("  {ref_name}");
        }
    }

    let keep = references
        .iter()
        .filter(|reference| {
            reference.get("kind").and_then(serde_json::Value::as_str) == Some("Branch")
                && reference
                    .get("remote")
                    .is_none_or(serde_json::Value::is_null)
        })
        .filter_map(|reference| {
            reference
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(str::to_string)
        })
        .collect::<HashSet<_>>();
    let pruned = local_branches_to_prune(db, &keep).await?;
    if pruned.is_empty() {
        println!("No branches would be pruned.");
    } else {
        println!("Branches that would be pruned (absent from the target view):");
        for name in pruned {
            println!("  {name}");
        }
    }
    Ok(())
}

async fn local_branches_to_prune(
    db: &sea_orm::DatabaseConnection,
    keep: &HashSet<String>,
) -> CliResult<Vec<String>> {
    let rows = db
        .query_all_raw(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT name FROM reference WHERE kind = 'Branch' AND remote IS NULL ORDER BY name",
        ))
        .await
        .map_err(|error| CliError::fatal(format!("failed to inspect local branches: {error}")))?;
    let mut branches = Vec::with_capacity(rows.len());
    for row in rows {
        let name = row
            .try_get_by_index::<String>(0)
            .map_err(|error| CliError::fatal(format!("failed to decode local branch: {error}")))?;
        if !keep.contains(&name)
            && !crate::internal::branch::is_locked_branch(&name)
            && !name.starts_with("libra/")
        {
            branches.push(name);
        }
    }
    Ok(branches)
}

fn restore_error_code(error: &RestoreError) -> StableErrorCode {
    match error {
        RestoreError::WorkspaceMissing(_)
        | RestoreError::WrongWorkspace(_)
        | RestoreError::WrongScope { .. }
        | RestoreError::NonRestorableOperation
        | RestoreError::IncompleteSnapshot
        | RestoreError::HeadConfirmationRequired => StableErrorCode::CliInvalidTarget,
        RestoreError::Cas(_) => StableErrorCode::ConflictUnresolved,
        RestoreError::Object { .. } | RestoreError::IncompleteView => StableErrorCode::RepoCorrupt,
        RestoreError::Io(_) => StableErrorCode::IoWriteFailed,
        RestoreError::Facet(_) | RestoreError::Storage(_) => {
            StableErrorCode::ConflictOperationBlocked
        }
    }
}

/// Resolve an operation reference against the v2 history.
async fn resolve_v2_history_ref<C: ConnectionTrait>(
    db: &C,
    repo_id: &str,
    op_ref: &str,
) -> CliResult<OperationHistoryEntry> {
    resolve_op_ref(db, repo_id, op_ref).await
}

async fn current_repo_id() -> CliResult<String> {
    ConfigKv::get("libra.repoid")
        .await
        .map_err(|e| CliError::fatal(format!("failed to read repository id: {e}")))?
        .map(|entry| entry.value)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            CliError::fatal("repository id is missing")
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("run 'libra init' to initialize repository metadata")
        })
}

/// Resolve an operation reference against the v2 order used by `op log`.
async fn resolve_op_ref<C: ConnectionTrait>(
    db: &C,
    repo_id: &str,
    op_ref: &str,
) -> CliResult<OperationHistoryEntry> {
    if let Some(index_text) = op_ref
        .strip_prefix("@{")
        .and_then(|value| value.strip_suffix('}'))
    {
        let index = index_text.parse::<usize>().map_err(|_| {
            CliError::fatal(format!("invalid operation index: {op_ref}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        let index_value = i64::try_from(index).map_err(|_| {
            CliError::fatal(format!("operation index {index} out of range"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use 'libra op log' to see available operations")
        })?;
        let sql = format!(
            "{OPERATION_HISTORY_CTE} SELECT {OPERATION_HISTORY_FIELDS} FROM ranked \
             WHERE history_index = ?"
        );
        let row = db
            .query_one_raw(Statement::from_sql_and_values(
                DbBackend::Sqlite,
                sql,
                [repo_id.to_string().into(), index_value.into()],
            ))
            .await
            .map_err(|error| {
                CliError::fatal(format!("failed to resolve operation index: {error}"))
            })?;
        return row
            .as_ref()
            .map(operation_history_entry_from_row)
            .transpose()?
            .ok_or_else(|| {
                CliError::fatal(format!("operation index {index} out of range"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra op log' to see available operations")
            });
    }

    let sql = format!(
        "{OPERATION_HISTORY_CTE} SELECT {OPERATION_HISTORY_FIELDS} FROM ranked \
         WHERE op_id = ?"
    );
    let row = db
        .query_one_raw(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            sql,
            [repo_id.to_string().into(), op_ref.to_string().into()],
        ))
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to resolve operation '{op_ref}': {error}"))
        })?;
    row.as_ref()
        .map(operation_history_entry_from_row)
        .transpose()?
        .ok_or_else(|| {
            CliError::fatal(format!("operation '{op_ref}' not found"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use 'libra op log' to list available operations")
        })
}

/// Convert one canonical history row into the command-layer log output shape.
fn log_entry_from_item(op: &OperationHistoryEntry) -> OpLogEntry {
    OpLogEntry {
        index: op.index,
        op_id: op.op_id.clone(),
        command_name: op.command_name.clone(),
        description: op.description.clone(),
        actor: op.actor.clone(),
        status: op.status.clone(),
        end_ts: op.end_ts,
    }
}

/// Format a unix timestamp for human-readable CLI output.
fn format_timestamp(ts: i64) -> String {
    use chrono::{TimeZone, Utc};
    Utc.timestamp_opt(ts, 0)
        .single()
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| ts.to_string())
}
