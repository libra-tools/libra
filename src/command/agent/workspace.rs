//! `libra agent workspace …` — the read-only machine interface over the
//! Part C W4 workspace registry (§C.8): keyset-paginated `list` and a
//! by-id `show`. Lease mutation stays internal to the AgentRuntime
//! services; this surface only OBSERVES `workspace_record`.

use clap::{Args, Subcommand};

use crate::{
    internal::{
        db::get_db_conn_instance,
        workspace::{WorkspaceListQuery, WorkspaceRecord, WorkspaceState, WorkspaceStore},
    },
    utils::{
        error::{CliError, CliResult},
        output::{OutputConfig, emit_json_data},
    },
};

/// `schema_version` of the paged list / show JSON `data` payloads
/// (additive evolution only — mirrors the AG-20 pagination precedent).
const WORKSPACE_SCHEMA_VERSION: u32 = 1;

#[derive(Subcommand, Debug)]
pub enum WorkspaceSubcommand {
    /// List workspace records, keyset-paginated (`workspace_id` ASC).
    #[command(about = "List workspace records (keyset pagination)")]
    List(WorkspaceListArgs),
    /// Show one workspace record by id.
    #[command(about = "Show one workspace record")]
    Show(WorkspaceShowArgs),
}

#[derive(Args, Debug)]
pub struct WorkspaceListArgs {
    /// Maximum rows to return (default 50, capped at 500).
    #[arg(long, value_name = "N")]
    pub limit: Option<u64>,
    /// Keyset cursor: the `next_cursor` value from the previous page,
    /// round-tripped verbatim.
    #[arg(long, value_name = "CURSOR")]
    pub cursor: Option<String>,
    /// Restrict to one or more states (repeatable):
    /// provisioning|active|releasing|released|orphaned.
    #[arg(long = "state", value_name = "STATE")]
    pub states: Vec<String>,
}

#[derive(Args, Debug)]
pub struct WorkspaceShowArgs {
    /// Workspace identifier from `agent workspace list`.
    #[arg(value_name = "WORKSPACE_ID")]
    pub workspace_id: String,
}

pub async fn execute_safe(cmd: WorkspaceSubcommand, output: &OutputConfig) -> CliResult<()> {
    match cmd {
        WorkspaceSubcommand::List(args) => list(args, output).await,
        WorkspaceSubcommand::Show(args) => show(args, output).await,
    }
}

fn parse_state(raw: &str) -> CliResult<WorkspaceState> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "provisioning" => Ok(WorkspaceState::Provisioning),
        "active" => Ok(WorkspaceState::Active),
        "releasing" => Ok(WorkspaceState::Releasing),
        "released" => Ok(WorkspaceState::Released),
        "orphaned" => Ok(WorkspaceState::Orphaned),
        other => Err(CliError::command_usage(format!(
            "invalid --state '{other}': expected provisioning, active, releasing, \
             released, or orphaned"
        ))),
    }
}

fn record_json(record: &WorkspaceRecord) -> serde_json::Value {
    serde_json::json!({
        "workspace_id": record.workspace_id,
        "kind": record.kind.as_db_value(),
        "worktree_id": record.worktree_id,
        "path": record.path,
        "owner_kind": record.owner_kind.as_db_value(),
        "owner_id": record.owner_id,
        "task_id": record.task_id,
        "session_id": record.session_id,
        "base_commit": record.base_commit,
        "branch": record.branch,
        "state": record.state.as_db_value(),
        "lease_owner": record.lease_owner,
        "lease_fence": record.lease_fence,
        "lease_expires_at": record.lease_expires_at,
        "created_at": record.created_at,
        "updated_at": record.updated_at,
    })
}

async fn list(args: WorkspaceListArgs, output: &OutputConfig) -> CliResult<()> {
    let states = if args.states.is_empty() {
        None
    } else {
        Some(
            args.states
                .iter()
                .map(|raw| parse_state(raw))
                .collect::<CliResult<Vec<_>>>()?,
        )
    };
    let query = WorkspaceListQuery {
        states,
        limit: args.limit,
        cursor: args.cursor,
    };
    let conn = get_db_conn_instance().await;
    let page = WorkspaceStore::list_with_conn(&conn, &query)
        .await
        .map_err(|error| CliError::fatal(format!("failed to list workspace records: {error}")))?;

    if output.is_json() {
        let payload = serde_json::json!({
            "schema_version": WORKSPACE_SCHEMA_VERSION,
            "workspaces": page.items.iter().map(record_json).collect::<Vec<_>>(),
            "next_cursor": page.next_cursor,
        });
        return emit_json_data("agent_workspaces", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    if page.items.is_empty() {
        println!("(no workspace records)");
        return Ok(());
    }
    for record in &page.items {
        println!(
            "{}  {:12} {:9} {}",
            record.workspace_id,
            record.state.as_db_value(),
            record.kind.as_db_value(),
            record.path
        );
    }
    if let Some(cursor) = &page.next_cursor {
        println!("(more rows: --cursor {cursor})");
    }
    Ok(())
}

async fn show(args: WorkspaceShowArgs, output: &OutputConfig) -> CliResult<()> {
    let conn = get_db_conn_instance().await;
    let record = WorkspaceStore::get_with_conn(&conn, &args.workspace_id)
        .await
        .map_err(|error| CliError::fatal(format!("failed to read workspace record: {error}")))?
        .ok_or_else(|| {
            CliError::fatal(format!(
                "no workspace matches id '{}'; list workspaces with `libra agent workspace list`",
                args.workspace_id
            ))
        })?;

    if output.is_json() {
        let payload = serde_json::json!({
            "schema_version": WORKSPACE_SCHEMA_VERSION,
            "workspace": record_json(&record),
        });
        return emit_json_data("agent_workspace", &payload, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("workspace_id: {}", record.workspace_id);
    println!("kind:         {}", record.kind.as_db_value());
    println!("state:        {}", record.state.as_db_value());
    println!("path:         {}", record.path);
    if let Some(worktree_id) = &record.worktree_id {
        println!("worktree_id:  {worktree_id}");
    }
    println!("owner_kind:   {}", record.owner_kind.as_db_value());
    if let Some(owner_id) = &record.owner_id {
        println!("owner_id:     {owner_id}");
    }
    if let Some(task_id) = &record.task_id {
        println!("task_id:      {task_id}");
    }
    if let Some(session_id) = &record.session_id {
        println!("session_id:   {session_id}");
    }
    if let Some(branch) = &record.branch {
        println!("branch:       {branch}");
    }
    if let Some(base_commit) = &record.base_commit {
        println!("base_commit:  {base_commit}");
    }
    if let Some(lease_owner) = &record.lease_owner {
        println!("lease_owner:  {lease_owner} (fence {})", record.lease_fence);
    }
    if let Some(expires) = record.lease_expires_at {
        println!("lease_expires_at: {expires}");
    }
    println!("created_at:   {}", record.created_at);
    println!("updated_at:   {}", record.updated_at);
    Ok(())
}
