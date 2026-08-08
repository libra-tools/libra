//! `libra revision` — revision ordinal index (lore.md §1.16, a Libra
//! extension porting Lore's `revision find number`). Git has no equivalent
//! surface; the nearest analogues are `git rev-list --first-parent --count`
//! (reverse) and `<tip>~<k>` arithmetic (forward).
//!
//! Ordinals live in a rebuildable SQLite side table over each ref's
//! first-parent chain; every read validates freshness (tip OID + the
//! `refs/replace` digest) inside the SAME transaction as the lookup — a
//! stale index never answers. NOTE: the first query on a long branch walks
//! its whole first-parent chain once (O(chain) object loads, possibly
//! remote under tiered storage); subsequent queries are index hits.

use clap::{Parser, Subcommand};
use serde::Serialize;

use crate::{
    internal::{
        branch::Branch, db::get_db_conn_instance, head::Head,
        revision_ordinal::RevisionOrdinalIndex,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util,
    },
};

pub const REVISION_EXAMPLES: &str = "\
EXAMPLES:
    libra revision find --number 42        OID of revision #42 on the current branch
    libra revision find -n 1 --ref main    The root revision of main
    libra revision number HEAD~3           Ordinal of a commit (reverse lookup)
    libra revision index                   Index freshness for the current branch
    libra revision index --rebuild         Force a full deterministic rebuild
    libra --json revision number HEAD      Structured output for agents

NOTES:
    Ordinals number each branch's FIRST-PARENT chain, 1 = root. Commits only
    reachable through merged-in side branches have no ordinal (the reverse
    lookup says so — it never invents a number). Freshness is re-validated on
    every read: fast-forwards extend the numbering (existing ordinals never
    change); history rewrites and refs/replace changes rebuild it.";

/// Look up revisions by ordinal on a branch's first-parent chain (Libra extension).
#[derive(Parser, Debug)]
#[command(after_help = REVISION_EXAMPLES)]
pub struct RevisionArgs {
    #[command(subcommand)]
    pub command: RevisionCommand,
}

#[derive(Subcommand, Debug)]
pub enum RevisionCommand {
    /// Find a revision by ordinal (Lore's `revision find number`).
    Find {
        /// The 1-based ordinal on the ref's first-parent chain.
        #[arg(short = 'n', long = "number", value_name = "N")]
        number: i64,
        /// The branch whose chain to query (default: the current branch).
        #[arg(long, value_name = "BRANCH")]
        r#ref: Option<String>,
    },
    /// The ordinal of a commit (reverse lookup).
    Number {
        /// Any commit-ish (branch, tag, OID prefix, HEAD~n, …).
        commitish: String,
        /// The branch whose chain to query (default: the current branch).
        #[arg(long, value_name = "BRANCH")]
        r#ref: Option<String>,
    },
    /// Report (or rebuild) the index for a branch.
    Index {
        /// The branch (default: the current branch).
        #[arg(long, value_name = "BRANCH")]
        r#ref: Option<String>,
        /// Force a full deterministic rebuild (also prunes index rows for
        /// refs that no longer exist).
        #[arg(long)]
        rebuild: bool,
    },
}

#[derive(Debug, Serialize)]
#[serde(tag = "action", rename_all = "kebab-case")]
enum RevisionOutput {
    Find {
        r#ref: String,
        ordinal: i64,
        oid: String,
        total: i64,
    },
    Number {
        r#ref: String,
        oid: String,
        ordinal: i64,
        total: i64,
    },
    Index {
        r#ref: String,
        tip_oid: String,
        max_ordinal: i64,
        built_at: String,
        rebuilt: bool,
        pruned_refs: usize,
    },
}

/// Resolve the target ref: explicit `--ref` (short or full heads name) or
/// the current branch (detached HEAD → actionable 128).
async fn resolve_target_ref(explicit: Option<&str>) -> CliResult<(String, String)> {
    let short = match explicit {
        Some(name) => name.strip_prefix("refs/heads/").unwrap_or(name).to_string(),
        None => match Head::current().await {
            Head::Branch(name) => name,
            Head::Detached(_) => {
                return Err(CliError::fatal("HEAD is detached; ordinals are per-branch")
                    .with_stable_code(StableErrorCode::RepoStateInvalid)
                    .with_hint("name a branch explicitly: --ref <branch>"));
            }
        },
    };
    let branch = Branch::find_branch_result(&short, None)
        .await
        .map_err(|error| {
            CliError::fatal(format!("failed to read branch '{short}': {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?
        .ok_or_else(|| {
            CliError::fatal(format!("branch '{short}' not found"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        })?;
    Ok((format!("refs/heads/{short}"), branch.commit.to_string()))
}

pub async fn execute(args: RevisionArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

pub async fn execute_safe(args: RevisionArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    match args.command {
        RevisionCommand::Find { number, r#ref } => {
            if number < 1 {
                return Err(CliError::command_usage(format!(
                    "--number must be a positive ordinal (got {number})"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            let (ref_name, tip) = resolve_target_ref(r#ref.as_deref()).await?;
            let tip_oid: git_internal::hash::ObjectHash = tip.parse().map_err(|_| {
                CliError::fatal("branch tip is not a valid OID")
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            let db = get_db_conn_instance().await;
            // Freshness + lookup in ONE transaction (never-lie).
            let txn = crate::internal::db::begin_write_transaction(&db)
                .await
                .map_err(map_db)?;
            let meta = RevisionOrdinalIndex::ensure_fresh_with_conn(&txn, &ref_name, &tip_oid)
                .await
                .map_err(map_index)?;
            let hit = RevisionOrdinalIndex::find_by_ordinal_with_conn(&txn, &ref_name, number)
                .await
                .map_err(map_index)?;
            txn.commit().await.map_err(map_db)?;
            let Some(oid) = hit else {
                return Err(CliError::failure(format!(
                    "revision {number} not found on {ref_name} (first-parent chain has {} revisions)",
                    meta.max_ordinal
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_exit_code(1));
            };
            let report = RevisionOutput::Find {
                r#ref: ref_name,
                ordinal: number,
                oid: oid.clone(),
                total: meta.max_ordinal,
            };
            if output.is_json() {
                return emit_json_data("revision", &report, output);
            }
            if !output.quiet {
                println!("{oid}");
            }
            Ok(())
        }
        RevisionCommand::Number { commitish, r#ref } => {
            let (ref_name, tip) = resolve_target_ref(r#ref.as_deref()).await?;
            let target = crate::command::get_target_commit(&commitish)
                .await
                .map_err(|_| {
                    CliError::fatal(format!("cannot resolve '{commitish}'"))
                        .with_stable_code(StableErrorCode::CliInvalidTarget)
                })?;
            let tip_oid: git_internal::hash::ObjectHash = tip.parse().map_err(|_| {
                CliError::fatal("branch tip is not a valid OID")
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            let db = get_db_conn_instance().await;
            let txn = crate::internal::db::begin_write_transaction(&db)
                .await
                .map_err(map_db)?;
            let meta = RevisionOrdinalIndex::ensure_fresh_with_conn(&txn, &ref_name, &tip_oid)
                .await
                .map_err(map_index)?;
            let hit =
                RevisionOrdinalIndex::ordinal_of_with_conn(&txn, &ref_name, &target.to_string())
                    .await
                    .map_err(map_index)?;
            txn.commit().await.map_err(map_db)?;
            let Some(ordinal) = hit else {
                return Err(CliError::failure(format!(
                    "{target} has no ordinal on the first-parent chain of {ref_name}"
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint(
                    "commits reachable only through merged-in side branches are not numbered",
                )
                .with_exit_code(1));
            };
            let report = RevisionOutput::Number {
                r#ref: ref_name,
                oid: target.to_string(),
                ordinal,
                total: meta.max_ordinal,
            };
            if output.is_json() {
                return emit_json_data("revision", &report, output);
            }
            if !output.quiet {
                println!("{ordinal}");
            }
            Ok(())
        }
        RevisionCommand::Index { r#ref, rebuild } => {
            let (ref_name, tip) = resolve_target_ref(r#ref.as_deref()).await?;
            let tip_oid: git_internal::hash::ObjectHash = tip.parse().map_err(|_| {
                CliError::fatal("branch tip is not a valid OID")
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            // List branches BEFORE the transaction: the listing opens its
            // own pool connection and would deadlock against our txn.
            let live: Vec<String> = if rebuild {
                Branch::list_branches_result(None)
                    .await
                    .map_err(|error| {
                        CliError::fatal(format!("failed to list branches: {error}"))
                            .with_stable_code(StableErrorCode::IoReadFailed)
                    })?
                    .into_iter()
                    .map(|branch| format!("refs/heads/{}", branch.name))
                    .collect()
            } else {
                Vec::new()
            };
            let db = get_db_conn_instance().await;
            let txn = crate::internal::db::begin_write_transaction(&db)
                .await
                .map_err(map_db)?;
            let mut pruned_refs = 0usize;
            let meta = if rebuild {
                // Prune rows for refs that no longer exist (the sweep's
                // real trigger), then rebuild this ref deterministically.
                pruned_refs = RevisionOrdinalIndex::prune_missing_refs_with_conn(&txn, &live)
                    .await
                    .map_err(map_index)?;
                RevisionOrdinalIndex::rebuild_with_conn(&txn, &ref_name, &tip_oid)
                    .await
                    .map_err(map_index)?
            } else {
                RevisionOrdinalIndex::ensure_fresh_with_conn(&txn, &ref_name, &tip_oid)
                    .await
                    .map_err(map_index)?
            };
            txn.commit().await.map_err(map_db)?;
            let report = RevisionOutput::Index {
                r#ref: ref_name.clone(),
                tip_oid: meta.tip_oid.clone(),
                max_ordinal: meta.max_ordinal,
                built_at: meta.built_at.clone(),
                rebuilt: rebuild,
                pruned_refs,
            };
            if output.is_json() {
                return emit_json_data("revision", &report, output);
            }
            if !output.quiet {
                println!(
                    "{ref_name}: {} revisions (tip {}, built {})",
                    meta.max_ordinal,
                    crate::utils::text::short_display_hash(&meta.tip_oid),
                    meta.built_at
                );
                if rebuild {
                    println!("index rebuilt ({pruned_refs} stale ref(s) pruned)");
                }
            }
            Ok(())
        }
    }
}

fn map_db(error: sea_orm::DbErr) -> CliError {
    CliError::fatal(format!("revision index storage error: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

fn map_index(error: anyhow::Error) -> CliError {
    CliError::fatal(format!("revision index error: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}
