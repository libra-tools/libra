//! Shortlog command for summarizing commit history by author.
//!
//! This module implements a `git shortlog`-style report used primarily for
//! release announcements and contributor overviews. It is structured as a
//! standard CLI command module, following the conventions used by other
//! commands in this crate:
//!
//! - **Argument parsing** is handled by [`ShortlogArgs`], which defines the
//!   supported flags and options using `clap::Parser`. The key flags are:
//!   - `numbered` (`-n` / `--numbered`): sort authors by descending commit
//!     count rather than by name.
//!   - `summary` (`-s` / `--summary`): emit only per-author commit counts,
//!     suppressing individual commit subjects.
//!   - `email` (`-e` / `--email`): include the author email address in the
//!     report header.
//!   - `since` / `until`: restrict the set of commits by committer timestamp,
//!     using the repository-wide date parser in [`parse_date`].
//!
//! - **Execution entrypoints**:
//!   - [`execute`] is the user-facing async entrypoint used by the CLI
//!     dispatcher. It writes human-readable output to `stdout`.
//!   - [`execute_to`] contains the core logic and is parameterized over an
//!     arbitrary `Write` implementor, which makes it easier to test and to
//!     reuse from other tooling without being tied to a specific output
//!     stream.
//!
//! - **Commit collection and filtering**:
//!   - [`get_commits_for_shortlog`] resolves the current [`Head`] and
//!     obtains the relevant list of [`Commit`] objects to be included in the
//!     report. The exact traversal strategy is delegated to the internal git
//!     engine.
//!   - [`passes_filter`] applies `since`/`until` constraints to each
//!     commit, converting user-supplied date strings via [`parse_date`] and
//!     comparing them against the commit committer timestamp (to match `git log`).
//!
//! - **Aggregation and formatting**:
//!   - Commits are grouped by author identity in an in-memory
//!     `HashMap<String, AuthorStats>`, where [`AuthorStats`] tracks the
//!     author name, optional email address, total commit count, and a list
//!     of commit subjects.
//!   - If `-e` is provided, grouping is by `name <email>`. Otherwise, it is
//!     by `name` only (merging multiple emails for the same author).
//!   - After aggregation, the authors are converted to a vector, optionally
//!     sorted by commit count (`numbered`) or left in deterministic order,
//!     and finally rendered to the provided writer in either detailed or
//!     summary form depending on the `summary` flag.
//!
//! The implementation is intentionally streaming-friendly at the output
//! layer (it writes directly to the provided `Write`), while still
//! aggregating per-author statistics in memory for predictable formatting.

use std::{
    collections::HashMap,
    fmt,
    io::{self, IsTerminal, Read, Write},
};

use clap::Parser;
use git_internal::internal::object::commit::Commit;
use serde::Serialize;

use crate::{
    internal::log::{
        date_parser::parse_date,
        formatter::{CommitFormatter, FormatContext, FormatType},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{ColorChoice, OutputConfig, emit_json_data},
        util::{self, CommitBaseError, require_repo},
    },
};

const SHORTLOG_EXAMPLES: &str = "\
EXAMPLES:
    libra shortlog                  Summarize commits reachable from HEAD by author
    libra shortlog HEAD~5           Summarize a subset of history starting from a revision
    libra shortlog -n -s            Sort by commit count, suppress subjects (count only)
    libra shortlog -c -s            Summarize by committer instead of author
    libra shortlog --group=trailer:Co-authored-by   Group by Co-authored-by trailer values
    libra shortlog --no-merges      Exclude merge commits from the summary
    libra shortlog --merges         Summarize only merge commits
    libra shortlog --top 5          Show only the top 5 authors
    libra shortlog --format='%h %s' Render each commit line with a custom template
    libra shortlog --since 24h      Restrict to commits in the last 24 hours
    libra shortlog -w50             Wrap commit subjects at 50 columns
    libra shortlog --json           Structured JSON output for agents
    git log | libra shortlog        Summarize piped log output (Git's stdin mode)";

#[derive(Parser, Debug)]
#[command(after_help = SHORTLOG_EXAMPLES)]
pub struct ShortlogArgs {
    /// Sort output according to the number of commits per author
    #[clap(short = 'n', long = "numbered")]
    pub numbered: bool,

    /// Suppress commit description and provide a commit count summary only
    #[clap(short = 's', long = "summary")]
    pub summary: bool,

    /// Show the email address of each author
    #[clap(short = 'e', long = "email")]
    pub email: bool,

    /// Linewrap the subject output: `-w[<width>[,<indent1>[,<indent2>]]]`.
    /// Defaults to width 76, first-line indent 6, continuation indent 9.
    /// A width of 0 indents without wrapping. Bare `-w` uses the defaults.
    #[clap(short = 'w', long = "wrap", value_name = "W[,I1[,I2]]", num_args = 0..=1, default_missing_value = "76,6,9")]
    pub wrap: Option<String>,

    /// Show commits more recent than DATE (RFC3339, `YYYY-MM-DD`, or relative like `24h` / `7d`)
    #[clap(long = "since", value_name = "DATE")]
    pub since: Option<String>,

    /// Show commits older than DATE (RFC3339, `YYYY-MM-DD`, or relative like `1h`)
    #[clap(long = "until", value_name = "DATE")]
    pub until: Option<String>,

    /// Only summarize commits whose author matches PATTERN (case-insensitive
    /// substring of `name <email>`). Filters on author even with `-c`.
    #[clap(long = "author", value_name = "PATTERN")]
    pub author: Option<String>,

    /// Group commits by committer identity instead of author.
    #[clap(short = 'c', long = "committer")]
    pub committer: bool,

    /// Do not include merge commits (commits with more than one parent).
    #[clap(long = "no-merges", overrides_with = "merges")]
    pub no_merges: bool,

    /// Include only merge commits (commits with more than one parent).
    #[clap(long = "merges", overrides_with = "no_merges")]
    pub merges: bool,

    /// Show only the top N authors.
    #[clap(long = "top", value_name = "N")]
    pub top: Option<usize>,

    /// Show only authors with at least N commits.
    #[clap(long = "min-count", value_name = "N")]
    pub min_count: Option<usize>,

    /// Reverse the output order.
    #[clap(long = "reverse")]
    pub reverse: bool,

    /// Group commits by `author` (default), `committer`, or `trailer:<key>`
    /// (group by each value of the given commit-message trailer, e.g.
    /// `trailer:Co-authored-by`). Takes precedence over `-c`/`--committer`.
    #[clap(long = "group", value_name = "TYPE")]
    pub group: Option<String>,

    /// Render each commit under its author header with a custom format string
    /// (the same `%`-placeholders as `libra log --format`, including `%b`,
    /// `%B`, `%n`, ASCII/control `%xNN`, strict ISO dates, raw timestamps,
    /// decorations, marks, and color placeholders) instead of the commit subject.
    #[clap(long = "format", value_name = "FORMAT")]
    pub format: Option<String>,

    /// Revision to summarize. Defaults to HEAD.
    pub revision: Option<String>,
}

/// How commits are grouped for the summary.
enum GroupMode {
    Author,
    Committer,
    /// Group by each value of the named commit-message trailer.
    Trailer(String),
}

/// Resolve the grouping mode from `--group` (preferred) or the legacy
/// `-c`/`--committer` flag. Returns a usage error for an unknown `--group`.
fn resolve_group_mode(args: &ShortlogArgs) -> CliResult<GroupMode> {
    match args.group.as_deref() {
        None => Ok(if args.committer {
            GroupMode::Committer
        } else {
            GroupMode::Author
        }),
        Some("author") => Ok(GroupMode::Author),
        Some("committer") => Ok(GroupMode::Committer),
        Some(spec) if spec.starts_with("trailer:") => {
            let key = spec["trailer:".len()..].trim();
            if key.is_empty() {
                return Err(
                    CliError::fatal("--group=trailer:<key> requires a non-empty key")
                        .with_stable_code(StableErrorCode::CliInvalidArguments)
                        .with_hint("example: --group=trailer:Co-authored-by"),
                );
            }
            Ok(GroupMode::Trailer(key.to_string()))
        }
        Some(other) => Err(CliError::fatal(format!("unknown --group type '{other}'"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("expected: author, committer, or trailer:<key>")),
    }
}

/// Extract `(name, email)` identities from the trailer block of `message` whose
/// key matches `key` (case-insensitive). The trailer block is the last
/// paragraph of the message; each `Key: Value` line whose key matches yields
/// one identity (Value parsed as `Name <email>`, or the raw text with an empty
/// email). A commit may contribute several identities (or none).
fn extract_trailer_identities(message: &str, key: &str) -> Vec<(String, String)> {
    // Shared Git-faithful parser (internal::log::trailer, lore.md §1.9): the
    // block must QUALIFY per git's rules (last paragraph, not the title,
    // alnum/dash keys, 25% rule). This tightened the old loose rsplit("\n\n")
    // parser to agree with `git shortlog --group=trailer:<key>` — notably a
    // single-paragraph message and a prose-heavy final paragraph no longer
    // contribute groups. The `key` itself is strengthened as a recognized key
    // so an explicitly requested group qualifies its own mixed block, matching
    // git's behavior for configured/requested trailers.
    let mut identities = Vec::new();
    for trailer in crate::internal::log::trailer::parse_trailers_with_recognized(message, &[key]) {
        if !trailer.key_matches(key) {
            continue;
        }
        let value = trailer.value.as_str();
        if value.is_empty() {
            continue;
        }
        // Parse `Name <email>` when present; otherwise treat the whole value as
        // the name with an empty email.
        if let (Some(open), Some(close)) = (value.rfind('<'), value.rfind('>'))
            && open < close
        {
            let name = value[..open].trim().to_string();
            let email = value[open + 1..close].trim().to_string();
            identities.push((name, email));
        } else {
            identities.push((value.to_string(), String::new()));
        }
    }
    identities
}

struct AuthorStats {
    name: String,
    email: String,
    count: usize,
    subjects: Vec<String>,
}

impl AuthorStats {
    fn new(name: String, email: String) -> Self {
        Self {
            name,
            email,
            count: 0,
            subjects: Vec::new(),
        }
    }

    fn add_commit(&mut self, subject: String) {
        self.count += 1;
        self.subjects.push(subject);
    }
}

#[derive(Debug, Clone, Serialize)]
struct ShortlogAuthor {
    name: String,
    email: Option<String>,
    count: usize,
    subjects: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct ShortlogOutput {
    revision: String,
    numbered: bool,
    summary: bool,
    email: bool,
    total_authors: usize,
    total_commits: usize,
    authors: Vec<ShortlogAuthor>,
    /// Parsed `-w` wrap configuration `(width, indent1, indent2)`; a render-only
    /// hint (omitted from JSON, which carries the unwrapped subjects).
    #[serde(skip)]
    wrap: Option<(usize, usize, usize)>,
}

/// Runs shortlog and writes **human-readable** output to the given writer.
///
/// This function always produces the human-formatted report regardless of
/// `OutputConfig` or `--json`. It is used by tests and callers that need
/// direct writer control. For the full CLI entry point that honours JSON /
/// quiet modes, use [`execute_safe`].
pub async fn execute_to(args: ShortlogArgs, writer: &mut impl Write) -> CliResult<()> {
    require_repo().map_err(|_| CliError::repo_not_found())?;
    let shortlog_output = run_shortlog(&args, false).await?;
    render_shortlog_output(&shortlog_output, writer)
}

fn write_shortlog_line(writer: &mut impl Write, args: fmt::Arguments<'_>) -> CliResult<bool> {
    match writer.write_fmt(args) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => return Ok(false),
        Err(err) => return Err(shortlog_output_error(err)),
    }

    match writer.write_all(b"\n") {
        Ok(()) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(err) => Err(shortlog_output_error(err)),
    }
}

fn shortlog_output_error(err: io::Error) -> CliError {
    CliError::fatal(format!("shortlog output error: {err}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

/// Parse a `-w[<width>[,<indent1>[,<indent2>]]]` spec into
/// `(width, indent1, indent2)`. Missing components default to Git's 76 / 6 / 9.
/// `None` input means no wrapping was requested.
fn parse_wrap_spec(spec: Option<&str>) -> CliResult<Option<(usize, usize, usize)>> {
    let Some(spec) = spec else {
        return Ok(None);
    };
    let parts: Vec<&str> = spec.split(',').collect();
    if parts.len() > 3 {
        return Err(CliError::command_usage(format!(
            "invalid --wrap value '{spec}' (expected <width>[,<indent1>[,<indent2>]])"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let parse_component = |raw: &str, default: usize| -> CliResult<usize> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(default);
        }
        trimmed.parse::<usize>().map_err(|_| {
            CliError::command_usage(format!("invalid --wrap value '{spec}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })
    };
    let width = parse_component(parts.first().copied().unwrap_or(""), 76)?;
    let indent1 = parse_component(parts.get(1).copied().unwrap_or(""), 6)?;
    let indent2 = parse_component(parts.get(2).copied().unwrap_or(""), 9)?;
    Ok(Some((width, indent1, indent2)))
}

/// Wrap one subject for `-w`, returning fully-indented output lines: the first
/// line is indented by `indent1`, continuations by `indent2`, and each line
/// (including its indent) is kept within `width` columns by word-wrapping. A
/// `width` of 0 indents the single line without wrapping.
fn wrap_subject_lines(subject: &str, width: usize, indent1: usize, indent2: usize) -> Vec<String> {
    if width == 0 {
        return vec![format!("{}{}", " ".repeat(indent1), subject)];
    }
    let mut lines = Vec::new();
    let mut current = String::new();
    let mut indent = indent1;
    for word in subject.split_whitespace() {
        if current.is_empty() {
            current = format!("{}{}", " ".repeat(indent), word);
        } else if current.chars().count() + 1 + word.chars().count() <= width {
            current.push(' ');
            current.push_str(word);
        } else {
            lines.push(std::mem::take(&mut current));
            indent = indent2;
            current = format!("{}{}", " ".repeat(indent), word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(" ".repeat(indent1));
    }
    lines
}

pub async fn execute(args: ShortlogArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Summarises commit history by author, delegating to
/// [`execute_to`] for formatted output.
pub async fn execute_safe(args: ShortlogArgs, output: &OutputConfig) -> CliResult<()> {
    require_repo().map_err(|_| CliError::repo_not_found())?;
    let shortlog_output = run_shortlog(&args, color_enabled_for_output(output)).await?;

    if output.is_json() {
        emit_json_data("shortlog", &shortlog_output, output)?;
    } else if !output.quiet {
        let mut stdout = std::io::stdout();
        render_shortlog_output(&shortlog_output, &mut stdout)?;
    }

    Ok(())
}

fn color_enabled_for_output(output: &OutputConfig) -> bool {
    match output.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::io::stdout().is_terminal(),
    }
}

async fn run_shortlog(args: &ShortlogArgs, color_enabled: bool) -> CliResult<ShortlogOutput> {
    // Pipe-input mode (matching `git log | git shortlog`): with no explicit
    // revision and a non-interactive stdin that carries data, summarise the
    // piped `git log` output instead of walking the repository. An empty /
    // terminal stdin falls back to the `HEAD` default (an intentional
    // convenience over Git, which has no default revision); see the command
    // docs. Walk-only options (`--since`/`--until`/`--merges`/`--no-merges`/
    // `--format`) have no commits to act on here and are ignored, as in Git.
    if args.revision.is_none() && !io::stdin().is_terminal() {
        let mut buffer = String::new();
        io::stdin().read_to_string(&mut buffer).map_err(|err| {
            CliError::fatal(format!("failed to read standard input: {err}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        if !buffer.trim().is_empty() {
            let group_mode = resolve_group_mode(args)?;
            let wrap = parse_wrap_spec(args.wrap.as_deref())?;
            let commits = parse_shortlog_stdin(&buffer);
            return Ok(aggregate_shortlog_stdin(args, &group_mode, commits, wrap));
        }
    }

    let since_ts = parse_shortlog_date_arg(args.since.as_deref(), "--since")?;
    let until_ts = parse_shortlog_date_arg(args.until.as_deref(), "--until")?;
    let revision = args.revision.clone().unwrap_or_else(|| "HEAD".to_string());
    let mut commits =
        get_commits_for_shortlog(args.revision.as_deref(), since_ts, until_ts).await?;

    // `--no-merges` drops merge commits (more than one parent) before
    // aggregation so the counts and totals reflect only non-merge commits.
    // `--merges` is the inverse, keeping only merge commits. The two override
    // each other (last one wins), so at most one filter applies.
    if args.no_merges {
        commits.retain(|commit| commit.parent_commit_ids.len() <= 1);
    } else if args.merges {
        commits.retain(|commit| commit.parent_commit_ids.len() >= 2);
    }

    // `--author=<pattern>` keeps only commits whose author identity contains the
    // pattern (case-insensitive), matched before aggregation.
    if let Some(pattern) = &args.author {
        let needle = pattern.to_lowercase();
        commits.retain(|commit| {
            author_identity_matches(&commit.author.name, &commit.author.email, &needle)
        });
    }

    let group_mode = resolve_group_mode(args)?;
    let wrap = parse_wrap_spec(args.wrap.as_deref())?;
    Ok(aggregate_shortlog(
        args,
        &group_mode,
        &revision,
        commits,
        wrap,
        color_enabled,
    ))
}

fn aggregate_shortlog(
    args: &ShortlogArgs,
    group_mode: &GroupMode,
    revision: &str,
    commits: Vec<Commit>,
    wrap: Option<(usize, usize, usize)>,
    color_enabled: bool,
) -> ShortlogOutput {
    let total_commits = commits.len();
    let mut author_map: HashMap<String, AuthorStats> = HashMap::new();

    // `--format`: render each commit with a custom template (the same renderer as
    // `libra log --format`) instead of its subject. The short-hash width matches
    // what `libra log` uses so `%h` is consistent across the two commands.
    let formatter = args.format.as_ref().map(|fmt| {
        CommitFormatter::new(FormatType::Custom(fmt.clone())).with_color_enabled(color_enabled)
    });
    let abbrev_len = util::get_min_unique_hash_length(&commits).max(7);

    for commit in commits {
        // Each commit contributes one identity for author/committer grouping,
        // or zero-or-more for trailer grouping (one per matching trailer value).
        let identities: Vec<(String, String)> = match group_mode {
            GroupMode::Author => vec![(commit.author.name.clone(), commit.author.email.clone())],
            GroupMode::Committer => {
                vec![(
                    commit.committer.name.clone(),
                    commit.committer.email.clone(),
                )]
            }
            GroupMode::Trailer(key) => extract_trailer_identities(&commit.message, key),
        };

        let subject = match &formatter {
            Some(formatter) => {
                let ctx = FormatContext {
                    graph_prefix: "",
                    decoration: "",
                    abbrev_len,
                    extra_hashes: "",
                };
                formatter.format(&commit, &ctx)
            }
            None => commit.format_message(),
        };

        for (author_name, author_email) in identities {
            let key = if args.email {
                format!("{} <{}>", author_name, author_email)
            } else {
                author_name.clone()
            };
            author_map
                .entry(key)
                .or_insert_with(|| AuthorStats::new(author_name.clone(), author_email.clone()))
                .add_commit(subject.clone());
        }
    }

    finalize_shortlog(args, author_map, revision, total_commits, wrap)
}

/// Shared tail of aggregation (used by both the repository-walk and the
/// standard-input paths): turn the per-identity `author_map` into a sorted,
/// filtered [`ShortlogOutput`] honouring `--numbered` / `--summary` / `--email`
/// / `--min-count` / `--reverse` / `--top`.
fn finalize_shortlog(
    args: &ShortlogArgs,
    author_map: HashMap<String, AuthorStats>,
    revision: &str,
    total_commits: usize,
    wrap: Option<(usize, usize, usize)>,
) -> ShortlogOutput {
    let mut authors: Vec<ShortlogAuthor> = author_map
        .into_values()
        .map(|stats| ShortlogAuthor {
            name: stats.name,
            email: args.email.then_some(stats.email),
            count: stats.count,
            subjects: if args.summary {
                Vec::new()
            } else {
                stats.subjects
            },
        })
        .collect();

    if args.numbered {
        authors.sort_by_key(|stats| (std::cmp::Reverse(stats.count), stats.name.to_lowercase()));
    } else {
        authors.sort_by_key(|stats| stats.name.to_lowercase());
    }

    // `--min-count` drops low-frequency identities, `--reverse` flips the
    // sorted order, and `--top` keeps only the leading N — applied in that
    // order so `--top` counts post-filter, post-reverse entries.
    if let Some(min_count) = args.min_count {
        authors.retain(|stats| stats.count >= min_count);
    }
    if args.reverse {
        authors.reverse();
    }
    if let Some(top) = args.top {
        authors.truncate(top);
    }

    ShortlogOutput {
        revision: revision.to_string(),
        numbered: args.numbered,
        summary: args.summary,
        email: args.email,
        total_authors: authors.len(),
        total_commits,
        authors,
        wrap,
    }
}

/// A single commit parsed from `git log` output supplied on standard input.
/// Only the fields Git's `shortlog` consumes from its stdin are kept: the
/// author/committer identities and the (de-indented) log message.
struct StdinCommit {
    author: Option<(String, String)>,
    committer: Option<(String, String)>,
    message: String,
}

/// Split a `Name <email>` identity string into `(name, email)`, tolerating a
/// missing angle-bracket address (the whole string becomes the name).
fn split_ident(ident: &str) -> (String, String) {
    let ident = ident.trim();
    if let Some(open) = ident.rfind(" <")
        && ident.ends_with('>')
    {
        let name = ident[..open].trim().to_string();
        let email = ident[open + 2..ident.len() - 1].to_string();
        return (name, email);
    }
    (ident.to_string(), String::new())
}

/// Parse the default (`medium`) or `fuller` `git log` output into per-commit
/// records. Records are delimited by `commit <hash>` lines; `Author:` /
/// `Commit:` header lines give the identities (the `:` distinguishes them from
/// `AuthorDate:` / `CommitDate:`), and 4-space-indented lines form the message
/// (its first line is the subject).
fn parse_shortlog_stdin(input: &str) -> Vec<StdinCommit> {
    let mut commits = Vec::new();
    let mut current: Option<StdinCommit> = None;
    let mut message_lines: Vec<String> = Vec::new();

    fn flush(
        current: &mut Option<StdinCommit>,
        message_lines: &mut Vec<String>,
        commits: &mut Vec<StdinCommit>,
    ) {
        if let Some(mut commit) = current.take() {
            commit.message = message_lines.join("\n").trim_end().to_string();
            commits.push(commit);
        }
        message_lines.clear();
    }

    for line in input.lines() {
        if line.starts_with("commit ") {
            flush(&mut current, &mut message_lines, &mut commits);
            current = Some(StdinCommit {
                author: None,
                committer: None,
                message: String::new(),
            });
        } else if let Some(rest) = line.strip_prefix("Author:") {
            current
                .get_or_insert_with(|| StdinCommit {
                    author: None,
                    committer: None,
                    message: String::new(),
                })
                .author = Some(split_ident(rest));
        } else if let Some(rest) = line.strip_prefix("Commit:") {
            current
                .get_or_insert_with(|| StdinCommit {
                    author: None,
                    committer: None,
                    message: String::new(),
                })
                .committer = Some(split_ident(rest));
        } else if let Some(body) = line.strip_prefix("    ") {
            // A message line (Git indents the log message by 4 spaces).
            if current.is_some() {
                message_lines.push(body.to_string());
            }
        } else if line.trim().is_empty() && !message_lines.is_empty() {
            // Preserve blank lines *within* a message (e.g. the subject/body
            // separator) so trailer grouping can still see the body, but drop
            // the blank lines that sit between the headers and the subject.
            message_lines.push(String::new());
        }
        // All other header lines (`Date:`, `AuthorDate:`, `Merge:`, …) are
        // irrelevant to the summary and ignored.
    }
    flush(&mut current, &mut message_lines, &mut commits);
    commits
}

/// Aggregate commits parsed from standard input. Mirrors the repository-walk
/// aggregation but draws identities and subjects from the parsed text: there is
/// no [`Commit`] object, so `--format` is not applied (the parsed subject is
/// used verbatim, as in Git). `--author` still filters by author identity even
/// when grouping by committer.
fn aggregate_shortlog_stdin(
    args: &ShortlogArgs,
    group_mode: &GroupMode,
    commits: Vec<StdinCommit>,
    wrap: Option<(usize, usize, usize)>,
) -> ShortlogOutput {
    let author_needle = args.author.as_ref().map(|pattern| pattern.to_lowercase());
    let mut author_map: HashMap<String, AuthorStats> = HashMap::new();
    // Count commits that survive the `--author` filter, matching the repo-walk
    // path (which filters before aggregation) so `total_commits` is consistent
    // across both modes — important for `--json` output.
    let mut total_commits = 0usize;

    for commit in commits {
        if let Some(needle) = &author_needle {
            let matches = commit
                .author
                .as_ref()
                .is_some_and(|(name, email)| author_identity_matches(name, email, needle));
            if !matches {
                continue;
            }
        }
        total_commits += 1;

        let subject = commit.message.lines().next().unwrap_or("").to_string();
        let identities: Vec<(String, String)> = match group_mode {
            GroupMode::Author => commit.author.into_iter().collect(),
            GroupMode::Committer => commit.committer.into_iter().collect(),
            GroupMode::Trailer(key) => extract_trailer_identities(&commit.message, key),
        };

        for (author_name, author_email) in identities {
            let key = if args.email {
                format!("{} <{}>", author_name, author_email)
            } else {
                author_name.clone()
            };
            author_map
                .entry(key)
                .or_insert_with(|| AuthorStats::new(author_name.clone(), author_email.clone()))
                .add_commit(subject.clone());
        }
    }

    finalize_shortlog(args, author_map, "(standard input)", total_commits, wrap)
}

fn render_shortlog_output(output: &ShortlogOutput, writer: &mut impl Write) -> CliResult<()> {
    let max_count = output
        .authors
        .iter()
        .map(|stats| stats.count)
        .max()
        .unwrap_or(0);
    let width = std::cmp::max(4, max_count.to_string().len());

    for stats in &output.authors {
        if output.email {
            if !write_shortlog_line(
                writer,
                format_args!(
                    "{:>width$}  {} <{}>",
                    stats.count,
                    stats.name,
                    stats.email.as_deref().unwrap_or(""),
                    width = width
                ),
            )? {
                return Ok(());
            }
        } else if !write_shortlog_line(
            writer,
            format_args!("{:>width$}  {}", stats.count, stats.name, width = width),
        )? {
            return Ok(());
        }

        if !output.summary {
            for subject in &stats.subjects {
                // A `--format` template may render multiple physical lines; each
                // is indented independently (a plain subject is always one line,
                // so this is a no-op for the default path).
                for physical in subject.split('\n') {
                    match output.wrap {
                        Some((width, indent1, indent2)) => {
                            for line in wrap_subject_lines(physical, width, indent1, indent2) {
                                if !write_shortlog_line(writer, format_args!("{line}"))? {
                                    return Ok(());
                                }
                            }
                        }
                        None => {
                            if !write_shortlog_line(writer, format_args!("      {}", physical))? {
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
    }

    Ok(())
}

async fn get_commits_for_shortlog(
    revision: Option<&str>,
    since_ts: Option<i64>,
    until_ts: Option<i64>,
) -> CliResult<Vec<Commit>> {
    use crate::command::log::get_reachable_commits;

    let revision = revision.unwrap_or("HEAD");
    let commit_hash = util::get_commit_base_typed(revision)
        .await
        .map_err(|error| shortlog_commit_base_error(revision, error))?
        .to_string();

    let mut commits: Vec<Commit> = get_reachable_commits(commit_hash, None)
        .await?
        .into_iter()
        .filter(|c| passes_filter(c, since_ts, until_ts))
        .collect();

    // newest first
    commits.sort_by_key(|c| std::cmp::Reverse(c.committer.timestamp));

    Ok(commits)
}

/// Case-insensitive substring match of a `shortlog --author` pattern against an
/// identity rendered as `name <email>`. `needle_lowercase` must already be
/// lowercased by the caller.
fn author_identity_matches(name: &str, email: &str, needle_lowercase: &str) -> bool {
    let identity = format!("{} <{}>", name.to_lowercase(), email.to_lowercase());
    identity.contains(needle_lowercase)
}

fn passes_filter(commit: &Commit, since_ts: Option<i64>, until_ts: Option<i64>) -> bool {
    let commit_ts = commit.committer.timestamp as i64;

    if let Some(since) = since_ts
        && commit_ts < since
    {
        return false;
    }

    if let Some(until) = until_ts
        && commit_ts > until
    {
        return false;
    }

    true
}

fn parse_shortlog_date_arg(value: Option<&str>, flag: &str) -> CliResult<Option<i64>> {
    value.map(parse_date).transpose().map_err(|error| {
        CliError::fatal(format!("invalid {flag} date: {error}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint(r#"supported formats: YYYY-MM-DD, "N days ago", unix timestamp"#)
    })
}

fn shortlog_commit_base_error(revision: &str, error: CommitBaseError) -> CliError {
    match error {
        CommitBaseError::HeadUnborn => CliError::fatal("HEAD does not point to a commit")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("create a commit before running 'libra shortlog'."),
        CommitBaseError::InvalidReference(message) => CliError::fatal(format!(
            "failed to resolve revision '{revision}': {message}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget),
        CommitBaseError::ReadFailure(message) => {
            CliError::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
        }
        CommitBaseError::CorruptReference(message) => {
            CliError::fatal(message).with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::{
        error::StableErrorCode,
        output::OutputConfig,
        test::{self, ChangeDirGuard},
    };

    #[test]
    fn test_parse_args() {
        let args = ShortlogArgs::parse_from(["shortlog"]);
        assert!(!args.numbered);
        assert!(!args.summary);
        assert!(!args.email);

        let args = ShortlogArgs::parse_from(["shortlog", "-n", "-s", "-e"]);
        assert!(args.numbered);
        assert!(args.summary);
        assert!(args.email);

        let args = ShortlogArgs::parse_from(["shortlog", "--since", "2024-01-01"]);
        assert!(args.since.is_some());

        let args = ShortlogArgs::parse_from(["shortlog", "--author", "Alice"]);
        assert_eq!(args.author.as_deref(), Some("Alice"));
    }

    #[test]
    fn test_author_identity_matches() {
        // Caller lowercases the needle; match is a case-insensitive substring
        // over "name <email>".
        assert!(author_identity_matches(
            "Alice Smith",
            "alice@x.com",
            "alice"
        ));
        assert!(author_identity_matches(
            "Bob",
            "bob@example.com",
            "example.com"
        ));
        assert!(!author_identity_matches("Carol", "carol@x.com", "alice"));
    }

    #[test]
    fn broken_pipe_writer_is_ignored() {
        struct BrokenPipeWriter;

        impl Write for BrokenPipeWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::BrokenPipe))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = BrokenPipeWriter;
        assert!(
            !write_shortlog_line(&mut writer, format_args!("alice")).unwrap(),
            "BrokenPipe should terminate output quietly"
        );
    }

    #[test]
    fn non_broken_pipe_writer_error_is_structured() {
        struct PermissionDeniedWriter;

        impl Write for PermissionDeniedWriter {
            fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
                Err(io::Error::from(io::ErrorKind::PermissionDenied))
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }

        let mut writer = PermissionDeniedWriter;
        let err = write_shortlog_line(&mut writer, format_args!("alice")).unwrap_err();
        assert_eq!(err.stable_code(), StableErrorCode::IoWriteFailed);
        assert!(err.message().contains("shortlog output error"));
    }

    #[tokio::test]
    #[serial]
    async fn execute_safe_requires_repository() {
        let temp = tempdir().unwrap();
        test::setup_clean_testing_env_in(temp.path());
        let _guard = ChangeDirGuard::new(temp.path());

        let err = execute_safe(
            ShortlogArgs::parse_from(["shortlog"]),
            &OutputConfig::default(),
        )
        .await
        .unwrap_err();

        assert_eq!(err.stable_code(), StableErrorCode::RepoNotFound);
    }
}
