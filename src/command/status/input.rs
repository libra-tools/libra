//! Status arguments and configuration resolution.

use super::*;

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
    /// The optional value is the similarity threshold (`-M`, `-M<n>`, `-M<n>%`,
    /// `--find-renames[=<n>]`, default 50%). `-M` keeps Git's glued-value short
    /// form; the pre-clap argv scan records the raw value and applies the
    /// shared `libra diff` score grammar.
    #[clap(
        short = 'M',
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
    pub(super) fn show_ahead_behind(&self) -> bool {
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

    pub(super) fn preflight_messages(&self) -> &[String] {
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
    pub(super) rename_occurrences: Vec<RenameThresholdOccurrence>,
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
    pub(super) fn ensure_agrees_with(&self, args: &StatusArgs) -> CliResult<()> {
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
            } else if tok == "-M" {
                // WT-02: `-M` is the short spelling of `--find-renames`; a
                // bare `-M` is the default threshold. The raw value is
                // recorded here and the token is rewritten so clap never
                // sees Git's glued score syntax.
                occurrences.push(RenameThresholdOccurrence::FindRaw(std::ffi::OsString::new()));
                argv[j] = std::ffi::OsString::from("-M50");
            } else if let Some(raw) = tok.strip_prefix("-M") {
                occurrences.push(RenameThresholdOccurrence::FindRaw(
                    std::ffi::OsString::from(raw),
                ));
                argv[j] = std::ffi::OsString::from("-M50");
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
                for (idx, ch) in cluster.char_indices() {
                    if ch == '=' {
                        break;
                    }
                    match status_shorts.get(&ch) {
                        Some(true) => {
                            // `-M` is the one OPTIONAL-value short (WT-02): the
                            // rest of the cluster after `M` is its raw
                            // threshold, so `-sM90` is `-s` plus `-M90`. Every
                            // other value-taking short keeps the plain break.
                            if ch == 'M' {
                                let rest = &cluster[idx + ch.len_utf8()..];
                                let raw = if rest.is_empty() {
                                    std::ffi::OsString::new()
                                } else {
                                    std::ffi::OsString::from(rest)
                                };
                                occurrences.push(RenameThresholdOccurrence::FindRaw(raw));
                                // Preserve the preceding flags; only the value
                                // is replaced by the clap-safe placeholder.
                                argv[j] =
                                    std::ffi::OsString::from(format!("-{}M50", &cluster[..idx]));
                            }
                            break;
                        }
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
pub(super) struct StatusConfigExtras {
    /// `status.renameUntracked` (§B.3.1): untracked worktree paths may be
    /// unstaged rename destinations only when true (default false = Git
    /// parity: a tracked→untracked move renders as `D` + `??`).
    pub(super) rename_untracked: bool,
    /// Engine-scale rename threshold resolved by
    /// [`resolve_status_threshold`] (None = detection disabled). Cache modes
    /// force None at collection time regardless.
    pub(super) rename_threshold: Option<u32>,
    /// `status.renameLimit` falling back to `diff.renameLimit` (§B.5):
    /// per-side inexact candidate cap, `0` = uncapped, default 1000.
    pub(super) rename_limit: usize,
    /// `core.quotePath` (§B.6.6): default true (Git parity).
    pub(super) quote_path: bool,
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
pub(super) async fn apply_status_config_defaults(
    args: &mut StatusArgs,
) -> CliResult<StatusConfigExtras> {
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
    pub(super) args: StatusArgs,
    pub(super) extras: StatusConfigExtras,
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
