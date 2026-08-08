//! Operation (op) command group for viewing and restoring command-level operation history.

use std::{collections::HashSet, str::FromStr};

use clap::{Parser, Subcommand};
use git_internal::hash::ObjectHash;
use sea_orm::DbErr;
use serde::Serialize;

use crate::{
    command::status,
    internal::{
        branch::{Branch, is_locked_branch},
        config::ConfigKv,
        db::get_db_conn_instance,
        head::Head,
        operation::{
            OperationGraphRecord, OperationLogListItem, OperationPage, OperationQueryPage,
            OperationService, OperationStatus,
        },
        operation_wrapper::{OperationMeta, OperationScope, with_operation_log},
    },
    utils::{
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
}

#[derive(Debug, Clone, Serialize)]
/// One entry rendered by `op log`.
pub struct OpLogEntry {
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
        } => handle_op_restore(op_ref, force, dry_run, output).await,
    }
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

    let page_start = result
        .page
        .saturating_sub(1)
        .saturating_mul(result.per_page) as usize;
    for (page_offset, op) in entries.iter().enumerate() {
        let idx = page_start + page_offset;
        let short_id = &op.op_id[..8.min(op.op_id.len())];
        let timestamp = op
            .end_ts
            .map(format_timestamp)
            .unwrap_or_else(|| "running".to_string());

        if verbose {
            println!("{short_id}@{{{idx}}}");
            println!("  command: {}", op.command_name);
            println!("  description: {}", op.description);
            println!("  actor: {}", op.actor);
            println!("  status: {}", op.status);
            println!("  time: {timestamp}");
            println!();
        } else {
            println!(
                "{short_id}@{{{idx}}} {} {} - {} [{}]",
                op.command_name, op.description, timestamp, op.status
            );
        }
    }

    Ok(())
}

/// Query one operation-log page, applying the optional command filter before pagination.
async fn query_operation_log_page<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
    query_page: OperationQueryPage,
    command_filter: Option<&str>,
) -> CliResult<OperationPage<OperationLogListItem>> {
    let command_filter = command_filter
        .map(str::trim)
        .filter(|value| !value.is_empty());
    OperationService::list_operations_by_repo_and_command_paginated_with_conn(
        db,
        repo_id,
        command_filter,
        query_page,
    )
    .await
    .map_err(|e| CliError::fatal(format!("failed to query operations: {e}")))
}

/// Render one `op show` request after resolving the supplied operation reference.
async fn handle_op_show(op_ref: String, show_view: bool, output: &OutputConfig) -> CliResult<()> {
    let db = get_db_conn_instance().await;
    let repo_id = current_repo_id().await?;
    let op_id = resolve_op_ref(&db, &repo_id, &op_ref).await?;

    let graph = load_operation_graph(&db, &op_id).await?;
    let op_record = &graph.operation;
    let op_output = OpOutput::Show {
        op_id: op_record.op_id.clone(),
        command_name: op_record.command_name.clone(),
        description: op_record.description.clone(),
        actor: op_record.actor.clone(),
        status: status_label(op_record.status).to_string(),
        start_ts: op_record.start_ts,
        end_ts: op_record.end_ts,
        view_id: op_record.view_id.clone(),
    };

    if output.is_json() {
        return emit_json_data("op", &op_output, output);
    }

    let short_id = &op_record.op_id[..8.min(op_record.op_id.len())];
    println!("Operation: {short_id}");
    println!("Command: {}", op_record.command_name);
    println!("Description: {}", op_record.description);
    println!("Actor: {}", op_record.actor);
    println!("Status: {}", status_label(op_record.status));
    println!("Started: {}", format_timestamp(op_record.start_ts));
    if let Some(end_ts) = op_record.end_ts {
        println!("Completed: {}", format_timestamp(end_ts));
        println!(
            "Duration: {}ms",
            end_ts.saturating_sub(op_record.start_ts) * 1000
        );
    }
    println!("View ID: {}", op_record.view_id);

    if show_view {
        println!();
        println!("View Snapshot:");
        println!(
            "  HEAD: {} ({})",
            graph.view.head_target, graph.view.head_kind
        );
        println!("  Refs:");
        for ref_rec in &graph.refs {
            let ref_name = if let Some(remote) = &ref_rec.ref_remote {
                format!("{}/{}/{}", ref_rec.ref_kind, remote, ref_rec.ref_name)
            } else {
                format!("{} {}", ref_rec.ref_kind, ref_rec.ref_name)
            };
            println!(
                "    {}: {}",
                ref_name,
                &ref_rec.target_oid[..7.min(ref_rec.target_oid.len())]
            );
        }
    }

    Ok(())
}

/// The set of local branch names that an `op restore` to `graph` must KEEP: the
/// local branches captured in the target view plus the restored HEAD branch.
/// Any other (non-locked) local branch is pruned so the restore reproduces the
/// operation's exact local-branch set.
fn restore_keep_set(graph: &OperationGraphRecord) -> HashSet<String> {
    let mut keep: HashSet<String> = graph
        .refs
        .iter()
        .filter(|r| r.ref_kind == "branch" && r.ref_remote.is_none())
        .map(|r| r.ref_name.clone())
        .collect();
    if graph.view.head_kind == "branch" {
        keep.insert(graph.view.head_target.clone());
    }
    keep
}

/// List the local branches that an `op restore` would prune for the given
/// `keep` set: present now, absent from the target view, and not a Libra-owned
/// locked branch (`main`/`intent`/`traces`/`agent-traces`), which are never
/// pruned so AI/session history and the trunk are preserved. Remote-tracking
/// refs are excluded (the listing is local-only).
async fn local_branches_to_prune<C: sea_orm::ConnectionTrait>(
    db: &C,
    keep: &HashSet<String>,
) -> Result<Vec<String>, DbErr> {
    let current = Branch::list_branches_result_with_conn(db, None)
        .await
        .map_err(|e| DbErr::Custom(e.to_string()))?;
    Ok(prune_candidates(current.into_iter().map(|b| b.name), keep))
}

/// Pure prune predicate over local branch names. A name is a prune candidate
/// unless it is in `keep`, is a Libra-owned locked branch
/// (`main`/`intent`/`traces`/`agent-traces`), or lives in the reserved `libra/`
/// namespace. The namespace guard protects AI-owned refs such as the history
/// branch `libra/intent` and the orchestrator's `libra/src`/`libra/target`,
/// which are stored as local `Branch` rows but must never be deleted by an
/// `op restore`. Split out so the protection can be unit-tested without a
/// database (the CLI refuses to create these refs, so an integration fixture
/// cannot reproduce one).
fn prune_candidates<I: IntoIterator<Item = String>>(
    current: I,
    keep: &HashSet<String>,
) -> Vec<String> {
    current
        .into_iter()
        .filter(|name| {
            !keep.contains(name) && !is_locked_branch(name) && !name.starts_with("libra/")
        })
        .collect()
}

/// Restore the repository view referenced by one prior operation.
async fn handle_op_restore(
    op_ref: String,
    force: bool,
    dry_run: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let db = get_db_conn_instance().await;
    let repo_id = current_repo_id().await?;
    let target_op_id = resolve_op_ref(&db, &repo_id, &op_ref).await?;
    let target_graph = load_operation_graph(&db, &target_op_id).await?;
    let target_op = target_graph.operation.clone();

    // plan-20260714 W0 (§C.11, ADR-0714-08): restoring rewrites THIS
    // worktree's HEAD, index and working tree from a snapshot. Doing that
    // from an operation that ran in a DIFFERENT worktree grafts that
    // worktree's state onto this one; doing it from an operation whose scope
    // was never recorded is the same act performed blind. Both are refused
    // before the dry-run report, so the preview cannot describe a restore
    // that is not permitted. Restoring the current worktree's own operations
    // is unaffected.
    // W1 §C.9: the snapshot is HEAD and refs. An operation that also moved an
    // index, a working tree or sequencer state declared itself non-restorable
    // when it was recorded — replaying it would move HEAD while leaving
    // `sequence_state` pointing at a todo that no longer matches. Checked
    // FIRST, before the dry-run report, so the preview cannot describe a
    // restore that will not happen. A stored property, not a guess from the
    // command name.
    if !target_op.restorable {
        return Err(CliError::fatal(format!(
            "cannot restore operation {}: '{}' changed state this snapshot does not \
             capture (the index, the working tree, or an in-progress sequence)",
            &target_op_id[..8.min(target_op_id.len())],
            target_op.command_name
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint(
            "inspect it with `libra op show`, and undo a sequencer control with that \
             command's own `--abort`",
        ));
    }
    // §C.9: a repository-scope operation acts on state every worktree shares,
    // and an `unknown` one was never attributed at all. Replaying either into
    // ONE worktree is what the plan says must fail closed until LR-02, and the
    // kind is what distinguishes them from a main-scope operation — their
    // `worktree_id` is the same empty string.
    // §C.9 fail-closed: `repository` (no worktree scope at all) and `unknown`
    // (never recorded) are both refused. A `branch` operation is NOT
    // repository-scoped — it is recorded with the worktree it ran in, so branch
    // restore keeps working, and the shared refs it would move are protected by
    // the checked-out-elsewhere guard further down (Codex R24).
    if !matches!(target_op.scope_kind.as_str(), "main" | "linked") {
        return Err(CliError::fatal(format!(
            "cannot restore operation {}: it is recorded with scope kind '{}', which this \
             snapshot cannot be replayed into a single worktree",
            &target_op_id[..8.min(target_op_id.len())],
            target_op.scope_kind
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint(
            "inspect it with `libra op show`; repository-wide restore is deferred to the \
             operation-recovery work",
        ));
    }
    // A claim that was never closed: the command crashed, or is still running.
    // Either way its recorded view is not the state it will end in.
    if target_op.status != crate::internal::operation::OperationStatus::Succeeded {
        return Err(CliError::fatal(format!(
            "cannot restore operation {}: it is recorded as {}, not a completed success",
            &target_op_id[..8.min(target_op_id.len())],
            target_op.status.as_str()
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("wait for it to finish, then restore from a completed operation"));
    }
    let current_scope = crate::utils::util::current_worktree_id().unwrap_or_default();
    // Accept ONLY the positive value. Testing for the literal "unknown"
    // instead would let any corrupted or mistyped provenance ("declraed")
    // through as trusted — failing open on precisely the rows whose
    // trustworthiness is the question.
    if target_op.scope_provenance != "declared" {
        let detail = if target_op.scope_provenance == "unknown" {
            "which worktree it ran in was never recorded (it predates worktree-scoped \
             operation records in a repository that has linked worktrees)"
                .to_string()
        } else {
            format!(
                "its recorded scope provenance '{}' is not a value this version understands",
                target_op.scope_provenance
            )
        };
        return Err(CliError::fatal(format!(
            "cannot restore operation {}: {detail}",
            &target_op_id[..8.min(target_op_id.len())]
        ))
        .with_stable_code(StableErrorCode::RepoCorrupt)
        .with_hint(
            "restore from an operation recorded after the upgrade, or inspect it with \
             `libra op log` and reproduce the change directly",
        ));
    }
    if target_op.worktree_id != current_scope {
        let describe = |scope: &str| {
            if scope.is_empty() {
                "the main worktree".to_string()
            } else {
                format!("worktree '{scope}'")
            }
        };
        return Err(CliError::fatal(format!(
            "operation {} ran in {}, but this is {}",
            &target_op_id[..8.min(target_op_id.len())],
            describe(&target_op.worktree_id),
            describe(&current_scope)
        ))
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("run `libra op restore` from the worktree the operation ran in"));
    }

    if !force && !status::is_clean().await {
        return Err(CliError::fatal("working tree has uncommitted changes")
            .with_stable_code(StableErrorCode::ConflictUnresolved)
            .with_hint("use --force to restore anyway, or commit/stash changes first"));
    }

    if dry_run {
        let short_id = &target_op_id[..8.min(target_op_id.len())];
        println!(
            "Would restore to operation {} ({})",
            short_id, target_op.description
        );
        println!(
            "  HEAD would become: {} ({})",
            target_graph.view.head_target, target_graph.view.head_kind
        );
        println!("Refs that would be restored:");
        for ref_rec in &target_graph.refs {
            println!(
                "  {}: {}",
                ref_rec.ref_name,
                &ref_rec.target_oid[..7.min(ref_rec.target_oid.len())]
            );
        }
        let keep = restore_keep_set(&target_graph);
        let pruned = local_branches_to_prune(&db, &keep)
            .await
            .map_err(|e| CliError::fatal(format!("failed to inspect branches: {e}")))?;
        if pruned.is_empty() {
            println!("No branches would be pruned.");
        } else {
            println!("Branches that would be pruned (absent from the target view):");
            for name in &pruned {
                println!("  {name}");
            }
        }
        return Ok(());
    }

    // Part C W0 (§C.11): op restore rewrites and prunes SHARED branch refs.
    // Refuse if any affected branch is checked out in ANOTHER worktree — moving
    // its tip or deleting it would corrupt that worktree's HEAD/working tree.
    // `branch_checked_out_elsewhere` excludes the current worktree, so restoring
    // this worktree's own branch stays allowed.
    {
        let keep = restore_keep_set(&target_graph);
        let pruned = local_branches_to_prune(&db, &keep)
            .await
            .map_err(|e| CliError::fatal(format!("failed to inspect branches: {e}")))?;
        let mut affected: Vec<String> = target_graph
            .refs
            .iter()
            .filter(|r| r.ref_kind == "branch")
            .map(|r| r.ref_name.clone())
            .collect();
        affected.extend(pruned);
        for branch in &affected {
            let short = branch.strip_prefix("refs/heads/").unwrap_or(branch);
            // §C.4.4: fail CLOSED. The infallible probe folded a database
            // error into "nobody has it checked out", so a transient DB
            // failure let a restore move a branch another worktree was on.
            let checked_out = Head::branch_checked_out_elsewhere_result(short)
                .await
                .map_err(|error| {
                    CliError::fatal(format!(
                        "cannot determine whether branch '{short}' is checked out in another \
                         worktree: {error}"
                    ))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
                })?;
            if let Some(other) = checked_out {
                return Err(CliError::fatal(format!(
                    "cannot restore: branch '{short}' is checked out at worktree '{other}'"
                ))
                // §C.13: branch checked out in another worktree →
                // LBR-CONFLICT-002.
                .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                .with_hint("switch that worktree to another branch first, or run restore there"));
            }
        }
    }

    let restore_meta = OperationMeta {
        command_name: "op restore".to_string(),
        description: format!("restore to {}", &target_op_id[..8.min(target_op_id.len())]),
        actor: operation_actor().await,
        repo_id,
        args_digest: Some(target_op_id.clone()),
    };
    let restore_graph = target_graph.clone();

    let result = with_operation_log(restore_meta, OperationScope::default(), move |txn| {
        Box::pin(async move {
            let new_head = if restore_graph.view.head_kind == "branch" {
                Head::Branch(restore_graph.view.head_target.clone())
            } else {
                Head::Detached(
                    ObjectHash::from_str(&restore_graph.view.head_target)
                        .map_err(|e| DbErr::Custom(e.to_string()))?,
                )
            };
            Head::update_result_with_conn(txn, new_head, None)
                .await
                .map_err(|e| DbErr::Custom(e.to_string()))?;

            for ref_rec in &restore_graph.refs {
                if ref_rec.ref_kind == "branch" {
                    Branch::update_branch_with_conn(
                        txn,
                        &ref_rec.ref_name,
                        &ref_rec.target_oid,
                        None,
                    )
                    .await?;
                }
            }

            // Prune local branches that are absent from the target view, so
            // `op restore` reproduces that operation's exact local-branch set
            // rather than only updating the branches it names. Libra-owned locked
            // branches (`main`/`intent`/`traces`/`agent-traces`) and the restored
            // HEAD branch are always kept; remote-tracking refs are left untouched
            // (the listing below is local-only).
            let keep = restore_keep_set(&restore_graph);
            let to_prune = local_branches_to_prune(txn, &keep).await?;
            for name in &to_prune {
                Branch::delete_branch_result_with_conn(txn, name, None)
                    .await
                    .map_err(|e| DbErr::Custom(e.to_string()))?;
            }

            Ok::<(), DbErr>(())
        })
    })
    .await
    .map_err(|e| CliError::fatal(format!("restore failed: {e}")))?;

    let op_output = OpOutput::Restore {
        target_op_id: target_op_id.clone(),
        new_op_id: result.op_id.clone(),
        message: format!(
            "Restored to operation {} ({})",
            &target_op_id[..8.min(target_op_id.len())],
            target_op.description
        ),
    };

    if output.is_json() {
        return emit_json_data("op", &op_output, output);
    }

    println!(
        "{}",
        match op_output {
            OpOutput::Restore { message, .. } => message,
            _ => unreachable!(),
        }
    );
    println!(
        "New operation recorded: {}",
        &result.op_id[..8.min(result.op_id.len())]
    );

    Ok(())
}

/// Read the current repository id from config and validate that it is non-empty.
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

/// Resolve the actor name recorded for newly created operation entries.
async fn operation_actor() -> String {
    ConfigKv::get("user.name")
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "libra-user".to_string())
}

/// Load the full restore graph for a resolved operation id.
async fn load_operation_graph<C: sea_orm::ConnectionTrait>(
    db: &C,
    op_id: &str,
) -> CliResult<OperationGraphRecord> {
    OperationService::load_restore_view_by_operation_with_conn(db, op_id)
        .await
        .map_err(|e| CliError::fatal(format!("failed to load operation '{op_id}': {e}")))?
        .ok_or_else(|| {
            CliError::fatal(format!("operation '{op_id}' not found"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use 'libra op log' to list available operations")
        })
}

/// Resolve an operation reference that may be either a raw id or an indexed `@{n}` entry.
async fn resolve_op_ref<C: sea_orm::ConnectionTrait>(
    db: &C,
    repo_id: &str,
    op_ref: &str,
) -> CliResult<String> {
    if let Some(index_str) = op_ref.strip_prefix("@{")
        && let Some(index_end) = index_str.find('}')
    {
        let index: usize = index_str[..index_end].parse().map_err(|_| {
            CliError::fatal(format!("invalid operation index: {op_ref}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        let page = OperationQueryPage {
            page: 1,
            per_page: (index + 1) as u64,
        };
        let result =
            OperationService::list_operations_by_repo_paginated_with_conn(db, repo_id, page)
                .await
                .map_err(|e| CliError::fatal(format!("failed to query operations: {e}")))?;

        return result
            .items
            .into_iter()
            .nth(index)
            .map(|op| op.op_id)
            .ok_or_else(|| {
                CliError::fatal(format!("operation index {index} out of range"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
                    .with_hint("use 'libra op log' to see available operations")
            });
    }

    Ok(op_ref.to_string())
}

/// Convert one service log item into the command-layer output shape.
fn log_entry_from_item(op: &OperationLogListItem) -> OpLogEntry {
    OpLogEntry {
        op_id: op.op_id.clone(),
        command_name: op.command_name.clone(),
        description: op.description.clone(),
        actor: op.actor.clone(),
        status: status_label(op.status).to_string(),
        end_ts: op.end_ts,
    }
}

/// Convert an operation status enum into its stable CLI label.
fn status_label(status: OperationStatus) -> &'static str {
    match status {
        OperationStatus::Running => "running",
        OperationStatus::Succeeded => "succeeded",
        OperationStatus::Failed => "failed",
        OperationStatus::Canceled => "canceled",
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `prune_candidates` never proposes a Libra-owned locked branch
    /// (`main`/`intent`/`traces`/`agent-traces`) for pruning, even when it is
    /// absent from the target view's keep set — so `op restore` cannot orphan
    /// AI/session history or delete the trunk. Ordinary branches absent from the
    /// keep set are pruned; branches in the keep set are retained.
    #[test]
    fn prune_candidates_protects_locked_branches() {
        let keep: HashSet<String> = ["keep".to_string()].into_iter().collect();
        let current = [
            "main".to_string(),
            "intent".to_string(),
            "traces".to_string(),
            "agent-traces".to_string(),
            // AI history + orchestrator refs live in the reserved `libra/` namespace.
            "libra/intent".to_string(),
            "libra/src".to_string(),
            "libra/target".to_string(),
            "keep".to_string(),
            "ephemeral".to_string(),
        ];
        let pruned = prune_candidates(current, &keep);
        // Only the ordinary, view-absent branch is a prune candidate; every
        // locked branch and every `libra/`-namespaced internal ref is protected.
        assert_eq!(pruned, vec!["ephemeral".to_string()]);
    }

    /// A branch in the keep set is never a prune candidate even if it shares a
    /// name shape with user branches; an empty keep set still protects locked
    /// branches.
    #[test]
    fn prune_candidates_respects_keep_and_locks_with_empty_keep() {
        let empty: HashSet<String> = HashSet::new();
        let current = ["main".to_string(), "feature".to_string()];
        let pruned = prune_candidates(current, &empty);
        assert_eq!(
            pruned,
            vec!["feature".to_string()],
            "locked `main` is protected; `feature` is pruned"
        );
    }
}
