//! Per-line authorship attribution (`libra blame`).
//!
//! Implements the `blame` subcommand. Loads the file at the requested
//! revision, walks the commit graph backwards from that revision, and uses
//! `compute_diff` against each parent to migrate line ownership to the
//! oldest ancestor whose content still matches.
//!
//! Non-obvious responsibilities:
//! - Maps domain failures into stable [`CliError`] codes via the
//!   `From<BlameError>` impl so JSON consumers and shell scripts can match
//!   on machine-readable categories.
//! - Supports JSON, quiet, and paged-text output: human output is fed
//!   through [`Pager`] so very long blames behave well in a terminal.
//! - Tracks two parallel structures: the in-flight `LineBlame` vector
//!   (mutated as the BFS progresses) and the queued
//!   `(commit, parent_lines)` work items.

use chrono::DateTime;
use clap::Parser;
use git_internal::{
    diff::compute_diff,
    hash::ObjectHash,
    internal::object::{blob::Blob, commit::Commit, tree::Tree},
};
use regex::Regex;
use serde::Serialize;

use crate::{
    command::{get_target_commit, load_object},
    internal::shallow::ShallowSet,
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        object_ext::TreeExt,
        output::{OutputConfig, emit_json_data},
        pager::Pager,
        util,
    },
};

const BLAME_EXAMPLES: &str = "\
EXAMPLES:
    libra blame src/main.rs                Blame a file at HEAD
    libra blame src/main.rs abc1234        Blame a file at a specific commit
    libra blame -L 10,20 src/main.rs       Blame lines 10-20
    libra blame -L 10,+5 src/main.rs       Blame 5 lines starting at line 10
    libra blame -L '/fn main/,/^}/' src/main.rs  Blame from a regex match to a regex match
    libra blame -l src/main.rs             Show full commit hashes
    libra blame -s src/main.rs             Suppress the author and date columns
    libra blame -w src/main.rs             Ignore whitespace-only changes when attributing lines
    libra --json blame src/main.rs         Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = BLAME_EXAMPLES)]
pub struct BlameArgs {
    /// The file to blame
    #[clap(value_name = "FILE")]
    pub file: String,

    /// The commit to use for blame
    #[clap(value_name = "COMMIT", default_value = "HEAD")]
    pub commit: String,

    /// The line range to blame
    #[clap(short = 'L', value_name = "RANGE")]
    pub line_range: Option<String>,

    /// Emit the machine-readable porcelain format (commit metadata once per commit).
    #[clap(short = 'p', long)]
    pub porcelain: bool,

    /// Like --porcelain, but repeat the commit metadata header for every line.
    #[clap(long = "line-porcelain")]
    pub line_porcelain: bool,

    /// Show the author email instead of the author name in the default output.
    #[clap(short = 'e', long = "show-email")]
    pub show_email: bool,

    /// Show the long (full) commit hash instead of the abbreviated one.
    #[clap(short = 'l')]
    pub long: bool,

    /// Suppress the author name and timestamp columns from the default output.
    #[clap(short = 's')]
    pub suppress: bool,

    /// Show the raw author timestamp (epoch seconds) instead of a formatted date.
    #[clap(short = 't')]
    pub raw_timestamp: bool,

    /// Use N hex digits for the abbreviated commit hash (ignored when `-l` is set).
    #[clap(long, value_name = "N")]
    pub abbrev: Option<usize>,

    /// Do not treat root commits as boundaries. Accepted for Git parity and is a
    /// no-op: Libra's blame never prefixes boundary commits with `^`, so a root
    /// commit is already shown as a normal commit.
    #[clap(long)]
    pub root: bool,

    /// Show the filename in the original commit (after the hash column). Libra
    /// does not follow renames/copies, so every line shows the blamed file.
    #[clap(short = 'f', long = "show-name")]
    pub show_name: bool,

    /// Ignore whitespace when comparing the parent's and child's versions of a
    /// line, so whitespace-only changes are attributed to the older commit.
    /// Matches Git's `-w` (ignore-all-whitespace) semantics.
    #[clap(short = 'w', long = "ignore-whitespace")]
    pub ignore_whitespace: bool,
}

/// Strip every whitespace character from a line for `-w` comparison, mirroring
/// Git's ignore-all-whitespace rule (`XDF_IGNORE_WHITESPACE`). The original
/// line content is preserved for display; only the comparison key is normalized.
///
/// Git's whitespace test is C `isspace()` on bytes — ASCII space/tab/newline/
/// vertical-tab/form-feed/carriage-return only. We deliberately do NOT use
/// `char::is_whitespace`, which also matches Unicode whitespace (e.g. NBSP), so
/// a non-ASCII-whitespace edit is still treated as a real change as in Git.
fn normalize_for_whitespace(line: &str) -> String {
    line.chars()
        .filter(|c| !matches!(c, ' ' | '\t' | '\n' | '\x0b' | '\x0c' | '\r'))
        .collect()
}

/// Single attributed line of a blame report. Serialised verbatim to JSON.
#[derive(Debug, Clone, Serialize)]
pub struct BlameLine {
    pub line_number: usize,
    pub short_hash: String,
    pub hash: String,
    pub author: String,
    pub author_email: String,
    pub date: String,
    /// Raw author timestamp (epoch seconds); surfaced for `-t` and JSON callers.
    pub timestamp: i64,
    pub content: String,
}

/// Whole-file result of a `libra blame` invocation.
#[derive(Debug, Clone, Serialize)]
pub struct BlameOutput {
    pub file: String,
    pub revision: String,
    pub lines: Vec<BlameLine>,
}

/// Internal mutable state for one source line during the back-walk.
/// `commit_id` is updated whenever an older ancestor still contains the same
/// text — the final value is the line's introducing commit.
struct LineBlame {
    line_number: usize,
    commit_id: ObjectHash,
    author: String,
    author_email: String,
    timestamp: i64,
    content: String,
}

/// Domain error for `libra blame`. Mapped to stable [`CliError`] codes by
/// the `From` impl below.
#[derive(Debug, thiserror::Error)]
enum BlameError {
    /// CWD is not inside a `.libra` repository.
    #[error("not a libra repository")]
    NotInRepo,

    /// User-supplied revision could not be resolved by `get_target_commit`.
    #[error("invalid revision: '{0}'")]
    InvalidRevision(String),

    /// A repository object (commit/tree/blob) failed to load — typically
    /// indicates corruption or partial fetch.
    #[error("failed to load {kind} '{object_id}': {detail}")]
    ObjectLoad {
        kind: &'static str,
        object_id: String,
        detail: String,
    },

    /// `.libra/shallow` could not be read or parsed (fail-closed).
    #[error("{0}")]
    Shallow(String),

    /// The requested path is not present in the tree of the target revision.
    #[error("file '{path}' not found in revision '{revision}'")]
    FileNotFound { path: String, revision: String },

    /// `-L` argument did not match a supported form (`LINE`, `START,END`,
    /// `START,+COUNT`, or `/regex/` endpoints), a `/regex/` matched no line, or the
    /// resolved numbers were out of range. Mapped to a usage error.
    #[error("invalid line range: {0}")]
    InvalidLineRange(String),
}

impl From<BlameError> for CliError {
    fn from(error: BlameError) -> Self {
        let message = error.to_string();
        match error {
            BlameError::NotInRepo => CliError::repo_not_found(),
            BlameError::InvalidRevision(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("check the revision name and try again"),
            BlameError::ObjectLoad { .. } => CliError::fatal(message)
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the object store may be corrupted"),
            BlameError::Shallow(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint(
                    "each line of .libra/shallow must be a full object id; remove the file if the clone is complete",
                ),
            BlameError::FileNotFound { .. } => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("check the file path; use 'libra show <rev>:' to list available files"),
            BlameError::InvalidLineRange(_) => CliError::command_usage(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(
                    r#"supported formats: "10", "10,20", "10,+5", "/regex/", "/start/,/end/""#,
                ),
        }
    }
}

/// Fire-and-forget CLI dispatcher for `libra blame`.
///
/// Functional scope:
/// - Calls [`execute_safe`] with a default [`OutputConfig`] and prints any
///   error to stderr without propagating it.
pub async fn execute(args: BlameArgs) {
    if let Err(e) = execute_safe(args, &OutputConfig::default()).await {
        e.print_stderr();
    }
}

/// Structured entry point used by `cli::parse` and integration tests.
///
/// Functional scope:
/// - Runs [`run_blame`] to produce a [`BlameOutput`], then renders to JSON,
///   stays silent in `--quiet` mode, prints "File is empty" for an empty
///   blob, or formats human-friendly lines and pipes them through [`Pager`].
///
/// Boundary conditions:
/// - Errors from [`run_blame`] are mapped to [`CliError`] via the
///   `From<BlameError>` impl, preserving stable codes and hints.
///
/// See: tests::blame_error_mapping_reports_repo_corrupt_for_storage_failures
/// in src/command/blame.rs:367;
/// tests::test_blame_json_output_includes_lines in
/// tests/command/blame_test.rs:50.
pub async fn execute_safe(args: BlameArgs, out_config: &OutputConfig) -> CliResult<()> {
    let result = run_blame(&args).await.map_err(CliError::from)?;

    if out_config.is_json() {
        return emit_json_data("blame", &result, out_config);
    }

    if out_config.quiet {
        return Ok(());
    }

    if args.porcelain || args.line_porcelain {
        return render_blame_porcelain(&result, &args.file, args.line_porcelain);
    }

    if result.lines.is_empty() {
        println!("File is empty");
        return Ok(());
    }

    let mut output = String::new();
    for blame in &result.lines {
        // Hash column: `-l` shows the full hash; `--abbrev=<n>` shows n digits;
        // otherwise the default short hash.
        let hash_col = if args.long {
            blame.hash.clone()
        } else if let Some(n) = args.abbrev {
            blame.hash.chars().take(n).collect::<String>()
        } else {
            blame.short_hash.clone()
        };

        // `-f`/`--show-name` inserts the filename right after the hash column.
        // Libra does not follow renames, so it is the blamed file on every line.
        let name_col = if args.show_name {
            format!(" {}", result.file)
        } else {
            String::new()
        };

        // `-s` suppresses the author/timestamp columns entirely.
        if args.suppress {
            output.push_str(&format!(
                "{}{} {}) {}\n",
                hash_col, name_col, blame.line_number, blame.content
            ));
            continue;
        }

        // `-e`/`--show-email` shows `<email>` (Git's form) in the author slot.
        let display_author = if args.show_email {
            format!("<{}>", blame.author_email)
        } else {
            blame.author.clone()
        };
        let author_short = if display_author.chars().count() > 15 {
            let truncated: String = display_author.chars().take(12).collect();
            format!("{truncated}...")
        } else {
            format!("{display_author:15}")
        };
        // `-t` shows the raw epoch timestamp; otherwise the localized date.
        let date_col = if args.raw_timestamp {
            blame.timestamp.to_string()
        } else {
            blame
                .date
                .parse::<DateTime<chrono::FixedOffset>>()
                .map(|dt| {
                    dt.with_timezone(&chrono::Local)
                        .format("%Y-%m-%d %H:%M:%S %z")
                        .to_string()
                })
                .unwrap_or_else(|_| blame.date.clone())
        };

        output.push_str(&format!(
            "{}{} ({:19} {} {}) {}\n",
            hash_col, name_col, author_short, date_col, blame.line_number, blame.content
        ));
    }

    let mut pager = Pager::with_config(out_config)?;
    pager.write_str(&output)?;
    pager.finish()?;
    Ok(())
}

/// Compute the per-line attribution.
///
/// Functional scope:
/// - Resolves the start commit and reads the file's lines at that revision.
/// - Initialises one [`LineBlame`] per line, blaming everything to the start
///   commit, then BFS-walks parents. For each `Equal` chunk in the diff to a
///   parent, lines whose content still matches inherit the parent's commit
///   id, author, and timestamp.
/// - Applies the optional `-L` filter as a final pass.
///
/// Boundary conditions:
/// - Empty target file -> returns an empty [`BlameOutput`] without walking
///   history.
/// - Shallow-boundary commits are treated as roots via [`ShallowSet`]; failed
///   parent loads below a missing shallow file are still skipped so blame can
///   produce a partial answer.
/// - Bad `-L` ranges produce [`BlameError::InvalidLineRange`].
async fn run_blame(args: &BlameArgs) -> Result<BlameOutput, BlameError> {
    util::require_repo().map_err(|_| BlameError::NotInRepo)?;
    let shallow = ShallowSet::load().map_err(|error| BlameError::Shallow(error.to_string()))?;

    let commit_id = get_target_commit(&args.commit)
        .await
        .map_err(|_| BlameError::InvalidRevision(args.commit.clone()))?;

    let commit_obj = load_object::<Commit>(&commit_id).map_err(|e| BlameError::ObjectLoad {
        kind: "commit",
        object_id: commit_id.to_string(),
        detail: e.to_string(),
    })?;

    let target_lines = get_file_lines(&commit_obj, &args.file, &args.commit)?;

    if target_lines.is_empty() {
        return Ok(BlameOutput {
            file: args.file.clone(),
            revision: commit_id.to_string(),
            lines: Vec::new(),
        });
    }

    let mut blame_lines: Vec<LineBlame> = target_lines
        .iter()
        .enumerate()
        .map(|(idx, content)| LineBlame {
            line_number: idx + 1,
            commit_id,
            author: commit_obj.author.name.clone(),
            author_email: commit_obj.author.email.clone(),
            timestamp: commit_obj.author.timestamp as i64,
            content: content.clone(),
        })
        .collect();

    use std::collections::VecDeque;
    // One BFS frame: a commit, its version of the file, and the line-number
    // mapping from that version to the final target file.
    type WalkFrame = (ObjectHash, Commit, Vec<String>, Vec<Option<usize>>);
    // Each queue entry carries `cur_to_final`: for every line of that commit's
    // version of the file, the index of the line it became in the final target
    // file (or `None` if it does not survive to the target). Diff line numbers
    // are positions in the *current* commit, so they must be remapped through
    // this table to reach the right `blame_lines` slot — a direct `new_line - 1`
    // index is wrong once an intervening commit inserts or deletes lines above.
    let init_map: Vec<Option<usize>> = (0..target_lines.len()).map(Some).collect();
    let mut queue: VecDeque<WalkFrame> = VecDeque::new();
    queue.push_back((commit_id, commit_obj, target_lines, init_map));

    while let Some((current_id, current_commit, current_lines, cur_to_final)) = queue.pop_front() {
        if !blame_lines.iter().any(|b| b.commit_id == current_id) {
            continue;
        }

        for parent_id in shallow.parents_for_walk(&current_id, &current_commit.parent_commit_ids) {
            let parent_commit = match load_object::<Commit>(parent_id) {
                Ok(obj) => obj,
                Err(_) => continue,
            };

            let parent_revision = parent_id.to_string();
            let parent_lines = match get_file_lines(&parent_commit, &args.file, &parent_revision) {
                Ok(lines) if !lines.is_empty() => lines,
                _ => continue,
            };

            // Carry the final-line mapping back to the parent: a line that is
            // `Equal` between parent and current keeps the same final position.
            let mut parent_to_final: Vec<Option<usize>> = vec![None; parent_lines.len()];

            // With `-w`, diff on whitespace-normalized copies so a line that
            // differs only in whitespace is treated as unchanged and attributed
            // to the parent. The default path diffs the borrowed line vectors
            // directly (no copy); the original lines are always kept for display.
            let operations = if args.ignore_whitespace {
                let diff_parent: Vec<String> = parent_lines
                    .iter()
                    .map(|l| normalize_for_whitespace(l))
                    .collect();
                let diff_current: Vec<String> = current_lines
                    .iter()
                    .map(|l| normalize_for_whitespace(l))
                    .collect();
                compute_diff(&diff_parent, &diff_current)
            } else {
                compute_diff(&parent_lines, &current_lines)
            };
            for op in operations {
                use git_internal::diff::DiffOperation;
                match op {
                    DiffOperation::Insert { .. } | DiffOperation::Delete { .. } => {}
                    DiffOperation::Equal { old_line, new_line } => {
                        // Remap the current-commit line number to the final target
                        // line via this commit's mapping (identity for the target).
                        let Some(Some(final_idx)) = cur_to_final.get(new_line - 1).copied() else {
                            continue;
                        };
                        if let Some(slot) = parent_to_final.get_mut(old_line - 1) {
                            *slot = Some(final_idx);
                        }
                        if let Some(blame) = blame_lines.get_mut(final_idx)
                            && blame.commit_id == current_id
                        {
                            // Compare the parent line against the blamed line using
                            // the same normalization the diff used, so `-w` matches
                            // whitespace-only differences. The default path compares
                            // borrowed strings directly (no allocation per line).
                            let parent_line = parent_lines.get(old_line - 1);
                            let is_match = if args.ignore_whitespace {
                                let blame_key = normalize_for_whitespace(&blame.content);
                                parent_line.map(|l| normalize_for_whitespace(l)) == Some(blame_key)
                            } else {
                                parent_line == Some(&blame.content)
                            };
                            if is_match {
                                blame.commit_id = *parent_id;
                                blame.author = parent_commit.author.name.clone();
                                blame.author_email = parent_commit.author.email.clone();
                                blame.timestamp = parent_commit.author.timestamp as i64;
                            }
                        }
                    }
                }
            }
            queue.push_back((*parent_id, parent_commit, parent_lines, parent_to_final));
        }
    }

    let filtered_lines = if let Some(ref range) = args.line_range {
        let (start, end) =
            parse_line_range(range, &blame_lines).map_err(BlameError::InvalidLineRange)?;
        blame_lines
            .into_iter()
            .filter(|b| b.line_number >= start && b.line_number <= end)
            .collect::<Vec<_>>()
    } else {
        blame_lines
    };

    Ok(BlameOutput {
        file: args.file.clone(),
        revision: commit_id.to_string(),
        lines: filtered_lines
            .into_iter()
            .map(|line| {
                let hash = line.commit_id.to_string();
                BlameLine {
                    line_number: line.line_number,
                    short_hash: hash.chars().take(8).collect(),
                    hash,
                    author: line.author,
                    author_email: line.author_email,
                    date: format_blame_timestamp(line.timestamp),
                    timestamp: line.timestamp,
                    content: line.content,
                }
            })
            .collect(),
    })
}
/// Read `file_path` at `commit` and return its lines (without trailing
/// newlines).
///
/// Boundary conditions:
/// - Returns [`BlameError::FileNotFound`] if the path is absent in the tree.
/// - Non-UTF-8 blobs are decoded with `from_utf8_lossy`, replacing invalid
///   sequences with U+FFFD.
fn get_file_lines(
    commit: &Commit,
    file_path: &str,
    revision: &str,
) -> Result<Vec<String>, BlameError> {
    let tree = load_object::<Tree>(&commit.tree_id).map_err(|e| BlameError::ObjectLoad {
        kind: "tree",
        object_id: commit.tree_id.to_string(),
        detail: e.to_string(),
    })?;

    let plain_items = tree.get_plain_items();
    let target_path = util::to_workdir_path(file_path);

    let blob_hash = plain_items
        .iter()
        .find(|(path, _)| path == &target_path)
        .map(|(_, hash)| hash)
        .ok_or_else(|| BlameError::FileNotFound {
            path: file_path.to_string(),
            revision: revision.to_string(),
        })?;

    let blob = load_object::<Blob>(blob_hash).map_err(|e| BlameError::ObjectLoad {
        kind: "blob",
        object_id: blob_hash.to_string(),
        detail: e.to_string(),
    })?;

    let content = String::from_utf8_lossy(&blob.data);
    Ok(content.lines().map(|s| s.to_string()).collect())
}

/// Format an epoch second as RFC 3339 (UTC). Falls back to the raw integer
/// when the timestamp is outside chrono's representable range.
/// Render blame output in Git's porcelain format. With `line_porcelain`, the
/// commit metadata header is repeated for every line; otherwise it is printed
/// once per commit (subsequent lines from that commit print only the SHA header
/// and the content). The commit metadata is read by reloading each attributing
/// commit. NOTE: the original line number is approximated by the final line
/// number — the blame walk does not track per-commit origin line numbers.
fn render_blame_porcelain(result: &BlameOutput, file: &str, line_porcelain: bool) -> CliResult<()> {
    use std::{collections::HashSet, io::Write, str::FromStr};

    let lines = &result.lines;
    // For each line, record the group size when it starts a new consecutive run
    // of lines from the same commit (Git prints this count on the group's first
    // line only).
    let mut group_start_size: Vec<Option<usize>> = vec![None; lines.len()];
    let mut i = 0;
    while i < lines.len() {
        let mut j = i + 1;
        while j < lines.len() && lines[j].hash == lines[i].hash {
            j += 1;
        }
        group_start_size[i] = Some(j - i);
        i = j;
    }

    let mut emitted: HashSet<String> = HashSet::new();
    let mut buf = String::new();
    for (idx, line) in lines.iter().enumerate() {
        // Header: "<sha> <orig-line> <final-line> [<group-size>]".
        let (orig, final_no) = (line.line_number, line.line_number);
        match group_start_size[idx] {
            Some(n) => buf.push_str(&format!("{} {orig} {final_no} {n}\n", line.hash)),
            None => buf.push_str(&format!("{} {orig} {final_no}\n", line.hash)),
        }

        if line_porcelain || emitted.insert(line.hash.clone()) {
            let hash = ObjectHash::from_str(&line.hash).map_err(|_| {
                CliError::fatal(format!("invalid blame commit hash '{}'", line.hash))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            let commit = load_object::<Commit>(&hash).map_err(|e| {
                CliError::fatal(format!("failed to load commit {}: {e}", line.hash))
                    .with_stable_code(StableErrorCode::RepoCorrupt)
            })?;
            // Strip any gpgsig/header block so the summary is the real subject.
            let (parsed_message, _) = crate::common_utils::parse_commit_msg(&commit.message);
            let summary = parsed_message
                .lines()
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            buf.push_str(&format!("author {}\n", commit.author.name));
            buf.push_str(&format!("author-mail <{}>\n", commit.author.email));
            buf.push_str(&format!("author-time {}\n", commit.author.timestamp));
            buf.push_str(&format!("author-tz {}\n", commit.author.timezone));
            buf.push_str(&format!("committer {}\n", commit.committer.name));
            buf.push_str(&format!("committer-mail <{}>\n", commit.committer.email));
            buf.push_str(&format!("committer-time {}\n", commit.committer.timestamp));
            buf.push_str(&format!("committer-tz {}\n", commit.committer.timezone));
            buf.push_str(&format!("summary {summary}\n"));
            buf.push_str(&format!("filename {file}\n"));
        }
        buf.push_str(&format!("\t{}\n", line.content));
    }

    let stdout = std::io::stdout();
    match stdout.lock().write_all(buf.as_bytes()) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(e) => Err(
            CliError::io(format!("failed to write blame porcelain output: {e}"))
                .with_stable_code(StableErrorCode::IoWriteFailed),
        ),
    }
}

fn format_blame_timestamp(timestamp: i64) -> String {
    chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| timestamp.to_string())
}

/// Parse a `-L` argument into an inclusive `(start, end)` line range.
///
/// Functional scope:
/// - Accepts `LINE`, `START,END`, and `START,+COUNT` (offset) syntaxes.
///
/// Boundary conditions:
/// - Returns `Err` for non-numeric tokens, zero indices, indices past the
///   file end, or `start > end`. Each error message is suitable for direct
///   inclusion in a [`BlameError::InvalidLineRange`].
fn parse_line_range(range_str: &str, lines: &[LineBlame]) -> Result<(usize, usize), String> {
    let total_lines = lines.len();
    let (start_token, end_token) = split_line_range_tokens(range_str)?;

    // Resolve the start endpoint (a number or `/regex/`, searched from the file start).
    let start = resolve_start_endpoint(start_token, lines)?;

    // A single endpoint means "from <start> to the end of the file" (matching Git),
    // not just that one line.
    let end = match end_token {
        None => total_lines,
        Some(token) => resolve_end_endpoint(token, lines, start)?,
    };

    if start == 0 || start > total_lines || end == 0 || end > total_lines || start > end {
        return Err(format!(
            "Invalid range {},{} (total lines: {})",
            start, end, total_lines
        ));
    }
    Ok((start, end))
}

/// Split a `-L` argument into its `<start>` token and optional `<end>` token. The
/// start may be a `/regex/` (which can itself contain commas, and `\/` escapes), so a
/// regex start is scanned to its closing slash before looking for the `,` separator;
/// a numeric start is split at the first comma.
fn split_line_range_tokens(range: &str) -> Result<(&str, Option<&str>), String> {
    if range.starts_with('/') {
        let bytes = range.as_bytes();
        let mut i = 1;
        while i < bytes.len() {
            match bytes[i] {
                b'\\' => i += 2, // skip an escaped character (e.g. `\/`)
                b'/' => {
                    let start = &range[..=i];
                    let rest = &range[i + 1..];
                    return match rest.strip_prefix(',') {
                        Some(end) => Ok((start, Some(end))),
                        None if rest.is_empty() => Ok((start, None)),
                        None => Err(format!("expected ',' after /regex/ in -L: {range}")),
                    };
                }
                _ => i += 1,
            }
        }
        Err(format!("unterminated /regex/ in -L: {range}"))
    } else {
        match range.find(',') {
            Some(idx) => Ok((&range[..idx], Some(&range[idx + 1..]))),
            None => Ok((range, None)),
        }
    }
}

/// If `token` is `/regex/`, return the inner regex source.
fn regex_token_body(token: &str) -> Option<&str> {
    token
        .strip_prefix('/')
        .and_then(|rest| rest.strip_suffix('/'))
}

fn compile_blame_regex(source: &str) -> Result<Regex, String> {
    Regex::new(source).map_err(|error| format!("invalid regex /{source}/: {error}"))
}

/// Resolve a `<start>` token: a line number, or a `/regex/` resolved to the first
/// matching line (searched from the start of the file), matching Git.
fn resolve_start_endpoint(token: &str, lines: &[LineBlame]) -> Result<usize, String> {
    if let Some(source) = regex_token_body(token) {
        let regex = compile_blame_regex(source)?;
        lines
            .iter()
            .position(|line| regex.is_match(&line.content))
            .map(|index| index + 1)
            .ok_or_else(|| format!("/{source}/: no match in file"))
    } else {
        token
            .parse::<usize>()
            .map_err(|_| format!("Invalid start line: {token}"))
    }
}

/// Resolve an `<end>` token: a line number, `+COUNT` relative to `start`, or a
/// `/regex/` resolved to the first matching line at or after `start` (matching Git).
fn resolve_end_endpoint(token: &str, lines: &[LineBlame], start: usize) -> Result<usize, String> {
    if let Some(source) = regex_token_body(token) {
        let regex = compile_blame_regex(source)?;
        lines
            .iter()
            .enumerate()
            .skip(start.saturating_sub(1))
            .find(|(_, line)| regex.is_match(&line.content))
            .map(|(index, _)| index + 1)
            .ok_or_else(|| format!("/{source}/: no match at or after line {start}"))
    } else if let Some(offset_str) = token.strip_prefix('+') {
        let offset = offset_str
            .parse::<usize>()
            .map_err(|_| format!("Invalid offset: {token}"))?;
        if offset == 0 {
            return Err(format!("Invalid offset: {token}"));
        }
        Ok(start + offset - 1)
    } else {
        token
            .parse::<usize>()
            .map_err(|_| format!("Invalid end line: {token}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `-w` normalization strips only ASCII whitespace (Git's `isspace` set),
    /// leaving Unicode whitespace such as NBSP intact so it still counts as a
    /// real change — matching Git's byte-based ignore-all-whitespace.
    #[test]
    fn normalize_for_whitespace_strips_ascii_only() {
        assert_eq!(normalize_for_whitespace("  let  x =\t1 \r"), "letx=1");
        assert_eq!(
            normalize_for_whitespace("a\x0bb\x0cc"),
            "abc",
            "vertical-tab and form-feed are ASCII whitespace"
        );
        // U+00A0 NBSP is Unicode-only whitespace; Git does not ignore it.
        assert_eq!(normalize_for_whitespace("a\u{a0}b"), "a\u{a0}b");
    }

    /// Pin the `Display` format for the static-message and direct-
    /// message variants of [`BlameError`]. These strings are used as
    /// the `CliError` message via `From<BlameError> for CliError` and
    /// surface in both human and `--json` envelopes.
    #[test]
    fn blame_error_display_pins_each_variant() {
        assert_eq!(BlameError::NotInRepo.to_string(), "not a libra repository");
        assert_eq!(
            BlameError::InvalidRevision("HEAD~99".to_string()).to_string(),
            "invalid revision: 'HEAD~99'",
        );
        assert_eq!(
            BlameError::ObjectLoad {
                kind: "tree",
                object_id: "deadbeef".to_string(),
                detail: "object not found".to_string(),
            }
            .to_string(),
            "failed to load tree 'deadbeef': object not found",
        );
        assert_eq!(
            BlameError::FileNotFound {
                path: "src/missing.rs".to_string(),
                revision: "HEAD".to_string(),
            }
            .to_string(),
            "file 'src/missing.rs' not found in revision 'HEAD'",
        );
        assert_eq!(
            BlameError::InvalidLineRange("10,5".to_string()).to_string(),
            "invalid line range: 10,5",
        );
    }

    /// Scenario: object-store failures must surface as `RepoCorrupt` so that
    /// shell scripts and JSON consumers can distinguish "the object store is
    /// broken" from "the user typed the wrong revision".
    #[test]
    fn blame_error_mapping_reports_repo_corrupt_for_storage_failures() {
        let error = CliError::from(BlameError::ObjectLoad {
            kind: "tree",
            object_id: "abc123".to_string(),
            detail: "corrupt object".to_string(),
        });
        assert_eq!(error.stable_code(), StableErrorCode::RepoCorrupt);
    }

    /// Scenario: "file not in revision" is a user-target mistake, not
    /// corruption. Verifying the stable code keeps the error category
    /// distinct from object-load failures handled by the previous test.
    #[test]
    fn blame_error_mapping_reports_invalid_target_for_missing_file() {
        let error = CliError::from(BlameError::FileNotFound {
            path: "tracked.txt".to_string(),
            revision: "HEAD".to_string(),
        });
        assert_eq!(error.stable_code(), StableErrorCode::CliInvalidTarget);
    }
}
