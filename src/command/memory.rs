//! `libra memory` — the deterministic Agent development-history projection.
//!
//! DM-03 registers the public command surface; DM-11 delivers the read-only
//! behavior. Every subcommand is `ReadOnly` (verified by
//! `memory_writes_no_operation_row`) and never answers from a stale projection
//! (ADR-DM-10): `status`/`list`/`show` fail-closed with `LBR-MEMORY-001` unless
//! `--allow-stale` is passed, in which case the `--json` envelope marks
//! `"stale": true` and human output prints a banner.
//!
//! The command never introduces new behavior outside `src/internal/ai/memory/`:
//! it is the read/rebuild face on top of the GC-DM-01 zero-authority projection
//! (DM-01/DM-10/DM-02/DM-13). `--json` output is deterministic and never
//! contains host-absolute paths (ER-11).

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::{
        ai::memory::{
            EpisodeView, MemoryError, list_episodes, read_episode, rebuild as do_rebuild,
        },
        config::ConfigKv,
        db,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

pub const MEMORY_EXAMPLES: &str = "\
EXAMPLES:
    libra memory status          Show the projection freshness / horizon state
    libra memory list            List derived memory episodes in the window
    libra memory show <id>       Show a single derived memory episode
    libra memory rebuild         Rebuild the zero-authority projection (GC-DM-01)
    libra --json memory status   Structured JSON envelope for agents

NOTES:
    The projection is zero-authority and fully rebuildable. Read subcommands
    refuse to answer from a stale projection (ADR-DM-10); pass --allow-stale
    to read anyway, which marks \"stale\": true in the JSON envelope.";

/// Inspect / rebuild the deterministic Agent development-history projection.
#[derive(Parser, Debug)]
#[command(after_help = MEMORY_EXAMPLES)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,

    /// Answer from the projection even when it is stale (ADR-DM-10).
    #[arg(long, global = true)]
    pub allow_stale: bool,
}

/// Read-only subcommands; behavior is DM-11.
#[derive(Subcommand, Debug)]
pub enum MemoryCommand {
    /// Report projection freshness / horizon state.
    Status,
    /// List derived memory episodes in the current window.
    List,
    /// Show a single derived memory episode.
    Show {
        /// The Episode id.
        #[arg(value_name = "EPISODE_ID")]
        episode_id: String,
        /// Reveal secrets / long bodies.
        #[arg(long)]
        reveal: bool,
    },
    /// Rebuild the zero-authority projection.
    Rebuild,
}

/// Resolve the repository database connection and repo id, then dispatch.
///
/// Wraps the real implementation in `execute_safe` so the `output` config
/// (notably `--json`) and any `--allow-stale` flag flow to the subcommands.
pub async fn execute(args: MemoryArgs) -> CliResult<()> {
    execute_safe(args, &OutputConfig::default()).await
}

pub async fn execute_safe(args: MemoryArgs, output: &OutputConfig) -> CliResult<()> {
    if util::require_repo().is_err() {
        return Err(CliError::repo_not_found());
    }
    let connection = open_repo_db().await?;
    let repo_id = current_repo_id(&connection).await?;

    match args.command {
        MemoryCommand::Status => status(&connection, &repo_id, args.allow_stale, output).await,
        MemoryCommand::List => list(&connection, &repo_id, args.allow_stale, output).await,
        MemoryCommand::Show { episode_id, reveal } => {
            show(
                &connection,
                &repo_id,
                &episode_id,
                reveal,
                args.allow_stale,
                output,
            )
            .await
        }
        MemoryCommand::Rebuild => rebuild(&connection, &repo_id, output).await,
    }
}

/// Open the repository database (same pattern as `automation`).
async fn open_repo_db() -> CliResult<sea_orm::DatabaseConnection> {
    let db_path = util::try_get_storage_path(None)
        .map(|storage| storage.join(util::DATABASE))
        .map_err(|error| {
            CliError::repo_not_found()
                .with_hint(format!("failed to resolve repository storage: {error}"))
        })?;
    db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|error| {
            CliError::failure(format!(
                "failed to open repository database {}: {error}",
                db_path.display()
            ))
        })
}

/// Resolve the repository id (`libra.repoid`), mirroring `libra op`.
async fn current_repo_id<C: sea_orm::ConnectionTrait>(connection: &C) -> CliResult<String> {
    ConfigKv::get_with_conn(connection, "libra.repoid")
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

/// Map a memory-layer error into a user-friendly CLI error.
fn map_memory_error(context: &str, error: MemoryError) -> CliError {
    CliError::fatal(format!("{context}: {error}"))
        .with_stable_code(StableErrorCode::RepoCorrupt)
        .with_hint("run 'libra memory rebuild' to re-derive the projection")
}

#[derive(Debug, Serialize)]
struct StatusOutput {
    schema_version: i64,
    stale: bool,
    selector_version: i64,
    rules_version: i64,
    horizon_truncated: bool,
    revoked_count: i64,
    aged_out_count: i64,
    rebuilt_at: i64,
}

async fn status(
    connection: &sea_orm::DatabaseConnection,
    repo_id: &str,
    allow_stale: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let projection = crate::internal::ai::memory::read_status(connection, repo_id)
        .await
        .map_err(|e| map_memory_error("failed to read memory projection status", e))?;
    let Some(projection) = projection else {
        // No projection yet — that is the "never built" state, which is stale
        // by construction but not an error; report it as a baseline.
        let report = StatusOutput {
            schema_version: 0,
            stale: true,
            selector_version: crate::internal::ai::memory::EPISODE_SELECTOR_VERSION,
            rules_version: 0,
            horizon_truncated: false,
            revoked_count: 0,
            aged_out_count: 0,
            rebuilt_at: 0,
        };
        return emit_status(report, true, allow_stale, output);
    };

    let stale = !projection.is_fresh;
    if stale && !allow_stale {
        return Err(CliError::failure(
            "the memory projection is stale; run 'libra memory rebuild' or pass --allow-stale",
        )
        .with_stable_code(StableErrorCode::MemoryProjectionStale)
        .with_hint("run 'libra memory rebuild' to refresh the projection"));
    }

    let report = StatusOutput {
        schema_version: projection.schema_version,
        stale,
        selector_version: crate::internal::ai::memory::EPISODE_SELECTOR_VERSION,
        rules_version: projection.rules_version,
        horizon_truncated: projection.horizon_truncated,
        revoked_count: projection.revoked_count,
        aged_out_count: projection.aged_out_count,
        rebuilt_at: projection.rebuilt_at,
    };
    emit_status(report, stale, allow_stale, output)
}

fn emit_status(
    report: StatusOutput,
    stale: bool,
    _allow_stale: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("memory", &report, output);
    }
    if output.quiet {
        return Ok(());
    }
    if stale {
        println!("memory projection is stale");
    }
    println!("schema_version:   {}", report.schema_version);
    println!("selector_version: {}", report.selector_version);
    println!("rules_version:    {}", report.rules_version);
    println!("horizon_truncated: {}", report.horizon_truncated);
    println!("revoked_count:     {}", report.revoked_count);
    println!("aged_out_count:    {}", report.aged_out_count);
    println!("rebuilt_at:        {}", report.rebuilt_at);
    Ok(())
}

#[derive(Debug, Serialize)]
struct ListOutput {
    episodes: Vec<EpisodeView>,
}

async fn list(
    connection: &sea_orm::DatabaseConnection,
    repo_id: &str,
    allow_stale: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let episodes = list_episodes(connection, repo_id)
        .await
        .map_err(|e| map_memory_error("failed to list memory episodes", e))?;

    // Fail-closed when there is a projection but it is stale. If there is no
    // projection at all (nothing derived yet), list an empty window gracefully.
    let stale = crate::internal::ai::memory::projection_is_stale(connection, repo_id)
        .await
        .map_err(|e| map_memory_error("failed to check memory projection freshness", e))?;
    if let Some(true) = stale
        && !allow_stale
    {
        return Err(CliError::failure(
            "the memory projection is stale; run 'libra memory rebuild' or pass --allow-stale",
        )
        .with_stable_code(StableErrorCode::MemoryProjectionStale)
        .with_hint("run 'libra memory rebuild' to refresh the projection"));
    }

    let report = ListOutput {
        episodes: episodes.iter().map(EpisodeView::from_episode).collect(),
    };
    if output.is_json() {
        return emit_json_data("memory", &report, output);
    }
    if output.quiet {
        return Ok(());
    }
    for episode in &report.episodes {
        println!(
            "{}\t{}\t{}",
            episode.episode_id, episode.source_kind, episode.title
        );
    }
    Ok(())
}

async fn show(
    connection: &sea_orm::DatabaseConnection,
    repo_id: &str,
    episode_id: &str,
    reveal: bool,
    allow_stale: bool,
    output: &OutputConfig,
) -> CliResult<()> {
    let episode = read_episode(connection, repo_id, episode_id)
        .await
        .map_err(|e| map_memory_error("failed to read memory episode", e))?;
    let Some(episode) = episode else {
        return Err(CliError::failure(format!(
            "memory episode '{episode_id}' not found in this repository"
        ))
        .with_stable_code(StableErrorCode::MemoryEpisodeNotFound)
        .with_hint("run 'libra memory list' to see available episode ids"));
    };

    // Fail-closed when stale, unless --allow-stale.
    let stale = crate::internal::ai::memory::projection_is_stale(connection, repo_id)
        .await
        .map_err(|e| map_memory_error("failed to check memory projection freshness", e))?;
    if let Some(true) = stale
        && !allow_stale
    {
        return Err(CliError::failure(
            "the memory projection is stale; run 'libra memory rebuild' or pass --allow-stale",
        )
        .with_stable_code(StableErrorCode::MemoryProjectionStale)
        .with_hint("run 'libra memory rebuild' to refresh the projection"));
    }

    let view = EpisodeView::from_episode(&episode);
    if output.is_json() {
        return emit_json_data("memory", &view, output);
    }
    if output.quiet {
        return Ok(());
    }
    print_episode_human(&view, reveal);
    Ok(())
}

fn print_episode_human(view: &EpisodeView, reveal: bool) {
    println!("episode_id:  {}", view.episode_id);
    println!("source_kind: {}", view.source_kind);
    println!("outcome:     {}", view.outcome);
    println!("title:       {}", view.title);
    if reveal || !view.body.is_empty() {
        println!("body:        {}", view.body);
    } else {
        println!("body:        (use --reveal for full body)");
    }
}

async fn rebuild(
    connection: &sea_orm::DatabaseConnection,
    repo_id: &str,
    output: &OutputConfig,
) -> CliResult<()> {
    let horizon = memory_horizon(connection).await?;
    let report = do_rebuild(connection, repo_id, horizon)
        .await
        .map_err(|e| map_memory_error("memory rebuild failed", e))?;
    if output.is_json() {
        return emit_json_data("memory", &report, output);
    }
    if output.quiet {
        return Ok(());
    }
    println!("memory projection rebuilt");
    println!("projected:         {}", report.projected);
    println!("horizon_truncated: {}", report.horizon_truncated);
    println!("revoked_count:     {}", report.revoked_count);
    println!("aged_out_count:    {}", report.aged_out_count);
    Ok(())
}

/// Read `memory.horizon` (default 5000), clamped to a sane positive bound.
async fn memory_horizon(connection: &sea_orm::DatabaseConnection) -> CliResult<usize> {
    match ConfigKv::get_with_conn(connection, "memory.horizon").await {
        Ok(Some(entry)) => entry
            .value
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                CliError::failure(format!(
                    "invalid memory.horizon value '{}': expected a positive integer",
                    entry.value
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
            }),
        Ok(None) => Ok(DEFAULT_HORIZON),
        Err(error) => Err(
            CliError::failure(format!("failed to read memory.horizon: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed),
        ),
    }
}

const DEFAULT_HORIZON: usize = 5000;
