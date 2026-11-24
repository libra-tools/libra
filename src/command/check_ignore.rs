//! `libra check-ignore` — report which pathnames are excluded by Git/Libra
//! ignore rules, mirroring `git check-ignore`.
//!
//! For each pathname (positional or read from `--stdin`), the command consults
//! the same ignore engine `status` / `add` use (via
//! [`util::check_gitignore_match`]) and reports the paths that are ignored. With
//! `-v` it also reports the deciding pattern's source file, line, and text.
//!
//! Exit codes follow Git: `0` when at least one path is ignored, `1` when none
//! are (a clean signal, not an error), and `128` for a usage/repository error.
//!
//! Libra-specific extension files are read alongside Git's standard ignore
//! sources.

use std::{
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
};

use clap::Parser;
use git_internal::internal::index::Index;
use serde::Serialize;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    path, util,
};

/// `--help` examples shown in `libra check-ignore --help` output.
///
/// Pins the common invocations (single path, verbose attribution, stdin
/// streaming, `-z` NUL framing, `--no-index` debugging, JSON for agents) per the
/// cross-cutting `--help` EXAMPLES contract in
/// `docs/development/commands/_general.md`.
pub const CHECK_IGNORE_EXAMPLES: &str = "\
EXAMPLES:
    libra check-ignore target/            Print the path if it is ignored
    libra check-ignore -v build/ a.log    Show the source/line/pattern that ignores each path
    libra check-ignore --stdin < paths    Read newline-separated pathnames from stdin
    libra check-ignore -z --stdin         NUL-delimited input and output (safe for odd names)
    libra check-ignore -v -n a.txt b.log  Also list non-matching paths (requires -v)
    libra check-ignore --no-index a.log   Match rules even for a tracked path (debugging)
    libra check-ignore --json target/     Structured JSON output for agents";

/// Report which pathnames are excluded by Git/Libra ignore rules.
#[derive(Parser, Debug)]
#[command(after_help = CHECK_IGNORE_EXAMPLES)]
pub struct CheckIgnoreArgs {
    /// Pathnames to check against the ignore rules. Omit when using `--stdin`.
    #[clap(value_name = "PATHNAME")]
    pub pathspec: Vec<String>,

    /// Read pathnames from standard input, one per line (or NUL-separated with
    /// `-z`), instead of from the command line.
    #[clap(long = "stdin")]
    pub stdin: bool,

    /// Use NUL (`\0`) as the delimiter for `--stdin` input and for output,
    /// instead of newlines. Safe for pathnames containing whitespace.
    #[clap(short = 'z')]
    pub null: bool,

    /// Verbose: for every checked path, also show the source file, line number,
    /// and the deciding pattern (`<source>:<line>:<pattern>\t<path>`).
    #[clap(short = 'v', long = "verbose")]
    pub verbose: bool,

    /// Also output pathnames that match no pattern. Requires `--verbose`.
    #[clap(short = 'n', long = "non-matching")]
    pub non_matching: bool,

    /// Do not consult the index: report a pattern match even for a path that is
    /// tracked. Useful for debugging why a tracked path was not ignored.
    #[clap(long = "no-index")]
    pub no_index: bool,
}

/// One checked pathname and its ignore verdict (JSON / structured output).
#[derive(Debug, Serialize)]
pub struct CheckIgnoreEntry {
    /// The pathname exactly as the user supplied it.
    pub path: String,
    /// Whether the path is excluded by an ignore rule.
    pub ignored: bool,
    /// The ignore file that supplied the deciding pattern, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// 1-based line number of the deciding pattern within `source`, if known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    /// The deciding pattern, if any (includes a leading `!` for a whitelist).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

/// Full check-ignore result for `--json` output.
#[derive(Debug, Serialize)]
pub struct CheckIgnoreOutput {
    pub results: Vec<CheckIgnoreEntry>,
}

pub async fn execute(args: CheckIgnoreArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point returning [`CliResult`]. Returns
/// `Err(CliError::silent_exit(1))` (exit 1, no output) when no path is ignored,
/// matching `git check-ignore`'s exit-code contract.
pub async fn execute_safe(args: CheckIgnoreArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // --- Argument validation. These are fatal usage errors; Git's check-ignore
    // exits 128 for them (not a clap usage 129), so the exit code is overridden
    // to 128 to match the documented contract. ---
    if args.non_matching && !args.verbose {
        return Err(
            CliError::command_usage("-n/--non-matching requires -v/--verbose")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(128),
        );
    }
    if args.stdin && !args.pathspec.is_empty() {
        return Err(
            CliError::command_usage("cannot specify pathnames with --stdin")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(128),
        );
    }
    if output.quiet && args.verbose {
        return Err(
            CliError::command_usage("--quiet cannot be combined with -v/--verbose")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(128),
        );
    }
    if output.quiet && !args.stdin && args.pathspec.len() > 1 {
        return Err(
            CliError::command_usage("--quiet accepts at most one pathname")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(128),
        );
    }
    if args.null && !args.stdin {
        return Err(CliError::command_usage("-z/--null requires --stdin")
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(128));
    }
    if !args.stdin && args.pathspec.is_empty() {
        return Err(
            CliError::command_usage("no pathnames specified; use --stdin or pass paths")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(128),
        );
    }

    let index = if args.no_index {
        None
    } else {
        Some(Index::load(path::index()).map_err(|error| {
            CliError::fatal(format!("failed to load index: {error}"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?)
    };

    let workdir = util::working_dir();

    if args.stdin && !output.is_json() {
        return stream_stdin(&args, output, &workdir, index.as_ref());
    }

    let paths = if args.stdin {
        read_stdin_paths(args.null)?
    } else {
        args.pathspec.clone()
    };

    let mut results = Vec::with_capacity(paths.len());
    for path_str in paths {
        results.push(classify_path(&path_str, &workdir, index.as_ref())?);
    }

    let any_ignored = results.iter().any(|entry| entry.ignored);
    // `-v` counts a matching NEGATED rule (`!pattern`) as a match too: the
    // deciding rule is recorded even though the path is not ignored.
    let any_match = results.iter().any(|entry| entry.source.is_some());

    render(&args, &results, output)?;

    let matched = if args.verbose && !output.quiet {
        any_match
    } else {
        any_ignored
    };
    if matched {
        Ok(())
    } else {
        // Exit 1 with no further output: "none of the paths are ignored".
        Err(CliError::silent_exit(1))
    }
}

/// Decide whether one pathname is ignored and capture the deciding rule.
///
/// Out-of-worktree paths are a fatal error (exit 128, matching Git), not a
/// silent non-match: a lexically-normalised absolute path that does not stay
/// under the worktree is rejected *before* the matcher runs (the matcher
/// asserts containment and would otherwise read ignore files outside the
/// repository when a `..` sequence escapes it).
fn classify_path(
    path_str: &str,
    workdir: &Path,
    index: Option<&Index>,
) -> CliResult<CheckIgnoreEntry> {
    let none = |ignored: bool| CheckIgnoreEntry {
        path: path_str.to_string(),
        ignored,
        source: None,
        line: None,
        pattern: None,
    };

    // Git resolves relative pathnames against the CALLER's current directory,
    // not the repository root; the original spelling is echoed in the output.
    let cwd = std::env::current_dir().map_err(|error| {
        CliError::fatal(format!("failed to resolve the current directory: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    let raw = if Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        cwd.join(path_str)
    };
    // Collapse `.`/`..` lexically (no filesystem/symlink resolution — the path
    // need not exist) so containment can be checked safely.
    let normalized = normalize_lexical(&raw);
    let Ok(relative) = normalized.strip_prefix(workdir) else {
        return Err(
            CliError::fatal(format!("path '{path_str}' is outside the repository"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        );
    };
    let relative_key = relative.to_string_lossy().replace('\\', "/");

    // Without --no-index, a tracked path is reported as NOT ignored (an explicit
    // `add` overrides the ignore rules), matching Git. The key is the
    // repo-relative path so absolute and relative inputs resolve identically.
    if let Some(index) = index
        && index.tracked(&relative_key, 0)
    {
        return Ok(none(false));
    }

    Ok(match util::check_gitignore_match(workdir, &normalized) {
        Some(info) => CheckIgnoreEntry {
            path: path_str.to_string(),
            ignored: info.ignored,
            source: info.source.map(|source| display_source(&source, workdir)),
            line: info.line,
            pattern: if info.pattern.is_empty() {
                None
            } else {
                Some(info.pattern)
            },
        },
        None => none(false),
    })
}

/// Lexically normalise a path by collapsing `.` and `..` components without
/// touching the filesystem (the target may not exist). A leading root/prefix is
/// preserved; `..` past the root is a no-op.
fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Render the ignore source path relative to the worktree root when it lives
/// inside it (so output reads `sub/.gitignore`, not an absolute path).
fn display_source(source: &Path, workdir: &Path) -> String {
    source
        .strip_prefix(workdir)
        .unwrap_or(source)
        .to_string_lossy()
        .into_owned()
}

/// Stream `--stdin` records: read one record, classify, write and flush, so a
/// producer that keeps stdin open sees each answer immediately (Git's
/// `maybe_flush_or_die` behaviour). `-z` records are NUL-delimited and never
/// C-(un)quoted; line records starting with `"` are C-unquoted and outputs are
/// C-quoted only when the path needs it.
fn stream_stdin(
    args: &CheckIgnoreArgs,
    output: &OutputConfig,
    workdir: &Path,
    index: Option<&Index>,
) -> CliResult<()> {
    use std::io::{BufRead, Write};

    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let stdout = io::stdout();
    let mut writer = stdout.lock();
    let mut any_match = false;

    loop {
        let mut buffer = Vec::new();
        let delimiter = if args.null { 0u8 } else { b'\n' };
        let read = reader
            .read_until(delimiter, &mut buffer)
            .map_err(|error| CliError::io(format!("failed to read stdin: {error}")))?;
        if read == 0 {
            break;
        }
        if buffer.last() == Some(&delimiter) {
            buffer.pop();
        }
        if !args.null && buffer.last() == Some(&b'\r') {
            buffer.pop();
        }
        if buffer.is_empty() {
            continue;
        }
        let raw = String::from_utf8_lossy(&buffer).into_owned();
        let path_str = if args.null {
            raw
        } else if raw.starts_with('"') {
            unquote_c_style(&raw)?
        } else {
            raw
        };
        let entry = classify_path(&path_str, workdir, index)?;
        if entry.ignored || (args.verbose && entry.source.is_some()) {
            any_match = true;
        }
        if !output.quiet {
            write_entry(&mut writer, args, &entry, args.null)?;
            writer
                .flush()
                .map_err(|error| CliError::io(format!("failed to flush output: {error}")))?;
        }
    }

    if any_match {
        Ok(())
    } else {
        Err(CliError::silent_exit(1))
    }
}

/// C-style unquote for a `"..."` record (Git's `unquote_c_style`).
fn unquote_c_style(input: &str) -> CliResult<String> {
    let bad = || {
        CliError::command_usage(format!("line is badly quoted: {input}"))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    };
    let mut chars = input.char_indices();
    let Some((_, '"')) = chars.next() else {
        return Err(bad());
    };
    let mut out = String::new();
    while let Some((index, ch)) = chars.next() {
        match ch {
            '"' => {
                if index + 1 != input.len() {
                    return Err(bad());
                }
                return Ok(out);
            }
            '\\' => {
                let Some((_, escaped)) = chars.next() else {
                    return Err(bad());
                };
                out.push(match escaped {
                    'a' => '\x07',
                    'b' => '\x08',
                    'f' => '\x0c',
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    'v' => '\x0b',
                    '\\' => '\\',
                    '"' => '"',
                    '0'..='7' => {
                        let mut value = escaped.to_digit(8).unwrap_or(0);
                        for _ in 0..2 {
                            let Some((_, digit)) = chars.clone().next() else {
                                break;
                            };
                            let Some(d) = digit.to_digit(8) else { break };
                            chars.next();
                            value = value * 8 + d;
                        }
                        char::from_u32(value).unwrap_or('\0')
                    }
                    _ => return Err(bad()),
                });
            }
            other => out.push(other),
        }
    }
    Err(bad())
}

/// Quote a path for terminal output only when needed (Git's `quote_c_style`).
fn quote_c_style(path: &str) -> String {
    let needs_quotes = path
        .chars()
        .any(|ch| ch <= ' ' || ch == '"' || ch == '\\' || ch == '\x7f');
    if !needs_quotes {
        return path.to_string();
    }
    let mut out = String::with_capacity(path.len() + 2);
    out.push('"');
    for ch in path.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\x07' => out.push_str("\\a"),
            '\x08' => out.push_str("\\b"),
            '\x0c' => out.push_str("\\f"),
            '\x0b' => out.push_str("\\v"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

fn write_entry<W: Write>(
    writer: &mut W,
    args: &CheckIgnoreArgs,
    entry: &CheckIgnoreEntry,
    null: bool,
) -> CliResult<()> {
    let terminator = if null { '\0' } else { '\n' };
    if args.verbose {
        let matched = entry.pattern.is_some() || entry.source.is_some() || entry.ignored;
        if !matched && !args.non_matching {
            return Ok(());
        }
        let quoted = |value: &str| {
            if null {
                value.to_string()
            } else {
                quote_c_style(value)
            }
        };
        let source = entry.source.as_deref().map(quoted).unwrap_or_default();
        let line = entry.line.map(|l| l.to_string()).unwrap_or_default();
        let pattern = entry.pattern.as_deref().map(quoted).unwrap_or_default();
        let path = quoted(&entry.path);
        if null {
            // Git's `-z -v`: the four columns are NUL-terminated fields.
            write!(writer, "{source}\0{line}\0{pattern}\0{path}\0")
                .map_err(|error| CliError::io(format!("failed to write output: {error}")))
        } else {
            write!(writer, "{source}:{line}:{pattern}\t{path}{terminator}")
                .map_err(|error| CliError::io(format!("failed to write output: {error}")))
        }
    } else if entry.ignored {
        let path = if null {
            entry.path.clone()
        } else {
            quote_c_style(&entry.path)
        };
        write!(writer, "{path}{terminator}")
            .map_err(|error| CliError::io(format!("failed to write output: {error}")))
    } else {
        Ok(())
    }
}

/// Upper bound on `--stdin` input (64 MiB). A pathname list this large is
/// pathological; bounding the read avoids an unbounded-memory DoS on malformed
/// or hostile input.
const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

/// Read pathnames from stdin, split on NUL when `null` is set, else on newlines.
/// The read is capped at [`MAX_STDIN_BYTES`]; exceeding it is a fatal error
/// rather than an unbounded allocation. In newline mode a trailing `\r` is
/// stripped (CRLF tolerance); in NUL mode the bytes are taken verbatim because a
/// `\r` may be a legitimate part of a pathname.
fn read_stdin_paths(null: bool) -> CliResult<Vec<String>> {
    let stdin = io::stdin();
    let mut buf = Vec::new();
    let read = stdin
        .lock()
        .take(MAX_STDIN_BYTES + 1)
        .read_to_end(&mut buf)
        .map_err(|error| CliError::io(format!("failed to read --stdin: {error}")))?
        as u64;
    if read > MAX_STDIN_BYTES {
        return Err(CliError::fatal(format!(
            "--stdin input exceeds the {MAX_STDIN_BYTES}-byte limit"
        ))
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let text = String::from_utf8(buf).map_err(|_| {
        CliError::command_usage("--stdin input is not valid UTF-8")
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(128)
    })?;
    let sep = if null { '\0' } else { '\n' };
    Ok(text
        .split(sep)
        .map(|line| {
            if null {
                line
            } else {
                line.trim_end_matches('\r')
            }
        })
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect())
}

fn render(
    args: &CheckIgnoreArgs,
    results: &[CheckIgnoreEntry],
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data(
            "check-ignore",
            &CheckIgnoreOutput {
                results: results.iter().map(clone_entry).collect(),
            },
            output,
        );
    }
    if output.quiet {
        return Ok(());
    }

    let stdout = io::stdout();
    let mut writer = stdout.lock();
    for entry in results {
        write_entry(&mut writer, args, entry, args.null)?;
    }
    Ok(())
}

fn clone_entry(entry: &CheckIgnoreEntry) -> CheckIgnoreEntry {
    CheckIgnoreEntry {
        path: entry.path.clone(),
        ignored: entry.ignored,
        source: entry.source.clone(),
        line: entry.line,
        pattern: entry.pattern.clone(),
    }
}
