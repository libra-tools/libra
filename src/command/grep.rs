//! Provides grep command logic for searching text patterns in working tree, index, or commit trees
//! with regex support, pathspec filtering, and various output formatting options.

use std::{
    fs,
    io::{BufRead, IsTerminal},
    path::{Path, PathBuf},
};

use clap::Parser;
use colored::Colorize;
use flate2::read::ZlibDecoder;
use git_internal::internal::index::Index;
use regex::RegexBuilder;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::Serialize;

use crate::{
    command::load_object,
    internal::{db, model::object_index},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data, record_warning},
        pager::Pager,
        path,
        pathspec::{PathspecDepthRoot, PathspecError, PathspecSet},
        util,
    },
};

#[derive(Debug)]
enum GrepReadError {
    Skippable(String),
    Fatal(String),
}

/// `--help` examples shown in `libra grep --help` output.
///
/// `grep` searches tracked files in the working tree, index, or a
/// revision tree. The banner pins the most common invocations
/// (literal vs regex pattern, multi-pattern, `--cached` index search,
/// `--tree REV` historical search, count, filename listing, line
/// numbers, JSON for agents) so users can map intent to invocation
/// without reading the design doc. Cross-cutting `--help` EXAMPLES
/// rollout per `docs/development/commands/_general.md` item B.
pub const GREP_EXAMPLES: &str = "\
EXAMPLES:
    libra grep 'TODO'                     Search the working tree for the regex 'TODO'
    libra grep -F 'fn foo()'              Treat the pattern as a literal string
    libra grep -i 'panic'                 Case-insensitive search
    libra grep -n 'TODO' src/             Show 1-based line numbers, restricted to src/
    libra grep -c 'unsafe' src/           Per-file match counts
    libra grep -l 'unwrap()' src/         Just the filenames that have matches
    libra grep -m 3 'TODO' src/           Stop after 3 matches per file
    libra grep --max-depth 1 'TODO' src/  Limit the search to 1 directory level below src/
    libra grep -o 'v[0-9]*' CHANGELOG     Print only the matched substrings
    libra grep -e 'TODO' -e 'FIXME'       Match either of multiple regexps
    libra grep --cached 'TODO'            Search files staged in the index instead of the worktree
    libra grep --tree HEAD~5 'TODO'       Search files inside a historical revision
    libra grep --json 'TODO'              Structured JSON output for agents";

/// Search for patterns in tracked files in the working tree, index, or commit trees.
#[derive(Parser, Debug)]
#[command(after_help = GREP_EXAMPLES)]
pub struct GrepArgs {
    /// The pattern to search for. Supports regular expressions by default.
    #[clap(value_name = "PATTERN", required_unless_present_any = ["regexp", "pattern_file"])]
    pattern: Option<String>,

    /// Add a pattern to search for. Can be specified multiple times.
    #[clap(short = 'e', long = "regexp", value_name = "PATTERN", action = clap::ArgAction::Append)]
    regexp: Vec<String>,

    /// Read patterns from a file, one per line. Can be specified multiple times.
    #[clap(short = 'f', long = "file", value_name = "FILE", action = clap::ArgAction::Append)]
    pattern_file: Vec<String>,

    /// Require all patterns to match at least once in a file.
    #[clap(long)]
    all_match: bool,

    /// Interpret pattern as a fixed string, not a regular expression.
    #[clap(short = 'F', long)]
    fixed_string: bool,

    /// Use POSIX extended regular expressions (the default dialect; accepted for Git compatibility).
    #[clap(short = 'E', long = "extended-regexp")]
    extended_regexp: bool,

    /// Use POSIX basic regular expressions (accepted; treated as the default dialect).
    #[clap(short = 'G', long = "basic-regexp")]
    basic_regexp: bool,

    /// Perl-compatible regular expressions (not supported in Libra).
    #[clap(short = 'P', long = "perl-regexp")]
    perl_regexp: bool,

    /// Process binary files as if they were text instead of skipping them.
    #[clap(short = 'a', long = "text")]
    text: bool,

    /// Never match in binary files (the default behavior; accepted for Git compatibility).
    #[clap(short = 'I')]
    no_binary: bool,

    /// Print NUM lines of trailing context after matching lines.
    #[clap(short = 'A', long = "after-context", value_name = "NUM")]
    after_context: Option<usize>,

    /// Print NUM lines of leading context before matching lines.
    #[clap(short = 'B', long = "before-context", value_name = "NUM")]
    before_context: Option<usize>,

    /// Print NUM lines of context before and after matching lines.
    #[clap(short = 'C', long = "context", value_name = "NUM")]
    context: Option<usize>,

    /// Ignore case distinctions in patterns and data.
    #[clap(short = 'i', long)]
    ignore_case: bool,

    /// Show only the number of matching lines for each file.
    #[clap(short = 'c', long)]
    count: bool,

    /// Show only the names of files with matches.
    #[clap(short = 'l', long, alias = "files-with-matches")]
    files_with_matches: bool,

    /// Show only the names of files without matches.
    #[clap(short = 'L', long, alias = "files-without-match")]
    files_without_matches: bool,

    /// Show line numbers for matching lines.
    #[clap(short = 'n', long)]
    line_number: bool,

    /// Select only those lines containing matches that form whole words.
    #[clap(short = 'w', long)]
    word_regexp: bool,

    /// Select non-matching lines.
    #[clap(short = 'v', long)]
    invert_match: bool,

    /// Show the 0-based byte offset of the first match on each line.
    #[clap(short = 'b', long)]
    byte_offset: bool,

    /// Only search in files matching the given pathspec.
    #[clap(value_name = "PATHS", num_args = 0..)]
    pathspec: Vec<String>,

    /// Search in the specified revision or commit instead of the working tree.
    #[clap(long, value_name = "REVISION")]
    tree: Option<String>,

    /// Search within tracked files in the index (staging area) instead of the working tree.
    #[clap(long)]
    cached: bool,

    /// In addition to tracked files, also search untracked, non-ignored files in the
    /// working tree. Cannot be combined with `--cached` or a `--tree` revision.
    #[clap(long, conflicts_with = "cached")]
    untracked: bool,

    /// Search the filesystem directly (the given paths, or the current directory),
    /// without a repository or its index. Works outside a repository, recurses every
    /// file including ignored ones (skipping the `.git`/`.libra` metadata
    /// directories), and shows paths relative to the current directory. Cannot be
    /// combined with `--cached`, `--untracked`, or `--tree`.
    #[clap(long = "no-index", conflicts_with_all = ["cached", "untracked", "tree"])]
    pub no_index: bool,

    /// Print the file name as a heading above its matches instead of as a per-line prefix.
    /// Paired with `--no-heading`; the last one given wins (Git semantics).
    #[clap(long, overrides_with = "no_heading")]
    heading: bool,

    /// Do not group matches under a file-name heading (the default).
    #[clap(long = "no-heading", overrides_with = "heading")]
    no_heading: bool,

    /// Print an empty line between matches from different files.
    /// Paired with `--no-break`; the last one given wins (Git semantics).
    #[clap(long = "break", overrides_with = "no_break")]
    break_: bool,

    /// Do not print an empty line between files (the default).
    #[clap(long = "no-break", overrides_with = "break_")]
    no_break: bool,

    /// Output a NUL byte after the file name (and line number) instead of ':', for machine consumption.
    #[clap(short = 'z', long = "null")]
    null: bool,

    /// Stop after NUM matching lines per file.
    #[clap(short = 'm', long = "max-count", value_name = "NUM")]
    max_count: Option<usize>,

    /// For each pathspec, descend at most DEPTH levels of directories below it
    /// (0 = only files directly in the pathspec, with no pathspec = top-level
    /// files). A negative value means no limit.
    #[clap(
        long = "max-depth",
        value_name = "DEPTH",
        allow_negative_numbers = true
    )]
    max_depth: Option<i64>,

    /// Print only the matched (non-empty) parts of a matching line, one match
    /// per output line (context lines are suppressed).
    #[clap(short = 'o', long = "only-matching")]
    only_matching: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// A single grep match result.
#[derive(Debug, Clone, Serialize)]
pub struct GrepMatch {
    /// The file path where the match was found.
    pub path: String,
    /// The line number (1-based) where the match was found.
    pub line_number: usize,
    /// The matching line content.
    pub line: String,
    /// The 0-based byte offset of the match start, if --byte-offset was requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub byte_offset: Option<usize>,
    /// True when this line is surrounding context (-A/-B/-C), not an actual match.
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_context: bool,
}

/// Internal representation of a file to search, with optional blob hash for tree/index searches.
struct SearchFile {
    /// Path used for display/output. Relative to the working-directory root for
    /// repository searches, or relative to the current directory for `--no-index`.
    path: PathBuf,
    /// Blob hash for tree/index searches (None for working tree / `--no-index`).
    blob_hash: Option<git_internal::hash::ObjectHash>,
    /// Absolute path to read content from, used by `--no-index` (which may run
    /// outside a repository, where the working-dir resolution would panic). When
    /// `None`, the on-disk read resolves `path` against the repository working dir.
    read_override: Option<PathBuf>,
}

/// Aggregated count result for a file (used with --count).
#[derive(Debug, Clone, Serialize)]
pub struct GrepCount {
    /// The file path.
    pub path: String,
    /// The number of matching lines in the file.
    pub count: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct GrepWarning {
    /// The file path that triggered the warning.
    pub path: String,
    /// Human-readable warning message.
    pub message: String,
}

/// Output structure for JSON mode.
#[derive(Debug, Clone, Serialize)]
pub struct GrepOutput {
    /// The pattern searched for.
    pub pattern: String,
    /// The full list of effective patterns searched.
    pub patterns: Vec<String>,
    /// The search context (working-tree, index, or tree ref).
    pub context: String,
    /// Total number of matching lines across all files.
    pub total_matches: usize,
    /// Total number of files with at least one matching line.
    pub total_files: usize,
    /// Individual match results (when not using --count, -l, or -L).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matches: Option<Vec<GrepMatch>>,
    /// Count results per file (when using --count). Each count is the number of matching lines in that file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub counts: Option<Vec<GrepCount>>,
    /// Files with matches (when using -l).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_with_matches: Option<Vec<String>>,
    /// Files without matches (when using -L).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub files_without_matches: Option<Vec<String>>,
    /// Warnings about skipped or unreadable files.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub warnings: Vec<GrepWarning>,
}

pub async fn execute(args: GrepArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. Searches for pattern matches in tracked files.
pub async fn execute_safe(args: GrepArgs, output: &OutputConfig) -> CliResult<()> {
    // `--no-index` greps the filesystem directly and does not require a repository.
    if !args.no_index {
        util::require_repo().map_err(|_| CliError::repo_not_found())?;
    }

    let result = run_grep(&args)
        .await
        .map_err(|error| error.with_exit_code(2))?;
    let has_selected_results = has_selected_results(&args, &result);
    render_grep_output(&args, &result, output)?;

    if has_selected_results {
        Ok(())
    } else {
        Err(CliError::silent_exit(1))
    }
}

fn has_selected_results(args: &GrepArgs, result: &GrepOutput) -> bool {
    if args.files_with_matches {
        result
            .files_with_matches
            .as_ref()
            .is_some_and(|files| !files.is_empty())
    } else if args.files_without_matches {
        result
            .files_without_matches
            .as_ref()
            .is_some_and(|files| !files.is_empty())
    } else if args.count {
        result
            .counts
            .as_ref()
            .is_some_and(|counts| !counts.is_empty())
    } else {
        result
            .matches
            .as_ref()
            .is_some_and(|matches| !matches.is_empty())
    }
}

/// Maximum file size to search (512KB, matching Git's core.bigFileThreshold default).
const MAX_FILE_SIZE: u64 = 512 * 1024;

/// Check if content appears to be binary (contains NUL bytes).
fn is_binary(content: &[u8]) -> bool {
    // Git checks the first 8000 bytes for NUL
    content.iter().take(8000).any(|&b| b == 0)
}

async fn run_grep(args: &GrepArgs) -> CliResult<GrepOutput> {
    if args.perl_regexp {
        return Err(
            CliError::command_usage("grep -P/--perl-regexp is not supported")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use the default regex dialect (-E) or -F for a fixed-string search"),
        );
    }

    let patterns = collect_patterns(args)?;
    let matcher = build_matcher(&patterns, args)?;
    let all_matchers = if args.all_match && patterns.len() > 1 {
        Some(build_individual_matchers(&patterns, args)?)
    } else {
        None
    };

    // Resolve the search context (working tree, index, specific tree, or no-index)
    let context_label = if args.no_index {
        "no-index".to_string()
    } else if let Some(tree_ref) = &args.tree {
        format!("tree:{}", tree_ref)
    } else if args.cached {
        "index".to_string()
    } else {
        "working-tree".to_string()
    };

    // Get the list of files to search
    let files = get_search_files(args).await?;

    // Process each file
    let mut matches: Vec<GrepMatch> = Vec::new();
    let mut counts: Vec<GrepCount> = Vec::new();
    let mut files_with_matches: Vec<String> = Vec::new();
    let mut files_without_matches: Vec<String> = Vec::new();
    let mut warnings: Vec<GrepWarning> = Vec::new();
    let mut total_matches = 0usize;
    let mut matched_file_count = 0usize;

    for search_file in &files {
        let path_str = search_file.path.display().to_string();
        let content = match read_file_content(search_file).await {
            Ok(c) => c,
            Err(GrepReadError::Skippable(message)) => {
                warnings.push(GrepWarning {
                    path: path_str,
                    message,
                });
                continue;
            }
            Err(GrepReadError::Fatal(message)) => {
                return Err(CliError::fatal(format!(
                    "failed to read search input '{}': {}",
                    search_file.path.display(),
                    message
                ))
                .with_stable_code(StableErrorCode::RepoCorrupt));
            }
        };

        // Binary files are skipped by default (Git's `-I` is the default here);
        // `-a`/`--text` forces them to be searched as text instead.
        if is_binary(&content) && !args.text {
            warnings.push(GrepWarning {
                path: search_file.path.display().to_string(),
                message: "skipped binary file".to_string(),
            });
            continue;
        }

        let mut file_matches = search_in_content(&content, &matcher, args);
        // `-m`/`--max-count`: keep only the first NUM real matches per file
        // (any trailing context up to the next match comes along).
        if let Some(max) = args.max_count {
            let mut seen = 0usize;
            let mut truncated = Vec::with_capacity(file_matches.len());
            for entry in file_matches {
                if !entry.3 {
                    if seen >= max {
                        break;
                    }
                    seen += 1;
                }
                truncated.push(entry);
            }
            file_matches = truncated;
        }
        let all_patterns_match = all_matchers.as_ref().is_none_or(|matchers| {
            matchers.iter().all(|pattern_matcher| {
                content
                    .split(|&byte| byte == b'\n')
                    .map(String::from_utf8_lossy)
                    .any(|line| pattern_matcher.is_match(&line))
            })
        });
        // Count only real matches, not the surrounding -A/-B/-C context lines.
        let actual_match_count = file_matches.iter().filter(|entry| !entry.3).count();
        let match_count = if all_patterns_match {
            actual_match_count
        } else {
            0
        };

        if match_count == 0 {
            if args.files_without_matches {
                files_without_matches.push(search_file.path.display().to_string());
            }
        } else {
            matched_file_count += 1;
            if args.files_with_matches {
                files_with_matches.push(search_file.path.display().to_string());
            } else if args.count {
                counts.push(GrepCount {
                    path: search_file.path.display().to_string(),
                    count: actual_match_count,
                });
            } else if args.only_matching {
                // `-o`: emit each matched substring on its own line; context
                // lines are dropped (they have no match to extract).
                for (line_num, line, _byte_off, is_ctx) in file_matches {
                    if is_ctx {
                        continue;
                    }
                    for m in matcher.find_iter(&line) {
                        matches.push(GrepMatch {
                            path: search_file.path.display().to_string(),
                            line_number: line_num,
                            line: m.as_str().to_string(),
                            // `byte_off` from `search_in_content` is the first
                            // match's within-line offset; under `-o` each match
                            // reports its own within-line offset (`m.start()`),
                            // consistent with Libra's existing within-line `-b`.
                            byte_offset: args.byte_offset.then_some(m.start()),
                            is_context: false,
                        });
                    }
                }
            } else {
                for (line_num, line, byte_off, is_ctx) in file_matches {
                    matches.push(GrepMatch {
                        path: search_file.path.display().to_string(),
                        line_number: line_num,
                        line,
                        byte_offset: (args.byte_offset && !is_ctx).then_some(byte_off),
                        is_context: is_ctx,
                    });
                }
            }
            total_matches += match_count;
        }
    }

    Ok(GrepOutput {
        pattern: patterns.first().cloned().unwrap_or_default(),
        patterns,
        context: context_label,
        total_matches,
        total_files: matched_file_count,
        matches: if !args.count && !args.files_with_matches && !args.files_without_matches {
            Some(matches)
        } else {
            None
        },
        counts: args.count.then_some(counts),
        files_with_matches: args.files_with_matches.then_some(files_with_matches),
        files_without_matches: args.files_without_matches.then_some(files_without_matches),
        warnings,
    })
}

fn collect_patterns(args: &GrepArgs) -> CliResult<Vec<String>> {
    let mut patterns = Vec::new();

    if let Some(pattern) = &args.pattern {
        patterns.push(pattern.clone());
    }

    patterns.extend(args.regexp.iter().cloned());

    for pattern_file in &args.pattern_file {
        let content = fs::read_to_string(pattern_file).map_err(|e| {
            CliError::fatal(format!(
                "failed to read pattern file '{}': {}",
                pattern_file, e
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        patterns.extend(
            content
                .lines()
                .filter(|line| !line.is_empty())
                .map(ToString::to_string),
        );
    }

    if patterns.is_empty() {
        return Err(
            CliError::command_usage("at least one search pattern is required")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }

    Ok(patterns)
}

/// Build a regex matcher based on command arguments.
fn build_matcher(patterns: &[String], args: &GrepArgs) -> CliResult<regex::Regex> {
    let compiled_patterns = normalize_patterns(patterns, args);

    let combined = if compiled_patterns.len() == 1 {
        compiled_patterns[0].clone()
    } else {
        format!("(?:{})", compiled_patterns.join(")|(?:"))
    };

    RegexBuilder::new(&combined)
        .case_insensitive(args.ignore_case)
        .build()
        .map_err(|e| {
            CliError::command_usage(format!(
                "invalid regex pattern '{}': {}",
                patterns.join(", "),
                e
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
        })
}

fn build_individual_matchers(patterns: &[String], args: &GrepArgs) -> CliResult<Vec<regex::Regex>> {
    normalize_patterns(patterns, args)
        .into_iter()
        .map(|pattern| {
            RegexBuilder::new(&pattern)
                .case_insensitive(args.ignore_case)
                .build()
                .map_err(|e| {
                    CliError::command_usage(format!("invalid regex pattern '{}': {}", pattern, e))
                        .with_stable_code(StableErrorCode::CliInvalidArguments)
                })
        })
        .collect()
}

fn normalize_patterns(patterns: &[String], args: &GrepArgs) -> Vec<String> {
    patterns
        .iter()
        .map(|pattern| {
            let mut normalized = if args.fixed_string {
                escape_regex(pattern)
            } else {
                pattern.clone()
            };

            if args.word_regexp {
                normalized = format!(r"\b(?:{})\b", normalized);
            }

            normalized
        })
        .collect()
}

/// Escape regex metacharacters in a string for literal matching.
fn escape_regex(s: &str) -> String {
    let mut escaped = String::with_capacity(s.len() * 2);
    for ch in s.chars() {
        if "[](){}.*+?^$|#\\".contains(ch) {
            escaped.push('\\');
        }
        escaped.push(ch);
    }
    escaped
}

/// Get the list of files to search, respecting pathspec and ignore rules.
async fn get_search_files(args: &GrepArgs) -> CliResult<Vec<SearchFile>> {
    if args.untracked && args.tree.is_some() {
        return Err(
            CliError::command_usage("--untracked cannot be used with a --tree revision")
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }

    let files = if args.no_index {
        // Search the filesystem directly (no repository / index).
        get_no_index_files(&args.pathspec)?
    } else if let Some(tree_ref) = &args.tree {
        // Search in a specific tree/commit
        get_tree_files(tree_ref, &args.pathspec).await?
    } else if args.cached {
        // Search in index (staged files)
        get_index_files(&args.pathspec)?
    } else if args.untracked {
        // Search tracked files plus untracked, non-ignored working-tree files.
        get_working_tree_files_with_untracked(&args.pathspec)?
    } else {
        // Search in working tree
        get_working_tree_files(&args.pathspec)?
    };

    apply_max_depth(files, args)
}

/// Drop files deeper than `--max-depth` levels below their matching pathspec
/// (or below the search root when no pathspec is given). A negative depth, or
/// no `--max-depth`, leaves the list unchanged. Depth is measured the same way
/// as Git: a file directly inside a pathspec directory is depth 0.
fn apply_max_depth(files: Vec<SearchFile>, args: &GrepArgs) -> CliResult<Vec<SearchFile>> {
    let Some(max_depth) = args.max_depth else {
        return Ok(files);
    };
    if max_depth < 0 {
        return Ok(files);
    }
    let max_depth = max_depth as usize;
    // Normalise the pathspecs into the SAME path form as the collected file
    // paths so the component math lines up: working-tree/index/tree paths are
    // workdir-relative (`to_workdir_path`), while `--no-index` display paths are
    // relative to the current directory.
    let specs: Vec<PathspecDepthRoot> = if args.no_index {
        let cwd = util::cur_dir();
        args.pathspec
            .iter()
            .map(|spec| {
                let path = PathBuf::from(spec);
                let absolute = if path.is_absolute() {
                    path.clone()
                } else {
                    cwd.join(&path)
                };
                PathspecDepthRoot::case_sensitive(
                    pathdiff::diff_paths(&absolute, &cwd).unwrap_or(path),
                )
            })
            .collect()
    } else {
        compile_repo_pathspecs(&args.pathspec)?.positive_depth_roots()
    };
    Ok(files
        .into_iter()
        .filter(|file| within_max_depth(&file.path, &specs, max_depth))
        .collect())
}

/// Whether `file` is within `max_depth` directory levels of at least one of
/// `specs` (or of the search root when `specs` is empty). The depth of a file
/// is `components(file) - components(spec) - 1`, clamped at 0, so a file
/// directly inside the pathspec (or a pathspec naming the file itself) is
/// depth 0 — matching Git's `--max-depth`.
fn within_max_depth(file: &Path, specs: &[PathspecDepthRoot], max_depth: usize) -> bool {
    let file_comps = path_depth_components(file);
    if specs.is_empty() {
        // No pathspec: depth is measured from the WORKTREE ROOT. Unlike Git
        // (which scopes a no-pathspec search to the current directory), `libra
        // grep` always searches the whole worktree with worktree-relative
        // paths regardless of cwd; the implicit root therefore stays the
        // worktree root. To limit to a subdirectory, pass it as a pathspec —
        // then depth is measured relative to that pathspec, matching Git.
        return file_comps.saturating_sub(1) <= max_depth;
    }
    specs.iter().any(|spec| {
        if depth_root_matches(file, spec) {
            let spec_comps = path_depth_components(spec.path());
            let depth = file_comps.saturating_sub(spec_comps + 1);
            depth <= max_depth
        } else {
            false
        }
    })
}

fn depth_root_matches(file: &Path, spec: &PathspecDepthRoot) -> bool {
    if !spec.icase() {
        return file == spec.path() || util::is_sub_path(file, spec.path());
    }

    let file = slash_path(file).to_lowercase();
    let spec = slash_path(spec.path()).to_lowercase();
    spec.is_empty()
        || file == spec
        || file
            .strip_prefix(spec.as_str())
            .is_some_and(|rest| rest.starts_with('/'))
}

fn slash_path(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().replace('\\', "/")),
            std::path::Component::CurDir => None,
            std::path::Component::ParentDir => Some("..".to_string()),
            std::path::Component::RootDir | std::path::Component::Prefix(_) => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Count the path components that contribute to directory depth, ignoring
/// `.` (`CurDir`) segments. A pathspec of `.` / `./` (the search root) thus
/// has depth 0, matching `util::is_sub_path`'s normalization and Git's
/// treatment of a root pathspec.
fn path_depth_components(path: &Path) -> usize {
    use std::path::Component;
    path.components()
        .filter(|component| !matches!(component, Component::CurDir))
        .count()
}

/// Collect files for `--no-index`: walk the given paths (or the current directory)
/// recursively WITHOUT a repository or index, like a plain recursive grep. Every
/// regular file is included (ignore rules are NOT applied, matching `git grep
/// --no-index`); the `.git`/`.libra` metadata directories and symlinks are skipped.
/// Display paths are relative to the current directory; content is read from the
/// absolute on-disk path (`read_override`).
fn get_no_index_files(pathspec: &[String]) -> CliResult<Vec<SearchFile>> {
    use walkdir::WalkDir;

    let cwd = util::cur_dir();
    let roots: Vec<PathBuf> = if pathspec.is_empty() {
        vec![cwd.clone()]
    } else {
        pathspec
            .iter()
            .map(|spec| {
                let path = PathBuf::from(spec);
                if path.is_absolute() {
                    path
                } else {
                    cwd.join(path)
                }
            })
            .collect()
    };

    let mut files = Vec::new();
    for root in roots {
        let walker = WalkDir::new(&root).into_iter().filter_entry(|entry| {
            // Prune the repository metadata directories.
            !(entry.file_type().is_dir()
                && matches!(entry.file_name().to_str(), Some(".git" | ".libra")))
        });
        for entry in walker {
            let entry = entry.map_err(|error| {
                CliError::command_usage(format!("failed to read '{}': {error}", root.display()))
                    .with_stable_code(StableErrorCode::CliInvalidTarget)
            })?;
            // Skip directories and symlinks; only regular files are searched.
            if !entry.file_type().is_file() {
                continue;
            }
            let absolute = entry.path().to_path_buf();
            let display = pathdiff::diff_paths(&absolute, &cwd).unwrap_or_else(|| absolute.clone());
            files.push(SearchFile {
                path: display,
                blob_hash: None,
                read_override: Some(absolute),
            });
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

/// Tracked working-tree files plus untracked, non-ignored files (matching
/// `git grep --untracked`). Both kinds read from disk (`blob_hash: None`); the
/// combined list is sorted by path for deterministic, Git-like output.
fn get_working_tree_files_with_untracked(pathspec: &[String]) -> CliResult<Vec<SearchFile>> {
    let index = load_index()?;
    let pathspecs = compile_repo_pathspecs(pathspec)?;

    let mut files = tracked_files_from_index(&index, &pathspecs, false);
    let tracked: std::collections::HashSet<PathBuf> =
        files.iter().map(|file| file.path.clone()).collect();

    // `list_workdir_files` returns non-ignored working-tree files (tracked and
    // untracked); the ones not already tracked are the untracked, non-ignored files.
    let worktree = util::list_workdir_files().map_err(|error| {
        CliError::fatal(format!("failed to list working tree: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    for path in worktree {
        if tracked.contains(&path) {
            continue;
        }
        if pathspecs.matches_path(&path) {
            files.push(SearchFile {
                path,
                blob_hash: None,
                read_override: None,
            });
        }
    }

    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(files)
}

async fn get_tree_files(tree_ref: &str, pathspec: &[String]) -> CliResult<Vec<SearchFile>> {
    use git_internal::internal::object::tree::Tree;

    use crate::utils::object_ext::TreeExt;

    let commit_hash = util::get_commit_base(tree_ref).await.map_err(|_| {
        CliError::command_usage(format!("invalid revision: {}", tree_ref))
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    })?;

    // Load the commit and get its tree.
    let commit: git_internal::internal::object::commit::Commit = load_object(&commit_hash)
        .map_err(|e| {
            CliError::fatal(format!("failed to load commit '{}': {}", commit_hash, e))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        })?;

    let tree: Tree = load_object(&commit.tree_id).map_err(|e| {
        CliError::fatal(format!("failed to load tree '{}': {}", commit.tree_id, e))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;

    // Get all files from the tree with their blob hashes.
    let all_files: Vec<(PathBuf, git_internal::hash::ObjectHash)> = tree.get_plain_items();

    let pathspecs = compile_repo_pathspecs(pathspec)?;

    let files: Vec<SearchFile> = if pathspecs.is_empty() {
        all_files
            .into_iter()
            .map(|(path, blob_hash)| SearchFile {
                path,
                blob_hash: Some(blob_hash),
                read_override: None,
            })
            .collect()
    } else {
        all_files
            .into_iter()
            .filter(|(p, _)| pathspecs.matches_path(p))
            .map(|(path, blob_hash)| SearchFile {
                path,
                blob_hash: Some(blob_hash),
                read_override: None,
            })
            .collect()
    };

    Ok(files)
}

/// Get files from the index (staged files).
fn get_index_files(pathspec: &[String]) -> CliResult<Vec<SearchFile>> {
    let index = load_index()?;
    let pathspecs = compile_repo_pathspecs(pathspec)?;

    Ok(tracked_files_from_index(&index, &pathspecs, true))
}

/// Get tracked files from the working tree while reading their current on-disk contents.
fn get_working_tree_files(pathspec: &[String]) -> CliResult<Vec<SearchFile>> {
    let index = load_index()?;
    let pathspecs = compile_repo_pathspecs(pathspec)?;

    Ok(tracked_files_from_index(&index, &pathspecs, false))
}

fn load_index() -> CliResult<Index> {
    Index::load(path::index()).map_err(|e| {
        CliError::fatal(format!("failed to load index: {}", e))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })
}

fn tracked_files_from_index(
    index: &Index,
    pathspecs: &PathspecSet,
    include_blob_hash: bool,
) -> Vec<SearchFile> {
    index
        .tracked_entries(0)
        .into_iter()
        .filter(|entry| pathspecs.matches_path(&entry.name))
        .map(|entry| SearchFile {
            path: PathBuf::from(&entry.name),
            blob_hash: include_blob_hash.then_some(entry.hash),
            read_override: None,
        })
        .collect()
}

fn compile_repo_pathspecs(pathspec: &[String]) -> CliResult<PathspecSet> {
    PathspecSet::from_workdir(pathspec, &util::cur_dir(), &util::working_dir())
        .map_err(pathspec_error_to_cli)
}

fn pathspec_error_to_cli(error: PathspecError) -> CliError {
    match error {
        PathspecError::OutsideRepository { .. } => CliError::fatal(error.to_string())
            .with_stable_code(StableErrorCode::CliInvalidTarget)
            .with_hint("all pathspecs must stay within the repository working tree"),
        PathspecError::UnsupportedMagic { .. } | PathspecError::InvalidPattern { .. } => {
            CliError::fatal(error.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use supported magic: top, exclude, icase, literal, glob")
        }
    }
}

/// Read file content from working tree or from a blob object.
async fn read_file_content(search_file: &SearchFile) -> Result<Vec<u8>, GrepReadError> {
    let content = if let Some(blob_hash) = &search_file.blob_hash {
        ensure_blob_within_size_limit(blob_hash).await?;

        // Read from blob object (tree/index search)
        let blob: git_internal::internal::object::blob::Blob = load_object(blob_hash)
            .map_err(|e| GrepReadError::Fatal(format!("failed to load blob: {}", e)))?;
        blob.data
    } else {
        // Read from working tree (or the `--no-index` absolute override).
        let abs_path = match &search_file.read_override {
            Some(absolute) => absolute.clone(),
            None => util::workdir_to_absolute(&search_file.path),
        };

        let metadata = std::fs::symlink_metadata(&abs_path)
            .map_err(|e| GrepReadError::Skippable(format!("failed to stat file: {}", e)))?;

        let file_type = metadata.file_type();
        if file_type.is_symlink() {
            return Err(GrepReadError::Skippable(
                "skipped symbolic link".to_string(),
            ));
        }
        if !file_type.is_file() {
            return Err(GrepReadError::Skippable(
                "skipped non-regular file".to_string(),
            ));
        }

        // Check file size before reading
        if metadata.len() > MAX_FILE_SIZE {
            return Err(GrepReadError::Skippable(format!(
                "file too large ({} bytes, max {} bytes)",
                metadata.len(),
                MAX_FILE_SIZE
            )));
        }

        std::fs::read(&abs_path)
            .map_err(|e| GrepReadError::Skippable(format!("failed to read file: {}", e)))?
    };

    if content.len() as u64 > MAX_FILE_SIZE {
        return Err(GrepReadError::Skippable(format!(
            "file too large ({} bytes, max {} bytes)",
            content.len(),
            MAX_FILE_SIZE
        )));
    }

    Ok(content)
}

async fn ensure_blob_within_size_limit(
    blob_hash: &git_internal::hash::ObjectHash,
) -> Result<(), GrepReadError> {
    if let Some(size) = lookup_indexed_blob_size(blob_hash).await? {
        if size > MAX_FILE_SIZE {
            return Err(GrepReadError::Skippable(format!(
                "file too large ({} bytes, max {} bytes)",
                size, MAX_FILE_SIZE
            )));
        }
        return Ok(());
    }

    if let Some(size) = read_loose_blob_size(blob_hash)?
        && size > MAX_FILE_SIZE
    {
        return Err(GrepReadError::Skippable(format!(
            "file too large ({} bytes, max {} bytes)",
            size, MAX_FILE_SIZE
        )));
    }

    Ok(())
}

async fn lookup_indexed_blob_size(
    blob_hash: &git_internal::hash::ObjectHash,
) -> Result<Option<u64>, GrepReadError> {
    let db = db::get_db_conn_instance().await;
    object_index::Entity::find()
        .filter(object_index::Column::OId.eq(blob_hash.to_string()))
        .filter(object_index::Column::OType.eq("blob"))
        .one(&db)
        .await
        .map_err(|e| GrepReadError::Fatal(format!("failed to query object index: {}", e)))
        .map(|record| record.map(|row| row.o_size as u64))
}

fn read_loose_blob_size(
    blob_hash: &git_internal::hash::ObjectHash,
) -> Result<Option<u64>, GrepReadError> {
    let object_path = path::objects()
        .join(&blob_hash.to_string()[0..2])
        .join(&blob_hash.to_string()[2..]);
    if !object_path.exists() {
        return Ok(None);
    }

    let raw = std::fs::read(&object_path)
        .map_err(|e| GrepReadError::Fatal(format!("failed to read object: {}", e)))?;
    let decoder = ZlibDecoder::new(raw.as_slice());
    let mut reader = std::io::BufReader::new(decoder);
    let mut header = Vec::new();
    reader
        .read_until(0, &mut header)
        .map_err(|e| GrepReadError::Fatal(format!("failed to read object header: {}", e)))?;
    let terminator = header
        .iter()
        .position(|&byte| byte == 0)
        .ok_or_else(|| GrepReadError::Fatal("invalid object header".to_string()))?;
    let header_str = std::str::from_utf8(&header[..terminator])
        .map_err(|e| GrepReadError::Fatal(format!("invalid object header: {}", e)))?;
    let mut parts = header_str.splitn(2, ' ');
    let object_type = parts.next().unwrap_or_default();
    let size = parts
        .next()
        .ok_or_else(|| GrepReadError::Fatal("invalid object header".to_string()))?
        .parse::<u64>()
        .map_err(|e| GrepReadError::Fatal(format!("invalid object size: {}", e)))?;

    if object_type != "blob" {
        return Ok(None);
    }

    Ok(Some(size))
}

/// Search for pattern matches in file content.
/// Returns a list of (line_number, line_content, byte_offset) tuples.
/// Resolve the effective (before, after) context-line counts from `-B`/`-A`/`-C`.
/// `-C` provides the default for either side; the side-specific flags win.
fn context_window(args: &GrepArgs) -> (usize, usize) {
    let before = args.before_context.or(args.context).unwrap_or(0);
    let after = args.after_context.or(args.context).unwrap_or(0);
    (before, after)
}

/// Search `content` line by line, returning `(line_number, line, byte_offset,
/// is_context)` tuples. When `-A`/`-B`/`-C` context is requested, surrounding
/// lines are included with `is_context = true`; `byte_offset` is only meaningful
/// for actual match lines.
fn search_in_content(
    content: &[u8],
    matcher: &regex::Regex,
    args: &GrepArgs,
) -> Vec<(usize, String, usize, bool)> {
    let content_str = String::from_utf8_lossy(content);
    let lines: Vec<&str> = content_str.lines().collect();
    let (before, after) = context_window(args);

    // First pass: collect the 0-based indices of matching lines.
    let match_indices: Vec<usize> = lines
        .iter()
        .enumerate()
        .filter(|(_, line)| {
            let m = matcher.is_match(line);
            if args.invert_match { !m } else { m }
        })
        .map(|(idx, _)| idx)
        .collect();

    let byte_off_of = |idx: usize| -> usize {
        if args.byte_offset {
            matcher.find(lines[idx]).map(|m| m.start()).unwrap_or(0)
        } else {
            0
        }
    };

    // Fast path: no context requested.
    if before == 0 && after == 0 {
        return match_indices
            .into_iter()
            .map(|idx| (idx + 1, lines[idx].to_string(), byte_off_of(idx), false))
            .collect();
    }

    // Expand each match to its context window, deduping overlapping windows via
    // an ordered set, then mark which emitted lines are real matches.
    let match_set: std::collections::HashSet<usize> = match_indices.iter().copied().collect();
    let mut emit: std::collections::BTreeSet<usize> = std::collections::BTreeSet::new();
    let last = lines.len().saturating_sub(1);
    for &idx in &match_indices {
        let lo = idx.saturating_sub(before);
        let hi = (idx + after).min(last);
        for i in lo..=hi {
            emit.insert(i);
        }
    }

    emit.into_iter()
        .map(|idx| {
            let is_match = match_set.contains(&idx);
            let byte_off = if is_match { byte_off_of(idx) } else { 0 };
            (idx + 1, lines[idx].to_string(), byte_off, !is_match)
        })
        .collect()
}

/// Render grep output to stdout or JSON.
fn render_grep_output(
    args: &GrepArgs,
    result: &GrepOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    for _warning in &result.warnings {
        record_warning();
    }

    if output.is_json() {
        return emit_json_data("grep", result, output);
    }

    for warning in &result.warnings {
        eprintln!("warning: {}: {}", warning.path, warning.message);
    }

    if output.quiet {
        return Ok(());
    }

    let mut pager = Pager::with_config(output)?;
    let should_color = std::io::stdout().is_terminal() && !output.is_json();
    let matcher = should_color
        .then(|| build_matcher(&result.patterns, args))
        .transpose()?;

    if args.files_with_matches {
        for file in result.files_with_matches.as_ref().unwrap_or(&Vec::new()) {
            if args.null {
                pager.write_str(&format!("{file}\0"))?;
            } else {
                pager.write_line(file)?;
            }
        }
    } else if args.files_without_matches {
        for file in result.files_without_matches.as_ref().unwrap_or(&Vec::new()) {
            if args.null {
                pager.write_str(&format!("{file}\0"))?;
            } else {
                pager.write_line(file)?;
            }
        }
    } else if args.count {
        let sep = if args.null { '\0' } else { ':' };
        for count in result.counts.as_ref().unwrap_or(&Vec::new()) {
            pager.write_line(&format!("{}{sep}{}", count.path, count.count))?;
        }
    } else {
        // Regular match output with optional highlighting and -A/-B/-C context.
        // Match lines use ':' separators; context lines use '-'. A "--" line
        // separates non-adjacent context groups (Git's behavior).
        let (before, after) = context_window(args);
        let context_active = before > 0 || after > 0;
        // `overrides_with` makes each `--x`/`--no-x` pair last-one-wins, so the
        // positive field already reflects the effective state.
        let heading = args.heading;
        let do_break = args.break_;
        let mut prev: Option<(String, usize)> = None;
        for match_item in result.matches.as_ref().unwrap_or(&Vec::new()) {
            let new_file = prev
                .as_ref()
                .map(|(prev_path, _)| *prev_path != match_item.path)
                .unwrap_or(true);

            if new_file {
                // File-group boundary. `--break` inserts a blank line between
                // files; otherwise preserve Git's "--" separator between context
                // groups across files when context is active. `--heading` prints
                // the file name as a standalone heading line.
                if prev.is_some() {
                    if do_break {
                        pager.write_line("")?;
                    } else if !heading && context_active {
                        pager.write_line("--")?;
                    }
                }
                if heading {
                    pager.write_line(&match_item.path)?;
                }
            } else if context_active
                && let Some((_, prev_line)) = &prev
                && match_item.line_number > *prev_line + 1
            {
                // Non-adjacent context group within the same file.
                pager.write_line("--")?;
            }

            // `-z`/`--null` replaces every field separator with NUL; otherwise
            // match lines use ':' and context lines use '-'.
            let sep: char = if args.null {
                '\0'
            } else if match_item.is_context {
                '-'
            } else {
                ':'
            };
            let rendered = if match_item.is_context {
                match_item.line.clone()
            } else if let Some(matcher) = matcher.as_ref().filter(|_| !args.invert_match) {
                colorize_match(&match_item.line, matcher)
            } else {
                match_item.line.clone()
            };

            // `--heading` drops the per-line file-name prefix.
            let prefix = if heading {
                String::new()
            } else {
                format!("{}{sep}", match_item.path)
            };
            let formatted = if args.byte_offset && !match_item.is_context {
                format!(
                    "{prefix}{}{sep}{}{sep}{rendered}",
                    match_item.line_number,
                    match_item.byte_offset.unwrap_or(0),
                )
            } else if args.line_number || args.byte_offset {
                format!("{prefix}{}{sep}{rendered}", match_item.line_number)
            } else {
                format!("{prefix}{rendered}")
            };
            pager.write_line(&formatted)?;

            prev = Some((match_item.path.clone(), match_item.line_number));
        }
    }

    pager.finish()?;
    Ok(())
}

/// Colorize matching portions of a line using the actual matcher spans.
fn colorize_match(line: &str, matcher: &regex::Regex) -> String {
    let mut result = String::new();
    let mut last_end = 0;

    for matched in matcher.find_iter(line) {
        result.push_str(&line[last_end..matched.start()]);
        result.push_str(&matched.as_str().red().bold().to_string());
        last_end = matched.end();
    }

    result.push_str(&line[last_end..]);
    result
}

#[cfg(test)]
struct ColorOverrideReset;

#[cfg(test)]
impl Drop for ColorOverrideReset {
    fn drop(&mut self) {
        colored::control::unset_override();
    }
}

#[cfg(test)]
mod tests {
    use serial_test::serial;

    use super::*;

    #[test]
    fn test_escape_regex() {
        assert_eq!(escape_regex("hello"), "hello");
        assert_eq!(escape_regex("foo.bar"), "foo\\.bar");
        assert_eq!(escape_regex("a*b+c?"), "a\\*b\\+c\\?");
        assert_eq!(escape_regex("[test]"), "\\[test\\]");
        assert_eq!(escape_regex("(group)"), "\\(group\\)");
        assert_eq!(escape_regex("$100"), "\\$100");
        assert_eq!(escape_regex("^start"), "\\^start");
        assert_eq!(escape_regex("a|b"), "a\\|b");
        assert_eq!(escape_regex("#comment"), "\\#comment");
        assert_eq!(escape_regex("back\\slash"), "back\\\\slash");
    }

    #[test]
    fn test_grep_args_parsing() {
        let args = GrepArgs::parse_from(["grep", "pattern"]);
        assert_eq!(args.pattern.as_deref(), Some("pattern"));
        assert!(!args.fixed_string);
        assert!(!args.ignore_case);
        assert!(!args.line_number);

        let args = GrepArgs::parse_from(["grep", "-i", "-n", "pattern"]);
        assert!(args.ignore_case);
        assert!(args.line_number);

        let args = GrepArgs::parse_from(["grep", "-F", "-l", "pattern"]);
        assert!(args.fixed_string);
        assert!(args.files_with_matches);

        let args = GrepArgs::parse_from(["grep", "-L", "pattern"]);
        assert!(args.files_without_matches);

        let args = GrepArgs::parse_from(["grep", "-e", "foo", "-e", "bar"]);
        assert_eq!(args.regexp, vec!["foo", "bar"]);

        let args = GrepArgs::parse_from(["grep", "--all-match", "-e", "foo", "-e", "bar"]);
        assert!(args.all_match);

        let args = GrepArgs::parse_from(["grep", "-c", "-w", "pattern"]);
        assert!(args.count);
        assert!(args.word_regexp);

        let args = GrepArgs::parse_from(["grep", "pattern", "src/", "lib/"]);
        assert_eq!(args.pathspec, vec!["src/", "lib/"]);
    }

    #[test]
    fn test_grep_args_dialect_and_binary_flags() {
        assert!(GrepArgs::parse_from(["grep", "-E", "pat"]).extended_regexp);
        assert!(GrepArgs::parse_from(["grep", "-G", "pat"]).basic_regexp);
        assert!(GrepArgs::parse_from(["grep", "-P", "pat"]).perl_regexp);
        assert!(GrepArgs::parse_from(["grep", "-a", "pat"]).text);
        assert!(GrepArgs::parse_from(["grep", "-I", "pat"]).no_binary);
    }

    #[test]
    fn test_grep_args_output_grouping_flags() {
        // -z / --null short and long forms.
        assert!(GrepArgs::parse_from(["grep", "-z", "pat"]).null);
        assert!(GrepArgs::parse_from(["grep", "--null", "pat"]).null);

        // Defaults are off.
        let default = GrepArgs::parse_from(["grep", "pat"]);
        assert!(!default.heading && !default.no_heading);
        assert!(!default.break_ && !default.no_break);

        // --heading / --break set the positive field.
        assert!(GrepArgs::parse_from(["grep", "--heading", "pat"]).heading);
        assert!(GrepArgs::parse_from(["grep", "--break", "pat"]).break_);

        // Negated pairs are last-one-wins (Git semantics) via clap overrides_with:
        // the positive field reflects the effective state regardless of which
        // form appears, with the final occurrence taking precedence.
        assert!(!GrepArgs::parse_from(["grep", "--heading", "--no-heading", "pat"]).heading);
        assert!(GrepArgs::parse_from(["grep", "--no-heading", "--heading", "pat"]).heading);
        assert!(!GrepArgs::parse_from(["grep", "--break", "--no-break", "pat"]).break_);
        assert!(GrepArgs::parse_from(["grep", "--no-break", "--break", "pat"]).break_);
    }

    #[test]
    fn test_collect_patterns_merges_positional_and_regexp() {
        let args = GrepArgs::parse_from(["grep", "pattern", "-e", "extra"]);
        let patterns = collect_patterns(&args).unwrap();
        assert_eq!(patterns, vec!["pattern", "extra"]);
    }

    #[test]
    fn test_build_matcher_basic() {
        let args = GrepArgs::parse_from(["grep", "test"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();
        assert!(matcher.is_match("this is a test"));
        assert!(!matcher.is_match("no match here"));
    }

    #[test]
    fn test_build_matcher_fixed_string() {
        let args = GrepArgs::parse_from(["grep", "-F", "foo.bar"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();
        assert!(matcher.is_match("this is foo.bar"));
        // With fixed string, the dot should not match any character
        assert!(!matcher.is_match("this is fooXbar"));
    }

    #[test]
    fn test_build_matcher_case_insensitive() {
        let args = GrepArgs::parse_from(["grep", "-i", "HELLO"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();
        assert!(matcher.is_match("hello world"));
        assert!(matcher.is_match("HELLO WORLD"));
        assert!(matcher.is_match("HeLLo WoRLd"));
    }

    #[test]
    fn test_build_matcher_word_regexp() {
        let args = GrepArgs::parse_from(["grep", "-w", "test"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();
        assert!(matcher.is_match("this is a test"));
        assert!(matcher.is_match("test case"));
        assert!(!matcher.is_match("testing"));
        assert!(!matcher.is_match("atestb"));
    }

    #[test]
    fn test_search_in_content_simple() {
        let content = b"line one\nline two\nline three\n";
        let args = GrepArgs::parse_from(["grep", "two"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();

        let results = search_in_content(content, &matcher, &args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 2); // line number
        assert_eq!(results[0].1, "line two");
    }

    #[test]
    fn test_search_in_content_invert() {
        let content = b"line one\nline two\nline three\n";
        let args = GrepArgs::parse_from(["grep", "-v", "two"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();

        let results = search_in_content(content, &matcher, &args);
        assert_eq!(results.len(), 2); // lines 1 and 3
        assert_eq!(results[0].0, 1);
        assert_eq!(results[1].0, 3);
    }

    #[test]
    fn test_search_in_content_multiple_matches() {
        let content = b"hello world\nhello again\nno match\nhello there\n";
        let args = GrepArgs::parse_from(["grep", "hello"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();

        let results = search_in_content(content, &matcher, &args);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_search_in_content_with_byte_offset() {
        let content = b"hello world\nfoo bar\n";
        let args = GrepArgs::parse_from(["grep", "-b", "bar"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();

        let results = search_in_content(content, &matcher, &args);
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].0, 2);
        assert_eq!(results[0].2, 4); // "bar" starts at byte offset 4 in "foo bar"
    }

    #[test]
    fn test_search_in_content_after_context() {
        let content = b"a\nMATCH\nb\nc\n";
        let args = GrepArgs::parse_from(["grep", "-A", "1", "MATCH"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();
        let results = search_in_content(content, &matcher, &args);
        // line 2 (match) + line 3 (trailing context)
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, 2);
        assert!(!results[0].3, "match line must not be marked context");
        assert_eq!(results[1].0, 3);
        assert!(results[1].3, "trailing line must be marked context");
    }

    #[test]
    fn test_search_in_content_symmetric_context() {
        let content = b"a\nb\nMATCH\nc\nd\n";
        let args = GrepArgs::parse_from(["grep", "-C", "1", "MATCH"]);
        let patterns = collect_patterns(&args).unwrap();
        let matcher = build_matcher(&patterns, &args).unwrap();
        let results = search_in_content(content, &matcher, &args);
        // lines 2 (ctx), 3 (match), 4 (ctx)
        assert_eq!(results.len(), 3);
        assert_eq!(
            results.iter().filter(|r| !r.3).count(),
            1,
            "exactly one match"
        );
        assert_eq!(results[0].0, 2);
        assert!(results[0].3);
        assert_eq!(results[1].0, 3);
        assert!(!results[1].3);
        assert_eq!(results[2].0, 4);
        assert!(results[2].3);
    }

    #[test]
    #[serial]
    fn test_colorize_match_basic() {
        let _guard = ColorOverrideReset;
        colored::control::set_override(true);
        let line = "hello world hello";
        let matcher = regex::Regex::new("hello").unwrap();
        let colored = colorize_match(line, &matcher);
        let plain = regex::Regex::new(r"\x1b\[[0-9;]*m")
            .unwrap()
            .replace_all(&colored, "");
        assert_eq!(plain, "hello world hello");
        assert!(colored.contains("\u{1b}["));
    }

    #[test]
    #[serial]
    fn test_colorize_match_regex_highlights_full_match() {
        let _guard = ColorOverrideReset;
        colored::control::set_override(true);
        let line = "foo123bar baz";
        let matcher = regex::Regex::new(r"foo\d+bar").unwrap();
        let colored = colorize_match(line, &matcher);
        let plain = regex::Regex::new(r"\x1b\[[0-9;]*m")
            .unwrap()
            .replace_all(&colored, "");
        assert_eq!(plain, "foo123bar baz");
        assert!(colored.contains("\u{1b}["));
    }

    #[test]
    #[serial]
    fn test_colorize_match_case_insensitive() {
        let _guard = ColorOverrideReset;
        colored::control::set_override(true);
        let line = "Hello World HELLO";
        let matcher = regex::RegexBuilder::new("hello")
            .case_insensitive(true)
            .build()
            .unwrap();
        let colored = colorize_match(line, &matcher);
        let plain = regex::Regex::new(r"\x1b\[[0-9;]*m")
            .unwrap()
            .replace_all(&colored, "");
        assert_eq!(plain, "Hello World HELLO");
        assert!(colored.contains("\u{1b}["));
    }

    #[test]
    fn test_colorize_match_preserves_content() {
        let line = "hello world";
        let matcher = regex::Regex::new("hello").unwrap();
        let colored = colorize_match(line, &matcher);
        // Remove ANSI codes and check content is preserved
        let plain = regex::Regex::new(r"\x1b\[[0-9;]*m")
            .unwrap()
            .replace_all(&colored, "");
        assert_eq!(plain, "hello world");
    }
}
