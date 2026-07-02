//! `libra commit-tree` — plumbing (lore.md §1.15): wrap an existing tree +
//! parents + message into a commit OBJECT, with zero side effects on the
//! index, worktree, HEAD, or any ref (publishing is the caller's job via
//! `update-ref`, whose protect/archive policy already guards head writes).
//! Together with `update-index --index-file` / `write-tree --index-file`
//! this closes the Git-idiomatic off-worktree revision-composition loop.
//!
//! Intentional differences from `git commit-tree` (documented in
//! COMPATIBILITY.md): empty messages are refused (the repo-wide
//! D-empty-message rule — replaying foreign history with empty messages
//! needs porcelain-side allowances first); commits are always UNSIGNED in
//! v1 (git honors `commit.gpgsign` here — vault signing is a recorded
//! follow-up); `-m`/`-F` may be combined but paragraphs append in group
//! order (all `-m` first, then all `-F`) rather than argv interleaving;
//! date-override envs (GIT_AUTHOR_DATE) are not honored yet, so OIDs are
//! not reproducible across runs (recorded follow-up).

use clap::Parser;
use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use serde::Serialize;

use crate::{
    command::{get_target_commit, load_object, save_object},
    common_utils::format_commit_msg,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

pub const COMMIT_TREE_EXAMPLES: &str = "\
EXAMPLES:
    libra commit-tree <tree> -m 'message'             Root commit from a tree
    libra commit-tree <tree> -p HEAD -m 'message'     One parent
    libra commit-tree <tree> -p A -p B -m 'merge'     Merge commit
    echo 'msg' | libra commit-tree <tree> -p HEAD     Message from stdin
    libra commit-tree HEAD -m 'reuse tree'            Commit-ish peels to its tree

NOTES:
    Pure plumbing: writes ONE commit object and prints its OID — the index,
    worktree, HEAD, and refs are untouched (publish with `libra update-ref`).
    Compose the tree first: `update-index --index-file <scratch> --cacheinfo
    … && write-tree --index-file <scratch>`.";

/// Create a commit object from an existing tree (plumbing; no ref updates).
#[derive(Parser, Debug)]
#[command(after_help = COMMIT_TREE_EXAMPLES)]
pub struct CommitTreeArgs {
    /// The tree to commit (a tree OID; commit-ish/ref/tag peel to their tree).
    pub tree: String,

    /// Parent commit (repeatable; order preserved; duplicates ignored with a
    /// warning, like Git).
    #[arg(short = 'p', value_name = "PARENT")]
    pub parents: Vec<String>,

    /// Commit message paragraph (repeatable; paragraphs joined by blank lines).
    #[arg(short = 'm', value_name = "MESSAGE")]
    pub message: Vec<String>,

    /// Read message paragraph(s) from a file (`-` = stdin; repeatable).
    #[arg(short = 'F', value_name = "FILE")]
    pub file: Vec<String>,
}

#[derive(Debug, Serialize)]
struct CommitTreeOutput {
    commit: String,
}

/// Assemble the message: `-m` paragraphs, then `-F` paragraphs (group order —
/// argv interleaving is not preserved, documented); bare piped stdin when
/// neither is given. A TTY with no message source is a usage error
/// (agent-safe: never hangs waiting for input).
fn assemble_message(args: &CommitTreeArgs) -> CliResult<String> {
    use std::io::{IsTerminal, Read};
    let mut paragraphs: Vec<String> = Vec::new();
    for m in &args.message {
        paragraphs.push(m.clone());
    }
    for f in &args.file {
        let content = if f == "-" {
            let mut buffer = String::new();
            std::io::stdin().read_to_string(&mut buffer).map_err(|e| {
                CliError::fatal(format!("failed to read the message from stdin: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            buffer
        } else {
            std::fs::read_to_string(f).map_err(|e| {
                CliError::fatal(format!("failed to read message file '{f}': {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
        };
        paragraphs.push(content);
    }
    if paragraphs.is_empty() {
        if std::io::stdin().is_terminal() {
            return Err(CliError::command_usage(
                "no message given (use -m/-F, or pipe the message on stdin)",
            )
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        let mut buffer = String::new();
        std::io::stdin().read_to_string(&mut buffer).map_err(|e| {
            CliError::fatal(format!("failed to read the message from stdin: {e}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        paragraphs.push(buffer);
    }
    // BYTE-EXACT single-source rule: one -m / one -F / bare stdin hashes the
    // content verbatim (only a single final newline is ensured — intentional
    // trailing blank lines survive). Multi-paragraph joins trim each part's
    // trailing newlines before the blank-line join (documented).
    let message = if paragraphs.len() == 1 {
        let single = &paragraphs[0];
        if single.ends_with('\n') {
            single.clone()
        } else {
            format!("{single}\n")
        }
    } else {
        let joined = paragraphs
            .iter()
            .map(|paragraph| paragraph.trim_end_matches('\n'))
            .collect::<Vec<_>>()
            .join("\n\n");
        format!("{joined}\n")
    };
    if message.trim().is_empty() {
        // Repo-wide D-empty-message rule (intentional difference from git
        // plumbing, which accepts empty messages).
        return Err(CliError::command_usage("commit message must not be empty")
            .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    Ok(message)
}

pub async fn execute(args: CommitTreeArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

pub async fn execute_safe(args: CommitTreeArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let tree_id = crate::command::read_tree::resolve_tree_ish(&args.tree).await?;

    // Parents: each must resolve AND load as a commit; duplicates are
    // ignored with a warning (matching git's behavior, as a warning).
    let mut parents: Vec<ObjectHash> = Vec::new();
    for parent in &args.parents {
        let oid = get_target_commit(parent).await.map_err(|_| {
            CliError::fatal(format!("cannot resolve parent '{parent}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        let _: Commit = load_object(&oid).map_err(|error| {
            CliError::fatal(format!("parent '{parent}' is not a commit: {error}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        if parents.contains(&oid) {
            crate::utils::error::emit_warning(format!("duplicate parent {oid} ignored"));
        } else {
            parents.push(oid);
        }
    }

    let message = assemble_message(&args)?;
    let (author, committer, _identity) =
        crate::command::commit::create_commit_signatures(None).await?;

    // The blank-line header/body separator is the LEADING newline inside the
    // message field (git-internal's Commit::to_data appends the message
    // directly after the committer line) — format_commit_msg provides it,
    // exactly like the porcelain commit path.
    let commit = Commit::new(
        author,
        committer,
        tree_id,
        parents,
        &format_commit_msg(&message, None),
    );
    save_object(&commit, &commit.id).map_err(|error| {
        CliError::fatal(format!("failed to write the commit object: {error}"))
            .with_stable_code(StableErrorCode::IoWriteFailed)
    })?;

    if output.is_json() {
        return emit_json_data(
            "commit-tree",
            &CommitTreeOutput {
                commit: commit.id.to_string(),
            },
            output,
        );
    }
    if !output.quiet {
        println!("{}", commit.id);
    }
    Ok(())
}
