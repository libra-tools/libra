//! `libra diff-tree` / `diff-index` / `diff-files` — plumbing entry points that
//! reuse the one `diff` engine rather than forking three diff implementations.
//!
//! Each command translates its arguments into the equivalent `diff` invocation
//! (built by re-parsing a synthetic argv into [`DiffArgs`]) and hands it to
//! [`crate::command::diff::execute_safe`]. Output and whitespace handling share
//! the porcelain engine, while plumbing exit codes and rename defaults differ.

use clap::Parser;

use crate::{
    command::diff::DiffArgs,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
    },
};

pub const DIFF_TREE_EXAMPLES: &str = "\
EXAMPLES:
    libra diff-tree <tree-a> <tree-b>        Diff between two trees/commits
    libra diff-tree <a> <b> -- src/          Limit the diff to a path";

pub const DIFF_INDEX_EXAMPLES: &str = "\
EXAMPLES:
    libra diff-index HEAD                    Diff a tree against the working tree
    libra diff-index HEAD -- src/            Limit the diff to a path";

pub const DIFF_FILES_EXAMPLES: &str = "\
EXAMPLES:
    libra diff-files                         Diff the index against the working tree
    libra diff-files -- src/                 Limit the diff to a path";

/// Diff between two trees (`git diff-tree <a> <b>`).
#[derive(Parser, Debug)]
#[command(after_help = DIFF_TREE_EXAMPLES)]
pub struct DiffTreeArgs {
    /// The "old" tree-ish.
    #[clap(value_name = "TREE-A")]
    pub tree_a: String,
    /// The "new" tree-ish.
    #[clap(value_name = "TREE-B")]
    pub tree_b: String,
    /// Optional path limiters.
    #[clap(value_name = "PATH", last = true)]
    pub paths: Vec<String>,
}

/// Diff a tree against the working tree (`git diff-index <tree>`).
#[derive(Parser, Debug)]
#[command(after_help = DIFF_INDEX_EXAMPLES)]
pub struct DiffIndexArgs {
    /// Compare against the index instead of the working tree. Not yet
    /// supported — use `diff --staged` for HEAD vs the index.
    #[clap(long, visible_alias = "cached")]
    pub cached: bool,
    /// The tree-ish to compare.
    #[clap(value_name = "TREE")]
    pub tree: String,
    /// Optional path limiters.
    #[clap(value_name = "PATH", last = true)]
    pub paths: Vec<String>,
}

/// Diff the index against the working tree (`git diff-files`).
#[derive(Parser, Debug)]
#[command(after_help = DIFF_FILES_EXAMPLES)]
pub struct DiffFilesArgs {
    /// Optional path limiters.
    #[clap(value_name = "PATH", last = true)]
    pub paths: Vec<String>,
}

/// Build a `DiffArgs` from a synthetic argv and run the shared diff engine.
///
/// Plumbing diff commands follow Git's plumbing exit convention — exit 1 when
/// there are differences, 0 when clean — so `--exit-code` is always on (unlike
/// the porcelain `diff`, which exits 0 unless `--exit-code` is requested).
async fn run_via_diff(mut argv: Vec<String>, output: &OutputConfig) -> CliResult<()> {
    // `diff.renames` is porcelain-only in Git. Keep the shared engine's default
    // rename detection out of plumbing, and bypass invalid porcelain config.
    argv.insert(1, "--no-renames".to_string());
    argv.insert(1, "--exit-code".to_string());
    // Plumbing keeps its historical 128 override on argv parse failures.
    delegate_to_diff(argv, output, Some(128)).await
}

/// Parse a synthetic diff argv and dispatch into the diff engine — the shared
/// delegation tail for the diff-tree plumbing wrappers and `branch diff`
/// (lore.md 1.12). `parse_error_exit_override` preserves the plumbing
/// convention (128); `None` keeps the usage default (129).
pub(crate) async fn delegate_to_diff(
    argv: Vec<String>,
    output: &OutputConfig,
    parse_error_exit_override: Option<i32>,
) -> CliResult<()> {
    let args = DiffArgs::try_parse_from(argv).map_err(|error| {
        let mapped = CliError::command_usage(format!("invalid diff request: {error}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments);
        match parse_error_exit_override {
            Some(code) => mapped.with_exit_code(code),
            None => mapped,
        }
    })?;
    crate::command::diff::execute_safe(args, output).await
}

/// Append `-- <paths...>` to a diff argv when path limiters are present.
pub(crate) fn push_paths(argv: &mut Vec<String>, paths: &[String]) {
    if !paths.is_empty() {
        argv.push("--".to_string());
        argv.extend(paths.iter().cloned());
    }
}

pub async fn execute_tree(args: DiffTreeArgs) {
    if let Err(err) = execute_tree_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_tree_safe(args: DiffTreeArgs, output: &OutputConfig) -> CliResult<()> {
    let mut argv = vec![
        "diff".to_string(),
        "--old".to_string(),
        args.tree_a,
        "--new".to_string(),
        args.tree_b,
    ];
    push_paths(&mut argv, &args.paths);
    run_via_diff(argv, output).await
}

pub async fn execute_index(args: DiffIndexArgs) {
    if let Err(err) = execute_index_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_index_safe(args: DiffIndexArgs, output: &OutputConfig) -> CliResult<()> {
    if args.cached {
        return Err(CliError::command_usage(
            "diff-index --cached is not yet supported; use `diff --staged` for HEAD vs the index"
                .to_string(),
        )
        .with_stable_code(StableErrorCode::Unsupported)
        .with_exit_code(128));
    }
    // Compare the tree to the working tree: `diff --old <tree>`.
    let mut argv = vec!["diff".to_string(), "--old".to_string(), args.tree];
    push_paths(&mut argv, &args.paths);
    run_via_diff(argv, output).await
}

pub async fn execute_files(args: DiffFilesArgs) {
    if let Err(err) = execute_files_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_files_safe(args: DiffFilesArgs, output: &OutputConfig) -> CliResult<()> {
    // The default `diff` already compares the index to the working tree.
    let mut argv = vec!["diff".to_string()];
    push_paths(&mut argv, &args.paths);
    run_via_diff(argv, output).await
}
