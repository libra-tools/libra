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
    /// Recurse into sub-trees. Libra always recurses, so this is accepted for
    /// Git plumbing parity and is a no-op.
    #[clap(short = 'r')]
    pub recurse: bool,
    /// Emit a patch instead of the default raw record.
    #[clap(short = 'p', long = "patch")]
    pub patch: bool,
    /// Suppress the diff body (raw/patch) entirely.
    #[clap(short = 's', long = "no-patch")]
    pub no_patch: bool,
    /// Force the raw record (the default when no output-mode flag is given).
    #[clap(long = "raw")]
    pub raw: bool,
    /// Show only changed path names.
    #[clap(long = "name-only")]
    pub name_only: bool,
    /// Show changed path names with status letters.
    #[clap(long = "name-status")]
    pub name_status: bool,
    /// Exit 1 when there are differences instead of the plumbing default 0
    /// (git's `--exit-code`).
    #[clap(long = "exit-code")]
    pub exit_code: bool,
    /// Suppress all output and exit 1 when there are differences (git's
    /// `--quiet`).
    #[clap(long)]
    pub quiet: bool,
    /// Combined-diff spelling. Libra renders the unmerged/combined form for
    /// merge commits through the shared engine; the flag is otherwise accepted.
    #[clap(short = 'c', long = "cc")]
    pub cc: bool,
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
    /// Compare against the index instead of the working tree.
    #[clap(long, visible_alias = "cached")]
    pub cached: bool,
    /// Emit a patch instead of the default raw record.
    #[clap(short = 'p', long = "patch")]
    pub patch: bool,
    /// Suppress the diff body (raw/patch) entirely.
    #[clap(short = 's', long = "no-patch")]
    pub no_patch: bool,
    /// Force the raw record (the default when no output-mode flag is given).
    #[clap(long = "raw")]
    pub raw: bool,
    /// Show only changed path names.
    #[clap(long = "name-only")]
    pub name_only: bool,
    /// Show changed path names with status letters.
    #[clap(long = "name-status")]
    pub name_status: bool,
    /// Exit 1 when there are differences instead of the plumbing default 0.
    #[clap(long = "exit-code")]
    pub exit_code: bool,
    /// Suppress all output and exit 1 when there are differences.
    #[clap(long)]
    pub quiet: bool,
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
    /// Emit a patch instead of the default raw record.
    #[clap(short = 'p', long = "patch")]
    pub patch: bool,
    /// Suppress the diff body (raw/patch) entirely.
    #[clap(short = 's', long = "no-patch")]
    pub no_patch: bool,
    /// Force the raw record (the default when no output-mode flag is given).
    #[clap(long = "raw")]
    pub raw: bool,
    /// Show changed file names only.
    #[clap(long = "name-only")]
    pub name_only: bool,
    /// Show changed file names with their status letter (`A`/`M`/`D`/`R`).
    #[clap(long = "name-status")]
    pub name_status: bool,
    /// Exit 1 when there are differences instead of the plumbing default 0.
    #[clap(long = "exit-code")]
    pub exit_code: bool,
    /// Suppress all output and exit 1 when there are differences.
    #[clap(long)]
    pub quiet: bool,
    /// Optional path limiters.
    #[clap(value_name = "PATH", last = true)]
    pub paths: Vec<String>,
}

/// Append the output-mode flags shared by the diff plumbing wrappers. Git's
/// plumbing default is raw, so raw is selected unless an explicit mode flag
/// asks for patch, names, or no output at all.
fn push_output_mode(
    argv: &mut Vec<String>,
    patch: bool,
    no_patch: bool,
    raw: bool,
    name_only: bool,
    name_status: bool,
) {
    if name_only {
        argv.push("--name-only".to_string());
    } else if name_status {
        argv.push("--name-status".to_string());
    } else if no_patch {
        argv.push("--no-patch".to_string());
    } else if patch || raw {
        // Patch is the shared engine's default. An explicit `--raw` is appended
        // once below, including when it is combined with `-p`.
    } else {
        argv.push("--raw".to_string());
    }
    // An explicit `--raw` alongside `-p` appends a raw record after the patch.
    // Do not append it again when the default branch already selected raw.
    if raw && !name_only && !name_status && !no_patch {
        argv.push("--raw".to_string());
    }
}

/// Build a `DiffArgs` from a synthetic argv and run the shared diff engine.
///
/// Plumbing diff commands follow Git's plumbing exit convention: exit 0 by
/// default even with differences; `--exit-code`/`--quiet` turn differences
/// into exit 1 (ADR-PL-09, issues/479 DF-01). The porcelain `diff` keeps its
/// own 0-unless-requested convention.
async fn run_via_diff(
    mut argv: Vec<String>,
    output: &OutputConfig,
    differences_exit_one: bool,
    stat_only: bool,
    plumbing_quiet: bool,
) -> CliResult<()> {
    // `diff.renames` is porcelain-only in Git. Keep the shared engine's default
    // rename detection out of plumbing, and bypass invalid porcelain config.
    argv.insert(1, "--no-renames".to_string());
    if differences_exit_one {
        argv.insert(1, "--exit-code".to_string());
    }
    if plumbing_quiet {
        argv.insert(1, "--plumbing-quiet".to_string());
    }
    if stat_only {
        argv.insert(1, "--plumbing-stat-only".to_string());
    }
    // Invalid synthetic arguments are usage errors (`LBR-CLI-002`, exit 129).
    // Exit 128 stays reserved for "not inside a repository".
    delegate_to_diff(argv, output, None).await
}

/// Parse a synthetic diff argv and dispatch into the diff engine — the shared
/// delegation tail for the diff-tree plumbing wrappers and `branch diff`
/// (lore.md 1.12). `parse_error_exit_override` replaces the usage default
/// (129) when a caller needs another code. Plumbing diff passes `None`, so a
/// bad synthetic argv stays 129. Exit 128 is only "not inside a repository".
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
    push_output_mode(
        &mut argv,
        args.patch,
        args.no_patch || args.quiet,
        args.raw,
        args.name_only,
        args.name_status,
    );
    push_paths(&mut argv, &args.paths);
    // diff-tree compares two tree-ishes: no worktree side, content-based.
    run_via_diff(
        argv,
        output,
        args.exit_code || args.quiet,
        false,
        args.quiet,
    )
    .await
}

pub async fn execute_index(args: DiffIndexArgs) {
    if let Err(err) = execute_index_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_index_safe(args: DiffIndexArgs, output: &OutputConfig) -> CliResult<()> {
    // Compare the tree to the index (`--cached`) or the working tree.
    let mut argv = vec!["diff".to_string(), "--old".to_string(), args.tree];
    if args.cached {
        argv.push("--staged".to_string());
    }
    push_output_mode(
        &mut argv,
        args.patch,
        args.no_patch || args.quiet,
        args.raw,
        args.name_only,
        args.name_status,
    );
    push_paths(&mut argv, &args.paths);
    run_via_diff(
        argv,
        output,
        args.exit_code || args.quiet,
        !args.cached,
        args.quiet,
    )
    .await
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
    push_output_mode(
        &mut argv,
        args.patch,
        args.no_patch || args.quiet,
        args.raw,
        args.name_only,
        args.name_status,
    );
    push_paths(&mut argv, &args.paths);
    run_via_diff(argv, output, args.exit_code || args.quiet, true, args.quiet).await
}
