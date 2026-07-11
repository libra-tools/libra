//! Log command rendering commit history with optional decorations, filtering, and custom formatting utilities.

pub(crate) mod config;

use std::{
    cell::RefCell,
    cmp::min,
    collections::{HashMap, HashSet, VecDeque},
    io::IsTerminal,
    path::PathBuf,
    rc::Rc,
    str::FromStr,
};

use clap::Parser;
use colored::Colorize;
use git_internal::{
    Diff,
    hash::ObjectHash,
    internal::object::{blob::Blob, commit::Commit, tree::Tree},
};
use serde::Serialize;

use self::config::{ResolvedLogConfig, resolve_log_config};
use crate::{
    command::load_object,
    common_utils::parse_commit_msg,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        head::Head,
        log::{
            date_parser::parse_date,
            formatter::{CommitFormatter, FormatContext, FormatType, LogPreset},
        },
        tag::{self, TagObject},
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{ColorChoice, OutputConfig, emit_json_data},
        pager::Pager,
        util,
    },
};

const LOG_EXAMPLES: &str = "\
EXAMPLES:
    libra log -n 5                         Show the latest 5 commits
    libra log --oneline --graph            Show a compact commit graph
    libra log --pretty=fuller              Use a named format preset (short/full/fuller/reference/raw)
    libra log --author alice                Filter commits by author (case-insensitive substring)
    libra log --since 24h --until 1h       Time-window filter (relative or RFC3339)
    libra log --grep 'fix(' -n 20          Filter commits by message substring
    libra log --grep fix -i                Case-insensitive message grep
    libra log --grep WIP --invert-grep     Hide commits whose message matches
    libra log --name-status src/           Show changed files under src/
    libra log --shortstat -n 5             Show just the diffstat summary line
    libra log --patch-with-stat -1         Diffstat block followed by the full patch
    libra log --oneline --parents          Show parent ids after each commit hash
    libra log --author-date-order          Order by author date instead of committer date
    libra --json log -n 1                  Structured JSON output for agents";

fn log_branch_store_error(context: &str, error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => {
            CliError::fatal(format!("failed to {context}: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        other => CliError::fatal(format!("failed to {context}: {other}"))
            .with_stable_code(StableErrorCode::RepoCorrupt),
    }
}

fn log_no_commits_error(branch_name: Option<&str>) -> CliError {
    let error = match branch_name {
        Some(name) => CliError::fatal(format!(
            "your current branch '{name}' does not have any commits yet"
        )),
        None => CliError::fatal("your current HEAD does not have any commits yet"),
    }
    .with_stable_code(StableErrorCode::RepoStateInvalid);

    error.with_hint("create a commit first before running 'libra log'.")
}

async fn resolve_log_head_commit() -> CliResult<(Option<String>, ObjectHash)> {
    let head = Head::current_result()
        .await
        .map_err(|error| log_branch_store_error("resolve HEAD", error))?;
    let branch_name = match head {
        Head::Branch(name) => Some(name),
        Head::Detached(_) => None,
    };

    if let Some(name) = &branch_name
        && Branch::find_branch_result(name, None)
            .await
            .map_err(|error| log_branch_store_error("inspect the current branch", error))?
            .is_none()
    {
        return Err(log_no_commits_error(Some(name)));
    }

    let current_head_commit = Head::current_commit_result()
        .await
        .map_err(|error| log_branch_store_error("resolve HEAD commit", error))?
        .ok_or_else(|| log_no_commits_error(branch_name.as_deref()))?;

    Ok((branch_name, current_head_commit))
}

fn log_invalid_object_error(object: &str) -> CliError {
    CliError::fatal(format!("invalid object name: {object}"))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("check the revision name and try again")
}

fn log_repo_corrupt_error(message: impl Into<String>) -> CliError {
    CliError::fatal(message.into()).with_stable_code(StableErrorCode::RepoCorrupt)
}

#[derive(Parser, Debug)]
#[command(after_help = LOG_EXAMPLES)]
pub struct LogArgs {
    /// Limit the number of output (Git alias: `--max-count`)
    #[clap(short, long, visible_alias = "max-count")]
    pub number: Option<usize>,
    /// Shorthand for --pretty=oneline --abbrev-commit
    #[clap(long)]
    pub oneline: bool,

    /// Show abbreviated commit hash instead of full hash
    #[clap(long)]
    pub abbrev_commit: bool,
    /// Number of hex digits for abbreviated commit hash (default: dynamically computed, min 7)
    #[clap(long, value_name = "N")]
    pub abbrev: Option<usize>,
    /// Show full hash
    #[clap(long)]
    pub no_abbrev_commit: bool,

    /// Print the parent commit ids after each commit hash (Git's `--parents`).
    #[clap(long, conflicts_with = "children")]
    pub parents: bool,
    /// Print the child commit ids (within the shown range) after each commit
    /// hash (Git's `--children`).
    #[clap(long, conflicts_with = "parents")]
    pub children: bool,

    /// Show diffs for each commit (like git -p)
    #[clap(short = 'p', long = "patch")]
    pub patch: bool,
    /// Show only names of changed files
    #[clap(long)]
    pub name_only: bool,
    /// Show names and status of changed files
    #[clap(long)]
    pub name_status: bool,
    /// Use NUL separators for log records and changed-path output
    #[clap(short = 'z', long = "null")]
    pub null: bool,
    /// Filter commits by author name or email (case-insensitive substring match)
    #[clap(long, value_name = "PATTERN")]
    pub author: Option<String>,
    /// Show commits more recent than DATE (RFC3339, `YYYY-MM-DD`, or relative like `24h` / `7d`)
    #[clap(long, value_name = "DATE")]
    pub since: Option<String>,
    /// Show commits older than DATE (RFC3339, `YYYY-MM-DD`, or relative like `1h`)
    #[clap(long, value_name = "DATE")]
    pub until: Option<String>,
    /// Custom pretty format string (e.g. `%h - %s`)
    #[clap(long, value_name = "FORMAT")]
    pub pretty: Option<String>,
    /// Alias for `--pretty=<format>` (Git's `--format`). Accepts the same preset
    /// names and `%`-placeholder templates as `--pretty`.
    #[clap(long, value_name = "FORMAT", conflicts_with = "pretty")]
    pub format: Option<String>,
    /// Date rendering mode for author/committer dates: default / short / iso /
    /// iso-strict / rfc / unix / raw.
    #[clap(long, value_name = "FORMAT")]
    pub date: Option<String>,
    /// Print out ref names of any commits that are shown
    #[clap(
        long,
        default_missing_value = "short",
        require_equals = true,
        num_args = 0..=1,
    )]
    pub decorate: Option<String>,
    /// Do not print out ref names of any commits that are shown
    #[clap(long)]
    pub no_decorate: bool,
    /// Draw a text-based graphical representation of the commit history
    #[clap(long)]
    pub graph: bool,
    /// Show diffstat (file change statistics) for each commit
    #[clap(long)]
    pub stat: bool,

    /// Show only the summary line of the diffstat (files changed, insertions,
    /// deletions) for each commit
    #[clap(long)]
    pub shortstat: bool,

    /// Show the diffstat block followed by the full patch for each commit
    /// (Git's synonym for `-p --stat`).
    #[clap(long = "patch-with-stat")]
    pub patch_with_stat: bool,

    /// Positional `[<revision-range>...] [<path>...]`: leading arguments are
    /// revisions (a single rev, or a range `A..B` / `A...B` / `^A`) until the
    /// first one that is not, after which the rest are pathspecs limiting diff
    /// output. A bare name that is both a valid revision and an existing path is
    /// rejected as ambiguous — use `--range` to force the revision.
    #[clap(value_name = "REVISION_OR_PATH", num_args = 0..)]
    pathspec: Vec<String>,

    /// Filter commits whose message contains PATTERN (case-sensitive substring match)
    #[clap(long, value_name = "PATTERN")]
    pub grep: Option<String>,

    /// Only list commits whose trailer block carries this trailer (Libra
    /// extension — Git has no such flag; nearest is a fragile
    /// `--grep='^Key: '`). `KEY` matches ASCII case-insensitively; an optional
    /// `=VALUE` requires an exact (case-sensitive) unfolded value. Repeatable;
    /// every `--trailer` must match (AND, like the other filters).
    #[clap(long = "trailer", value_name = "KEY[=VALUE]")]
    pub trailers: Vec<String>,

    /// Show only each commit's trailer block instead of the message (Libra
    /// extension; nearest Git equivalent `--pretty='%(trailers)'`). Combined
    /// with `--trailer`, only the selected keys are shown. Does not filter on
    /// its own; no-trailer commits print with an empty message section.
    #[clap(long = "only-trailers", conflicts_with_all = ["oneline", "pretty", "format"])]
    pub only_trailers: bool,

    /// Match `--grep` case-insensitively. (Author/committer matching is already
    /// case-insensitive in Libra.)
    #[clap(short = 'i', long = "regexp-ignore-case")]
    pub ignore_case: bool,

    /// Keep commits whose message does NOT match `--grep`.
    #[clap(long = "invert-grep")]
    pub invert_grep: bool,

    /// Filter commits by committer name or email (case-insensitive substring match)
    #[clap(long, value_name = "PATTERN")]
    pub committer: Option<String>,

    /// Show only merge commits (those with at least two parents).
    #[clap(long, conflicts_with = "no_merges")]
    pub merges: bool,

    /// Hide merge commits (those with at least two parents).
    #[clap(long)]
    pub no_merges: bool,

    /// Show only commits with at least N parents.
    #[clap(long, value_name = "N")]
    pub min_parents: Option<usize>,

    /// Show only commits with at most N parents.
    #[clap(long, value_name = "N")]
    pub max_parents: Option<usize>,

    /// Follow only the first parent of merge commits when traversing history.
    #[clap(long)]
    pub first_parent: bool,

    /// Pickaxe: show commits that change the number of occurrences of STRING
    /// in the files they touch (Git's `-S`).
    #[clap(short = 'S', value_name = "STRING", conflicts_with = "pickaxe_regex")]
    pub pickaxe_string: Option<String>,

    /// Pickaxe: show commits whose diff contains an added/removed line matching
    /// the given regex (Git's `-G`).
    #[clap(short = 'G', value_name = "REGEX")]
    pub pickaxe_regex: Option<String>,

    /// Skip the first N matching commits before printing.
    #[clap(long, value_name = "N")]
    pub skip: Option<usize>,

    /// Show commits in reverse order (oldest first).
    #[clap(long)]
    pub reverse: bool,

    /// Order commits by author date instead of committer date (newest first).
    /// Libra sorts by timestamp without Git's additional topological constraint.
    #[clap(long = "author-date-order")]
    pub author_date_order: bool,

    /// Order commits by committer date (newest first). This is Libra's default,
    /// so the flag is accepted for Git parity and selects the default ordering;
    /// it conflicts with `--author-date-order`. Libra sorts purely by timestamp
    /// (no topological constraint).
    #[clap(long = "date-order", conflicts_with = "author_date_order")]
    pub date_order: bool,

    /// Do not expand tabs in the log message. Accepted for Git parity and is a
    /// no-op: Libra never expands tabs in commit messages (it prints them
    /// verbatim), so this already matches the default. (Git's opposite
    /// `--expand-tabs[=<n>]` is not implemented.)
    #[clap(long = "no-expand-tabs")]
    pub no_expand_tabs: bool,

    /// Do not show commit notes. Accepted for Git parity and is a no-op: Libra's
    /// log never displays notes inline, so this already matches the default.
    /// (Git's opposite `--notes[=<ref>]` is not implemented; use `libra notes
    /// show <commit>` to read a note.)
    #[clap(long = "no-notes")]
    pub no_notes: bool,

    /// Do not use a `.mailmap` to rewrite author/committer identities. Accepted
    /// for Git parity and is a no-op: Libra's log never applies a mailmap, so it
    /// already shows the raw recorded identities. (Git's opposite `--mailmap`
    /// is not implemented.)
    #[clap(long = "no-mailmap")]
    pub no_mailmap: bool,

    /// Do not display the GPG signature of signed commits. Accepted for Git
    /// parity and is a no-op: Libra's log never displays commit signatures
    /// inline, so it already matches the default. (Git's opposite
    /// `--show-signature` is not implemented.)
    #[clap(long = "no-show-signature")]
    pub no_show_signature: bool,

    /// Pretend as if all the refs in refs/, along with HEAD, are listed on the command line.
    #[clap(long)]
    pub all: bool,

    /// Show history of a single file, following renames across commits.
    #[clap(long, value_name = "FILE", conflicts_with = "no_follow")]
    pub follow: Option<String>,

    /// Do not follow renames, overriding `log.follow=true`.
    #[clap(long = "no-follow")]
    pub no_follow: bool,

    /// Trace the evolution of the line range in the given file.
    /// Format: `<start>,<end>:<file>` or `:funcname:<file>`.
    #[clap(short = 'L', value_name = "RANGE:FILE")]
    pub line_range: Vec<String>,

    /// Revision range expression: a single commit, or a range like A..B or A...B.
    /// Can be given multiple times. When omitted, defaults to HEAD.
    #[clap(long = "range", value_name = "SPEC")]
    pub ranges: Vec<String>,
}

#[derive(PartialEq, Debug)]
enum DecorateOptions {
    No,
    Short,
    Full,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangeType {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct FileChange {
    pub path: PathBuf,
    pub status: ChangeType,
}

#[derive(Debug)]
struct SelectedLogCommit {
    commit: Commit,
    cached_changes: Option<Vec<FileChange>>,
    path_filters: Vec<PathBuf>,
}

#[derive(Debug)]
struct TraversedLogCommit {
    commit: Commit,
    follow_paths: Option<Vec<PathBuf>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogFileChange {
    pub path: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogCommitEntry {
    pub hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub author_email: String,
    pub author_date: String,
    pub committer_name: String,
    pub committer_email: String,
    pub committer_date: String,
    pub subject: String,
    pub body: String,
    pub parents: Vec<String>,
    pub refs: Vec<String>,
    pub files: Vec<LogFileChange>,
    /// The commit's qualifying trailer block, parsed (empty when none; the
    /// `body` intentionally still contains the trailer lines inline so
    /// existing consumers are unaffected — additive field, lore.md §1.9).
    pub trailers: Vec<LogTrailerEntry>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogTrailerEntry {
    pub key: String,
    pub value: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogOutput {
    pub commits: Vec<LogCommitEntry>,
    pub total: Option<usize>,
}

/// Resolve `--merges`/`--no-merges`/`--min-parents`/`--max-parents` into
/// effective parent-count bounds. `--merges` means ≥2 parents; `--no-merges`
/// means ≤1 parent; the explicit `--min-parents`/`--max-parents` win.
fn resolve_parent_bounds(
    merges: bool,
    no_merges: bool,
    min_parents: Option<usize>,
    max_parents: Option<usize>,
) -> (Option<usize>, Option<usize>) {
    let min = min_parents.or(if merges { Some(2) } else { None });
    let max = max_parents.or(if no_merges { Some(1) } else { None });
    (min, max)
}

struct CommitFilter {
    author: Option<String>,
    committer: Option<String>,
    since: Option<i64>,
    until: Option<i64>,
    paths: Vec<PathBuf>,
    grep: Option<String>,
    /// `-i`/`--regexp-ignore-case`: case-insensitive `--grep` message match.
    grep_ignore_case: bool,
    /// `--invert-grep`: keep commits whose message does NOT match `--grep`.
    invert_grep: bool,
    min_parents: Option<usize>,
    max_parents: Option<usize>,
    /// Pickaxe filter (`-S` literal occurrence count, or `-G` diff-line regex).
    pickaxe: Option<PickaxeKind>,
    /// `--trailer KEY[=VALUE]` filters — all must match (AND).
    trailer_filters: Vec<TrailerFilter>,
}

/// One `--trailer KEY[=VALUE]` filter: key matched ASCII case-insensitively,
/// value (when given) matched exactly against the unfolded trailer value.
struct TrailerFilter {
    key: String,
    value: Option<String>,
}

/// Parse the repeatable `--trailer` flags. Split at the FIRST `=` (values may
/// contain `=`; keys cannot). An empty key is a usage error.
fn parse_trailer_filters(specs: &[String]) -> Result<Vec<TrailerFilter>, CliError> {
    specs
        .iter()
        .map(|spec| {
            let (key, value) = match spec.split_once('=') {
                Some((key, value)) => (key, Some(value.to_string())),
                None => (spec.as_str(), None),
            };
            if key.trim().is_empty() {
                return Err(CliError::fatal(format!(
                    "invalid --trailer '{spec}': the key must not be empty"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("example: --trailer Reviewed-by  or  --trailer Change-Id=I1234"));
            }
            Ok(TrailerFilter {
                key: key.trim().to_string(),
                value,
            })
        })
        .collect()
}

/// The two pickaxe modes. `StringCount` matches when a commit changes the number
/// of occurrences of the literal string in the files it touches (`-S`).
/// `DiffRegex` matches when an added/removed diff line matches the regex (`-G`).
enum PickaxeKind {
    StringCount(String),
    DiffRegex(regex::Regex),
}

impl CommitFilter {
    #[allow(clippy::too_many_arguments)]
    fn new(
        author: Option<String>,
        committer: Option<String>,
        since: Option<i64>,
        until: Option<i64>,
        paths: Vec<PathBuf>,
        grep: Option<String>,
        min_parents: Option<usize>,
        max_parents: Option<usize>,
        pickaxe: Option<PickaxeKind>,
    ) -> Self {
        Self {
            author: author.map(|s| s.to_lowercase()),
            committer: committer.map(|s| s.to_lowercase()),
            since,
            until,
            paths,
            grep,
            grep_ignore_case: false,
            invert_grep: false,
            min_parents,
            max_parents,
            pickaxe,
            trailer_filters: Vec::new(),
        }
    }

    /// Attach `--trailer` filters (Libra extension, lore.md §1.9).
    fn with_trailer_filters(mut self, trailer_filters: Vec<TrailerFilter>) -> Self {
        self.trailer_filters = trailer_filters;
        self
    }

    /// Apply `-i`/`--regexp-ignore-case` and `--invert-grep` to the `--grep`
    /// message filter. Author/committer matching is already case-insensitive
    /// (both sides are lower-cased), so `-i` only affects `--grep` here.
    fn with_grep_options(mut self, ignore_case: bool, invert_grep: bool) -> Self {
        self.grep_ignore_case = ignore_case;
        self.invert_grep = invert_grep;
        self
    }

    fn passes_non_path_filters(&self, commit: &Commit) -> bool {
        if let Some(author_filter) = &self.author {
            let author = format!(
                "{} <{}>",
                commit.author.name.to_lowercase(),
                commit.author.email.to_lowercase()
            );
            if !author.contains(author_filter) {
                return false;
            }
        }

        if let Some(committer_filter) = &self.committer {
            let committer = format!(
                "{} <{}>",
                commit.committer.name.to_lowercase(),
                commit.committer.email.to_lowercase()
            );
            if !committer.contains(committer_filter) {
                return false;
            }
        }

        if !self.trailer_filters.is_empty() {
            let trailers = crate::internal::log::trailer::parse_trailers(&commit.message);
            for filter in &self.trailer_filters {
                let matched = trailers.iter().any(|trailer| {
                    trailer.key_matches(&filter.key)
                        && filter
                            .value
                            .as_deref()
                            .is_none_or(|value| trailer.value == value)
                });
                if !matched {
                    return false;
                }
            }
        }

        let parent_count = commit.parent_commit_ids.len();
        if let Some(min) = self.min_parents
            && parent_count < min
        {
            return false;
        }
        if let Some(max) = self.max_parents
            && parent_count > max
        {
            return false;
        }

        let ts = commit.committer.timestamp as i64;
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

        if let Some(pattern) = &self.grep
            && !pattern.is_empty()
        {
            let matches = if self.grep_ignore_case {
                commit
                    .message
                    .to_lowercase()
                    .contains(&pattern.to_lowercase())
            } else {
                commit.message.contains(pattern.as_str())
            };
            // `--invert-grep` keeps the non-matching commits: exclude exactly
            // when `matches == invert_grep` (matches & !invert, or !matches & invert).
            if matches == self.invert_grep {
                return false;
            }
        }

        true
    }

    async fn matches_paths(
        &self,
        commit: &Commit,
        cached_changes: Option<&[FileChange]>,
    ) -> Result<bool, CliError> {
        if self.paths.is_empty() {
            return Ok(true);
        }

        if let Some(changes) = cached_changes {
            Ok(!changes.is_empty())
        } else {
            commit_touches_paths(commit, &self.paths).await
        }
    }

    async fn matches(
        &self,
        commit: &Commit,
        cached_changes: Option<&[FileChange]>,
    ) -> Result<bool, CliError> {
        if !self.passes_non_path_filters(commit) {
            return Ok(false);
        }

        if let Some(pickaxe) = &self.pickaxe {
            let matched = match pickaxe {
                PickaxeKind::StringCount(needle) => commit_changes_string_count(commit, needle)?,
                PickaxeKind::DiffRegex(regex) => commit_diff_matches_regex(commit, regex)?,
            };
            if !matched {
                return Ok(false);
            }
        }

        self.matches_paths(commit, cached_changes).await
    }
}

fn str_to_decorate_option(s: &str) -> Result<DecorateOptions, String> {
    match s {
        "no" => Ok(DecorateOptions::No),
        "short" => Ok(DecorateOptions::Short),
        "full" => Ok(DecorateOptions::Full),
        "auto" => {
            if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
                Ok(DecorateOptions::Short)
            } else {
                Ok(DecorateOptions::No)
            }
        }
        _ => Err(s.to_owned()),
    }
}

async fn determine_decorate_option(args: &LogArgs) -> Result<DecorateOptions, String> {
    let arg_deco = args
        .decorate
        .as_ref()
        .map(|s| str_to_decorate_option(s))
        .transpose()?;

    match arg_deco {
        Some(a) => {
            if args.no_decorate {
                let args_os = std::env::args_os().peekable();
                for arg in args_os {
                    if arg == "--no-decorate" {
                        return Ok(a);
                    } else if arg.to_str().unwrap_or_default().starts_with("--decorate") {
                        return Ok(DecorateOptions::No);
                    };
                }
            } else {
                return Ok(a);
            }
        }
        None => {
            if args.no_decorate {
                return Ok(DecorateOptions::No);
            }
        }
    };

    if let Some(config_deco) = ConfigKv::get("log.decorate")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .and_then(|s| str_to_decorate_option(&s).ok())
    {
        Ok(config_deco)
    } else {
        str_to_decorate_option("auto")
    }
}

/// Get all reachable commits from the given commit hash, up to a specified depth.
/// **didn't consider the order of the commits**
pub async fn get_reachable_commits(
    commit_hash: String,
    depth: Option<usize>,
) -> Result<Vec<Commit>, CliError> {
    let mut queue = VecDeque::new();
    let mut commit_set: HashSet<ObjectHash> = HashSet::new();
    let mut reachable_commits: Vec<Commit> = Vec::new();

    // Push the initial commit with depth 0
    let initial_hash =
        ObjectHash::from_str(&commit_hash).map_err(|_| log_invalid_object_error(&commit_hash))?;
    queue.push_back((initial_hash, 0)); // (commit_id, current_depth)

    while let Some((commit_id, current_depth)) = queue.pop_front() {
        // If we've already seen this commit, skip it
        if !commit_set.insert(commit_id) {
            continue;
        }

        let commit = load_object::<Commit>(&commit_id).map_err(|e| {
            log_repo_corrupt_error(format!("storage broken, object not found: {e}"))
        })?;

        // If depth is limited and the current depth exceeds the limit, skip further processing
        if let Some(max_depth) = depth
            && current_depth >= max_depth
        {
            continue;
        }

        // Add parent commits to the queue with incremented depth
        for parent_commit_id in &commit.parent_commit_ids {
            queue.push_back((*parent_commit_id, current_depth + 1));
        }

        // Add the current commit to the result list
        reachable_commits.push(commit);
    }
    Ok(reachable_commits)
}

// Ordered as they should appear in log
#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
enum ReferenceKind {
    Tag,    // decorate color = yellow
    Remote, // red
    Local,  // green
}

#[derive(PartialEq, Eq, PartialOrd, Ord, Clone)]
struct Reference {
    kind: ReferenceKind,
    name: String,
}

fn parse_date_arg(value: &str) -> CliResult<i64> {
    parse_date(value).map_err(|e| {
        CliError::command_usage(e.to_string())
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint(r#"supported formats: YYYY-MM-DD, "N days ago", unix timestamp"#)
    })
}

async fn resolve_decorate_option(args: &LogArgs) -> CliResult<DecorateOptions> {
    determine_decorate_option(args).await.map_err(|value| {
        CliError::command_usage(format!("invalid --decorate option: {value}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("valid options: no, short, full, auto")
    })
}

pub async fn execute(args: LogArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Resolve a commit-ish string to an ObjectHash.
async fn resolve_commitish(spec: &str) -> CliResult<ObjectHash> {
    util::get_commit_base_typed(spec).await.map_err(|e| {
        CliError::fatal(format!("invalid revision '{spec}': {e}"))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("use a commit hash, branch name, tag, or HEAD")
    })
}

/// Parsed revision expression.
#[derive(Debug)]
enum RevisionExpr {
    Single(ObjectHash),
    Range {
        /// Excluded start commit for A..B (None means no boundary).
        exclude: Option<ObjectHash>,
        /// Included end commit.
        include: ObjectHash,
    },
    /// `A...B`: the symmetric difference — commits reachable from either `left`
    /// or `right` but not from BOTH. Both sides are included; `common` is the full
    /// set of commits reachable from both (their shared history, already
    /// ancestor-closed), which is excluded. `common` is empty when the two sides
    /// have no common ancestor (disjoint histories), so both are shown in full,
    /// and it correctly covers criss-cross histories with multiple merge bases.
    SymmetricRange {
        left: ObjectHash,
        right: ObjectHash,
        common: Vec<ObjectHash>,
    },
}

/// Parse a single revision spec into a structured expression.
async fn parse_revision_expr(spec: &str) -> CliResult<RevisionExpr> {
    if let Some((left, right)) = spec.split_once("...") {
        let left = if left.is_empty() {
            resolve_commitish("HEAD").await?
        } else {
            resolve_commitish(left).await?
        };
        let right = if right.is_empty() {
            resolve_commitish("HEAD").await?
        } else {
            resolve_commitish(right).await?
        };
        // A...B is the symmetric difference: commits reachable from A or B but
        // not from BOTH. Compute the shared history as the intersection of the
        // two reachable sets (this is correct for criss-cross histories with
        // multiple merge bases, and empty for disjoint histories) and exclude it.
        let left_reachable = reachable_commit_ids(left).await?;
        let right_reachable = reachable_commit_ids(right).await?;
        let common: Vec<ObjectHash> = left_reachable
            .intersection(&right_reachable)
            .copied()
            .collect();
        Ok(RevisionExpr::SymmetricRange {
            left,
            right,
            common,
        })
    } else if let Some((left, right)) = spec.split_once("..") {
        let left = if left.is_empty() {
            resolve_commitish("HEAD").await?
        } else {
            resolve_commitish(left).await?
        };
        let right = if right.is_empty() {
            resolve_commitish("HEAD").await?
        } else {
            resolve_commitish(right).await?
        };
        Ok(RevisionExpr::Range {
            exclude: Some(left),
            include: right,
        })
    } else {
        Ok(RevisionExpr::Single(resolve_commitish(spec).await?))
    }
}

/// All commits reachable from `tip` (the commit itself plus every ancestor),
/// used to compute the shared history for an `A...B` symmetric difference.
async fn reachable_commit_ids(tip: ObjectHash) -> CliResult<HashSet<ObjectHash>> {
    let mut reachable: HashSet<ObjectHash> = HashSet::new();
    let mut queue: VecDeque<ObjectHash> = VecDeque::new();
    queue.push_back(tip);
    while let Some(commit_id) = queue.pop_front() {
        if !reachable.insert(commit_id) {
            continue;
        }
        let commit = load_object::<Commit>(&commit_id).map_err(|e| {
            log_repo_corrupt_error(format!("failed to load commit {commit_id}: {e}"))
        })?;
        for parent in &commit.parent_commit_ids {
            queue.push_back(*parent);
        }
    }
    Ok(reachable)
}

/// Whether a token using range syntax (`A..B` / `A...B`, or a leading `^`)
/// actually resolves as a revision expression. Used to distinguish a genuine
/// positional range from a pathspec that merely contains `..` (e.g. `../file`)
/// or `^`. Returns `false` on any resolution failure so the caller falls back to
/// treating the token as a path.
async fn revision_syntax_resolves(arg: &str) -> bool {
    // Lightweight endpoint check only — avoid the heavier merge-base/common-set
    // computation that full `parse_revision_expr` does for `A...B`. An empty
    // endpoint defaults to HEAD (e.g. `..B`, `A..`).
    async fn endpoint_resolves(endpoint: &str) -> bool {
        endpoint.is_empty() || resolve_commitish(endpoint).await.is_ok()
    }
    if let Some(rest) = arg.strip_prefix('^') {
        return resolve_commitish(rest).await.is_ok();
    }
    // `...` must be checked before `..` since the former contains the latter.
    if let Some((left, right)) = arg.split_once("...") {
        return endpoint_resolves(left).await && endpoint_resolves(right).await;
    }
    if let Some((left, right)) = arg.split_once("..") {
        return endpoint_resolves(left).await && endpoint_resolves(right).await;
    }
    resolve_commitish(arg).await.is_ok()
}

/// Split positional `log` arguments into revision specs and pathspecs, matching
/// Git's `log [<revision>...] [[--] <path>...]`. Leading arguments are revisions
/// until the first one that is not; from there everything remaining is a path.
///
/// An argument using range syntax (`A..B` / `A...B` or a leading `^`) is always a
/// revision — its resolution (and any error) is deferred to
/// [`resolve_log_start_commits`]. A bare name is a revision only if it resolves
/// to a commit; a bare name that resolves to a commit AND also names an existing
/// path is ambiguous and rejected (use `--range <rev>` to force the revision),
/// matching Git's refusal to guess.
async fn split_log_positionals(positionals: &[String]) -> CliResult<(Vec<String>, Vec<String>)> {
    let mut revisions = Vec::new();
    let mut paths = Vec::new();
    let mut in_paths = false;

    for arg in positionals {
        if in_paths {
            paths.push(arg.clone());
            continue;
        }

        // Range syntax (`A..B`/`A...B`, or a leading `^`): if it resolves, it is
        // a revision. If it does not resolve but names an existing path (e.g.
        // `../file` or a file literally named `foo..bar`), treat it as a
        // pathspec. Otherwise it is a typoed revision range — error rather than
        // silently filtering by a non-existent path.
        if arg.contains("..") || arg.starts_with('^') {
            if revision_syntax_resolves(arg).await {
                revisions.push(arg.clone());
            } else if std::path::Path::new(arg).exists() {
                in_paths = true;
                paths.push(arg.clone());
            } else {
                return Err(CliError::command_usage(format!(
                    "ambiguous argument '{arg}': unknown revision or path not in the working tree"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            continue;
        }

        // A bare token is a revision only if it resolves to a commit.
        match resolve_commitish(arg).await {
            Ok(_) => {
                if std::path::Path::new(arg).exists() {
                    return Err(CliError::command_usage(format!(
                        "ambiguous argument '{arg}': both a revision and a path; \
                         use '--range {arg}' to select the revision"
                    ))
                    .with_stable_code(StableErrorCode::CliInvalidArguments));
                }
                revisions.push(arg.clone());
            }
            Err(_) => {
                in_paths = true;
                paths.push(arg.clone());
            }
        }
    }

    Ok((revisions, paths))
}

/// The effective revision specs (`--range` plus positional revisions) and the
/// effective pathspecs, after splitting positional arguments Git-style.
async fn resolve_log_inputs(args: &LogArgs) -> CliResult<(Vec<String>, Vec<String>)> {
    let (positional_revisions, paths) = split_log_positionals(&args.pathspec).await?;
    let mut ranges = args.ranges.clone();
    ranges.extend(positional_revisions);
    Ok((ranges, paths))
}

fn configured_follow_path(paths: &[PathBuf], enabled: bool) -> Option<PathBuf> {
    (enabled && paths.len() == 1 && util::workdir_to_absolute(&paths[0]).is_file())
        .then(|| paths[0].clone())
}

/// Resolve the starting commit(s) and optional exclusion set from CLI arguments.
/// `ranges` carries the effective revision specs (`--range` plus any positional
/// revisions) computed by [`resolve_log_inputs`].
async fn resolve_log_start_commits(
    args: &LogArgs,
    ranges: &[String],
) -> CliResult<(Vec<ObjectHash>, Option<HashSet<ObjectHash>>)> {
    let mut includes = Vec::new();
    let mut excludes: HashSet<ObjectHash> = HashSet::new();

    if args.all {
        let all_refs = collect_all_reference_tips().await?;
        includes.extend(all_refs);
    } else if ranges.is_empty() {
        let head = resolve_log_head_commit().await?;
        includes.push(head.1);
    } else {
        for spec in ranges {
            // Support `^EXCLUDE` syntax.
            if let Some(excluded) = spec.strip_prefix('^') {
                let hash = resolve_commitish(excluded).await?;
                excludes.insert(hash);
            } else {
                match parse_revision_expr(spec).await? {
                    RevisionExpr::Single(h) => includes.push(h),
                    RevisionExpr::Range { exclude, include } => {
                        if let Some(ex) = exclude {
                            excludes.insert(ex);
                        }
                        includes.push(include);
                    }
                    RevisionExpr::SymmetricRange {
                        left,
                        right,
                        common,
                    } => {
                        includes.push(left);
                        includes.push(right);
                        excludes.extend(common);
                    }
                }
            }
        }
    }

    Ok((includes, Some(excludes)))
}

/// Collect the current tips of all local branches and tags.
async fn collect_all_reference_tips() -> CliResult<Vec<ObjectHash>> {
    let mut tips = Vec::new();
    let branches = Branch::list_branches_result(None)
        .await
        .map_err(|e| log_branch_store_error("list branches", e))?;
    for branch in branches {
        tips.push(branch.commit);
    }
    let tags = tag::list().await.map_err(|e| {
        CliError::fatal(format!("failed to list tags: {e}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for t in tags {
        if let TagObject::Commit(commit) = t.object {
            tips.push(commit.id);
        }
    }
    // Also include HEAD if it points to a commit.
    if let Some(head_commit) = Head::current_commit_result().await.ok().flatten() {
        tips.push(head_commit);
    }
    Ok(tips)
}

/// Get all reachable commits from any of the starting commits, excluding the given set.
async fn get_reachable_commits_excluding(
    starts: Vec<ObjectHash>,
    excludes: Option<HashSet<ObjectHash>>,
    depth: Option<usize>,
    first_parent: bool,
) -> Result<Vec<Commit>, CliError> {
    let mut queue: VecDeque<(ObjectHash, usize)> = VecDeque::new();
    let mut commit_set: HashSet<ObjectHash> = HashSet::new();
    let mut reachable_commits: Vec<Commit> = Vec::new();
    let exclude_tips = excludes.unwrap_or_default();

    // Expand the exclude tips to their full ancestor closure: a range like
    // `A..B` (or `^A B`) hides *everything reachable from A*, not just A itself,
    // so any commit reachable from an excluded tip must also be excluded. (The
    // closure ignores `--first-parent`/`depth`, which shape only the shown set.)
    let mut excludes: HashSet<ObjectHash> = HashSet::new();
    let mut exclude_queue: VecDeque<ObjectHash> = exclude_tips.into_iter().collect();
    while let Some(commit_id) = exclude_queue.pop_front() {
        if !excludes.insert(commit_id) {
            continue;
        }
        let commit = load_object::<Commit>(&commit_id).map_err(|e| {
            log_repo_corrupt_error(format!("storage broken, object not found: {e}"))
        })?;
        for parent_commit_id in &commit.parent_commit_ids {
            exclude_queue.push_back(*parent_commit_id);
        }
    }

    for start in starts {
        queue.push_back((start, 0));
    }

    while let Some((commit_id, current_depth)) = queue.pop_front() {
        if excludes.contains(&commit_id) || !commit_set.insert(commit_id) {
            continue;
        }

        let commit = load_object::<Commit>(&commit_id).map_err(|e| {
            log_repo_corrupt_error(format!("storage broken, object not found: {e}"))
        })?;

        if let Some(max_depth) = depth
            && current_depth >= max_depth
        {
            continue;
        }

        for (idx, parent_commit_id) in commit.parent_commit_ids.iter().enumerate() {
            // `--first-parent` follows only the first parent of merge commits,
            // collapsing merged side branches out of the traversal.
            if first_parent && idx > 0 {
                break;
            }
            queue.push_back((*parent_commit_id, current_depth + 1));
        }

        reachable_commits.push(commit);
    }
    Ok(reachable_commits)
}

/// Sort commits newest-first by committer date, or by author date when
/// `--author-date-order` is requested. Libra sorts purely by timestamp and does
/// not add Git's extra topological "no parent before its children" constraint.
fn sort_commits_newest_first(commits: &mut [Commit], by_author_date: bool) {
    if by_author_date {
        commits.sort_by_key(|c| std::cmp::Reverse(c.author.timestamp));
    } else {
        commits.sort_by_key(|c| std::cmp::Reverse(c.committer.timestamp));
    }
}

/// Parsed line-range specifier for `-L`.
#[derive(Debug)]
#[allow(dead_code)]
struct LineRange {
    start: usize,
    end: usize,
    file: PathBuf,
}

fn parse_line_range(spec: &str) -> CliResult<LineRange> {
    let parts: Vec<&str> = spec.rsplitn(2, ':').collect();
    if parts.len() != 2 {
        return Err(
            CliError::command_usage(format!("invalid -L format: '{spec}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use '<start>,<end>:<file>' or ':<funcname>:<file>'"),
        );
    }
    let file = PathBuf::from(parts[0]);
    let range_spec = parts[1];

    let (start, end) = if range_spec.starts_with(':') {
        // function-name syntax not supported yet; fall back to whole file
        (1, usize::MAX)
    } else if let Some((s, e)) = range_spec.split_once(',') {
        let start = s.parse::<usize>().map_err(|_| {
            CliError::command_usage(format!("invalid line number in -L: '{s}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        let end = e.parse::<usize>().map_err(|_| {
            CliError::command_usage(format!("invalid line number in -L: '{e}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        (start, end)
    } else {
        let line = range_spec.parse::<usize>().map_err(|_| {
            CliError::command_usage(format!("invalid line number in -L: '{range_spec}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        (line, line)
    };

    Ok(LineRange { start, end, file })
}

/// Detect whether a commit changed the given file path, following renames.
async fn commit_touches_path_follow(
    commit: &Commit,
    target: &PathBuf,
) -> Result<Option<PathBuf>, CliError> {
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load tree object: {e}")))?;
    let current_items: HashMap<PathBuf, ObjectHash> = tree.get_plain_items().into_iter().collect();
    let current_blob = current_items.get(target).copied();

    if commit.parent_commit_ids.is_empty() {
        return Ok(current_blob.map(|_| target.clone()));
    }

    let parent_commit = load_object::<Commit>(&commit.parent_commit_ids[0])
        .map_err(|e| log_repo_corrupt_error(format!("failed to load parent commit: {e}")))?;
    let parent_tree = load_object::<Tree>(&parent_commit.tree_id)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load parent tree: {e}")))?;
    let parent_items: HashMap<PathBuf, ObjectHash> =
        parent_tree.get_plain_items().into_iter().collect();
    let parent_blob = parent_items.get(target).copied();

    match (current_blob, parent_blob) {
        (Some(current), Some(parent)) => {
            return Ok((current != parent).then(|| target.clone()));
        }
        (None, Some(_)) => return Ok(Some(target.clone())),
        (None, None) => return Ok(None),
        (Some(_), None) => {}
    }

    // The path was added in this commit. An exact blob match at a different
    // parent path is the best-effort rename predecessor for the next step.
    let Some(current_blob) = current_blob else {
        return Ok(None);
    };
    let predecessor = parent_items
        .iter()
        .filter(|(path, hash)| {
            **path != *target && **hash == current_blob && !current_items.contains_key(*path)
        })
        .map(|(path, _)| path)
        .min()
        .cloned();
    if let Some(path) = predecessor {
        return Ok(Some(path));
    }
    Ok(Some(target.clone()))
}

/// Filter reachable commits for `--follow` and `-L` paths.
async fn apply_follow_and_line_filters(
    commits: Vec<Commit>,
    follow: &Option<PathBuf>,
    line_ranges: &[String],
) -> Result<Vec<TraversedLogCommit>, CliError> {
    let mut result = Vec::new();
    let mut current_path = follow.clone();
    let ranges: Vec<LineRange> = line_ranges
        .iter()
        .map(|s| parse_line_range(s))
        .collect::<Result<Vec<_>, _>>()?;

    for commit in commits {
        let (path_to_check, follow_paths) = if let Some(path) = &current_path {
            let commit_path = path.clone();
            let touched = commit_touches_path_follow(&commit, &commit_path).await?;
            if let Some(new_path) = touched {
                current_path = Some(new_path.clone());
                let mut paths = vec![commit_path.clone()];
                if new_path != commit_path {
                    paths.push(new_path.clone());
                }
                (Some(new_path), Some(paths))
            } else {
                continue;
            }
        } else {
            (None, None)
        };

        if !ranges.is_empty() {
            let Some(path) = path_to_check.as_ref() else {
                continue;
            };
            if !commit_affects_line_range(&commit, path, &ranges).await? {
                continue;
            }
        }

        result.push(TraversedLogCommit {
            commit,
            follow_paths,
        });
    }

    Ok(result)
}

/// Best-effort check whether a commit affected any of the requested line ranges.
async fn commit_affects_line_range(
    commit: &Commit,
    path: &PathBuf,
    ranges: &[LineRange],
) -> Result<bool, CliError> {
    let _ = ranges;
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load tree object: {e}")))?;
    let current_items: HashMap<PathBuf, ObjectHash> = tree.get_plain_items().into_iter().collect();

    let Some(current_hash) = current_items.get(path).copied() else {
        // File deleted; conservatively include if any range existed before.
        return Ok(true);
    };

    if commit.parent_commit_ids.is_empty() {
        return Ok(true);
    }

    let parent_commit = load_object::<Commit>(&commit.parent_commit_ids[0])
        .map_err(|e| log_repo_corrupt_error(format!("failed to load parent commit: {e}")))?;
    let parent_tree = load_object::<Tree>(&parent_commit.tree_id)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load parent tree: {e}")))?;
    let parent_items: HashMap<PathBuf, ObjectHash> =
        parent_tree.get_plain_items().into_iter().collect();

    let parent_hash = parent_items.get(path).copied();
    if parent_hash != Some(current_hash) {
        // Content changed; include unless we can prove the range is unchanged.
        // A precise check would require blame, so we include by default.
        return Ok(true);
    }

    Ok(false)
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Walks commit history applying filters (date range,
/// author, path) and renders formatted log output.
pub async fn execute_safe(args: LogArgs, output: &OutputConfig) -> CliResult<()> {
    let log_config = resolve_log_config(&args, !output.is_json()).await?;
    let decorate_option = resolve_decorate_option(&args).await?;

    if output.is_json() {
        let result = run_log(&args, &log_config).await?;
        return emit_json_data("log", &result, output);
    }

    let name_status = args.name_status;
    // Check parameter mutual exclusion: if both name flags and --patch are specified, prioritize the name display flags
    let name_only = args.name_only && !name_status;
    // `--patch-with-stat` is Git's synonym for `-p --stat`, so it enables the
    // patch view. The name flags still take precedence over any diff output.
    let patch = (args.patch || args.patch_with_stat) && !name_only && !name_status;
    // Emit the diffstat block before the patch when both are requested — via
    // `--patch-with-stat` or an explicit `-p --stat` combination.
    let stat_before_patch = patch && (args.stat || args.patch_with_stat);

    let since = args.since.as_deref().map(parse_date_arg).transpose()?;
    let until = args.until.as_deref().map(parse_date_arg).transpose()?;
    let (ranges, paths) = resolve_log_inputs(&args).await?;
    let path_filters: Vec<PathBuf> = paths.iter().map(util::to_workdir_path).collect();
    let configured_follow = configured_follow_path(&path_filters, log_config.follow);
    let effective_follow = args
        .follow
        .as_deref()
        .map(util::to_workdir_path)
        .or_else(|| configured_follow.clone());
    let selection_path_filters = if effective_follow.is_some() {
        Vec::new()
    } else {
        path_filters.clone()
    };
    let (min_parents, max_parents) = resolve_parent_bounds(
        args.merges,
        args.no_merges,
        args.min_parents,
        args.max_parents,
    );
    let pickaxe = build_pickaxe(&args)?;
    let filter = CommitFilter::new(
        args.author.clone(),
        args.committer.clone(),
        since,
        until,
        selection_path_filters,
        args.grep.clone(),
        min_parents,
        max_parents,
        pickaxe,
    )
    .with_grep_options(args.ignore_case, args.invert_grep)
    .with_trailer_filters(parse_trailer_filters(&args.trailers)?);

    let (branch_name, current_head_commit) = resolve_log_head_commit().await?;
    let (start_commits, excludes) = resolve_log_start_commits(&args, &ranges).await?;
    if start_commits.is_empty() {
        return Err(log_no_commits_error(branch_name.as_deref()));
    }

    let mut reachable_commits =
        get_reachable_commits_excluding(start_commits, excludes, None, args.first_parent).await?;
    // newest first
    sort_commits_newest_first(&mut reachable_commits, args.author_date_order);
    let default_abbrev = util::get_min_unique_hash_length(&reachable_commits).max(7);
    let mut traversed_commits =
        apply_follow_and_line_filters(reachable_commits, &effective_follow, &args.line_range)
            .await?;
    if args.reverse {
        traversed_commits.reverse();
    }

    let max_output_number = min(args.number.unwrap_or(usize::MAX), traversed_commits.len());
    let reuse_changed_files = name_only || name_status;
    let selected_commits = select_log_commits(
        traversed_commits,
        &filter,
        &path_filters,
        max_output_number,
        args.skip.unwrap_or(0),
        reuse_changed_files,
    )
    .await?;

    if output.quiet {
        return validate_selected_log_commits(
            &selected_commits,
            name_only,
            name_status,
            patch,
            // `--shortstat` reads the same per-commit stats as `--stat`, so it
            // must trigger the same quiet-mode validation of blob data.
            args.stat || args.shortstat,
        )
        .await;
    }

    let mut pager = Pager::with_config(output)?;

    let ref_commits = if decorate_option == DecorateOptions::No {
        HashMap::new()
    } else {
        create_reference_commit_map().await
    };
    let full_hash_len = current_head_commit.to_string().len();

    let format_type = if args.oneline {
        FormatType::Oneline
    } else if let Some(pretty) = args.pretty.clone() {
        parse_pretty_format(pretty)
    } else if let Some(format) = args.format.clone() {
        // `--format` is Git's alias for `--pretty=<format>`.
        parse_pretty_format(format)
    } else if let Some(pretty) = log_config.pretty.clone() {
        parse_pretty_format(pretty)
    } else {
        FormatType::Full
    };
    let color_enabled = match output.color {
        ColorChoice::Always => true,
        ColorChoice::Never => false,
        ColorChoice::Auto => std::io::stdout().is_terminal(),
    };
    let mut formatter = CommitFormatter::new(format_type)
        .with_date_mode(log_config.date.clone().unwrap_or_default())
        .with_color_enabled(color_enabled);
    if args.only_trailers {
        // Key-filter the display to the `--trailer` keys when given.
        let selected_keys: Vec<String> = parse_trailer_filters(&args.trailers)?
            .into_iter()
            .map(|filter| filter.key)
            .collect();
        formatter = formatter.with_only_trailers(selected_keys);
    }

    let mut graph_state = if args.graph {
        Some(GraphState::new())
    } else {
        None
    };
    // `medium` is the default full renderer and therefore retains the full
    // commit id. Other explicit/configured pretty formats keep the existing
    // abbreviated `%h` context.
    let pretty_requests_abbreviation =
        |pretty: Option<&String>| pretty.is_some_and(|value| value.as_str() != "medium");
    // Decide abbreviated hash length
    let abbrev_len = if args.no_abbrev_commit {
        full_hash_len
    } else if let Some(n) = args.abbrev {
        if n == 0 { default_abbrev } else { n }
    } else if args.abbrev_commit
        || args.oneline
        || pretty_requests_abbreviation(args.pretty.as_ref())
        || pretty_requests_abbreviation(args.format.as_ref())
        || pretty_requests_abbreviation(log_config.pretty.as_ref())
    {
        default_abbrev
    } else {
        full_hash_len
    };
    // For `--children`, map each shown commit to the shown commits that list it
    // as a parent (built before the loop consumes `selected_commits`).
    let child_map: std::collections::HashMap<String, Vec<String>> = if args.children {
        let visible: std::collections::HashSet<String> = selected_commits
            .iter()
            .map(|s| s.commit.id.to_string())
            .collect();
        let mut map: std::collections::HashMap<String, Vec<String>> =
            std::collections::HashMap::new();
        for selected in &selected_commits {
            let child = selected.commit.id.to_string();
            for parent in &selected.commit.parent_commit_ids {
                let parent_id = parent.to_string();
                if visible.contains(&parent_id) {
                    map.entry(parent_id).or_default().push(child.clone());
                }
            }
        }
        map
    } else {
        std::collections::HashMap::new()
    };

    for (index, selected) in selected_commits.into_iter().enumerate() {
        let SelectedLogCommit {
            commit,
            mut cached_changes,
            path_filters: commit_path_filters,
        } = selected;
        let ref_msg = if decorate_option != DecorateOptions::No {
            let mut ref_msgs: Vec<String> = vec![];
            if index == 0 {
                ref_msgs.push(if let Some(b_name) = &branch_name {
                    format!(
                        "{} -> {}{}",
                        "HEAD".cyan(),
                        (if decorate_option == DecorateOptions::Full {
                            "refs/heads/"
                        } else {
                            ""
                        })
                        .green(),
                        b_name.green()
                    )
                } else {
                    "HEAD".cyan().to_string()
                });
            };

            let mut refs = ref_commits.get(&commit.id).cloned().unwrap_or_default();
            refs.sort();

            ref_msgs.append(
                &mut refs
                    .iter()
                    .filter_map(|r| {
                        if r.kind == ReferenceKind::Local && Some(r.name.to_owned()) == branch_name
                        {
                            None
                        } else {
                            Some(match r.kind {
                                ReferenceKind::Tag => format!(
                                    "tag: {}{}",
                                    if decorate_option == DecorateOptions::Full {
                                        "refs/tags/"
                                    } else {
                                        ""
                                    },
                                    r.name
                                )
                                .yellow()
                                .to_string(),
                                ReferenceKind::Remote => format!(
                                    "{}{}",
                                    if decorate_option == DecorateOptions::Full {
                                        "refs/remotes/"
                                    } else {
                                        ""
                                    },
                                    r.name
                                )
                                .red()
                                .to_string(),
                                ReferenceKind::Local => format!(
                                    "{}{}",
                                    if decorate_option == DecorateOptions::Full {
                                        "refs/heads/"
                                    } else {
                                        ""
                                    },
                                    r.name
                                )
                                .green()
                                .to_string(),
                            })
                        }
                    })
                    .collect(),
            );
            ref_msgs.join(", ")
        } else {
            String::new()
        };

        let graph_prefix = if let Some(ref mut gs) = graph_state {
            gs.render(&commit)
        } else {
            String::new()
        };

        // `--parents`/`--children` append abbreviated ids after the commit hash;
        // children are limited to the commits in this log's rendered output (the
        // `child_map` is built over `selected_commits`), so out-of-range children
        // are not listed.
        let abbreviate = |id: &str| id.chars().take(abbrev_len).collect::<String>();
        let extra_hashes = if args.parents {
            commit
                .parent_commit_ids
                .iter()
                .map(|p| abbreviate(&p.to_string()))
                .collect::<Vec<_>>()
                .join(" ")
        } else if args.children {
            child_map
                .get(&commit.id.to_string())
                .map(|kids| {
                    kids.iter()
                        .map(|c| abbreviate(c))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default()
        } else {
            String::new()
        };

        let ctx = FormatContext {
            graph_prefix: &graph_prefix,
            decoration: &ref_msg,
            abbrev_len,
            extra_hashes: &extra_hashes,
        };
        let mut message = formatter.format(&commit, &ctx);

        if name_only || name_status {
            let changes = cached_changes.take().unwrap_or_default();
            append_changed_paths(&mut message, &changes, name_status, args.null);
        } else if patch {
            // Build the optional diffstat block first (stat, blank line, then the
            // patch), reusing the existing `--stat` and `-p` renderers in Git's
            // `--patch-with-stat` order.
            let mut block = String::new();
            if stat_before_patch {
                let stats = compute_commit_stat(&commit, commit_path_filters.clone()).await?;
                let stat_output = format_stat_output(&stats);
                if !stat_output.is_empty() {
                    block.push_str(stat_output.trim_end_matches('\n'));
                    block.push_str("\n\n");
                }
            }
            let patch_output = generate_diff(&commit, commit_path_filters.clone()).await?;
            block.push_str(&patch_output);
            if !block.is_empty() {
                if !message.ends_with('\n') {
                    message.push('\n');
                }
                message.push_str(&block);
            }
        } else if args.stat {
            let stats = compute_commit_stat(&commit, commit_path_filters.clone()).await?;
            let stat_output = format_stat_output(&stats);
            if !stat_output.is_empty() {
                if !message.ends_with('\n') {
                    message.push('\n');
                }
                message.push_str(&stat_output);
            }
        } else if args.shortstat {
            let stats = compute_commit_stat(&commit, commit_path_filters).await?;
            let shortstat_output = format_shortstat_output(&stats);
            if !shortstat_output.is_empty() {
                if !message.ends_with('\n') {
                    message.push('\n');
                }
                message.push_str(&shortstat_output);
            }
        }

        if args.null {
            if !(name_only || name_status) {
                message.push('\0');
            }
            pager.write_str(&message)?;
        } else {
            pager.write_line(&message)?;
        }
    }

    pager.finish()?;
    Ok(())
}

async fn run_log(args: &LogArgs, log_config: &ResolvedLogConfig) -> CliResult<LogOutput> {
    let since = args.since.as_deref().map(parse_date_arg).transpose()?;
    let until = args.until.as_deref().map(parse_date_arg).transpose()?;
    let (ranges, paths) = resolve_log_inputs(args).await?;
    let path_filters: Vec<PathBuf> = paths.iter().map(util::to_workdir_path).collect();
    let configured_follow = configured_follow_path(&path_filters, log_config.follow);
    let effective_follow = args
        .follow
        .as_deref()
        .map(util::to_workdir_path)
        .or_else(|| configured_follow.clone());
    let selection_path_filters = if effective_follow.is_some() {
        Vec::new()
    } else {
        path_filters.clone()
    };
    let (min_parents, max_parents) = resolve_parent_bounds(
        args.merges,
        args.no_merges,
        args.min_parents,
        args.max_parents,
    );
    let pickaxe = build_pickaxe(args)?;
    let filter = CommitFilter::new(
        args.author.clone(),
        args.committer.clone(),
        since,
        until,
        selection_path_filters,
        args.grep.clone(),
        min_parents,
        max_parents,
        pickaxe,
    )
    .with_grep_options(args.ignore_case, args.invert_grep)
    .with_trailer_filters(parse_trailer_filters(&args.trailers)?);

    let (branch_name, current_head_commit) = resolve_log_head_commit().await?;
    let (start_commits, excludes) = resolve_log_start_commits(args, &ranges).await?;
    if start_commits.is_empty() {
        return Err(log_no_commits_error(branch_name.as_deref()));
    }

    let mut reachable_commits =
        get_reachable_commits_excluding(start_commits, excludes, None, args.first_parent).await?;
    // newest first
    sort_commits_newest_first(&mut reachable_commits, args.author_date_order);
    let mut traversed_commits =
        apply_follow_and_line_filters(reachable_commits, &effective_follow, &args.line_range)
            .await?;
    if args.reverse {
        traversed_commits.reverse();
    }

    let max_output_number = min(args.number.unwrap_or(usize::MAX), traversed_commits.len());
    let include_total = args.number.is_none();
    let ref_commits = create_reference_commit_map().await;
    let mut commits = Vec::new();
    let mut total = 0usize;
    let skip = args.skip.unwrap_or(0);

    for traversed in traversed_commits {
        let commit = traversed.commit;
        let effective_path_filters = traversed.follow_paths.as_deref().unwrap_or(&path_filters);
        if !include_total && commits.len() >= max_output_number {
            break;
        }
        if !filter.passes_non_path_filters(&commit) {
            continue;
        }

        let files = get_changed_files_for_commit(&commit, effective_path_filters).await?;
        if !filter.matches(&commit, Some(&files)).await? {
            continue;
        }

        total += 1;
        // `--skip N`: drop the first N matching commits from the output.
        if total <= skip {
            continue;
        }
        if commits.len() >= max_output_number {
            continue;
        }

        let (parsed_message, _) = parse_commit_msg(&commit.message);
        let mut message_lines = parsed_message.lines();
        let subject = message_lines.next().unwrap_or("").to_string();
        let body = message_lines.collect::<Vec<_>>().join("\n");
        let trailers = crate::internal::log::trailer::parse_trailers(&commit.message)
            .into_iter()
            .map(|trailer| LogTrailerEntry {
                key: trailer.key,
                value: trailer.value,
            })
            .collect();
        let hash = commit.id.to_string();
        let short_hash = hash.get(..7).unwrap_or(&hash).to_string();

        commits.push(LogCommitEntry {
            hash,
            short_hash,
            author_name: commit.author.name.trim().to_string(),
            author_email: commit.author.email.trim().to_string(),
            author_date: format_log_timestamp(commit.author.timestamp as i64),
            committer_name: commit.committer.name.trim().to_string(),
            committer_email: commit.committer.email.trim().to_string(),
            committer_date: format_log_timestamp(commit.committer.timestamp as i64),
            subject,
            body,
            parents: commit
                .parent_commit_ids
                .iter()
                .map(ToString::to_string)
                .collect(),
            refs: collect_log_refs(
                &commit,
                &ref_commits,
                branch_name.as_deref(),
                Some(current_head_commit),
            ),
            trailers,
            files: files
                .into_iter()
                .map(|file| LogFileChange {
                    path: file.path.display().to_string(),
                    status: match file.status {
                        ChangeType::Added => "added",
                        ChangeType::Modified => "modified",
                        ChangeType::Deleted => "deleted",
                    }
                    .to_string(),
                })
                .collect(),
        });
    }

    Ok(LogOutput {
        commits,
        total: include_total.then_some(total),
    })
}

async fn select_log_commits(
    reachable_commits: Vec<TraversedLogCommit>,
    filter: &CommitFilter,
    path_filters: &[PathBuf],
    max_output_number: usize,
    skip: usize,
    keep_changed_files: bool,
) -> Result<Vec<SelectedLogCommit>, CliError> {
    let mut selected = Vec::new();
    let mut skipped = 0usize;

    for traversed in reachable_commits {
        let commit = traversed.commit;
        let effective_path_filters = traversed
            .follow_paths
            .unwrap_or_else(|| path_filters.to_vec());
        if selected.len() >= max_output_number {
            break;
        }
        if !filter.passes_non_path_filters(&commit) {
            continue;
        }

        let cached_changes = if filter.paths.is_empty() && !keep_changed_files {
            None
        } else {
            Some(get_changed_files_for_commit(&commit, &effective_path_filters).await?)
        };

        if !filter.matches(&commit, cached_changes.as_deref()).await? {
            continue;
        }

        // `--skip N`: drop the first N matching commits before collecting output.
        if skipped < skip {
            skipped += 1;
            continue;
        }

        selected.push(SelectedLogCommit {
            commit,
            cached_changes,
            path_filters: effective_path_filters,
        });
    }

    Ok(selected)
}

async fn validate_selected_log_commits(
    selected_commits: &[SelectedLogCommit],
    name_only: bool,
    name_status: bool,
    patch: bool,
    stat: bool,
) -> CliResult<()> {
    for selected in selected_commits {
        if name_only || name_status {
            if selected.cached_changes.is_none() {
                let _ =
                    get_changed_files_for_commit(&selected.commit, &selected.path_filters).await?;
            }
        } else if patch {
            let _ = generate_diff(&selected.commit, selected.path_filters.clone()).await?;
        } else if stat {
            let _ = compute_commit_stat(&selected.commit, selected.path_filters.clone()).await?;
        }
    }

    Ok(())
}

/// Pickaxe (`-S`) test: returns true when the number of occurrences of `needle`
/// in the files the commit touches differs from its first parent (i.e. the
/// commit added or removed instances of the string). An empty needle never
/// matches. Only files whose blob changed contribute to the delta.
fn commit_changes_string_count(commit: &Commit, needle: &str) -> Result<bool, CliError> {
    use std::collections::HashMap;

    if needle.is_empty() {
        return Ok(false);
    }

    fn load_tree_blobs(tree_id: &ObjectHash) -> Result<HashMap<PathBuf, ObjectHash>, CliError> {
        let tree = load_object::<Tree>(tree_id)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load tree {tree_id}: {e}")))?;
        Ok(tree.get_plain_items().into_iter().collect())
    }

    let new_blobs = load_tree_blobs(&commit.tree_id)?;
    let old_blobs = if let Some(parent_id) = commit.parent_commit_ids.first() {
        let parent = load_object::<Commit>(parent_id).map_err(|e| {
            log_repo_corrupt_error(format!("failed to load parent commit {parent_id}: {e}"))
        })?;
        load_tree_blobs(&parent.tree_id)?
    } else {
        HashMap::new()
    };

    let mut old_count = 0usize;
    let mut new_count = 0usize;
    let mut seen: HashSet<&PathBuf> = HashSet::new();
    for path in new_blobs.keys().chain(old_blobs.keys()) {
        if !seen.insert(path) {
            continue;
        }
        let old_hash = old_blobs.get(path);
        let new_hash = new_blobs.get(path);
        if old_hash == new_hash {
            continue; // unchanged file contributes nothing to the delta
        }
        if let Some(hash) = old_hash {
            old_count += count_string_occurrences(hash, needle)?;
        }
        if let Some(hash) = new_hash {
            new_count += count_string_occurrences(hash, needle)?;
        }
    }

    Ok(old_count != new_count)
}

/// Count non-overlapping occurrences of `needle` in a blob's lossy-UTF-8 content.
fn count_string_occurrences(hash: &ObjectHash, needle: &str) -> Result<usize, CliError> {
    let blob = load_object::<Blob>(hash)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load blob {hash}: {e}")))?;
    Ok(String::from_utf8_lossy(&blob.data).matches(needle).count())
}

/// Build the pickaxe filter from `-S`/`-G` (mutually exclusive at the clap
/// layer). `-G`'s regex is compiled here so an invalid pattern is a usage error.
fn build_pickaxe(args: &LogArgs) -> CliResult<Option<PickaxeKind>> {
    if let Some(string) = &args.pickaxe_string {
        Ok(Some(PickaxeKind::StringCount(string.clone())))
    } else if let Some(pattern) = &args.pickaxe_regex {
        let regex = regex::Regex::new(pattern).map_err(|e| {
            CliError::command_usage(format!("invalid -G regex '{pattern}': {e}"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        Ok(Some(PickaxeKind::DiffRegex(regex)))
    } else {
        Ok(None)
    }
}

/// Pickaxe (`-G`) test: returns true when an added or removed diff line (vs the
/// first parent) matches `regex`, in any file the commit touches.
fn commit_diff_matches_regex(commit: &Commit, regex: &regex::Regex) -> Result<bool, CliError> {
    use std::collections::HashMap;

    use git_internal::diff::{DiffOperation, compute_diff};

    fn load_tree_blobs(tree_id: &ObjectHash) -> Result<HashMap<PathBuf, ObjectHash>, CliError> {
        let tree = load_object::<Tree>(tree_id)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load tree {tree_id}: {e}")))?;
        Ok(tree.get_plain_items().into_iter().collect())
    }
    fn blob_lines(hash: &ObjectHash) -> Result<Vec<String>, CliError> {
        let blob = load_object::<Blob>(hash)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load blob {hash}: {e}")))?;
        Ok(String::from_utf8_lossy(&blob.data)
            .lines()
            .map(String::from)
            .collect())
    }

    let new_blobs = load_tree_blobs(&commit.tree_id)?;
    let old_blobs = if let Some(parent_id) = commit.parent_commit_ids.first() {
        let parent = load_object::<Commit>(parent_id).map_err(|e| {
            log_repo_corrupt_error(format!("failed to load parent commit {parent_id}: {e}"))
        })?;
        load_tree_blobs(&parent.tree_id)?
    } else {
        HashMap::new()
    };

    let mut seen: HashSet<&PathBuf> = HashSet::new();
    for path in new_blobs.keys().chain(old_blobs.keys()) {
        if !seen.insert(path) {
            continue;
        }
        let old_hash = old_blobs.get(path);
        let new_hash = new_blobs.get(path);
        if old_hash == new_hash {
            continue;
        }
        let old_lines = match old_hash {
            Some(hash) => blob_lines(hash)?,
            None => Vec::new(),
        };
        let new_lines = match new_hash {
            Some(hash) => blob_lines(hash)?,
            None => Vec::new(),
        };
        for op in compute_diff(&old_lines, &new_lines) {
            match op {
                DiffOperation::Insert { content, .. } => {
                    if regex.is_match(&content) {
                        return Ok(true);
                    }
                }
                DiffOperation::Delete { line } => {
                    if let Some(text) = old_lines.get(line.saturating_sub(1))
                        && regex.is_match(text)
                    {
                        return Ok(true);
                    }
                }
                DiffOperation::Equal { .. } => {}
            }
        }
    }
    Ok(false)
}

/// Interpret a `--pretty=<value>` argument. Recognizes the named presets
/// (`oneline`, `medium`, `short`, `full`, `fuller`, `reference`, `raw`) and the
/// `format:` / `tformat:` prefixes (which carry a custom template). `medium`
/// (and the empty value) map to the default full format — Git's default. Any
/// other value is treated as a raw custom template, matching the prior behavior.
pub(crate) fn parse_pretty_format(pretty: String) -> FormatType {
    match pretty.as_str() {
        "oneline" => FormatType::Oneline,
        "medium" | "" => FormatType::Full,
        "short" => FormatType::Preset(LogPreset::Short),
        "full" => FormatType::Preset(LogPreset::Full),
        "fuller" => FormatType::Preset(LogPreset::Fuller),
        "reference" => FormatType::Preset(LogPreset::Reference),
        "raw" => FormatType::Preset(LogPreset::Raw),
        _ => {
            if let Some(fmt) = pretty.strip_prefix("format:") {
                FormatType::Custom(fmt.to_string())
            } else if let Some(fmt) = pretty.strip_prefix("tformat:") {
                FormatType::Custom(fmt.to_string())
            } else {
                FormatType::Custom(pretty)
            }
        }
    }
}

fn format_log_timestamp(timestamp: i64) -> String {
    chrono::DateTime::from_timestamp(timestamp, 0)
        .map(|date| date.to_rfc3339())
        .unwrap_or_else(|| timestamp.to_string())
}

fn collect_log_refs(
    commit: &Commit,
    ref_commits: &HashMap<ObjectHash, Vec<Reference>>,
    head_branch: Option<&str>,
    current_head_commit: Option<ObjectHash>,
) -> Vec<String> {
    let mut refs = Vec::new();
    if current_head_commit == Some(commit.id) {
        if let Some(branch) = head_branch {
            refs.push(format!("HEAD -> {branch}"));
        } else {
            refs.push("HEAD".to_string());
        }
    }

    let mut extra_refs = ref_commits.get(&commit.id).cloned().unwrap_or_default();
    extra_refs.sort();
    for reference in extra_refs {
        if reference.kind == ReferenceKind::Local && Some(reference.name.as_str()) == head_branch {
            continue;
        }

        refs.push(match reference.kind {
            ReferenceKind::Tag => format!("tag: {}", reference.name),
            ReferenceKind::Remote | ReferenceKind::Local => reference.name,
        });
    }

    refs
}

fn load_commit_blob_content(hash: &ObjectHash) -> Result<Vec<u8>, CliError> {
    load_object::<Blob>(hash)
        .map(|blob| blob.data)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load blob object {hash}: {e}")))
}

fn record_commit_diff_error(slot: &Rc<RefCell<Option<CliError>>>, error: CliError) {
    let mut slot = slot.borrow_mut();
    if slot.is_none() {
        *slot = Some(error);
    }
}

fn build_commit_diff_items(
    old_blobs: Vec<(PathBuf, ObjectHash)>,
    new_blobs: Vec<(PathBuf, ObjectHash)>,
    paths: Vec<PathBuf>,
) -> Result<Vec<git_internal::diff::DiffItem>, CliError> {
    let load_error = Rc::new(RefCell::new(None::<CliError>));
    let load_error_for_read = Rc::clone(&load_error);
    let diffs = Diff::diff(
        old_blobs,
        new_blobs,
        paths.into_iter().collect(),
        move |_file, hash| match load_commit_blob_content(hash) {
            Ok(blob) => blob,
            Err(error) => {
                record_commit_diff_error(&load_error_for_read, error);
                Vec::new()
            }
        },
    );
    if let Some(error) = load_error.borrow_mut().take() {
        return Err(error);
    }
    Ok(diffs)
}

async fn commit_touches_paths(commit: &Commit, filters: &[PathBuf]) -> Result<bool, CliError> {
    if filters.is_empty() {
        return Ok(true);
    }
    let changes = get_changed_files_for_commit(commit, filters).await?;
    Ok(!changes.is_empty())
}

/// Get list of changed files for a commit
pub(crate) async fn get_changed_files_for_commit(
    commit: &Commit,
    paths: &[PathBuf],
) -> Result<Vec<FileChange>, CliError> {
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load tree object: {e}")))?;
    let new_blobs: Vec<(PathBuf, ObjectHash)> = tree.get_plain_items();

    let old_blobs: Vec<(PathBuf, ObjectHash)> = if !commit.parent_commit_ids.is_empty() {
        let parent = &commit.parent_commit_ids[0];
        let parent_commit = load_object::<Commit>(parent)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load parent commit: {e}")))?;
        let parent_tree = load_object::<Tree>(&parent_commit.tree_id)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load parent tree: {e}")))?;
        parent_tree.get_plain_items()
    } else {
        Vec::new()
    };

    let matches_filter = |path: &PathBuf, filters: &[PathBuf]| -> bool {
        if filters.is_empty() {
            return true;
        }
        filters.iter().any(|filter| util::is_sub_path(path, filter))
    };

    let old_files: HashSet<PathBuf> = old_blobs.iter().map(|(path, _)| path.clone()).collect();
    let new_files: HashSet<PathBuf> = new_blobs.iter().map(|(path, _)| path.clone()).collect();

    let mut changed_files = Vec::new();

    for file in &new_files {
        if !old_files.contains(file) && matches_filter(file, paths) {
            changed_files.push(FileChange {
                path: file.clone(),
                status: ChangeType::Added,
            });
        }
    }

    for (file, new_hash) in &new_blobs {
        if let Some((_, old_hash)) = old_blobs.iter().find(|(old_file, _)| old_file == file)
            && new_hash != old_hash
            && matches_filter(file, paths)
        {
            changed_files.push(FileChange {
                path: file.clone(),
                status: ChangeType::Modified,
            });
        }
    }

    for file in &old_files {
        if !new_files.contains(file) && matches_filter(file, paths) {
            changed_files.push(FileChange {
                path: file.clone(),
                status: ChangeType::Deleted,
            });
        }
    }

    changed_files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(changed_files)
}

fn append_changed_paths(
    message: &mut String,
    changes: &[FileChange],
    include_status: bool,
    null: bool,
) {
    if null {
        message.push('\0');
        if !changes.is_empty() {
            message.push('\n');
            message.push_str(&format_changes(changes, include_status, true));
        }
    } else if !changes.is_empty() {
        if !message.ends_with('\n') {
            message.push('\n');
        }
        message.push('\n');
        message.push_str(&format_changes(changes, include_status, false));
    }
}

fn format_changes(changes: &[FileChange], include_status: bool, null: bool) -> String {
    let mut out = String::new();
    for change in changes {
        if include_status {
            let status = match change.status {
                ChangeType::Added => "A",
                ChangeType::Modified => "M",
                ChangeType::Deleted => "D",
            };
            if null {
                out.push_str(status);
                out.push('\0');
                out.push_str(&change.path.display().to_string());
                out.push('\0');
            } else {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&format!("{}\t{}", status, change.path.display()));
            }
        } else {
            if null {
                out.push_str(&change.path.display().to_string());
                out.push('\0');
            } else {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&change.path.display().to_string());
            }
        }
    }
    out
}

/// Represents statistics about changes to a file in a commit.
///
/// This struct is used to report the number of lines inserted and deleted for a file
/// as part of a commit's diff. It is typically returned by functions that compute
/// per-file change statistics for a commit.
#[derive(Debug)]
pub struct FileStat {
    /// The path to the file relative to the repository root.
    pub path: String,
    /// The number of lines inserted in this file by the commit.
    pub insertions: usize,
    /// The number of lines deleted from this file by the commit.
    pub deletions: usize,
}

/// Computes file statistics (insertions and deletions) for a given commit by comparing it with its parent commit.
///
/// # Parameters
/// - `commit`: The commit to analyze.
/// - `paths`: A list of path filters (files or directories) to restrict the analysis; pass an empty vector for no filtering.
///
/// # Returns
/// A vector of [`FileStat`] structs, each containing the file path, number of insertions, and number of deletions.
pub async fn compute_commit_stat(
    commit: &Commit,
    paths: Vec<PathBuf>,
) -> Result<Vec<FileStat>, CliError> {
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load tree object: {e}")))?;
    let new_blobs: Vec<(PathBuf, ObjectHash)> = tree.get_plain_items();

    let old_blobs: Vec<(PathBuf, ObjectHash)> = if !commit.parent_commit_ids.is_empty() {
        let parent = &commit.parent_commit_ids[0];
        let parent_commit = load_object::<Commit>(parent)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load parent commit: {e}")))?;
        let parent_tree = load_object::<Tree>(&parent_commit.tree_id)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load parent tree: {e}")))?;
        parent_tree.get_plain_items()
    } else {
        Vec::new()
    };

    let diffs = build_commit_diff_items(old_blobs, new_blobs, paths)?;

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
    Ok(stats)
}

/// Formats a list of file statistics into a Git-style summary with colored bars.
///
/// Each file is displayed on its own line, showing the file path, the total number of changes,
/// and a visual bar: green `+` for insertions and red `-` for deletions. The bar's length is
/// proportional to the number of changes, up to a maximum width. At the end, a summary line
/// shows the total number of files changed, insertions, and deletions.
///
/// If `stats` is empty, returns an empty string.
pub fn format_stat_output(stats: &[FileStat]) -> String {
    const MAX_STAT_BAR_WIDTH: usize = 40;

    if stats.is_empty() {
        return String::new();
    }

    let mut output = String::new();
    let total_insertions: usize = stats.iter().map(|s| s.insertions).sum();
    let total_deletions: usize = stats.iter().map(|s| s.deletions).sum();
    let total_files = stats.len();

    for stat in stats {
        let changes = stat.insertions + stat.deletions;
        let bar_width = if changes > MAX_STAT_BAR_WIDTH {
            MAX_STAT_BAR_WIDTH
        } else {
            changes
        };

        let plus_count = (stat.insertions * bar_width)
            .checked_div(changes)
            .unwrap_or(0);
        let minus_count = bar_width.saturating_sub(plus_count);

        output.push_str(&format!(
            " {} | {:>3} {}{}\n",
            stat.path,
            changes,
            "+".repeat(plus_count).green(),
            "-".repeat(minus_count).red()
        ));
    }

    output.push_str(&format!(
        " {} file{} changed, {} insertion{}({}), {} deletion{}({})\n",
        total_files,
        if total_files == 1 { "" } else { "s" },
        total_insertions,
        if total_insertions == 1 { "" } else { "s" },
        "+".green(),
        total_deletions,
        if total_deletions == 1 { "" } else { "s" },
        "-".red()
    ));

    output
}

/// Render the `--shortstat` summary line for a commit's file statistics:
/// ` N file(s) changed[, M insertion(s)(+)][, K deletion(s)(-)]`, matching
/// `git log --shortstat` (the insertion/deletion clauses are omitted when
/// zero). Returns an empty string when there are no changes.
pub fn format_shortstat_output(stats: &[FileStat]) -> String {
    if stats.is_empty() {
        return String::new();
    }

    let total_files = stats.len();
    let total_insertions: usize = stats.iter().map(|s| s.insertions).sum();
    let total_deletions: usize = stats.iter().map(|s| s.deletions).sum();

    let mut summary = format!(
        " {} file{} changed",
        total_files,
        if total_files == 1 { "" } else { "s" }
    );
    if total_insertions > 0 {
        summary.push_str(&format!(
            ", {} insertion{}(+)",
            total_insertions,
            if total_insertions == 1 { "" } else { "s" }
        ));
    }
    if total_deletions > 0 {
        summary.push_str(&format!(
            ", {} deletion{}(-)",
            total_deletions,
            if total_deletions == 1 { "" } else { "s" }
        ));
    }
    summary.push('\n');
    summary
}

/// Maintains state for rendering an ASCII commit graph visualization.
///
/// `GraphState` tracks the columns representing active branches and parent/child relationships
/// as the commit history is traversed. It is designed to be created once and used to render
/// each commit in traversal order (e.g., topological or chronological), producing the correct
/// graph prefix for each commit line. The internal algorithm updates the columns vector to
/// reflect merges and branchings, ensuring the visual structure matches the commit graph.
#[derive(Default)]
pub struct GraphState {
    columns: Vec<Option<ObjectHash>>,
}

impl GraphState {
    /// Creates a new, empty `GraphState` for rendering a commit graph.
    pub fn new() -> Self {
        Self::default()
    }

    /// Renders the ASCII graph prefix for a given commit, updating internal state.
    ///
    /// Call this method for each commit in traversal order. It returns a string representing
    /// the graph structure (e.g., `* | |`) for the current commit, updating the internal
    /// columns to reflect parent/child relationships and merges.
    ///
    /// # Arguments
    ///
    /// * `commit` - The commit to render in the graph.
    ///
    /// # Returns
    ///
    /// A string containing the ASCII graph prefix for the commit.
    pub fn render(&mut self, commit: &Commit) -> String {
        let commit_id = commit.id;
        let parent_ids = &commit.parent_commit_ids;

        let mut prefix = String::new();

        if let Some(pos) = self.columns.iter().position(|&c| c == Some(commit_id)) {
            for (i, col) in self.columns.iter().enumerate() {
                if i == pos {
                    prefix.push_str("* ");
                } else if col.is_some() {
                    prefix.push_str("| ");
                } else {
                    prefix.push_str("  ");
                }
            }

            if parent_ids.is_empty() {
                self.columns[pos] = None;
            } else if parent_ids.len() == 1 {
                self.columns[pos] = Some(parent_ids[0]);
            } else {
                self.columns[pos] = Some(parent_ids[0]);

                for parent_id in parent_ids.iter().skip(1) {
                    self.columns.push(Some(*parent_id));
                }
            }
        } else {
            self.columns.insert(0, None);
            prefix.push_str("* ");
            for _ in 1..self.columns.len() {
                prefix.push_str("| ");
            }

            if !parent_ids.is_empty() {
                self.columns[0] = Some(parent_ids[0]);

                for parent_id in parent_ids.iter().skip(1) {
                    self.columns.push(Some(*parent_id));
                }
            }
        }

        self.columns.retain(|c| c.is_some());

        prefix
    }
}

async fn create_reference_commit_map() -> HashMap<ObjectHash, Vec<Reference>> {
    let mut commit_to_refs: HashMap<ObjectHash, Vec<Reference>> = HashMap::new();

    let all_branches = Branch::list_branches_best_effort(None).await;
    for branch in all_branches {
        commit_to_refs
            .entry(branch.commit)
            .or_default()
            .push(match &branch.remote {
                Some(remote) => Reference {
                    name: format!("{}/{}", remote, branch.name),
                    kind: ReferenceKind::Remote,
                },
                None => Reference {
                    name: branch.name,
                    kind: ReferenceKind::Local,
                },
            });
    }

    let all_tags = tag::list().await.unwrap_or_else(|e| {
        tracing::warn!("failed to list tags for log decoration: {e}");
        Vec::new()
    });
    for tag in all_tags {
        let commit_id = match tag.object {
            TagObject::Commit(c) => c.id,
            TagObject::Tag(t) => t.object_hash,
            _ => continue,
        };
        commit_to_refs
            .entry(commit_id)
            .or_default()
            .push(Reference {
                name: tag.name,
                kind: ReferenceKind::Tag,
            });
    }

    commit_to_refs
}

/// Generate unified diff between commit and its first parent (or empty tree)
pub(crate) async fn generate_diff(
    commit: &Commit,
    paths: Vec<PathBuf>,
) -> Result<String, CliError> {
    // prepare old and new blobs
    // new_blobs from commit tree
    let tree = load_object::<Tree>(&commit.tree_id)
        .map_err(|e| log_repo_corrupt_error(format!("failed to load tree object: {e}")))?;
    let new_blobs: Vec<(PathBuf, ObjectHash)> = tree.get_plain_items();

    // old_blobs from first parent if exists
    let old_blobs: Vec<(PathBuf, ObjectHash)> = if !commit.parent_commit_ids.is_empty() {
        let parent = &commit.parent_commit_ids[0];
        let parent_commit = load_object::<Commit>(parent)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load parent commit: {e}")))?;
        let parent_tree = load_object::<Tree>(&parent_commit.tree_id)
            .map_err(|e| log_repo_corrupt_error(format!("failed to load parent tree: {e}")))?;
        parent_tree.get_plain_items()
    } else {
        Vec::new()
    };

    let diffs = build_commit_diff_items(old_blobs, new_blobs, paths)?;
    let mut out = String::new();
    for d in diffs {
        out.push_str(&d.data);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use clap::Parser;

    use super::*;

    // Test parameter parsing
    #[test]
    fn test_log_args_name_only() {
        // Test that the --name-only parameter is parsed correctly
        let args = LogArgs::parse_from(["libra", "--name-only"]);
        assert!(args.name_only);

        let args = LogArgs::parse_from(["libra"]);
        assert!(!args.name_only);
    }

    #[test]
    fn test_name_only_precedence_over_patch() {
        // Test --name-only takes precedence over --patch
        let args = LogArgs::parse_from(["libra", "--name-only", "--patch"]);
        assert!(args.name_only);
        assert!(args.patch);
        // In the execute function, patch should be ignored when name_only is true
    }

    #[test]
    fn test_name_only_with_oneline() {
        // Test --name-only and --oneline combination
        let args = LogArgs::parse_from(["libra", "--name-only", "--oneline"]);
        assert!(args.name_only);
        assert!(args.oneline);
    }

    #[test]
    fn test_name_only_with_number_limit() {
        // Test --name-only combined with quantity limit
        let args = LogArgs::parse_from(["libra", "--name-only", "-n", "5"]);
        assert!(args.name_only);
        assert_eq!(args.number, Some(5));
    }

    // Test decoration option parsing
    #[test]
    fn test_str_to_decorate_option() {
        assert_eq!(str_to_decorate_option("no").unwrap(), DecorateOptions::No);
        assert_eq!(
            str_to_decorate_option("short").unwrap(),
            DecorateOptions::Short
        );
        assert_eq!(
            str_to_decorate_option("full").unwrap(),
            DecorateOptions::Full
        );
        assert!(str_to_decorate_option("auto").is_ok());
        assert!(str_to_decorate_option("invalid").is_err());
    }

    // Test parameter combination
    #[test]
    fn test_complex_arg_combinations() {
        // Test multiple parameter combinations
        let args = LogArgs::parse_from(["libra", "--name-only", "--oneline", "-n", "10"]);
        assert!(args.name_only);
        assert!(args.oneline);
        assert_eq!(args.number, Some(10));

        let args = LogArgs::parse_from(["libra", "--name-only", "src/main.rs", "src/lib.rs"]);
        assert!(args.name_only);
        // Update expected pathspec value to include "log"
        assert_eq!(args.pathspec, vec!["src/main.rs", "src/lib.rs"]);
    }

    #[test]
    fn test_new_filters_parsing() {
        let args = LogArgs::parse_from([
            "libra",
            "--author",
            "lvy",
            "--since",
            "2025-12-19",
            "--until",
            "2025-12-19",
        ]);
        assert_eq!(args.author.as_deref(), Some("lvy"));
        assert_eq!(args.since.as_deref(), Some("2025-12-19"));
        assert_eq!(args.until.as_deref(), Some("2025-12-19"));
    }

    #[test]
    fn test_log_parent_and_committer_filters_parse() {
        let args = LogArgs::parse_from([
            "libra",
            "--committer",
            "bob",
            "--merges",
            "--min-parents",
            "1",
            "--max-parents",
            "3",
        ]);
        assert_eq!(args.committer.as_deref(), Some("bob"));
        assert!(args.merges);
        assert_eq!(args.min_parents, Some(1));
        assert_eq!(args.max_parents, Some(3));

        // --no-merges => at most 1 parent; --merges => at least 2 parents.
        assert_eq!(
            resolve_parent_bounds(false, true, None, None),
            (None, Some(1))
        );
        assert_eq!(
            resolve_parent_bounds(true, false, None, None),
            (Some(2), None)
        );
        // explicit bounds win over the flag shorthands.
        assert_eq!(
            resolve_parent_bounds(true, true, Some(0), Some(5)),
            (Some(0), Some(5))
        );
    }

    #[test]
    fn test_parse_pretty_format_presets() {
        assert!(matches!(
            parse_pretty_format("oneline".into()),
            FormatType::Oneline
        ));
        assert!(matches!(
            parse_pretty_format("medium".into()),
            FormatType::Full
        ));
        assert!(matches!(
            parse_pretty_format(String::new()),
            FormatType::Full
        ));
        match parse_pretty_format("format:%h %s".into()) {
            FormatType::Custom(t) => assert_eq!(t, "%h %s"),
            _ => panic!("format: prefix should map to a custom template"),
        }
        match parse_pretty_format("tformat:%H".into()) {
            FormatType::Custom(t) => assert_eq!(t, "%H"),
            _ => panic!("tformat: prefix should map to a custom template"),
        }
        match parse_pretty_format("%an".into()) {
            FormatType::Custom(t) => assert_eq!(t, "%an"),
            _ => panic!("a bare template should map to a custom template"),
        }
    }

    #[test]
    fn test_log_max_count_alias() {
        assert_eq!(
            LogArgs::parse_from(["libra", "--max-count", "3"]).number,
            Some(3)
        );
        assert_eq!(LogArgs::parse_from(["libra", "-n", "3"]).number, Some(3));
    }

    #[test]
    fn test_sort_commits_newest_first_author_vs_committer() {
        // A: author OLD (100), committer NEW (400).
        let mut a = Commit::from_tree_id(ObjectHash::new(&[1; 20]), vec![], "A");
        a.author.timestamp = 100;
        a.committer.timestamp = 400;
        // B: author NEW (200), committer OLD (300).
        let mut b = Commit::from_tree_id(ObjectHash::new(&[2; 20]), vec![], "B");
        b.author.timestamp = 200;
        b.committer.timestamp = 300;

        let mut commits = vec![a, b];
        // Default (committer date, newest first): A=400 > B=300 → A leads.
        sort_commits_newest_first(&mut commits, false);
        assert_eq!(commits[0].message, "A", "committer-date order");
        // --author-date-order: B=200 > A=100 → B leads (distinct from committer order).
        sort_commits_newest_first(&mut commits, true);
        assert_eq!(commits[0].message, "B", "author-date order");
    }

    #[test]
    fn test_build_pickaxe_modes() {
        let s = LogArgs::parse_from(["libra", "-S", "needle"]);
        assert!(matches!(
            build_pickaxe(&s).unwrap(),
            Some(PickaxeKind::StringCount(_))
        ));

        let g = LogArgs::parse_from(["libra", "-G", "foo.*bar"]);
        assert_eq!(g.pickaxe_regex.as_deref(), Some("foo.*bar"));
        assert!(matches!(
            build_pickaxe(&g).unwrap(),
            Some(PickaxeKind::DiffRegex(_))
        ));

        // An invalid -G regex is a usage error.
        let bad = LogArgs::parse_from(["libra", "-G", "("]);
        assert!(build_pickaxe(&bad).is_err());

        // -S and -G are mutually exclusive at the clap layer.
        assert!(LogArgs::try_parse_from(["libra", "-S", "a", "-G", "b"]).is_err());

        // No pickaxe flag => None.
        assert!(
            build_pickaxe(&LogArgs::parse_from(["libra"]))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn test_log_date_mode_parses() {
        assert_eq!(
            LogArgs::parse_from(["libra", "--date", "short"])
                .date
                .as_deref(),
            Some("short")
        );
    }

    #[test]
    fn test_name_status_parsing() {
        let args = LogArgs::parse_from(["libra", "--name-status"]);
        assert!(args.name_status);
        assert!(!args.name_only);

        let args = LogArgs::parse_from(["libra", "-z", "--name-status"]);
        assert!(args.null);
    }

    #[test]
    fn test_format_changes_output() {
        let changes = vec![FileChange {
            path: PathBuf::from("src/main.rs"),
            status: ChangeType::Added,
        }];
        let with_status = format_changes(&changes, true, false);
        assert_eq!(with_status, "A\tsrc/main.rs");

        let names_only = format_changes(&changes, false, false);
        assert_eq!(names_only, "src/main.rs");
        assert!(!names_only.contains("A\t"));

        let with_status_z = format_changes(&changes, true, true);
        assert_eq!(with_status_z.as_bytes(), b"A\0src/main.rs\0");
    }

    #[tokio::test]
    async fn test_commit_filter_author_and_time() {
        let mut commit = Commit::from_tree_id(ObjectHash::new(&[1; 20]), vec![], "msg");
        commit.author.name = "lvy".into();
        commit.author.email = "lvy@test.com".into();
        commit.committer.timestamp = 1_766_102_400; // 2025-12-19 00:00:00 UTC

        let filter = CommitFilter::new(
            Some("lvy".to_string()),
            None,
            Some(1_766_000_000),
            Some(1_766_200_000),
            Vec::new(),
            None,
            None,
            None,
            None,
        );

        assert!(filter.matches(&commit, None).await.unwrap());
    }

    #[tokio::test]
    async fn test_commit_filter_merges_and_committer() {
        // A merge commit (two parents) committed by alice.
        let mut merge = Commit::from_tree_id(
            ObjectHash::new(&[1; 20]),
            vec![ObjectHash::new(&[2; 20]), ObjectHash::new(&[3; 20])],
            "merge",
        );
        merge.committer.name = "alice".into();
        merge.committer.email = "alice@test.com".into();

        // --merges (min 2 parents) keeps it; --no-merges (max 1 parent) drops it.
        let merges = CommitFilter::new(
            None,
            None,
            None,
            None,
            Vec::new(),
            None,
            Some(2),
            None,
            None,
        );
        assert!(merges.matches(&merge, None).await.unwrap());
        let no_merges = CommitFilter::new(
            None,
            None,
            None,
            None,
            Vec::new(),
            None,
            None,
            Some(1),
            None,
        );
        assert!(!no_merges.matches(&merge, None).await.unwrap());

        // --committer matches on a case-insensitive substring of name/email.
        let by_committer = CommitFilter::new(
            None,
            Some("alice".to_string()),
            None,
            None,
            Vec::new(),
            None,
            None,
            None,
            None,
        );
        assert!(by_committer.matches(&merge, None).await.unwrap());
        let by_other = CommitFilter::new(
            None,
            Some("zzz".to_string()),
            None,
            None,
            Vec::new(),
            None,
            None,
            None,
            None,
        );
        assert!(!by_other.matches(&merge, None).await.unwrap());
    }

    // Test parameter mutual exclusion logic
    #[test]
    fn test_parameter_mutual_exclusion() {
        let args = LogArgs::parse_from(["libra", "--name-only", "--patch"]);

        let name_status = args.name_status;
        let name_only = args.name_only && !name_status;
        let patch = args.patch && !name_only && !name_status;

        assert!(name_only);
        assert!(!patch);

        let args = LogArgs::parse_from(["libra", "--name-status", "--patch"]);
        let name_status = args.name_status;
        let name_only = args.name_only && !name_status;
        let patch = args.patch && !name_only && !name_status;

        assert!(name_status);
        assert!(!patch);
    }

    // Test grep parameter parsing
    #[test]
    fn test_log_args_grep() {
        let args = LogArgs::parse_from(["libra", "--grep", "fix"]);
        assert_eq!(args.grep, Some("fix".to_string()));
        assert!(args.pathspec.is_empty());

        let args = LogArgs::parse_from(["libra"]);
        assert_eq!(args.grep, None);
        assert!(args.pathspec.is_empty());
    }

    // Test grep combined with other arguments
    #[test]
    fn test_grep_with_other_args() {
        let args = LogArgs::parse_from(["libra", "--grep", "feature", "--oneline", "-n", "5"]);
        assert_eq!(args.grep, Some("feature".to_string()));
        assert!(args.oneline);
        assert_eq!(args.number, Some(5));
        assert!(args.pathspec.is_empty());
    }

    // Test case-sensitive matching
    #[test]
    fn test_grep_case_sensitive() {
        let args = LogArgs::parse_from(["libra", "--grep", "FIX"]);
        assert_eq!(args.grep, Some("FIX".to_string()));
        assert!(args.pathspec.is_empty());
    }

    // Test empty string grep
    #[test]
    fn test_grep_empty_string() {
        let args = LogArgs::parse_from(["libra", "--grep", ""]);
        assert_eq!(args.grep, Some("".to_string()));
        assert!(args.pathspec.is_empty());
    }

    // Test graph with grep combination
    #[test]
    fn test_graph_with_grep() {
        let args = LogArgs::parse_from(["libra", "--graph", "--grep", "fix"]);
        assert!(args.graph);
        assert_eq!(args.grep, Some("fix".to_string()));
        assert!(args.pathspec.is_empty());
    }
}
