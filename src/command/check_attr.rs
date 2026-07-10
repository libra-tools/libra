//! `libra check-attr` — report Libra attributes for pathnames, the analogue of
//! `git check-attr` adapted to Libra's attribute model.
//!
//! Libra reads Git attributes sources plus `.libra_attributes` extension files
//! and reports them without running filters. This preserves the D5 intentional
//! difference: Libra does not implement the Git `.gitattributes` smudge/clean
//! filter bridge, so `check-attr` is a read-only query, not a filter driver.
//!
//! Exit codes: `0` on success (even when every queried attribute is
//! `unspecified`), `128` on a usage/repository error.

use std::{
    io::{self, Read, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use serde::Serialize;

use crate::utils::{
    attributes::{self, AttributeState},
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    util,
};

/// Git's value for an attribute that is not set on a path.
const UNSPECIFIED: &str = "unspecified";

/// Upper bound on `--stdin` input (64 MiB), guarding against unbounded reads.
const MAX_STDIN_BYTES: u64 = 64 * 1024 * 1024;

/// `--help` examples shown in `libra check-attr --help` output.
///
/// Per the cross-cutting `--help` EXAMPLES contract in
/// `docs/development/commands/_general.md`.
pub const CHECK_ATTR_EXAMPLES: &str = "\
EXAMPLES:
    libra check-attr filter a.bin         Report the `filter` attribute for a.bin
    libra check-attr filter -- a.bin b.c  Use `--` to separate attributes from paths
    libra check-attr --all data.bin       Report every attribute set on the path
    libra check-attr filter --stdin       Read pathnames from stdin
    libra check-attr -z filter --stdin    NUL-delimited stdin input and output
    libra check-attr --json filter a.bin  Structured JSON output for agents";

/// Report Git/Libra attributes for the given pathnames.
#[derive(Parser, Debug)]
#[command(after_help = CHECK_ATTR_EXAMPLES)]
pub struct CheckAttrArgs {
    /// Attribute names to query (before `--`). Ignored with `--all`.
    #[clap(value_name = "ATTR")]
    pub attrs: Vec<String>,

    /// Pathnames to query (after `--`).
    #[clap(value_name = "PATHNAME", last = true)]
    pub paths: Vec<String>,

    /// Report every attribute set on each path instead of named attributes.
    #[clap(short = 'a', long = "all")]
    pub all: bool,

    /// Read pathnames from standard input (newline-separated, or NUL-separated
    /// with `-z`) instead of the command line.
    #[clap(long = "stdin")]
    pub stdin: bool,

    /// Use NUL (`\0`) as the delimiter for `--stdin` input and for output.
    #[clap(short = 'z')]
    pub null: bool,
}

/// One `(path, attr, value)` triple of structured output.
#[derive(Debug, Clone, Serialize)]
pub struct CheckAttrEntry {
    pub path: String,
    pub attr: String,
    pub value: String,
}

/// Full check-attr result for `--json` output.
#[derive(Debug, Serialize)]
pub struct CheckAttrOutput {
    pub results: Vec<CheckAttrEntry>,
}

pub async fn execute(args: CheckAttrArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point returning [`CliResult`]. Usage/repository errors exit 128
/// (Git's fatal class); otherwise the command always succeeds (exit 0).
pub async fn execute_safe(args: CheckAttrArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let usage = |message: &str| {
        CliError::command_usage(message.to_string())
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(128)
    };

    // Resolve attribute names and command-line pathnames from the positional
    // arguments. `--` separates attrs (before) from paths (after); `--all`
    // queries every attribute and treats all positionals as paths; otherwise the
    // first positional is the attribute and the rest are paths.
    let (attr_names, cli_paths): (Vec<String>, Vec<String>) = if args.all {
        (
            Vec::new(),
            [args.attrs.clone(), args.paths.clone()].concat(),
        )
    } else if !args.paths.is_empty() {
        (args.attrs.clone(), args.paths.clone())
    } else if args.stdin {
        (args.attrs.clone(), Vec::new())
    } else {
        let mut positionals = args.attrs.clone();
        if positionals.len() < 2 {
            return Err(usage(
                "specify <attr> and <pathname>..., or use --all / --stdin / '--'",
            ));
        }
        let attr = positionals.remove(0);
        (vec![attr], positionals)
    };

    let paths = if args.stdin {
        if !cli_paths.is_empty() {
            return Err(usage("cannot specify pathnames with --stdin"));
        }
        read_stdin_paths(args.null)?
    } else {
        cli_paths
    };

    if !args.all && attr_names.is_empty() {
        return Err(usage("no attributes specified"));
    }
    if paths.is_empty() {
        return Err(usage("no pathnames specified; use --stdin or pass paths"));
    }

    let workdir = util::working_dir();
    let mut results = Vec::new();
    for path_str in &paths {
        let absolute = resolve_workdir_path(path_str, &workdir);
        if args.all {
            // Report only the attributes that are actually set.
            for (attr, state) in attributes::all_attribute_states_for_path(&absolute) {
                if let Some(value) = state.check_attr_value() {
                    results.push(CheckAttrEntry {
                        path: path_str.clone(),
                        attr,
                        value,
                    });
                }
            }
        } else {
            for attr in &attr_names {
                results.push(CheckAttrEntry {
                    path: path_str.clone(),
                    attr: attr.clone(),
                    value: attribute_value(attr, &absolute),
                });
            }
        }
    }

    render(&args, &results, output)
}

/// The value of `attr` for a path. `None` / `!attr` reports Git's
/// `unspecified`; bare attrs report `set`; `-attr` reports `unset`; valued attrs
/// report their value.
fn attribute_value(attr: &str, absolute: &Path) -> String {
    attributes::attribute_state_for_path(attr, absolute)
        .and_then(|state| match state {
            AttributeState::Unspecified => None,
            other => other.check_attr_value(),
        })
        .unwrap_or_else(|| UNSPECIFIED.to_string())
}

fn resolve_workdir_path(path_str: &str, workdir: &Path) -> PathBuf {
    if Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        workdir.join(path_str)
    }
}

/// Read pathnames from stdin, split on NUL when `null` is set, else newlines.
/// Bounded at [`MAX_STDIN_BYTES`]. A trailing `\r` is stripped only in newline
/// mode (a `\r` may be a legitimate byte of a NUL-framed pathname).
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
    args: &CheckAttrArgs,
    results: &[CheckAttrEntry],
    output: &OutputConfig,
) -> CliResult<()> {
    if output.is_json() {
        return emit_json_data(
            "check-attr",
            &CheckAttrOutput {
                results: results.to_vec(),
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
        // Without `-z`: `<path>: <attr>: <value>` (Git's human format). With
        // `-z`: the three fields NUL-separated and NUL-terminated.
        let line = if args.null {
            format!("{}\0{}\0{}\0", entry.path, entry.attr, entry.value)
        } else {
            format!("{}: {}: {}\n", entry.path, entry.attr, entry.value)
        };
        write!(writer, "{line}")
            .map_err(|error| CliError::io(format!("failed to write output: {error}")))?;
    }
    Ok(())
}
