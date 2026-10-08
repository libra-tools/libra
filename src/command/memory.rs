//! `libra memory` — the deterministic Agent development-history projection.
//!
//! DM-03 registers the public command surface only; the subcommand behavior,
//! stable error codes and docs are delivered by DM-11. Each subcommand is a
//! no-op placeholder here (it must not write an `operation` row, verified by
//! `memory_writes_no_operation_row`). No user-visible behavior change yet.

use clap::{Parser, Subcommand};

use crate::utils::error::CliResult;

pub const MEMORY_EXAMPLES: &str = "\
EXAMPLES:
    libra memory status        Show the projection freshness / horizon state
    libra memory list          List derived memory episodes
    libra memory show          Show a single derived memory episode
    libra memory rebuild       Rebuild the zero-authority projection";

/// Inspect / rebuild the deterministic Agent development-history projection.
#[derive(Parser, Debug)]
#[command(after_help = MEMORY_EXAMPLES)]
pub struct MemoryArgs {
    #[command(subcommand)]
    pub command: MemoryCommand,
}

/// Placeholder subcommand names; behavior is DM-11.
#[derive(Subcommand, Debug)]
pub enum MemoryCommand {
    /// Report projection freshness / horizon state (DM-11).
    Status,
    /// List derived memory episodes (DM-11).
    List,
    /// Show a single derived memory episode (DM-11).
    Show {
        /// The Episode id.
        #[arg(value_name = "EPISODE_ID")]
        episode_id: String,
        /// Reveal secrets / long bodies.
        #[arg(long)]
        reveal: bool,
    },
    /// Rebuild the zero-authority projection (DM-11).
    Rebuild,
}

/// No-op placeholder execution: accepts the subcommand and returns without
/// writing an `operation` row (DM-11 fills in the behavior).
pub async fn execute(_args: MemoryArgs) -> CliResult<()> {
    Ok(())
}
