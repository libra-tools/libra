//! Implements `show-ref` to list all refs (branches, tags) with their object IDs.

use clap::Parser;
use serde::Serialize;

use crate::{
    command::{
        show_ref_check, show_ref_deref, show_ref_exclude_existing,
        show_ref_render::{ShowRefRenderOptions, render_show_ref_entries},
    },
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        head::Head,
        tag::{self, ListTagError},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::OutputConfig,
    },
};

/// `--help` examples shown in `libra show-ref --help` output.
///
/// `show-ref` lists local references with their object hashes. The
/// banner pins the all-refs default, `--heads` / `--tags` scope
/// filters, the `--head` opt-in for including HEAD, `-s` for hash-only
/// output, a Git-style path-segment pattern filter, and a JSON variant
/// for agents so users see all supported forms without reading the
/// design doc. Cross-cutting `--help` EXAMPLES rollout per
/// `docs/development/commands/_general.md` item B.
pub const SHOW_REF_EXAMPLES: &str = "\
EXAMPLES:
    libra show-ref                   List all local refs with their object hashes
    libra show-ref --heads           List only branches (refs/heads/)
    libra show-ref --branches        Alias for --heads
    libra show-ref --branches --no-branches
                                     Reset branch-only filtering back to the default branch+tag listing
    libra show-ref --tags            List only tags (refs/tags/)
    libra show-ref --head            Include HEAD in the output
    libra show-ref -s --heads        Print branch hashes only (one per line, scripting-friendly)
    libra show-ref --abbrev=12       Abbreviate object IDs to 12 hex digits
    libra show-ref --abbrev=12 --no-abbrev --heads
                                     Reset abbreviated display back to full object IDs
    libra show-ref -d --tags         Peel annotated tags and show refs/tags/<name>^{} lines
    libra show-ref --verify refs/heads/main
                                     Verify an exact refname and print it
    libra show-ref --exists refs/heads/main
                                     Check whether an exact refname exists
    libra show-ref --exclude-existing
                                     Filter stdin to refs that do not exist locally
    libra show-ref main              Filter refs ending in the path segment 'main'
    libra show-ref --json --heads    Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = SHOW_REF_EXAMPLES)]
pub struct ShowRefArgs {
    /// Show only branches (refs/heads/); --branches is a Git-compatible alias
    #[clap(long, visible_alias = "branches", overrides_with = "no_heads")]
    pub heads: bool,

    /// Reset --heads / --branches scope filtering
    #[clap(long = "no-branches", overrides_with = "heads")]
    pub no_heads: bool,

    /// Show only tags (refs/tags/)
    #[clap(long, overrides_with = "no_tags")]
    pub tags: bool,

    /// Reset --tags scope filtering
    #[clap(long = "no-tags", overrides_with = "tags")]
    pub no_tags: bool,

    /// Include HEAD in the output
    #[clap(long = "head", overrides_with = "no_head")]
    pub head: bool,

    /// Reset --head so HEAD is omitted unless another mode includes it
    #[clap(long = "no-head", overrides_with = "head")]
    pub no_head: bool,

    /// Only show the object hash, optionally shortened to N hex digits
    #[clap(
        short = 's',
        long = "hash",
        visible_alias = "no-hash",
        value_name = "N",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "0"
    )]
    pub hash: Option<usize>,

    /// Abbreviate object IDs to N hex digits, or 7 when no value is supplied
    #[clap(
        long = "abbrev",
        value_name = "N",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "7",
        overrides_with = "no_abbrev"
    )]
    pub abbrev: Option<usize>,

    /// Reset --abbrev so object IDs are displayed at full width
    #[clap(long = "no-abbrev", overrides_with = "abbrev")]
    pub no_abbrev: bool,

    /// Dereference annotated tags and include peeled refs/tags/<name>^{} entries
    #[clap(short = 'd', long = "dereference", overrides_with = "no_dereference")]
    pub dereference: bool,

    /// Reset --dereference so annotated tags are not peeled
    #[clap(long = "no-dereference", overrides_with = "dereference")]
    pub no_dereference: bool,

    /// Verify exact refnames instead of pattern filtering
    #[clap(long, conflicts_with = "exists", overrides_with = "no_verify")]
    pub verify: bool,

    /// Reset --verify and return to normal pattern filtering
    #[clap(long = "no-verify", overrides_with = "verify")]
    pub no_verify: bool,

    /// Check whether exactly one ref exists without printing it
    #[clap(long, conflicts_with = "verify", overrides_with = "no_exists")]
    pub exists: bool,

    /// Reset --exists and return to normal ref listing
    #[clap(long = "no-exists", overrides_with = "exists")]
    pub no_exists: bool,

    /// Filter stdin to refs that do not already exist locally
    #[clap(
        long = "exclude-existing",
        value_name = "PATTERN",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "",
        conflicts_with_all = ["verify", "exists"]
    )]
    pub exclude_existing: Option<String>,

    /// Filter refs by path-segment suffix pattern
    pub pattern: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct ShowRefEntry {
    pub(crate) hash: String,
    pub(crate) refname: String,
}

pub async fn execute(args: ShowRefArgs) -> Result<(), String> {
    execute_safe(args, &OutputConfig::default())
        .await
        .map_err(|err| err.render())
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Lists all refs (branches, tags) with their object IDs.
pub async fn execute_safe(args: ShowRefArgs, output: &OutputConfig) -> CliResult<()> {
    if let Some(pattern) = args.exclude_existing.as_deref() {
        return show_ref_exclude_existing::execute(
            if pattern.is_empty() {
                None
            } else {
                Some(pattern)
            },
            output,
        )
        .await;
    }
    if args.exists {
        return show_ref_check::execute_exists(&args, output).await;
    }
    if args.verify {
        return show_ref_check::execute_verify(&args, output).await;
    }

    let entries = collect_show_ref_entries(&args).await?;
    render_show_ref_entries(
        &entries,
        ShowRefRenderOptions::from_args(args.hash, args.abbrev),
        output,
    )
}

fn show_ref_branch_store_error(context: &str, error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => {
            CliError::fatal(format!("failed to {context}: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        other => CliError::fatal(format!("failed to {context}: {other}"))
            .with_stable_code(StableErrorCode::RepoCorrupt),
    }
}

fn show_ref_tag_list_error(error: ListTagError) -> CliError {
    let stable_code = match error {
        ListTagError::Query(_) => StableErrorCode::IoReadFailed,
        ListTagError::MissingCommit { .. }
        | ListTagError::InvalidObjectHash { .. }
        | ListTagError::MissingName
        | ListTagError::LoadObject { .. } => StableErrorCode::RepoCorrupt,
    };

    CliError::fatal(format!("failed to list tags: {error}")).with_stable_code(stable_code)
}

async fn collect_show_ref_entries(args: &ShowRefArgs) -> CliResult<Vec<ShowRefEntry>> {
    let show_heads = args.heads || !args.tags;
    let show_tags = args.tags || !args.heads;
    let mut entries =
        collect_raw_show_ref_entries(args.head, show_heads, show_tags, args.dereference).await?;
    if !args.pattern.is_empty() {
        entries.retain(|entry| {
            entry.refname == "HEAD"
                || args
                    .pattern
                    .iter()
                    .any(|p| show_ref_pattern_matches(&entry.refname, p))
        });
    }

    if entries.is_empty() {
        return Err(CliError::failure("no matching refs found")
            .with_stable_code(StableErrorCode::CliInvalidTarget));
    }

    Ok(entries)
}

pub(crate) async fn collect_raw_show_ref_entries(
    include_head: bool,
    show_heads: bool,
    show_tags: bool,
    dereference_tags: bool,
) -> CliResult<Vec<ShowRefEntry>> {
    let mut entries = Vec::new();

    if include_head
        && let Some(hash) = Head::current_commit_result()
            .await
            .map_err(|error| show_ref_branch_store_error("resolve HEAD", error))?
    {
        entries.push(ShowRefEntry {
            hash: hash.to_string(),
            refname: String::from("HEAD"),
        });
    }

    if show_heads {
        let branches = Branch::list_branches_result(None)
            .await
            .map_err(|error| show_ref_branch_store_error("list branches", error))?;
        for branch in branches {
            entries.push(ShowRefEntry {
                hash: branch.commit.to_string(),
                refname: format!("refs/heads/{}", branch.name),
            });
        }

        let remotes = ConfigKv::all_remote_configs().await.map_err(|error| {
            CliError::fatal(format!("failed to list remotes: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        for remote in remotes {
            let branches = Branch::list_branches_result(Some(&remote.name))
                .await
                .map_err(|error| {
                    show_ref_branch_store_error(
                        &format!("list remote-tracking branches for '{}'", remote.name),
                        error,
                    )
                })?;
            for branch in &branches {
                entries.push(ShowRefEntry {
                    hash: branch.commit.to_string(),
                    refname: remote_refname(&remote.name, &branch.name),
                });
            }

            // A cached remote HEAD is a symbolic ref stored separately from
            // remote-tracking branches. Resolve it to the target branch so
            // `show-ref --verify refs/remotes/<remote>/HEAD` observes the same
            // ref that `for-each-ref` and `remote set-head` expose.
            if let Some(Head::Branch(target)) = Head::remote_current(&remote.name).await {
                let target_ref = remote_refname(&remote.name, &target);
                if let Some(branch) = branches
                    .iter()
                    .find(|branch| remote_refname(&remote.name, &branch.name) == target_ref)
                {
                    entries.push(ShowRefEntry {
                        hash: branch.commit.to_string(),
                        refname: format!("refs/remotes/{}/HEAD", remote.name),
                    });
                }
            }
        }
    }

    if show_tags {
        let tag_list = tag::list().await.map_err(show_ref_tag_list_error)?;
        for t in tag_list {
            entries.extend(show_ref_deref::tag_entries(t, dereference_tags).await?);
        }
    }

    Ok(entries)
}

fn show_ref_pattern_matches(refname: &str, pattern: &str) -> bool {
    let base_refname = refname.strip_suffix("^{}").unwrap_or(refname);
    base_refname == pattern
        || base_refname
            .strip_suffix(pattern)
            .is_some_and(|prefix| prefix.ends_with('/'))
}

fn remote_refname(remote: &str, branch_name: &str) -> String {
    if branch_name.starts_with("refs/remotes/") {
        return branch_name.to_string();
    }
    format!("refs/remotes/{remote}/{branch_name}")
}
