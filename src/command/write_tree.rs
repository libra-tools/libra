//! `libra write-tree` — write the current index out as a (nested) tree object
//! and print its object id. Plumbing companion to `read-tree`.

use clap::Parser;
use git_internal::internal::index::Index;
use serde::Serialize;

use crate::{
    internal::tree_plumbing,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        path, util,
    },
};

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const WRITE_TREE_EXAMPLES: &str = "\
EXAMPLES:
    libra write-tree              Write the index as a tree and print its object id
    libra --json write-tree       Structured JSON output for agents";

/// Write the current index out as a tree object and print its object id.
#[derive(Parser, Debug)]
#[command(after_help = WRITE_TREE_EXAMPLES)]
pub struct WriteTreeArgs {
    /// Use this index file instead of `.libra/index` (a Libra flag standing
    /// in for Git's GIT_INDEX_FILE env). A missing file acts as an empty
    /// index (yielding the canonical empty tree).
    #[clap(long = "index-file", value_name = "PATH")]
    pub index_file: Option<String>,
}

#[derive(Debug, Serialize)]
struct WriteTreeOutput {
    tree: String,
}

pub async fn execute(args: WriteTreeArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Writes `.libra/index` as a nested tree (preserving file
/// modes and the repository hash kind) and reports the root tree id. An empty
/// index yields the canonical empty tree.
pub async fn execute_safe(args: WriteTreeArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let index_path = args
        .index_file
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(path::index);
    // GIT_INDEX_FILE parity: a missing scratch index is an EMPTY index (the
    // canonical empty tree), only for explicit --index-file targets.
    let index = if args.index_file.is_some() && !index_path.exists() {
        Index::new()
    } else {
        Index::load(&index_path).map_err(|error| {
            CliError::fatal(format!("failed to load index: {error}"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?
    };

    let tree = tree_plumbing::write_tree_from_index(&index).map_err(|error| {
        CliError::fatal(format!("failed to write tree from index: {error}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;

    if output.is_json() {
        emit_json_data(
            "write-tree",
            &WriteTreeOutput {
                tree: tree.to_string(),
            },
            output,
        )
    } else {
        if !output.quiet {
            println!("{tree}");
        }
        Ok(())
    }
}
