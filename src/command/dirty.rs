//! `libra dirty` — advisory dirty-set marks (lore.md §1.1, a Libra
//! extension; Git has no equivalent). Marks paths in the `working_dirty`
//! cache without reading file contents or touching the index: over-reporting
//! is the safe direction, so manual marks never invalidate the cache's scan
//! snapshot. Consumed by `status --cached` / `--check-dirty`; the cache is
//! rebuilt authoritatively by `status --scan`.

use clap::Parser;
use serde::Serialize;

use crate::{
    internal::dirty::DirtyCache,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

pub const DIRTY_EXAMPLES: &str = "\
EXAMPLES:
    libra dirty src/main.rs                Mark a path dirty in the cache (no file reads)
    libra dirty a.txt b.txt                Mark several paths
    libra dirty --list                     Show the cached dirty set and its freshness
    libra status --scan                    Rebuild the cache authoritatively
    libra status --cached                  Consume the cache instead of walking the tree
    libra --json dirty --list              Structured output for agents

NOTES:
    Marks are advisory: they can only make the cached view over-report (safe),
    never hide a change. Nonexistent paths are legal — a deletion IS dirty.
    Default `libra status` never reads or writes the cache.";

/// Mark paths dirty in the dirty-set cache, or list it (Libra extension).
#[derive(Parser, Debug)]
#[command(after_help = DIRTY_EXAMPLES)]
pub struct DirtyArgs {
    /// Paths to mark dirty (repo-relative or cwd-relative; must stay inside
    /// the repository). May not exist — a deletion is dirty too.
    #[clap(required_unless_present = "list")]
    pub paths: Vec<String>,

    /// List the cached dirty set instead of marking.
    #[clap(long, conflicts_with = "paths")]
    pub list: bool,
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
enum DirtyOutput {
    Mark {
        marked: Vec<String>,
        total_cached: usize,
        cache_state: String,
    },
    List {
        entries: Vec<DirtyListEntry>,
        cache_state: String,
        scanned_at: Option<String>,
    },
}

#[derive(Debug, Serialize)]
struct DirtyListEntry {
    path: String,
    kind: String,
    source: String,
    marked_at: String,
    verified_at: Option<String>,
}

async fn cache_state_label() -> String {
    use crate::internal::dirty::{DirtyCache, current_index_fingerprint};
    let Ok(index_path) = crate::utils::path::try_index() else {
        return "missing".to_string();
    };
    let Ok(fingerprint) = current_index_fingerprint(&index_path) else {
        return "missing".to_string();
    };
    let head = crate::internal::head::Head::current_commit()
        .await
        .map(|oid| oid.to_string());
    match DirtyCache::meta().await {
        Ok(meta) => DirtyCache::classify(meta.as_ref(), &fingerprint, head.as_deref())
            .as_str()
            .to_string(),
        Err(_) => "missing".to_string(),
    }
}

pub async fn execute(args: DirtyArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

pub async fn execute_safe(args: DirtyArgs, output: &OutputConfig) -> CliResult<()> {
    if util::require_repo().is_err() {
        return Err(CliError::repo_not_found());
    }
    // Part C W1 (§C.4.1.1): the dirty cache is worktree-scoped (migration
    // 2026072302 + per-scope DirtyCache predicates), so `dirty` runs in any
    // worktree against its own rows.
    if args.list {
        let entries: Vec<DirtyListEntry> = DirtyCache::list()
            .await
            .map_err(|e| {
                CliError::fatal(format!("failed to read the dirty cache: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
            .into_iter()
            .map(|entry| DirtyListEntry {
                path: entry.path,
                kind: entry.kind,
                source: entry.source,
                marked_at: entry.marked_at,
                verified_at: entry.verified_at,
            })
            .collect();
        let meta = DirtyCache::meta().await.ok().flatten();
        let report = DirtyOutput::List {
            entries,
            cache_state: cache_state_label().await,
            scanned_at: meta.and_then(|meta| meta.scanned_at),
        };
        if output.is_json() {
            return emit_json_data("dirty", &report, output);
        }
        if let DirtyOutput::List {
            entries,
            cache_state,
            ..
        } = &report
            && !output.quiet
        {
            for entry in entries {
                println!("{}\t{}\t{}", entry.kind, entry.source, entry.path);
            }
            eprintln!("cache: {cache_state}");
        }
        return Ok(());
    }

    // Validation is enforced INSIDE the owner API: the whole batch is
    // refused if any path escapes the repo root. Nonexistent paths are legal
    // (a deletion IS dirty); no file contents are read, no index writes.
    let workdir_relative: Vec<std::path::PathBuf> =
        args.paths.iter().map(util::to_workdir_path).collect();
    let stored = match DirtyCache::mark_paths(&workdir_relative).await {
        Ok(stored) => stored,
        Err(error @ crate::internal::dirty::MarkError::Escaping(_)) => {
            return Err(CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("dirty marks are repo-relative; pass paths inside the working tree"));
        }
        Err(error) => {
            return Err(
                CliError::fatal(format!("failed to write the dirty cache: {error}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed),
            );
        }
    };
    let total = DirtyCache::list().await.map(|rows| rows.len()).unwrap_or(0);
    let report = DirtyOutput::Mark {
        marked: stored,
        total_cached: total,
        cache_state: cache_state_label().await,
    };
    if output.is_json() {
        return emit_json_data("dirty", &report, output);
    }
    if let DirtyOutput::Mark { marked, .. } = &report
        && !output.quiet
    {
        println!("marked {} path(s) dirty", marked.len());
    }
    Ok(())
}
