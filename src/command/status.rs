//! Implements status reporting with ignore policy support, computing staged/unstaged/untracked sets and printing concise summaries.

use std::{
    collections::{HashMap, HashSet, VecDeque},
    io,
    io::{IsTerminal, Write},
    path::{Path, PathBuf},
};

use clap::{Parser, ValueEnum};
use git_internal::{
    errors::GitError,
    hash::{ObjectHash, get_hash_kind},
    internal::{
        index::Index,
        object::{
            commit::Commit,
            tree::{Tree, TreeItemMode},
        },
    },
};
use serde::Serialize;

use super::{
    merge, rename_detect, stash, status_untracked,
    unmerged::{self, UnmergedEntry},
};
use crate::{
    command::calc_file_blob_hash,
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        head::Head,
    },
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        ignore::IgnorePolicy,
        object_ext::{CommitExt, TreeExt},
        output::{ColorChoice, OutputConfig, emit_json_data},
        path,
        pathspec::{PathspecError, PathspecSet},
        util,
    },
};

// ---------------------------------------------------------------------------
// Args & enums
// ---------------------------------------------------------------------------

const STATUS_EXAMPLES: &str = "\
EXAMPLES:
    libra status                       Show working tree status
    libra status -s                    Short format output
    libra status --porcelain           Machine-readable output (v1)
    libra status --porcelain v2        Extended machine-readable output
    libra status -sb                   Include branch info in short output (-b = --branch)
    libra status --show-stash          Show stash count
    libra status --ignored             Include ignored files
    libra status -uno                  Hide untracked files (-u = --untracked-files; bare -u = all)
    libra status --renames             Detect renames (--no-renames disables)
    libra status --json                Structured JSON output for agents
    libra status --exit-code           Exit 1 if working tree is dirty
    libra status --quiet --exit-code   Silent dirty check for scripts";

/// Show the working tree status.
// EXAMPLES are wired via `#[command(after_help = STATUS_EXAMPLES)]` and render
// at the bottom of `libra status --help`. The meta-commentary that used to
// live here as a `///` line leaked into clap's `--help` body (see
// `tests/command/status_test.rs::test_status_help_does_not_leak_impl_meta`).
#[derive(Parser, Debug, Default, Clone)]
#[command(after_help = STATUS_EXAMPLES)]
pub struct StatusArgs {
    /// Output in a machine-readable format (default v1). Use v2 for extended format.
    #[clap(
        long = "porcelain",
        value_name = "VERSION",
        num_args = 0..=1,
        default_missing_value = "v1",
        conflicts_with = "short"
    )]
    pub porcelain: Option<PorcelainVersion>,

    /// Give the output in the short-format
    #[clap(short = 's', long = "short", conflicts_with = "porcelain")]
    pub short: bool,

    /// Give the output in the long-format. This is Libra's default, so the flag
    /// is accepted for Git parity and simply selects the default rendering;
    /// it conflicts with `--short`/`--porcelain`.
    #[clap(long = "long", conflicts_with_all = ["short", "porcelain"])]
    pub long_format: bool,

    /// Output with branch info (short or porcelain mode)
    #[clap(short = 'b', long = "branch")]
    pub branch: bool,

    /// Do not show branch info in the short format, overriding
    /// `status.branch=true` (and an earlier `--branch`; the last one wins).
    #[clap(long = "no-branch", overrides_with = "branch")]
    pub no_branch: bool,

    /// Show ahead/behind counts in branch info (default: true).
    /// Use --no-ahead-behind to suppress the counts.
    #[clap(long = "ahead-behind")]
    pub ahead_behind: bool,

    /// Suppress ahead/behind counts in branch info.
    #[clap(long = "no-ahead-behind", overrides_with = "ahead_behind")]
    pub no_ahead_behind: bool,

    /// Output with stash info (only in standard mode)
    #[clap(long = "show-stash")]
    pub show_stash: bool,

    /// Do not show the stash hint, overriding `status.showStash=true` (and an
    /// earlier `--show-stash`; the last one wins).
    #[clap(long = "no-show-stash", overrides_with = "show_stash")]
    pub no_show_stash: bool,

    /// Show ignored files
    #[clap(long = "ignored")]
    pub ignored: bool,

    /// Control untracked files display: `no`, `normal` (the default when both
    /// the flag and `status.showUntrackedFiles` are absent), or `all`. As in
    /// Git, the short `-u`/long `--untracked-files` with no value means `all`
    /// (e.g. `-u`, `-uno`, `--untracked-files=all`); when the flag is absent
    /// the `status.showUntrackedFiles` config default applies.
    #[clap(
        short = 'u',
        long = "untracked-files",
        value_name = "MODE",
        num_args = 0..=1,
        default_missing_value = "all"
    )]
    pub untracked_files: Option<UntrackedFiles>,

    /// Libra extension (lore.md 1.1): consume the dirty-set cache instead of
    /// walking the working tree. Requires a fresh cache (`status --scan`);
    /// a missing/stale cache degrades to the full reconcile with a hint.
    /// NOTE: unrelated to Git's `--cached` (= the index) — this reads Libra's
    /// `working_dirty` SQLite cache.
    #[clap(long = "cached", conflicts_with_all = ["check_dirty", "scan", "porcelain", "short", "ignored", "renames", "no_renames", "find_renames"])]
    pub cached: bool,

    /// Libra extension (lore.md 1.1): re-verify ONLY the cached dirty set
    /// (O(dirty paths)) — rows re-verified clean are pruned; nothing new is
    /// discovered. Degrades to the full reconcile when the cache is stale.
    #[clap(long = "check-dirty", conflicts_with_all = ["cached", "scan", "porcelain", "short", "ignored", "renames", "no_renames", "find_renames"])]
    pub check_dirty: bool,

    /// Libra extension (lore.md 1.1): run the normal full status AND rebuild
    /// the dirty-set cache atomically from it (the only authoritative writer).
    #[clap(long = "scan", conflicts_with_all = ["cached", "check_dirty", "porcelain", "short", "ignored"])]
    pub scan: bool,

    /// Print status entries with columns aligned (human output only).
    #[clap(long = "column", overrides_with = "no_column")]
    pub column: bool,

    /// Do not print status entries in columns (equivalent to `--column=never`),
    /// countermanding an earlier `--column` (last one on the command line wins),
    /// matching `git status --no-column`. Status is not columnar by default, so
    /// on its own this is a no-op.
    #[clap(long = "no-column", overrides_with = "column")]
    pub no_column: bool,

    /// Terminate each status entry with a NUL byte instead of a newline.
    /// This is intended for machine-readable short/porcelain output.
    #[clap(
        short = 'z',
        long = "null",
        conflicts_with = "long_format",
        conflicts_with = "cached",
        conflicts_with = "check_dirty",
        conflicts_with = "scan"
    )]
    pub null_terminated: bool,

    /// Detect renames in staged/unstaged changes.
    /// The optional value is the similarity threshold percentage (default 50).
    #[clap(
        long = "find-renames",
        value_name = "PERCENT",
        num_args = 0..=1,
        default_missing_value = "50",
        overrides_with = "find_renames",
        value_parser = clap::value_parser!(u8).range(..=100)
    )]
    pub find_renames: Option<u8>,

    /// Enable rename detection at the default threshold (Git's
    /// `--renames`); the LAST of the three rename spellings wins.
    #[clap(long = "renames", overrides_with = "no_renames")]
    pub renames: bool,

    /// Disable rename detection (Git's `--no-renames`); the LAST of the
    /// three rename spellings on the command line wins.
    #[clap(long = "no-renames", overrides_with = "renames")]
    pub no_renames: bool,

    /// Exit with code 1 if the working tree has changes.
    /// Can be combined with --quiet for silent dirty checking.
    #[clap(long = "exit-code")]
    pub exit_code: bool,

    /// Limit status output to files matching the given pathspec(s).
    #[clap(value_name = "pathspec")]
    pub pathspec: Vec<String>,

    /// Resolved `status.relativePaths` (config-only, like Git): `true` (the
    /// default) renders human long/short paths relative to the current
    /// directory; `false` keeps repository-root-relative paths. Populated by
    /// [`apply_status_config_defaults`], never by the CLI.
    #[clap(skip = true)]
    pub relative_paths: bool,

    /// Resolved rename-detection default from `status.renames` (falling back
    /// to `diff.renames`), config-only. `None` = unset (feature default 50%
    /// applies); `Some(false)` = disabled; `Some(true)` = enabled at 50%.
    /// CLI flags (`--no-renames`/`--find-renames`/`--renames`) always win.
    /// Populated by [`apply_status_config_defaults`], never by the CLI.
    #[clap(skip)]
    pub renames_config: Option<bool>,
}

impl StatusArgs {
    /// Whether ahead/behind counts should be shown in branch info.
    fn show_ahead_behind(&self) -> bool {
        !self.no_ahead_behind
    }
}

/// The warnings that belong to ONE invocation (§B.4.3, R0-4).
///
/// Preflight advisories are buffered process-wide, which is right for a CLI
/// process that runs exactly one command — and wrong for anything else. In a
/// long-running `libra code` server the buffer accumulates whatever the
/// process has emitted since it started, so an API status collection would
/// report warnings that have nothing to do with the request, and would keep
/// reporting them.
///
/// The context is therefore passed EXPLICITLY (never read from a global or a
/// thread-local at the point of use): the CLI adopts the process buffer, the
/// API starts empty.
#[derive(Clone, Debug, Default)]
pub struct InvocationWarningCtx {
    preflight: Vec<String>,
}

impl InvocationWarningCtx {
    /// The CLI invocation: adopt what this process's preflight buffered.
    /// `reset_warning_tracker` clears it at the start of each invocation, so
    /// the buffer belongs to this command.
    pub fn from_process_preflight() -> Self {
        Self {
            preflight: crate::utils::output::pending_warning_messages(),
        }
    }

    /// An embedded/API invocation: no inherited warnings, and nothing this
    /// collection does may leak into the process-wide exit tracker.
    pub fn empty() -> Self {
        Self::default()
    }

    fn preflight_messages(&self) -> &[String] {
        &self.preflight
    }
}

/// One rename-threshold-affecting occurrence in argv order (§B.4.3, R0-4).
#[derive(Clone, Debug)]
pub(crate) enum RenameThresholdOccurrence {
    /// `--find-renames[=RAW]`; empty = bare.
    ///
    /// `OsString`, not `String` (§B.4.3): argv is not guaranteed to be UTF-8,
    /// and an occurrence that is NOT the last one is never interpreted — so a
    /// non-UTF-8 value that a later flag overrides must not fail, and must
    /// certainly not abort the process on the way in.
    FindRaw(std::ffi::OsString),
    /// `--renames`.
    EnableDefault,
    /// `--no-renames`.
    Disable,
}

/// Result of the pre-clap status argv normalization (§B.4.3, R0-4): the
/// rewritten argv (raw `--find-renames` values replaced by a clap-safe
/// placeholder) plus the full occurrence order so the LAST occurrence wins
/// across all three spellings — clap's pairwise `overrides_with` cannot
/// express that.
pub(crate) struct StatusArgvResolution {
    pub(crate) argv: Vec<std::ffi::OsString>,
    rename_occurrences: Vec<RenameThresholdOccurrence>,
    /// Which format-selecting flags the argv scan SAW (§B.4.3). Recorded by
    /// the same arity-driven pass that finds the rename occurrences, so a
    /// letter inside a short option's VALUE can never be mistaken for a flag.
    pub(crate) format: StatusFormatFlags,
}

/// Format flags as they appear in argv, independent of clap's parse.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StatusFormatFlags {
    /// `-z` / `--null` appeared (including inside a short cluster).
    pub(crate) z_explicit: bool,
    /// `-s` / `--short` appeared.
    pub(crate) short_explicit: bool,
    /// `--long` appeared.
    pub(crate) long_explicit: bool,
    /// `--porcelain[=N]` appeared, with its version when given.
    pub(crate) porcelain_explicit: Option<u8>,
    /// `--cached` / `--check-dirty` appeared.
    pub(crate) cached_mode: bool,
}

impl StatusFormatFlags {
    /// Every flag the argv scan SAW must be a flag clap also parsed.
    ///
    /// One-directional: config may set `short`/`porcelain` with nothing in
    /// argv. The other direction is the interesting one — it catches a
    /// cluster-scan bug (a letter inside a value read as a flag, or a flag
    /// missed inside a cluster) at the point where it would otherwise become
    /// output the user did not ask for.
    fn ensure_agrees_with(&self, args: &StatusArgs) -> CliResult<()> {
        let disagreement = if self.z_explicit && !args.null_terminated {
            Some("-z/--null")
        } else if self.short_explicit && !args.short {
            Some("-s/--short")
        } else if self.long_explicit && !args.long_format {
            Some("--long")
        } else if self.porcelain_explicit.is_some() && args.porcelain.is_none() {
            Some("--porcelain")
        } else if self.cached_mode && !(args.cached || args.check_dirty) {
            Some("--cached/--check-dirty")
        } else {
            None
        };
        match disagreement {
            None => Ok(()),
            Some(flag) => Err(CliError::fatal(format!(
                "internal: the argument scan saw `{flag}` but the parser did not"
            ))
            .with_stable_code(StableErrorCode::InternalInvariant)
            .with_hint("re-run without short-option clusters, and report this")),
        }
    }
}

/// Locate the `status`/`st` subcommand using the ROOT command's global-arg
/// metadata (never a hand-written flag list) and rewrite only its argument
/// slice. Any other subcommand — including `status` appearing as a pathspec
/// of another command — returns argv unchanged.
pub(crate) fn normalize_status_argv(
    raw_argv: Vec<std::ffi::OsString>,
    root: &clap::Command,
) -> StatusArgvResolution {
    let unchanged = |argv: Vec<std::ffi::OsString>| StatusArgvResolution {
        argv,
        rename_occurrences: Vec::new(),
        format: StatusFormatFlags::default(),
    };
    // Global long/short tables: name → takes-a-separate-value.
    let mut longs: std::collections::HashMap<String, bool> = std::collections::HashMap::new();
    let mut shorts: std::collections::HashMap<char, bool> = std::collections::HashMap::new();
    for arg in root.get_arguments() {
        // Consume a SEPARATE next token only when a value is REQUIRED and
        // `require_equals` is off; optional-value globals (`--json[=v]`)
        // never eat the following token, so they cannot shift the
        // subcommand position.
        let takes_value = arg
            .get_num_args()
            .map(|r| r.min_values() >= 1)
            .unwrap_or(false)
            && !arg.is_require_equals_set();
        if let Some(long) = arg.get_long() {
            longs.insert(long.to_string(), takes_value);
        }
        for alias in arg.get_all_aliases().unwrap_or_default() {
            longs.insert(alias.to_string(), takes_value);
        }
        if let Some(short) = arg.get_short() {
            shorts.insert(short, takes_value);
        }
    }
    // The short arity table for scanning clusters INSIDE the status slice:
    // the subcommand's own options PLUS the root globals, because clap
    // accepts a global after the subcommand too (`libra status -J=ndjson`).
    // Leaving the globals out made `ndjson` scan as a cluster, and its `s`
    // read as `--short`.
    //
    // `-u` takes an optional value, so `-buno` is `-b` plus `-u=no`, and the
    // letters of that value are never flags.
    let mut status_shorts: std::collections::HashMap<char, bool> = shorts.clone();
    if let Some(status) = root
        .get_subcommands()
        .find(|candidate| candidate.get_name() == "status")
    {
        for arg in status.get_arguments() {
            if let Some(short) = arg.get_short() {
                let takes_value = arg
                    .get_num_args()
                    .map(|range| range.max_values() >= 1)
                    .unwrap_or(false);
                status_shorts.insert(short, takes_value);
            }
        }
    }

    let mut i = 1usize; // skip argv[0]
    while i < raw_argv.len() {
        // A token that is not valid UTF-8 cannot be an ASCII option, so it is
        // a positional — which, before the subcommand, means "not status".
        let Some(token) = raw_argv[i].to_str() else {
            return unchanged(raw_argv);
        };
        if token == "--" {
            return unchanged(raw_argv); // root-level `--`: no subcommand area
        }
        if let Some(long) = token.strip_prefix("--") {
            let name = long.split_once('=').map(|(n, _)| n).unwrap_or(long);
            let attached = long.contains('=');
            match longs.get(name) {
                Some(true) if !attached => i += 2,
                Some(_) => i += 1,
                None => return unchanged(raw_argv), // unknown root option: clap will report
            }
            continue;
        }
        if let Some(cluster) = token.strip_prefix('-') {
            if cluster.is_empty() {
                break; // bare "-": positional
            }
            let mut consumed_value = false;
            let mut attached_equals = false;
            for ch in cluster.chars() {
                if ch == '=' {
                    // `-J=value` (require_equals short form): the remainder
                    // is an attached value — never option letters. clap
                    // validates the value later; the locator must not bail.
                    attached_equals = true;
                    break;
                }
                match shorts.get(&ch) {
                    Some(true) => {
                        consumed_value = true;
                        break; // rest of cluster (or next token) is the value
                    }
                    Some(false) => {}
                    None => return unchanged(raw_argv),
                }
            }
            if attached_equals {
                i += 1;
            } else if consumed_value && !cluster.ends_with(|c: char| shorts.get(&c) == Some(&true))
            {
                // value attached inside the cluster
                i += 1;
            } else if consumed_value {
                i += 2;
            } else {
                i += 1;
            }
            continue;
        }
        // First non-option token: the subcommand.
        if token != "status" && token != "st" {
            return unchanged(raw_argv);
        }
        // Rewrite the slice after the subcommand up to `--`.
        let mut argv = raw_argv.clone();
        let mut occurrences = Vec::new();
        let mut format = StatusFormatFlags::default();
        let mut j = i + 1;
        while j < argv.len() {
            // A non-UTF-8 token is a pathspec — EXCEPT for the one option
            // whose VALUE is allowed to be arbitrary bytes. Matching the
            // prefix on the platform representation is what lets
            // `--find-renames=<invalid utf-8>` be recognised, recorded, and
            // replaced by the placeholder; skipping it would leave the raw
            // bytes for clap, which rejects the whole command line.
            let Some(tok) = argv[j].to_str().map(str::to_string) else {
                if os_starts_with(&argv[j], "--find-renames=") {
                    occurrences.push(RenameThresholdOccurrence::FindRaw(raw_find_renames_value(
                        &argv[j],
                    )));
                    argv[j] = std::ffi::OsString::from("--find-renames=50");
                }
                j += 1;
                continue;
            };
            if tok == "--" {
                break;
            }
            if tok == "--find-renames" {
                occurrences.push(RenameThresholdOccurrence::FindRaw(std::ffi::OsString::new()));
                // Placeholder stops clap's num_args=0..=1 from eating the
                // next pathspec token.
                argv[j] = std::ffi::OsString::from("--find-renames=50");
            } else if tok.starts_with("--find-renames=") {
                // The RAW value is taken from the ORIGINAL `OsStr`, not from
                // the UTF-8 copy: `--find-renames=<invalid utf-8>` must reach
                // the resolver intact, and only fail if it is the occurrence
                // that wins.
                occurrences.push(RenameThresholdOccurrence::FindRaw(raw_find_renames_value(
                    &argv[j],
                )));
                // Placeholder keeps clap from rejecting Git raw syntax the
                // resolver validates later.
                argv[j] = std::ffi::OsString::from("--find-renames=50");
            } else if tok == "--renames" {
                occurrences.push(RenameThresholdOccurrence::EnableDefault);
            } else if tok == "--no-renames" {
                occurrences.push(RenameThresholdOccurrence::Disable);
            } else if tok == "--null" {
                format.z_explicit = true;
            } else if tok == "--short" {
                format.short_explicit = true;
            } else if tok == "--long" {
                format.long_explicit = true;
            } else if tok == "--porcelain" {
                format.porcelain_explicit = Some(1);
            } else if let Some(version) = tok.strip_prefix("--porcelain=") {
                format.porcelain_explicit = Some(version.parse().unwrap_or(1));
            } else if tok == "--cached" || tok == "--check-dirty" {
                format.cached_mode = true;
            } else if let Some(cluster) = tok
                .strip_prefix('-')
                .filter(|rest| !rest.is_empty() && !rest.starts_with('-'))
            {
                // Short cluster: interpreted through the merged arity table,
                // stopping at the first value-taking option OR at an `=` —
                // every character after that is a VALUE, not a flag. `-uno`
                // is `-u=no` and `-J=ndjson` is a global with an attached
                // value; neither contributes flags.
                for ch in cluster.chars() {
                    if ch == '=' {
                        break;
                    }
                    match status_shorts.get(&ch) {
                        Some(true) => break,
                        // An unknown letter means this is not a cluster we
                        // understand; clap will report it. Recording flags
                        // from the rest would be guessing.
                        None => break,
                        Some(false) => match ch {
                            'z' => format.z_explicit = true,
                            's' => format.short_explicit = true,
                            _ => {}
                        },
                    }
                }
            }
            j += 1;
        }
        return StatusArgvResolution {
            argv,
            rename_occurrences: occurrences,
            format,
        };
    }
    unchanged(raw_argv)
}

/// Does this argument begin with `prefix`, comparing the PLATFORM
/// representation rather than a UTF-8 rendering it may not have?
fn os_starts_with(token: &std::ffi::OsStr, prefix: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        token.as_bytes().starts_with(prefix.as_bytes())
    }
    #[cfg(not(unix))]
    {
        token.to_string_lossy().starts_with(prefix)
    }
}

/// The RAW bytes after `--find-renames=`, preserved as an `OsString`.
///
/// Splitting on the UTF-8 rendering would lose exactly the values this exists
/// to carry, so the split happens on the platform representation.
fn raw_find_renames_value(token: &std::ffi::OsStr) -> std::ffi::OsString {
    const PREFIX_LEN: usize = "--find-renames=".len();
    #[cfg(unix)]
    {
        use std::os::unix::ffi::{OsStrExt, OsStringExt};
        let bytes = token.as_bytes();
        std::ffi::OsString::from_vec(bytes[PREFIX_LEN.min(bytes.len())..].to_vec())
    }
    #[cfg(not(unix))]
    {
        // On platforms without a byte view, a non-UTF-8 value cannot be split
        // losslessly; such a token is not valid UTF-8 as a whole, and the
        // resolver rejects it if it wins.
        match token.to_str() {
            Some(text) => std::ffi::OsString::from(&text[PREFIX_LEN.min(text.len())..]),
            None => std::ffi::OsString::from(token),
        }
    }
}

/// Resolve the engine-scale rename threshold (§B.4.3): with a CLI resolution
/// the LAST occurrence wins (`Disable` → None, `EnableDefault`/bare/0 →
/// 30000, raw → Git score grammar via the shared diff parser); without one
/// (API/struct-literal path) the existing percent field + config cascade
/// applies. Cache modes force None upstream.
pub(crate) fn resolve_status_threshold(
    args: &StatusArgs,
    resolution: Option<&StatusArgvResolution>,
) -> CliResult<Option<u32>> {
    // §B.4.3: a cache mode forces detection OFF, and it does so FIRST —
    // before any occurrence or config value is considered. clap refuses the
    // flag combination on the command line, but `StatusArgs` is public: a
    // struct-literal caller can set `cached: true` with a threshold, and
    // resolving that to a live threshold would run rename detection against
    // a cache that cannot support it.
    if args.cached || args.check_dirty {
        return Ok(None);
    }
    if let Some(resolution) = resolution
        && let Some(last) = resolution.rename_occurrences.last()
    {
        return Ok(match last {
            RenameThresholdOccurrence::Disable => None,
            RenameThresholdOccurrence::EnableDefault => Some(30000),
            RenameThresholdOccurrence::FindRaw(raw) if raw.is_empty() => Some(30000),
            RenameThresholdOccurrence::FindRaw(raw) => {
                // UTF-8 is required only HERE, of the occurrence that WON. An
                // earlier `--find-renames=<invalid utf-8>` that a later flag
                // overrides is never interpreted, and never fails (§B.4.3) —
                // which is also why it had to survive as an `OsString`.
                let raw = raw.to_str().ok_or_else(|| {
                    CliError::command_usage(format!(
                        "invalid --find-renames value {}: not valid UTF-8",
                        raw.to_string_lossy()
                    ))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("use Git score syntax: N (0.N), N%, or a decimal")
                })?;
                let score =
                    crate::command::diff::options::parse_rename_score(raw).map_err(|_| {
                        CliError::command_usage(format!("invalid --find-renames value '{raw}'"))
                            .with_stable_code(StableErrorCode::CliInvalidArguments)
                            .with_hint("use Git score syntax: N (0.N), N%, or a decimal")
                    })?;
                Some(if score == 0 { 30000 } else { score })
            }
        });
    }
    // Legacy percent path (config cascade already resolved into args).
    Ok(if args.no_renames {
        None
    } else if let Some(percent) = args.find_renames {
        // §B.4.3: the API percent field accepts ONLY 0..=100. clap's range
        // guard covers the parser path; a struct-literal caller bypasses
        // clap, so the resolver validates too — silently clamping 101..255
        // to exact-only would misreport what the caller asked for.
        if percent > 100 {
            return Err(CliError::command_usage(format!(
                "invalid rename threshold percentage '{percent}' (expected 0-100)"
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_hint("pass a similarity percentage between 0 and 100"));
        }
        Some(u32::from(percent) * 600)
    } else if args.renames {
        Some(30000)
    } else {
        match args.renames_config {
            Some(false) => None,
            Some(true) | None => Some(30000),
        }
    })
}

/// Module-private resolved status config that deliberately does NOT live on
/// the public `StatusArgs` (adding fields there breaks exhaustive struct
/// literals in downstream code, even with the `Default` derive).
#[derive(Clone, Copy, Default)]
struct StatusConfigExtras {
    /// `status.renameUntracked` (§B.3.1): untracked worktree paths may be
    /// unstaged rename destinations only when true (default false = Git
    /// parity: a tracked→untracked move renders as `D` + `??`).
    rename_untracked: bool,
    /// Engine-scale rename threshold resolved by
    /// [`resolve_status_threshold`] (None = detection disabled). Cache modes
    /// force None at collection time regardless.
    rename_threshold: Option<u32>,
    /// `status.renameLimit` falling back to `diff.renameLimit` (§B.5):
    /// per-side inexact candidate cap, `0` = uncapped, default 1000.
    rename_limit: usize,
    /// `core.quotePath` (§B.6.6): default true (Git parity).
    quote_path: bool,
}

/// Structured status degradation warning (§B.5; reused verbatim by the JSON
/// `data.warnings[]` array). Human/short/porcelain modes render these on
/// stderr; JSON never writes them to stderr.
#[derive(Clone, Debug, serde::Serialize)]
pub struct StatusWarning {
    pub code: StatusWarningCode,
    pub message: String,
    pub source: StatusWarningSource,
}

/// Declare a warning enum together with its `ALL` registry from ONE
/// variant list: a variant cannot exist without appearing in `ALL`, so
/// the schema-snapshot and doc-closeout guards cannot be left green by a
/// forgotten registry entry (2026-08-06 R0-7 review; the old hand-written
/// `ALL` was a silently-driftable duplicate).
macro_rules! declare_status_warning_enum {
    (
        $(#[$enum_meta:meta])*
        $vis:vis enum $name:ident {
            $(
                $(#[$variant_meta:meta])*
                $variant:ident $(= $discriminant:literal)?
            ),+ $(,)?
        }
    ) => {
        $(#[$enum_meta])*
        $vis enum $name {
            $(
                $(#[$variant_meta])*
                $variant $(= $discriminant)?
            ),+
        }

        impl $name {
            /// Every variant, generated from the single declaration —
            /// exhaustive by construction.
            $vis const ALL: &'static [$name] = &[$($name::$variant),+];
        }
    };
}

declare_status_warning_enum! {
    /// Stable warning codes (§B.5), declared in the order the user-facing
    /// warning table lists them (`compat_r0_9_doc_closeout` walks `ALL` to
    /// assert the docs stay in sync, so a code added here without a doc
    /// row fails the build rather than shipping undocumented).
    /// Serialization names are pinned by `json_warnings_schema_snapshot`.
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    #[non_exhaustive]
    // Explicit discriminants preserve the ORIGINAL pre-macro declaration's
    // numeric values: this is a public fieldless enum embedders may cast,
    // and the table-ordered redeclaration must not silently renumber it
    // (2026-08-06 R0-7 review).
    pub enum StatusWarningCode {
        RenameLimitProductSkipped = 1,
        SimilarityBudgetExceeded = 0,
        /// §B.3.2: the rename-destination probe tripped a budget; partial
        /// destinations still pair, but detection may be incomplete.
        ProbeTruncated = 2,
        /// §B.6.1: a non-UTF-8 path was skipped as a rename candidate (its
        /// base D/A/`??` rows are unaffected).
        RenamePathEncodingUnsupported = 9,
        /// §B.3.4: repository-object reads for inexact scoring were skipped
        /// (missing/corrupt/unavailable objects); affected candidates
        /// dropped.
        MetadataUnavailable = 3,
        /// §B.3.4: an OBJECT read budget (per-object size cap, byte total,
        /// slot count, or deadline) was hit; affected candidates dropped,
        /// detection may be incomplete. The worktree-side equivalent is
        /// [`StatusWarningCode::WorktreeBudgetExceeded`] — the two are
        /// separate because `source` distinguishes `metadata` from
        /// `worktree`.
        MetadataBudgetExceeded = 4,
        /// §B.3.4: a WORKTREE read budget (per-file size cap, byte total,
        /// or task count) was hit during optional rename content reads;
        /// affected candidates dropped, detection may be incomplete.
        WorktreeBudgetExceeded = 5,
        /// §B.3.3: a worktree read failed (I/O) during optional rename
        /// content reads; the affected candidate was dropped.
        WorktreeReadFailed = 6,
        /// §B.6.0.1 reason taxonomy (R0-8 io_blocked contract).
        WorktreePermissionDenied = 7,
        /// §B.6.0.1 reason taxonomy (R0-8 io_blocked contract).
        WorktreeIoTimeout = 8,
        DirtyCacheLockStolen = 10,
        DirtyCacheStaleFallback = 11,
        DirtyCacheConcurrentInvalidate = 12,
        /// A path could not be encoded for the dirty cache (a non-UTF-8
        /// name), so its row was omitted from the snapshot. The base
        /// status is unaffected; `--cached` simply will not list that
        /// path.
        DirtyCachePathUnencodable = 13,
        /// A repository-level PREFLIGHT advisory raised before the command
        /// ran (e.g. a pending durable object-index repair). Carried in
        /// `warnings[]` so `--exit-code-on-warning` can never return 9
        /// with an empty structured list — §B.5 forbids a stderr-only
        /// channel.
        RepositoryPreflight = 14,
    }
}

impl StatusWarningCode {
    /// The subsystem a code is ALWAYS emitted under (§B.5 table). Every
    /// emit site derives its `source` from here instead of repeating the
    /// pairing, so the code→source mapping has exactly one definition and
    /// the published table can be checked against it.
    pub fn source(self) -> StatusWarningSource {
        match self {
            StatusWarningCode::ProbeTruncated => StatusWarningSource::Probe,
            StatusWarningCode::SimilarityBudgetExceeded
            | StatusWarningCode::RenameLimitProductSkipped
            | StatusWarningCode::RenamePathEncodingUnsupported => StatusWarningSource::RenameDetect,
            StatusWarningCode::MetadataUnavailable | StatusWarningCode::MetadataBudgetExceeded => {
                StatusWarningSource::Metadata
            }
            StatusWarningCode::WorktreeBudgetExceeded
            | StatusWarningCode::WorktreeReadFailed
            | StatusWarningCode::WorktreePermissionDenied
            | StatusWarningCode::WorktreeIoTimeout => StatusWarningSource::Worktree,
            StatusWarningCode::DirtyCacheLockStolen
            | StatusWarningCode::DirtyCacheStaleFallback
            | StatusWarningCode::DirtyCacheConcurrentInvalidate
            | StatusWarningCode::DirtyCachePathUnencodable => StatusWarningSource::Cache,
            StatusWarningCode::RepositoryPreflight => StatusWarningSource::Config,
        }
    }
}

declare_status_warning_enum! {
    /// Which subsystem produced a warning (§B.5).
    #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
    #[serde(rename_all = "snake_case")]
    #[non_exhaustive]
    pub enum StatusWarningSource {
        /// Repository-level advisories not tied to a scan — currently
        /// `repository_preflight`. Config RESOLUTION itself never warns: an
        /// invalid value fails closed instead of degrading.
        Config,
        /// The bounded rename-destination worktree probe (§B.3.2). Distinct
        /// from `RenameDetect` on purpose: a truncated probe means "we may
        /// not have SEEN every candidate", while a `RenameDetect` warning
        /// means "we saw them but could not score them". Consumers act
        /// differently on the two (re-run narrower vs. accept the pairing).
        Probe,
        RenameDetect,
        Cache,
        /// Repository-object reads (§B.3.4 metadata side).
        Metadata,
        /// Worktree reads (§B.3.3/§B.3.4 worktree side).
        Worktree,
    }
}

/// Resolve the Git-compatible `status.*` config defaults (plan-20260708
/// P1-05d): `status.showUntrackedFiles`, `status.short`, `status.branch`,
/// `status.showStash`, `status.relativePaths`, plus the rename keys
/// (`status.renames`→`diff.renames` and the `status.renameUntracked`
/// extension, §B.3.1), each read through the strict
/// local → global → system cascade. Every key is validated
/// UP FRONT — an invalid value is a usage error and an unreadable
/// local/global scope an IO error, both before any status output — and then
/// applied only where Git applies them: CLI flags always win;
/// `status.short` yields to an explicit `--long`/`--porcelain`;
/// `status.branch` affects only the short format (porcelain stays
/// config-immune, matching Git's stable-script contract).
async fn apply_status_config_defaults(args: &mut StatusArgs) -> CliResult<StatusConfigExtras> {
    use crate::internal::config::{
        LocalIdentityTarget, parse_git_config_bool, read_cascaded_config_value_strict,
    };

    async fn read_value(key: &str) -> CliResult<Option<String>> {
        read_cascaded_config_value_strict(LocalIdentityTarget::CurrentRepo, key)
            .await
            .map_err(|error| {
                CliError::fatal(format!("failed to read config '{key}': {error:#}"))
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })
    }
    fn invalid(key: &str, value: &str, expected: &str) -> CliError {
        CliError::command_usage(format!(
            "bad config value '{value}' for '{key}' (expected {expected})"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint(format!(
            "fix the offending value with 'libra config {key} <value>'"
        ))
    }
    async fn read_bool(key: &str) -> CliResult<Option<bool>> {
        match read_value(key).await? {
            Some(value) => match parse_git_config_bool(&value) {
                Some(enabled) => Ok(Some(enabled)),
                None => Err(invalid(key, &value, "a Git boolean")),
            },
            None => Ok(None),
        }
    }

    // Validate every key up front so a bad value fails closed even when the
    // requested format would not consult it.
    let untracked = match read_value("status.showUntrackedFiles").await? {
        Some(value) => Some(match value.trim().to_ascii_lowercase().as_str() {
            "no" => UntrackedFiles::No,
            "normal" => UntrackedFiles::Normal,
            "all" => UntrackedFiles::All,
            _ => {
                return Err(invalid(
                    "status.showUntrackedFiles",
                    &value,
                    "no, normal, or all",
                ));
            }
        }),
        None => None,
    };
    let short = read_bool("status.short").await?;
    let branch = read_bool("status.branch").await?;
    let show_stash = read_bool("status.showStash").await?;
    let relative_paths = read_bool("status.relativePaths").await?;

    // Rename detection default (§B.5): `status.renames`, falling back to
    // `diff.renames`. Accepts a Git boolean or `copy`/`copies`; `copy` is
    // fail-closed in R0 (copy detection is not supported yet) rather than
    // silently degrading to rename detection.
    async fn read_renames(key: &str) -> CliResult<Option<bool>> {
        match read_value(key).await? {
            None => Ok(None),
            Some(value) => {
                let lower = value.trim().to_ascii_lowercase();
                if lower == "copy" || lower == "copies" {
                    return Err(CliError::command_usage(format!(
                        "copy detection is not supported for '{key}'; use true or false"
                    ))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("set the value to true or false"));
                }
                match parse_git_config_bool(&value) {
                    Some(enabled) => Ok(Some(enabled)),
                    None => Err(invalid(key, &value, "a Git boolean or copy/copies")),
                }
            }
        }
    }
    let renames_config = match read_renames("status.renames").await? {
        Some(value) => Some(value),
        None => read_renames("diff.renames").await?,
    };

    // Libra extension (§B.3.1): untracked paths become unstaged rename
    // destinations only under `status.renameUntracked=true`. Strict Git
    // boolean; invalid values fail closed before any output.
    let rename_untracked = read_bool("status.renameUntracked").await?.unwrap_or(false);

    // §B.5: `status.renameLimit` caps each inexact side, falling back to
    // `diff.renameLimit`; non-negative Git integer, `0` disables the cap,
    // default 1000 (Git/diff parity). Invalid values fail closed before any
    // output.
    async fn read_rename_limit(key: &str) -> CliResult<Option<usize>> {
        match read_value(key).await? {
            None => Ok(None),
            Some(value) => {
                crate::internal::config::parse_git_config_int(&value.trim().to_ascii_lowercase())
                    .filter(|number| *number >= 0)
                    .and_then(|number| usize::try_from(number).ok())
                    .map(Some)
                    .ok_or_else(|| invalid(key, &value, "a non-negative integer"))
            }
        }
    }
    let rename_limit = match read_rename_limit("status.renameLimit").await? {
        Some(value) => value,
        None => read_rename_limit("diff.renameLimit").await?.unwrap_or(1000),
    };

    // §B.6.6: `core.quotePath` — strict Git boolean, default true (escape
    // non-ASCII bytes in human-short/non-`-z` porcelain paths). Invalid
    // values fail closed before any output.
    let quote_path = read_bool("core.quotePath").await?.unwrap_or(true);

    if args.untracked_files.is_none() {
        args.untracked_files = untracked;
    }
    if !args.short && !args.long_format && args.porcelain.is_none() && short == Some(true) {
        args.short = true;
    }
    // Git scopes the status.branch default to the short format; porcelain
    // headers still require an explicit `-b`/`--branch`.
    if args.short && !args.branch && !args.no_branch && branch == Some(true) {
        args.branch = true;
    }
    if !args.show_stash && !args.no_show_stash && show_stash == Some(true) {
        args.show_stash = true;
    }

    // Bare `-z`/`--null` with no explicit format (§B.6, R0-4): Git treats it
    // as machine intent — force porcelain v1 + NUL instead of NUL-ing the
    // human format. Config-selected short (status.short) counts as a format.
    if args.null_terminated && args.porcelain.is_none() && !args.short && !args.long_format {
        args.porcelain = Some(PorcelainVersion::V1);
    }
    args.relative_paths = relative_paths.unwrap_or(true);
    args.renames_config = renames_config;
    let rename_threshold = resolve_status_threshold(args, None)?;
    Ok(StatusConfigExtras {
        rename_untracked,
        rename_threshold,
        rename_limit,
        quote_path,
    })
}

/// Resolve and validate all `status.*` defaults without collecting repository
/// state or producing output. Embedded consumers use this before side effects,
/// then pass the returned arguments to [`execute_to_resolved`].
pub(crate) async fn resolve_config_defaults(mut args: StatusArgs) -> CliResult<ResolvedStatusArgs> {
    let extras = apply_status_config_defaults(&mut args).await?;
    Ok(ResolvedStatusArgs { args, extras })
}

/// Resolved status arguments bundled with the module-private extras,
/// produced by [`resolve_config_defaults`] exactly once and consumed by
/// [`execute_to_resolved`]. Opaque outside this module, so the single-read
/// contract cannot be bypassed and the public `StatusArgs` stays unchanged.
pub(crate) struct ResolvedStatusArgs {
    args: StatusArgs,
    extras: StatusConfigExtras,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
pub enum PorcelainVersion {
    #[clap(name = "v1")]
    V1,
    #[clap(name = "v2")]
    V2,
}

#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum, Default)]
pub enum UntrackedFiles {
    /// Show untracked files (default): only list untracked directories, not their contents.
    #[default]
    Normal,
    /// Show all untracked files, recursively listing files within untracked directories.
    All,
    /// Do not show untracked files
    No,
}

// ---------------------------------------------------------------------------
// Changes
// ---------------------------------------------------------------------------

/// path: to workdir
#[derive(Debug, Default, Clone)]
pub struct Changes {
    pub new: Vec<PathBuf>,
    pub modified: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    /// Detected renames: (source_path, target_path) pairs.
    pub renamed: Vec<(PathBuf, PathBuf)>,
}

impl Changes {
    pub fn is_empty(&self) -> bool {
        self.new.is_empty()
            && self.modified.is_empty()
            && self.deleted.is_empty()
            && self.renamed.is_empty()
    }

    /// to relative path(to cur_dir)
    pub fn to_relative(&self) -> Changes {
        let mut change = self.clone();
        [&mut change.new, &mut change.modified, &mut change.deleted]
            .into_iter()
            .for_each(|paths| {
                *paths = paths.iter().map(util::workdir_to_current).collect();
            });
        change.renamed = change
            .renamed
            .into_iter()
            .map(|(old, new)| {
                (
                    util::workdir_to_current(&old),
                    util::workdir_to_current(&new),
                )
            })
            .collect();
        change
    }
    pub fn polymerization(&self) -> Vec<PathBuf> {
        let mut poly = self.new.clone();
        poly.extend(self.modified.clone());
        poly.extend(self.deleted.clone());
        poly.extend(self.renamed.iter().map(|(_, new)| new.clone()));
        poly
    }

    pub fn extend(&mut self, other: Changes) {
        self.new.extend(other.new);
        self.modified.extend(other.modified);
        self.deleted.extend(other.deleted);
        self.renamed.extend(other.renamed);
    }
}

// ---------------------------------------------------------------------------
// StatusError + CliError mapping
// ---------------------------------------------------------------------------

#[derive(thiserror::Error, Debug)]
pub enum StatusError {
    #[error("failed to open index '{path}': {source}")]
    IndexLoad { path: PathBuf, source: GitError },
    #[error("path '{path}' is not valid UTF-8")]
    InvalidPathEncoding { path: PathBuf },
    #[error("failed to hash '{path}': {source}")]
    FileHash { path: PathBuf, source: io::Error },
    #[error("cannot read tracked path '{path}': {source}")]
    WorktreeRead { path: PathBuf, source: io::Error },
    #[error("failed to list files in '{path}': {source}")]
    ListWorkdirFiles { path: PathBuf, source: io::Error },
    #[error("failed to determine working directory: {source}")]
    Workdir { source: io::Error },
    #[error("{source}")]
    ConfigRead { source: anyhow::Error },
    #[error("cannot read the HEAD {what} '{oid}': the object is missing or corrupt")]
    HeadObjectUnreadable { what: &'static str, oid: String },
}

impl From<StatusError> for CliError {
    fn from(error: StatusError) -> Self {
        let msg = format!("failed to determine working tree status: {error}");
        match &error {
            StatusError::IndexLoad { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the index file may be corrupted"),
            StatusError::InvalidPathEncoding { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("path contains non-UTF-8 characters"),
            StatusError::FileHash { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::IoReadFailed)
            }
            StatusError::WorktreeRead { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint(
                    "a tracked file could not be read (e.g. permission denied); \
                     status fails closed rather than reporting it as deleted",
                ),
            StatusError::ListWorkdirFiles { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::IoReadFailed)
            }
            StatusError::Workdir { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::RepoNotFound)
            }
            StatusError::ConfigRead { .. } => {
                CliError::fatal(msg).with_stable_code(StableErrorCode::IoReadFailed)
            }
            StatusError::HeadObjectUnreadable { .. } => CliError::fatal(msg)
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("run 'libra fsck' or restore the object, then retry"),
        }
    }
}

// ---------------------------------------------------------------------------
// UpstreamInfo
// ---------------------------------------------------------------------------

/// Upstream tracking information for the current branch.
#[derive(Debug, Clone, Serialize)]
pub struct UpstreamInfo {
    /// Tracking ref display name, e.g. "origin/main"
    pub remote_ref: String,
    /// Commits ahead of upstream (None when gone)
    pub ahead: Option<usize>,
    /// Commits behind upstream (None when gone)
    pub behind: Option<usize>,
    /// True when upstream is configured but tracking ref no longer exists
    pub gone: bool,
}

/// In-progress merge metadata surfaced by `status` for recovery guidance.
#[derive(Debug, Clone, Serialize)]
pub struct MergeStatusInfo {
    pub target_ref: String,
    pub conflicted_paths: Vec<String>,
    pub unresolved_count: usize,
}

// ---------------------------------------------------------------------------
// StatusData — shared data layer
// ---------------------------------------------------------------------------

/// Pre-computed status data shared across all renderers (human/JSON/short/porcelain).
#[derive(Clone)]
struct StatusData {
    head: Head,
    head_oid: Option<ObjectHash>,
    has_commits: bool,
    staged: Changes,
    unstaged: Changes,
    unmerged: Vec<UnmergedEntry>,
    ignored_files: Vec<PathBuf>,
    stash_count: Option<usize>,
    upstream: Option<UpstreamInfo>,
    merge_state: Option<MergeStatusInfo>,
    /// A non-merge sequence in progress (cherry-pick/revert/rebase), surfaced
    /// as a one-line human advisory (lore.md 2.6). Merge has its own richer
    /// rendering; porcelain/JSON are unchanged.
    sequence_notice: Option<String>,
    /// lore.md 2.2: a read-only sparse view is ACTIVELY filtering (enabled AND
    /// non-empty AND compiled — matches SparseView::is_active). status itself
    /// is NEVER filtered (it must stay honest about what commit will record);
    /// this is only an advisory that ls-files/diff are scoped. An
    /// enabled-but-empty view is a no-op, so no advisory.
    sparse_view_active: bool,
    porcelain_v2: Option<std::sync::Arc<PorcelainV2Data>>,
    /// Score/exactness per staged rename pair (display-base keys, §B.6.4/5).
    staged_rename_details: RenameDetails,
    /// Score/exactness per unstaged rename pair (only populated under
    /// `status.renameUntracked=true`).
    unstaged_rename_details: RenameDetails,
    /// Structured degradation warnings collected during data assembly
    /// (§B.5): rename-engine budget/limit downgrades. Rendered per the
    /// delivery matrix by the callers.
    warnings: Vec<StatusWarning>,
    /// `core.quotePath` (§B.6.6): escape non-ASCII bytes in human-short and
    /// non-`-z` porcelain paths (default true, Git parity).
    quote_path: bool,
    /// §B.3.3/§B.6.0.1: paths the base scan or the rename probe could not
    /// inspect (workdir-relative, sorted, deduplicated). Text formats fail
    /// closed on any entry; JSON reports the partial result plus
    /// `data.io_blocked[]` with `is_clean = false`.
    io_blocked: Vec<crate::command::status_probe::IoBlockedEvent>,
    /// Whether the RENAME side (probe or candidate reads) was blocked, as
    /// opposed to the base scan. `rename_detection_complete` keys off this
    /// so a base-scan-only block does not also claim the rename pairing
    /// degraded.
    rename_scan_blocked: bool,
    /// Whether the BASE scan (tracked dirty + untracked enumeration) hit an
    /// I/O block — `data.base_scan_complete` is its negation; probe blocks
    /// only affect `rename_detection_complete`.
    base_scan_blocked: bool,
}

/// Human advisory for a non-merge sequence in progress (read-only detection).
async fn sequence_notice() -> CliResult<Option<String>> {
    use crate::internal::sequencer::{self, ActiveSequenceKind, SequenceKind};
    let active = sequencer::detect_active_operation()
        .await
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to inspect in-progress operation state: {error}"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("repair the repository sequencer state, then retry 'libra status'")
        })?;
    Ok(match active {
        Some(ActiveSequenceKind::Am) => Some(
            "You are in the middle of an am operation; use 'libra am --continue', '--skip', or '--abort'."
                .to_string(),
        ),
        Some(ActiveSequenceKind::Known(SequenceKind::CherryPick)) => Some(
            "cherry-pick in progress; run 'libra cherry-pick --continue' or '--abort'".to_string(),
        ),
        Some(ActiveSequenceKind::Known(SequenceKind::Revert)) => {
            Some("revert in progress; run 'libra revert --continue' or '--abort'".to_string())
        }
        Some(ActiveSequenceKind::Known(SequenceKind::Rebase)) => {
            Some("rebase in progress; run 'libra rebase --continue' or '--abort'".to_string())
        }
        // An ambiguous legacy directory is REPORTED, not fatal: this worktree
        // has no sequencer state, so status is exactly the command that should
        // still work and tell the user what is there. (A sequence-START path
        // refuses it — see `ensure_none_for`.)
        // `bisect` has its own dedicated status rendering elsewhere; the
        // sequence advisory does not duplicate it.
        Some(ActiveSequenceKind::Bisect) => None,
        Some(ActiveSequenceKind::AmbiguousLegacy(state)) => {
            // Medium-accurate guidance: a table is not a file, and telling a
            // user to delete a directory that does not exist is advice they
            // cannot act on.
            let (what, how) = state.describe();
            Some(format!(
                "{what}, and this repository has linked-worktree history. {how}."
            ))
        }
        // Merge has its own dedicated rendering below.
        Some(ActiveSequenceKind::Known(SequenceKind::Merge)) | None => None,
    })
}

impl StatusData {
    fn is_dirty(&self) -> bool {
        !self.staged.is_empty()
            || !self.unstaged.is_empty()
            || self.merge_state.is_some()
            || !self.unmerged.is_empty()
            // §B.6.0.1: "cannot inspect" must never report clean.
            || !self.io_blocked.is_empty()
    }
}

/// Collect all status data in one pass, eliminating duplicate computation
/// between human/JSON/short/porcelain renderers.
async fn collect_status_data(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    warning_ctx: &InvocationWarningCtx,
) -> CliResult<StatusData> {
    // lore.md 2.4: layer-overlay paths are excluded from status like ignored
    // files (a no-op with no layers). W1 §C.4.1.1: refreshed with this
    // request's resolved worktree scope.
    crate::internal::layer::refresh_exclusion_snapshot(
        &crate::internal::worktree_scope::WorktreeScope::for_request(),
    )
    .await;
    if is_bare_repository().await? {
        return Err(CliError::fatal("this operation must be run in a work tree")
            .with_stable_code(StableErrorCode::RepoStateInvalid)
            .with_hint("this command requires a working tree; bare repositories do not have one"));
    }
    let _status_io_root = crate::command::status_io_worker::begin_status_io_root_session()
        .map_err(|source| {
            CliError::fatal(format!("cannot resolve worktree for status I/O: {source}"))
        })?;
    let ignore_case = effective_ignore_case_for_status().await?;

    let head = Head::current_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD", error))?;
    let head_oid = Head::current_commit_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD commit", error))?;
    let has_commits = head_oid.is_some();

    let mut staged = changes_to_be_committed_safe()
        .await
        .map(|c| c.to_relative())
        .map_err(CliError::from)?;
    let worktree = status_untracked::collect_status_worktree_changes(
        args.untracked_files.unwrap_or(UntrackedFiles::Normal),
        args.ignored,
        ignore_case,
    )
    .map_err(CliError::from)?;
    let mut unstaged = status_untracked::changes_to_current_directory(worktree.unstaged);
    let unmerged = unmerged::collect(&worktree.index)
        .into_iter()
        .map(|entry| {
            let current_path = util::workdir_to_current(&entry.path);
            entry.with_path(current_path)
        })
        .collect::<Vec<_>>();
    let unmerged_paths = unmerged
        .iter()
        .map(|entry| entry.path.clone())
        .collect::<HashSet<_>>();
    unstaged.new.retain(|path| !unmerged_paths.contains(path));
    let ignored_files = worktree
        .ignored_files
        .into_iter()
        .map(|path| {
            // The marker-preserving projection (directory markers are
            // built on the raw name; see `with_dir_marker`).
            let projected = util::workdir_to_current(&path);
            with_dir_marker(&path, projected)
        })
        .collect();
    let mut io_blocked = worktree.io_blocked;
    let base_scan_blocked = !io_blocked.is_empty();
    // Tracked separately from `base_scan_blocked`: a block during the
    // BASE scan does not mean rename detection degraded, and reporting
    // both flags false for one base-scan EACCES tells consumers the
    // rename pairing is unreliable when it was never attempted.
    let mut rename_scan_blocked = false;
    // One accumulator for BOTH detection sides: §B.5 requires exactly one
    // warning per {code, source} for the whole run, so the per-side stats
    // are folded together and rendered once (see `merge_rename_stats`).
    let mut rename_stats = rename_detect::RenameDetectStats::default();
    let mut rename_budgets = RenameBudgets::new();
    let mut maybe_index = Some(worktree.index);

    // Resolve rename detection (§B.5). Precedence: CLI flags always win —
    // `--no-renames` disables, `--find-renames[=N]`/`--renames` enable at the
    // given (or default 50%) threshold. Otherwise the resolved
    // `status.renames`/`diff.renames` config applies (`false` disables). When
    // nothing is set, rename detection is ON at 50%, matching Git.
    // `--cached`/`--check-dirty` (Libra dirty-cache extensions) never run it.
    let rename_threshold: Option<u32> = if args.cached || args.check_dirty {
        None
    } else {
        extras.rename_threshold
    };

    // Apply rename detection before collapsing untracked dirs / porcelain
    // metadata. Staged snapshot: old = HEAD tree, new = index stage-0.
    // Unstaged snapshot: old = index stage-0, new = worktree — but untracked
    // paths only become destinations under the `status.renameUntracked`
    // extension (§B.3.1; Git default: a tracked→untracked move is `D` + `??`).
    let mut staged_rename_details: RenameDetails = HashMap::new();
    let mut unstaged_rename_details: RenameDetails = HashMap::new();
    let mut warnings: Vec<StatusWarning> = Vec::new();
    if let Some(threshold) = rename_threshold {
        let head_blobs = head_oid
            .as_ref()
            .map(load_head_tree_blobs)
            .transpose()?
            .unwrap_or_default();
        let index_blobs = maybe_index
            .as_ref()
            .map(load_index_stage0_blobs)
            .unwrap_or_default();
        // Git 0..=60000 similarity scale (already engine-scale here).
        let config = rename_detect::RenameDetectConfig {
            threshold,
            rename_limit: extras.rename_limit,
            comparison_budget: Some(status_comparison_budget()),
        };
        // An unresolved conflict is NOT a staged deletion for rename
        // pairing: the stage-0-less index classifies it as deleted, and a
        // same-content staged addition could otherwise consume it as a
        // rename SOURCE — emitting a `2` record for a path whose only
        // truthful spelling is the unmerged `u` row. Pull conflicts out
        // for the detection pass only and restore them after, leaving
        // every format's classification of the conflict itself untouched
        // (2026-08-06 R0-5 review).
        let conflicted_staged_deletes: Vec<PathBuf> = staged
            .deleted
            .iter()
            .filter(|path| unmerged_paths.contains(*path))
            .cloned()
            .collect();
        if !conflicted_staged_deletes.is_empty() {
            staged.deleted.retain(|path| !unmerged_paths.contains(path));
        }
        detect_renames_in_changes(
            &mut staged,
            &config,
            RenameBlobSide::Known(&head_blobs),
            RenameBlobSide::Known(&index_blobs),
            &mut staged_rename_details,
            &mut rename_stats,
            &mut rename_budgets,
        );
        if !conflicted_staged_deletes.is_empty() {
            staged.deleted.extend(conflicted_staged_deletes);
            staged.deleted.sort();
        }
        // §B.3.1 Git default: unstaged "new" entries are untracked paths,
        // which may only be consumed as rename destinations under the
        // `status.renameUntracked` extension. Skipping detection keeps a
        // tracked→untracked move rendered as `D` + `??`.
        //
        // R0-3: under the extension, DESTINATIONS come from the bounded
        // probe (§B.3.1.1–§B.3.2) — decoupled from the untracked display
        // scan (`-uno` hides markers, never the probe) and qualified by the
        // same tracked/ignore layering. The probe only runs when there is a
        // deleted side to pair against.
        if extras.rename_untracked
            && !unstaged.deleted.is_empty()
            && let Some(index_ref) = maybe_index.as_ref()
        {
            let workdir = util::working_dir();
            let compiled_pathspecs = if args.pathspec.is_empty() {
                None
            } else {
                Some(
                    PathspecSet::from_workdir(&args.pathspec, &util::cur_dir(), &workdir)
                        .map_err(pathspec_error_to_cli)?,
                )
            };
            let tracked_paths = crate::command::status_untracked_paths::TrackedPaths::from_index(
                index_ref,
                ignore_case,
            );
            let filter = crate::command::status_probe::DestinationFilter {
                workdir: &workdir,
                index: index_ref,
                tracked: &tracked_paths,
                pathspecs: compiled_pathspecs.as_ref(),
            };
            let roots =
                crate::command::status_probe::pathspec_probe_roots(compiled_pathspecs.as_ref());
            let outcome = crate::command::status_probe::probe_rename_destinations(
                &roots,
                &filter,
                crate::command::status_probe::ProbeLimits::effective(),
            );
            // §B.3.2 merge rules (R0-8): blocked probe paths accumulate into
            // `data.io_blocked` — text formats fail closed at render time,
            // JSON keeps pairing and reports the partial contract.
            rename_scan_blocked |= !outcome.io_blocked.is_empty();
            io_blocked.extend(outcome.io_blocked.iter().cloned());
            if let Some(kind) = outcome.truncated {
                warnings.push(StatusWarning {
                    code: StatusWarningCode::ProbeTruncated,
                    message: format!(
                        "rename-destination probe truncated: {} budget exhausted; rename detection may be incomplete",
                        match kind {
                            crate::command::status_probe::ProbeBudgetKind::Enumeration => "enumeration",
                            crate::command::status_probe::ProbeBudgetKind::Destination => "destination",
                        }
                    ),
                    source: StatusWarningCode::ProbeTruncated.source(),
                });
            }
            if outcome.encoding_skipped > 0 {
                // §B.6.1 / DEFER-02: non-UTF-8 names keep their base `??`
                // rows but sit out rename scoring until R0.5 — one
                // deduplicated warning covers every skipped candidate.
                warnings.push(StatusWarning {
                    code: StatusWarningCode::RenamePathEncodingUnsupported,
                    message: format!(
                        "rename detection skipped {} candidate(s) with non-UTF-8 names; their untracked/base status is unaffected",
                        outcome.encoding_skipped
                    ),
                    source: StatusWarningCode::RenamePathEncodingUnsupported.source(),
                });
            }
            // Detection runs on the probe's destination set (display base);
            // consumed destinations then collapse their display rows and
            // `? dir/` markers (§B.3.5).
            let destinations_display: Vec<PathBuf> = outcome
                .destinations
                .iter()
                .map(util::workdir_to_current)
                .collect();
            let consumed = detect_renames_with_destinations(
                &mut unstaged,
                &config,
                RenameBlobSide::Known(&index_blobs),
                &destinations_display,
                &mut unstaged_rename_details,
                &mut rename_stats,
                &mut rename_budgets,
            );
            let complete_roots: Vec<PathBuf> = outcome
                .complete_roots
                .iter()
                .map(|root| {
                    if root.as_os_str().is_empty() {
                        // "" = the whole worktree; keep it empty so the
                        // collapse treats every marker as governed.
                        PathBuf::new()
                    } else {
                        util::workdir_to_current(root)
                    }
                })
                .collect();
            crate::command::status_probe::collapse_untracked_markers(
                &mut unstaged.new,
                &destinations_display,
                &consumed,
                &complete_roots,
            );
        }
    }

    let stash_count = if args.show_stash {
        Some(stash::get_stash_num().map_err(|detail| {
            CliError::fatal(format!("failed to read stash state for status: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("repair or remove the corrupt stash log, then retry")
        })?)
    } else {
        None
    };

    // Resolve upstream tracking info
    let upstream = resolve_upstream_info(&head, head_oid.as_ref()).await?;
    let merge_state = match merge::MergeState::load_optional_sync().map_err(|detail| {
        CliError::fatal(format!("failed to inspect merge state: {detail}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        Some(state) => {
            if maybe_index.is_none() {
                maybe_index = Some(load_status_index()?);
            }
            let index = maybe_index
                .as_ref()
                .ok_or_else(|| CliError::internal("status index should be loaded"))?;
            let conflicted_paths =
                merge::unresolved_conflicted_paths(index, &state.conflicted_paths);
            Some(MergeStatusInfo {
                target_ref: state.target_ref,
                unresolved_count: conflicted_paths.len(),
                conflicted_paths,
            })
        }
        None => None,
    };
    let porcelain_v2 = if matches!(args.porcelain, Some(PorcelainVersion::V2)) {
        let index = maybe_index
            .take()
            .ok_or_else(|| CliError::internal("porcelain v2 metadata should be loaded"))?;
        Some(std::sync::Arc::new(build_porcelain_v2_data(
            index,
            head_oid.as_ref(),
        )?))
    } else {
        None
    };

    // The engine's own warnings are the rename-side degradation signal. The
    // worktree/metadata CODES are shared with the io_blocked mapping (a
    // base-scan EACCES emits `worktree_permission_denied` too), so the flag
    // is captured HERE — before the io_blocked-derived warnings are
    // synthesized into the same list — rather than inferred from codes.
    warnings_from_rename_stats(&rename_stats, &mut warnings);
    // Preflight advisories join the structured list before anything reads
    // it, so exit arbitration and the JSON payload see the same set.
    for message in warning_ctx.preflight_messages() {
        warnings.push(StatusWarning {
            code: StatusWarningCode::RepositoryPreflight,
            message: message.clone(),
            source: StatusWarningCode::RepositoryPreflight.source(),
        });
    }
    rename_scan_blocked |= warnings.iter().any(|warning| {
        matches!(
            warning.source,
            StatusWarningSource::Worktree | StatusWarningSource::Metadata
        )
    });
    let mut data = StatusData {
        head,
        head_oid,
        has_commits,
        staged,
        unstaged,
        unmerged,
        ignored_files,
        stash_count,
        upstream,
        merge_state,
        sequence_notice: sequence_notice().await?,
        sparse_view_active: crate::internal::sparse::SparseView::load(
            &crate::internal::worktree_scope::WorktreeScope::for_request(),
        )
        .await
        .is_active(),
        porcelain_v2,
        staged_rename_details,
        unstaged_rename_details,
        warnings: {
            // §B.5/§B.6.0.1: every blocked path contributes its
            // worktree-family warning HERE (not at JSON-render time), so
            // exit arbitration (`--exit-code-on-warning` → 9) and the
            // stderr delivery of text formats see exactly the same set.
            // The list is then deduplicated on the FULL {code, source,
            // message} triple, not just {code, source}: two detection passes
            // (staged + unstaged) must not double-report the same
            // degradation, but two different blocked paths carry different
            // messages and must each keep their own warning — the JSON
            // contract is one warning per `io_blocked[]` entry.
            let mut warnings = warnings;
            let mut blocked_sorted = io_blocked.clone();
            blocked_sorted.sort_by_key(|event| raw_path_sort_key(&event.path));
            blocked_sorted.dedup_by(|a, b| a.path == b.path);
            for event in &blocked_sorted {
                let (reason, code) = io_blocked_reason_and_code(event.reason);
                warnings.push(StatusWarning {
                    code,
                    message: format!(
                        "cannot inspect '{}': {reason}",
                        quote_pathname(&event.path, extras.quote_path)
                    ),
                    source: code.source(),
                });
            }
            let mut seen: HashSet<(StatusWarningCode, StatusWarningSource, String)> =
                HashSet::new();
            warnings.retain(|warning| {
                seen.insert((warning.code, warning.source, warning.message.clone()))
            });
            warnings
        },
        quote_path: extras.quote_path,
        io_blocked: {
            let mut events = io_blocked;
            events.sort_by_key(|event| raw_path_sort_key(&event.path));
            collapse_io_blocked_by_path(&mut events);
            events
        },
        base_scan_blocked,
        rename_scan_blocked,
    };
    filter_status_data_by_pathspec(&mut data, args)?;
    Ok(data)
}

/// Reattach a collapsed-directory trailing `/` marker that path projection
/// (`to_workdir_path`/`workdir_to_current`/`current_to_workdir`) normalizes
/// away — built from raw `OsString` bytes so a non-UTF-8 directory name
/// survives intact (never `display()`).
fn with_dir_marker(path: &Path, projected: PathBuf) -> PathBuf {
    if path.as_os_str().as_encoded_bytes().ends_with(b"/")
        && !projected.as_os_str().as_encoded_bytes().ends_with(b"/")
    {
        let mut marker = projected.into_os_string();
        marker.push("/");
        PathBuf::from(marker)
    } else {
        projected
    }
}

impl StatusData {
    /// A copy whose change lists use repository-root-relative paths (the
    /// machine-format base, §B.6.4). Cheap enough for one render and
    /// keeps the human path base untouched.
    fn to_repo_relative(&self) -> StatusData {
        /// Collapsed untracked/ignored directories carry a deliberate
        /// trailing `/` marker; `current_to_workdir` normalizes through
        /// path components and would eat it, making `?? dir/` render as
        /// `?? dir` — indistinguishable from an untracked FILE named `dir`.
        fn current_to_workdir_keeping_marker(path: &Path) -> PathBuf {
            with_dir_marker(path, current_to_workdir(path))
        }
        fn project(paths: &[PathBuf]) -> Vec<PathBuf> {
            paths
                .iter()
                .map(|path| current_to_workdir_keeping_marker(path))
                .collect()
        }
        fn project_changes(changes: &Changes) -> Changes {
            Changes {
                new: project(&changes.new),
                modified: project(&changes.modified),
                deleted: project(&changes.deleted),
                renamed: changes
                    .renamed
                    .iter()
                    .map(|(old, new)| {
                        (
                            current_to_workdir_keeping_marker(old),
                            current_to_workdir_keeping_marker(new),
                        )
                    })
                    .collect(),
            }
        }
        fn project_details(details: &RenameDetails) -> RenameDetails {
            details
                .iter()
                .map(|((old, new), value)| {
                    ((current_to_workdir(old), current_to_workdir(new)), *value)
                })
                .collect()
        }

        let mut projected = self.clone();
        projected.staged = project_changes(&self.staged);
        projected.unstaged = project_changes(&self.unstaged);
        projected.ignored_files = project(&self.ignored_files);
        projected.staged_rename_details = project_details(&self.staged_rename_details);
        projected.unstaged_rename_details = project_details(&self.unstaged_rename_details);
        for entry in &mut projected.unmerged {
            entry.path = current_to_workdir(&entry.path);
        }
        projected
    }
}

fn filter_status_data_by_pathspec(data: &mut StatusData, args: &StatusArgs) -> CliResult<()> {
    if args.pathspec.is_empty() {
        return Ok(());
    }
    let pathspecs =
        PathspecSet::from_workdir(&args.pathspec, &util::cur_dir(), &util::working_dir())
            .map_err(pathspec_error_to_cli)?;

    filter_changes_by_pathspec(&mut data.staged, &pathspecs);
    filter_changes_by_pathspec(&mut data.unstaged, &pathspecs);
    // §B.3.2: a blocked path OUTSIDE the requested pathspec is not this
    // run's problem — it must neither fail a narrowed status closed nor
    // leak through `io_blocked[]`. The base-scan walk is pathspec-blind,
    // so the narrowing happens here, and the derived warnings follow.
    let blocked_before = data.io_blocked.len();
    // Keep an event when the path matches the spec OR could CONTAIN a match:
    // `:(glob)wanted/*.txt` never matches the directory `wanted`, yet a block
    // on that directory is exactly what hides the files the caller asked
    // for. The probe roots are already derived from the spec set, so a path
    // at, under, or above a root is in scope.
    let roots = crate::command::status_probe::pathspec_probe_roots(Some(&pathspecs));
    data.io_blocked.retain(|event| {
        if pathspecs.matches_path(&event.path) {
            return true;
        }
        let path = current_to_workdir(&event.path);
        roots.iter().any(|root| {
            root.as_os_str().is_empty() || path.starts_with(root) || root.starts_with(&path)
        })
    });
    if data.io_blocked.len() != blocked_before {
        // Only the warnings DERIVED from `io_blocked[]` are rebuilt. The
        // worktree family also carries aggregate rename-scoring warnings
        // (`worktree_read_failed` / `worktree_io_timeout` from
        // `warnings_from_rename_stats`) that name no path and are not tied
        // to any event — dropping those by source would hide a real
        // degradation, flip `rename_detection_complete` back to true, and
        // silently downgrade `--exit-code-on-warning` from 9.
        data.warnings
            .retain(|warning| !warning.message.starts_with("cannot inspect '"));
        let mut seen: HashSet<PathBuf> = HashSet::new();
        for event in &data.io_blocked {
            if !seen.insert(event.path.clone()) {
                continue;
            }
            let (reason, code) = io_blocked_reason_and_code(event.reason);
            data.warnings.push(StatusWarning {
                code,
                message: format!(
                    "cannot inspect '{}': {reason}",
                    quote_pathname(&event.path, data.quote_path)
                ),
                source: code.source(),
            });
        }
        if data.io_blocked.is_empty() {
            data.base_scan_blocked = false;
            // The rename-side flag survives unless BOTH sources of it are
            // gone: the blocked events (now empty) and the engine's own
            // aggregate warnings, which name no path and therefore are not
            // rebuilt above. Clearing it on event count alone would report
            // `rename_detection_complete = true` while an
            // "N candidate(s): worktree reads failed" warning is still in
            // the payload.
            data.rename_scan_blocked = data.warnings.iter().any(|warning| {
                matches!(
                    warning.source,
                    StatusWarningSource::Worktree | StatusWarningSource::Metadata
                ) && !warning.message.starts_with("cannot inspect '")
            });
        }
    }
    data.unmerged
        .retain(|entry| current_relative_matches(&entry.path, &pathspecs));
    data.ignored_files
        .retain(|path| current_relative_matches(path, &pathspecs));
    if let Some(merge_state) = data.merge_state.as_mut() {
        merge_state
            .conflicted_paths
            .retain(|path| pathspecs.matches_path(Path::new(path)));
    }

    Ok(())
}

fn filter_changes_by_pathspec(changes: &mut Changes, pathspecs: &PathspecSet) {
    changes
        .new
        .retain(|path| current_relative_matches(path, pathspecs));
    changes
        .modified
        .retain(|path| current_relative_matches(path, pathspecs));
    changes
        .deleted
        .retain(|path| current_relative_matches(path, pathspecs));
    // Per-end pathspec semantics (§B.3): a rename pair survives only when
    // BOTH endpoints match. An old-only match demotes to a deletion and a
    // new-only match to an addition, so an out-of-scope endpoint can never
    // leak into the output through a rename record.
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (old, new) in changes.renamed.drain(..) {
        let old_in = current_relative_matches(&old, pathspecs);
        let new_in = current_relative_matches(&new, pathspecs);
        match (old_in, new_in) {
            (true, true) => kept.push((old, new)),
            (true, false) => changes.deleted.push(old),
            (false, true) => changes.new.push(new),
            (false, false) => {}
        }
    }
    changes.deleted.sort();
    changes.new.sort();
    changes.renamed = kept;
}

fn current_relative_matches(path: &Path, pathspecs: &PathspecSet) -> bool {
    pathspecs.matches_path(util::to_workdir_path(path))
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

/// Where one side of a rename snapshot draws its blob identities from.
enum RenameBlobSide<'a> {
    /// HEAD tree or index stage-0: repo-relative path → (oid, mode), a
    /// content-addressed fact (`KnownObjectId`, §B.4.1).
    Known(&'a HashMap<PathBuf, (ObjectHash, u32)>),
    /// The worktree: OID is streamed from the file during this call
    /// (`ComputedWorktreeThisCall`).
    Worktree,
}

/// Content provider for inexact scoring: HEAD/index blobs are read from the
/// object store by OID (de-duplicated, budgeted), worktree files are read
/// under the separate worktree budget (§B.7). The engine caches spanhashes
/// per path, so each path is requested at most once per side.
struct StatusContentSource {
    old_is_worktree: bool,
    new_is_worktree: bool,
    objects: rename_detect::ObjectReadBudget,
    worktree: rename_detect::WorktreeReadBudget,
}

impl StatusContentSource {
    fn read(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
        from_worktree: bool,
    ) -> rename_detect::ContentOutcome {
        use rename_detect::{BlobEvidence, ContentOutcome, SkipReason};
        match blob.evidence {
            BlobEvidence::KnownObjectId { oid } if !from_worktree => self.objects.read_blob(&oid),
            _ if from_worktree => {
                let abs = util::workdir_to_absolute(path);
                self.worktree.read_worktree_blob(&abs)
            }
            // A worktree-computed OID on the object side, or an Unknown blob:
            // no trustworthy object to read.
            _ => ContentOutcome::Skipped(SkipReason::ObjectUnavailable),
        }
    }
}

impl rename_detect::RenameContentSource for StatusContentSource {
    fn old_content(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        let from_worktree = self.old_is_worktree;
        self.read(path, blob, from_worktree)
    }

    fn new_content(
        &mut self,
        path: &Path,
        blob: &rename_detect::BlobRef,
    ) -> rename_detect::ContentOutcome {
        let from_worktree = self.new_is_worktree;
        self.read(path, blob, from_worktree)
    }
}

/// Build one side of a [`rename_detect::RenameSnapshot`] from a change list.
///
/// `paths` are in the change list's own base (repo- or cwd-relative); each is
/// mapped to a repo-relative key via [`util::to_workdir_path`] so the HEAD/
/// index lookups and worktree reads are correct from any working directory
/// (fixing the historical subdirectory bug). The returned map is keyed by the
/// repo-relative path.
fn build_rename_side(
    paths: &[PathBuf],
    side: &RenameBlobSide<'_>,
    worktree_budget: &mut rename_detect::WorktreeReadBudget,
    snapshot_skips: &mut HashMap<rename_detect::SkipReason, u64>,
) -> HashMap<PathBuf, rename_detect::BlobRef> {
    use rename_detect::{BlobEvidence, BlobKind, BlobRef};
    // §B.4.1: the empty blob is recognizable by OID alone, so HEAD/index
    // sides can carry `size = Some(0)` (the engine's empty-file inexact
    // skip) without any object read; the constant follows the process hash
    // kind.
    let empty_blob_oid =
        git_internal::internal::object::blob::Blob::from_content_bytes(Vec::new()).id;
    let mut map = HashMap::new();
    for path in paths {
        let repo_key = util::to_workdir_path(path);
        let blob = match side {
            RenameBlobSide::Known(known) => {
                let Some((oid, mode)) = known.get(&repo_key).copied() else {
                    continue;
                };
                BlobRef {
                    kind: BlobKind::from_mode(mode),
                    mode,
                    size: (oid == empty_blob_oid).then_some(0),
                    evidence: BlobEvidence::KnownObjectId { oid },
                }
            }
            RenameBlobSide::Worktree => {
                let abs = util::workdir_to_absolute(&repo_key);
                // §B.3.3: this OPTIONAL stat runs under the deadline like
                // every other worktree read — a hung mount must cost the
                // rename candidate and a warning, not the whole command.
                // Debug-only seam for the scan→stat disappearance race: the
                // named path's stat is overridden with a genuine `NotFound`
                // error kind — the same branch an OS-level deletion drives,
                // without the hook mutating the worktree. Gated on
                // `LIBRA_TEST` like every seam in this family.
                #[cfg(debug_assertions)]
                let forced_vanish = std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
                    && std::env::var("LIBRA_TEST_VANISH_PATH")
                        .ok()
                        .filter(|target| !target.is_empty())
                        .is_some_and(|target| repo_key == std::path::Path::new(&target));
                #[cfg(not(debug_assertions))]
                let forced_vanish = false;
                let stat_target = abs.clone();
                // Bounded by the SHARED batch window, not the standalone
                // per-operation timeout: these preliminary stats are part of
                // the same §B.3.4 batch as the content reads, and giving
                // each its own 10s allowance would let snapshot construction
                // alone outlive the 5s deadline (2026-08-05 R0-1 review).
                let stat = crate::command::status_probe::with_io_deadline_bounded(
                    worktree_budget.read_window(),
                    move || stat_target.symlink_metadata(),
                )
                .unwrap_or_else(|()| Err(io::Error::new(io::ErrorKind::TimedOut, "reclaimed")));
                let stat = if forced_vanish {
                    Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "simulated vanish for the test seam",
                    ))
                } else {
                    stat
                };
                // Debug-only seam for the stat→hash race, which is far too
                // narrow to hit reliably from a test: the named path is
                // treated as having changed TYPE between the two reads.
                #[cfg(debug_assertions)]
                let forced_type_race = std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV)
                    .is_some()
                    && std::env::var("LIBRA_TEST_TYPE_RACE_PATH")
                        .ok()
                        .filter(|target| !target.is_empty())
                        .is_some_and(|target| repo_key == std::path::Path::new(&target));
                #[cfg(not(debug_assertions))]
                let forced_type_race = false;
                let stat_kind = stat.as_ref().err().map(|error| error.kind());
                let (kind, mode) = match stat {
                    Ok(meta) if meta.file_type().is_symlink() => (BlobKind::Symlink, 0o120000),
                    Ok(_) => (BlobKind::Regular, 0o100644),
                    Err(_) => {
                        // §B.3.4/§B.4.1: a candidate we cannot even stat is
                        // a DEGRADATION, not a silent non-candidate — the
                        // base status stays truthful, but the run must say
                        // rename detection was incomplete. That includes
                        // `NotFound`: the path DID exist when the scan
                        // enumerated it, so its disappearance is a race that
                        // cost a rename candidate, and reporting the run as
                        // complete would claim a pairing was ruled out when
                        // it was never attempted.
                        *snapshot_skips
                            .entry(if matches!(stat_kind, Some(io::ErrorKind::TimedOut)) {
                                rename_detect::SkipReason::IoTimeout
                            } else {
                                rename_detect::SkipReason::WorktreeIoFailed
                            })
                            .or_default() += 1;
                        continue;
                    }
                };
                // §B.3.4: worktree OID computation streams through the SAME
                // read budget that later feeds inexact content reads, so a
                // pathological candidate set cannot bypass the caps via the
                // exact stage (LFS paths hash the pointer blob, matching
                // what the index records).
                let (oid, size, observed_kind) =
                    match worktree_budget.worktree_blob_oid_and_size(&abs) {
                        Ok(triple) => triple,
                        Err(reason) => {
                            // Budget/size/I-O failures during the OPTIONAL
                            // worktree hash drop the candidate; record the
                            // reason so the same deduplicated warning family
                            // fires as for inexact content reads.
                            *snapshot_skips.entry(reason).or_default() += 1;
                            continue;
                        }
                    };
                // The kind was stat'ed above and the OID streamed just now.
                // If the path changed type in between, the OID describes
                // something the recorded kind does not — a symlink target
                // labelled `Regular` could then clear the exact gate against
                // a blob. Drop the candidate and report the race.
                if observed_kind != kind || forced_type_race {
                    *snapshot_skips
                        .entry(rename_detect::SkipReason::WorktreeIoFailed)
                        .or_default() += 1;
                    continue;
                }
                BlobRef {
                    kind,
                    mode,
                    size: Some(size),
                    evidence: BlobEvidence::ComputedWorktreeThisCall { oid },
                }
            }
        };
        map.insert(repo_key, blob);
    }
    map
}

/// The §B.7 comparison cap for `status`. Debug builds honor
/// `LIBRA_TEST_STATUS_COMPARISON_BUDGET` so a test can observe the shared
/// allowance without generating 500k real comparisons.
fn status_comparison_budget() -> u64 {
    #[cfg(debug_assertions)]
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
        && let Ok(value) = std::env::var("LIBRA_TEST_STATUS_COMPARISON_BUDGET")
        && let Ok(parsed) = value.parse::<u64>()
        && parsed > 0
    {
        // Tighten-only, like the probe-limit seams: a test may shrink the
        // budget to force exhaustion, never raise it past production's cap.
        return parsed.min(rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS);
    }
    rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS
}

/// Call-level rename budget STATE, carried between detection sides.
///
/// §B.3.4/§B.7 specify ONE 500k comparison cap, ONE 64 MiB read cap and ONE
/// 5 s scoring deadline per `status` invocation. Rebuilding any of these per
/// side let a single call spend them twice while each side individually
/// looked compliant — so the remaining amounts, the ORIGINAL deadline, and
/// the OID de-duplication cache all travel between the passes.
struct RenameBudgets {
    objects_total: u64,
    objects_slots: u32,
    worktree_total: u64,
    worktree_tasks: u32,
    /// Absolute batch deadline; a fresh one would hand the second side
    /// another full 5 s.
    deadline: std::time::Instant,
    /// Shared so an object both sides need is read once, not twice.
    object_cache: Vec<(ObjectHash, Result<Vec<u8>, rename_detect::SkipReason>)>,
    /// Comparisons already spent; the next side's config is narrowed by it.
    comparisons_spent: u64,
}

impl RenameBudgets {
    fn new() -> Self {
        let objects = rename_detect::ObjectReadBudget::with_defaults();
        let worktree = rename_detect::WorktreeReadBudget::with_defaults();
        let (objects_total, objects_slots) = objects.remaining();
        let (worktree_total, worktree_tasks) = worktree.remaining();
        Self {
            objects_total,
            objects_slots,
            worktree_total,
            worktree_tasks,
            deadline: objects.deadline(),
            object_cache: Vec::new(),
            comparisons_spent: 0,
        }
    }

    fn take_objects(&mut self) -> rename_detect::ObjectReadBudget {
        rename_detect::ObjectReadBudget::resumed(
            self.objects_total,
            self.objects_slots,
            self.deadline,
            std::mem::take(&mut self.object_cache),
        )
    }

    fn take_worktree(&self) -> rename_detect::WorktreeReadBudget {
        rename_detect::WorktreeReadBudget::resumed(
            self.worktree_total,
            self.worktree_tasks,
            self.deadline,
        )
    }

    fn restore_objects(&mut self, budget: &mut rename_detect::ObjectReadBudget) {
        let (total, slots) = budget.remaining();
        self.objects_total = total;
        self.objects_slots = slots;
        self.object_cache = budget.take_cache();
    }

    fn restore_worktree(&mut self, budget: &rename_detect::WorktreeReadBudget) {
        let (total, tasks) = budget.remaining();
        self.worktree_total = total;
        self.worktree_tasks = tasks;
    }

    /// The config for the NEXT side, with its comparison allowance reduced
    /// by what earlier sides already spent.
    fn narrowed(
        &self,
        config: &rename_detect::RenameDetectConfig,
    ) -> rename_detect::RenameDetectConfig {
        rename_detect::RenameDetectConfig {
            comparison_budget: config
                .comparison_budget
                .map(|budget| budget.saturating_sub(self.comparisons_spent)),
            ..config.clone()
        }
    }

    fn record_comparisons(&mut self, spent: u64) {
        self.comparisons_spent = self.comparisons_spent.saturating_add(spent);
    }
}

/// Per-pair rename detail: percentage score and exactness (§B.6.4/§B.6.5),
/// keyed by the display-base `(old, new)` pair recorded in `Changes.renamed`.
type RenameDetails = HashMap<(PathBuf, PathBuf), (u32, bool)>;

/// Detect renames between the `deleted` (old) and `new` sides of `changes`
/// using the diffcore engine (exact by OID → unique basename → bounded
/// exhaustive inexact, §B.4.2). Matched pairs are recorded in
/// `changes.renamed` and pruned from `deleted`/`new`; each pair's score and
/// exactness are added to `details`. Paths keep the change list's original
/// base for display; detection runs on repo-relative keys.
/// Map rename-engine degradation stats onto structured warnings (§B.5).
/// Split out so the seam is unit-testable independent of read budgets.
/// Fold one detection side's stats into the run-wide accumulator.
fn merge_rename_stats(
    acc: &mut rename_detect::RenameDetectStats,
    side: &rename_detect::RenameDetectStats,
) {
    acc.comparisons += side.comparisons;
    acc.skipped_by_limit |= side.skipped_by_limit;
    acc.exhaustive_discarded |= side.exhaustive_discarded;
    acc.peak_edges = acc.peak_edges.max(side.peak_edges);
    for (reason, count) in &side.content_skips {
        *acc.content_skips.entry(*reason).or_default() += count;
    }
}

fn warnings_from_rename_stats(
    stats: &rename_detect::RenameDetectStats,
    warnings: &mut Vec<StatusWarning>,
) {
    if stats.skipped_by_limit {
        warnings.push(StatusWarning {
            code: StatusWarningCode::RenameLimitProductSkipped,
            message: "rename detection skipped the exhaustive inexact pass: too many candidates on one side (renameLimit); exact and unique-basename matches were kept".to_string(),
            source: StatusWarningCode::RenameLimitProductSkipped.source(),
        });
    }
    if stats.exhaustive_discarded {
        warnings.push(StatusWarning {
            code: StatusWarningCode::SimilarityBudgetExceeded,
            message:
                "rename detection discarded the exhaustive inexact pass: similarity comparison budget exceeded; exact and already-scored unique-basename matches were kept"
                    .to_string(),
            source: StatusWarningCode::SimilarityBudgetExceeded.source(),
        });
    }
    // §B.3.4: content-read skips surface as deduplicated warnings — object
    // problems on the metadata side, worktree I/O on the worktree side,
    // budget/size caps as the budget family. Affected candidates were
    // dropped; the base status stays truthful.
    use rename_detect::SkipReason;
    let count = |reasons: &[SkipReason]| -> u64 {
        reasons
            .iter()
            .filter_map(|r| stats.content_skips.get(r))
            .sum()
    };
    // The reasons are side-qualified, so each family lands under the source
    // its published `source` claims: object-store problems under `metadata`,
    // working-tree problems under `worktree`. Folding them together would
    // report a worktree budget as a repository-object budget.
    let unavailable = count(&[
        SkipReason::ObjectMissing,
        SkipReason::ObjectCorrupt,
        SkipReason::ObjectUnavailable,
        SkipReason::ObjectIoFailed,
    ]);
    if unavailable > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::MetadataUnavailable,
            message: format!(
                "rename detection skipped {unavailable} candidate(s): repository objects missing, corrupt, or unreadable"
            ),
            source: StatusWarningCode::MetadataUnavailable.source(),
        });
    }
    let budget = count(&[SkipReason::ObjectTooLarge, SkipReason::ObjectBudgetExceeded]);
    if budget > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::MetadataBudgetExceeded,
            message: format!(
                "rename detection skipped {budget} candidate(s): object-read budget or per-object size cap reached"
            ),
            source: StatusWarningCode::MetadataBudgetExceeded.source(),
        });
    }
    let worktree_budget = count(&[
        SkipReason::WorktreeTooLarge,
        SkipReason::WorktreeBudgetExceeded,
    ]);
    if worktree_budget > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::WorktreeBudgetExceeded,
            message: format!(
                "rename detection skipped {worktree_budget} candidate(s): worktree-read budget or per-file size cap reached"
            ),
            source: StatusWarningCode::WorktreeBudgetExceeded.source(),
        });
    }
    let io_timeout = count(&[SkipReason::IoTimeout]);
    // NOTE: the worktree-family codes below are also emitted by the
    // io_blocked mapping for BASE-scan blocks, so `rename_detection_complete`
    // cannot key off the code alone — see `rename_scan_blocked`, which the
    // caller sets from these same counts.

    if io_timeout > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::WorktreeIoTimeout,
            message: format!(
                "rename detection reclaimed {io_timeout} candidate read(s) that exceeded the I/O deadline"
            ),
            source: StatusWarningCode::WorktreeIoTimeout.source(),
        });
    }
    let io_failed = count(&[SkipReason::WorktreeIoFailed]);
    if io_failed > 0 {
        warnings.push(StatusWarning {
            code: StatusWarningCode::WorktreeReadFailed,
            message: format!(
                "rename detection skipped {io_failed} candidate(s): worktree reads failed"
            ),
            source: StatusWarningCode::WorktreeReadFailed.source(),
        });
    }
}

/// R0-3: run unstaged rename detection against an EXPLICIT destination list
/// (the bounded probe's output, display base) instead of the untracked
/// display set. Matched pairs land in `changes.renamed`; the returned set
/// holds the consumed destinations for §B.3.5 marker collapse.
fn detect_renames_with_destinations(
    changes: &mut Changes,
    config: &rename_detect::RenameDetectConfig,
    old_side: RenameBlobSide<'_>,
    destinations_display: &[PathBuf],
    details: &mut RenameDetails,
    stats_acc: &mut rename_detect::RenameDetectStats,
    budgets: &mut RenameBudgets,
) -> HashSet<PathBuf> {
    let mut consumed_new: HashSet<PathBuf> = HashSet::new();
    if changes.deleted.is_empty() || destinations_display.is_empty() {
        return consumed_new;
    }
    let mut worktree_budget = budgets.take_worktree();
    let mut snapshot_skips: HashMap<rename_detect::SkipReason, u64> = HashMap::new();
    let snapshot = rename_detect::RenameSnapshot {
        old_map: build_rename_side(
            &changes.deleted,
            &old_side,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
        new_map: build_rename_side(
            destinations_display,
            &RenameBlobSide::Worktree,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
    };
    let mut source = StatusContentSource {
        old_is_worktree: matches!(old_side, RenameBlobSide::Worktree),
        new_is_worktree: true,
        objects: budgets.take_objects(),
        worktree: worktree_budget,
    };
    let narrowed = budgets.narrowed(config);
    let mut outcome = rename_detect::match_pairs(&snapshot, &narrowed, &mut source);
    // Hand the drawn-down budgets back, exactly like the sibling detector:
    // the run-level caps are call-level (§B.3.4), so this pass must neither
    // keep the remainder nor spend comparisons off the books — a pass added
    // after this one would otherwise restart with fresh budgets.
    budgets.restore_objects(&mut source.objects);
    budgets.restore_worktree(&source.worktree);
    budgets.record_comparisons(outcome.stats.comparisons);
    // Snapshot-construction skips (optional worktree hash/stat failures)
    // join the engine's own content skips so ONE warning family covers
    // every candidate the run had to drop.
    for (reason, count) in snapshot_skips {
        *outcome.stats.content_skips.entry(reason).or_default() += count;
    }
    // The stats are ACCUMULATED, not turned into warnings here: staged and
    // unstaged detection run separately, and emitting per side would produce
    // two warnings with the same {code, source} and different counts —
    // §B.5 requires exactly one per code/source for the whole run.
    merge_rename_stats(stats_acc, &outcome.stats);
    if outcome.matches.is_empty() {
        return consumed_new;
    }

    let mut consumed_old: HashSet<PathBuf> = HashSet::new();
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    for m in &outcome.matches {
        let old_display = util::workdir_to_current(&m.old);
        let new_display = util::workdir_to_current(&m.new);
        consumed_old.insert(old_display.clone());
        consumed_new.insert(new_display.clone());
        details.insert(
            (old_display.clone(), new_display.clone()),
            (m.score_percent(), m.exact),
        );
        renamed.push((old_display, new_display));
    }
    changes.deleted.retain(|p| !consumed_old.contains(p));
    changes.new.retain(|p| !consumed_new.contains(p));
    changes.deleted.sort();
    changes.new.sort();
    renamed.sort_by(|a, b| a.1.cmp(&b.1));
    changes.renamed.extend(renamed);
    consumed_new
}

fn detect_renames_in_changes(
    changes: &mut Changes,
    config: &rename_detect::RenameDetectConfig,
    old_side: RenameBlobSide<'_>,
    new_side: RenameBlobSide<'_>,
    details: &mut RenameDetails,
    stats_acc: &mut rename_detect::RenameDetectStats,
    budgets: &mut RenameBudgets,
) {
    if changes.deleted.is_empty() || changes.new.is_empty() {
        return;
    }
    // Budgets are CALL-level, not per-side: the staged and unstaged passes
    // draw from the same caps. Constructing them here let one `status` spend
    // 2 × 500k comparisons and 2 × 64 MiB of reads while both sides
    // individually looked compliant.
    let mut worktree_budget = budgets.take_worktree();
    let mut snapshot_skips: HashMap<rename_detect::SkipReason, u64> = HashMap::new();
    let snapshot = rename_detect::RenameSnapshot {
        old_map: build_rename_side(
            &changes.deleted,
            &old_side,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
        new_map: build_rename_side(
            &changes.new,
            &new_side,
            &mut worktree_budget,
            &mut snapshot_skips,
        ),
    };
    let mut source = StatusContentSource {
        old_is_worktree: matches!(old_side, RenameBlobSide::Worktree),
        new_is_worktree: matches!(new_side, RenameBlobSide::Worktree),
        objects: budgets.take_objects(),
        worktree: worktree_budget,
    };
    let narrowed = budgets.narrowed(config);
    let mut outcome = rename_detect::match_pairs(&snapshot, &narrowed, &mut source);
    // Hand the drawn-down budgets back so the OTHER side continues from
    // here rather than starting fresh.
    budgets.restore_objects(&mut source.objects);
    budgets.restore_worktree(&source.worktree);
    budgets.record_comparisons(outcome.stats.comparisons);
    for (reason, count) in snapshot_skips {
        *outcome.stats.content_skips.entry(reason).or_default() += count;
    }
    // The stats are ACCUMULATED, not turned into warnings here: staged and
    // unstaged detection run separately, and emitting per side would produce
    // two warnings with the same {code, source} and different counts —
    // §B.5 requires exactly one per code/source for the whole run.
    merge_rename_stats(stats_acc, &outcome.stats);
    if outcome.matches.is_empty() {
        return;
    }

    // Map repo-relative matches back to the change list's display base and
    // prune consumed endpoints.
    let mut consumed_old: HashSet<PathBuf> = HashSet::new();
    let mut consumed_new: HashSet<PathBuf> = HashSet::new();
    let mut renamed: Vec<(PathBuf, PathBuf)> = Vec::new();
    for m in &outcome.matches {
        let old_display = util::workdir_to_current(&m.old);
        let new_display = util::workdir_to_current(&m.new);
        consumed_old.insert(old_display.clone());
        consumed_new.insert(new_display.clone());
        details.insert(
            (old_display.clone(), new_display.clone()),
            (m.score_percent(), m.exact),
        );
        renamed.push((old_display, new_display));
    }
    changes.deleted.retain(|p| !consumed_old.contains(p));
    changes.new.retain(|p| !consumed_new.contains(p));
    changes.deleted.sort();
    changes.new.sort();
    renamed.sort_by(|a, b| a.1.cmp(&b.1));
    changes.renamed.extend(renamed);
}

/// Numeric Unix mode for a `TreeItemMode` (`100644`/`100755`/`120000`/
/// `160000`), matching Git's stored blob modes.
fn tree_item_mode_to_unix(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Commit => 0o160000,
        TreeItemMode::Tree => 0o040000,
    }
}

/// The error for a HEAD object that passes ref validation but is not in the
/// object store. `Commit::load` / `Tree::load` PANIC in that case, so every
/// status path that expands HEAD goes through the `try_load` pair below: a
/// pruned or corrupt object is a repository problem the user can act on, not
/// a reason to take the process down.
pub(crate) fn head_object_unreadable(what: &str, oid: &ObjectHash) -> CliError {
    CliError::fatal(format!(
        "cannot read the HEAD {what} '{oid}': the object is missing or corrupt"
    ))
    .with_stable_code(StableErrorCode::RepoStateInvalid)
    .with_hint("run 'libra fsck' or restore the object, then retry")
}

/// Load the HEAD commit and its tree, failing closed when either is absent.
pub(crate) fn load_head_commit_tree(head_oid: &ObjectHash) -> CliResult<(Commit, Tree)> {
    let commit =
        Commit::try_load(head_oid).ok_or_else(|| head_object_unreadable("commit", head_oid))?;
    let tree = Tree::try_load(&commit.tree_id)
        .ok_or_else(|| head_object_unreadable("tree", &commit.tree_id))?;
    Ok((commit, tree))
}

/// HEAD tree blobs keyed by repo-relative path → (oid, mode) (§B.4.1 old side
/// of the staged snapshot).
fn load_head_tree_blobs(head_oid: &ObjectHash) -> CliResult<HashMap<PathBuf, (ObjectHash, u32)>> {
    let (_, tree) = load_head_commit_tree(head_oid)?;
    Ok(tree
        .get_plain_items_with_mode()
        .into_iter()
        .map(|(path, hash, mode)| (path, (hash, tree_item_mode_to_unix(mode))))
        .collect())
}

/// Index stage-0 blobs keyed by repo-relative path → (oid, mode) (index side
/// of both snapshots).
fn load_index_stage0_blobs(index: &Index) -> HashMap<PathBuf, (ObjectHash, u32)> {
    index
        .tracked_entries(0)
        .into_iter()
        .map(|entry| (PathBuf::from(&entry.name), (entry.hash, entry.mode)))
        .collect()
}

pub(crate) fn load_status_index() -> CliResult<Index> {
    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    Index::load(&index_path).map_err(|source| {
        CliError::from(StatusError::IndexLoad {
            path: index_path,
            source,
        })
    })
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Collect repository status and render it inside the same `{ok, command,
/// data}` envelope that `libra status --json` prints, so `/api/repo/status`
/// stays byte-compatible with the CLI output.
///
/// Internally re-uses [`collect_status_data`] + [`build_status_json`] with a
/// default [`StatusArgs`] (untracked files in normal mode, no porcelain v2,
/// no ignored files, no stash count).
///
/// Status collection currently resolves storage from the process working
/// directory; the embedded web server expects to be launched from (or with
/// `--cwd`/`--repo` already chdir'd to) the repository root. Callers that
/// need to scope to a specific path should pass it via `working_dir`.
pub async fn collect_status_json_envelope_for_api(
    working_dir: &std::path::Path,
) -> CliResult<serde_json::Value> {
    use std::path::PathBuf;

    // Serialize concurrent API collections: interleaved collections against
    // the shared repository connection have produced transient read errors
    // and — worse — silently inconsistent snapshots (a staged-deletion side
    // observed empty mid-interleave). The API serves one same-cwd repository,
    // so a process-wide mutex is the correct, cheap consistency guarantee.
    static API_STATUS_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
    let _serialized = API_STATUS_LOCK.lock().await;

    let mut args = StatusArgs::default();
    let canon_working =
        std::fs::canonicalize(working_dir).unwrap_or_else(|_| PathBuf::from(working_dir));
    let canon_cwd = std::env::current_dir()
        .ok()
        .and_then(|cwd| std::fs::canonicalize(&cwd).ok());
    if canon_cwd.as_deref() != Some(canon_working.as_path()) {
        return Err(CliError::fatal(format!(
            "/api/repo/status currently requires the libra process to run inside its repository root. Expected '{}', found '{}'. Re-launch `libra code` from the repo or open an issue if you need cross-directory status.",
            canon_working.display(),
            canon_cwd
                .as_deref()
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "<unavailable>".to_string()),
        )));
    }

    // Byte-parity with `libra status --json`: the API honors the same
    // resolved `status.*` defaults (and the same fail-closed validation) as
    // the CLI entry points.
    let extras = apply_status_config_defaults(&mut args).await?;
    let args = args;
    // An API request inherits NOTHING from the process: a long-running
    // server's preflight buffer is not this request's warning list.
    let data = collect_status_data(&args, extras, &InvocationWarningCtx::empty()).await?;
    let inner = build_status_json(&data, &args);
    Ok(serde_json::json!({
        "ok": true,
        "command": "status",
        "data": inner,
    }))
}

pub async fn execute(args: StatusArgs) {
    if let Err(err) = execute_to(args, &mut std::io::stdout()).await {
        err.print_stderr();
    }
}

/// Safe entry point that returns structured [`CliResult`] instead of printing
/// errors and exiting. JSON mode propagates status-computation failures as
/// structured CLI errors; text mode uses the same structured error contract.
pub async fn execute_safe(args: StatusArgs, output: &OutputConfig) -> CliResult<()> {
    execute_safe_with_resolution(args, output, None).await
}

/// CLI entry with the pre-clap argv resolution (§B.4.3): the occurrence list
/// overrides the legacy percent field so all three rename spellings obey
/// true last-one-wins.
pub(crate) async fn execute_safe_with_resolution(
    mut args: StatusArgs,
    output: &OutputConfig,
    resolution: Option<&StatusArgvResolution>,
) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    // Fail closed on invalid `status.*` config before any mode runs or any
    // output is produced; CLI flags keep precedence inside the resolver.
    let mut extras = apply_status_config_defaults(&mut args).await?;
    if let Some(resolution) = resolution {
        extras.rename_threshold = resolve_status_threshold(&args, Some(resolution))?;
        // The argv scan and clap must agree about which format flags were
        // GIVEN. The scan interprets short clusters from the subcommand's own
        // arity table (so `-uno` is `-u=no`, and its letters are not flags);
        // if it ever disagreed with clap, the disagreement would show up as
        // silently wrong output — NUL separators that were never asked for,
        // or a porcelain format the user did not select. It fails closed
        // instead. The implication is one-directional on purpose: config can
        // set `short` with nothing in argv, but argv can never set something
        // clap did not see.
        resolution.format.ensure_agrees_with(&args)?;
    }
    let args = args;

    // Dirty-set cache modes (lore.md 1.1). NOTE: only this CLI entry routes
    // them — the legacy `execute_to` writer entry ignores the flags (its
    // callers never set them).
    //
    // Part C W1 (§C.4.1.1): the dirty cache is worktree-scoped, so the
    // cache-semantic modes run in any worktree against their own rows.
    if args.scan {
        return run_status_scan(&args, extras, output).await;
    }
    if args.cached || args.check_dirty {
        return run_status_cache_mode(&args, extras, output).await;
    }

    let data = collect_status_data(
        &args,
        extras,
        &InvocationWarningCtx::from_process_preflight(),
    )
    .await?;

    if output.is_json() {
        let json_data = build_status_json(&data, &args);
        // A non-EPIPE stdout failure must not swallow the collected
        // warnings: JSON is their only channel, so they fall back to stderr
        // rather than vanishing with the envelope.
        if let Err(error) = emit_json_data("status", &json_data, output) {
            if !error.is_silent() {
                deliver_warnings_stderr(&data.warnings);
            }
            return Err(error);
        }
    } else {
        // §B.5 delivery matrix: stderr warnings even under `--quiet`
        // (quiet suppresses the body, never diagnostics).
        deliver_warnings_stderr(&data.warnings);
        // Fail closed BEFORE the quiet check: suppressing the body must not
        // suppress the "could not inspect" verdict.
        fail_closed_on_io_blocked(&data, output)?;
        if !output.quiet {
            let mut stdout = std::io::stdout();
            render_status_to_writer(&data, &args, output, &mut stdout).await?;
        }
    }

    // §B.5 exit arbitration: warnings + --exit-code-on-warning (9) beats
    // the --exit-code dirty exit (1); JSON gets the same silent 9 without a
    // second stderr envelope.
    StatusOutcome::new(&data, &args).resolve(output)?;

    Ok(())
}

// ─── Dirty-set cache modes (lore.md §1.1) ───────────────────────────────────

async fn effective_ignore_case_for_status() -> CliResult<bool> {
    crate::utils::path_case::effective_ignore_case()
        .await
        .map_err(|error| {
            CliError::fatal(error.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
        })
}

fn dirty_cache_error(action: &str, error: anyhow::Error) -> CliError {
    CliError::fatal(format!("failed to {action} the dirty cache: {error}"))
        .with_stable_code(StableErrorCode::IoWriteFailed)
}

/// Snapshot rows from the raw sets ('/'-normalized repo-relative paths).
/// Cache rows for a snapshot, plus the count of paths that could not be
/// represented in the cache at all.
struct SnapshotRows {
    rows: Vec<(String, &'static str)>,
    unencodable: u64,
}

fn snapshot_rows(staged: &Changes, unstaged: &Changes) -> SnapshotRows {
    use crate::internal::dirty;
    let mut rows: Vec<(String, &'static str)> = Vec::new();
    let mut unencodable = 0u64;
    let mut push = |paths: &[PathBuf], kind: &'static str| {
        for path in paths {
            // A non-UTF-8 name cannot be stored, but §B.6.1 forbids failing
            // status over one: the row is DROPPED (never lossy-mangled into
            // a different file's key) and the omission is reported, so
            // `--cached` under-reporting that path is visible rather than
            // silent.
            match dirty::native_path_to_stored(path) {
                Ok(stored) => rows.push((stored, kind)),
                Err(_) => unencodable += 1,
            }
        }
    };
    push(&unstaged.new, dirty::KIND_NEW);
    push(&unstaged.modified, dirty::KIND_MODIFIED);
    push(&unstaged.deleted, dirty::KIND_DELETED);
    push(&staged.new, dirty::KIND_STAGED_NEW);
    push(&staged.modified, dirty::KIND_STAGED_MODIFIED);
    push(&staged.deleted, dirty::KIND_STAGED_DELETED);
    SnapshotRows { rows, unencodable }
}

/// `status --scan`: run the full safe reconcile and atomically replace the
/// cache snapshot from it. TOCTOU-safe: the index fingerprint and HEAD are
/// captured BEFORE the reconcile and re-verified AFTER — a concurrent index
/// writer aborts the cache commit (the old snapshot stays intact) instead of
/// stamping rows computed against an older index as fresh.
async fn run_status_scan(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    output: &OutputConfig,
) -> CliResult<()> {
    use crate::internal::dirty::{DirtyCache, ScanLockOutcome};

    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;
    let mut cache_warnings: Vec<StatusWarning> = Vec::new();
    let pid = std::process::id() as i64;
    match DirtyCache::try_acquire_scan_lock_with_conn(&db, pid)
        .await
        .map_err(|e| dirty_cache_error("lock", e))?
    {
        ScanLockOutcome::Acquired { stole } => {
            if stole {
                cache_warnings.push(cache_warning(
                    StatusWarningCode::DirtyCacheLockStolen,
                    "stole a stale dirty-cache scan lock (previous scanner crashed?)",
                ));
            }
        }
        ScanLockOutcome::Held { pid, since } => {
            return Err(CliError::failure(format!(
                "another `status --scan` holds the dirty-cache lock (pid {pid}, since {since})"
            ))
            .with_stable_code(StableErrorCode::ConflictOperationBlocked)
            .with_hint("wait for it to finish, or re-run later (stale locks are stolen)"));
        }
    }
    // Everything below must release the lock — including error paths.
    let result = run_status_scan_locked(args, output, &index_path, extras, cache_warnings).await;
    let _ = DirtyCache::release_scan_lock_with_conn(&db, pid).await;
    result?;
    // Re-open a plain connection for the final read in JSON mode is not
    // needed; run_status_scan_locked rendered already.
    Ok(())
}

async fn run_status_scan_locked(
    args: &StatusArgs,
    output: &OutputConfig,
    index_path: &std::path::Path,
    extras: StatusConfigExtras,
    mut cache_warnings: Vec<StatusWarning>,
) -> CliResult<()> {
    // Preserve the stolen-lock diagnostic on EVERY failure inside the locked
    // scan (fingerprints, raw sets, collection, cache txn, even a failed
    // JSON emit after the payload append) — parity with the pre-R0-8b
    // immediate stderr warning. The snapshot makes delivery independent of
    // where the failure lands; JSON mode also gets the stderr line on ERROR
    // paths only (the success-path matrix — clean stderr — is untouched,
    // and losing the diagnostic entirely would be worse than annotating an
    // already-broken envelope exchange). Success delivers exactly once via
    // the payload.
    let result =
        run_status_scan_locked_inner(args, output, index_path, extras, &mut cache_warnings).await;
    if result.as_ref().is_err_and(|error| !error.is_silent()) {
        // Silent exits skip the fallback BY DESIGN: (a) the 9≻1 arbitration
        // already delivered via the rendered payload; (b) a stdout
        // BrokenPipe maps to a silent exit whose P0-06 contract
        // (`compat_broken_pipe_output`) pins stderr to ZERO noise after the
        // downstream closes — delivering there would violate that guard.
        // Only real failures need the fallback. The vec is the CANONICAL
        // pending set: the inner fn drains collected rename warnings into it
        // right after collection and only moves everything into the payload
        // at the final render, so whatever failed leaves the full set here.
        deliver_warnings_stderr(&cache_warnings);
    }
    result
}

async fn run_status_scan_locked_inner(
    args: &StatusArgs,
    output: &OutputConfig,
    index_path: &std::path::Path,
    extras: StatusConfigExtras,
    cache_warnings: &mut Vec<StatusWarning>,
) -> CliResult<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::dirty::{DirtyCache, current_index_fingerprint};

    let fingerprint_before =
        current_index_fingerprint(index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_before = Head::current_commit().await.map(|oid| oid.to_string());
    let scan_started_at = crate::internal::dirty::now_timestamp();

    // The io_blocked-aware collection runs FIRST: the legacy raw walker
    // below fails fast on an unreadable directory, which would bypass the
    // partial contract (JSON) and the cache guard (both formats).
    let mut data = collect_status_data(
        args,
        extras,
        &InvocationWarningCtx::from_process_preflight(),
    )
    .await?;
    // Fold the collected (rename) warnings into the canonical pending vec so
    // ANY later failure — recheck, txn, JSON emit, render, summary — reaches
    // the wrapper fallback with the complete set.
    cache_warnings.append(&mut data.warnings);

    let fingerprint_after =
        current_index_fingerprint(index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_after = Head::current_commit().await.map(|oid| oid.to_string());
    if fingerprint_before != fingerprint_after || head_before != head_after {
        return Err(CliError::failure(
            "the index or HEAD changed while scanning; the dirty cache was left untouched",
        )
        .with_stable_code(StableErrorCode::ConflictOperationBlocked)
        .with_hint("re-run 'libra status --scan' once the concurrent operation finishes"));
    }

    // The CACHE is a whole-repository snapshot, so it is collected WITHOUT
    // the pathspec — still through the bounded, io_blocked-aware
    // accumulator, never a legacy fail-fast walk. It runs BEFORE the guard
    // below so a block discovered only by this pass takes the same shared
    // route: JSON reports the partial result, text fails closed. Returning
    // fatal from inside the collection would bypass `data.io_blocked[]`,
    // the warnings and the completeness flags in JSON mode.
    let snapshot_data = {
        let mut unfiltered = args.clone();
        unfiltered.pathspec.clear();
        // The cache records individual PATHS, so the snapshot walk must not
        // collapse untracked directories into a `dir/` marker the way the
        // display default does — `--cached docs` has to be able to answer
        // with `docs/readme.md`.
        unfiltered.untracked_files = Some(UntrackedFiles::All);
        // Rename detection is DISABLED for the snapshot. Pairing removes the
        // endpoints from `deleted`/`new` and records them under `renamed`,
        // which the cache has no row kind for — so a scan of a renamed file
        // would persist an empty snapshot and a later `--cached --exit-code`
        // would call the repository clean.
        let mut snapshot_extras = extras;
        snapshot_extras.rename_threshold = None;
        collect_status_data(
            &unfiltered,
            snapshot_extras,
            &InvocationWarningCtx::from_process_preflight(),
        )
        .await?
    };
    for event in &snapshot_data.io_blocked {
        if !data
            .io_blocked
            .iter()
            .any(|existing| existing.path == event.path)
        {
            data.io_blocked.push(event.clone());
            data.base_scan_blocked = true;
            let (reason, code) = io_blocked_reason_and_code(event.reason);
            cache_warnings.push(StatusWarning {
                code,
                message: format!(
                    "cannot inspect '{}': {reason}",
                    quote_pathname(&event.path, data.quote_path)
                ),
                source: code.source(),
            });
        }
    }
    data.io_blocked
        .sort_by_key(|event| raw_path_sort_key(&event.path));

    // §B.3.3 dirty-cache guard: an I/O-blocked scan must not write ANY
    // cache content — an unreadable path is "cannot tell", and caching the
    // partial snapshot would prune NEW rows or confirm DELETED ones that
    // were merely unreadable. Text formats fail closed below anyway; JSON
    // reports the partial result while the previous snapshot stays intact.
    if !data.io_blocked.is_empty() {
        if output.is_json() {
            // JSON keeps the partial contract: report the blocked paths and
            // the untouched cache instead of failing, then let the shared
            // arbitration decide the exit code (warning 9 ≻ dirty 1).
            data.warnings = cache_warnings.clone();
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] = serde_json::json!("scan");
            json_data["cache_written"] = serde_json::json!(false);
            // A non-EPIPE stdout failure must not swallow the collected
            // warnings; the OUTER `run_status_scan_locked` wrapper owns the
            // stderr fallback for every non-silent inner failure (a second
            // delivery here double-printed the shared `cache_warnings` set,
            // Codex W5-09 r9 P2).
            emit_json_data("status", &json_data, output)?;
            StatusOutcome::new(&data, args).resolve(output)?;
            return Ok(());
        }
        return Err(CliError::fatal(format!(
            "cannot rebuild the dirty cache: {} path(s) could not be inspected (first: '{}')",
            data.io_blocked.len(),
            quote_pathname(&data.io_blocked[0].path, data.quote_path)
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
        .with_hint("fix the unreadable path permissions and re-run 'libra status --scan'")
        .with_hint(
            "the previous dirty-cache snapshot was left untouched; use --json to inspect \
             the partial result with data.io_blocked[]",
        ));
    }
    // Fully inspectable: the cache rows come from the accumulator walk above.
    let rooted = snapshot_data.to_repo_relative();
    let (staged_raw, unstaged_raw) = (rooted.staged.clone(), rooted.unstaged.clone());
    let SnapshotRows { rows, unencodable } = snapshot_rows(&staged_raw, &unstaged_raw);
    if unencodable > 0 {
        cache_warnings.push(cache_warning(
            StatusWarningCode::DirtyCachePathUnencodable,
            format!(
                "{unencodable} path(s) with non-UTF-8 names were omitted from the dirty cache; \
                 the full status still reports them"
            ),
        ));
    }
    let row_count = rows.len();
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;
    let txn = db
        .begin()
        .await
        .map_err(|e| dirty_cache_error("open a transaction for", anyhow::anyhow!(e)))?;
    DirtyCache::replace_all_with_conn(
        &txn,
        &rows,
        &fingerprint_before,
        head_before.as_deref(),
        &scan_started_at,
    )
    .await
    .map_err(|e| dirty_cache_error("write", e))?;
    txn.commit()
        .await
        .map_err(|e| dirty_cache_error("commit", anyhow::anyhow!(e)))?;

    // NON-DESTRUCTIVE copy into the render payload: the canonical vec stays
    // intact so the wrapper's fallback still holds the complete set if the
    // JSON emit / body render / summary write below fails non-silently.
    // (Success delivers via the payload; the wrapper only fires on Err.)
    data.warnings = cache_warnings.clone();

    if output.is_json() {
        let mut json_data = build_status_json(&data, args);
        json_data["mode"] = serde_json::json!("scan");
        json_data["cached_paths"] = serde_json::json!(row_count);
        // A non-EPIPE stdout failure must not swallow the collected
        // warnings; the OUTER `run_status_scan_locked` wrapper owns the
        // stderr fallback for every non-silent inner failure (a second
        // delivery here double-printed the shared `cache_warnings` set,
        // Codex W5-09 r9 P2).
        emit_json_data("status", &json_data, output)?;
    } else {
        // Deliver AFTER the body renders: a render failure then reaches the
        // wrapper's snapshot fallback instead of double-printing (warnings
        // still fire under --quiet, which only skips the body).
        if !output.quiet {
            use std::io::Write;
            let mut stdout = std::io::stdout();
            render_status_to_writer(&data, args, output, &mut stdout).await?;
            // Fallible write (a bare println! would panic on stdout failure
            // and bypass the wrapper's warning fallback).
            writeln!(stdout, "dirty cache rebuilt ({row_count} paths)").map_err(|error| {
                crate::utils::output::stdout_write_error("write the status scan summary", error)
            })?;
        }
        deliver_warnings_stderr(&data.warnings);
    }
    StatusOutcome::new(&data, args).resolve(output)?;
    Ok(())
}

/// Classify a manual (`kind='unknown'`) mark against the index, bounded and
/// panic-free (deliberately no `Index::is_modified`, which panics on missing
/// entries/files): returns the effective kind, or `None` when clean.
/// §B.3.3 revalidation tri-state: `NotFound` is the only proof of absence —
/// any other metadata error means "cannot tell" and must neither prune a
/// cached NEW row nor confirm a cached DELETED row.
#[derive(Clone, Copy)]
enum CachedPathState {
    Exists,
    Gone,
    /// Carries the §B.6.0.1 reason so a blocked re-verification can be
    /// reported through `data.io_blocked[]` with the same taxonomy the
    /// worktree walk uses, instead of a generic "something failed".
    Blocked(crate::command::status_probe::IoBlockedReason),
}

/// Classify a revalidation I/O error into the §B.6.0.1 reason taxonomy.
/// `TimedOut` is its OWN reason: the docs promise consumers can tell a hung
/// mount from an ordinary read failure, and collapsing it into `io_error`
/// silently breaks that distinction on the cache path.
fn blocked_reason(error: &io::Error) -> crate::command::status_probe::IoBlockedReason {
    use crate::command::status_probe::IoBlockedReason;
    match error.kind() {
        io::ErrorKind::PermissionDenied => IoBlockedReason::PermissionDenied,
        io::ErrorKind::TimedOut => IoBlockedReason::IoTimeout,
        _ => IoBlockedReason::IoError,
    }
}

/// Hash a worktree file for cache revalidation under the §B.3.3 deadline.
/// A reclaimed read surfaces as `TimedOut` so callers classify it as blocked
/// rather than as a content change.
fn hash_under_deadline(abs: &Path, workdir: &Path) -> io::Result<git_internal::hash::ObjectHash> {
    match crate::command::status_io_worker::deadline_file_blob_hash(abs, workdir) {
        Ok(result) => result,
        Err(()) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "content read exceeded the status I/O deadline",
        )),
    }
}

fn cached_path_state(abs: &Path) -> CachedPathState {
    use crate::command::status_probe::IoBlockedReason;
    // §B.3.3: revalidation stats run under the same deadline as the full
    // scan. A cached path that became a FIFO or moved onto a hung mount must
    // reclaim the caller and keep its cache row, not block `--check-dirty`
    // forever.
    let stat = match crate::command::status_io_worker::deadline_stat(abs) {
        Ok(result) => result,
        Err(()) => return CachedPathState::Blocked(IoBlockedReason::IoTimeout),
    };
    match stat {
        Ok(_) => CachedPathState::Exists,
        Err(error) if error.kind() == io::ErrorKind::NotFound => CachedPathState::Gone,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            CachedPathState::Blocked(IoBlockedReason::PermissionDenied)
        }
        Err(error) if error.kind() == io::ErrorKind::TimedOut => {
            CachedPathState::Blocked(IoBlockedReason::IoTimeout)
        }
        Err(_) => CachedPathState::Blocked(IoBlockedReason::IoError),
    }
}

/// Outcome of re-classifying a manual `libra dirty` mark (stored as
/// `unknown`). The `Blocked` arm exists because "cannot inspect" is neither
/// dirty nor clean: collapsing it into either one lets `--check-dirty`
/// delete a still-valid row or render a present file as deleted.
enum ManualMarkClass {
    Dirty(&'static str),
    Clean,
    Blocked(crate::command::status_probe::IoBlockedReason),
}

fn classify_manual_mark(index: &Index, workdir: &std::path::Path, stored: &str) -> ManualMarkClass {
    use crate::internal::dirty;
    let native = dirty::stored_path_to_native(stored);
    let Some(path_str) = native.to_str() else {
        return ManualMarkClass::Dirty(dirty::KIND_NEW); // undecodable: over-report
    };
    let tracked = index.tracked(path_str, 0);
    let abs = workdir.join(&native);
    // Tri-state stat (§B.6.0.1): EACCES must not masquerade as absence, or a
    // tracked-but-unreadable path is reported deleted and a manual mark on
    // an unreadable untracked path is pruned from the cache.
    let exists = match cached_path_state(&abs) {
        CachedPathState::Exists => true,
        CachedPathState::Gone => false,
        CachedPathState::Blocked(reason) => return ManualMarkClass::Blocked(reason),
    };
    match (tracked, exists) {
        (false, true) => ManualMarkClass::Dirty(dirty::KIND_NEW),
        (false, false) => ManualMarkClass::Clean, // neither tracked nor present: not dirty
        (true, false) => ManualMarkClass::Dirty(dirty::KIND_DELETED),
        (true, true) => {
            // Content confirm (no stat shortcut: manual marks are few, and a
            // wrong stat shortcut here would silently drop a real edit).
            match hash_under_deadline(&abs, workdir) {
                Ok(hash) if index.verify_hash(path_str, 0, &hash) => ManualMarkClass::Clean,
                Ok(_) => ManualMarkClass::Dirty(dirty::KIND_MODIFIED),
                // Readable metadata but an unreadable body: still "cannot
                // inspect", so never let it confirm or prune a cache row.
                Err(error) => ManualMarkClass::Blocked(blocked_reason(&error)),
            }
        }
    }
}

/// `status --cached` and `status --check-dirty`: consume / re-verify the
/// cache. Any freshness doubt degrades to the full reconcile (the cache may
/// over-report or degrade, never silently under-report).
async fn run_status_cache_mode(
    args: &StatusArgs,
    extras: StatusConfigExtras,
    output: &OutputConfig,
) -> CliResult<()> {
    use sea_orm::TransactionTrait;

    use crate::internal::dirty::{self, CacheState, DirtyCache, current_index_fingerprint};

    let index_path =
        path::try_index().map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let fingerprint =
        current_index_fingerprint(&index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_oid = Head::current_commit().await.map(|oid| oid.to_string());
    let db = crate::internal::sequencer::request_db_checked()
        .await
        .map_err(|error| CliError::fatal(error).with_stable_code(StableErrorCode::IoReadFailed))?;
    let meta = DirtyCache::meta_with_conn(&db)
        .await
        .map_err(|e| dirty_cache_error("read", e))?;
    let state = DirtyCache::classify(meta.as_ref(), &fingerprint, head_oid.as_deref());

    if state != CacheState::Fresh {
        // Degrade to the full reconcile — never trust a doubtful cache.
        let mut data = collect_status_data(
            args,
            extras,
            &InvocationWarningCtx::from_process_preflight(),
        )
        .await?;
        data.warnings.push(cache_warning(
            StatusWarningCode::DirtyCacheStaleFallback,
            format!(
                "dirty cache is {}; falling back to the full status (run 'libra status --scan' to rebuild)",
                state.as_str()
            ),
        ));
        if output.is_json() {
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] =
                serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
            json_data["freshness"] = serde_json::json!("full");
            json_data["cache_state"] = serde_json::json!(state.as_str());
            if let Err(error) = emit_json_data("status", &json_data, output) {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
                return Err(error);
            }
        } else {
            // Fail closed on any blocked path BEFORE rendering — and before
            // the quiet check. The fallback runs a full scan, so it can
            // discover blocked paths exactly like the normal path; skipping
            // the guard here would let `--cached` print a partial body (or
            // exit 0 under `--quiet`) on a repository it could not inspect.
            fail_closed_on_io_blocked(&data, output).inspect_err(|error| {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
            })?;
            // Deliver AFTER the body: EPIPE mid-render stays fully silent
            // (P0-06), while a real render failure still surfaces the
            // warning before propagating.
            if !output.quiet {
                let mut stdout = std::io::stdout();
                if let Err(error) = render_status_to_writer(&data, args, output, &mut stdout).await
                {
                    if !error.is_silent() {
                        deliver_warnings_stderr(&data.warnings);
                    }
                    return Err(error);
                }
            }
            deliver_warnings_stderr(&data.warnings);
        }
        StatusOutcome::new(&data, args).resolve(output)?;
        return Ok(());
    }

    let rows = DirtyCache::list_with_conn(&db)
        .await
        .map_err(|e| dirty_cache_error("read", e))?;
    let workdir = util::try_working_dir()
        .map_err(|source| CliError::from(StatusError::Workdir { source }))?;
    let index = load_status_index()?;

    // Build the raw sets from the cache (staged snapshot + unstaged rows +
    // classified manual marks), optionally re-verifying (--check-dirty).
    let mut staged = Changes::default();
    let mut unstaged = Changes::default();
    let mut pruned: Vec<(String, String)> = Vec::new();
    let mut confirmed: Vec<(String, String)> = Vec::new();
    // Rows whose re-verification could not run (§B.6.0.1). These are neither
    // confirmed nor pruned, and their presence suppresses the cache write
    // entirely — a partial re-verification must never be persisted as if it
    // had inspected everything.
    let mut blocked_paths: Vec<(PathBuf, crate::command::status_probe::IoBlockedReason)> =
        Vec::new();
    for row in &rows {
        let native = dirty::stored_path_to_native(&row.path);
        let verify = args.check_dirty;
        match row.kind.as_str() {
            dirty::KIND_STAGED_NEW => staged.new.push(native),
            dirty::KIND_STAGED_MODIFIED => staged.modified.push(native),
            dirty::KIND_STAGED_DELETED => staged.deleted.push(native),
            dirty::KIND_NEW => {
                // An undecodable stored path cannot be re-verified — keep it
                // (the cache must never under-report a recorded fact).
                let Some(path_str) = native.to_str() else {
                    unstaged.new.push(native);
                    continue;
                };
                let abs = workdir.join(&native);
                // ONE tri-state stat, reused for both the guard and the
                // decision. Calling it twice reintroduces the race it exists
                // to close: a permission change between the two calls makes
                // the second read "gone" while `blocked_paths` stays empty,
                // and the row is pruned from a cache it was never verified
                // against.
                let state = if verify {
                    cached_path_state(&abs)
                } else {
                    CachedPathState::Exists
                };
                if let (true, CachedPathState::Blocked(reason)) = (verify, state) {
                    // Unreadable is NOT proof the path went away (§B.6.0.1):
                    // keep the cached fact, write nothing — pruning here
                    // would silently forget a real untracked file.
                    blocked_paths.push((native.clone(), reason));
                    unstaged.new.push(native);
                    continue;
                }
                let still = !verify
                    || (matches!(state, CachedPathState::Exists) && !index.tracked(path_str, 0));
                if still {
                    unstaged.new.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            dirty::KIND_MODIFIED => {
                let Some(path_str) = native.to_str() else {
                    unstaged.modified.push(native);
                    continue;
                };
                let abs = workdir.join(&native);
                // Single tri-state stat, reused below (see the NEW branch).
                let state = if verify {
                    cached_path_state(&abs)
                } else {
                    CachedPathState::Exists
                };
                if let (true, CachedPathState::Blocked(reason)) = (verify, state) {
                    // Unreadable: keep the cached fact, write nothing.
                    blocked_paths.push((native.clone(), reason));
                    unstaged.modified.push(native);
                    continue;
                }
                // The stat can succeed while the CONTENT read fails (a
                // chmod-000 file still stats fine), so the hash failure is
                // its own blocked case: keep the row, report it, write
                // nothing. Treating it as "still modified" without an event
                // would leave `io_blocked[]` and `warnings[]` empty on a run
                // that demonstrably could not inspect the file.
                let mut still = !verify;
                if verify {
                    still = index.tracked(path_str, 0)
                        && matches!(state, CachedPathState::Exists)
                        && match hash_under_deadline(&abs, &workdir) {
                            Ok(hash) => !index.verify_hash(path_str, 0, &hash),
                            Err(error) => {
                                blocked_paths.push((native.clone(), blocked_reason(&error)));
                                true // unreadable: keep (over-report)
                            }
                        };
                }
                if still {
                    unstaged.modified.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            dirty::KIND_DELETED => {
                let Some(path_str) = native.to_str() else {
                    unstaged.deleted.push(native);
                    continue;
                };
                // `--cached` promises NO worktree walk: only `--check-dirty`
                // re-verifies, so the stat is skipped entirely rather than
                // performed and discarded (which would still take an I/O
                // worker slot and could wait out a deadline on a hung mount).
                let state = if verify {
                    cached_path_state(&workdir.join(&native))
                } else {
                    CachedPathState::Exists
                };
                if let (true, CachedPathState::Blocked(reason)) = (verify, state) {
                    // Unreadable is NOT proof of deletion (§B.6.0.1): keep
                    // the cached fact, write nothing — never confirm.
                    blocked_paths.push((native.clone(), reason));
                    unstaged.deleted.push(native);
                    continue;
                }
                let still = !verify
                    || (index.tracked(path_str, 0) && matches!(state, CachedPathState::Gone));
                if still {
                    unstaged.deleted.push(native);
                    if verify {
                        confirmed.push((row.path.clone(), row.kind.clone()));
                    }
                } else {
                    pruned.push((row.path.clone(), row.kind.clone()));
                }
            }
            _ => {
                // Manual 'unknown' marks: classified in memory, always content
                // confirmed (both modes — cheap, marks are few).
                if !verify {
                    // `--cached` consumes the snapshot only (documented "no
                    // worktree walk"). A manual mark has no recorded kind, so
                    // the conservative reading is "still dirty" — never a
                    // stat/hash that could block on the worktree.
                    unstaged.modified.push(native);
                    continue;
                }
                match classify_manual_mark(&index, &workdir, &row.path) {
                    ManualMarkClass::Dirty(dirty::KIND_NEW) => unstaged.new.push(native),
                    ManualMarkClass::Dirty(dirty::KIND_DELETED) => unstaged.deleted.push(native),
                    ManualMarkClass::Dirty(_) => unstaged.modified.push(native),
                    ManualMarkClass::Blocked(reason) => {
                        // Cannot inspect: keep the mark and surface it as a
                        // blocked path, never prune and never invent a
                        // deletion. Text formats fail closed downstream;
                        // `--json` reports it in `data.io_blocked[]`.
                        blocked_paths.push((native.clone(), reason));
                        unstaged.modified.push(native);
                    }
                    ManualMarkClass::Clean => {
                        if verify {
                            pruned.push((row.path.clone(), row.kind.clone()));
                        }
                        // --cached: clean manual marks are dropped from the
                        // VIEW but kept in the cache (read-only fast path).
                    }
                }
            }
        }
    }
    let checked = rows.len();
    // Test-only fault-injection seam (debug builds only, runtime-gated on
    // LIBRA_TEST like the rest of the seam family): widen the read→re-verify
    // window so the mid-read concurrent-invalidate branch can be triggered
    // deterministically. Compiled out of release binaries.
    #[cfg(debug_assertions)]
    if std::env::var_os("LIBRA_TEST").is_some_and(|v| v == "1")
        && let Some(ms) = std::env::var("LIBRA_TEST_CACHE_READ_PAUSE_MS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
    {
        tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    }
    // Re-verify the epoch AFTER processing: a concurrent index/HEAD change
    // since the initial classify would make this view (and any prune) stale —
    // degrade instead of committing or rendering it as fresh.
    let fingerprint_now =
        current_index_fingerprint(&index_path).map_err(|e| dirty_cache_error("fingerprint", e))?;
    let head_now = Head::current_commit().await.map(|oid| oid.to_string());
    if fingerprint_now != fingerprint || head_now != head_oid {
        let mut data = collect_status_data(
            args,
            extras,
            &InvocationWarningCtx::from_process_preflight(),
        )
        .await?;
        data.warnings.push(cache_warning(
            StatusWarningCode::DirtyCacheConcurrentInvalidate,
            "the index or HEAD changed while reading the dirty cache; falling back to the full status",
        ));
        if output.is_json() {
            let mut json_data = build_status_json(&data, args);
            json_data["mode"] =
                serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
            json_data["freshness"] = serde_json::json!("full");
            json_data["cache_state"] = serde_json::json!("stale");
            if let Err(error) = emit_json_data("status", &json_data, output) {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
                return Err(error);
            }
        } else {
            // Same fail-closed guard as every other text path (§B.6.0.1):
            // the concurrent-invalidate fallback also runs a full scan, so
            // it can discover blocked paths and must not print a partial
            // body or exit 0 under `--quiet`.
            fail_closed_on_io_blocked(&data, output).inspect_err(|error| {
                if !error.is_silent() {
                    deliver_warnings_stderr(&data.warnings);
                }
            })?;
            // Deliver AFTER the body: EPIPE mid-render stays fully silent
            // (P0-06), while a real render failure still surfaces the
            // warning before propagating.
            if !output.quiet {
                let mut stdout = std::io::stdout();
                if let Err(error) = render_status_to_writer(&data, args, output, &mut stdout).await
                {
                    if !error.is_silent() {
                        deliver_warnings_stderr(&data.warnings);
                    }
                    return Err(error);
                }
            }
            deliver_warnings_stderr(&data.warnings);
        }
        StatusOutcome::new(&data, args).resolve(output)?;
        return Ok(());
    }
    // §B.6.0.1: a re-verification that could not inspect every row must not
    // persist its partial view — pruning on an incomplete pass is how a
    // still-dirty path silently disappears from the cache.
    if args.check_dirty && !blocked_paths.is_empty() {
        pruned.clear();
        confirmed.clear();
    }
    if args.check_dirty && (!pruned.is_empty() || !confirmed.is_empty()) {
        let txn = db
            .begin()
            .await
            .map_err(|e| dirty_cache_error("open a transaction for", anyhow::anyhow!(e)))?;
        DirtyCache::prune_and_confirm_with_conn(&txn, &pruned, &confirmed)
            .await
            .map_err(|e| dirty_cache_error("update", e))?;
        txn.commit()
            .await
            .map_err(|e| dirty_cache_error("commit", anyhow::anyhow!(e)))?;
    }

    // Assemble display data: cheap fresh pieces (head/upstream/merge state),
    // cache-derived changes (cwd-relative for display), NO rename detection
    // (would need object loads; documented) and no worktree walk.
    // Result-returning, like the full-scan path: a corrupt HEAD row (or an
    // OID of the wrong hash algorithm) must surface as an actionable error,
    // not as a process panic from the lossy `Head::current()` wrapper.
    let head = Head::current_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD", error))?;
    let head_oid_hash = Head::current_commit_result()
        .await
        .map_err(|error| status_branch_store_error("resolve HEAD commit", error))?;
    let staged = staged.to_relative();
    let mut unstaged = unstaged.to_relative();
    // Honor the resolved display defaults exactly like the full status
    // (P1-05d): `status.showUntrackedFiles=no`/`-uno` hides untracked
    // entries (the cache stores explicit paths, so `normal` and `all`
    // render identically here), and `--show-stash`/`status.showStash`
    // surfaces the stash count. `status.relativePaths=false` is applied by
    // the shared renderer.
    if args.untracked_files == Some(UntrackedFiles::No) {
        unstaged.new.clear();
    }
    let stash_count = if args.show_stash {
        Some(stash::get_stash_num().map_err(|detail| {
            CliError::fatal(format!("failed to read stash state for status: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("repair or remove the corrupt stash log, then retry")
        })?)
    } else {
        None
    };
    let upstream = resolve_upstream_info(&head, head_oid_hash.as_ref()).await?;
    let merge_state = match merge::MergeState::load_optional_sync().map_err(|detail| {
        CliError::fatal(format!("failed to inspect merge state: {detail}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        Some(state) => {
            let conflicted_paths =
                merge::unresolved_conflicted_paths(&index, &state.conflicted_paths);
            Some(MergeStatusInfo {
                target_ref: state.target_ref.clone(),
                unresolved_count: conflicted_paths.len(),
                conflicted_paths,
            })
        }
        None => None,
    };
    let mut data = StatusData {
        head,
        has_commits: head_oid_hash.is_some(),
        head_oid: head_oid_hash,
        staged,
        unstaged,
        unmerged: vec![],
        ignored_files: vec![],
        stash_count,
        upstream,
        merge_state,
        sequence_notice: sequence_notice().await?,
        sparse_view_active: crate::internal::sparse::SparseView::load(
            &crate::internal::worktree_scope::WorktreeScope::for_request(),
        )
        .await
        .is_active(),
        porcelain_v2: None,
        staged_rename_details: RenameDetails::new(),
        unstaged_rename_details: RenameDetails::new(),
        // One `worktree_*` warning per blocked row, exactly like the full
        // scan: `io_blocked[]` and `warnings[]` are a documented 1:1 pairing,
        // and `--exit-code-on-warning` reads the warning list, so an empty
        // one here would silently downgrade exit 9 to exit 1.
        warnings: {
            // Cache mode is a CLI-only path, so its invocation context is
            // the process preflight buffer — bound once here rather than read
            // at each use.
            let cache_warning_ctx = InvocationWarningCtx::from_process_preflight();
            let mut preflight: Vec<StatusWarning> = cache_warning_ctx
                .preflight_messages()
                .iter()
                .map(|message| StatusWarning {
                    code: StatusWarningCode::RepositoryPreflight,
                    message: message.clone(),
                    source: StatusWarningCode::RepositoryPreflight.source(),
                })
                .collect();
            let mut seen: HashSet<PathBuf> = HashSet::new();
            let mut sorted = blocked_paths.clone();
            sorted.sort_by(|a, b| a.0.cmp(&b.0));
            sorted
                .into_iter()
                .filter(|(path, _)| seen.insert(path.clone()))
                .map(|(path, reason)| {
                    let (text, code) = io_blocked_reason_and_code(reason);
                    StatusWarning {
                        code,
                        message: format!(
                            "cannot inspect '{}': {text}",
                            quote_pathname(&path, extras.quote_path)
                        ),
                        source: code.source(),
                    }
                })
                .collect::<Vec<_>>()
                .into_iter()
                .chain(preflight.drain(..))
                .collect()
        },
        quote_path: extras.quote_path,
        io_blocked: {
            let mut events: Vec<_> = blocked_paths
                .iter()
                .map(
                    |(path, reason)| crate::command::status_probe::IoBlockedEvent {
                        path: path.clone(),
                        reason: *reason,
                        absorbed: false,
                    },
                )
                .collect();
            events.sort_by_key(|event| raw_path_sort_key(&event.path));
            events.dedup_by(|a, b| a.path == b.path);
            events
        },
        // A re-verification that could not inspect every row is NOT a
        // complete scan: reporting `base_scan_complete: true` alongside a
        // non-empty `io_blocked[]` tells automation the cached answer is
        // authoritative when it demonstrably is not.
        base_scan_blocked: !blocked_paths.is_empty(),
        rename_scan_blocked: false,
    };
    filter_status_data_by_pathspec(&mut data, args)?;

    if output.is_json() {
        let mut json_data = build_status_json(&data, args);
        json_data["mode"] = serde_json::json!(if args.cached { "cached" } else { "check_dirty" });
        json_data["freshness"] = serde_json::json!("cached");
        json_data["cache_state"] = serde_json::json!("fresh");
        json_data["cached_paths"] = serde_json::json!(checked);
        if args.check_dirty {
            json_data["checked_paths"] = serde_json::json!(checked);
            json_data["stale_paths"] = serde_json::json!(
                pruned
                    .iter()
                    .map(|(path, _)| path.clone())
                    .collect::<Vec<_>>()
            );
        }
        // A non-EPIPE stdout failure must not swallow the collected
        // warnings: JSON is their only channel, so they fall back to stderr
        // rather than vanishing with the envelope.
        if let Err(error) = emit_json_data("status", &json_data, output) {
            if !error.is_silent() {
                deliver_warnings_stderr(&data.warnings);
            }
            return Err(error);
        }
    } else {
        deliver_warnings_stderr(&data.warnings);
        fail_closed_on_io_blocked(&data, output)?;
        if !output.quiet {
            render_cached_status_body(&data, args, output, checked, pruned.len()).await?;
        }
    }
    StatusOutcome::new(&data, args).resolve(output)?;
    Ok(())
}

async fn render_cached_status_body(
    data: &StatusData,
    args: &StatusArgs,
    output: &OutputConfig,
    checked: usize,
    pruned: usize,
) -> CliResult<()> {
    {
        use std::io::Write;
        let mut stdout = std::io::stdout();
        render_status_to_writer(data, args, output, &mut stdout).await?;
        if args.check_dirty {
            // Fallible write: a bare println! would panic on stdout EPIPE.
            writeln!(
                stdout,
                "dirty cache re-verified ({checked} checked, {pruned} pruned)"
            )
            .map_err(|error| {
                crate::utils::output::stdout_write_error("write the check-dirty summary", error)
            })?;
        }
    }
    Ok(())
}

/// Legacy entry point that writes status to the given writer.
/// Used by the old `execute()` path and tests.
pub async fn execute_to(mut args: StatusArgs, writer: &mut impl Write) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let extras = apply_status_config_defaults(&mut args).await?;
    execute_to_resolved(ResolvedStatusArgs { args, extras }, writer).await
}

/// Collect and render status from arguments whose config defaults were already
/// resolved by [`resolve_config_defaults`]. This avoids a second, potentially
/// inconsistent config read after an embedded caller has crossed a side-effect
/// boundary.
pub(crate) async fn execute_to_resolved(
    resolved: ResolvedStatusArgs,
    writer: &mut impl Write,
) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    // Exactly one config read: the bundle carries the resolution performed
    // by `resolve_config_defaults` across the side-effect boundary, so no
    // second (potentially inconsistent) read ever happens here.
    let ResolvedStatusArgs { args, extras } = resolved;
    // This writer entry renders a plain full status. It does NOT implement
    // the dirty-cache modes or the exit arbitration (it returns `()`, not an
    // exit code), so silently ignoring those options would hand an embedder
    // an ordinary status while it believed it had asked for a cache read or
    // a dirty exit code. Refuse instead — a caller that needs them must go
    // through `execute_safe_with_resolution`.
    let unsupported: &[(&str, bool)] = &[
        ("--scan", args.scan),
        ("--cached", args.cached),
        ("--check-dirty", args.check_dirty),
        ("--exit-code", args.exit_code),
    ];
    if let Some((flag, _)) = unsupported.iter().find(|(_, set)| *set) {
        return Err(CliError::command_usage(format!(
            "'{flag}' is not supported by the status writer entry point"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)
        .with_hint("run the status command itself; this entry renders a plain full status"));
    }
    let data = collect_status_data(
        &args,
        extras,
        &InvocationWarningCtx::from_process_preflight(),
    )
    .await?;
    deliver_warnings_stderr(&data.warnings);
    let output = OutputConfig::default();
    fail_closed_on_io_blocked(&data, &output)?;
    render_status_to_writer(&data, &args, &output, writer).await
}

// ---------------------------------------------------------------------------
// Rendering dispatcher
// ---------------------------------------------------------------------------

/// §B.3.3/§B.6.0.1 delivery matrix: human/short/porcelain fail CLOSED on any
/// I/O-blocked path — no partial dirty/porcelain body is ever printed. Only
/// JSON/API may report the partial result (`io_blocked[]`).
///
/// This lives OUTSIDE the renderer because `--quiet` skips rendering: routing
/// the guard through the writer would let `libra --quiet status` exit 0 on a
/// repository it could not fully inspect, which is precisely the silent
/// "looks clean" answer the contract exists to prevent.
/// Collapse a SORTED `io_blocked[]` to one entry per path.
///
/// A path survives as `absorbed` only if EVERY report of it was absorbed.
/// The same unreadable directory can be compensated for by one consumer and
/// not another — an untracked scan emits its `?? dir/` marker regardless,
/// while rename detection genuinely could not see what was inside it. A
/// plain dedup keeps whichever event happened to be recorded first, so an
/// absorbed report could silently downgrade a real omission and stop the
/// command failing closed.
fn collapse_io_blocked_by_path(events: &mut Vec<crate::command::status_probe::IoBlockedEvent>) {
    let mut collapsed: Vec<crate::command::status_probe::IoBlockedEvent> =
        Vec::with_capacity(events.len());
    for event in events.drain(..) {
        match collapsed.last_mut() {
            Some(last) if last.path == event.path => last.absorbed &= event.absorbed,
            _ => collapsed.push(event),
        }
    }
    *events = collapsed;
}

fn fail_closed_on_io_blocked(data: &StatusData, output: &OutputConfig) -> CliResult<()> {
    if output.is_json() {
        return Ok(());
    }
    // §B.6.0.1: in text modes ANY blocked path is fatal — including an
    // absorbed one. An unreadable untracked directory gets its `?? dir/`
    // marker emitted (over-reporting, the safe direction), but a marker is
    // not an inspection result, so the command still fails closed rather
    // than claim a complete status.
    let Some(first) = data.io_blocked.first() else {
        return Ok(());
    };
    let count = data.io_blocked.len();
    Err(CliError::fatal(format!(
        "cannot inspect '{}' ({count} path(s) blocked); status output would be incomplete",
        quote_pathname(&first.path, data.quote_path),
    ))
    .with_stable_code(StableErrorCode::IoReadFailed)
    .with_hint("fix the unreadable path permissions and retry")
    .with_hint("use --json to inspect the partial result with data.io_blocked[]"))
}

async fn render_status_to_writer(
    data: &StatusData,
    args: &StatusArgs,
    output: &OutputConfig,
    writer: &mut impl Write,
) -> CliResult<()> {
    fail_closed_on_io_blocked(data, output)?;
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);
    let mut buffer = Vec::new();

    // §B.6.4 machine-format path base: porcelain v1/v2 ALWAYS emit
    // repository-root-relative paths (Git parity), while the human
    // formats honor `status.relativePaths`. The collected data carries
    // the display base, so project it back for the porcelain renderers.
    let porcelain_data;
    let data = if args.porcelain.is_some() {
        porcelain_data = data.to_repo_relative();
        &porcelain_data
    } else {
        data
    };

    // Porcelain modes
    match args.porcelain {
        Some(PorcelainVersion::V2) => {
            if args.branch {
                write_branch_info_v2(
                    &data.head,
                    data.head_oid.as_ref(),
                    data.upstream.as_ref(),
                    args.show_ahead_behind(),
                    args.null_terminated,
                    &mut buffer,
                )?;
            }
            output_porcelain_v2(
                &data.staged,
                &data.unstaged,
                &data.unmerged,
                &data.ignored_files,
                data.porcelain_v2.as_deref(),
                &data.staged_rename_details,
                &data.unstaged_rename_details,
                args.null_terminated,
                data.quote_path,
                &mut buffer,
            )?;
            writer.write_all(&buffer).map_err(write_error)?;
            return Ok(());
        }
        Some(PorcelainVersion::V1) => {
            if args.branch {
                print_branch_info(
                    &data.head,
                    data.upstream.as_ref(),
                    args.show_ahead_behind(),
                    args.null_terminated,
                    &mut buffer,
                )?;
            }
            output_porcelain_with_unmerged(
                &data.staged,
                &data.unstaged,
                &data.unmerged,
                args.null_terminated,
                data.quote_path,
                &mut buffer,
            )?;
            if args.ignored && !data.ignored_files.is_empty() {
                for file in &data.ignored_files {
                    if args.null_terminated {
                        write!(&mut buffer, "!! ").map_err(write_error)?;
                        write_raw_path(&mut buffer, file).map_err(write_error)?;
                        buffer.push(b'\0');
                    } else {
                        buffer.extend_from_slice(b"!! ");
                        buffer.extend_from_slice(&quote_pathname_bytes(file, data.quote_path));
                        buffer.push(b'\n');
                    }
                }
            }
            writer.write_all(&buffer).map_err(write_error)?;
            return Ok(());
        }
        None => {}
    };

    // `status.relativePaths=false`: Git renders the HUMAN formats (short and
    // long) with repository-root-relative paths. Collection stays cwd-relative
    // throughout (pathspec filtering and porcelain metadata lookups depend on
    // it); only the rendered copy is converted here. Porcelain/JSON output is
    // reached before this point and keeps its existing path shape.
    let rooted_data;
    let data = if args.relative_paths {
        data
    } else {
        rooted_data = data_with_repo_root_paths(data);
        &rooted_data
    };

    // Short format
    if args.short {
        if args.branch {
            print_branch_info(
                &data.head,
                data.upstream.as_ref(),
                args.show_ahead_behind(),
                args.null_terminated,
                &mut buffer,
            )?;
        }
        output_short_format_with_config(
            &data.staged,
            &data.unstaged,
            &data.unmerged,
            output,
            args.null_terminated,
            data.quote_path,
            &mut buffer,
        )
        .await?;
        if args.ignored {
            for file in &data.ignored_files {
                if args.null_terminated {
                    write!(&mut buffer, "!! ").map_err(write_error)?;
                    write_raw_path(&mut buffer, file).map_err(write_error)?;
                    buffer.push(b'\0');
                } else {
                    buffer.extend_from_slice(b"!! ");
                    buffer.extend_from_slice(&quote_pathname_bytes(file, data.quote_path));
                    buffer.push(b'\n');
                }
            }
        }
        writer.write_all(&buffer).map_err(write_error)?;
        return Ok(());
    }

    // Standard human format
    render_human_status(data, args, &mut buffer)?;
    writer.write_all(&buffer).map_err(write_error)?;
    Ok(())
}

/// Convert every display path in `data` from cwd-relative to
/// repository-root-relative (`status.relativePaths=false`). Rename pairs,
/// unmerged entries, and ignored paths are converted alongside the staged and
/// unstaged change sets.
fn data_with_repo_root_paths(data: &StatusData) -> StatusData {
    // Collapsed untracked/ignored directories carry a deliberate trailing
    // `/` marker (see `status_untracked`); path conversion must not eat it,
    // or directories become indistinguishable from files in the output.
    fn convert(path: &Path) -> PathBuf {
        with_dir_marker(path, util::to_workdir_path(path))
    }
    fn changes(changes: &Changes) -> Changes {
        Changes {
            new: changes.new.iter().map(|p| convert(p)).collect(),
            modified: changes.modified.iter().map(|p| convert(p)).collect(),
            deleted: changes.deleted.iter().map(|p| convert(p)).collect(),
            renamed: changes
                .renamed
                .iter()
                .map(|(from, to)| (convert(from), convert(to)))
                .collect(),
        }
    }
    fn details(details: &RenameDetails) -> RenameDetails {
        details
            .iter()
            .map(|((from, to), value)| ((convert(from), convert(to)), *value))
            .collect()
    }
    let mut rooted = data.clone();
    rooted.staged = changes(&data.staged);
    rooted.unstaged = changes(&data.unstaged);
    // Keep the score/exactness lookup keys aligned with the converted rename
    // pairs, or JSON emission from a subdirectory would miss every detail.
    rooted.staged_rename_details = details(&data.staged_rename_details);
    rooted.unstaged_rename_details = details(&data.unstaged_rename_details);
    rooted.unmerged = data
        .unmerged
        .iter()
        .map(|entry| entry.clone().with_path(convert(&entry.path)))
        .collect();
    rooted.ignored_files = data.ignored_files.iter().map(|p| convert(p)).collect();
    rooted
}

// ---------------------------------------------------------------------------
// Human standard format
// ---------------------------------------------------------------------------

fn render_human_status(
    data: &StatusData,
    args: &StatusArgs,
    buffer: &mut Vec<u8>,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

    // Branch header
    match &data.head {
        Head::Detached(commit_hash) => {
            writeln!(buffer, "HEAD detached at {}", &commit_hash.to_string()[..8])
                .map_err(write_error)?;
        }
        Head::Branch(branch) => {
            writeln!(buffer, "On branch {branch}").map_err(write_error)?;
        }
    }

    // Upstream tracking info
    if let Some(upstream) = &data.upstream {
        render_upstream_human(upstream, buffer)?;
    }

    if let Some(notice) = &data.sequence_notice {
        writeln!(buffer, "{notice}").map_err(write_error)?;
    }
    if data.sparse_view_active {
        writeln!(
            buffer,
            "note: a sparse view is active (scopes 'ls-files'/'diff' output; status is not filtered)"
        )
        .map_err(write_error)?;
    }
    if let Some(merge_state) = &data.merge_state {
        render_merge_state_human(merge_state, buffer)?;
    }

    if !data.has_commits {
        writeln!(buffer, "\nNo commits yet\n").map_err(write_error)?;
    }

    // Stash info
    if let Some(stash_count) = data.stash_count
        && stash_count > 0
    {
        let entry_text = if stash_count == 1 { "entry" } else { "entries" };
        writeln!(
            buffer,
            "Your stash currently has {stash_count} {entry_text}"
        )
        .map_err(write_error)?;
    }

    // Clean tree
    if data.merge_state.is_none()
        && data.staged.is_empty()
        && data.unstaged.is_empty()
        && data.unmerged.is_empty()
    {
        writeln!(buffer, "nothing to commit, working tree clean").map_err(write_error)?;
        return Ok(());
    }

    // Staged changes
    if !data.staged.is_empty() {
        writeln!(buffer, "Changes to be committed:").map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra restore --staged <file>...\" to unstage"
        )
        .map_err(write_error)?;
        let entries = build_human_entries(
            &data.staged.deleted,
            "deleted:",
            &data.staged.modified,
            "modified:",
            &data.staged.new,
            "new file:",
            &data.staged.renamed,
            "renamed:",
            data.quote_path,
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightGreen)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightGreen.to_fg_str(), &line);
            }
        }
    }

    // Unstaged changes (modified + deleted + renamed — a probe-paired
    // unstaged rename can be the section's ONLY content, §B.3.1)
    if !data.unstaged.deleted.is_empty()
        || !data.unstaged.modified.is_empty()
        || !data.unstaged.renamed.is_empty()
    {
        writeln!(buffer, "Changes not staged for commit:").map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra add <file>...\" to update what will be committed"
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra restore <file>...\" to discard changes in working directory"
        )
        .map_err(write_error)?;
        let entries = build_human_entries(
            &data.unstaged.deleted,
            "deleted:",
            &data.unstaged.modified,
            "modified:",
            &[],
            "",
            &data.unstaged.renamed,
            "renamed:",
            data.quote_path,
        );
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    if !data.unmerged.is_empty() {
        writeln!(buffer, "Unmerged paths:").map_err(write_error)?;
        writeln!(buffer, "  use \"libra add <file>...\" to mark resolution")
            .map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra merge --abort\" or the active sequencer abort command to abort"
        )
        .map_err(write_error)?;
        let entries = data
            .unmerged
            .iter()
            .map(|entry| {
                (
                    unmerged_human_label(entry),
                    quote_pathname_bytes(&entry.path, data.quote_path),
                )
            })
            .collect::<Vec<_>>();
        if args.column {
            render_columnated_labeled_entries(buffer, &entries, colored::Color::BrightRed)?;
        } else {
            for (label, path) in entries {
                let mut line = format!("\t{label} ").into_bytes();
                line.extend_from_slice(&path);
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    // Untracked
    if !data.unstaged.new.is_empty() {
        writeln!(buffer, "Untracked files:").map_err(write_error)?;
        writeln!(
            buffer,
            "  use \"libra add <file>...\" to include in what will be committed"
        )
        .map_err(write_error)?;
        if args.column {
            render_columnated_paths(buffer, &data.unstaged.new, data.quote_path)?;
        } else {
            for f in &data.unstaged.new {
                let mut line = b"\t".to_vec();
                line.extend_from_slice(&quote_pathname_bytes(f, data.quote_path));
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    // Ignored
    if args.ignored && !data.ignored_files.is_empty() {
        writeln!(buffer, "Ignored files:").map_err(write_error)?;
        writeln!(
            buffer,
            "  (modify .libraignore to change which files are ignored)"
        )
        .map_err(write_error)?;
        if args.column {
            render_columnated_paths(buffer, &data.ignored_files, data.quote_path)?;
        } else {
            for f in &data.ignored_files {
                let mut line = b"\t".to_vec();
                line.extend_from_slice(&quote_pathname_bytes(f, data.quote_path));
                push_colored_line(buffer, &colored::Color::BrightRed.to_fg_str(), &line);
            }
        }
    }

    Ok(())
}

fn unmerged_human_label(entry: &UnmergedEntry) -> &'static str {
    match entry.xy() {
        ('D', 'D') => "both deleted:",
        ('A', 'U') => "added by us:",
        ('U', 'D') => "deleted by them:",
        ('U', 'A') => "added by them:",
        ('D', 'U') => "deleted by us:",
        ('A', 'A') => "both added:",
        _ => "both modified:",
    }
}

/// Build a flat list of (label, path) for human output.
#[allow(clippy::too_many_arguments)]
fn build_human_entries<'a>(
    deleted: &[PathBuf],
    deleted_label: &'a str,
    modified: &[PathBuf],
    modified_label: &'a str,
    new_files: &[PathBuf],
    new_label: &'a str,
    renamed: &[(PathBuf, PathBuf)],
    renamed_label: &'a str,
    quote_path: bool,
) -> Vec<(&'a str, Vec<u8>)> {
    let mut entries = Vec::new();
    for f in deleted {
        entries.push((deleted_label, quote_pathname_bytes(f, quote_path)));
    }
    for f in modified {
        entries.push((modified_label, quote_pathname_bytes(f, quote_path)));
    }
    for (old, new) in renamed {
        let mut line = quote_pathname_bytes(old, quote_path);
        line.extend_from_slice(b" -> ");
        line.extend_from_slice(&quote_pathname_bytes(new, quote_path));
        entries.push((renamed_label, line));
    }
    for f in new_files {
        entries.push((new_label, quote_pathname_bytes(f, quote_path)));
    }
    entries
}

/// Render labeled entries in aligned columns.
fn render_columnated_labeled_entries(
    buffer: &mut Vec<u8>,
    entries: &[(&str, Vec<u8>)],
    color: colored::Color,
) -> CliResult<()> {
    if entries.is_empty() {
        return Ok(());
    }
    let max_label_width = entries.iter().map(|(l, _)| l.len()).max().unwrap_or(0);
    for (label, path) in entries {
        let mut line = format!("\t{label:max_label_width$} ").into_bytes();
        line.extend_from_slice(path);
        push_colored_line(buffer, &color.to_fg_str(), &line);
    }
    Ok(())
}

/// Wrap one content line in an ANSI fg color + reset and terminate it,
/// honoring the colored crate's own colorize gate (so piped/test output
/// stays plain, exactly like the `.bright_green()` call sites it
/// replaces). The colored crate's API is `String`-only, so byte-faithful
/// paths (raw non-UTF-8 bytes under `core.quotePath=false`) are colored
/// manually with the same codes the crate emits.
fn push_colored_line(buffer: &mut Vec<u8>, fg: &str, line: &[u8]) {
    let colorize = colored::control::SHOULD_COLORIZE.should_colorize();
    if colorize {
        buffer.extend_from_slice(b"\x1b[");
        buffer.extend_from_slice(fg.as_bytes());
        buffer.extend_from_slice(b"m");
    }
    buffer.extend_from_slice(line);
    if colorize {
        buffer.extend_from_slice(b"\x1b[0m");
    }
    buffer.push(b'\n');
}

/// Render plain paths in multiple columns like `ls`.
fn render_columnated_paths(
    buffer: &mut Vec<u8>,
    paths: &[PathBuf],
    quote_path: bool,
) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);
    if paths.is_empty() {
        return Ok(());
    }

    let names: Vec<Vec<u8>> = paths
        .iter()
        .map(|p| quote_pathname_bytes(p, quote_path))
        .collect();
    let widths: Vec<usize> = names.iter().map(|n| n.len()).collect();
    let max_width = *widths.iter().max().unwrap_or(&0);
    let term_width = terminal_width().unwrap_or(80);
    // Leave a leading tab and some padding room.
    let usable_width = term_width.saturating_sub(8);
    let col_width = max_width + 2;
    let num_cols = usable_width
        .checked_div(col_width)
        .unwrap_or(usable_width)
        .max(1);
    let num_rows = names.len().div_ceil(num_cols);

    for row in 0..num_rows {
        write!(buffer, "\t").map_err(write_error)?;
        for col in 0..num_cols {
            let idx = col * num_rows + row;
            if idx >= names.len() {
                break;
            }
            let name = &names[idx];
            buffer.extend_from_slice(name);
            if col + 1 < num_cols {
                for _ in name.len()..col_width {
                    buffer.push(b' ');
                }
            }
        }
        writeln!(buffer).map_err(write_error)?;
    }
    Ok(())
}

/// Best-effort terminal width.
fn terminal_width() -> Option<usize> {
    if std::io::stdout().is_terminal() {
        std::env::var("COLUMNS")
            .ok()
            .and_then(|s| s.parse().ok())
            .or(Some(80))
    } else {
        None
    }
}

fn render_merge_state_human(merge_state: &MergeStatusInfo, buffer: &mut Vec<u8>) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

    writeln!(
        buffer,
        "You are in the middle of a merge with '{}'.",
        merge_state.target_ref
    )
    .map_err(write_error)?;
    if merge_state.unresolved_count == 0 {
        writeln!(
            buffer,
            "  (all conflicts fixed: run \"libra merge --continue\")"
        )
        .map_err(write_error)?;
    } else if merge_state.conflicted_paths.is_empty() {
        writeln!(
            buffer,
            "  (conflicts remain outside the selected pathspec; run \"libra status\" to see them)"
        )
        .map_err(write_error)?;
    } else {
        writeln!(
            buffer,
            "  (fix conflicts and run \"libra merge --continue\")"
        )
        .map_err(write_error)?;
    }
    writeln!(buffer, "  (use \"libra merge --abort\" to abort the merge)").map_err(write_error)?;
    Ok(())
}

fn render_upstream_human(upstream: &UpstreamInfo, buffer: &mut Vec<u8>) -> CliResult<()> {
    let write_error =
        |err: io::Error| crate::utils::output::stdout_write_error("write status output", err);

    if upstream.gone {
        writeln!(
            buffer,
            "Your branch is based on '{}', but the upstream is gone.",
            upstream.remote_ref
        )
        .map_err(write_error)?;
        return Ok(());
    }

    // ahead/behind are None on an unborn branch (no local commit to compare).
    let (ahead, behind) = match (upstream.ahead, upstream.behind) {
        (Some(a), Some(b)) => (a, b),
        _ => {
            // Unborn branch: upstream exists but no local commits yet.
            return Ok(());
        }
    };

    if ahead == 0 && behind == 0 {
        writeln!(
            buffer,
            "Your branch is up to date with '{}'.",
            upstream.remote_ref
        )
        .map_err(write_error)?;
    } else if ahead > 0 && behind == 0 {
        writeln!(
            buffer,
            "Your branch is ahead of '{}' by {} commit{}.",
            upstream.remote_ref,
            ahead,
            if ahead == 1 { "" } else { "s" }
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "  (use \"libra push\" to publish your local commits)"
        )
        .map_err(write_error)?;
    } else if ahead == 0 && behind > 0 {
        writeln!(
            buffer,
            "Your branch is behind '{}' by {} commit{}.",
            upstream.remote_ref,
            behind,
            if behind == 1 { "" } else { "s" }
        )
        .map_err(write_error)?;
        writeln!(buffer, "  (use \"libra pull\" to update your local branch)")
            .map_err(write_error)?;
    } else {
        writeln!(
            buffer,
            "Your branch and '{}' have diverged,",
            upstream.remote_ref
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "and have {ahead} and {behind} different commits each, respectively."
        )
        .map_err(write_error)?;
        writeln!(
            buffer,
            "  (use \"libra pull\" to merge the remote branch into yours)"
        )
        .map_err(write_error)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// JSON rendering
// ---------------------------------------------------------------------------

/// Render collected warnings to stderr (`warning: …`) and mark the global
/// warning tracker — human/short/porcelain delivery only (§B.5 matrix).
/// JSON callers must NOT use this: their warnings ride in `data.warnings[]`.
fn deliver_warnings_stderr(warnings: &[StatusWarning]) {
    // In TEXT mode RepositoryPreflight entries were already printed live by
    // `emit_warning` when the preflight raised them (with structured output
    // inactive it prints immediately); they join the structured list only so
    // JSON payloads and §B.5 exit arbitration see them, and re-printing them
    // here doubled the stderr line. Under a structured envelope nothing was
    // printed live (`emit_warning` only buffers), so the JSON output-failure
    // fallback that calls this MUST deliver them or they vanish with the
    // envelope — the skip is therefore conditional on live delivery having
    // happened.
    let preflight_already_live = !crate::utils::output::structured_output_active();
    let mut delivered_any = false;
    for warning in warnings {
        if preflight_already_live && matches!(warning.code, StatusWarningCode::RepositoryPreflight)
        {
            continue;
        }
        eprintln!("warning: {}", warning.message);
        delivered_any = true;
    }
    if delivered_any {
        crate::utils::output::record_warning();
    }
}

/// Build a `source = cache` structured warning (§B.5 R0-8b).
fn cache_warning(code: StatusWarningCode, message: impl Into<String>) -> StatusWarning {
    StatusWarning {
        code,
        message: message.into(),
        source: code.source(),
    }
}

/// The single §B.5 exit arbitration point.
///
/// Every status path — full scan, `--scan`, both cache modes, and both
/// fallbacks — resolves its exit code here instead of repeating the
/// comparison. That matters because the ordering is subtle and one branch
/// getting it wrong is invisible in review: an early `silent_exit(1)` for a
/// dirty tree would preempt the warning exit 9 that is supposed to outrank
/// it. Priority: **fatal ≻ 9 (`--exit-code-on-warning`) ≻ 1 (dirty,
/// including a non-empty `io_blocked`) ≻ 0**. Fatal is raised earlier by
/// `fail_closed_on_io_blocked`, so this resolver covers 9 ≻ 1 ≻ 0.
struct StatusOutcome<'a> {
    data: &'a StatusData,
    args: &'a StatusArgs,
}

impl<'a> StatusOutcome<'a> {
    fn new(data: &'a StatusData, args: &'a StatusArgs) -> Self {
        Self { data, args }
    }

    fn resolve(&self, output: &OutputConfig) -> CliResult<()> {
        if let Some(exit) = warning_exit(output, &self.data.warnings) {
            return Err(exit);
        }
        // `--exit-code`: dirty → exit 1, silently (no error line).
        if self.args.exit_code && self.data.is_dirty() {
            return Err(CliError::silent_exit(1));
        }
        Ok(())
    }
}

/// §B.5 exit arbitration, rule 2: warnings + `--exit-code-on-warning` exit 9
/// and take precedence over the dirty exit 1. Local to each return point
/// because an early `silent_exit(1)` would otherwise preempt the top-level
/// exit-9 pass in `cli.rs` (and JSON never records globally).
fn warning_exit(output: &OutputConfig, warnings: &[StatusWarning]) -> Option<CliError> {
    // Decided from THIS invocation's structured list only. Consulting the
    // process-global tracker would let a warning emitted by an earlier
    // embedded call flip a later, clean one to exit 9 with an empty
    // `warnings[]` — and the reverse leak, where a status call marks the
    // tracker for whoever runs next. Preflight advisories are folded into
    // the list at collection time, so nothing is lost by dropping the
    // global read.
    (output.exit_code_on_warning && !warnings.is_empty()).then(|| CliError::silent_exit(9))
}

/// Sort key for `io_blocked[]`: the RAW path encoding, matching what
/// `raw_base64` serializes. `PathBuf`'s own ordering compares WTF-8 bytes on
/// Windows, which disagrees with UTF-16 code-unit order once supplementary
/// characters are involved — so a machine consumer relying on the documented
/// "sorted by raw path bytes" would see a different order than it computed.
pub(crate) fn raw_path_sort_key(path: &Path) -> Vec<u8> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes().to_vec()
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        // BIG-endian in the SORT KEY only. The documented order is by UTF-16
        // code unit, and a bytewise comparison reproduces that only when the
        // high byte comes first: little-endian would put U+0101 (`01 01`)
        // after U+0200 (`00 02`), inverting the required order. The
        // published `raw_base64` stays little-endian — the key exists to be
        // compared, not to be transmitted.
        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_be_bytes());
        }
        bytes
    }
    #[cfg(not(any(unix, windows)))]
    path.to_string_lossy().into_owned().into_bytes()
}

/// Reversible encoding of a path whose name is not valid UTF-8, for the
/// `io_blocked[].path.raw_base64` contract (§B.6.0.1). Returns `None` for a
/// valid-UTF-8 name, whose `display` form is already lossless.
///
/// Unix encodes the raw `OsStr` bytes. Windows encodes the UTF-16 code units
/// LITTLE-ENDIAN, because an unpaired surrogate has no UTF-8 form at all —
/// returning `None` there would break reversibility for exactly the names
/// that need it.
pub fn raw_path_base64(path: &Path) -> Option<String> {
    use base64::Engine as _;

    if path.to_str().is_some() {
        return None;
    }
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        Some(base64::engine::general_purpose::STANDARD.encode(path.as_os_str().as_bytes()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let mut bytes = Vec::new();
        for unit in path.as_os_str().encode_wide() {
            bytes.extend_from_slice(&unit.to_le_bytes());
        }
        Some(base64::engine::general_purpose::STANDARD.encode(bytes))
    }
    #[cfg(not(any(unix, windows)))]
    None
}

/// A path as a JSON string, using the same escaping the docs promise for
/// display forms. Undecodable bytes become `\ooo` octal escapes rather than
/// `U+FFFD`, so distinct filenames stay distinct in the payload. The quoting
/// wrapper is stripped: JSON supplies its own quoting.
fn json_path_string(path: &Path, quote_path: bool) -> String {
    match path.to_str() {
        Some(text) => text.to_string(),
        None => {
            let quoted = quote_pathname(path, quote_path);
            quoted
                .strip_prefix('"')
                .and_then(|rest| rest.strip_suffix('"'))
                .map(str::to_string)
                .unwrap_or(quoted)
        }
    }
}

fn build_status_json(data: &StatusData, _args: &StatusArgs) -> serde_json::Value {
    // §B.5 delivery matrix: porcelain and JSON paths are ALWAYS
    // repository-root-relative, regardless of the invocation subdirectory.
    // Collection stays cwd-relative (pathspec filtering depends on it), so
    // the JSON payload converts here — identity when cwd is the repo root.
    let rooted = data_with_repo_root_paths(data);
    let data = &rooted;
    // Paths render through the SAME escaping the docs promise for display
    // forms. `Path::display()` replaces undecodable bytes with U+FFFD, so
    // two different real filenames could collapse to one JSON string —
    // silently merging distinct entries for every consumer.
    let quote = data.quote_path;
    let paths_to_json = move |paths: &[PathBuf]| -> Vec<serde_json::Value> {
        paths
            .iter()
            .map(|p| serde_json::Value::String(json_path_string(p, quote)))
            .collect()
    };

    let renamed_to_json = move |renamed: &[(PathBuf, PathBuf)]| -> Vec<serde_json::Value> {
        renamed
            .iter()
            .map(|(old, new)| {
                serde_json::json!({
                    "from": json_path_string(old, quote),
                    "to": json_path_string(new, quote),
                })
            })
            .collect()
    };

    // Top-level `renames[]` with score/exactness/side (§B.6.5), sorted by the
    // destination path for determinism.
    let mut renames: Vec<serde_json::Value> = Vec::new();
    let mut push_renames = |pairs: &[(PathBuf, PathBuf)], details: &RenameDetails, staged: bool| {
        for (old, new) in pairs {
            let (score, exact) = details
                .get(&(old.clone(), new.clone()))
                .copied()
                .unwrap_or((100, true));
            renames.push(serde_json::json!({
                "from": json_path_string(old, quote),
                "to": json_path_string(new, quote),
                "score": score,
                "exact": exact,
                "staged": staged,
                "unstaged": !staged,
            }));
        }
    };
    push_renames(&data.staged.renamed, &data.staged_rename_details, true);
    push_renames(&data.unstaged.renamed, &data.unstaged_rename_details, false);
    renames.sort_by(|a, b| a["to"].as_str().cmp(&b["to"].as_str()));

    // §B.6.0.1 io_blocked[] public contract: escaped repo-relative display
    // (same quoting as non-`-z` porcelain), lossless raw bytes for
    // non-UTF-8 paths, the KNOWN staged component only, the reason
    // taxonomy, and the staged rename pair when one is known. Sorted by raw
    // path, deduplicated. Every entry also emits a worktree-family warning.
    // Warnings already carry the worktree family from collection time
    // (§B.5 single arbitration source); JSON only serializes them.
    let warnings_json = data.warnings.clone();
    let mut io_blocked_json: Vec<serde_json::Value> = Vec::new();
    for event in &data.io_blocked {
        let display = quote_pathname(&event.path, data.quote_path);
        let raw_base64: serde_json::Value = match raw_path_base64(&event.path) {
            Some(encoded) => serde_json::Value::String(encoded),
            None => serde_json::Value::Null,
        };
        // Compare on REPO-RELATIVE keys: `event.path` is repo-relative
        // while the change lists carry the display base, so from a
        // subdirectory a display-base conversion of the event would never
        // match (the historical `staged`/`rename` = null bug).
        // The change lists may carry repo-relative OR display-base paths
        // depending on the caller; accept either spelling of the same file
        // so the schema fields stay correct from a subdirectory.
        let matches_event = |candidate: &PathBuf| -> bool {
            candidate == &event.path || current_to_workdir(candidate) == event.path
        };
        let staged_component = if data.staged.modified.iter().any(matches_event) {
            serde_json::json!("M")
        } else if data.staged.new.iter().any(matches_event) {
            serde_json::json!("A")
        } else if data.staged.deleted.iter().any(matches_event) {
            serde_json::json!("D")
        } else if data
            .staged
            .renamed
            .iter()
            .any(|(_, new)| matches_event(new))
        {
            serde_json::json!("R")
        } else {
            serde_json::Value::Null
        };
        let rename = data
            .staged
            .renamed
            .iter()
            .find(|(_, new)| matches_event(new))
            .map(|pair| {
                let score = data
                    .staged_rename_details
                    .get(pair)
                    .map(|(pct, _)| *pct)
                    .unwrap_or(100);
                // Lossless like every other JSON path in the payload —
                // `display()` would U+FFFD-corrupt a non-UTF-8 pair
                // (2026-08-06 R0-6 review; latent, since staged pairs
                // currently require addable UTF-8 index paths).
                serde_json::json!({
                    "from": json_path_string(&pair.0, quote),
                    "to": json_path_string(&pair.1, quote),
                    "score": score,
                })
            })
            .unwrap_or(serde_json::Value::Null);
        let (reason, _warning_code) = io_blocked_reason_and_code(event.reason);
        io_blocked_json.push(serde_json::json!({
            "path": { "display": display, "raw_base64": raw_base64 },
            "staged": staged_component,
            "reason": reason,
            "rename": rename,
        }));
    }
    // §B.6.0.1: rename detection is complete only when nothing degraded it —
    // no probe truncation/blocks and no engine skip/limit/budget warnings.
    let rename_detection_complete = !data.rename_scan_blocked
        && !data.warnings.iter().any(|w| {
            matches!(
                w.code,
                StatusWarningCode::ProbeTruncated
                    | StatusWarningCode::RenameLimitProductSkipped
                    | StatusWarningCode::SimilarityBudgetExceeded
                    | StatusWarningCode::MetadataUnavailable
                    | StatusWarningCode::MetadataBudgetExceeded
                    | StatusWarningCode::WorktreeBudgetExceeded
                    | StatusWarningCode::RenamePathEncodingUnsupported
            )
        });

    let head = match &data.head {
        Head::Branch(name) => serde_json::json!({"type": "branch", "name": name}),
        Head::Detached(hash) => {
            serde_json::json!({"type": "detached", "oid": hash.to_string()})
        }
    };

    let upstream_json = match &data.upstream {
        Some(u) => serde_json::json!({
            "remote_ref": u.remote_ref,
            "ahead": u.ahead,
            "behind": u.behind,
            "gone": u.gone,
        }),
        None => serde_json::Value::Null,
    };

    let mut json_data = serde_json::json!({
        "head": head,
        "has_commits": data.has_commits,
        "upstream": upstream_json,
        "staged": {
            "new": paths_to_json(&data.staged.new),
            "modified": paths_to_json(&data.staged.modified),
            "deleted": paths_to_json(&data.staged.deleted),
            "renamed": renamed_to_json(&data.staged.renamed),
        },
        "unstaged": {
            "modified": paths_to_json(&data.unstaged.modified),
            "deleted": paths_to_json(&data.unstaged.deleted),
            "renamed": renamed_to_json(&data.unstaged.renamed),
        },
        "unmerged": paths_to_json(
            &data
                .unmerged
                .iter()
                .map(|entry| entry.path.clone())
                .collect::<Vec<_>>()
        ),
        "untracked": paths_to_json(&data.unstaged.new),
        "ignored": paths_to_json(&data.ignored_files),
        "warnings": warnings_json,
        "renames": renames,
        "io_blocked": io_blocked_json,
        "base_scan_complete": !data.base_scan_blocked,
        "rename_detection_complete": rename_detection_complete,
        "complete": !data.base_scan_blocked && rename_detection_complete,
        "is_clean": !data.is_dirty(),
    });

    if let Some(merge_state) = &data.merge_state
        && let Some(map) = json_data.as_object_mut()
    {
        map.insert(
            "merge_state".to_string(),
            serde_json::json!({
                "target_ref": merge_state.target_ref,
                "conflicted_paths": merge_state.conflicted_paths,
            }),
        );
    }

    if let Some(stash_count) = data.stash_count
        && let Some(map) = json_data.as_object_mut()
    {
        map.insert("stash_entries".to_string(), serde_json::json!(stash_count));
    }

    json_data
}

// ---------------------------------------------------------------------------
// Porcelain v1
// ---------------------------------------------------------------------------

pub fn output_porcelain(
    staged: &Changes,
    unstaged: &Changes,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    output_porcelain_with_unmerged(staged, unstaged, &[], null_terminated, true, writer)
}

fn output_porcelain_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    // Renames render as a single `R  <old> -> <new>` record (Git porcelain v1
    // §B.6.3), never as two `R` endpoint rows. Under `-z` the record is
    // `XY SP <new> NUL <old> NUL` (raw path bytes, new before old, matching
    // Git); non-`-z` paths go through `quote_pathname` (§B.6.6).
    for entry in generate_short_status_entries_with_unmerged(staged, unstaged, unmerged) {
        match entry {
            ShortStatusEntry::Path {
                path,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &path).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
            ShortStatusEntry::Rename {
                old,
                new,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &new).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                    write_raw_path(writer, &old).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Porcelain v2
// ---------------------------------------------------------------------------

/// File information from HEAD tree for porcelain v2 output.
struct FileInfo {
    mode: u32,
    hash: String,
}

struct PorcelainV2Data {
    index: Index,
    head_tree_items: HashMap<PathBuf, FileInfo>,
}

fn tree_item_mode_to_u32(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Tree => 0o040000,
        TreeItemMode::Commit => 0o160000,
    }
}

/// Classify a raw index entry mode into the tree-item mode it would commit as,
/// mirroring `tree::create_tree_from_index`. Lets staged-change detection notice
/// a mode-only change (e.g. the executable bit set by `add --chmod=+x`).
fn index_mode_to_tree_item_mode(mode: u32) -> TreeItemMode {
    match mode & 0o170000 {
        0o120000 => TreeItemMode::Link,
        0o040000 => TreeItemMode::Tree,
        0o160000 => TreeItemMode::Commit,
        _ if mode & 0o111 != 0 => TreeItemMode::BlobExecutable,
        _ => TreeItemMode::Blob,
    }
}

fn format_mode(mode: u32) -> String {
    format!("{:06o}", mode)
}

fn current_to_workdir(path: &std::path::Path) -> PathBuf {
    let abs_path = util::cur_dir().join(path);
    util::to_workdir_path(&abs_path)
}

/// Tri-state worktree mode. "Gone" and "unreadable" are different answers:
/// the first is representable (`000000`), the second must never be guessed.
enum WorktreeMode {
    Mode(u32),
    Gone,
    Unreadable,
}

/// Mode of an already repository-root-relative path. The porcelain v2
/// payload is projected to repo-root paths before rendering, so it must NOT
/// go through `current_to_workdir` a second time.
fn get_worktree_mode_result_for_workdir(workdir_path: &std::path::Path) -> WorktreeMode {
    // Debug-only seam: the RENDER-time unreadable branch is otherwise only
    // reachable by winning a race between collection and rendering, so tests
    // name the path that must report as unreadable.
    #[cfg(debug_assertions)]
    if std::env::var_os(crate::utils::pager::LIBRA_TEST_ENV).is_some()
        && let Ok(target) = std::env::var("LIBRA_TEST_UNREADABLE_MODE_PATH")
        && !target.is_empty()
        && workdir_path == std::path::Path::new(&target)
    {
        return WorktreeMode::Unreadable;
    }
    let abs_path = util::workdir_to_absolute(workdir_path);
    match std::fs::symlink_metadata(&abs_path) {
        Ok(metadata) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                WorktreeMode::Mode(if metadata.file_type().is_symlink() {
                    0o120000
                } else if metadata.permissions().mode() & 0o111 != 0 {
                    0o100755
                } else {
                    0o100644
                })
            }
            #[cfg(not(unix))]
            {
                let _ = metadata;
                WorktreeMode::Mode(0o100644)
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => WorktreeMode::Gone,
        Err(_) => WorktreeMode::Unreadable,
    }
}

/// Worktree mode of an already repository-root-relative path, with the
/// pre-R0 lenient fallback. The porcelain v2 payload is projected once at
/// the render entry (`to_repo_relative`), so this must NOT re-project via
/// `current_to_workdir` — from a subdirectory that second projection made
/// every lookup miss and fabricated `100644` (2026-08-06 R0-5 review).
/// The fallback (vs the rename arm's hard error) is justified by the
/// collection phase failing closed on unreadable tracked paths first.
fn get_worktree_mode(workdir_path: &std::path::Path) -> u32 {
    match get_worktree_mode_result_for_workdir(workdir_path) {
        WorktreeMode::Mode(mode) => mode,
        _ => 0o100644,
    }
}

/// Whether `path` is recorded at stage 0 of `index` as a `160000` gitlink.
fn is_gitlink_index_entry(index: &Index, path: &str) -> bool {
    index
        .get(path, 0)
        .is_some_and(|entry| is_submodule_mode(entry.mode))
}

fn is_submodule_mode(mode: u32) -> bool {
    mode == 0o160000
}

fn get_submodule_status(_file_path: &std::path::Path) -> String {
    "S...".to_string()
}

fn build_porcelain_v2_data(
    index: Index,
    head_oid: Option<&ObjectHash>,
) -> CliResult<PorcelainV2Data> {
    let head_tree_items = if let Some(commit_hash) = head_oid {
        let (_, tree) = load_head_commit_tree(commit_hash)?;
        tree.get_plain_items_with_mode()
            .into_iter()
            .map(|(path, hash, mode)| {
                (
                    path,
                    FileInfo {
                        mode: tree_item_mode_to_u32(mode),
                        hash: hash.to_string(),
                    },
                )
            })
            .collect()
    } else {
        HashMap::new()
    };

    Ok(PorcelainV2Data {
        index,
        head_tree_items,
    })
}

/// Emit a porcelain v2 `2 <xy> …` rename record (§B.6.4):
/// `2 <xy> <sub> <mH> <mI> <mW> <hH> <hI> R<pct> <new>\t<old>`. Under `-z` the
/// path field becomes `<new> NUL <old> NUL`.
#[allow(clippy::too_many_arguments)]
fn write_rename_porcelain_v2(
    old: &Path,
    new: &Path,
    x: char,
    y: char,
    score: u32,
    metadata: &PorcelainV2Data,
    zero_hash: &str,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    // The porcelain payload was ALREADY projected to repository-root paths
    // (`to_repo_relative`), so these keys are used as-is. Converting again
    // would double the prefix when `status` runs from a subdirectory —
    // `sub/a.txt` became `sub/sub/a.txt`, the HEAD/index lookups missed, and
    // the record fail-closed instead of reporting its real mode and hash.
    let old_workdir = old.to_path_buf();
    let new_workdir = new.to_path_buf();

    // Staged rename: HEAD side is the OLD path, index side is the NEW path.
    // Unstaged-only rename (`.R`): there is no staged component, so Git copies
    // the index fields into the HEAD fields.
    let staged_rename = x == 'R';
    // §B.6.4 forbids fabricated all-zero hashes / default modes in a
    // rename record: a script that trusts `2 R…` must be able to trust
    // its mode+hash columns. Missing metadata is an internal
    // inconsistency, so fail closed with the offending path instead.
    let missing = |field: &str, path: &Path| -> CliError {
        CliError::fatal(format!(
            "cannot render the porcelain v2 rename record for '{}': {field} metadata is missing",
            path.display()
        ))
        .with_stable_code(StableErrorCode::RepoStateInvalid)
        .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
    };
    // The HEAD side comes from the HEAD TREE whenever the record has a real
    // staged component — `R.` (staged rename) and `MR`/`AR` (an unstaged
    // rename whose SOURCE also changed in the index) alike. Only a pure
    // `.R`, where HEAD and index agree by construction, copies the index
    // fields; doing that for `MR` would claim HEAD matches an index the user
    // just changed.
    let head_from_tree = staged_rename || x != '.';
    let (mode_head, hash_head) = if staged_rename {
        metadata
            .head_tree_items
            .get(&old_workdir)
            .map(|info| (info.mode, info.hash.clone()))
            .ok_or_else(|| missing("HEAD tree", &old_workdir))?
    } else if head_from_tree {
        // `MR`/`AR`: the source is the HEAD path. An `A` source has no HEAD
        // entry at all, which is exactly what `A` means, so the zero hash is
        // the honest answer there rather than a fabrication.
        match metadata.head_tree_items.get(&old_workdir) {
            Some(info) => (info.mode, info.hash.clone()),
            None if x == 'A' => (0, zero_hash.to_string()),
            None => return Err(missing("HEAD tree", &old_workdir)),
        }
    } else {
        // `.R`: filled from the index below (fixup).
        (0, zero_hash.to_string())
    };
    let index_key = if staged_rename {
        &new_workdir
    } else {
        &old_workdir
    };
    let index_str = index_key
        .to_str()
        .ok_or_else(|| missing("index (non-UTF-8 path)", index_key))?;
    let (mode_index, hash_index) = metadata
        .index
        .get(index_str, 0)
        .map(|entry| (entry.mode, entry.hash.to_string()))
        .ok_or_else(|| missing("index", index_key))?;
    let (mode_head, hash_head) = if staged_rename || head_from_tree {
        (mode_head, hash_head)
    } else {
        (mode_index, hash_index.clone())
    };
    // A worktree-deleted destination (`RD`) has no worktree entry: mW must
    // be 000000 like an ordinary v2 deleted row, not a fabricated 100644.
    let mode_worktree = if y == 'D' {
        0
    } else {
        // A mode read that FAILS is not `100644`. Between the scan and this
        // render the destination can be deleted or made unreadable; emitting
        // a fabricated regular-file mode would hand a script a value the
        // filesystem never reported. Fail closed instead — the same rule the
        // hash fields already follow.
        match get_worktree_mode_result_for_workdir(&new_workdir) {
            WorktreeMode::Mode(mode) => mode,
            // Genuinely absent (a chained rename already moved it on):
            // `000000`, the v2 spelling for "no worktree entry".
            WorktreeMode::Gone => 0,
            // Present but UNREADABLE: a fabricated `100644` would hand a
            // script a mode the filesystem never reported.
            WorktreeMode::Unreadable => {
                return Err(CliError::fatal(format!(
                    "cannot read the worktree mode of '{}' while rendering its rename record",
                    quote_pathname(new, true)
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("re-run 'libra status' once the path is readable again"));
            }
        }
    };
    let sub = if is_submodule_mode(mode_index) || is_submodule_mode(mode_head) {
        get_submodule_status(new)
    } else {
        "N...".to_string()
    };

    write!(
        writer,
        "2 {x}{y} {} {} {} {} {} {} R{} ",
        sub,
        format_mode(mode_head),
        format_mode(mode_index),
        format_mode(mode_worktree),
        hash_head,
        hash_index,
        score,
    )
    .map_err(write_err)?;
    if null_terminated {
        write_raw_path(writer, new).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
        write_raw_path(writer, old).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        writer
            .write_all(&quote_pathname_bytes(new, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\t").map_err(write_err)?;
        writer
            .write_all(&quote_pathname_bytes(old, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

/// Output porcelain v2 format using metadata collected during status computation.
#[allow(clippy::too_many_arguments)]
fn output_porcelain_v2(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    ignored: &[PathBuf],
    metadata: Option<&PorcelainV2Data>,
    staged_rename_details: &RenameDetails,
    unstaged_rename_details: &RenameDetails,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let metadata =
        metadata.ok_or_else(|| CliError::internal("missing porcelain v2 metadata for status"))?;
    let zero_hash = zero_hash_str();
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    for entry in unmerged {
        write_unmerged_porcelain_v2(entry, &zero_hash, null_terminated, quote_path, writer)?;
    }

    // Rename records (`2 …`) render separately from the flattened `1 …` list;
    // their endpoints are excluded below so they never double as change rows.
    let mut endpoints: HashSet<PathBuf> = HashSet::new();
    for (old, new) in &staged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // Worktree state of the NEW path rides in the second XY column
        // (`RM`/`RD`), mirroring Git — the endpoint row is suppressed.
        let unstaged_char = if unstaged.modified.contains(new) {
            'M'
        } else if unstaged.deleted.contains(new) {
            'D'
        } else {
            '.'
        };
        // A missing score is NOT 100: `R100` is the documented spelling of an
        // exact rename, so defaulting to it would publish an inexact pair as
        // byte-identical. The mode and hash fields already fail closed on
        // missing metadata; the score column gets the same treatment.
        let score = staged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "cannot render the porcelain v2 rename record for '{}': score metadata is missing",
                    quote_pathname(new, quote_path)
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
            })?;
        write_rename_porcelain_v2(
            old,
            new,
            'R',
            unstaged_char,
            score,
            metadata,
            &zero_hash,
            null_terminated,
            quote_path,
            writer,
        )?;
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        let score = unstaged_rename_details
            .get(&(old.clone(), new.clone()))
            .map(|(pct, _)| *pct)
            .ok_or_else(|| {
                CliError::fatal(format!(
                    "cannot render the porcelain v2 rename record for '{}': score metadata is missing",
                    quote_pathname(new, quote_path)
                ))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
                .with_hint("re-run 'libra status' after 'libra add'/'libra reset' settles the index")
            })?;
        // The SOURCE of an unstaged rename may also carry a staged change
        // (edit `a`, `add a`, then move it to `b`). Git reports that as
        // `MR`, keeping the real HEAD and index sides distinct. Hardcoding
        // `.R` both lost the staged component — the endpoint row is
        // suppressed, so it vanished entirely — and made the record copy the
        // index hash into `hH`, claiming HEAD and index agree when they do
        // not.
        let staged_char = if staged.modified.contains(old) {
            'M'
        } else if staged.new.contains(old) {
            'A'
        } else {
            '.'
        };
        write_rename_porcelain_v2(
            old,
            new,
            staged_char,
            'R',
            score,
            metadata,
            &zero_hash,
            null_terminated,
            quote_path,
            writer,
        )?;
    }

    // An unresolved conflict lives ONLY in its `u` record: the
    // stage-0-less index also classifies the path as a staged deletion,
    // and without this exclusion the ordinary loop would emit a bogus
    // duplicate `1 D.` row for it (2026-08-06 R0-5 review).
    let unmerged_paths: std::collections::HashSet<&std::path::Path> =
        unmerged.iter().map(|entry| entry.path.as_path()).collect();
    let status_list = generate_short_format_status(staged, unstaged);
    for (file, staged_status, unstaged_status) in status_list {
        if endpoints.contains(&file) {
            continue;
        }
        if unmerged_paths.contains(file.as_path()) {
            continue;
        }
        if staged_status == '?' && unstaged_status == '?' {
            if null_terminated {
                write!(writer, "? ").map_err(write_err)?;
                write_raw_path(writer, &file).map_err(write_err)?;
            } else {
                write!(writer, "? ").map_err(write_err)?;
                writer
                    .write_all(&quote_pathname_bytes(&file, quote_path))
                    .map_err(write_err)?;
            }
            if null_terminated {
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writer.write_all(b"\n").map_err(write_err)?;
            }
            continue;
        }

        // The porcelain payload is ALREADY repository-root-relative
        // (`to_repo_relative` at the render entry). Re-projecting through
        // `current_to_workdir` here made every index/HEAD lookup miss from
        // a subdirectory and fall back to fabricated `100644`/zero-hash
        // metadata (2026-08-06 R0-5 review).
        let workdir_path = file.clone();
        let file_str = workdir_path.to_str().unwrap_or_default();

        let (mode_index, hash_index) = if let Some(entry) = metadata.index.get(file_str, 0) {
            (entry.mode, entry.hash.to_string())
        } else if staged_status == 'D' {
            // Semantically absent: the deletion is staged, so stage 0 has
            // no entry — Git v2 spells that `000000` plus the zero hash.
            (0, zero_hash.clone())
        } else {
            // Every other `1` row REQUIRES a stage-0 entry; fabricating
            // `100644` here would forge metadata for a lookup that must
            // not miss (2026-08-06 R0-5 review).
            return Err(CliError::fatal(format!(
                "missing index entry for '{file_str}' while rendering porcelain v2"
            ))
            .with_stable_code(StableErrorCode::RepoStateInvalid));
        };

        let (mode_head, hash_head) = if staged_status == 'A' {
            (0, zero_hash.clone())
        } else if let Some(info) = metadata.head_tree_items.get(&workdir_path) {
            (info.mode, info.hash.clone())
        } else {
            (0, zero_hash.clone())
        };

        let mode_worktree = if unstaged_status == 'D' {
            0
        } else {
            match get_worktree_mode_result_for_workdir(&workdir_path) {
                WorktreeMode::Mode(mode) => mode,
                // A path absent from the worktree (e.g. a staged deletion's
                // `D.` row) is semantically gone: `000000`, like Git.
                WorktreeMode::Gone => 0,
                // Unreadable is a different answer from gone and must never
                // be spelled `100644` (2026-08-06 R0-5 review, mirroring
                // the rename-record arm).
                WorktreeMode::Unreadable => {
                    return Err(CliError::fatal(format!(
                        "cannot read the worktree mode of '{file_str}' while rendering \
                         porcelain v2"
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed));
                }
            }
        };

        let sub = if is_submodule_mode(mode_index) || is_submodule_mode(mode_head) {
            get_submodule_status(&file)
        } else {
            "N...".to_string()
        };

        // Git porcelain v2 spells an unmodified side as `.`, never the
        // v1-style space — `1  M` instead of `1 .M` breaks fixed-column
        // consumers (2026-08-06 R0-5 review).
        let dot = |status: char| if status == ' ' { '.' } else { status };
        write!(
            writer,
            "1 {}{} {} {} {} {} {} {} ",
            dot(staged_status),
            dot(unstaged_status),
            sub,
            format_mode(mode_head),
            format_mode(mode_index),
            format_mode(mode_worktree),
            hash_head,
            hash_index,
        )
        .map_err(write_err)?;
        if null_terminated {
            // `1` rows always carry UTF-8 index paths today, but the `-z`
            // wire format is raw bytes by contract.
            write_raw_path(writer, &file).map_err(write_err)?;
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer
                .write_all(&quote_pathname_bytes(&file, quote_path))
                .map_err(write_err)?;
            writer.write_all(b"\n").map_err(write_err)?;
        }
    }

    for file in ignored {
        if null_terminated {
            write!(writer, "! ").map_err(write_err)?;
            write_raw_path(writer, file).map_err(write_err)?;
        } else {
            write!(writer, "! ").map_err(write_err)?;
            writer
                .write_all(&quote_pathname_bytes(file, quote_path))
                .map_err(write_err)?;
        }
        if null_terminated {
            writer.write_all(b"\0").map_err(write_err)?;
        } else {
            writer.write_all(b"\n").map_err(write_err)?;
        }
    }
    Ok(())
}

fn zero_hash_str() -> String {
    ObjectHash::zero_str(get_hash_kind())
}

fn write_unmerged_porcelain_v2(
    entry: &UnmergedEntry,
    zero_hash: &str,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    let (staged_status, unstaged_status) = entry.xy();
    let mode = |stage| {
        entry
            .stage(stage)
            .map(|stage| format_mode(stage.mode))
            .unwrap_or_else(|| "000000".to_string())
    };
    let hash = |stage| {
        entry
            .stage(stage)
            .map(|stage| stage.hash.to_string())
            .unwrap_or_else(|| zero_hash.to_string())
    };
    write!(
        writer,
        "u {}{} N... {} {} {} {} {} {} {} ",
        staged_status,
        unstaged_status,
        mode(1),
        mode(2),
        mode(3),
        format_mode(get_unmerged_worktree_mode(&entry.path)),
        hash(1),
        hash(2),
        hash(3)
    )
    .map_err(write_err)?;
    // §B.6.6: `-z` carries RAW path bytes (a non-UTF-8 name must survive
    // byte-for-byte), every other mode carries the C-style-escaped form.
    // `display()` did neither — it lossily replaced undecodable bytes and
    // left control characters unescaped, which can break the line format.
    if null_terminated {
        write_raw_path(writer, &entry.path).map_err(write_err)?;
        writer.write_all(b"\0").map_err(write_err)?;
    } else {
        writer
            .write_all(&quote_pathname_bytes(&entry.path, quote_path))
            .map_err(write_err)?;
        writer.write_all(b"\n").map_err(write_err)?;
    }
    Ok(())
}

/// `u`-row worktree mode; the unmerged payload is repository-root-relative
/// like the rest of the projected porcelain data (2026-08-06 R0-5 review:
/// same double-projection fix as `get_worktree_mode`).
fn get_unmerged_worktree_mode(workdir_path: &std::path::Path) -> u32 {
    let abs_path = util::workdir_to_absolute(workdir_path);
    if std::fs::symlink_metadata(&abs_path).is_ok() {
        get_worktree_mode(workdir_path)
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// Short format
// ---------------------------------------------------------------------------

/// Core logic for generating short format status without color (for testing).
///
/// LEGACY tuple API (pre-R0 shape, §B.6.0.1): renames are DECOMPOSED into
/// their endpoint states — a staged rename contributes `old = D ` / `new =
/// A `, an unstaged rename contributes an unstaged `D` on its source (merged
/// with any staged state via the ordinary rules) and `??` on its
/// destination — so a chain `a→b` staged + `b→c` unstaged yields `a = D `,
/// `b = AD`, `c = ??` with no duplicate rows. Rename-aware consumers use
/// [`generate_short_status_entries`] instead.
pub fn generate_short_format_status(
    staged: &Changes,
    unstaged: &Changes,
) -> Vec<(std::path::PathBuf, char, char)> {
    generate_short_format_status_with_unmerged(staged, unstaged, &[])
}

fn process_unstaged_changes(
    files: &[PathBuf],
    file_status: &mut HashMap<PathBuf, (char, char)>,
    unstaged_char: char,
) {
    for file in files {
        let staged_status = file_status.get(file).map(|(s, _)| *s);
        if let Some(status) = staged_status {
            file_status.insert(file.clone(), (status, unstaged_char));
        } else {
            file_status.insert(file.clone(), (' ', unstaged_char));
        }
    }
}

/// Shared base XY map: every non-rename change plus unmerged entries. Rename
/// pairs are handled by the caller (decomposed by the legacy API, first-class
/// in [`generate_short_status_entries`]).
fn short_xy_base(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
) -> HashMap<PathBuf, (char, char)> {
    let mut file_status: HashMap<PathBuf, (char, char)> = HashMap::new();

    for file in &staged.new {
        file_status.insert(file.clone(), ('A', ' '));
    }
    for file in &staged.modified {
        file_status.insert(file.clone(), ('M', ' '));
    }
    for file in &staged.deleted {
        file_status.insert(file.clone(), ('D', ' '));
    }

    process_unstaged_changes(&unstaged.modified, &mut file_status, 'M');
    process_unstaged_changes(&unstaged.deleted, &mut file_status, 'D');

    for file in &unstaged.new {
        file_status.insert(file.clone(), ('?', '?'));
    }
    for entry in unmerged {
        file_status.insert(entry.path.clone(), entry.xy());
    }
    file_status
}

pub(crate) fn generate_short_format_status_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
) -> Vec<(std::path::PathBuf, char, char)> {
    let mut file_status: HashMap<PathBuf, (char, char)> = HashMap::new();

    for file in &staged.new {
        file_status.insert(file.clone(), ('A', ' '));
    }
    for file in &staged.modified {
        file_status.insert(file.clone(), ('M', ' '));
    }
    for file in &staged.deleted {
        file_status.insert(file.clone(), ('D', ' '));
    }
    // Pre-R0 decomposition: a staged rename is a delete of the old path plus
    // an add of the new path in this legacy tuple view.
    for (old, new) in &staged.renamed {
        file_status.insert(old.clone(), ('D', ' '));
        file_status.insert(new.clone(), ('A', ' '));
    }

    process_unstaged_changes(&unstaged.modified, &mut file_status, 'M');
    process_unstaged_changes(&unstaged.deleted, &mut file_status, 'D');
    // Pre-R0 decomposition: an unstaged rename is an unstaged delete of its
    // source (merged with any staged state) …
    for (old, _new) in &unstaged.renamed {
        process_unstaged_changes(std::slice::from_ref(old), &mut file_status, 'D');
    }

    for file in &unstaged.new {
        file_status.insert(file.clone(), ('?', '?'));
    }
    // … and an untracked destination.
    for (_old, new) in &unstaged.renamed {
        file_status.insert(new.clone(), ('?', '?'));
    }
    for entry in unmerged {
        file_status.insert(entry.path.clone(), entry.xy());
    }

    let mut sorted_files: Vec<_> = file_status.iter().collect();
    sorted_files.sort_by(|a, b| a.0.cmp(b.0));

    sorted_files
        .into_iter()
        .map(|(file, (staged_status, unstaged_status))| {
            (file.clone(), *staged_status, *unstaged_status)
        })
        .collect()
}

/// One short-format / porcelain-v1 render entry (§B.6.1 public API): either a
/// plain per-path change or a first-class rename pair (rendered with Git's
/// `old -> new` arrow instead of two endpoint rows).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ShortStatusEntry {
    Path {
        path: PathBuf,
        staged: char,
        unstaged: char,
    },
    Rename {
        old: PathBuf,
        new: PathBuf,
        staged: char,
        unstaged: char,
    },
}

impl ShortStatusEntry {
    /// Sort key — Git orders renames by their destination path.
    fn sort_key(&self) -> &Path {
        match self {
            ShortStatusEntry::Path { path, .. } => path,
            ShortStatusEntry::Rename { new, .. } => new,
        }
    }
}

/// Build the shared short-format / porcelain-v1 entry list (§B.6.1): rename
/// pairs stay first-class — the unstaged column of a staged rename carries
/// the DESTINATION's worktree state (`RM`/`RD`) — and every non-endpoint
/// path renders as an XY tuple. Entries sort by path, renames by their
/// destination.
pub fn generate_short_status_entries(
    staged: &Changes,
    unstaged: &Changes,
) -> Vec<ShortStatusEntry> {
    generate_short_status_entries_with_unmerged(staged, unstaged, &[])
}

pub(crate) fn generate_short_status_entries_with_unmerged(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
) -> Vec<ShortStatusEntry> {
    let mut entries: Vec<ShortStatusEntry> = Vec::new();
    let mut endpoints: HashSet<PathBuf> = HashSet::new();
    for (old, new) in &staged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The endpoint rows are suppressed, so this column is the only
        // signal for the destination's worktree state.
        let unstaged_char = if unstaged.modified.contains(new) {
            'M'
        } else if unstaged.deleted.contains(new) {
            'D'
        } else {
            ' '
        };
        entries.push(ShortStatusEntry::Rename {
            old: old.clone(),
            new: new.clone(),
            staged: 'R',
            unstaged: unstaged_char,
        });
    }
    for (old, new) in &unstaged.renamed {
        endpoints.insert(old.clone());
        endpoints.insert(new.clone());
        // The suppressed source row is the only carrier of the SOURCE's
        // staged state: a staged-modify→worktree-rename is `MR`, a
        // staged-add→worktree-rename `AR` — hard-coding a space here
        // erased that component while porcelain v2 derived it correctly
        // (2026-08-06 R0-6 review, mirroring the v2 derivation).
        let staged_char = if staged.modified.contains(old) {
            'M'
        } else if staged.new.contains(old) {
            'A'
        } else {
            ' '
        };
        entries.push(ShortStatusEntry::Rename {
            old: old.clone(),
            new: new.clone(),
            staged: staged_char,
            unstaged: 'R',
        });
    }

    let mut base: Vec<_> = short_xy_base(staged, unstaged, unmerged)
        .into_iter()
        .collect();
    base.sort_by(|a, b| a.0.cmp(&b.0));
    for (path, (staged_char, unstaged_char)) in base {
        if endpoints.contains(&path) {
            continue;
        }
        entries.push(ShortStatusEntry::Path {
            path,
            staged: staged_char,
            unstaged: unstaged_char,
        });
    }
    entries.sort_by(|a, b| a.sort_key().cmp(b.sort_key()));
    entries
}

/// Short format output — legacy public API used by tests.
pub async fn output_short_format(
    staged: &Changes,
    unstaged: &Changes,
    writer: &mut impl Write,
) -> CliResult<()> {
    output_short_format_with_config(
        staged,
        unstaged,
        &[],
        &OutputConfig::default(),
        false,
        true,
        writer,
    )
    .await
}

/// C-style path quoting for human-short and non-`-z` porcelain output
/// (§B.6.6). Control characters, `"` and `\` are ALWAYS escaped; bytes above
/// 0x7F are additionally escaped as octal `\ooo` only under
/// `core.quotePath=true` (the default, matching Git). A path needing any
/// escape is wrapped in double quotes; `-z` output never calls this.
/// §B.6.0.1 reason taxonomy → JSON reason string + warning code.
pub(crate) fn io_blocked_reason_and_code(
    reason: crate::command::status_probe::IoBlockedReason,
) -> (&'static str, StatusWarningCode) {
    use crate::command::status_probe::IoBlockedReason;

    match reason {
        IoBlockedReason::PermissionDenied => (
            "permission_denied",
            StatusWarningCode::WorktreePermissionDenied,
        ),
        IoBlockedReason::IoError => ("io_error", StatusWarningCode::WorktreeReadFailed),
        IoBlockedReason::IoTimeout => ("io_timeout", StatusWarningCode::WorktreeIoTimeout),
    }
}

/// Write a path under `-z` as RAW OS bytes (Git parity: `-z` never quotes,
/// and on Unix a non-UTF-8 name keeps its exact bytes on the wire). Non-Unix
/// platforms fall back to the platform's stable `display()` encoding.
fn write_raw_path(writer: &mut impl Write, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        writer.write_all(path.as_os_str().as_bytes())
    }
    #[cfg(not(unix))]
    {
        write!(writer, "{}", path.display())
    }
}

/// Public so the wave-0 suite can pin the §B.6.6 escape matrix on paths
/// (LF/CR) that cannot be created as files on every filesystem.
pub fn quote_pathname(path: &Path, quote_path: bool) -> String {
    let bytes = quote_pathname_bytes(path, quote_path);
    match String::from_utf8(bytes) {
        Ok(text) => text,
        // String-typed callers can never hold raw non-UTF-8 bytes, so they
        // keep the lossless octal-escaped form (every byte maps 1:1).
        // Byte-oriented writers call `quote_pathname_bytes` and stay raw
        // when `core.quotePath` is off (Git parity).
        Err(_) => {
            String::from_utf8(quote_pathname_bytes(path, true)).unwrap_or_else(|error| {
                // INVARIANT: the quote_path=true form octal-escapes every
                // byte >= 0x80, so it is pure ASCII and always valid UTF-8.
                String::from_utf8_lossy(error.as_bytes()).into_owned()
            })
        }
    }
}

/// Byte-faithful variant of [`quote_pathname`] for byte-oriented writers
/// (§B.6.6): TAB/LF/CR/`"`/`\` are always escaped; bytes >= 0x80 are
/// octal-escaped only while `quote_path` holds — with `core.quotePath=false`
/// they are written RAW, including when the path is not valid UTF-8 (Git
/// parity). `-z` surfaces never call this: they are raw end to end.
pub fn quote_pathname_bytes(path: &Path, quote_path: bool) -> Vec<u8> {
    // Escape the RAW OS path bytes (Unix), not a lossy `display()` copy, so
    // a non-UTF-8 name renders its true bytes (`\377`) instead of U+FFFD
    // replacement bytes. On non-Unix `display()` is the platform's stable
    // encoding.
    #[cfg(unix)]
    let bytes: &[u8] = {
        use std::os::unix::ffi::OsStrExt;
        path.as_os_str().as_bytes()
    };
    #[cfg(not(unix))]
    let display = path.display().to_string();
    #[cfg(not(unix))]
    let bytes: &[u8] = display.as_bytes();
    let needs_escape =
        |b: u8| b < 0x20 || b == 0x7f || b == b'"' || b == b'\\' || (quote_path && b >= 0x80);
    if !bytes.iter().copied().any(needs_escape) {
        return bytes.to_vec();
    }
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len() + 8);
    out.push(b'"');
    for &b in bytes {
        match b {
            b'\t' => out.extend_from_slice(b"\\t"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            _ if b < 0x20 || b == 0x7f || (quote_path && b >= 0x80) => {
                out.extend_from_slice(format!("\\{b:03o}").as_bytes());
            }
            _ => out.push(b),
        }
    }
    out.push(b'"');
    out
}

/// Short format output with color controlled by OutputConfig.
///
/// Renames are rendered as a single `R  <old> -> <new>` line (Git's short
/// rename form), not as two separate `R` rows (§B.6.1). Under `-z` the record
/// is `XY SP <new> NUL <old> NUL` (new before old, matching Git) with RAW
/// unquoted paths; non-`-z` paths go through [`quote_pathname`] (§B.6.6).
async fn output_short_format_with_config(
    staged: &Changes,
    unstaged: &Changes,
    unmerged: &[UnmergedEntry],
    output: &OutputConfig,
    null_terminated: bool,
    quote_path: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let use_colors = should_use_colors(output).await;
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);

    for entry in generate_short_status_entries_with_unmerged(staged, unstaged, unmerged) {
        match entry {
            ShortStatusEntry::Path {
                path,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &path).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else if use_colors {
                    // Only the XY letters are colored; the path is appended
                    // byte-faithfully so `core.quotePath=false` keeps raw
                    // high bytes even under forced color.
                    let head = format_colored_status(x, y, "");
                    writer.write_all(head.as_bytes()).map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&path, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
            ShortStatusEntry::Rename {
                old,
                new,
                staged: x,
                unstaged: y,
            } => {
                if null_terminated {
                    // `XY SP <new> NUL <old> NUL` (§B.6.1), raw path bytes.
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    write_raw_path(writer, &new).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                    write_raw_path(writer, &old).map_err(write_err)?;
                    writer.write_all(b"\0").map_err(write_err)?;
                } else if use_colors {
                    // Same byte-faithful shape as the Path arm: colored XY,
                    // then raw quoted bytes, then the arrow and the new path.
                    let head = format_colored_status(x, y, "");
                    writer.write_all(head.as_bytes()).map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                } else {
                    write!(writer, "{x}{y} ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&old, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b" -> ").map_err(write_err)?;
                    writer
                        .write_all(&quote_pathname_bytes(&new, quote_path))
                        .map_err(write_err)?;
                    writer.write_all(b"\n").map_err(write_err)?;
                }
            }
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Color control — unified with OutputConfig
// ---------------------------------------------------------------------------

/// Check if colors should be used, respecting OutputConfig overrides first,
/// then falling back to config-based / TTY detection.
async fn should_use_colors(output: &OutputConfig) -> bool {
    use std::io::IsTerminal;

    match output.color {
        ColorChoice::Never => return false,
        ColorChoice::Always => return true,
        ColorChoice::Auto => {}
    }

    // Auto: check git-style config, then TTY
    if let Some(color_setting) = ConfigKv::get("color.status.short")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
    {
        match color_setting.as_str() {
            "always" => return true,
            "never" | "false" => return false,
            "auto" | "true" => return io::stdout().is_terminal(),
            _ => return false,
        }
    }

    if let Some(color_setting) = ConfigKv::get("color.ui")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
    {
        match color_setting.as_str() {
            "always" => return true,
            "never" | "false" => return false,
            "auto" | "true" => return io::stdout().is_terminal(),
            _ => return false,
        }
    }

    io::stdout().is_terminal()
}

fn format_colored_status(staged_status: char, unstaged_status: char, file: &str) -> String {
    use colored::Colorize;

    let colored_staged = match staged_status {
        'A' => staged_status.to_string().green(),
        'M' => staged_status.to_string().green(),
        'D' => staged_status.to_string().red(),
        'R' => staged_status.to_string().yellow(),
        'C' => staged_status.to_string().yellow(),
        'U' => staged_status.to_string().red(),
        '?' => staged_status.to_string().bright_red(),
        ' ' => staged_status.to_string().into(),
        _ => staged_status.to_string().into(),
    };

    let colored_unstaged = match unstaged_status {
        'M' => unstaged_status.to_string().red(),
        'D' => unstaged_status.to_string().red(),
        'U' => unstaged_status.to_string().red(),
        '?' => unstaged_status.to_string().bright_red(),
        '!' => unstaged_status.to_string().bright_red(),
        ' ' => unstaged_status.to_string().into(),
        _ => unstaged_status.to_string().into(),
    };

    format!("{colored_staged}{colored_unstaged} {file}")
}

// ---------------------------------------------------------------------------
// Branch info helpers (short / porcelain)
// ---------------------------------------------------------------------------

/// Print branch info line for short / porcelain v1 `--branch`.
fn print_branch_info(
    head: &Head,
    upstream: Option<&UpstreamInfo>,
    show_ahead_behind: bool,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    match head {
        Head::Detached(commit_hash) => {
            let line = format!("## HEAD (detached at {})", &commit_hash.to_string()[..8]);
            if null_terminated {
                write!(writer, "{line}").map_err(write_err)?;
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writeln!(writer, "{line}").map_err(write_err)?;
            }
        }
        Head::Branch(branch) => {
            let line = if let Some(u) = upstream {
                let tracking = format!("{}...{}", branch, u.remote_ref);
                if u.gone {
                    format!("## {tracking} [gone]")
                } else if show_ahead_behind {
                    let ahead = u.ahead.unwrap_or(0);
                    let behind = u.behind.unwrap_or(0);
                    if ahead > 0 && behind > 0 {
                        format!("## {tracking} [ahead {ahead}, behind {behind}]")
                    } else if ahead > 0 {
                        format!("## {tracking} [ahead {ahead}]")
                    } else if behind > 0 {
                        format!("## {tracking} [behind {behind}]")
                    } else {
                        format!("## {tracking}")
                    }
                } else {
                    format!("## {tracking}")
                }
            } else {
                format!("## {branch}")
            };
            if null_terminated {
                write!(writer, "{line}").map_err(write_err)?;
                writer.write_all(b"\0").map_err(write_err)?;
            } else {
                writeln!(writer, "{line}").map_err(write_err)?;
            }
        }
    }
    Ok(())
}

/// Write branch information in porcelain v2 style.
fn write_branch_info_v2(
    head: &Head,
    head_oid: Option<&ObjectHash>,
    upstream: Option<&UpstreamInfo>,
    show_ahead_behind: bool,
    null_terminated: bool,
    writer: &mut impl Write,
) -> CliResult<()> {
    let write_err =
        |e: io::Error| crate::utils::output::stdout_write_error("write status output", e);
    let term = if null_terminated { b"\0" } else { b"\n" };

    match head {
        Head::Detached(_) => {
            write!(writer, "# branch.head (detached)").map_err(write_err)?;
            writer.write_all(term).map_err(write_err)?;
        }
        Head::Branch(name) => {
            write!(writer, "# branch.head {}", name).map_err(write_err)?;
            writer.write_all(term).map_err(write_err)?;
        }
    }

    if let Some(oid) = head_oid {
        write!(writer, "# branch.oid {oid}").map_err(write_err)?;
    } else {
        write!(writer, "# branch.oid (initial)").map_err(write_err)?;
    }
    writer.write_all(term).map_err(write_err)?;

    if let Some(u) = upstream {
        write!(writer, "# branch.upstream {}", u.remote_ref).map_err(write_err)?;
        writer.write_all(term).map_err(write_err)?;
        if !u.gone && show_ahead_behind {
            let ahead = u.ahead.unwrap_or(0);
            let behind = u.behind.unwrap_or(0);
            write!(writer, "# branch.ab +{ahead} -{behind}").map_err(write_err)?;
            writer.write_all(term).map_err(write_err)?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Upstream tracking resolution
// ---------------------------------------------------------------------------

fn status_branch_store_error(context: &str, error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Query(detail) => {
            CliError::fatal(format!("failed to {context}: {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        other => CliError::fatal(format!("failed to {context}: {other}"))
            .with_stable_code(StableErrorCode::RepoCorrupt),
    }
}

fn status_config_read_error(context: &str, error: anyhow::Error) -> CliError {
    CliError::fatal(format!("failed to {context}: {error}"))
        .with_stable_code(StableErrorCode::IoReadFailed)
}

async fn resolve_upstream_info(
    head: &Head,
    local_commit: Option<&ObjectHash>,
) -> CliResult<Option<UpstreamInfo>> {
    let branch_name = match head {
        Head::Branch(name) => name.clone(),
        Head::Detached(_) => return Ok(None),
    };

    let branch_config = match ConfigKv::branch_config(&branch_name).await {
        Ok(Some(config)) => config,
        Ok(None) => return Ok(None),
        Err(error) => {
            return Err(status_config_read_error(
                &format!("read branch configuration for '{branch_name}'"),
                error,
            ));
        }
    };

    let remote = &branch_config.remote;
    let merge_branch = &branch_config.merge;
    let remote_ref_display = format!("{remote}/{merge_branch}");

    // Tracking refs are stored under their fully-qualified
    // `refs/remotes/<remote>/<branch>` name (clone/fetch/push writers), so the
    // lookup must use that name — a short-name query never matches and made
    // every fresh clone report "upstream is gone" (#464). The short-name probe
    // is kept as a fallback for repositories written before the
    // fully-qualified convention.
    let tracking_full_ref = format!("refs/remotes/{remote}/{merge_branch}");
    let tracking_branch = Branch::find_branch_result(&tracking_full_ref, Some(remote))
        .await
        .map_err(|error| status_branch_store_error("resolve upstream branch", error))?;
    let tracking_branch = match tracking_branch {
        Some(branch) => Some(branch),
        None => Branch::find_branch_result(merge_branch, Some(remote))
            .await
            .map_err(|error| status_branch_store_error("resolve upstream branch", error))?,
    };

    let tracking_commit = match tracking_branch {
        Some(b) => b.commit,
        None => {
            // Upstream configured but tracking ref doesn't exist → gone
            return Ok(Some(UpstreamInfo {
                remote_ref: remote_ref_display,
                ahead: None,
                behind: None,
                gone: true,
            }));
        }
    };

    let local_commit = match local_commit {
        Some(commit) => commit,
        None => {
            // Unborn branch: no local commit to compare against.
            // Return None for ahead/behind — numeric counts would imply
            // a comparison that never happened.
            return Ok(Some(UpstreamInfo {
                remote_ref: remote_ref_display,
                ahead: None,
                behind: None,
                gone: false,
            }));
        }
    };

    let (ahead, behind) = compute_ahead_behind(local_commit, &tracking_commit);

    Ok(Some(UpstreamInfo {
        remote_ref: remote_ref_display,
        ahead: Some(ahead),
        behind: Some(behind),
        gone: false,
    }))
}

/// Compute the number of commits ahead/behind between two refs.
///
/// Performs a bidirectional BFS from both tips, classifying each commit as
/// local-only, remote-only, or common (reachable from both sides).  Once a
/// commit is found from the opposite side it is reclassified as common and
/// its ancestors are not enqueued again, which reduces redundant work when
/// the histories share a recent merge-base.
///
/// **Complexity**: proportional to the number of commits reachable from
/// both tips until the queues are drained.  For disjoint histories (no
/// common ancestor) this visits all reachable commits from both sides.
/// Falls back gracefully when a commit object is missing or corrupt
/// (e.g. shallow clone) by stopping traversal on that branch.
pub(crate) fn compute_ahead_behind(local: &ObjectHash, remote: &ObjectHash) -> (usize, usize) {
    if local == remote {
        return (0, 0);
    }

    let mut local_only: HashSet<ObjectHash> = HashSet::new();
    let mut remote_only: HashSet<ObjectHash> = HashSet::new();
    let mut common: HashSet<ObjectHash> = HashSet::new();
    let mut local_queue: VecDeque<ObjectHash> = VecDeque::new();
    let mut remote_queue: VecDeque<ObjectHash> = VecDeque::new();

    local_queue.push_back(*local);
    remote_queue.push_back(*remote);

    while !local_queue.is_empty() || !remote_queue.is_empty() {
        // Expand one commit from the local side.
        if let Some(hash) = local_queue.pop_front() {
            if common.contains(&hash) {
                // Already common — skip without expanding parents.
                continue;
            } else if remote_only.remove(&hash) {
                // Discovered from the remote side too → merge-base.
                common.insert(hash);
            } else if local_only.insert(hash)
                && let Some(commit) = Commit::try_load(&hash)
            {
                for parent in &commit.parent_commit_ids {
                    if !common.contains(parent) {
                        local_queue.push_back(*parent);
                    }
                }
            }
        }

        // Expand one commit from the remote side.
        if let Some(hash) = remote_queue.pop_front() {
            if common.contains(&hash) {
                continue;
            } else if local_only.remove(&hash) {
                common.insert(hash);
            } else if remote_only.insert(hash)
                && let Some(commit) = Commit::try_load(&hash)
            {
                for parent in &commit.parent_commit_ids {
                    if !common.contains(parent) {
                        remote_queue.push_back(*parent);
                    }
                }
            }
        }
    }

    (local_only.len(), remote_only.len())
}

// ---------------------------------------------------------------------------
// Bare repository detection
// ---------------------------------------------------------------------------

/// Shared-parser `core.bare` read (all git boolean spellings). FAILS CLOSED:
/// an unparseable value or a config read failure refuses status rather than
/// silently proceeding into worktree-status collection on a bare repository.
async fn is_bare_repository() -> CliResult<bool> {
    match ConfigKv::get("core.bare").await {
        Ok(Some(entry)) => crate::internal::config::parse_git_bool(&entry.value).ok_or_else(|| {
            CliError::fatal(format!(
                "invalid core.bare value '{}': expected true/false/yes/no/on/off/1/0",
                entry.value
            ))
            .with_stable_code(StableErrorCode::CliInvalidArguments)
        }),
        Ok(None) => Ok(false),
        Err(error) => Err(CliError::fatal(format!(
            "cannot read core.bare to classify this repository: {error}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)),
    }
}

// ---------------------------------------------------------------------------
// Untracked directory collapsing
// ---------------------------------------------------------------------------

pub(crate) fn collapse_untracked_directories(
    untracked_files: Vec<PathBuf>,
    index: &Index,
) -> Vec<PathBuf> {
    use std::collections::BTreeSet;

    if untracked_files.is_empty() {
        return untracked_files;
    }

    let mut dir_files: HashMap<PathBuf, Vec<PathBuf>> = HashMap::new();
    let mut root_files: Vec<PathBuf> = Vec::new();

    for file in &untracked_files {
        let components: Vec<_> = file.components().collect();
        if components.len() > 1 {
            let top_dir = PathBuf::from(components[0].as_os_str());
            dir_files.entry(top_dir).or_default().push(file.clone());
        } else {
            root_files.push(file.clone());
        }
    }

    let mut result: BTreeSet<PathBuf> = BTreeSet::new();

    for file in root_files {
        result.insert(file);
    }

    for (dir, files) in dir_files {
        // Component-wise prefix check, never a `display()` string: a
        // non-UTF-8 directory name must not be flattened (U+FFFD would
        // break the comparison AND corrupt the marker).
        let has_tracked_files = index.tracked_files().iter().any(|f| f.starts_with(&dir));

        if has_tracked_files {
            for file in files {
                result.insert(file);
            }
        } else {
            // The marker is `<dir>/` built on the RAW name (see
            // `status_untracked_paths::directory_marker`).
            let mut marker = dir.into_os_string();
            marker.push("/");
            result.insert(PathBuf::from(marker));
        }
    }

    result.into_iter().collect()
}

// ---------------------------------------------------------------------------
// Clean check
// ---------------------------------------------------------------------------

/// Check if the working tree is clean.
///
/// Returns `false` when the status cannot be determined (e.g. corrupt index).
pub async fn is_clean() -> bool {
    let staged = match changes_to_be_committed_safe().await {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("failed to calculate committed changes: {err}");
            return false;
        }
    };
    let unstaged = match changes_to_be_staged() {
        Ok(c) => c,
        Err(err) => {
            tracing::error!("failed to calculate staged changes: {err}");
            return false;
        }
    };
    staged.is_empty() && unstaged.is_empty()
}

// ---------------------------------------------------------------------------
// Status computation (public API preserved)
// ---------------------------------------------------------------------------

/// Convenience wrapper around [`changes_to_be_committed_safe`].
///
/// On error (e.g. corrupt index), logs the failure and returns an empty
/// [`Changes`] set instead of panicking.
pub async fn changes_to_be_committed() -> Changes {
    match changes_to_be_committed_safe().await {
        Ok(changes) => changes,
        Err(err) => {
            tracing::error!("changes_to_be_committed failed: {err}");
            Changes::default()
        }
    }
}

pub async fn changes_to_be_committed_safe() -> Result<Changes, StatusError> {
    let mut changes = Changes::default();
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    let head_commit = Head::current_commit().await;
    let tracked_files = index.tracked_files();

    if head_commit.is_none() {
        changes.new = tracked_files;
        return Ok(changes);
    }

    let head_commit = match head_commit {
        Some(head_commit) => head_commit,
        None => return Ok(changes),
    };
    let commit =
        Commit::try_load(&head_commit).ok_or_else(|| StatusError::HeadObjectUnreadable {
            what: "commit",
            oid: head_commit.to_string(),
        })?;
    let tree =
        Tree::try_load(&commit.tree_id).ok_or_else(|| StatusError::HeadObjectUnreadable {
            what: "tree",
            oid: commit.tree_id.to_string(),
        })?;
    let tree_files = tree.get_plain_items_with_mode();

    for (item_path, item_hash, item_mode) in tree_files.iter() {
        // §B.6.1: a tree path that is not valid UTF-8 cannot match an index
        // key, so it is skipped rather than made fatal.
        let Some(item_str) = item_path.to_str() else {
            continue;
        };
        if index.tracked(item_str, 0) {
            // A staged change is either a content change (blob hash differs) OR a
            // mode change (e.g. `add --chmod=+x`): the index records 100755 while
            // the HEAD tree still has 100644, with the same blob.
            let content_changed = !index.verify_hash(item_str, 0, item_hash);
            let mode_changed = index
                .get(item_str, 0)
                .is_some_and(|entry| index_mode_to_tree_item_mode(entry.mode) != *item_mode);
            if content_changed || mode_changed {
                changes.modified.push(item_path.clone());
            }
        } else {
            changes.deleted.push(item_path.clone());
        }
    }
    let tree_files_set: HashSet<PathBuf> =
        tree_files.into_iter().map(|(path, _, _)| path).collect();
    changes.new = tracked_files
        .into_iter()
        .filter(|path| !tree_files_set.contains(path))
        .collect();

    Ok(changes)
}

/// Compare the difference between `index` and the `workdir` using the default ignore rules.
pub fn changes_to_be_staged() -> Result<Changes, StatusError> {
    changes_to_be_staged_with_policy(IgnorePolicy::Respect)
}

/// Variant of [`changes_to_be_staged`] that lets callers pick the ignore strategy explicitly.
/// Commands such as `add --force` or `status --ignored` can switch policies as needed.
pub fn changes_to_be_staged_with_policy(policy: IgnorePolicy) -> Result<Changes, StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let ignore_case = effective_ignore_case_for_workdir(&workdir)?;
    changes_to_be_staged_with_policy_and_ignore_case(policy, ignore_case)
}

fn changes_to_be_staged_with_policy_and_ignore_case(
    policy: IgnorePolicy,
    ignore_case: bool,
) -> Result<Changes, StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    let (mut visible, ignored) =
        changes_to_be_staged_split_with_index(&workdir, &index, ignore_case)?;
    match policy {
        IgnorePolicy::Respect => Ok(visible),
        IgnorePolicy::OnlyIgnored => Ok(ignored),
        IgnorePolicy::IncludeIgnored => {
            visible.extend(ignored);
            Ok(visible)
        }
    }
}

pub fn changes_to_be_staged_split_safe() -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let ignore_case = effective_ignore_case_for_workdir(&workdir)?;
    changes_to_be_staged_split_safe_with_ignore_case(ignore_case)
}

pub(crate) fn changes_to_be_staged_split_safe_with_ignore_case(
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    changes_to_be_staged_split_with_index(&workdir, &index, ignore_case)
}

/// List changes to be staged with --force semantics (recurse into ignored directories)
pub fn changes_to_be_staged_split_force() -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let ignore_case = effective_ignore_case_for_workdir(&workdir)?;
    changes_to_be_staged_split_force_with_ignore_case(ignore_case)
}

fn effective_ignore_case_for_workdir(workdir: &Path) -> Result<bool, StatusError> {
    crate::utils::path_case::effective_ignore_case_for_dir_sync(workdir)
        .map_err(|source| StatusError::ConfigRead { source })
}

pub(crate) fn changes_to_be_staged_split_force_with_ignore_case(
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let workdir = util::try_working_dir().map_err(|source| StatusError::Workdir { source })?;
    let index_path = path::try_index().map_err(|source| StatusError::Workdir { source })?;
    let index = Index::load(&index_path).map_err(|source| StatusError::IndexLoad {
        path: index_path.clone(),
        source,
    })?;
    changes_to_be_staged_split_force_with_index(&workdir, &index, ignore_case)
}

fn changes_to_be_staged_split_force_with_index(
    workdir: &PathBuf,
    index: &Index,
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let mut visible = Changes::default();
    let mut ignored = Changes::default();
    let tracked_files = index.tracked_files();
    let tracked_fold = tracked_files_by_fold(&tracked_files, ignore_case);
    for file in tracked_files.iter() {
        // §B.6.1: skip the keyed comparisons for an undecodable name rather
        // than failing the whole status (see `collect_tracked_worktree_changes`).
        let Some(file_str) = file.to_str() else {
            continue;
        };
        // Gitlinks have no working-tree blob to compare (see
        // `changes_to_be_staged_split_with_index`).
        if is_gitlink_index_entry(index, file_str) {
            match workdir.join(file).symlink_metadata() {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    continue;
                }
                // Absent is the normal shape: Libra never checks a submodule out.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                // A plain file or symlink is neither shape, so it stays visible.
                Ok(_) => {
                    visible.modified.push(file.clone());
                    continue;
                }
                // Anything else is a real worktree read failure: fall through to
                // the ordinary handling below rather than reporting clean.
                Err(_) => {}
            }
        }
        let file_abs = workdir.join(file);
        if file_abs.symlink_metadata().is_err() {
            visible.deleted.push(file.clone());
        } else if index.is_modified(file_str, 0, workdir) {
            let file_hash =
                calc_file_blob_hash(&file_abs).map_err(|source| StatusError::FileHash {
                    path: file_abs.clone(),
                    source,
                })?;
            if !index.verify_hash(file_str, 0, &file_hash) {
                visible.modified.push(file.clone());
            }
        }
    }
    let (files, ignored_files) = list_workdir_files_split_force(workdir).map_err(|source| {
        StatusError::ListWorkdirFiles {
            path: workdir.clone(),
            source,
        }
    })?;
    // A non-UTF-8 name is NOT a status failure (§B.6.1): the base `??` row
    // survives everywhere else, and failing the whole command here would
    // make `--scan` the one mode a repository containing such a file could
    // never run. It cannot be a tracked-lookup key, so it is simply treated
    // as untracked — which is what it is.
    for file in files {
        let untracked = match file.to_str() {
            Some(file_str) => !index.tracked(file_str, 0),
            None => true,
        };
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
            visible.new.push(file);
        }
    }
    for file in ignored_files {
        let untracked = match file.to_str() {
            Some(file_str) => !index.tracked(file_str, 0),
            None => true,
        };
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
            ignored.new.push(file);
        }
    }
    Ok((visible, ignored))
}

fn changes_to_be_staged_split_with_index(
    workdir: &PathBuf,
    index: &Index,
    ignore_case: bool,
) -> Result<(Changes, Changes), StatusError> {
    let mut visible = Changes::default();
    let mut ignored = Changes::default();
    let tracked_files = index.tracked_files();
    let tracked_fold = tracked_files_by_fold(&tracked_files, ignore_case);
    for file in tracked_files.iter() {
        // §B.6.1: skip the keyed comparisons for an undecodable name rather
        // than failing the whole status (see `collect_tracked_worktree_changes`).
        let Some(file_str) = file.to_str() else {
            continue;
        };
        // A `160000` gitlink names a SUBMODULE commit, not a blob of this
        // repository. Comparing one against the working tree as a file reports
        // every submodule as deleted — or, when the directory exists, fails the
        // whole status trying to hash a directory — and that verdict feeds
        // `ensure_clean_status`, so it would block `switch`/`merge`/`rebase` in
        // any repository that merely CONTAINS a submodule. Submodule content is
        // out of scope (ADR-MG-01), so the two shapes Libra expects — absent
        // (never materialized) and a directory (checked out by the user) — are
        // left alone. A plain file or symlink there is neither, and stays
        // visible as a modification rather than being silently ignored.
        if is_gitlink_index_entry(index, file_str) {
            match workdir.join(file).symlink_metadata() {
                Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {
                    continue;
                }
                // Absent is the normal shape: Libra never checks a submodule out.
                Err(error)
                    if matches!(
                        error.kind(),
                        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                    ) =>
                {
                    continue;
                }
                // A plain file or symlink is neither shape, so it stays visible.
                Ok(_) => {
                    visible.modified.push(file.clone());
                    continue;
                }
                // Anything else is a real worktree read failure: fall through to
                // the ordinary handling below rather than reporting clean.
                Err(_) => {}
            }
        }
        let file_abs = workdir.join(file);
        if file_abs.symlink_metadata().is_err() {
            visible.deleted.push(file.clone());
        } else if index.is_modified(file_str, 0, workdir) {
            let file_hash =
                calc_file_blob_hash(&file_abs).map_err(|source| StatusError::FileHash {
                    path: file_abs.clone(),
                    source,
                })?;
            if !index.verify_hash(file_str, 0, &file_hash) {
                visible.modified.push(file.clone());
            }
        }
    }
    let (files, ignored_files) =
        list_workdir_files_split_safe(workdir).map_err(|source| StatusError::ListWorkdirFiles {
            path: workdir.clone(),
            source,
        })?;
    // §B.6.1: an undecodable name is untracked, not a fatal error — it can
    // never be an index key, so the lookup is simply skipped.
    for file in files {
        let untracked = file.to_str().is_none_or(|name| !index.tracked(name, 0));
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
            visible.new.push(file);
        }
    }
    for file in ignored_files {
        let untracked = file.to_str().is_none_or(|name| !index.tracked(name, 0));
        if untracked && !is_same_file_tracked_alias(workdir, &file, &tracked_fold) {
            ignored.new.push(file);
        }
    }
    Ok((visible, ignored))
}

fn tracked_files_by_fold(tracked_files: &[PathBuf], ignore_case: bool) -> HashMap<String, PathBuf> {
    if !ignore_case {
        return HashMap::new();
    }
    tracked_files
        .iter()
        .map(|path| {
            (
                crate::utils::path_case::fold_path_key(path.to_string_lossy().as_ref()),
                path.clone(),
            )
        })
        .collect()
}

fn is_same_file_tracked_alias(
    workdir: &Path,
    file: &Path,
    tracked_fold: &HashMap<String, PathBuf>,
) -> bool {
    let key = crate::utils::path_case::fold_path_key(file.to_string_lossy().as_ref());
    tracked_fold.get(&key).is_some_and(|tracked| {
        crate::utils::path_case::is_same_file_case_alias(workdir, file, tracked)
    })
}

fn list_workdir_files_split_safe(workdir: &PathBuf) -> io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut files = Vec::new();
    let mut ignored = Vec::new();
    let mut pending_dirs = vec![workdir.clone()];
    // ONE snapshot for the whole walk (§C.4.1.1): capturing per path would
    // re-read process-global state thousands of times, and a concurrent re-pin
    // between two paths would switch which worktree's exclusions this walk is
    // applying halfway through.
    let layers = crate::internal::layer::ExclusionSnapshot::for_request();

    while let Some(dir) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            // Always skip `.libra` (Libra metadata) and `.git` (like Git, which
            // hardcodes ignoring `.git`); neither is ever surfaced or staged.
            if entry.file_name() == std::ffi::OsStr::new(util::ROOT_DIR)
                || entry.file_name() == std::ffi::OsStr::new(util::GIT_DIR)
            {
                continue;
            }

            let file_type = entry.file_type()?;
            let relative = path
                .strip_prefix(workdir)
                .map_err(|err| io::Error::other(err.to_string()))?
                .to_path_buf();
            if file_type.is_dir() {
                if util::check_gitignore_with_layers(workdir, &path, &layers) {
                    ignored.push(relative);
                } else {
                    pending_dirs.push(path);
                }
            } else if file_type.is_file() || file_type.is_symlink() {
                if util::check_gitignore_with_layers(workdir, &path, &layers) {
                    ignored.push(relative);
                } else {
                    files.push(relative);
                }
            }
        }
    }

    Ok((files, ignored))
}

/// List workdir files with --force semantics: recurse into ignored directories
/// and include their files in the ignored list
fn list_workdir_files_split_force(workdir: &PathBuf) -> io::Result<(Vec<PathBuf>, Vec<PathBuf>)> {
    let mut files = Vec::new();
    let mut ignored = Vec::new();
    let mut pending_dirs = vec![workdir.clone()];

    while let Some(dir) = pending_dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let entry = entry?;
            let path = entry.path();
            // Always skip `.libra` (Libra metadata) and `.git` (like Git, which
            // hardcodes ignoring `.git`); `--force` must not stage `.git` either.
            if entry.file_name() == std::ffi::OsStr::new(util::ROOT_DIR)
                || entry.file_name() == std::ffi::OsStr::new(util::GIT_DIR)
            {
                continue;
            }

            let file_type = entry.file_type()?;
            let relative = path
                .strip_prefix(workdir)
                .map_err(|err| io::Error::other(err.to_string()))?
                .to_path_buf();
            if file_type.is_dir() {
                // Always recurse into directories, even ignored ones.
                // We never push the directory entry itself — only its files
                // — so `add --force` sees concrete blobs, not a path that
                // would panic when `Blob::from_file` tries to read it.
                pending_dirs.push(path.clone());
            } else if file_type.is_file() || file_type.is_symlink() {
                if util::check_gitignore(workdir, &path) {
                    ignored.push(relative);
                } else {
                    files.push(relative);
                }
            }
        }
    }

    Ok((files, ignored))
}

/// List ignored files (not tracked by index, but ignored by configured rules) under workdir
pub fn list_ignored_files() -> Result<Changes, StatusError> {
    changes_to_be_staged_with_policy(IgnorePolicy::OnlyIgnored)
}

#[cfg(test)]
mod argv_normalization_test {
    use super::*;

    fn argv(parts: &[&str]) -> Vec<std::ffi::OsString> {
        parts.iter().map(std::ffi::OsString::from).collect()
    }

    fn normalize(parts: &[&str]) -> StatusArgvResolution {
        normalize_status_argv(
            argv(parts),
            &<crate::cli::Cli as clap::CommandFactory>::command(),
        )
    }

    /// §B.4.3: the argv scan itself records the format flags, from the
    /// SUBCOMMAND's arity table.
    ///
    /// This is the level the cluster logic has to be tested at. An
    /// end-to-end assertion about `-bz` output cannot fail if the scan is
    /// deleted, because clap parses the cluster too — so it proves clap
    /// works, not that the scan does.
    #[test]
    fn clusters_are_scanned_from_the_subcommand_arity_table() {
        for cluster in ["-bz", "-zb"] {
            let resolution = normalize(&["libra", "status", cluster]);
            assert!(
                resolution.format.z_explicit,
                "`{cluster}` contains -z: {:?}",
                resolution.format
            );
            assert!(
                !resolution.format.short_explicit,
                "`{cluster}` contains no -s: {:?}",
                resolution.format
            );
        }
        for cluster in ["-sz", "-zs"] {
            let resolution = normalize(&["libra", "status", cluster]);
            assert!(
                resolution.format.z_explicit && resolution.format.short_explicit,
                "`{cluster}` contains both -s and -z: {:?}",
                resolution.format
            );
        }
    }

    /// A cluster STOPS at the first value-taking option: everything after it
    /// is that option's value, and its letters are not flags. `-uzs` is
    /// `-u=zs`, so neither `z` nor `s` may be recorded.
    #[test]
    fn a_cluster_value_is_never_read_as_flags() {
        let resolution = normalize(&["libra", "status", "-uzs"]);
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "`zs` is -u's VALUE, not two flags: {:?}",
            resolution.format
        );
        // And the same letters BEFORE the value option are flags.
        let resolution = normalize(&["libra", "status", "-zuno"]);
        assert!(
            resolution.format.z_explicit,
            "the -z before -u is a flag: {:?}",
            resolution.format
        );
        assert!(
            !resolution.format.short_explicit,
            "`no` is -u's value: {:?}",
            resolution.format
        );
    }

    /// A global AFTER the subcommand is a global, not a cluster.
    ///
    /// clap accepts `libra status -J=ndjson`, and the scan has to as well:
    /// reading `ndjson` as a cluster made its `s` look like `--short`, and
    /// the agreement check then refused a perfectly ordinary command line.
    #[test]
    fn a_global_after_the_subcommand_is_not_a_cluster() {
        let resolution = normalize(&["libra", "status", "-J=ndjson"]);
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "`ndjson` is -J's value: {:?}",
            resolution.format
        );
        // The same for a global taking a SEPARATE value.
        let resolution = normalize(&["libra", "status", "--color", "never"]);
        assert_eq!(resolution.format, StatusFormatFlags::default());
    }

    /// A valued GLOBAL with an attached value does not shift subcommand
    /// location, and the value's letters are not flags.
    #[test]
    fn a_global_attached_value_does_not_shift_the_subcommand() {
        let resolution = normalize(&["libra", "-J=ndjson", "status", "--find-renames=505"]);
        assert_eq!(
            resolution.rename_occurrences.len(),
            1,
            "the status slice was found after a valued global"
        );
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "`ndjson` is a value, not flags: {:?}",
            resolution.format
        );
        // The raw value survives for the resolver; argv carries a placeholder.
        assert_eq!(
            resolution.argv[3],
            std::ffi::OsString::from("--find-renames=50")
        );
    }

    /// Everything after `--` is copied verbatim: not scanned for flags, not
    /// collected as an occurrence, not rewritten.
    #[test]
    fn tokens_after_the_separator_are_untouched() {
        let resolution = normalize(&["libra", "status", "--", "--find-renames=505", "-z"]);
        assert!(
            resolution.rename_occurrences.is_empty(),
            "a pathspec is not an occurrence"
        );
        assert_eq!(
            resolution.format,
            StatusFormatFlags::default(),
            "a pathspec is not a flag: {:?}",
            resolution.format
        );
        assert_eq!(
            resolution.argv[3],
            std::ffi::OsString::from("--find-renames=505"),
            "and it is not rewritten"
        );
    }

    /// A non-status subcommand is never rewritten, even when its own
    /// arguments spell `status`.
    #[test]
    fn a_non_status_subcommand_is_left_alone() {
        let resolution = normalize(&["libra", "diff", "status", "--find-renames=505"]);
        assert!(resolution.rename_occurrences.is_empty());
        assert_eq!(
            resolution.argv[3],
            std::ffi::OsString::from("--find-renames=505")
        );
    }

    /// §B.4.3: the API percent field accepts ONLY 0..=100 — a struct-literal
    /// caller passing 101..=255 fails closed with LBR-CLI-002 instead of a
    /// silent clamp to exact-only, and the clap parser path refuses the
    /// value outright (2026-08-05 R0-4 review).
    #[test]
    fn api_percent_above_100_fails_closed() {
        use clap::Parser as _;

        let args = StatusArgs {
            find_renames: Some(101),
            ..Default::default()
        };
        let err = resolve_status_threshold(&args, None).expect_err("101% is out of range");
        assert_eq!(err.stable_code(), StableErrorCode::CliInvalidArguments);

        assert!(
            StatusArgs::try_parse_from(["status", "--find-renames=101"]).is_err(),
            "clap's range guard refuses 101 on the parser path"
        );

        let full = resolve_status_threshold(
            &StatusArgs {
                find_renames: Some(100),
                ..Default::default()
            },
            None,
        )
        .expect("100% stays valid");
        assert_eq!(full, Some(60000), "100% means exact-only, not an error");
    }
}

#[cfg(test)]
mod test {
    use sea_orm::{ConnectionTrait, Statement};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        internal::db::reset_db_conn_instance_for_path,
        utils::{
            error::StableErrorCode,
            test::{self, ChangeDirGuard},
        },
    };

    /// Pin the `Display` format for the static-message variants of
    /// [`StatusError`]. Only `InvalidPathEncoding` has a fully static
    /// pattern — the others are all source-chained (`{source}`) and
    /// owned by their wrapped error type, so they're intentionally
    /// skipped. The CliError mapping above prefixes "failed to determine
    /// working tree status: " in front of every variant before sending
    /// it to the human / --json envelope, so direct-Display matters
    /// less for this enum than for typed errors with more variants.
    #[test]
    fn status_error_display_pins_invalid_path_encoding_variant() {
        assert_eq!(
            StatusError::InvalidPathEncoding {
                path: PathBuf::from("src/foo"),
            }
            .to_string(),
            "path 'src/foo' is not valid UTF-8",
        );
    }

    #[test]
    fn short_format_surface_emits_all_seven_unmerged_xy_codes() {
        use crate::command::unmerged::{UnmergedEntry, UnmergedStage};

        let hash = ObjectHash::new(&[0u8; 20]);
        let mk = |name: &str, stages: [bool; 3]| {
            let stage = |present: bool| {
                present.then_some(UnmergedStage {
                    mode: 0o100644,
                    hash,
                })
            };
            UnmergedEntry::new(
                PathBuf::from(name),
                [stage(stages[0]), stage(stages[1]), stage(stages[2])],
            )
        };
        let unmerged = vec![
            mk("dd.txt", [true, false, false]),
            mk("au.txt", [false, true, false]),
            mk("ud.txt", [true, true, false]),
            mk("ua.txt", [false, false, true]),
            mk("du.txt", [true, false, true]),
            mk("aa.txt", [false, true, true]),
            mk("uu.txt", [true, true, true]),
        ];
        let empty = Changes::default();
        let rows = generate_short_format_status_with_unmerged(&empty, &empty, &unmerged);
        let by_path: std::collections::BTreeMap<_, _> = rows
            .into_iter()
            .map(|(path, x, y)| (path, (x, y)))
            .collect();
        assert_eq!(by_path.get(&PathBuf::from("dd.txt")), Some(&('D', 'D')));
        assert_eq!(by_path.get(&PathBuf::from("au.txt")), Some(&('A', 'U')));
        assert_eq!(by_path.get(&PathBuf::from("ud.txt")), Some(&('U', 'D')));
        assert_eq!(by_path.get(&PathBuf::from("ua.txt")), Some(&('U', 'A')));
        assert_eq!(by_path.get(&PathBuf::from("du.txt")), Some(&('D', 'U')));
        assert_eq!(by_path.get(&PathBuf::from("aa.txt")), Some(&('A', 'A')));
        assert_eq!(by_path.get(&PathBuf::from("uu.txt")), Some(&('U', 'U')));
    }

    #[test]
    fn list_workdir_files_prunes_ignored_directories() {
        let repo = tempdir().expect("failed to create temp repo");
        let workdir = repo.path().to_path_buf();
        std::fs::write(workdir.join(".libraignore"), "ignored-dir/\n")
            .expect("failed to write ignore file");
        std::fs::create_dir_all(workdir.join("ignored-dir/nested"))
            .expect("failed to create ignored directory");
        std::fs::write(workdir.join("ignored-dir/nested/file.txt"), "ignored")
            .expect("failed to write ignored file");
        std::fs::write(workdir.join("visible.txt"), "visible").expect("failed to write file");

        let (visible, ignored) =
            list_workdir_files_split_safe(&workdir).expect("failed to list workdir files");

        assert!(visible.contains(&PathBuf::from(".libraignore")));
        assert!(visible.contains(&PathBuf::from("visible.txt")));
        assert!(ignored.contains(&PathBuf::from("ignored-dir")));
        assert!(!visible.contains(&PathBuf::from("ignored-dir/nested/file.txt")));
        assert!(!ignored.contains(&PathBuf::from("ignored-dir/nested/file.txt")));
    }

    #[tokio::test]
    #[serial]
    async fn sequence_notice_surfaces_corrupt_sequence_kind() {
        let repo = tempdir().expect("failed to create temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        let db_path = repo.path().join(".libra").join("libra.db");
        let db = crate::internal::sequencer::request_db_checked()
            .await
            .expect("test fixture database");
        db.execute_raw(Statement::from_string(
            db.get_database_backend(),
            "INSERT INTO sequence_state \
             (worktree_id, kind, head_name, head_orig, current_oid, todo, payload) \
             VALUES ('', 'corrupt', 'main', 'a', 'b', '', '{}')",
        ))
        .await
        .expect("insert corrupt sequence row");

        let error = sequence_notice()
            .await
            .expect_err("corrupt sequence state must fail closed");
        assert_eq!(error.stable_code(), StableErrorCode::RepoStateInvalid);
        assert!(
            error
                .to_string()
                .contains("unknown sequence kind 'corrupt'")
        );

        reset_db_conn_instance_for_path(&db_path).await;
    }

    #[tokio::test]
    #[serial]
    async fn resolve_upstream_info_surfaces_branch_config_query_failures() {
        let repo = tempdir().expect("failed to create temp repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = ChangeDirGuard::new(repo.path());
        let db_path = repo.path().join(".libra").join("libra.db");

        let db = crate::internal::sequencer::request_db_checked()
            .await
            .expect("test fixture database");
        db.execute_raw(Statement::from_string(
            db.get_database_backend(),
            "DROP TABLE config_kv",
        ))
        .await
        .expect("dropping config_kv table should succeed");

        let err = resolve_upstream_info(&Head::Branch("main".to_string()), None)
            .await
            .expect_err("missing config_kv table should surface as an error");

        assert_eq!(err.stable_code(), StableErrorCode::IoReadFailed);
        assert!(
            err.to_string()
                .contains("failed to read branch configuration for 'main'"),
            "unexpected error: {err}"
        );

        reset_db_conn_instance_for_path(&db_path).await;
    }

    /// §B.5 seam: engine degradation stats map onto both structured warning
    /// codes (the end-to-end similarity path is covered by
    /// `similarity_budget_warning`; this pins the mapping in isolation).
    #[test]
    fn rename_stats_map_to_structured_warnings() {
        let stats = rename_detect::RenameDetectStats {
            skipped_by_limit: true,
            exhaustive_discarded: true,
            ..Default::default()
        };
        let mut warnings = Vec::new();
        warnings_from_rename_stats(&stats, &mut warnings);
        assert_eq!(warnings.len(), 2);
        assert!(matches!(
            warnings[0].code,
            StatusWarningCode::RenameLimitProductSkipped
        ));
        assert!(matches!(
            warnings[1].code,
            StatusWarningCode::SimilarityBudgetExceeded
        ));
        // Full-text pins: both degradation messages must name their
        // SURVIVORS — the engine keeps exact and already-scored
        // unique-basename pairs and discards only the exhaustive stage, and
        // a rewording that claims the whole inexact pass was lost would
        // misreport the output the user is looking at.
        assert_eq!(
            warnings[0].message,
            "rename detection skipped the exhaustive inexact pass: too many candidates on one side (renameLimit); exact and unique-basename matches were kept"
        );
        assert_eq!(
            warnings[1].message,
            "rename detection discarded the exhaustive inexact pass: similarity comparison budget exceeded; exact and already-scored unique-basename matches were kept"
        );
        assert!(
            warnings
                .iter()
                .all(|w| matches!(w.source, StatusWarningSource::RenameDetect))
        );
    }

    /// §B.5: the frozen `source` enum means `metadata` for the object store
    /// and `worktree` for the working tree. Both sides can hit a size cap,
    /// exhaust a budget, and fail to read, so this asserts BOTH directions:
    /// no worktree problem is published as `metadata`, and no object problem
    /// is published as `worktree`.
    #[test]
    fn content_skips_map_to_metadata_and_worktree_warnings() {
        use rename_detect::SkipReason;
        let mut stats = rename_detect::RenameDetectStats::default();
        stats.content_skips.insert(SkipReason::ObjectMissing, 2);
        stats.content_skips.insert(SkipReason::ObjectCorrupt, 1);
        stats.content_skips.insert(SkipReason::ObjectIoFailed, 6);
        stats.content_skips.insert(SkipReason::ObjectTooLarge, 3);
        stats
            .content_skips
            .insert(SkipReason::ObjectBudgetExceeded, 4);
        stats.content_skips.insert(SkipReason::WorktreeTooLarge, 8);
        stats
            .content_skips
            .insert(SkipReason::WorktreeBudgetExceeded, 9);
        stats.content_skips.insert(SkipReason::WorktreeIoFailed, 5);
        let mut warnings = Vec::new();
        warnings_from_rename_stats(&stats, &mut warnings);
        assert_eq!(warnings.len(), 4, "{warnings:?}");

        let find = |code: StatusWarningCode| {
            warnings
                .iter()
                .find(|w| w.code == code)
                .unwrap_or_else(|| panic!("{code:?} missing from {warnings:?}"))
        };

        // Object side: missing + corrupt + object I/O failure, all `metadata`.
        let unavailable = find(StatusWarningCode::MetadataUnavailable);
        assert_eq!(unavailable.source, StatusWarningSource::Metadata);
        assert!(
            unavailable.message.contains("9 candidate(s)"),
            "{unavailable:?}"
        );
        // Object side caps, still `metadata` — and NOT inflated by the
        // worktree caps below.
        let object_budget = find(StatusWarningCode::MetadataBudgetExceeded);
        assert_eq!(object_budget.source, StatusWarningSource::Metadata);
        assert!(
            object_budget.message.contains("7 candidate(s)"),
            "{object_budget:?}"
        );
        // Worktree caps get their OWN code under `worktree`; before the
        // split these were reported as a repository-object budget.
        let worktree_budget = find(StatusWarningCode::WorktreeBudgetExceeded);
        assert_eq!(worktree_budget.source, StatusWarningSource::Worktree);
        assert!(
            worktree_budget.message.contains("17 candidate(s)"),
            "{worktree_budget:?}"
        );
        // And a worktree read failure stays `worktree` — the object-side
        // I/O failure above must not have leaked into this count.
        let worktree_failed = find(StatusWarningCode::WorktreeReadFailed);
        assert_eq!(worktree_failed.source, StatusWarningSource::Worktree);
        assert!(
            worktree_failed.message.contains("5 candidate(s)"),
            "{worktree_failed:?}"
        );
    }
}

#[cfg(test)]
mod rename_destination_budget_test {
    use super::*;
    use crate::utils::test::ChangeDirGuard;

    /// The destination (untracked-side) detector must hand its drawn-down
    /// budgets back to the run-level `RenameBudgets` — remaining bytes and
    /// tasks, the shared OID cache, and the spent comparisons. Without the
    /// restore a detection pass added after it would restart with fresh
    /// budgets, silently doubling the call-level caps (§B.3.4).
    #[test]
    fn destination_detector_restores_budgets_and_records_comparisons() {
        let repo = tempfile::tempdir().expect("temp repo");
        // Minimal bare-layout markers so path discovery treats the temp dir
        // as a repository and object lookups fail as genuine misses.
        std::fs::create_dir_all(repo.path().join("objects")).expect("objects dir");
        std::fs::write(repo.path().join("libra.db"), b"").expect("db marker");
        let _guard = ChangeDirGuard::new(repo.path());

        let mut details: RenameDetails = HashMap::new();
        let mut stats = rename_detect::RenameDetectStats::default();
        let mut budgets = RenameBudgets::new();
        let (worktree_before, tasks_before) = (budgets.worktree_total, budgets.worktree_tasks);
        let (objects_before, slots_before) = (budgets.objects_total, budgets.objects_slots);
        let config = rename_detect::RenameDetectConfig {
            threshold: 30000,
            rename_limit: 1000,
            comparison_budget: Some(500_000),
        };

        // A scorable inexact pair (both sides worktree this call) spends
        // worktree budget on hashing/reads and records real comparisons.
        // The fixture is large enough that spanhash sees many shared spans.
        let old_payload = b"alpha beta gamma delta\n".repeat(200);
        let mut new_payload = old_payload.clone();
        let mid = new_payload.len() / 2;
        new_payload[mid] = b'X';
        std::fs::write(repo.path().join("gone.txt"), &old_payload).expect("old");
        std::fs::write(repo.path().join("came.txt"), &new_payload).expect("new");
        let mut changes = Changes {
            new: vec![],
            modified: vec![],
            deleted: vec![PathBuf::from("gone.txt")],
            renamed: vec![],
        };
        let consumed = detect_renames_with_destinations(
            &mut changes,
            &config,
            RenameBlobSide::Worktree,
            &[PathBuf::from("came.txt")],
            &mut details,
            &mut stats,
            &mut budgets,
        );
        assert!(
            consumed.contains(&PathBuf::from("came.txt")),
            "the similar pair should match inexact and consume the destination"
        );
        assert!(
            budgets.worktree_total < worktree_before || budgets.worktree_tasks < tasks_before,
            "worktree bytes/tasks spent by the destination pass must be restored \
             to the shared budget (before={worktree_before}/{tasks_before} \
             after={}/{})",
            budgets.worktree_total,
            budgets.worktree_tasks
        );
        assert!(
            budgets.comparisons_spent > 0,
            "inexact scoring comparisons must be recorded against the shared cap"
        );
        // The NEXT consumer sees the depleted remainder, not a fresh cap.
        let narrowed = budgets.narrowed(&config);
        assert_eq!(
            narrowed.comparison_budget,
            Some(500_000u64.saturating_sub(budgets.comparisons_spent)),
            "a later pass must inherit the spent comparisons"
        );

        // A HEAD/index-side candidate whose object is missing consumes an
        // object slot and lands in the SHARED OID cache; both must come back
        // with the restored object budget.
        let missing_oid = git_internal::internal::object::blob::Blob::from_content_bytes(
            b"never stored in this repository".to_vec(),
        )
        .id;
        std::fs::create_dir_all(repo.path().join("d")).expect("d dir");
        std::fs::create_dir_all(repo.path().join("u")).expect("u dir");
        std::fs::write(repo.path().join("u/same.txt"), b"payload\n").expect("dest");
        let known: HashMap<PathBuf, (ObjectHash, u32)> =
            [(PathBuf::from("d/same.txt"), (missing_oid, 0o100644))]
                .into_iter()
                .collect();
        let mut changes2 = Changes {
            new: vec![],
            modified: vec![],
            deleted: vec![PathBuf::from("d/same.txt")],
            renamed: vec![],
        };
        detect_renames_with_destinations(
            &mut changes2,
            &config,
            RenameBlobSide::Known(&known),
            &[PathBuf::from("u/same.txt")],
            &mut details,
            &mut stats,
            &mut budgets,
        );
        assert!(
            !budgets.object_cache.is_empty(),
            "the shared OID cache must return with the restored object budget"
        );
        assert!(
            budgets.objects_slots < slots_before,
            "the missing-object lookup consumed a slot that must be restored \
             (before={slots_before} after={})",
            budgets.objects_slots
        );
        assert!(
            budgets.objects_total <= objects_before,
            "object byte budget must never grow across a pass"
        );
    }
}

#[cfg(test)]
mod seam_gate_test {
    use super::*;

    /// `LIBRA_TEST_STATUS_COMPARISON_BUDGET` must be honored only under the
    /// test harness: without `LIBRA_TEST` the production cap stays in
    /// effect; with the gate the override bites.
    #[test]
    #[serial_test::serial]
    fn comparison_budget_override_requires_the_harness_gate() {
        // SAFETY: serialized test body; every variable is removed again
        // before the test returns.
        unsafe {
            std::env::set_var("LIBRA_TEST_STATUS_COMPARISON_BUDGET", "1");
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
            assert_eq!(
                status_comparison_budget(),
                rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS,
                "without LIBRA_TEST the budget override must be ignored"
            );
            std::env::set_var(crate::utils::pager::LIBRA_TEST_ENV, "1");
            assert_eq!(
                status_comparison_budget(),
                1,
                "with the gate the budget override applies"
            );
            // Tighten-only: even under the gate, a value above the
            // production cap clamps back to it — the seam can shrink the
            // budget to force exhaustion, never raise it.
            std::env::set_var(
                "LIBRA_TEST_STATUS_COMPARISON_BUDGET",
                (rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS + 1).to_string(),
            );
            assert_eq!(
                status_comparison_budget(),
                rename_detect::STATUS_MAX_SIMILARITY_COMPARISONS,
                "a gated override above the production cap is clamped to the cap"
            );
            std::env::remove_var("LIBRA_TEST_STATUS_COMPARISON_BUDGET");
            std::env::remove_var(crate::utils::pager::LIBRA_TEST_ENV);
        }
    }
}
