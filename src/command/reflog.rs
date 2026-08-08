//! Reads and displays reflog entries for HEAD or branches with filtering and timestamp formatting options.

use std::{
    collections::HashMap,
    fmt::{Display, Formatter},
    str::FromStr,
};

use clap::{Parser, Subcommand};
use colored::Colorize;
use git_internal::{hash::ObjectHash, internal::object::commit::Commit};
use sea_orm::{
    ConnectionTrait, DbBackend, DbErr, Statement, TransactionError, sqlx::types::chrono,
};
use serde::Serialize;

use crate::{
    command::{load_object, log::format_stat_output},
    internal::{
        config,
        db::get_db_conn_instance,
        log::date_parser::parse_date,
        model::reflog::Model,
        reflog::{
            ExpireCutoff, ExpireOptions, ExpireResult, HEAD, Reflog, ReflogError,
            expire_defaults_with_conn, expire_reflog, parse_expire_cutoff,
        },
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        pager::Pager,
    },
};

/// `--help` examples shown in `libra reflog --help` output.
///
/// reflog exposes three sub-commands (`show`, `delete`, `exists`); the
/// banner pins one example per sub-command plus a filtered `show` and a
/// JSON variant so users can map their intent to the right invocation
/// without reading the design doc. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
pub const REFLOG_EXAMPLES: &str = "\
EXAMPLES:
    libra reflog show                          Show HEAD reflog entries
    libra reflog show main --number 20         Show the last 20 entries for refs/heads/main
    libra reflog show --grep 'commit (amend)'  Filter HEAD reflog by message pattern
    libra reflog show --no-abbrev              Show entries with full object names
    libra reflog exists refs/heads/feature-x   Probe whether a ref has reflog entries
    libra reflog delete HEAD@{2}               Remove a single reflog selector
    libra reflog expire --all --dry-run        Preview which entries would be pruned
    libra reflog expire --expire=now --all     Prune all time-expired entries now
    libra reflog --json show HEAD              Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = REFLOG_EXAMPLES)]
pub struct ReflogArgs {
    #[clap(subcommand)]
    command: Option<Subcommands>,
}

/// Bare `libra reflog` (no subcommand) behaves like `libra reflog show HEAD`,
/// matching Git's default.
fn default_reflog_command() -> Subcommands {
    Subcommands::Show {
        ref_name: "HEAD".to_string(),
        pretty: FormatterKind::default(),
        since: None,
        until: None,
        grep: None,
        author: None,
        number: None,
        patch: false,
        stat: false,
        no_abbrev: false,
    }
}

#[derive(Subcommand, Debug, Clone)]
enum Subcommands {
    /// show reflog records.
    Show {
        #[clap(default_value = "HEAD")]
        ref_name: String,
        #[arg(long = "pretty")]
        #[clap(default_value_t = FormatterKind::default())]
        pretty: FormatterKind,
        /// Show reflog entries newer than date
        #[arg(long)]
        since: Option<String>,
        /// Show reflog entries older than date
        #[arg(long)]
        until: Option<String>,
        /// Filter reflog entries by message pattern
        #[arg(long)]
        grep: Option<String>,
        /// Filter reflog entries by author (matches reflog committer name or email)
        #[arg(long)]
        author: Option<String>,
        /// Limit the number of output entries
        #[clap(short, long)]
        number: Option<usize>,
        /// Show diffs for each reflog entry
        #[clap(short = 'p', long = "patch")]
        patch: bool,
        /// Show diffstat for each reflog entry
        #[arg(long)]
        stat: bool,
        /// Print full object names instead of the abbreviated 7-char prefix
        #[arg(long = "no-abbrev")]
        no_abbrev: bool,
    },
    /// clear the reflog record of the specified branch.
    Delete {
        #[clap(required = true, num_args = 1..)]
        selectors: Vec<String>,
    },
    /// check whether a reference has a reflog record, usually using by automatic scripts.
    Exists {
        #[clap(required = true)]
        ref_name: String,
    },
    /// Prune old or unreachable reflog entries (see `git reflog expire`).
    Expire {
        /// Process the reflog of every ref instead of explicit `<refs>`
        #[arg(long)]
        all: bool,
        /// Prune entries older than this time (`never`, `now`, `all`, a number of days, or a date)
        #[arg(long)]
        expire: Option<String>,
        /// Prune unreachable entries older than this time
        #[arg(long = "expire-unreachable")]
        expire_unreachable: Option<String>,
        /// Rewrite the `old`/`new` chain so it stays continuous across pruned entries
        #[arg(long)]
        rewrite: bool,
        /// Move a pruned local branch ref to its newest surviving entry
        #[arg(long)]
        updateref: bool,
        /// Prune entries whose new value no longer loads as a commit object
        #[arg(long = "stale-fix")]
        stale_fix: bool,
        /// Compute and print the plan without changing the database
        #[arg(long = "dry-run", short = 'n')]
        dry_run: bool,
        /// Print each pruned entry
        #[arg(long, short = 'v')]
        verbose: bool,
        /// Reflog refs to expire (mutually informative with `--all`)
        refs: Vec<String>,
    },
}

pub async fn execute(args: ReflogArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Dispatches to the show, delete, or exists sub-command
/// for reflog management.
pub async fn execute_safe(args: ReflogArgs, output: &OutputConfig) -> CliResult<()> {
    match args.command.unwrap_or_else(default_reflog_command) {
        Subcommands::Show {
            ref_name,
            pretty,
            since,
            until,
            grep,
            author,
            number,
            patch,
            stat,
            no_abbrev,
        } => {
            let options = ReflogShowOptions {
                pretty,
                since,
                until,
                grep,
                author,
                number,
                patch,
                stat,
                no_abbrev,
            };
            handle_show(&ref_name, options, output).await
        }
        Subcommands::Delete { selectors } => handle_delete(&selectors, output).await,
        Subcommands::Exists { ref_name } => handle_exists(&ref_name, output).await,
        Subcommands::Expire {
            all,
            expire,
            expire_unreachable,
            rewrite,
            updateref,
            stale_fix,
            dry_run,
            verbose,
            refs,
        } => {
            let options = ExpireCliOptions {
                all,
                expire,
                expire_unreachable,
                rewrite,
                updateref,
                stale_fix,
                dry_run,
                verbose,
                refs,
            };
            handle_expire(options, output).await
        }
    }
}

struct ExpireCliOptions {
    all: bool,
    expire: Option<String>,
    expire_unreachable: Option<String>,
    rewrite: bool,
    updateref: bool,
    stale_fix: bool,
    dry_run: bool,
    verbose: bool,
    refs: Vec<String>,
}

/// Options for reflog show command
#[derive(Clone)]
struct ReflogShowOptions {
    pretty: FormatterKind,
    since: Option<String>,
    until: Option<String>,
    grep: Option<String>,
    author: Option<String>,
    number: Option<usize>,
    patch: bool,
    stat: bool,
    no_abbrev: bool,
}

#[derive(Debug, Serialize)]
struct ReflogShowOutput {
    ref_name: String,
    pretty: String,
    count: usize,
    total_count: usize,
    filters: ReflogFiltersOutput,
    entries: Vec<ReflogEntryOutput>,
}

#[derive(Debug, Serialize)]
struct ReflogFiltersOutput {
    since: Option<String>,
    until: Option<String>,
    grep: Option<String>,
    author: Option<String>,
    number: Option<usize>,
    patch: bool,
    stat: bool,
}

#[derive(Debug, Serialize)]
struct ReflogEntryOutput {
    selector: String,
    index: usize,
    ref_name: String,
    old_oid: String,
    new_oid: String,
    short_new_oid: String,
    timestamp: i64,
    datetime: String,
    committer: ReflogIdentityOutput,
    action: String,
    message: String,
    summary: String,
    commit: ReflogCommitOutput,
    patch: Option<String>,
    stat: Option<String>,
}

#[derive(Debug, Serialize)]
struct ReflogIdentityOutput {
    name: String,
    email: String,
}

#[derive(Debug, Serialize)]
struct ReflogCommitOutput {
    author: ReflogIdentityOutput,
    message: String,
}

#[derive(Debug, Serialize)]
struct ReflogDeleteOutput {
    selectors: Vec<String>,
    deleted_count: usize,
}

#[derive(Debug, Serialize)]
struct ReflogExistsOutput {
    ref_name: String,
    exists: bool,
}

async fn handle_show(
    ref_name: &str,
    options: ReflogShowOptions,
    output: &OutputConfig,
) -> CliResult<()> {
    let db = get_db_conn_instance().await;

    // Parse date filters
    let since_ts = options
        .since
        .as_deref()
        .map(parse_date)
        .transpose()
        .map_err(|e| {
            CliError::fatal(format!("invalid --since date: {e}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;

    let until_ts = options
        .until
        .as_deref()
        .map(parse_date)
        .transpose()
        .map_err(|e| {
            CliError::fatal(format!("invalid --until date: {e}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;

    let ref_name = parse_ref_name(ref_name).await;
    let logs = Reflog::find_all(&db, &ref_name).await.map_err(|e| {
        CliError::fatal(format!("failed to get reflog entries: {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let total_count = logs.len();

    // Preserve original indices before filtering
    let logs_with_index: Vec<_> = logs.into_iter().enumerate().collect();

    // Apply filters
    let filter = ReflogFilter::new(
        since_ts,
        until_ts,
        options.grep.clone(),
        options.author.clone(),
    );
    let filtered_logs: Vec<_> = logs_with_index
        .into_iter()
        .filter(|(_, log)| filter.passes(log))
        .collect();

    // Apply number limit
    let max_output = options.number.unwrap_or(filtered_logs.len());
    let limited_logs = &filtered_logs[..filtered_logs.len().min(max_output)];

    if output.is_json() {
        let structured = build_reflog_show_output(&ref_name, &options, total_count, limited_logs)?;
        return emit_json_data("reflog.show", &structured, output);
    }

    let formatter = ReflogFormatter {
        logs: limited_logs,
        kind: options.pretty,
        patch: options.patch,
        stat: options.stat,
        no_abbrev: options.no_abbrev,
    };

    let mut pager = Pager::with_config(output)?;
    pager.write_line(&formatter.to_string())?;
    pager.finish()?;

    Ok(())
}

fn build_reflog_show_output(
    ref_name: &str,
    options: &ReflogShowOptions,
    total_count: usize,
    logs: &[(usize, Model)],
) -> CliResult<ReflogShowOutput> {
    let entries = logs
        .iter()
        .map(|(idx, log)| build_reflog_entry(ref_name, *idx, log, options))
        .collect::<CliResult<Vec<_>>>()?;

    Ok(ReflogShowOutput {
        ref_name: ref_name.to_string(),
        pretty: options.pretty.to_string(),
        count: entries.len(),
        total_count,
        filters: ReflogFiltersOutput {
            since: options.since.clone(),
            until: options.until.clone(),
            grep: options.grep.clone(),
            author: options.author.clone(),
            number: options.number,
            patch: options.patch,
            stat: options.stat,
        },
        entries,
    })
}

fn build_reflog_entry(
    requested_ref_name: &str,
    idx: usize,
    log: &Model,
    options: &ReflogShowOptions,
) -> CliResult<ReflogEntryOutput> {
    let commit = find_commit_checked(&log.new_oid)?;
    let patch = if options.patch {
        Some(generate_diff_sync(&commit).map_err(|e| {
            CliError::fatal(format!(
                "failed to render reflog patch for '{}': {e}",
                log.new_oid
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?)
    } else {
        None
    };
    let stat = if options.stat {
        Some(generate_stat_sync(&commit).map_err(|e| {
            CliError::fatal(format!(
                "failed to render reflog stat for '{}': {e}",
                log.new_oid
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        })?)
    } else {
        None
    };

    let selector_ref = if log.ref_name.is_empty() {
        requested_ref_name
    } else {
        &log.ref_name
    };
    Ok(ReflogEntryOutput {
        selector: format!("{selector_ref}@{{{idx}}}"),
        index: idx,
        ref_name: log.ref_name.clone(),
        old_oid: log.old_oid.clone(),
        new_oid: log.new_oid.clone(),
        short_new_oid: short_oid(&log.new_oid),
        timestamp: log.timestamp,
        datetime: format_datetime_checked(log.timestamp)?,
        committer: ReflogIdentityOutput {
            name: log.committer_name.clone(),
            email: log.committer_email.clone(),
        },
        action: log.action.clone(),
        message: log.message.clone(),
        summary: format!("{}: {}", log.action, log.message),
        commit: ReflogCommitOutput {
            author: ReflogIdentityOutput {
                name: commit.author.name.clone(),
                email: commit.author.email.clone(),
            },
            message: commit.message.trim().to_string(),
        },
        patch,
        stat,
    })
}

// `partial_ref_name` is the branch name entered by the user.
async fn parse_ref_name(partial_ref_name: &str) -> String {
    if partial_ref_name == HEAD {
        return HEAD.to_string();
    }
    if partial_ref_name.starts_with("refs/") {
        return partial_ref_name.to_string();
    }
    if !partial_ref_name.contains("/") {
        return format!("refs/heads/{partial_ref_name}");
    }
    if let Some((ref_name, _)) = partial_ref_name.split_once("/")
        && config::ConfigKv::get(&format!("remote.{ref_name}.url"))
            .await
            .ok()
            .flatten()
            .is_some()
    {
        return format!("refs/remotes/{partial_ref_name}");
    }
    format!("refs/heads/{partial_ref_name}")
}

async fn handle_exists(ref_name: &str, output: &OutputConfig) -> CliResult<()> {
    let db = get_db_conn_instance().await;
    let ref_name = parse_ref_name(ref_name).await;
    let log = Reflog::find_one(&db, &ref_name).await.map_err(|e| {
        CliError::fatal(format!("failed to get reflog entry: {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    if log.is_none() {
        return Err(
            CliError::failure(format!("reflog entry for '{}' not found", ref_name))
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    }
    if output.is_json() {
        let result = ReflogExistsOutput {
            ref_name,
            exists: true,
        };
        return emit_json_data("reflog.exists", &result, output);
    }
    Ok(())
}

/// Production reachability loader: a commit OID → its parent OIDs (or `None`
/// when the OID does not load as a commit). A plain `fn` (captures nothing) so
/// it satisfies the `Send + 'static` bound of [`expire_reflog`].
fn load_commit_parents(oid: &str) -> Option<Vec<String>> {
    let hash = ObjectHash::from_str(oid).ok()?;
    let commit = load_object::<Commit>(&hash).ok()?;
    Some(
        commit
            .parent_commit_ids
            .iter()
            .map(|parent| parent.to_string())
            .collect(),
    )
}

/// Production `--stale-fix` predicate: whether `oid` loads as a commit object.
fn oid_is_commit(oid: &str) -> bool {
    ObjectHash::from_str(oid)
        .ok()
        .is_some_and(|hash| load_object::<Commit>(&hash).is_ok())
}

/// Reject obviously-malformed ref names before touching the database.
fn validate_expire_ref(name: &str) -> CliResult<()> {
    let invalid = name.is_empty()
        || name.len() > 4096
        || name.contains("..")
        || name.chars().any(|c| c.is_control() || c == ' ');
    if invalid {
        return Err(CliError::failure(format!("invalid ref name '{name}'"))
            .with_stable_code(StableErrorCode::CliInvalidTarget));
    }
    Ok(())
}

/// Parse a CLI `--expire` / `--expire-unreachable` value into an absolute
/// cutoff, surfacing a friendly hint on failure.
fn parse_cli_expire_cutoff(raw: &str) -> CliResult<ExpireCutoff> {
    parse_expire_cutoff(raw).ok_or_else(|| {
        CliError::fatal(format!("invalid expire time '{raw}'"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint(
                "use 'never', 'now', 'all', a number of days, or a date like '10 days ago' \
                 (the dotted form '10.days.ago' is not supported)",
            )
    })
}

fn map_reflog_error(error: ReflogError) -> CliError {
    match error {
        ReflogError::Config(detail) => CliError::fatal(format!("reflog expire: {detail}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments),
        other => CliError::fatal(format!("reflog expire failed: {other}"))
            .with_stable_code(StableErrorCode::IoWriteFailed),
    }
}

/// Phase A of `reflog expire`: resolve and validate the ref list with **no**
/// writes, so an invalid/unknown ref aborts before any other ref is touched.
async fn resolve_expire_refs<C: ConnectionTrait>(
    conn: &C,
    options: &ExpireCliOptions,
) -> CliResult<Vec<String>> {
    if options.all {
        let rows = conn
            .query_all_raw(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT DISTINCT ref_name FROM reflog;".to_string(),
            ))
            .await
            .map_err(|e| {
                CliError::fatal(format!("failed to enumerate reflog refs: {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
        let mut refs: Vec<String> = rows
            .iter()
            .filter_map(|row| row.try_get::<String>("", "ref_name").ok())
            .collect();
        refs.sort();
        refs.dedup();
        return Ok(refs);
    }

    if options.refs.is_empty() {
        return Err(
            CliError::fatal("reflog expire: no reflog specified to delete")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(128)
                .with_hint("specify one or more refs, or use --all"),
        );
    }

    let mut refs = Vec::new();
    for raw in &options.refs {
        validate_expire_ref(raw)?;
        let normalized = parse_ref_name(raw).await;
        let exists = Reflog::find_one(conn, &normalized)
            .await
            .map_err(|e| {
                CliError::fatal(format!("failed to read reflog for '{normalized}': {e}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
            .is_some();
        if !exists {
            return Err(
                CliError::failure(format!("reflog for '{normalized}' not found"))
                    .with_stable_code(StableErrorCode::CliInvalidTarget),
            );
        }
        refs.push(normalized);
    }
    refs.sort();
    refs.dedup();
    Ok(refs)
}

async fn handle_expire(options: ExpireCliOptions, output: &OutputConfig) -> CliResult<()> {
    let db = get_db_conn_instance().await;

    let (default_expire, default_unreachable) = expire_defaults_with_conn(&db)
        .await
        .map_err(map_reflog_error)?;
    let expire = match &options.expire {
        Some(raw) => parse_cli_expire_cutoff(raw)?,
        None => default_expire,
    };
    let expire_unreachable = match &options.expire_unreachable {
        Some(raw) => parse_cli_expire_cutoff(raw)?,
        None => default_unreachable,
    };

    let expire_options = ExpireOptions {
        expire,
        expire_unreachable,
        rewrite: options.rewrite,
        updateref: options.updateref,
        stale_fix: options.stale_fix,
        dry_run: options.dry_run,
    };

    // Phase A: resolve + validate (no writes).
    let refs = resolve_expire_refs(&db, &options).await?;

    // Part C W0 (§C.11): `--updateref` moves a local branch tip to its newest
    // surviving reflog entry. Refuse before any write if a target branch is
    // checked out in ANOTHER worktree — moving its tip would diverge that
    // worktree's working tree from its branch. Non-`--updateref` expiry only
    // trims reflog entries and is unaffected.
    if options.updateref && !options.dry_run {
        for ref_name in &refs {
            if let Some(branch) = ref_name.strip_prefix("refs/heads/") {
                let checked_out_elsewhere =
                    crate::internal::head::Head::branch_checked_out_elsewhere_result(branch)
                        .await
                        .map_err(|error| {
                            CliError::fatal(format!(
                                "cannot verify whether '{branch}' is checked out in another \
                                 worktree: {error}"
                            ))
                            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                            .with_hint("repair the repository database, then retry")
                        })?;
                if let Some(other) = checked_out_elsewhere {
                    return Err(CliError::fatal(format!(
                        "cannot expire --updateref: branch '{branch}' is checked out at worktree '{other}'"
                    ))
                    .with_stable_code(StableErrorCode::ConflictOperationBlocked)
                    .with_hint(
                        "switch that worktree to another branch first, or run the command there",
                    ));
                }
            }
        }
    }

    // Phase B: expire each ref in its own transaction.
    let mut results = Vec::new();
    for ref_name in &refs {
        let result = expire_reflog(
            &db,
            ref_name,
            &expire_options,
            load_commit_parents,
            oid_is_commit,
        )
        .await
        .map_err(map_reflog_error)?;
        results.push(result);
    }

    render_expire(&results, options.verbose, output)
}

fn render_expire(results: &[ExpireResult], verbose: bool, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data("reflog.expire", &results.to_vec(), output);
    }
    if output.quiet {
        return Ok(());
    }
    for result in results {
        if verbose {
            for entry in &result.pruned_entries {
                let reason = format!("{:?}", entry.reason).to_lowercase();
                println!(
                    "{reason} {}@{{{}}} {}..{}",
                    result.ref_name,
                    entry.index,
                    short_oid(&entry.old_oid),
                    short_oid(&entry.new_oid),
                );
            }
        }
        if result.pruned > 0 {
            println!(
                "{}: pruned {} of {} reflog entries",
                result.ref_name, result.pruned, result.scanned
            );
        }
    }
    Ok(())
}

async fn handle_delete(selectors: &[String], output: &OutputConfig) -> CliResult<()> {
    let mut groups = HashMap::new();
    for selector in selectors {
        if let Some(parsed) = parse_reflog_selector(selector) {
            let normalized_ref_name = parse_ref_name(parsed.0).await;
            groups
                .entry(normalized_ref_name.clone())
                .or_insert_with(Vec::new)
                .push((normalized_ref_name, parsed.1));
            continue;
        }
        return Err(
            CliError::fatal(format!("invalid reflog entry format: {selector}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }

    let groups = groups
        .into_values()
        .map(|mut group| {
            group.sort_by_key(|b| std::cmp::Reverse(b.1));
            group
        })
        .collect::<Vec<_>>();
    let mut deleted_count = 0;
    for group in groups {
        deleted_count += delete_single_group(&group).await?;
    }
    if output.is_json() {
        let result = ReflogDeleteOutput {
            selectors: selectors.to_vec(),
            deleted_count,
        };
        return emit_json_data("reflog.delete", &result, output);
    }
    Ok(())
}

async fn delete_single_group(group: &[(String, usize)]) -> CliResult<usize> {
    let db = get_db_conn_instance().await;
    // clone this to move it into async block to make compiler happy :(
    let group = group.to_vec();

    crate::internal::db::write_transaction(&db, |txn| {
        Box::pin(async move {
            let ref_name = &group[0].0;
            let logs = Reflog::find_all(txn, ref_name)
                .await
                .map_err(|err| DbErr::Custom(err.to_string()))?;

            let mut deleted = 0;
            for (_, index) in &group {
                if let Some(entry) = logs.get(*index) {
                    let id = entry.id;
                    txn.execute_raw(Statement::from_sql_and_values(
                        DbBackend::Sqlite,
                        "DELETE FROM reflog WHERE id = ?;",
                        [id.into()],
                    ))
                    .await?;
                    deleted += 1;
                    continue;
                }
                return Err(DbErr::Custom(format!(
                    "reflog entry `{ref_name}@{{{index}}}` not found"
                )));
            }

            Ok::<_, DbErr>(deleted)
        })
    })
    .await
    .map_err(map_reflog_delete_error)
}

fn map_reflog_delete_error(err: TransactionError<DbErr>) -> CliError {
    let detail = match err {
        TransactionError::Connection(err) | TransactionError::Transaction(err) => err.to_string(),
    };
    let stable_code = if detail.to_ascii_lowercase().contains("not found") {
        StableErrorCode::CliInvalidTarget
    } else {
        StableErrorCode::IoWriteFailed
    };
    CliError::fatal(format!("failed to delete reflog entries: {detail}"))
        .with_stable_code(stable_code)
}

fn parse_reflog_selector(selector: &str) -> Option<(&str, usize)> {
    if let (Some(at_brace), Some(end_brace)) = (selector.find("@{"), selector.find('}'))
        && at_brace < end_brace
    {
        let ref_name = &selector[..at_brace];
        let index_str = &selector[at_brace + 2..end_brace];

        if let Ok(index) = index_str.parse::<usize>() {
            return Some((ref_name, index));
        }
    }
    None
}

/// Filter for reflog entries based on time and message patterns
struct ReflogFilter {
    since: Option<i64>,
    until: Option<i64>,
    grep: Option<String>,
    author: Option<String>,
}

impl ReflogFilter {
    /// Create a new filter from optional parameters
    fn new(
        since: Option<i64>,
        until: Option<i64>,
        grep: Option<String>,
        author: Option<String>,
    ) -> Self {
        Self {
            since,
            until,
            grep: grep.map(|s| s.to_lowercase()),
            author: author.map(|s| s.to_lowercase()),
        }
    }

    /// Check if a reflog entry passes all filters
    fn passes(&self, entry: &Model) -> bool {
        // Time filters
        let ts = entry.timestamp;

        if let Some(since) = self.since
            && ts < since
        {
            return false;
        }

        if let Some(until) = self.until
            && ts > until
        {
            return false;
        }

        // Message filter (matches both action and message fields)
        if let Some(grep_pattern) = &self.grep {
            let full_message = format!("{}: {}", entry.action, entry.message);
            if !full_message.to_lowercase().contains(grep_pattern) {
                return false;
            }
        }

        // Author filter (matches committer_name or committer_email)
        if let Some(author_filter) = &self.author {
            let committer = format!(
                "{} <{}>",
                entry.committer_name.to_lowercase(),
                entry.committer_email.to_lowercase()
            );
            if !committer.contains(author_filter) {
                return false;
            }
        }

        true
    }
}

#[derive(Debug, Copy, Clone, Default)]
enum FormatterKind {
    #[default]
    Oneline,
    Short,
    Medium,
    Full,
}

impl Display for FormatterKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Oneline => f.write_str("oneline"),
            Self::Short => f.write_str("short"),
            Self::Medium => f.write_str("medium"),
            Self::Full => f.write_str("full"),
        }
    }
}

impl From<String> for FormatterKind {
    fn from(value: String) -> Self {
        match value.as_str() {
            "oneline" => FormatterKind::Oneline,
            "short" => FormatterKind::Short,
            "medium" => FormatterKind::Medium,
            "full" => FormatterKind::Full,
            _ => FormatterKind::Oneline,
        }
    }
}

struct ReflogFormatter<'a> {
    logs: &'a [(usize, Model)],
    kind: FormatterKind,
    patch: bool,
    stat: bool,
    /// `--no-abbrev`: print the full object name instead of the 7-char prefix.
    no_abbrev: bool,
}

impl Display for ReflogFormatter<'_> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        let all = self.logs
            .iter()
            .map(|(idx, log)| {
                let head = format!("HEAD@{{{idx}}}");
                let new_oid = if self.no_abbrev {
                    log.new_oid.as_str()
                } else {
                    // Abbreviate to 7 hex chars; `get` avoids a panic if the
                    // stored id is somehow shorter than 7 bytes.
                    log.new_oid.get(..7).unwrap_or(log.new_oid.as_str())
                };

                let commit = find_commit(&log.new_oid);
                let full_msg = format!("{}: {}", log.action, log.message);

                let author = format!("{} <{}>", commit.author.name, commit.author.email);
                let committer = format!("{} <{}>", log.committer_name, log.committer_email);
                let commit_msg = &commit.message.trim();
                let datetime = format_datetime(log.timestamp);

                let mut output = match self.kind {
                    FormatterKind::Oneline => format!(
                        "{} {head}: {full_msg}",
                        new_oid.to_string().bright_magenta(),
                    ),
                    FormatterKind::Short => format!(
                        "{}\nReflog: {head} ({author})\nReflog message: {full_msg}\nAuthor: {author}\n\n  {commit_msg}\n",
                        format!("commit {new_oid}").bright_magenta(),
                    ),
                    FormatterKind::Medium => format!(
                        "{}\nReflog: {head} ({author})\nReflog message: {full_msg}\nAuthor: {author}\nDate:   {datetime}\n\n  {commit_msg}\n",
                        format!("commit {new_oid}").bright_magenta(),
                    ),
                    FormatterKind::Full => format!(
                        "{}\nReflog: {head} ({author})\nReflog message: {full_msg}\nAuthor: {author}\nCommit: {committer}\n\n  {commit_msg}\n",
                        format!("commit {new_oid}").bright_magenta(),
                    ),
                };

                // Append diff output if requested
                if self.patch
                    && let Ok(patch_output) = generate_diff_sync(&commit)
                    && !patch_output.is_empty()
                {
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str(&patch_output);
                }

                // Append stat output if requested
                if self.stat
                    && let Ok(stat_output) = generate_stat_sync(&commit)
                    && !stat_output.is_empty()
                {
                    if !output.ends_with('\n') {
                        output.push('\n');
                    }
                    output.push_str(&stat_output);
                }

                output
            })
            .collect::<Vec<_>>()
            .join("\n");
        writeln!(f, "{all}")
    }
}

// INVARIANT: `commit_hash` comes from the reflog which only stores valid hashes
// pointing to existing objects. If the object store is corrupt, panicking during
// formatting is acceptable. The Result-returning sibling `find_commit_checked`
// surfaces both failure modes as `RepoCorrupt` for callers that handle them.
fn find_commit(commit_hash: &str) -> Commit {
    let hash = ObjectHash::from_str(commit_hash)
        .expect("reflog commit hash is malformed (reflog object store may be corrupt)");
    load_object::<Commit>(&hash)
        .expect("reflog commit object is missing (reflog object store may be corrupt)")
}

fn find_commit_checked(commit_hash: &str) -> CliResult<Commit> {
    let hash = ObjectHash::from_str(commit_hash).map_err(|e| {
        CliError::fatal(format!("invalid reflog object id '{commit_hash}': {e}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    load_object::<Commit>(&hash).map_err(|e| {
        CliError::fatal(format!("failed to load reflog commit '{commit_hash}': {e}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })
}

// INVARIANT: reflog timestamps are always valid Unix timestamps written by our
// own code. `from_timestamp` only returns `None` for out-of-range values that
// cannot occur in practice. The Result-returning sibling `format_datetime_checked`
// surfaces an out-of-range timestamp as `RepoCorrupt`.
fn format_datetime(timestamp: i64) -> String {
    let naive = chrono::DateTime::from_timestamp(timestamp, 0)
        .expect("reflog timestamp out of chrono::DateTime range (reflog may be corrupt)");
    let local = naive.with_timezone(&chrono::Local);

    let git_format = "%a %b %d %H:%M:%S %Y %z";
    local.format(git_format).to_string()
}

fn format_datetime_checked(timestamp: i64) -> CliResult<String> {
    let naive = chrono::DateTime::from_timestamp(timestamp, 0).ok_or_else(|| {
        CliError::fatal(format!("invalid reflog timestamp '{timestamp}'"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let local = naive.with_timezone(&chrono::Local);

    let git_format = "%a %b %d %H:%M:%S %Y %z";
    Ok(local.format(git_format).to_string())
}

fn short_oid(oid: &str) -> String {
    oid.chars().take(7).collect()
}

/// Synchronous wrapper for generating diff output
fn generate_diff_sync(commit: &Commit) -> Result<String, Box<dyn std::error::Error>> {
    use git_internal::{
        Diff,
        internal::object::{blob::Blob, tree::Tree},
    };

    use crate::utils::object_ext::TreeExt;

    // new_blobs from commit tree
    let tree = load_object::<Tree>(&commit.tree_id)?;
    let new_blobs: Vec<(std::path::PathBuf, ObjectHash)> = tree.get_plain_items();

    // old_blobs from first parent if exists
    let old_blobs: Vec<(std::path::PathBuf, ObjectHash)> = if !commit.parent_commit_ids.is_empty() {
        let parent = &commit.parent_commit_ids[0];
        let parent_hash = ObjectHash::from_str(&parent.to_string())?;
        let parent_commit = load_object::<Commit>(&parent_hash)?;
        let parent_tree = load_object::<Tree>(&parent_commit.tree_id)?;
        parent_tree.get_plain_items()
    } else {
        Vec::new()
    };

    let read_content = |_file: &std::path::PathBuf, hash: &ObjectHash| -> Vec<u8> {
        load_object::<Blob>(hash)
            .map(|blob| blob.data)
            .unwrap_or_default()
    };

    let diffs = Diff::diff(
        old_blobs,
        new_blobs,
        Vec::new(), // No path filters for reflog
        read_content,
    );

    let mut diff_output = String::new();
    for diff in diffs {
        diff_output.push_str(&format!("--- a/{}\n", diff.path));
        diff_output.push_str(&format!("+++ b/{}\n", diff.path));
        diff_output.push_str(&diff.data);
        diff_output.push('\n');
    }

    Ok(diff_output)
}

/// Synchronous wrapper for generating stat output
fn generate_stat_sync(commit: &Commit) -> Result<String, Box<dyn std::error::Error>> {
    use git_internal::{
        Diff,
        internal::object::{blob::Blob, tree::Tree},
    };

    use crate::{command::log::FileStat, utils::object_ext::TreeExt};

    // new_blobs from commit tree
    let tree = load_object::<Tree>(&commit.tree_id)?;
    let new_blobs: Vec<(std::path::PathBuf, ObjectHash)> = tree.get_plain_items();

    // old_blobs from first parent if exists
    let old_blobs: Vec<(std::path::PathBuf, ObjectHash)> = if !commit.parent_commit_ids.is_empty() {
        let parent = &commit.parent_commit_ids[0];
        let parent_hash = ObjectHash::from_str(&parent.to_string())?;
        let parent_commit = load_object::<Commit>(&parent_hash)?;
        let parent_tree = load_object::<Tree>(&parent_commit.tree_id)?;
        parent_tree.get_plain_items()
    } else {
        Vec::new()
    };

    let read_content = |_file: &std::path::PathBuf, hash: &ObjectHash| -> Vec<u8> {
        load_object::<Blob>(hash)
            .map(|blob| blob.data)
            .unwrap_or_default()
    };

    let diffs = Diff::diff(
        old_blobs,
        new_blobs,
        Vec::new(), // No path filters for reflog
        read_content,
    );

    // Compute per-file statistics
    let mut stats = Vec::new();
    for diff_item in diffs {
        let mut insertions = 0;
        let mut deletions = 0;
        for line in diff_item.data.lines() {
            if line.starts_with('+') && !line.starts_with("+++") {
                insertions += 1;
            } else if line.starts_with('-') && !line.starts_with("---") {
                deletions += 1;
            }
        }
        if insertions > 0 || deletions > 0 {
            stats.push(FileStat {
                path: diff_item.path,
                insertions,
                deletions,
            });
        }
    }

    // Use log module's formatting function for consistent output
    Ok(format_stat_output(&stats))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[test]
    fn test_show_args_with_filters() {
        let args = ReflogArgs::parse_from([
            "reflog",
            "show",
            "--since",
            "2024-01-01",
            "--until",
            "2024-12-31",
            "--grep",
            "commit",
        ]);

        if let Some(Subcommands::Show {
            ref_name,
            pretty: _,
            since,
            until,
            grep,
            author: _,
            number: _,
            patch: _,
            stat: _,
            no_abbrev: _,
        }) = args.command
        {
            assert_eq!(ref_name, "HEAD");
            assert_eq!(since.as_deref(), Some("2024-01-01"));
            assert_eq!(until.as_deref(), Some("2024-12-31"));
            assert_eq!(grep.as_deref(), Some("commit"));
        } else {
            panic!("Expected Show subcommand");
        }
    }

    #[test]
    fn test_reflog_filter_time() {
        let entry1 = Model {
            id: 1,
            ref_name: "HEAD".to_string(),
            old_oid: "abc".to_string(),
            new_oid: "def".to_string(),
            timestamp: 1_700_000_000,
            committer_name: "Test".to_string(),
            committer_email: "test@test.com".to_string(),
            action: "commit".to_string(),
            message: "Test message".to_string(),
            worktree_id: None,
        };

        let entry2 = Model {
            id: 2,
            ref_name: "HEAD".to_string(),
            old_oid: "def".to_string(),
            new_oid: "ghi".to_string(),
            timestamp: 1_750_000_000,
            committer_name: "Test".to_string(),
            committer_email: "test@test.com".to_string(),
            action: "commit".to_string(),
            message: "Another message".to_string(),
            worktree_id: None,
        };

        let filter = ReflogFilter::new(Some(1_720_000_000), None, None, None);
        assert!(!filter.passes(&entry1));
        assert!(filter.passes(&entry2));

        let filter = ReflogFilter::new(None, Some(1_730_000_000), None, None);
        assert!(filter.passes(&entry1));
        assert!(!filter.passes(&entry2));
    }

    #[test]
    fn test_reflog_filter_grep() {
        let entry1 = Model {
            id: 1,
            ref_name: "HEAD".to_string(),
            old_oid: "abc".to_string(),
            new_oid: "def".to_string(),
            timestamp: 1_700_000_000,
            committer_name: "Test".to_string(),
            committer_email: "test@test.com".to_string(),
            action: "commit".to_string(),
            message: "Add feature".to_string(),
            worktree_id: None,
        };

        let entry2 = Model {
            id: 2,
            ref_name: "HEAD".to_string(),
            old_oid: "def".to_string(),
            new_oid: "ghi".to_string(),
            timestamp: 1_750_000_000,
            committer_name: "Test".to_string(),
            committer_email: "test@test.com".to_string(),
            action: "merge".to_string(),
            message: "Merge branch".to_string(),
            worktree_id: None,
        };

        let filter = ReflogFilter::new(None, None, Some("COMMIT".to_string()), None);
        assert!(filter.passes(&entry1));
        assert!(!filter.passes(&entry2));

        let filter = ReflogFilter::new(None, None, Some("merge".to_string()), None);
        assert!(!filter.passes(&entry1));
        assert!(filter.passes(&entry2));
    }

    #[test]
    fn test_reflog_filter_combined() {
        let entry = Model {
            id: 1,
            ref_name: "HEAD".to_string(),
            old_oid: "abc".to_string(),
            new_oid: "def".to_string(),
            timestamp: 1_725_000_000,
            committer_name: "Test".to_string(),
            committer_email: "test@test.com".to_string(),
            action: "commit".to_string(),
            message: "Add feature".to_string(),
            worktree_id: None,
        };

        let filter = ReflogFilter::new(
            Some(1_700_000_000),
            Some(1_750_000_000),
            Some("feature".to_string()),
            None,
        );
        assert!(filter.passes(&entry));

        let filter = ReflogFilter::new(
            Some(1_730_000_000),
            Some(1_750_000_000),
            Some("feature".to_string()),
            None,
        );
        assert!(!filter.passes(&entry));
    }

    #[test]
    fn test_reflog_filter_author() {
        let entry1 = Model {
            id: 1,
            ref_name: "HEAD".to_string(),
            old_oid: "abc".to_string(),
            new_oid: "def".to_string(),
            timestamp: 1_700_000_000,
            committer_name: "Alice".to_string(),
            committer_email: "alice@example.com".to_string(),
            action: "commit".to_string(),
            message: "Test message".to_string(),
            worktree_id: None,
        };

        let entry2 = Model {
            id: 2,
            ref_name: "HEAD".to_string(),
            old_oid: "def".to_string(),
            new_oid: "ghi".to_string(),
            timestamp: 1_750_000_000,
            committer_name: "Bob".to_string(),
            committer_email: "bob@example.com".to_string(),
            action: "commit".to_string(),
            message: "Another message".to_string(),
            worktree_id: None,
        };

        // Test author filtering by name
        let filter = ReflogFilter::new(None, None, None, Some("alice".to_string()));
        assert!(filter.passes(&entry1));
        assert!(!filter.passes(&entry2));

        // Test author filtering by email
        let filter = ReflogFilter::new(None, None, None, Some("bob@example".to_string()));
        assert!(!filter.passes(&entry1));
        assert!(filter.passes(&entry2));

        // Test case-insensitive matching
        let filter = ReflogFilter::new(None, None, None, Some("ALICE".to_string()));
        assert!(filter.passes(&entry1));
    }
}
