//! `libra merge-base` — print the best common ancestor(s) of two commits, a
//! focused subset of `git merge-base`. Backed by the single LCA implementation
//! in [`crate::internal::merge_base`], which `diff A...B` also uses.

use clap::Parser;
use serde::Serialize;

use crate::{
    internal::merge_base,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const MERGE_BASE_EXAMPLES: &str = "\
EXAMPLES:
    libra merge-base main feature          Print the best common ancestor
    libra merge-base --all main feature    Print every lowest common ancestor
    libra merge-base --is-ancestor A B     Exit 0 if A is an ancestor of B, else 1
    libra --json merge-base main feature   Structured { bases: [...] }";

/// Find the best common ancestor(s) of two commits.
#[derive(Parser, Debug)]
#[command(after_help = MERGE_BASE_EXAMPLES)]
pub struct MergeBaseArgs {
    /// Print all lowest common ancestors, not just one.
    #[clap(long)]
    pub all: bool,

    /// Test whether the first commit is an ancestor of the second (exit 0/1)
    /// instead of printing a base.
    #[clap(long = "is-ancestor")]
    pub is_ancestor: bool,

    /// The two commits (branch, tag, `HEAD`, or object id).
    #[clap(value_name = "COMMIT")]
    pub commits: Vec<String>,
}

#[derive(Debug, Serialize)]
struct MergeBaseOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    bases: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    is_ancestor: Option<bool>,
}

pub async fn execute(args: MergeBaseArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Exit 0 when a base is found / the ancestry holds; exit 1
/// when there is no common ancestor or the ancestry does not hold; exit 128 on
/// usage errors or unresolvable commits.
pub async fn execute_safe(args: MergeBaseArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let usage = |message: String| {
        CliError::command_usage(message)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(128)
    };

    if args.is_ancestor && args.all {
        return Err(usage(
            "--is-ancestor and --all cannot be combined".to_string(),
        ));
    }
    if args.commits.len() != 2 {
        return Err(usage(format!(
            "merge-base requires exactly two commits, got {}",
            args.commits.len()
        )));
    }

    let a = resolve_commit(&args.commits[0]).await?;
    let b = resolve_commit(&args.commits[1]).await?;

    let internal_err = |error: merge_base::MergeBaseError| match &error {
        merge_base::MergeBaseError::Shallow(shallow) => CliError::fatal(error.to_string())
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoCorrupt)
            .with_hint(shallow.hint()),
        merge_base::MergeBaseError::Load(_) => CliError::fatal(error.to_string())
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::RepoStateInvalid),
    };

    if args.is_ancestor {
        let yes = merge_base::is_ancestor(&a, &b).map_err(internal_err)?;
        if output.is_json() {
            emit_json_data(
                "merge-base",
                &MergeBaseOutput {
                    bases: None,
                    is_ancestor: Some(yes),
                },
                output,
            )?;
        }
        return if yes {
            Ok(())
        } else {
            Err(CliError::silent_exit(1))
        };
    }

    let bases = merge_base::merge_bases(&a, &b).map_err(internal_err)?;
    let printed: Vec<String> = if args.all {
        bases.iter().map(|id| id.to_string()).collect()
    } else {
        bases.iter().take(1).map(|id| id.to_string()).collect()
    };

    if output.is_json() {
        emit_json_data(
            "merge-base",
            &MergeBaseOutput {
                bases: Some(printed.clone()),
                is_ancestor: None,
            },
            output,
        )?;
    } else {
        for id in &printed {
            println!("{id}");
        }
    }

    // No common ancestor: Git exits 1 with no output (128 is reserved for bad
    // revisions / usage errors).
    if printed.is_empty() {
        return Err(CliError::silent_exit(1));
    }
    Ok(())
}

/// Resolve a commit-ish to its object id, mapping failures to a 128 exit.
async fn resolve_commit(name: &str) -> CliResult<git_internal::hash::ObjectHash> {
    util::get_commit_base(name).await.map_err(|error| {
        CliError::fatal(format!("not a valid commit '{name}': {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    })
}
