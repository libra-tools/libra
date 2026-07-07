//! Provides diff command logic comparing commits, the index, and the working tree with algorithm selection, pathspec filtering, and optional file output.

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::{
    cell::RefCell,
    collections::{HashMap, HashSet, VecDeque},
    fmt::Write as _,
    io::{self, IsTerminal},
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
use colored::Colorize;
use git_internal::{
    Diff,
    hash::ObjectHash,
    internal::{
        index::{Index, IndexEntry, Time},
        object::{blob::Blob, commit::Commit, tree::Tree, types::ObjectType},
        pack::utils::calculate_object_hash,
    },
};
use serde::Serialize;
use similar::{Algorithm, ChangeTag, TextDiff};
use tempfile::NamedTempFile;

use crate::{
    command::{get_target_commit, load_object},
    internal::{config::ConfigKv, head::Head},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        ignore::{self, IgnorePolicy},
        object_ext::TreeExt,
        output::{ColorChoice, OutputConfig, ProgressMode, emit_json_data},
        pager::Pager,
        path, util,
    },
};

const DIFF_EXAMPLES: &str = "\
EXAMPLES:
    libra diff                              Compare index against the working tree
    libra diff --staged                     Compare HEAD against the index
    libra diff --old HEAD~1 --new HEAD      Compare two revisions (flag form)
    libra diff HEAD~1 HEAD                  Compare two revisions (positional, same as A..B)
    libra diff main...feature               Diff from merge-base(main,feature) to feature
    libra diff HEAD -- src/                 '--' separates revisions from paths
    libra diff --stat src/                  Show diff statistics under src/
    libra diff --shortstat                  Show just the files-changed/insertions/deletions line
    libra diff --word-diff                   Word-level diff ([-removed-]{+added+} inline)
    libra diff -U0                          Patch with no surrounding context (default is 3)
    libra diff -w                           Ignore whitespace-only changes
    libra diff -b                           Ignore changes in the amount of whitespace
    libra diff --ignore-blank-lines         Ignore changes that are only blank lines
    libra diff -s --exit-code               Status-only check: no output, exit 1 if changes
    libra diff --name-only -z               NUL-terminated changed-file list for scripts
    libra diff --cached --check             Warn about whitespace errors on added lines
    libra diff -R                           Reverse diff (swap additions and deletions)
    libra --json diff --staged              Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = DIFF_EXAMPLES)]
pub struct DiffArgs {
    /// Old commit, default is HEAD
    #[clap(long, value_name = "COMMIT")]
    pub old: Option<String>,

    /// New commit, default is working directory
    #[clap(long, value_name = "COMMIT")]
    #[clap(requires = "old", group = "op_new")]
    pub new: Option<String>,

    /// Use stage as new commit. This option is conflict with --new.
    /// `--cached` is accepted as a Git-compatible alias for `--staged`.
    #[clap(long, visible_alias = "cached")]
    #[clap(group = "op_new")]
    pub staged: bool,

    #[clap(help = "Files to compare")]
    pathspec: Vec<String>,

    /// Paths after a `--` separator: always treated as pathspecs, never
    /// revisions (Git's revision/path disambiguation separator).
    #[clap(last = true, value_name = "PATH")]
    after_dashdash: Vec<String>,

    /// Diff algorithm. `histogram` is currently the only implemented backend.
    #[clap(
        long,
        default_value = "histogram",
        value_name = "NAME",
        value_parser = ["histogram", "myers", "myersMinimal"],
    )]
    pub algorithm: Option<String>,

    /// Write the diff to `FILENAME` instead of stdout
    #[clap(long, value_name = "FILENAME")]
    pub output: Option<String>,

    /// Show only changed file names
    #[clap(long)]
    pub name_only: bool,

    /// Show changed file names with status
    #[clap(long)]
    pub name_status: bool,

    /// Show a word diff instead of a line patch. MODE is `plain` (the default
    /// when given with no value; removed words wrapped in `[-…-]`, added in
    /// `{+…+}`), `color` (highlight with color instead of brackets, in a
    /// terminal), `porcelain` (machine format: one token per line, `-`/`+`/` `
    /// prefixes, `~` for newlines), or `none` (disable). Words are
    /// whitespace-delimited.
    #[clap(long = "word-diff", value_name = "MODE", num_args = 0..=1, require_equals = true, default_missing_value = "plain")]
    pub word_diff: Option<String>,

    /// Show insertion/deletion counts in a machine-friendly format
    #[clap(long)]
    pub numstat: bool,

    /// Show diff statistics
    #[clap(long)]
    pub stat: bool,

    /// Generate the patch with `<n>` lines of context (default 3). Changes only
    /// the surrounding context, not the +/- lines, so `--stat`/`--name-only`/
    /// `--numstat` counts are unaffected; the `--json` hunk ranges/lines follow `<n>`.
    #[clap(short = 'U', long = "unified", value_name = "N")]
    pub unified: Option<usize>,

    /// Ignore whitespace entirely when comparing lines: a change that is only
    /// whitespace is not reported (the file drops out if that is its only change),
    /// and context lines are shown from the new side. This re-diffs affected files,
    /// so `--stat`/`--name-only`/`--numstat`/JSON all reflect the whitespace-ignored
    /// result. Honors `-U<n>`.
    #[clap(short = 'w', long = "ignore-all-space")]
    pub ignore_all_space: bool,

    /// Ignore changes in the amount of whitespace: runs of whitespace are treated
    /// as a single space and trailing whitespace is ignored (so `a  b` matches
    /// `a b`, but `a b` still differs from `ab`). Same re-diff behavior as `-w`;
    /// `-w` takes precedence if both are given.
    #[clap(short = 'b', long = "ignore-space-change")]
    pub ignore_space_change: bool,

    /// Ignore whitespace changes at end of line only; leading and internal
    /// whitespace compare exactly. Same re-diff behavior as `-w`; `-w`/`-b` take
    /// precedence if combined.
    #[clap(long = "ignore-space-at-eol")]
    pub ignore_space_at_eol: bool,

    /// Ignore a carriage return at end of line: trailing `\r`s are stripped
    /// before comparing, so a CRLF↔LF-only change drops out. The weakest
    /// whitespace flag — `-w`/`-b`/`--ignore-space-at-eol` each already ignore a
    /// trailing `\r` (it is whitespace) and take precedence when combined. A
    /// mid-line `\r` still compares exactly. (Known approximation vs Git: Git
    /// allows at most ONE trailing CR to remain on each side — a non-transitive
    /// relation no per-line normalizer can express — so a pathological
    /// multi-CR ending like `a\r\r\r\n` vs `a\r\n` matches here but
    /// differs in Git; the everyday CRLF↔LF and `\r\r\n`↔`\r\n` cases
    /// match Git.)
    #[clap(long = "ignore-cr-at-eol")]
    pub ignore_cr_at_eol: bool,

    /// Ignore changes whose lines are all blank: a change consisting only of
    /// added/removed empty lines is not reported (an added/deleted file whose only
    /// content is blank lines is still listed with zero counts), while a change
    /// near a real edit is shown in full. Only truly empty lines count as blank (a
    /// `\r`-only CRLF line is not blank). Re-diffs affected files so
    /// `--stat`/`--name-only`/`--numstat`/JSON reflect the result; honors `-U<n>`.
    /// Composes with a whitespace flag (`-w`/`-b`/`--ignore-space-at-eol`/
    /// `--ignore-cr-at-eol`): under any whitespace flag an all-whitespace line
    /// counts as blank (matching Git's `xdl_blankline`).
    #[clap(long = "ignore-blank-lines")]
    pub ignore_blank_lines: bool,

    /// Show a condensed summary of created and deleted files
    #[clap(long)]
    pub summary: bool,

    /// Output only the last line of `--stat`: the files-changed / insertions /
    /// deletions summary.
    #[clap(long)]
    pub shortstat: bool,

    /// Exit with code 1 when there are differences, 0 when there are none
    /// (the diff is still printed, unlike `--quiet`).
    #[clap(long = "exit-code")]
    pub exit_code: bool,

    /// Suppress the patch (diff body) output. Combine with `--exit-code` for a
    /// status-only check.
    #[clap(short = 's', long = "no-patch")]
    pub no_patch: bool,

    /// NUL-terminate output records (for `--name-only`/`--name-status`/`--numstat`);
    /// `--name-status` then emits the status and path as separate NUL fields.
    #[clap(short = 'z', long = "null")]
    pub null: bool,

    /// Warn about whitespace errors on added lines instead of printing the diff.
    /// Detects trailing whitespace and space-before-tab in the indent; exits 2
    /// when any problem is found. (Git's blank-at-eof check is not performed.)
    /// Unaffected by `-w`/`-b`/`--ignore-space-at-eol` — like Git, the scan runs
    /// on the full diff, so added trailing whitespace is still reported.
    #[clap(long = "check")]
    pub check: bool,

    /// Show the reverse diff: swap the two sides so additions become deletions
    /// and vice-versa (e.g. the patch that would undo the change).
    #[clap(short = 'R', long = "reverse")]
    pub reverse: bool,

    /// Treat all files as text: diff the content even of files Libra would
    /// otherwise detect as binary (a NUL byte in either side), suppressing the
    /// "Binary files … differ" line and the `--binary` patch.
    #[clap(short = 'a', long = "text")]
    pub text: bool,

    /// Output a binary patch (`GIT binary patch` with base85-encoded literals for
    /// both directions) for files detected as binary, instead of "Binary files …
    /// differ". The patch is valid and appliable, but its compressed bytes are not
    /// byte-identical to Git's (Libra deflates with `flate2`, and always emits a
    /// `literal` chunk rather than Git's smaller-of-literal/delta choice).
    #[clap(long = "binary")]
    pub binary: bool,

    /// Disable the external diff driver (`diff.external`) for this run, forcing
    /// the built-in diff engine even when one is configured.
    #[clap(long = "no-ext-diff")]
    pub no_ext_diff: bool,

    /// Color moved lines (lines deleted in one place and added in another) with a
    /// distinct color in terminal output. Bare `--color-moved` and the
    /// block-significance modes (`default`/`zebra`/`blocks`/`dimmed-zebra`) are
    /// accepted but approximated by `plain` — Libra colors EVERY moved line
    /// (removed → bold magenta, added → bold cyan); it does not implement Git's
    /// conservative moved-block significance/zebra striping. `--color-moved=no`
    /// or `--no-color-moved` turns it off (the default). Only affects colored
    /// output (a terminal or `--color=always`).
    // `require_equals` is safe here (unlike `-M`, this is long-only with no glued
    // short form): bare `--color-moved` uses the default mode, `--color-moved=<m>`
    // sets it, and `--color-moved <pathspec>` is NOT swallowed as the mode.
    #[clap(
        long = "color-moved",
        value_name = "mode",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "default",
        overrides_with = "no_color_moved"
    )]
    pub color_moved: Option<String>,

    /// Do not color moved lines differently from added/removed lines (the default;
    /// countermands an earlier `--color-moved`).
    #[clap(long = "no-color-moved", overrides_with = "color_moved")]
    pub no_color_moved: bool,

    /// Detect renames: a deleted + added pair whose content is similar enough is
    /// reported as a single rename (`similarity index N%` / `rename from`/`to`).
    /// `-M`/`--find-renames` alone uses a 50% threshold; `-M<n>` / `-M<n>%` /
    /// `--find-renames=<n>` set it (e.g. `-M90%`, `-M100%` for exact-only).
    /// `--no-renames` countermands it.
    // Optional value: bare `-M`/`--find-renames` is 50%; a glued/`=`-attached
    // value sets the threshold. We deliberately do NOT set `require_equals`,
    // because that would reject Git's standard glued short form `-M90`. The
    // trade-off is that a pathspec must not directly follow a bare `-M` /
    // `--find-renames` (it would be read as the score); place pathspecs before
    // the flag, after `--`, or use a glued threshold (`-M50 <pathspec>`).
    #[clap(
        short = 'M',
        long = "find-renames",
        value_name = "n",
        num_args = 0..=1,
        default_missing_value = "50",
        overrides_with = "no_renames"
    )]
    pub find_renames: Option<String>,

    /// Turn off rename detection (the default, and countermands an earlier
    /// `-M`/`--find-renames`).
    #[clap(long = "no-renames", overrides_with = "find_renames")]
    pub no_renames: bool,

    /// Show paths relative to the repository root, not the current directory.
    /// This is Libra's default; the flag is accepted for Git parity and takes
    /// precedence over `--relative` (when both are given, relative output is disabled).
    #[clap(long = "no-relative")]
    pub no_relative: bool,

    /// Restrict the diff to a directory and show paths relative to it. With a value,
    /// `--relative=<path>` uses `<path>` (resolved from the current directory); bare
    /// `--relative` uses the current directory. Paths outside the directory are
    /// excluded and the directory prefix is stripped from displayed paths.
    #[clap(
        long = "relative",
        value_name = "PATH",
        num_args = 0..=1,
        require_equals = true
    )]
    pub relative: Option<Option<String>>,

    /// Disable the indent heuristic for hunk boundaries. Accepted for Git parity
    /// and is a no-op: Libra's diff does not apply Git's indent heuristic.
    /// (Git's `--indent-heuristic` is not exposed.)
    #[clap(long = "no-indent-heuristic")]
    pub no_indent_heuristic: bool,

    /// Run textconv filters to make content human-diffable: a file whose
    /// `diff=<driver>` attribute (in `.libra_attributes`) names a driver with a
    /// `diff.<driver>.textconv` command has each side converted by that command
    /// before diffing. Like Git, textconv is ON by default for `diff`; this flag
    /// is the explicit opposite of `--no-textconv`. The resulting patch is for
    /// reading, not applying.
    #[clap(long = "textconv", overrides_with = "no_textconv")]
    pub textconv: bool,

    /// Do not run textconv filters; diff the raw content (countermands an earlier
    /// `--textconv`). Textconv is otherwise on by default when a file's
    /// `diff=<driver>` attribute names a driver with a `diff.<driver>.textconv`
    /// command configured.
    #[clap(long = "no-textconv", overrides_with = "textconv")]
    pub no_textconv: bool,

    /// Allow an external diff driver (`diff.external`) to generate the patch.
    /// Accepted for Git parity: when `diff.external` is configured, each file's
    /// patch is produced by that command (GIT_EXTERNAL_DIFF protocol) unless
    /// `--no-ext-diff` is given. Has no effect when `diff.external` is unset.
    #[clap(long = "ext-diff", overrides_with = "no_ext_diff")]
    pub ext_diff: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_lines: usize,
    pub new_start: usize,
    pub new_lines: usize,
    pub lines: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffFileStat {
    pub path: String,
    pub status: String,
    pub insertions: usize,
    pub deletions: usize,
    pub hunks: Vec<DiffHunk>,
    #[serde(skip_serializing)]
    raw_diff: String,
    /// For a detected rename (`-M`), the original path; `path` holds the new
    /// name. `None` for non-rename entries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename_from: Option<String>,
    /// For a detected rename, the similarity index as a whole percent (0-100).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub similarity: Option<u32>,
    /// For a binary file, the `(old_size, new_size)` byte counts (used by
    /// `--stat`'s `Bin <old> -> <new> bytes` and to mark `--numstat` with `-`).
    /// `None` for text files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub binary: Option<(u64, u64)>,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiffOutput {
    pub old_ref: String,
    pub new_ref: String,
    pub files: Vec<DiffFileStat>,
    pub total_insertions: usize,
    pub total_deletions: usize,
    pub files_changed: usize,
    /// Set when an external diff driver (`diff.external`) produced the patch
    /// bodies; the caller then skips the internal word-diff/relative transforms.
    #[serde(skip)]
    pub external_diff_applied: bool,
    /// Set when `--binary` produced a `GIT binary patch`; the patch body must be
    /// rendered verbatim so the blank-line terminator after each literal survives
    /// (Git's binary-patch parser requires it).
    #[serde(skip)]
    pub binary_patch: bool,
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum DiffError {
    #[error("not a libra repository")]
    NotInRepo,

    #[error("invalid revision: '{0}'")]
    InvalidRevision(String),

    #[error("failed to load {kind} '{object_id}': {detail}")]
    ObjectLoad {
        kind: &'static str,
        object_id: String,
        detail: String,
    },

    #[error("failed to load index: {0}")]
    IndexLoad(String),

    #[error("failed to list working directory files: {0}")]
    WorkdirList(String),

    #[error("failed to read file '{path}': {detail}")]
    FileRead { path: String, detail: String },

    #[error("failed to write output file '{path}': {detail}")]
    OutputWrite { path: String, detail: String },

    #[error(
        "diff --algorithm={0} is not supported yet; only --algorithm=histogram is currently implemented"
    )]
    UnsupportedAlgorithm(String),

    #[error("invalid argument to find-renames: '{0}'")]
    InvalidRenameScore(String),

    #[error("invalid argument to color-moved: '{0}'")]
    InvalidColorMoved(String),

    #[error("textconv filter '{command}' failed: {detail}")]
    TextconvFailed { command: String, detail: String },

    /// A leading positional is BOTH a resolvable revision and an existing file
    /// and no `--` separator was given — Git's ambiguity error.
    #[error("ambiguous argument '{0}': both a revision and a filename")]
    AmbiguousArgument(String),

    /// A pre-`--` positional neither resolves as a revision nor exists as a
    /// path (Git's `unknown revision or path not in the working tree`).
    #[error("unknown revision or path not in the working tree: '{0}'")]
    UnknownRevisionOrPath(String),

    /// More than two positional revisions were given. Git ≥2.38 accepts this
    /// as the combined-diff form for a merge; Libra has no combined diff, so
    /// it is a declined surface (documented in COMPATIBILITY.md).
    #[error("more than two revisions given: '{0}'")]
    TooManyRevisions(String),

    /// `--staged` combines with at most ONE revision (commit vs index); a
    /// range or a second revision is meaningless there.
    #[error("--staged compares a single commit against the index; '{0}' is one revision too many")]
    StagedRevisionRange(String),

    /// `A...B` where both sides resolve but share no merge base.
    #[error("no merge base found for '{left}' and '{right}'")]
    NoMergeBase { left: String, right: String },
}

impl From<DiffError> for CliError {
    fn from(error: DiffError) -> Self {
        let message = error.to_string();
        match error {
            DiffError::NotInRepo => CliError::repo_not_found(),
            DiffError::InvalidRevision(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("check the revision name and try again"),
            DiffError::ObjectLoad { .. } => CliError::fatal(message)
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the object store may be corrupted; try 'libra status' to verify"),
            DiffError::IndexLoad(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::RepoCorrupt)
                .with_hint("the index file may be corrupted"),
            DiffError::WorkdirList(_) => {
                CliError::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
            }
            DiffError::FileRead { .. } => {
                CliError::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
            }
            DiffError::OutputWrite { .. } => {
                CliError::fatal(message).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            DiffError::UnsupportedAlgorithm(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(
                    "omit --algorithm or use --algorithm=histogram until alternate diff backends are available",
                ),
            DiffError::InvalidRenameScore(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(
                    "use -M, -M<n> (e.g. -M90%), or --find-renames=<n>; a pathspec after a bare -M must follow '--'",
                ),
            DiffError::InvalidColorMoved(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("expected no, default, plain, blocks, zebra, or dimmed-zebra"),
            DiffError::TextconvFailed { .. } => CliError::fatal(message)
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint(
                    "check the diff.<driver>.textconv command, or pass --no-textconv to diff raw content",
                ),
            DiffError::AmbiguousArgument(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(
                    "use '--' to separate paths from revisions: libra diff <revision>... -- <path>...",
                ),
            DiffError::UnknownRevisionOrPath(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint(
                    "use '--' to separate paths from revisions: libra diff <revision>... -- <path>...",
                ),
            DiffError::TooManyRevisions(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("libra diff takes at most two revisions; put paths after '--'"),
            DiffError::StagedRevisionRange(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("drop --staged, or pass a single revision: libra diff --staged <commit>"),
            DiffError::NoMergeBase { .. } => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("the two revisions share no common ancestor"),
        }
    }
}

pub async fn execute(args: DiffArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

pub async fn execute_safe(args: DiffArgs, output: &OutputConfig) -> CliResult<()> {
    if util::require_repo().is_err() {
        return Err(CliError::from(DiffError::NotInRepo));
    }
    let mut args = args;
    resolve_positional_revisions(&mut args)
        .await
        .map_err(CliError::from)?;
    validate_diff_algorithm(&args).map_err(CliError::from)?;
    emit_worktree_scan_progress(&args, output);
    let mut result = run_diff(&args, output).await.map_err(CliError::from)?;
    // lore.md 2.2: read-only sparse view — scope ONLY the working-tree diff
    // (unstaged: new side is the worktree, not `--staged`, not rev-vs-rev), the
    // one that is pure browsing. `--staged` (index-vs-HEAD, commit-authoritative)
    // and `A..B` (rev-vs-rev) are NEVER filtered, so diff never hides what a
    // commit will record. Applied on repo-root-relative paths BEFORE the
    // `--relative` prefix strip.
    if !result.external_diff_applied && !args.staged && args.new.is_none() {
        apply_sparse_view_filter(&mut result).await;
    }
    // External-driver output is verbatim: skip the internal relative-path rewrite
    // and word-diff transforms (they would mangle the driver's own format).
    if !result.external_diff_applied {
        apply_relative_filter(&args, &mut result);
        apply_word_diff(&args, &mut result, output, io::stdout().is_terminal())?;
    }
    render_diff_output(&args, &result, output)
}

/// Whether `--word-diff` is set to a rendering mode (i.e. not `none`/absent), in
/// which case the diff body is already fully rendered and must not be re-colored.
fn word_diff_active(args: &DiffArgs) -> bool {
    matches!(args.word_diff.as_deref(), Some(mode) if mode != "none")
}

/// The `--relative[=<path>]` directory prefix (with a trailing `/`) that the diff
/// is restricted to, or `None` when `--no-relative`, no `--relative`, or a cwd at
/// the repo root means "no restriction".
fn relative_prefix(args: &DiffArgs) -> Option<String> {
    if args.no_relative {
        return None;
    }
    let raw_prefix = match &args.relative {
        None => return None,
        Some(Some(path)) => util::to_workdir_path(path),
        Some(None) => util::to_workdir_path("."),
    };
    let prefix = raw_prefix.to_string_lossy().replace('\\', "/");
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() || prefix == "." {
        return None;
    }
    Some(format!("{prefix}/"))
}

/// Apply `--relative[=<path>]`: keep only files under the directory prefix and strip
/// that prefix from every displayed path (the file path, the patch's
/// `diff --git`/`---`/`+++`/`rename|copy from|to` lines, and — via `path` — `--stat`,
/// JSON, and create/delete mode summaries). `--no-relative` and a cwd at the repo
/// root are no-ops. The file-set restriction is also applied (without path
/// rewriting) inside `run_diff` before an external driver runs, so this rewrite
/// pass is skipped for external output.
/// lore.md 2.2: retain only in-view files in a working-tree diff and recompute
/// the stat totals. A no-op when the view is inactive.
async fn apply_sparse_view_filter(result: &mut DiffOutput) {
    let view = crate::internal::sparse::SparseView::load().await;
    if !view.is_active() {
        return;
    }
    result.files.retain(|file| {
        // A rename's old path counts as in-view too (either side visible).
        view.contains_str(&file.path)
            || file
                .rename_from
                .as_deref()
                .is_some_and(|from| view.contains_str(from))
    });
    result.files_changed = result.files.len();
    result.total_insertions = result.files.iter().map(|file| file.insertions).sum();
    result.total_deletions = result.files.iter().map(|file| file.deletions).sum();
}

fn apply_relative_filter(args: &DiffArgs, result: &mut DiffOutput) {
    let Some(strip) = relative_prefix(args) else {
        return;
    };

    result.files.retain(|file| file.path.starts_with(&strip));
    for file in &mut result.files {
        // A rename carries the old path on its `a/` side (`diff --git a/<old>`,
        // `--- a/<old>`, `rename from <old>`) and in the `rename_from` field used
        // by name-status/numstat/stat/summary. Strip that prefix first (a separate
        // pass keyed on the old path), then the new-path pass handles the `b/` side.
        if let Some(from) = file.rename_from.clone()
            && let Some(rest) = from.strip_prefix(&strip)
        {
            file.raw_diff = strip_relative_prefix_in_diff(&file.raw_diff, &strip, &from, rest);
            file.rename_from = Some(rest.to_string());
        }
        let full = file.path.clone();
        let stripped = full[strip.len()..].to_string();
        file.raw_diff = strip_relative_prefix_in_diff(&file.raw_diff, &strip, &full, &stripped);
        file.path = stripped;
    }
    result.files_changed = result.files.len();
    result.total_insertions = result.files.iter().map(|file| file.insertions).sum();
    result.total_deletions = result.files.iter().map(|file| file.deletions).sum();
}

/// Word-diff rendering mode (`--word-diff=<MODE>`), excluding `none` (which
/// disables the transform entirely).
#[derive(Clone, Copy, PartialEq, Eq)]
enum WordDiffMode {
    Plain,
    Color,
    Porcelain,
}

/// Resolve a `--word-diff` value to a mode, or `None` for `none` (no transform).
fn resolve_word_diff_mode(value: &str) -> CliResult<Option<WordDiffMode>> {
    match value {
        "none" => Ok(None),
        "plain" => Ok(Some(WordDiffMode::Plain)),
        "color" => Ok(Some(WordDiffMode::Color)),
        "porcelain" => Ok(Some(WordDiffMode::Porcelain)),
        other => Err(CliError::command_usage(format!(
            "invalid --word-diff mode '{other}' (expected plain, color, porcelain, or none)"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments)),
    }
}

/// Apply `--word-diff`: rewrite each file's unified diff body into word-diff
/// form (the headers/`@@` lines are kept; each hunk's old side vs new side is
/// re-diffed at word granularity). `none`/absent is a no-op.
fn apply_word_diff(
    args: &DiffArgs,
    result: &mut DiffOutput,
    output: &OutputConfig,
    color: bool,
) -> CliResult<()> {
    let Some(value) = &args.word_diff else {
        return Ok(());
    };
    // Resolve (and validate) the mode even when another output mode wins, so an
    // invalid `--word-diff=<bad>` is still reported.
    let Some(mode) = resolve_word_diff_mode(value)? else {
        return Ok(());
    };
    // Word-diff only rewrites the textual patch body. Summary/check/JSON paths
    // read `raw_diff` (or the per-file stats) differently — e.g. `--check`
    // scans `raw_diff` for added-line whitespace errors — so leave it untouched
    // for them (matching Git, where those modes ignore `--word-diff`). A
    // status-only `--quiet` with no `--output` emits no patch, so skip the
    // (potentially large) transform; `--quiet --output <file>` still writes the
    // file and so must run it.
    if args.check
        || args.name_only
        || args.name_status
        || args.numstat
        || args.stat
        || args.shortstat
        || args.summary
        || args.no_patch
        || output.is_json()
        || (output.quiet && args.output.is_none())
    {
        return Ok(());
    }
    for file in &mut result.files {
        file.raw_diff = word_diff_transform(&file.raw_diff, mode, color);
    }
    Ok(())
}

/// Rewrite one file's unified diff text into the chosen word-diff mode. Header
/// lines (`diff --git`, `index`, `---`, `+++`, `@@`) are preserved; each hunk's
/// body is reconstructed into its old side (context + removed lines) and new
/// side (context + added lines), word-diffed, and re-rendered.
fn word_diff_transform(raw_diff: &str, mode: WordDiffMode, color: bool) -> String {
    let lines: Vec<&str> = raw_diff.lines().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        if !line.starts_with("@@") {
            out.push_str(line);
            out.push('\n');
            i += 1;
            continue;
        }
        // Hunk header: keep it, then collect the body up to the next hunk/EOF.
        out.push_str(line);
        out.push('\n');
        i += 1;
        let mut old_lines: Vec<&str> = Vec::new();
        let mut new_lines: Vec<&str> = Vec::new();
        while i < lines.len() && !lines[i].starts_with("@@") {
            let body = lines[i];
            match body.as_bytes().first() {
                Some(b' ') => {
                    let content = &body[1..];
                    old_lines.push(content);
                    new_lines.push(content);
                }
                Some(b'-') => old_lines.push(&body[1..]),
                Some(b'+') => new_lines.push(&body[1..]),
                // "\ No newline at end of file" and any stray line: leave out of
                // the word diff (its presence does not change words).
                _ => {}
            }
            i += 1;
        }
        // Append the trailing newline that each hunk line carried in the source
        // (the common case — files ending in a newline), so the final line break
        // is word-diffed too (e.g. porcelain's closing `~`).
        let with_trailing = |lines: &[&str]| -> String {
            if lines.is_empty() {
                String::new()
            } else {
                format!("{}\n", lines.join("\n"))
            }
        };
        let old_side = with_trailing(&old_lines);
        let new_side = with_trailing(&new_lines);
        out.push_str(&render_word_diff(&old_side, &new_side, mode, color));
    }
    out
}

/// Split text into word-diff tokens: a single newline, a run of non-newline
/// whitespace, or a run of non-whitespace (a "word"). Matches Git's default
/// whitespace-delimited tokenization (`--word-diff-regex` is not supported).
fn word_tokens(text: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let mut chars = text.char_indices().peekable();
    while let Some(&(start, c)) = chars.peek() {
        if c == '\n' {
            tokens.push(&text[start..start + 1]);
            chars.next();
        } else if c.is_whitespace() {
            let mut end = start + c.len_utf8();
            chars.next();
            while let Some(&(idx, ch)) = chars.peek() {
                if ch == '\n' || !ch.is_whitespace() {
                    break;
                }
                end = idx + ch.len_utf8();
                chars.next();
            }
            tokens.push(&text[start..end]);
        } else {
            let mut end = start + c.len_utf8();
            chars.next();
            while let Some(&(idx, ch)) = chars.peek() {
                if ch.is_whitespace() {
                    break;
                }
                end = idx + ch.len_utf8();
                chars.next();
            }
            tokens.push(&text[start..end]);
        }
    }
    tokens
}

/// Whether a token is "delimiter" whitespace: a non-newline run made entirely of
/// whitespace. Newlines are hard line boundaries, never trimmed.
fn is_delimiter_whitespace(token: &str) -> bool {
    token != "\n" && token.chars().all(char::is_whitespace)
}

/// Normalize a token-level change list so that whitespace behaves as a delimiter
/// (matching Git): within each run of consecutive same-tag changed words,
/// leading/trailing delimiter-whitespace is re-tagged `Equal` for inserts (it
/// stays a plain separator) and dropped for deletes (deleted spacing is not
/// shown), while whitespace *inside* a multi-word run stays in the marker.
/// Newlines bound runs and are left untouched.
fn normalize_word_changes(changes: Vec<(ChangeTag, &str)>) -> Vec<(ChangeTag, &str)> {
    let mut out: Vec<(ChangeTag, &str)> = Vec::with_capacity(changes.len());
    let mut i = 0;
    while i < changes.len() {
        let (tag, token) = changes[i];
        if tag == ChangeTag::Equal || token == "\n" {
            out.push(changes[i]);
            i += 1;
            continue;
        }
        // Collect a maximal run of this changed tag, stopping at a newline.
        let run_tag = tag;
        let start = i;
        while i < changes.len() && changes[i].0 == run_tag && changes[i].1 != "\n" {
            i += 1;
        }
        let run = &changes[start..i];
        let first_word = run.iter().position(|(_, t)| !is_delimiter_whitespace(t));
        let keep_boundary = run_tag == ChangeTag::Insert;
        match first_word {
            // Whole run is delimiter whitespace: keep (as Equal) for inserts,
            // drop for deletes.
            None => {
                if keep_boundary {
                    out.extend(run.iter().map(|&(_, t)| (ChangeTag::Equal, t)));
                }
            }
            Some(first) => {
                // INVARIANT: reaching the `Some(first)` arm means `position` with
                // this same predicate already found a non-delimiter-whitespace
                // token in `run`, so `rposition` (identical predicate, scanning
                // from the back) must find at least that token — `first <= last`.
                let last = run
                    .iter()
                    .rposition(|(_, t)| !is_delimiter_whitespace(t))
                    .expect("INVARIANT: run contains a non-whitespace token (first_word matched)");
                if keep_boundary {
                    out.extend(run[..first].iter().map(|&(_, t)| (ChangeTag::Equal, t)));
                }
                out.extend_from_slice(&run[first..=last]);
                if keep_boundary {
                    out.extend(run[last + 1..].iter().map(|&(_, t)| (ChangeTag::Equal, t)));
                }
            }
        }
    }
    out
}

/// Word-diff `old` vs `new` and render the body in the chosen mode (ending with
/// a trailing newline). Newlines always break lines and close any open marker.
fn render_word_diff(old: &str, new: &str, mode: WordDiffMode, color: bool) -> String {
    let old_toks = word_tokens(old);
    let new_toks = word_tokens(new);
    let diff = TextDiff::from_slices(&old_toks, &new_toks);
    let changes: Vec<(ChangeTag, &str)> = normalize_word_changes(
        diff.iter_all_changes()
            .map(|change| (change.tag(), change.value()))
            .collect(),
    );

    if mode == WordDiffMode::Porcelain {
        return render_word_porcelain(&changes);
    }

    // Plain / color: emit a running line per output line; removed-word runs are
    // wrapped `[-…-]` and added runs `{+…+}` (or colored, bracket-less, when
    // `color`). A newline token closes any open marker and breaks the line.
    let mut out = String::new();
    let mut run: Vec<&str> = Vec::new();
    let mut run_tag = ChangeTag::Equal;
    let flush = |out: &mut String, run: &mut Vec<&str>, tag: ChangeTag| {
        if run.is_empty() {
            return;
        }
        let text = run.concat();
        match tag {
            ChangeTag::Equal => out.push_str(&text),
            ChangeTag::Delete => {
                if color {
                    out.push_str(&text.red().to_string());
                } else {
                    out.push_str("[-");
                    out.push_str(&text);
                    out.push_str("-]");
                }
            }
            ChangeTag::Insert => {
                if color {
                    out.push_str(&text.green().to_string());
                } else {
                    out.push_str("{+");
                    out.push_str(&text);
                    out.push_str("+}");
                }
            }
        }
        run.clear();
    };
    for &(tag, token) in &changes {
        if token == "\n" {
            flush(&mut out, &mut run, run_tag);
            out.push('\n');
            continue;
        }
        if tag != run_tag {
            flush(&mut out, &mut run, run_tag);
            run_tag = tag;
        }
        run.push(token);
    }
    flush(&mut out, &mut run, run_tag);
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Render the porcelain word-diff body: consecutive same-tag tokens become one
/// line prefixed by ` ` (context), `-` (removed), or `+` (added); each newline
/// becomes a `~` line.
fn render_word_porcelain(changes: &[(ChangeTag, &str)]) -> String {
    let mut out = String::new();
    let mut run: Vec<&str> = Vec::new();
    let mut run_tag = ChangeTag::Equal;
    let flush = |out: &mut String, run: &mut Vec<&str>, tag: ChangeTag| {
        if run.is_empty() {
            return;
        }
        let prefix = match tag {
            ChangeTag::Equal => ' ',
            ChangeTag::Delete => '-',
            ChangeTag::Insert => '+',
        };
        out.push(prefix);
        out.push_str(&run.concat());
        out.push('\n');
        run.clear();
    };
    for &(tag, token) in changes {
        if token == "\n" {
            flush(&mut out, &mut run, run_tag);
            out.push_str("~\n");
            continue;
        }
        if tag != run_tag {
            flush(&mut out, &mut run, run_tag);
            run_tag = tag;
        }
        run.push(token);
    }
    flush(&mut out, &mut run, run_tag);
    out
}

/// Strip the relative directory prefix from the path-bearing lines of a single file's
/// unified diff text, leaving hunk/content lines untouched.
///
/// `diff --git`/`---`/`+++` lines use EXACT replacement of the known full path (`full`
/// → `stripped`) rather than splitting on ` b/`, so a path that itself contains a
/// space and a `b/` fragment is not corrupted. `rename`/`copy from|to` lines (Libra's
/// diff does not currently emit them, since it reports no renames) carry a single
/// path, so a prefix strip is unambiguous.
fn strip_relative_prefix_in_diff(
    raw_diff: &str,
    strip: &str,
    full: &str,
    stripped: &str,
) -> String {
    let had_trailing_newline = raw_diff.ends_with('\n');
    let mut lines: Vec<String> = raw_diff
        .lines()
        .map(|line| strip_relative_prefix_in_line(line, strip, full, stripped))
        .collect();
    if had_trailing_newline {
        lines.push(String::new());
    }
    lines.join("\n")
}

fn strip_relative_prefix_in_line(line: &str, strip: &str, full: &str, stripped: &str) -> String {
    if line.starts_with("diff --git ")
        || line.starts_with("--- ")
        || line.starts_with("+++ ")
        || line.starts_with("<LargeFile>")
        || line.starts_with("Binary files ")
    {
        // Exact replacement of the `a/<full>`/`b/<full>` path positions (also in a
        // `Binary files a/<full> and b/<full> differ` line), plus the
        // `<LargeFile><full>:…</LargeFile>` marker emitted for over-large files.
        return line
            .replace(&format!("a/{full}"), &format!("a/{stripped}"))
            .replace(&format!("b/{full}"), &format!("b/{stripped}"))
            .replace(
                &format!("<LargeFile>{full}"),
                &format!("<LargeFile>{stripped}"),
            );
    }
    for keyword in ["rename from ", "rename to ", "copy from ", "copy to "] {
        if let Some(path) = line.strip_prefix(keyword) {
            return match path.strip_prefix(strip) {
                Some(remainder) => format!("{keyword}{remainder}"),
                None => line.to_string(),
            };
        }
    }
    line.to_string()
}

/// Whether the raw argv for this `diff` invocation carried a `--` separator.
/// clap consumes a value-less trailing `--` without a trace (`after_dashdash`
/// stays empty), so recover it from `std::env::args()` — consulted ONLY when
/// `after_dashdash` is empty, keeping `DiffArgs::parse_from` unit tests
/// deterministic (same pattern as rev-parse). Caveat inherited from rev-parse:
/// an earlier argv token literally equal to `diff` could confuse the scan.
fn bare_dashdash_in_diff_argv() -> bool {
    let argv: Vec<String> = std::env::args().collect();
    match argv.iter().position(|a| a == "diff") {
        Some(idx) => argv[idx + 1..].iter().any(|a| a == "--"),
        None => false,
    }
}

/// Whether `tok` names something on disk (cwd-relative). Uses
/// `symlink_metadata` so a dangling symlink still counts as a path (Git's
/// `check_filename` lstats).
fn exists_as_path(tok: &str) -> bool {
    std::path::Path::new(tok).symlink_metadata().is_ok()
}

/// Whether `tok` carries pathspec glob magic. Git's `check_filename` exempts
/// wildcard pathspecs from the unknown-revision-or-path error (`git diff '*.c'`
/// works with no literal `*.c` file); mirror that so globs stay pathspecs.
fn has_glob_magic(tok: &str) -> bool {
    tok.contains(['*', '?', '['])
}

/// Resolve leading positional revisions and the `--` pathspec separator,
/// matching Git's `diff [<revision>...] [--] [<path>...]` grammar
/// (lore.md §1.4):
///
/// - `A..B` / `A...B` glued ranges as the first positional (three-dot diffs
///   from `merge-base(A,B)` to `B`; empty sides default to HEAD).
/// - Bare revisions: `diff A` (A vs worktree), `diff A B` (≡ `A..B`), and
///   `diff --staged A` (A vs index).
/// - Everything after `--` is a pathspec, never a revision.
/// - Git's two disambiguation errors: a pre-`--` token that is BOTH a
///   revision and an existing file is `ambiguous argument`; one that is
///   neither is `unknown revision or path not in the working tree` (globs
///   exempt). With a `--` present, every pre-`--` token must be a revision.
///
/// The Libra-only `--old`/`--new` flags keep their documented leniency: when
/// given, positionals stay pathspecs and no ambiguity walk runs. Note an
/// ambiguous object-name PREFIX folds into the unknown-revision error rather
/// than Git's distinct `ambiguous object name` message (documented).
async fn resolve_positional_revisions(args: &mut DiffArgs) -> Result<(), DiffError> {
    let dashdash = !args.after_dashdash.is_empty() || bare_dashdash_in_diff_argv();
    // Post-`--` tokens are pathspecs verbatim (no existence check, matching
    // `git diff -- nosuch` → empty diff). Fold them in up front.
    let trailing_paths = std::mem::take(&mut args.after_dashdash);

    if args.old.is_some() || args.new.is_some() {
        args.pathspec.extend(trailing_paths);
        return Ok(());
    }

    let mut revisions = 0usize;
    let max_revisions = if args.staged { 1 } else { 2 };

    // Glued range (`A..B` / `A...B`) as the first positional. `...` first —
    // it contains `..`.
    if let Some(first) = args.pathspec.first().cloned() {
        let range_result: Option<Result<(), DiffError>> =
            if let Some((left, right)) = first.split_once("...") {
                let left_spec = if left.is_empty() { "HEAD" } else { left };
                let right_spec = if right.is_empty() { "HEAD" } else { right };
                let sides = (
                    crate::utils::util::get_commit_base(left_spec).await,
                    crate::utils::util::get_commit_base(right_spec).await,
                );
                match sides {
                    (Ok(left_id), Ok(right_id)) => {
                        match crate::internal::merge_base::merge_base(&left_id, &right_id) {
                            Ok(Some(base)) => {
                                args.old = Some(base.to_string());
                                args.new = Some(right_spec.to_string());
                                Some(Ok(()))
                            }
                            // Both sides resolve but share no ancestor: a clear
                            // error, not a silent fall-through to pathspec.
                            _ => Some(Err(DiffError::NoMergeBase {
                                left: left_spec.to_string(),
                                right: right_spec.to_string(),
                            })),
                        }
                    }
                    _ => None, // not a resolvable range; fall through to token rules
                }
            } else if let Some((left, right)) = first.split_once("..") {
                let left_spec = if left.is_empty() { "HEAD" } else { left };
                let left_ok = crate::command::get_target_commit(left_spec).await.is_ok();
                let right_ok =
                    right.is_empty() || crate::command::get_target_commit(right).await.is_ok();
                if left_ok && right_ok {
                    args.old = Some(left_spec.to_string());
                    if !right.is_empty() {
                        args.new = Some(right.to_string());
                    }
                    Some(Ok(()))
                } else {
                    None
                }
            } else {
                None
            };

        match range_result {
            Some(Err(error)) => return Err(error),
            Some(Ok(())) => {
                if args.staged {
                    // A range names two endpoints; the index IS the new side.
                    return Err(DiffError::StagedRevisionRange(first));
                }
                args.pathspec.remove(0);
                revisions = 2; // a consumed range uses up both revision slots
            }
            None => {}
        }
    }

    // Walk the remaining leading positionals as bare revisions.
    let mut remaining: Vec<String> = Vec::with_capacity(args.pathspec.len());
    let mut revs_done = revisions >= max_revisions && revisions > 0;
    let mut paths_started = false;
    for tok in std::mem::take(&mut args.pathspec) {
        if paths_started {
            remaining.push(tok);
            continue;
        }
        let resolves = crate::command::get_target_commit(&tok).await.is_ok();
        let is_path = exists_as_path(&tok);
        if resolves && is_path && !dashdash {
            return Err(DiffError::AmbiguousArgument(tok));
        }
        // A resolving token is a revision — unconditionally when `--` is
        // present (that is the separator's whole purpose: pre-`--` tokens are
        // revisions even when a file of the same name exists).
        if resolves && (dashdash || !is_path) {
            if args.staged && revisions >= 1 {
                return Err(DiffError::StagedRevisionRange(tok));
            }
            if revisions >= max_revisions || revs_done {
                return Err(DiffError::TooManyRevisions(tok));
            }
            if revisions == 0 {
                args.old = Some(tok);
            } else {
                args.new = Some(tok);
            }
            revisions += 1;
            continue;
        }
        // Not a revision (or shadowed by an existing file): pathspec territory.
        // With a `--` present every pre-`--` token must be a revision; without
        // one, a token that neither resolves nor exists (and has no glob
        // magic) is Git's unknown-revision-or-path error.
        if dashdash {
            return Err(DiffError::UnknownRevisionOrPath(tok));
        }
        if !is_path && !has_glob_magic(&tok) {
            return Err(DiffError::UnknownRevisionOrPath(tok));
        }
        paths_started = true;
        revs_done = true;
        remaining.push(tok);
    }
    args.pathspec = remaining;
    args.pathspec.extend(trailing_paths);
    Ok(())
}

fn validate_diff_algorithm(args: &DiffArgs) -> Result<(), DiffError> {
    match args.algorithm.as_deref().unwrap_or("histogram") {
        "histogram" => Ok(()),
        unsupported => Err(DiffError::UnsupportedAlgorithm(unsupported.to_string())),
    }
}

fn emit_worktree_scan_progress(args: &DiffArgs, output: &OutputConfig) {
    if output.quiet || output.is_json() || args.staged || args.new.is_some() {
        return;
    }

    match output.progress {
        ProgressMode::Text => eprintln!("Scanning working tree ..."),
        ProgressMode::Json => {
            let event = serde_json::json!({
                "event": "diff_scan.start",
                "task": "Scanning working tree",
            });
            eprintln!("{event}");
        }
        // OutputConfig resolves `--progress=auto` to None when stderr is not a
        // TTY. `diff` still emits this one-line startup signal for auto mode so
        // large ignored trees do not look hung in captured/non-interactive runs.
        ProgressMode::None
            if output.progress_preference != crate::utils::output::ProgressPreference::None =>
        {
            eprintln!("Scanning working tree ...")
        }
        ProgressMode::None => {}
    }
}

async fn run_diff(args: &DiffArgs, output: &OutputConfig) -> Result<DiffOutput, DiffError> {
    util::require_repo().map_err(|_| DiffError::NotInRepo)?;
    tracing::debug!("diff args: {:?}", args);
    let index = Index::load(path::index()).map_err(|e| DiffError::IndexLoad(e.to_string()))?;

    let old_side = resolve_diff_side(&args.old, args.staged, false, &index).await?;
    let new_side = resolve_diff_side(&args.new, args.staged, true, &index).await?;

    let paths: Vec<PathBuf> = args.pathspec.iter().map(util::to_workdir_path).collect();
    let worktree_entries = new_side.worktree_entries.clone();
    // Separate copy for the external-diff pass (the one above is moved into the
    // diff closure below). Lets the GIT_EXTERNAL_DIFF protocol report a zero hash
    // for a new side that is the live working tree.
    let ext_worktree_entries = new_side.worktree_entries.clone();
    // `Rc` so the `-U<n>` post-pass can read the blob content the diff closure
    // cached (keyed by hash) without re-loading it from the object store/disk.
    let worktree_cache: Rc<RefCell<HashMap<ObjectHash, Vec<u8>>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let repo_cache: Rc<RefCell<HashMap<ObjectHash, Vec<u8>>>> =
        Rc::new(RefCell::new(HashMap::new()));
    let worktree_cache_in = Rc::clone(&worktree_cache);
    let repo_cache_in = Rc::clone(&repo_cache);
    let load_error = Rc::new(RefCell::new(None::<DiffError>));
    let load_error_for_read = Rc::clone(&load_error);
    // `-R`/`--reverse`: swap the two sides so the diff is computed new->old. The
    // loader resolves blobs by hash (content-addressed) and the worktree check
    // above stays correct regardless of which side a blob lands on.
    let (first_blobs, second_blobs, old_label, new_label) = if args.reverse {
        (
            new_side.blobs,
            old_side.blobs,
            new_side.label,
            old_side.label,
        )
    } else {
        (
            old_side.blobs,
            new_side.blobs,
            old_side.label,
            new_side.label,
        )
    };
    // Path → blob-hash for each side (in the diff direction git_internal uses),
    // captured before the blobs are moved into `Diff::diff`, so the `-U<n>`
    // post-pass can look up each file's old/new content from the caches.
    let first_map: HashMap<PathBuf, ObjectHash> = first_blobs.iter().cloned().collect();
    let second_map: HashMap<PathBuf, ObjectHash> = second_blobs.iter().cloned().collect();
    let diff_output = Diff::diff(first_blobs, second_blobs, paths, move |path, hash| {
        if worktree_entries.get(path) == Some(hash) {
            if let Some(data) = worktree_cache_in.borrow().get(hash).cloned() {
                return data;
            }

            match read_worktree_blob_content(path) {
                Ok(data) => {
                    worktree_cache_in.borrow_mut().insert(*hash, data.clone());
                    data
                }
                Err(err) => {
                    record_diff_content_error(&load_error_for_read, err);
                    Vec::new()
                }
            }
        } else {
            if let Some(data) = repo_cache_in.borrow().get(hash).cloned() {
                return data;
            }

            match load_repo_blob_content(hash) {
                Ok(data) => {
                    repo_cache_in.borrow_mut().insert(*hash, data.clone());
                    data
                }
                Err(err) => {
                    record_diff_content_error(&load_error_for_read, err);
                    Vec::new()
                }
            }
        }
    });
    if let Some(err) = load_error.borrow_mut().take() {
        return Err(err);
    }

    let mut files: Vec<DiffFileStat> = diff_output.iter().map(parse_diff_item).collect();

    // Resolve the external diff driver (`diff.external`) when it should drive this
    // run: a patch-body output mode (not `--stat`/name/numstat/summary/`-s`/
    // `--check`), human/file output (not `--json`/`--quiet`), and not disabled by
    // `--no-ext-diff`. When active it REPLACES the patch entirely (applied after
    // the internal post-passes below, which are then skipped), matching Git.
    let external_command: Option<String> =
        if !args.no_ext_diff && !output.is_json() && !output.quiet && patch_body_is_shown(args) {
            ConfigKv::get("diff.external")
                .await
                .ok()
                .flatten()
                .map(|entry| entry.value)
                .filter(|cmd| !cmd.trim().is_empty())
        } else {
            None
        };

    // Post-pass regeneration (both reuse the blob text the diff closure cached —
    // keyed by hash — with no re-load; the default path leaves git_internal's
    // output untouched):
    //   * A whitespace-ignoring flag (`-w`/`-b`/`--ignore-space-at-eol`) re-diffs
    //     each text file through the matching line normalizer, DROPS files whose
    //     only change is whitespace under that rule, and recomputes that file's
    //     +/- counts (so stat/name/numstat/JSON all reflect the result).
    //   * `--ignore-blank-lines` re-diffs ignoring blank-only changes (drops files
    //     whose only change is blank lines, recomputes counts).
    //   * `-U<n>` (when `n != 3`, git_internal's hard-coded default) regenerates
    //     hunk bodies at `n` context lines; +/- lines are unchanged so counts are
    //     untouched — only the surrounding context (and re-parsed `hunks`) change.
    // The re-diff flags honor `-U<n>` for context width; `-w` > `-b` >
    // `--ignore-space-at-eol` if more than one is given (matching Git).
    // `--ignore-blank-lines` COMPOSES with a whitespace flag: the diff and the
    // blank classification both run through the normalizer (matching Git).
    let regen_context = args.unified.unwrap_or(3);
    let ws_normalize: Option<fn(&str) -> String> = if args.ignore_all_space {
        Some(normalize_ignore_all_space)
    } else if args.ignore_space_change {
        Some(normalize_ignore_space_change)
    } else if args.ignore_space_at_eol {
        Some(normalize_ignore_space_at_eol)
    } else if args.ignore_cr_at_eol {
        Some(normalize_ignore_cr_at_eol)
    } else {
        None
    };
    let rediffs = ws_normalize.is_some() || args.ignore_blank_lines;

    // `--relative` restricts WHICH files are diffed; apply that restriction now —
    // before rename detection — so a rename pair is only formed when BOTH sides
    // lie inside the prefix, matching Git (which filters before diffcore-rename).
    // A pair straddling the boundary therefore stays an add or a delete. The
    // path-rewriting half runs later (`apply_relative_filter`, or skipped for
    // verbatim external output).
    if let Some(strip) = relative_prefix(args) {
        files.retain(|file| file.path.starts_with(&strip));
    }

    // `-M`/`--find-renames`: fold matched delete+add pairs into single rename
    // entries. Done here (after the whitespace/context selection, before the
    // post-passes) so the rename's own content diff honors `-U<n>`/`-w`/blank
    // rules and the post-passes then leave rename entries alone.
    if let Some(threshold) = resolve_rename_threshold(args)? {
        // `--check` scans added lines for whitespace errors and ignores the
        // whitespace-ignore flags, so the rename body must stay unfiltered.
        let (rn_ws, rn_blank) = if args.check {
            (None, false)
        } else {
            (ws_normalize, args.ignore_blank_lines)
        };
        apply_rename_detection(
            &mut files,
            &first_map,
            &second_map,
            &ext_worktree_entries,
            threshold,
            regen_context,
            rn_ws,
            rn_blank,
        );
    }

    // Textconv (`--textconv`, on by default unless `--no-textconv`): re-diff the
    // output of each file's `diff.<driver>.textconv` command instead of the raw
    // bytes. Skipped under `--check` (it scans raw added lines) and when an
    // external driver is active (that takes precedence). The post-pass below then
    // leaves textconv'd files alone.
    let textconv_paths: std::collections::HashSet<String> =
        if !args.no_textconv && !args.check && external_command.is_none() {
            let drivers = extract_diff_drivers(&path::attributes());
            if drivers.is_empty() {
                std::collections::HashSet::new()
            } else {
                let mut command_cache: HashMap<String, Option<String>> = HashMap::new();
                // Per file: the (old-side, new-side) textconv command. A rename's
                // old side is at `rename_from` and may resolve a different driver
                // than the new side (Git resolves textconv per blob/path), so each
                // side is looked up independently.
                let mut path_commands: HashMap<String, (Option<String>, Option<String>)> =
                    HashMap::new();
                for file in &files {
                    let new_path = PathBuf::from(&file.path);
                    let old_path = file
                        .rename_from
                        .as_deref()
                        .map(PathBuf::from)
                        .unwrap_or_else(|| new_path.clone());
                    let new_driver = diff_driver_for_path(&drivers, &new_path);
                    let new_command =
                        resolve_textconv_command(new_driver.as_deref(), &mut command_cache).await;
                    let old_command = if old_path == new_path {
                        new_command.clone()
                    } else {
                        let old_driver = diff_driver_for_path(&drivers, &old_path);
                        resolve_textconv_command(old_driver.as_deref(), &mut command_cache).await
                    };
                    if old_command.is_some() || new_command.is_some() {
                        path_commands.insert(file.path.clone(), (old_command, new_command));
                    }
                }
                if path_commands.is_empty() {
                    std::collections::HashSet::new()
                } else {
                    apply_textconv(
                        &mut files,
                        &path_commands,
                        &first_map,
                        &second_map,
                        &ext_worktree_entries,
                        regen_context,
                        ws_normalize,
                        args.ignore_blank_lines,
                    )?
                }
            }
        } else {
            std::collections::HashSet::new()
        };

    // Binary detection: a file whose content carries a NUL byte is shown as
    // `Binary files … differ` (or, with `--binary`, a `GIT binary patch`) instead
    // of a content diff. `--text` forces the content diff; `--check` and an active
    // external driver take over the body, and textconv'd files are already text.
    // The context/whitespace post-pass below then skips binary files.
    let mut binary_patch = false;
    if !args.text && !args.check && external_command.is_none() {
        binary_patch = apply_binary_detection(
            &mut files,
            &first_map,
            &second_map,
            &ext_worktree_entries,
            &textconv_paths,
            args.binary,
        )?;
    } else if args.text && !args.check && external_command.is_none() {
        // `--text` forces content even for non-UTF-8 files git_internal already
        // collapsed to a bare `Binary files differ`.
        force_text_for_bare_binary(
            &mut files,
            &first_map,
            &second_map,
            &ext_worktree_entries,
            regen_context,
        )?;
    }

    // `--binary` implies `--full-index`: rewrite EVERY file's `index` line to full
    // object ids (binary files were already given full ids above; this covers the
    // text files in the same diff).
    if args.binary && external_command.is_none() {
        for file in files.iter_mut() {
            // Binary files were already given full ids (with the correct
            // blank-line terminator) in `apply_binary_detection`; don't re-process.
            if file.binary.is_some() {
                continue;
            }
            let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
            let old_id = first_map
                .get(&PathBuf::from(old_path))
                .map(|h| h.to_string());
            let new_id = second_map
                .get(&PathBuf::from(&file.path))
                .map(|h| h.to_string());
            let width = old_id
                .as_ref()
                .or(new_id.as_ref())
                .map(String::len)
                .unwrap_or(40);
            let zeros = "0".repeat(width);
            file.raw_diff = binary_index_full(
                &file.raw_diff,
                &old_id.unwrap_or_else(|| zeros.clone()),
                &new_id.unwrap_or(zeros),
            );
        }
    }

    // `--check` (whitespace-error scan) ignores the whitespace-ignore flags and
    // operates on git_internal's original diff — matching Git, where
    // `diff --check -w`/`-b`/`--ignore-space-at-eol` still reports trailing-
    // whitespace errors. It replaces the patch output, so the post-pass (which
    // only rewrites the patch/stat/counts) is skipped entirely when `--check`.
    if external_command.is_none()
        && !args.check
        && (rediffs || (args.unified.is_some() && regen_context != 3))
    {
        let blob_text = |map: &HashMap<PathBuf, ObjectHash>, path: &Path| -> String {
            let Some(hash) = map.get(path) else {
                return String::new();
            };
            // Clone out of each borrow so no reference escapes the temporary `Ref`.
            let bytes = worktree_cache
                .borrow()
                .get(hash)
                .cloned()
                .or_else(|| repo_cache.borrow().get(hash).cloned());
            bytes
                .map(|b| String::from_utf8_lossy(&b).into_owned())
                .unwrap_or_default()
        };
        if rediffs {
            files.retain_mut(|file| {
                // Rename and textconv entries already carry their final rendered
                // body (textconv re-diffs the converted content at this context),
                // so leave them untouched by the whitespace/context re-diff.
                if file.status == "renamed" || textconv_paths.contains(&file.path) {
                    return true;
                }
                // Binary / no-hunk diffs have no body to re-diff: keep as-is.
                if !file.raw_diff.contains("\n@@ ") {
                    return true;
                }
                let path = PathBuf::from(&file.path);
                let old_text = blob_text(&first_map, &path);
                let new_text = blob_text(&second_map, &path);
                // `--ignore-blank-lines` composes with a whitespace normalizer when
                // both are given (matching `git diff -w --ignore-blank-lines`).
                let body = if args.ignore_blank_lines {
                    match ws_normalize {
                        Some(normalize) => compute_unified_hunks_ignore_blank_normalized(
                            &old_text,
                            &new_text,
                            regen_context,
                            normalize,
                        ),
                        None => {
                            compute_unified_hunks_ignore_blank(&old_text, &new_text, regen_context)
                        }
                    }
                } else if let Some(normalize) = ws_normalize {
                    compute_unified_hunks_normalized(&old_text, &new_text, regen_context, normalize)
                } else {
                    compute_unified_hunks(&old_text, &new_text, regen_context)
                };
                // No change survives the rule. Git still reports an added/deleted
                // filepair (header, zero counts, no hunk) even when its only content
                // is blank lines — only a modification disappears entirely.
                if body.trim().is_empty() {
                    // `file.status` is parsed only from the pre-hunk header lines
                    // (`parse_diff_status` stops at the first `@@`), so a body line
                    // that merely contains "new file mode" cannot misclassify a
                    // modification as an add/delete.
                    let is_add_or_delete = file.status == "added" || file.status == "deleted";
                    if !is_add_or_delete {
                        return false;
                    }
                    file.insertions = 0;
                    file.deletions = 0;
                    file.hunks = Vec::new();
                    file.raw_diff = strip_unified_diff_body(&file.raw_diff);
                    return true;
                }
                let (insertions, deletions) = count_body_changes(&body);
                file.insertions = insertions;
                file.deletions = deletions;
                file.raw_diff = splice_unified_body(&file.raw_diff, &body);
                file.hunks = parse_diff_hunks(&file.raw_diff);
                true
            });
        } else {
            for file in files.iter_mut() {
                // Rename entries already rendered their content diff at the
                // requested context in `build_rename_entry`; do not re-diff them
                // (their old side is at `rename_from`, not `file.path`). Textconv'd
                // files were likewise already re-diffed at this context, and binary
                // files have no text body.
                if file.status == "renamed"
                    || textconv_paths.contains(&file.path)
                    || file.binary.is_some()
                {
                    continue;
                }
                let path = PathBuf::from(&file.path);
                let old_text = blob_text(&first_map, &path);
                let new_text = blob_text(&second_map, &path);
                file.raw_diff = rewrite_unified_diff_context(
                    &file.raw_diff,
                    &old_text,
                    &new_text,
                    regen_context,
                );
                file.hunks = parse_diff_hunks(&file.raw_diff);
            }
        }
    }

    // Apply the external diff driver LAST so its verbatim output is never touched
    // by the internal post-passes (skipped above) or the later word-diff pass
    // (skipped in `execute_safe` via `external_diff_applied`).
    let external_diff_applied = if let Some(command) = &external_command {
        // The `--relative` file-set restriction was already applied above (before
        // rename detection); the path-rewriting half stays skipped for verbatim
        // driver output, so the driver only sees files inside the prefix.
        apply_external_diff(
            &mut files,
            command,
            &first_map,
            &second_map,
            &ext_worktree_entries,
        )?;
        true
    } else {
        false
    };

    let total_insertions = files.iter().map(|file| file.insertions).sum();
    let total_deletions = files.iter().map(|file| file.deletions).sum();
    let files_changed = files.len();

    Ok(DiffOutput {
        old_ref: old_label,
        new_ref: new_label,
        files,
        total_insertions,
        total_deletions,
        files_changed,
        external_diff_applied,
        binary_patch,
    })
}

#[derive(Debug)]
struct DiffSide {
    label: String,
    blobs: Vec<(PathBuf, ObjectHash)>,
    worktree_entries: HashMap<PathBuf, ObjectHash>,
}

/// diff needs to print hashes even if the files have not been staged yet.
/// This helper maps workdir paths to blob ids while applying the shared ignore policy.
fn get_files_blobs(
    files: &[PathBuf],
    index: &Index,
    policy: IgnorePolicy,
) -> Result<Vec<(PathBuf, ObjectHash)>, DiffError> {
    files
        .iter()
        .filter(|path| !ignore::should_ignore(path, policy, index))
        .map(|p| {
            if let Some(hash) = index_hash_if_worktree_stat_matches(p, index) {
                return Ok((p.to_owned(), hash));
            }
            let path = util::workdir_to_absolute(p);
            let data = std::fs::read(&path).map_err(|e| DiffError::FileRead {
                path: path.display().to_string(),
                detail: e.to_string(),
            })?;
            Ok((p.to_owned(), calculate_object_hash(ObjectType::Blob, &data)))
        })
        .collect()
}

fn index_hash_if_worktree_stat_matches(path: &Path, index: &Index) -> Option<ObjectHash> {
    let entry = index.get(path.to_str()?, 0)?;
    let absolute = util::workdir_to_absolute(path);
    let metadata = std::fs::symlink_metadata(&absolute).ok()?;
    index_entry_matches_worktree_stat(entry, &metadata).then_some(entry.hash)
}

fn index_entry_matches_worktree_stat(entry: &IndexEntry, metadata: &std::fs::Metadata) -> bool {
    let Ok(size) = u32::try_from(metadata.len()) else {
        return false;
    };
    let ctime = Time::from_system_time(index_ctime(metadata));
    let mtime = Time::from_system_time(index_mtime(metadata));

    entry.ctime == ctime
        && entry.mtime == mtime
        && entry.dev == index_dev_from_metadata(metadata)
        && entry.ino == index_ino_from_metadata(metadata)
        && entry.size == size
        && entry.uid == index_uid_from_metadata(metadata)
        && entry.gid == index_gid_from_metadata(metadata)
        && entry.mode == index_mode_from_metadata(metadata)
}

#[cfg(unix)]
fn index_ctime(metadata: &std::fs::Metadata) -> SystemTime {
    unix_metadata_time(metadata.ctime(), metadata.ctime_nsec())
}

#[cfg(not(unix))]
fn index_ctime(metadata: &std::fs::Metadata) -> SystemTime {
    metadata
        .created()
        .or_else(|_| metadata.modified())
        .unwrap_or(UNIX_EPOCH)
}

#[cfg(unix)]
fn index_mtime(metadata: &std::fs::Metadata) -> SystemTime {
    unix_metadata_time(metadata.mtime(), metadata.mtime_nsec())
}

#[cfg(not(unix))]
fn index_mtime(metadata: &std::fs::Metadata) -> SystemTime {
    metadata
        .modified()
        .or_else(|_| metadata.created())
        .unwrap_or(UNIX_EPOCH)
}

#[cfg(unix)]
fn unix_metadata_time(seconds: i64, nanos: i64) -> SystemTime {
    if seconds < 0 {
        return UNIX_EPOCH;
    }

    let nanos = u32::try_from(nanos)
        .ok()
        .filter(|nanos| *nanos < 1_000_000_000)
        .unwrap_or(0);

    UNIX_EPOCH + Duration::new(seconds as u64, nanos)
}

fn index_dev_from_metadata(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.dev() as u32
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

fn index_ino_from_metadata(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.ino() as u32
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

fn index_uid_from_metadata(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.uid()
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

fn index_gid_from_metadata(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        metadata.gid()
    }

    #[cfg(not(unix))]
    {
        let _ = metadata;
        0
    }
}

fn index_mode_from_metadata(metadata: &std::fs::Metadata) -> u32 {
    #[cfg(unix)]
    {
        match metadata.mode() & 0o170000 {
            0o100000 => match metadata.mode() & 0o111 {
                0 => 0o100644,
                _ => 0o100755,
            },
            0o120000 => 0o120000,
            _ => 0o100644,
        }
    }

    #[cfg(windows)]
    {
        if metadata.file_type().is_symlink() {
            0o120000
        } else {
            0o100644
        }
    }

    #[cfg(not(any(unix, windows)))]
    {
        0o100644
    }
}

fn get_worktree_diff_files(index: &Index) -> Result<Vec<PathBuf>, DiffError> {
    let mut seen = HashSet::new();
    let mut files = Vec::new();

    for file in util::list_workdir_files().map_err(|e| DiffError::WorkdirList(e.to_string()))? {
        if seen.insert(file.clone()) {
            files.push(file);
        }
    }

    for file in index.tracked_files() {
        let absolute = util::workdir_to_absolute(&file);
        if absolute.is_file() && seen.insert(file.clone()) {
            files.push(file);
        }
    }

    Ok(files)
}

/// Returns (path, hash) pairs from the index's stored entries (stage 0).
/// Unlike `get_files_blobs`, this uses the hash already recorded in the index
/// rather than reading the current file on disk, which is essential for
/// producing a correct working-directory diff (index vs working tree).
fn get_index_blobs(index: &Index, policy: IgnorePolicy) -> Vec<(PathBuf, ObjectHash)> {
    index
        .tracked_entries(0)
        .iter()
        .filter(|entry| !ignore::should_ignore(&PathBuf::from(&entry.name), policy, index))
        .map(|entry| (PathBuf::from(&entry.name), entry.hash))
        .collect()
}

async fn resolve_diff_side(
    source: &Option<String>,
    staged: bool,
    is_new: bool,
    index: &Index,
) -> Result<DiffSide, DiffError> {
    if let Some(source) = source {
        let commit_hash = get_target_commit(source)
            .await
            .map_err(|_| DiffError::InvalidRevision(source.clone()))?;
        return Ok(DiffSide {
            label: source.clone(),
            blobs: get_commit_blobs(&commit_hash).await?,
            worktree_entries: HashMap::new(),
        });
    }

    if is_new {
        if staged {
            Ok(DiffSide {
                label: "index".to_string(),
                blobs: get_index_blobs(index, IgnorePolicy::Respect),
                worktree_entries: HashMap::new(),
            })
        } else {
            let files = get_worktree_diff_files(index)?;
            let blobs = get_files_blobs(&files, index, IgnorePolicy::Respect)?;
            Ok(DiffSide {
                label: "working tree".to_string(),
                worktree_entries: blobs.iter().cloned().collect(),
                blobs,
            })
        }
    } else if staged {
        match Head::current_commit().await {
            Some(commit_hash) => Ok(DiffSide {
                label: "HEAD".to_string(),
                blobs: get_commit_blobs(&commit_hash).await?,
                worktree_entries: HashMap::new(),
            }),
            None => Ok(DiffSide {
                label: "HEAD".to_string(),
                blobs: Vec::new(),
                worktree_entries: HashMap::new(),
            }),
        }
    } else {
        Ok(DiffSide {
            label: "index".to_string(),
            blobs: get_index_blobs(index, IgnorePolicy::Respect),
            worktree_entries: HashMap::new(),
        })
    }
}

async fn get_commit_blobs(
    commit_hash: &ObjectHash,
) -> Result<Vec<(PathBuf, ObjectHash)>, DiffError> {
    let commit = load_object::<Commit>(commit_hash).map_err(|e| DiffError::ObjectLoad {
        kind: "commit",
        object_id: commit_hash.to_string(),
        detail: e.to_string(),
    })?;
    let tree = load_object::<Tree>(&commit.tree_id).map_err(|e| DiffError::ObjectLoad {
        kind: "tree",
        object_id: commit.tree_id.to_string(),
        detail: e.to_string(),
    })?;
    Ok(tree.get_plain_items())
}

/// Render a Git-style `--stat` block for the changes between two commits'
/// trees, reusing the same diff engine and `--stat` formatter as `libra diff
/// --stat`. Used by `libra merge --stat` to show what a merge changed. Returns
/// an empty string when the two trees are identical.
pub(crate) async fn diff_stat_between_commits(
    old_commit: &ObjectHash,
    new_commit: &ObjectHash,
) -> Result<String, DiffError> {
    let old_blobs = get_commit_blobs(old_commit).await?;
    let new_blobs = get_commit_blobs(new_commit).await?;

    // Capture the first blob-read failure from the (infallible-signature) diff
    // closure and surface it after, mirroring `run_diff`.
    let load_error: RefCell<Option<DiffError>> = RefCell::new(None);
    let diff_output =
        Diff::diff(
            old_blobs,
            new_blobs,
            Vec::new(),
            |_path, hash| match load_repo_blob_content(hash) {
                Ok(data) => data,
                Err(err) => {
                    if load_error.borrow().is_none() {
                        *load_error.borrow_mut() = Some(err);
                    }
                    Vec::new()
                }
            },
        );
    if let Some(err) = load_error.borrow_mut().take() {
        return Err(err);
    }

    let files: Vec<DiffFileStat> = diff_output.iter().map(parse_diff_item).collect();
    let total_insertions = files.iter().map(|file| file.insertions).sum();
    let total_deletions = files.iter().map(|file| file.deletions).sum();
    let files_changed = files.len();
    let output = DiffOutput {
        old_ref: old_commit.to_string(),
        new_ref: new_commit.to_string(),
        files,
        total_insertions,
        total_deletions,
        files_changed,
        external_diff_applied: false,
        binary_patch: false,
    };
    Ok(format_diff_stat_output(&output))
}

fn load_repo_blob_content(hash: &ObjectHash) -> Result<Vec<u8>, DiffError> {
    let blob = load_object::<Blob>(hash).map_err(|e| DiffError::ObjectLoad {
        kind: "blob",
        object_id: hash.to_string(),
        detail: e.to_string(),
    })?;
    Ok(blob.data)
}

fn read_worktree_blob_content(path_buf: &PathBuf) -> Result<Vec<u8>, DiffError> {
    let absolute = util::workdir_to_absolute(path_buf);
    std::fs::read(&absolute).map_err(|e| DiffError::FileRead {
        path: absolute.display().to_string(),
        detail: e.to_string(),
    })
}

/// Whether the textual patch body is shown for this invocation. The
/// `--stat`/`--numstat`/`--shortstat`/`--name-only`/`--name-status`/`--summary`/
/// `-s`/`--check` modes render from the internal diff and bypass external
/// drivers (matching Git, which never runs `diff.external` for those modes).
fn patch_body_is_shown(args: &DiffArgs) -> bool {
    !(args.stat
        || args.numstat
        || args.shortstat
        || args.name_only
        || args.name_status
        || args.summary
        || args.no_patch
        || args.check)
}

/// Extract the `old`/`new` file modes for the external-diff protocol from a
/// file's internal patch headers, defaulting to `100644` for a regular file.
fn external_diff_modes(raw_diff: &str) -> (String, String) {
    let mut old_mode = "100644".to_string();
    let mut new_mode = "100644".to_string();
    for line in raw_diff.lines() {
        if let Some(rest) = line.strip_prefix("index ") {
            // `index <old>..<new> <mode>` carries the (shared) mode for a content
            // change with an unchanged mode — including a non-100644 file such as
            // an executable. Mode-change headers below override it.
            if let Some(mode) = rest.split_whitespace().nth(1) {
                old_mode = mode.to_string();
                new_mode = mode.to_string();
            }
        } else if let Some(rest) = line.strip_prefix("old mode ") {
            old_mode = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("new mode ") {
            new_mode = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            new_mode = rest.trim().to_string();
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            old_mode = rest.trim().to_string();
        }
    }
    (old_mode, new_mode)
}

/// The Git index mode for a working-tree path: `120000` for a symlink, `100755`
/// when the executable bit is set, else `100644`. Used for the external-diff
/// protocol's working-tree side. Falls back to `100644` if the path is unreadable.
fn worktree_file_mode(path: &Path) -> String {
    let absolute = util::workdir_to_absolute(path);
    match std::fs::symlink_metadata(&absolute) {
        Ok(meta) if meta.file_type().is_symlink() => "120000".to_string(),
        Ok(meta) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                if meta.permissions().mode() & 0o111 != 0 {
                    return "100755".to_string();
                }
            }
            let _ = &meta;
            "100644".to_string()
        }
        Err(_) => "100644".to_string(),
    }
}

/// Resolve `--color-moved[=<mode>]`: whether moved lines should be colored.
/// `no`/`--no-color-moved`/unset → off. Every other (valid) mode → on; Libra
/// approximates Git's block-significance modes with `plain` coloring. An
/// unrecognized mode is a usage error.
fn color_moved_active(args: &DiffArgs) -> Result<bool, DiffError> {
    if args.no_color_moved {
        return Ok(false);
    }
    let Some(mode) = args.color_moved.as_deref() else {
        return Ok(false);
    };
    match mode {
        "no" => Ok(false),
        "default" | "plain" | "blocks" | "zebra" | "dimmed-zebra" | "dimmed_zebra" => Ok(true),
        other => Err(DiffError::InvalidColorMoved(other.to_string())),
    }
}

/// Resolve `-M`/`--find-renames[=<n>]` to a similarity score threshold on Git's
/// 0..60000 scale, or `None` when rename detection is off (or `--no-renames`).
fn resolve_rename_threshold(args: &DiffArgs) -> Result<Option<u32>, DiffError> {
    if args.no_renames {
        return Ok(None);
    }
    let Some(raw) = args.find_renames.as_ref() else {
        return Ok(None);
    };
    let score = parse_rename_score(raw)?;
    // Git's `diffcore_rename` treats a zero minimum score (`-M0`, `-M0%`, empty
    // value, or a value that truncates to 0) as the 50% default before pairing,
    // so it never folds unrelated pairs into `R000` renames.
    Ok(Some(if score == 0 { 30000 } else { score }))
}

/// Parse a `-M`/`--find-renames` argument into a similarity threshold on Git's
/// 0..60000 scale, matching Git's `parse_rename_score`: `<n>%` is a literal
/// percent; `<n>` carrying a decimal point is a literal fraction (`0.9` = 90%);
/// a bare integer is read as the fractional digits after an implied `0.` (so
/// `-M5` = 50%, `-M90` = 90%, `-M100` = 10%). Invalid input is a usage error.
fn parse_rename_score(raw: &str) -> Result<u32, DiffError> {
    let invalid = || DiffError::InvalidRenameScore(raw.to_string());
    // Parse a decimal string into (num, denom) so value == num/denom, using
    // integer arithmetic (no float rounding — matches Git's integer scaling and
    // its truncation at boundaries). At most one '.', digits only.
    let parse_decimal = |s: &str| -> Option<(u128, u128)> {
        let mut num: u128 = 0;
        let mut denom: u128 = 1;
        let mut seen_dot = false;
        let mut any_digit = false;
        // Cap BOTH num and denom: a huge integer part grows `num` (denom stays 1),
        // while a long all-zero fractional part grows `denom` (num stays 0). Once
        // either hits the cap, further digits are dropped — Git likewise stops
        // scaling past a cap, and the threshold needs nothing finer. This keeps
        // `num * 10` well within u128 so no malformed argument can overflow.
        const CAP: u128 = 1_000_000_000_000;
        for b in s.bytes() {
            match b {
                b'.' if !seen_dot => seen_dot = true,
                b'0'..=b'9' => {
                    any_digit = true;
                    if num < CAP && denom < CAP {
                        num = num * 10 + (b - b'0') as u128;
                        if seen_dot {
                            denom *= 10;
                        }
                    }
                }
                _ => return None,
            }
        }
        any_digit.then_some((num, denom))
    };
    // `<n>%` is a literal percent (divide the fraction by 100); `<n>` carrying a
    // decimal point is a literal fraction; a bare integer is read after an
    // implied `0.` (so `-M5` = 0.5 = 50%, `-M100` = 0.100 = 10%).
    let (num, denom) = if let Some(body) = raw.strip_suffix('%') {
        let (n, d) = parse_decimal(body).ok_or_else(invalid)?;
        (n, d * 100)
    } else if raw.contains('.') {
        parse_decimal(raw).ok_or_else(invalid)?
    } else {
        parse_decimal(&format!("0.{raw}")).ok_or_else(invalid)?
    };
    const MAX: u128 = 60000;
    // Git: a fraction >= 1 clamps to MAX_SCORE; otherwise floor(MAX * num/denom).
    let score = if num >= denom { MAX } else { MAX * num / denom };
    Ok(score as u32)
}

/// Chunk `data` the way Git's rename spanhash does — a chunk ends at a newline or
/// after 64 bytes; a `\r` in a `\r\n` is ignored for text — and accumulate the
/// byte count per chunk-hash. We hash each chunk with FNV-1a rather than Git's
/// weaker `HASHBASE` rolling hash: for real content the similarity is identical
/// (equal chunks always match; FNV collisions are astronomically rare), but a
/// contrived input engineered to collide under Git's hash can score differently.
fn spanhash_counts(data: &[u8]) -> HashMap<u64, u64> {
    let is_text = !data.contains(&0);
    let mut counts: HashMap<u64, u64> = HashMap::new();
    let mut chunk: Vec<u8> = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let c = data[i];
        if is_text && c == b'\r' && i + 1 < data.len() && data[i + 1] == b'\n' {
            i += 1;
            continue;
        }
        chunk.push(c);
        i += 1;
        if chunk.len() >= 64 || c == b'\n' {
            *counts.entry(fnv1a(&chunk)).or_default() += chunk.len() as u64;
            chunk.clear();
        }
    }
    if !chunk.is_empty() {
        *counts.entry(fnv1a(&chunk)).or_default() += chunk.len() as u64;
    }
    counts
}

fn fnv1a(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    for &b in bytes {
        h ^= b as u64;
        h = h.wrapping_mul(0x100000001b3);
    }
    h
}

/// Git's similarity score (0..60000): common chunk bytes * 60000 / max file size.
/// Two empty files are identical (full score). The displayed percent is
/// `score / 600`.
fn similarity_score(old: &[u8], new: &[u8]) -> u32 {
    let max_size = old.len().max(new.len()) as u64;
    if max_size == 0 {
        return 60000;
    }
    let old_counts = spanhash_counts(old);
    let new_counts = spanhash_counts(new);
    let mut common: u64 = 0;
    for (hash, &old_bytes) in &old_counts {
        if let Some(&new_bytes) = new_counts.get(hash) {
            common += old_bytes.min(new_bytes);
        }
    }
    ((common * 60000) / max_size) as u32
}

/// Detect renames among the deleted + added files and fold each matched pair into
/// a single rename entry (`-M`). Exact (same blob id) pairs are matched first,
/// then the best inexact pairs whose similarity meets the threshold. Each side is
/// used at most once.
#[allow(clippy::too_many_arguments)]
fn apply_rename_detection(
    files: &mut Vec<DiffFileStat>,
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &HashMap<PathBuf, ObjectHash>,
    threshold: u32,
    context: usize,
    ws_normalize: Option<fn(&str) -> String>,
    ignore_blank: bool,
) {
    let load = |path: &str, map: &HashMap<PathBuf, ObjectHash>| -> Option<Vec<u8>> {
        let pb = PathBuf::from(path);
        let hash = map.get(&pb)?;
        if worktree_entries.get(&pb) == Some(hash) {
            read_worktree_blob_content(&pb).ok()
        } else {
            load_repo_blob_content(hash).ok()
        }
    };

    // Indices of the deleted (old-only) and added (new-only) entries.
    let deleted: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].status == "deleted")
        .collect();
    let added: Vec<usize> = (0..files.len())
        .filter(|&i| files[i].status == "added")
        .collect();
    if deleted.is_empty() || added.is_empty() {
        return;
    }

    let mut used_del = vec![false; files.len()];
    let mut used_add = vec![false; files.len()];
    // (old_idx, new_idx, score) for the chosen pairs.
    let mut pairs: Vec<(usize, usize, u32)> = Vec::new();

    // Pass 1: exact renames (identical blob id).
    for &di in &deleted {
        let Some(dh) = first_map.get(&PathBuf::from(&files[di].path)) else {
            continue;
        };
        for &ai in &added {
            if used_add[ai] {
                continue;
            }
            if second_map.get(&PathBuf::from(&files[ai].path)) == Some(dh) {
                pairs.push((di, ai, 60000));
                used_del[di] = true;
                used_add[ai] = true;
                break;
            }
        }
    }

    // Pass 2: inexact renames — score every remaining pair, then assign greedily
    // by descending score (each side once), keeping only pairs >= threshold.
    // Like Git, a matching basename breaks ties so an ambiguous equal-score set
    // prefers same-name pairings. `-M100%` (threshold == MAX_SCORE) is exact-only:
    // Git skips inexact detection entirely, so a 100%-similar but non-identical
    // pair (e.g. reordered lines) must NOT be folded.
    const MAX_SCORE: u32 = 60000;
    let basename = |path: &str| path.rsplit('/').next().unwrap_or(path).to_string();
    if threshold < MAX_SCORE {
        // (score, same_basename, di, ai)
        let mut candidates: Vec<(u32, bool, usize, usize)> = Vec::new();
        for &di in &deleted {
            if used_del[di] {
                continue;
            }
            let Some(old) = load(&files[di].path, first_map) else {
                continue;
            };
            for &ai in &added {
                if used_add[ai] {
                    continue;
                }
                let Some(new) = load(&files[ai].path, second_map) else {
                    continue;
                };
                let score = similarity_score(&old, &new);
                if score >= threshold {
                    let same_base = basename(&files[di].path) == basename(&files[ai].path);
                    candidates.push((score, same_base, di, ai));
                }
            }
        }
        candidates.sort_by(|a, b| {
            b.0.cmp(&a.0)
                .then(b.1.cmp(&a.1))
                .then(a.2.cmp(&b.2))
                .then(a.3.cmp(&b.3))
        });
        for (score, _, di, ai) in candidates {
            if !used_del[di] && !used_add[ai] {
                used_del[di] = true;
                used_add[ai] = true;
                pairs.push((di, ai, score));
            }
        }
    }

    if pairs.is_empty() {
        return;
    }

    // Build the rename entries, then drop the consumed del/add entries.
    let mut renames: Vec<(usize, DiffFileStat)> = Vec::new();
    for (di, ai, score) in &pairs {
        let old_path = files[*di].path.clone();
        let new_path = files[*ai].path.clone();
        let percent = score / 600;
        let entry = build_rename_entry(
            &old_path,
            &new_path,
            percent,
            first_map.get(&PathBuf::from(&old_path)),
            second_map.get(&PathBuf::from(&new_path)),
            &load(&old_path, first_map).unwrap_or_default(),
            &load(&new_path, second_map).unwrap_or_default(),
            context,
            ws_normalize,
            ignore_blank,
        );
        // Insert at the added entry's position so output order stays stable.
        renames.push((*ai, entry));
    }
    let drop: std::collections::HashSet<usize> =
        pairs.iter().flat_map(|(d, a, _)| [*d, *a]).collect();
    let mut rebuilt: Vec<DiffFileStat> = Vec::with_capacity(files.len());
    for (idx, file) in files.drain(..).enumerate() {
        if let Some(pos) = renames.iter().position(|(ai, _)| *ai == idx) {
            rebuilt.push(renames.remove(pos).1);
        } else if !drop.contains(&idx) {
            rebuilt.push(file);
        }
    }
    *files = rebuilt;
}

/// Render one rename entry (patch + metadata). A byte-identical rename emits only
/// the rename headers; any rename whose blobs differ — even at 100% similarity
/// (e.g. reordered lines) — also carries the content diff (`index`/`---`/`+++`/
/// hunks) between the old and new blobs.
#[allow(clippy::too_many_arguments)]
fn build_rename_entry(
    old_path: &str,
    new_path: &str,
    percent: u32,
    old_hash: Option<&ObjectHash>,
    new_hash: Option<&ObjectHash>,
    old_content: &[u8],
    new_content: &[u8],
    context: usize,
    ws_normalize: Option<fn(&str) -> String>,
    ignore_blank: bool,
) -> DiffFileStat {
    let mut raw = format!(
        "diff --git a/{old_path} b/{new_path}\nsimilarity index {percent}%\nrename from {old_path}\nrename to {new_path}\n"
    );
    let (mut insertions, mut deletions) = (0usize, 0usize);
    // Emit the content diff whenever the blobs actually differ — even at 100%
    // similarity (e.g. reordered lines), matching Git, which shows the body for a
    // non-identical rename. Only a byte-identical rename has no body.
    if old_content != new_content {
        let old_text = String::from_utf8_lossy(old_content);
        let new_text = String::from_utf8_lossy(new_content);
        // Honor the active whitespace / blank-line / context rules so a rename's
        // content diff matches `libra diff` for the same flags.
        let hunks = if ignore_blank {
            match ws_normalize {
                Some(normalize) => compute_unified_hunks_ignore_blank_normalized(
                    &old_text, &new_text, context, normalize,
                ),
                None => compute_unified_hunks_ignore_blank(&old_text, &new_text, context),
            }
        } else if let Some(normalize) = ws_normalize {
            compute_unified_hunks_normalized(&old_text, &new_text, context, normalize)
        } else {
            compute_unified_hunks(&old_text, &new_text, context)
        };
        // A rename that differs only in ignored whitespace/blank lines has an
        // empty body: emit just the rename headers (no `index`/`---`/`+++`).
        if !hunks.trim().is_empty() {
            let old_abbrev = old_hash
                .map(|h| h.to_string()[..7].to_string())
                .unwrap_or_else(|| "0000000".to_string());
            let new_abbrev = new_hash
                .map(|h| h.to_string()[..7].to_string())
                .unwrap_or_else(|| "0000000".to_string());
            raw.push_str(&format!("index {old_abbrev}..{new_abbrev} 100644\n"));
            raw.push_str(&format!("--- a/{old_path}\n+++ b/{new_path}\n"));
            raw.push_str(&hunks);
            let (ins, del) = count_body_changes(&hunks);
            insertions = ins;
            deletions = del;
        }
    }
    DiffFileStat {
        path: new_path.to_string(),
        status: "renamed".to_string(),
        insertions,
        deletions,
        hunks: parse_diff_hunks(&raw),
        raw_diff: raw,
        rename_from: Some(old_path.to_string()),
        similarity: Some(percent),
        binary: None,
    }
}

/// Parse `.libra_attributes` for `diff` attributes, returning `(pattern, value)`
/// pairs in file order. `value` is `Some(driver)` for `diff=<driver>` and `None`
/// for `-diff` / `!diff` / a bare `diff` (which unset or reset the driver, so a
/// later such entry clears an earlier `diff=<driver>` under last-match-wins).
fn extract_diff_drivers(attr_path: &Path) -> Vec<(String, Option<String>)> {
    let Ok(content) = std::fs::read_to_string(attr_path) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for line in content.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut tokens = line.split_whitespace();
        let Some(pattern) = tokens.next() else {
            continue;
        };
        for token in tokens {
            if let Some(driver) = token.strip_prefix("diff=") {
                if !driver.is_empty() {
                    out.push((pattern.to_string(), Some(driver.to_string())));
                }
            } else if token == "diff" || token == "-diff" || token == "!diff" {
                // Set/unset/unspecified: no named driver → clears any earlier one.
                out.push((pattern.to_string(), None));
            }
        }
    }
    out
}

/// The diff driver assigned to `path` by `.libra_attributes` — the LAST matching
/// `diff` attribute wins (Git's attribute semantics); a matching unset/reset
/// (`-diff`/`!diff`/bare `diff`) clears any earlier `diff=<driver>`, so this
/// returns `None` for it.
fn diff_driver_for_path(drivers: &[(String, Option<String>)], path: &Path) -> Option<String> {
    let workdir = util::working_dir();
    let mut result = None;
    for (pattern, value) in drivers {
        let mut builder = ::ignore::gitignore::GitignoreBuilder::new(&workdir);
        if builder.add_line(None, pattern).is_err() {
            continue;
        }
        if let Ok(gi) = builder.build()
            && matches!(gi.matched(path, false), ::ignore::Match::Ignore(_))
        {
            result = value.clone();
        }
    }
    result
}

/// Run a `diff.<driver>.textconv` command on `content`: Git writes the blob to a
/// temp file and passes its path as the sole argument; the command's stdout is
/// the converted text. A temp-file, spawn, or non-zero-exit failure is a fatal
/// error (matching Git, which dies with "unable to read files to diff" rather
/// than silently diffing raw bytes).
fn run_textconv(command: &str, content: &[u8]) -> Result<Vec<u8>, DiffError> {
    use std::io::Write as _;
    let fail = |detail: String| DiffError::TextconvFailed {
        command: command.to_string(),
        detail,
    };
    let mut tmp =
        NamedTempFile::new().map_err(|e| fail(format!("could not create temp file: {e}")))?;
    tmp.write_all(content)
        .map_err(|e| fail(format!("could not write temp file: {e}")))?;
    let path = tmp.path().to_string_lossy().into_owned();
    let output = std::process::Command::new("sh")
        .arg("-c")
        .arg(format!("{command} \"$@\""))
        .arg(command)
        .arg(&path)
        .output()
        .map_err(|e| fail(format!("could not run command: {e}")))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(fail(format!(
            "command exited with {}{}",
            output.status,
            if stderr.trim().is_empty() {
                String::new()
            } else {
                format!(": {}", stderr.trim())
            }
        )))
    }
}

/// Resolve a diff driver name to its `diff.<driver>.textconv` command (cached by
/// driver). `None` driver or unset/empty command → `None`.
async fn resolve_textconv_command(
    driver: Option<&str>,
    cache: &mut HashMap<String, Option<String>>,
) -> Option<String> {
    let driver = driver?;
    if let Some(cached) = cache.get(driver) {
        return cached.clone();
    }
    let resolved = ConfigKv::get(&format!("diff.{driver}.textconv"))
        .await
        .ok()
        .flatten()
        .map(|entry| entry.value)
        .filter(|cmd| !cmd.trim().is_empty());
    cache.insert(driver.to_string(), resolved.clone());
    resolved
}

/// Apply textconv filters (`--textconv`, on by default): for each file with a
/// per-side `(old_command, new_command)` entry, re-diff the command's output for
/// the old and new sides instead of the raw bytes, keeping the file's existing
/// patch header (including a rename's `similarity`/`rename from`/`to`, whose old
/// side lives at `rename_from` and may resolve a DIFFERENT driver than the new
/// side). A side with no command is diffed raw. A modification whose converted
/// content is unchanged is dropped (like a whitespace-only change);
/// created/deleted/renamed files keep their header. Returns the set of textconv'd
/// paths so the later context/whitespace post-pass skips them. Blob read failures
/// surface as errors (not empty content).
#[allow(clippy::too_many_arguments)]
fn apply_textconv(
    files: &mut Vec<DiffFileStat>,
    path_commands: &HashMap<String, (Option<String>, Option<String>)>,
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &HashMap<PathBuf, ObjectHash>,
    context: usize,
    ws_normalize: Option<fn(&str) -> String>,
    ignore_blank: bool,
) -> Result<std::collections::HashSet<String>, DiffError> {
    // `None` = the side is absent from its map (a created/deleted side) and must
    // stay raw-empty — NOT fed through textconv (a converter that emits text for
    // empty input would fabricate hunks). `Some` = a present blob; a mapped blob
    // that fails to load is a real error and propagates.
    let load =
        |path: &str, map: &HashMap<PathBuf, ObjectHash>| -> Result<Option<Vec<u8>>, DiffError> {
            let pb = PathBuf::from(path);
            let Some(hash) = map.get(&pb) else {
                return Ok(None);
            };
            let bytes = if worktree_entries.get(&pb) == Some(hash) {
                read_worktree_blob_content(&pb)?
            } else {
                load_repo_blob_content(hash)?
            };
            Ok(Some(bytes))
        };
    // Convert one side with its own driver, or pass the raw bytes through when
    // that side has no driver (Git resolves textconv per blob/path).
    let convert = |cmd: Option<&str>, raw: &[u8]| -> Result<String, DiffError> {
        match cmd {
            Some(cmd) => Ok(String::from_utf8_lossy(&run_textconv(cmd, raw)?).into_owned()),
            None => Ok(String::from_utf8_lossy(raw).into_owned()),
        }
    };
    let regen = |old: &str, new: &str| -> String {
        if ignore_blank {
            match ws_normalize {
                Some(n) => compute_unified_hunks_ignore_blank_normalized(old, new, context, n),
                None => compute_unified_hunks_ignore_blank(old, new, context),
            }
        } else if let Some(n) = ws_normalize {
            compute_unified_hunks_normalized(old, new, context, n)
        } else {
            compute_unified_hunks(old, new, context)
        }
    };

    // Pass 1: load + convert both sides (so a read error can propagate — this
    // cannot be done inside `retain_mut`, whose closure returns `bool`).
    let mut converted: HashMap<String, (String, String)> = HashMap::new();
    for file in files.iter() {
        let Some((old_cmd, new_cmd)) = path_commands.get(&file.path) else {
            continue;
        };
        // Over-large files are emitted as a `<LargeFile>` marker LINE (no content
        // was loaded for diffing); leave them as-is rather than loading/converting
        // a potentially huge blob. Match the sentinel as a line PREFIX — a normal
        // hunk line containing that text is `+`/`-`/space-prefixed, so it never
        // starts a line and is correctly still converted.
        if file
            .raw_diff
            .lines()
            .any(|line| line.starts_with("<LargeFile>"))
        {
            continue;
        }
        // A rename's old side is at `rename_from`; both sides of anything else are
        // at `file.path`. Each PRESENT side is converted with its OWN driver's
        // command; an absent side stays empty (no textconv on a missing blob).
        let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
        let old_text = match load(old_path, first_map)? {
            Some(bytes) => convert(old_cmd.as_deref(), &bytes)?,
            None => String::new(),
        };
        let new_text = match load(&file.path, second_map)? {
            Some(bytes) => convert(new_cmd.as_deref(), &bytes)?,
            None => String::new(),
        };
        converted.insert(file.path.clone(), (old_text, new_text));
    }

    // Pass 2: splice the re-diffed body (no fallible work).
    let mut done = std::collections::HashSet::new();
    files.retain_mut(|file| {
        let Some((old_text, new_text)) = converted.get(&file.path) else {
            return true;
        };
        let body = regen(old_text, new_text);
        done.insert(file.path.clone());
        // An exact rename has no content hunk; everything else does (Libra emits a
        // hunk for every changed file).
        let has_body = file.raw_diff.contains("\n@@ ");
        if body.trim().is_empty() {
            if !has_body {
                // Exact rename whose converted content is also identical: nothing
                // to add, keep the rename header.
                return true;
            }
            // Converted content is identical: drop a pure modification, but keep a
            // created/deleted/renamed entry (header only, zero counts).
            let keep_header = matches!(file.status.as_str(), "added" | "deleted" | "renamed");
            if !keep_header {
                return false;
            }
            file.insertions = 0;
            file.deletions = 0;
            file.hunks = Vec::new();
            file.raw_diff = strip_unified_diff_body(&file.raw_diff);
            return true;
        }
        let (insertions, deletions) = count_body_changes(&body);
        file.insertions = insertions;
        file.deletions = deletions;
        if has_body {
            file.raw_diff = splice_unified_body(&file.raw_diff, &body);
        } else if file.status != "renamed" {
            // The only no-hunk entry that can reach here is an exact rename (large
            // files were skipped in pass 1). Anything else: leave it untouched.
            return true;
        } else {
            // An exact rename whose converted sides DIFFER (e.g. the old/new paths
            // resolve different drivers) has no body yet — synthesize the
            // `index`/`---`/`+++`/hunk onto the existing rename header.
            let old_path = file
                .rename_from
                .clone()
                .unwrap_or_else(|| file.path.clone());
            let abbrev = |map: &HashMap<PathBuf, ObjectHash>, p: &str| {
                map.get(&PathBuf::from(p))
                    .map(|h| h.to_string()[..7].to_string())
                    .unwrap_or_else(|| "0000000".to_string())
            };
            let mut raw = file.raw_diff.clone();
            if !raw.ends_with('\n') {
                raw.push('\n');
            }
            raw.push_str(&format!(
                "index {}..{} 100644\n--- a/{old_path}\n+++ b/{}\n",
                abbrev(first_map, &old_path),
                abbrev(second_map, &file.path),
                file.path,
            ));
            raw.push_str(&body);
            file.raw_diff = raw;
        }
        file.hunks = parse_diff_hunks(&file.raw_diff);
        true
    });
    Ok(done)
}

/// zlib-deflate `data` (for `--binary` literal chunks). Uses `flate2` at the
/// default level; the bytes are valid zlib but NOT byte-identical to Git's own
/// `zlib` output (a documented divergence).
fn zlib_deflate(data: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

/// Encode `data` with Git's base85 (the `binary-patch` line format): each line
/// carries up to 52 bytes, prefixed by a length char (`A`-`Z` for 1-26 bytes,
/// `a`-`z` for 27-52), then 5 base85 digits per 4 bytes (zero-padded), big-endian.
fn git_base85(data: &[u8]) -> String {
    const ALPHABET: &[u8] =
        b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";
    let mut out = String::new();
    let mut i = 0;
    while i < data.len() {
        let n = (data.len() - i).min(52);
        out.push(if n <= 26 {
            (b'A' + n as u8 - 1) as char
        } else {
            (b'a' + n as u8 - 27) as char
        });
        let mut j = 0;
        while j < n {
            let mut acc: u32 = 0;
            for k in 0..4 {
                acc = (acc << 8) | if j + k < n { data[i + j + k] as u32 } else { 0 };
            }
            let mut digits = [0u8; 5];
            for d in (0..5).rev() {
                digits[d] = ALPHABET[(acc % 85) as usize];
                acc /= 85;
            }
            for &d in &digits {
                out.push(d as char);
            }
            j += 4;
        }
        out.push('\n');
        i += 52;
    }
    out
}

/// Rewrite the `index <old>..<new>[ <mode>]` line in a diff header to use FULL
/// object ids (Git's `--binary` implies `--full-index`), preserving the optional
/// trailing mode.
fn binary_index_full(header: &str, old_full: &str, new_full: &str) -> String {
    let had_trailing_newline = header.ends_with('\n');
    let mut result = header
        .lines()
        .map(|line| match line.strip_prefix("index ") {
            Some(rest) => {
                let suffix = rest
                    .split_once(' ')
                    .map(|(_, mode)| format!(" {mode}"))
                    .unwrap_or_default();
                format!("index {old_full}..{new_full}{suffix}")
            }
            None => line.to_string(),
        })
        .collect::<Vec<_>>()
        .join("\n");
    // `.lines()` drops the trailing terminator; restore it so a verbatim-rendered
    // patch keeps its structure (a text file's final newline; binary files are
    // already full-indexed and are not re-processed here).
    if had_trailing_newline {
        result.push('\n');
    }
    result
}

/// `--text`/`-a`: git_internal collapses a non-UTF-8 file to a bare
/// `Binary files differ`, but `--text` must show its content — re-diff such files
/// from the raw bytes (lossy UTF-8) and splice a content body onto the header.
fn force_text_for_bare_binary(
    files: &mut [DiffFileStat],
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &HashMap<PathBuf, ObjectHash>,
    context: usize,
) -> Result<(), DiffError> {
    let load = |path: &str, map: &HashMap<PathBuf, ObjectHash>| -> Result<Vec<u8>, DiffError> {
        let pb = PathBuf::from(path);
        let Some(hash) = map.get(&pb) else {
            return Ok(Vec::new());
        };
        if worktree_entries.get(&pb) == Some(hash) {
            read_worktree_blob_content(&pb)
        } else {
            load_repo_blob_content(hash)
        }
    };
    for file in files.iter_mut() {
        // Exact match (no `trim`) so a text hunk's context line ` Binary files
        // differ` is not mistaken for git_internal's bare binary marker.
        if !file
            .raw_diff
            .lines()
            .any(|line| line == "Binary files differ")
        {
            continue;
        }
        let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
        let old_text = String::from_utf8_lossy(&load(old_path, first_map)?).into_owned();
        let new_text = String::from_utf8_lossy(&load(&file.path, second_map)?).into_owned();
        let hunks = compute_unified_hunks(&old_text, &new_text, context);
        if hunks.trim().is_empty() {
            continue;
        }
        let (old_label, new_label) = match file.status.as_str() {
            "added" => ("/dev/null".to_string(), format!("b/{}", file.path)),
            "deleted" => (format!("a/{old_path}"), "/dev/null".to_string()),
            _ => (format!("a/{old_path}"), format!("b/{}", file.path)),
        };
        let header = {
            let cut = file.raw_diff.find("\nBinary files ");
            match cut {
                Some(pos) => file.raw_diff[..pos].to_string(),
                None => file.raw_diff.trim_end_matches('\n').to_string(),
            }
        };
        file.raw_diff = format!("{header}\n--- {old_label}\n+++ {new_label}\n{hunks}");
        file.hunks = parse_diff_hunks(&file.raw_diff);
        let (insertions, deletions) = count_body_changes(&hunks);
        file.insertions = insertions;
        file.deletions = deletions;
    }
    Ok(())
}

/// Detect binary files (a NUL byte in either side's content, surfaced as a NUL in
/// the internal content diff) and replace their patch body: with `--binary`, a
/// `GIT binary patch` (full-index header + base85 `literal` chunks for the new
/// then the old side); otherwise the `Binary files … differ` line. Sets each
/// file's `binary` marker (old/new sizes) so `--stat`/`--numstat` render `Bin …`
/// / `-`. Skipped for `--text` and textconv'd files (those are diffed as text).
fn apply_binary_detection(
    files: &mut [DiffFileStat],
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &HashMap<PathBuf, ObjectHash>,
    textconv_paths: &std::collections::HashSet<String>,
    want_patch: bool,
) -> Result<bool, DiffError> {
    let mut emitted_patch = false;
    let load = |path: &str, map: &HashMap<PathBuf, ObjectHash>| -> Result<Vec<u8>, DiffError> {
        let pb = PathBuf::from(path);
        let Some(hash) = map.get(&pb) else {
            return Ok(Vec::new());
        };
        if worktree_entries.get(&pb) == Some(hash) {
            read_worktree_blob_content(&pb)
        } else {
            load_repo_blob_content(hash)
        }
    };

    for file in files.iter_mut() {
        if textconv_paths.contains(&file.path) {
            continue;
        }
        // A file is binary if its content diff carries a NUL byte (a text diff
        // never does), OR git_internal already collapsed it to a bare
        // `Binary files differ` line (it does that for non-UTF-8 content). The
        // marker is matched EXACTLY (no `trim`) so a text hunk's context line
        // ` Binary files differ` is not mistaken for it.
        let bare_binary = file
            .raw_diff
            .lines()
            .any(|line| line == "Binary files differ");
        let raw_signal = file.raw_diff.contains('\0') || bare_binary;
        let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
        // A rename's body was reconstructed via lossy UTF-8 (`build_rename_entry`),
        // so the raw-diff signal is unreliable for it — scan the actual blob bytes
        // (NUL or non-UTF-8 = binary, matching git_internal). Non-rename files
        // keep the cheap raw-diff signal (no blob load when clearly text).
        let (old_bytes, new_bytes, is_binary) = if raw_signal {
            (
                load(old_path, first_map)?,
                load(&file.path, second_map)?,
                true,
            )
        } else if file.status == "renamed" {
            let is_binary_bytes = |b: &[u8]| b.contains(&0) || std::str::from_utf8(b).is_err();
            let old = load(old_path, first_map)?;
            let new = load(&file.path, second_map)?;
            let binary = is_binary_bytes(&old) || is_binary_bytes(&new);
            (old, new, binary)
        } else {
            continue;
        };
        if !is_binary {
            continue;
        }
        // An exact rename of a binary file (identical bytes) stays header-only —
        // Git shows the rename headers with no `Binary files … differ` body — but
        // it is still binary metadata for `--stat` (bare `Bin`), `--numstat`
        // (`-`/`-`), and JSON, so record the marker without touching the body.
        if old_bytes == new_bytes {
            file.binary = Some((old_bytes.len() as u64, new_bytes.len() as u64));
            continue;
        }

        // The `Binary files <a> and <b> differ` labels come from the existing
        // `---`/`+++` lines when present; the bare-marker form has none, so fall
        // back to the status (so a created/deleted side uses `/dev/null`).
        let label = |prefix: &str| {
            file.raw_diff
                .lines()
                .find_map(|line| line.strip_prefix(prefix).map(str::to_string))
        };
        let (default_old, default_new) = match file.status.as_str() {
            "added" => ("/dev/null".to_string(), format!("b/{}", file.path)),
            "deleted" => (format!("a/{old_path}"), "/dev/null".to_string()),
            _ => (format!("a/{old_path}"), format!("b/{}", file.path)),
        };
        let old_label = label("--- ").unwrap_or(default_old);
        let new_label = label("+++ ").unwrap_or(default_new);

        // Header = `diff --git` + mode + `index`, stopping before the body —
        // which for the bare-marker form is the `Binary files differ` line itself.
        let header = {
            let cut = file
                .raw_diff
                .find("\n--- ")
                .or_else(|| file.raw_diff.find("\n@@ "))
                .or_else(|| file.raw_diff.find("\nBinary files "));
            match cut {
                Some(pos) => file.raw_diff[..pos].to_string(),
                None => file.raw_diff.trim_end_matches('\n').to_string(),
            }
        };
        let raw = if want_patch {
            let hash_of = |map: &HashMap<PathBuf, ObjectHash>, p: &str| {
                map.get(&PathBuf::from(p)).map(|h| h.to_string())
            };
            let old_id = hash_of(first_map, old_path);
            let new_id = hash_of(second_map, &file.path);
            let width = old_id
                .as_ref()
                .or(new_id.as_ref())
                .map(String::len)
                .unwrap_or(40);
            let zeros = "0".repeat(width);
            let old_full = old_id.unwrap_or_else(|| zeros.clone());
            let new_full = new_id.unwrap_or(zeros);
            format!(
                "{}\nGIT binary patch\nliteral {}\n{}\nliteral {}\n{}\n",
                binary_index_full(&header, &old_full, &new_full),
                new_bytes.len(),
                git_base85(&zlib_deflate(&new_bytes)),
                old_bytes.len(),
                git_base85(&zlib_deflate(&old_bytes)),
            )
        } else {
            format!("{header}\nBinary files {old_label} and {new_label} differ\n")
        };
        file.raw_diff = raw;
        file.binary = Some((old_bytes.len() as u64, new_bytes.len() as u64));
        file.insertions = 0;
        file.deletions = 0;
        file.hunks = Vec::new();
        if want_patch {
            emitted_patch = true;
        }
    }
    Ok(emitted_patch)
}

/// Replace each file's patch body with the output of the configured external
/// diff driver (`diff.external`), following Git's `GIT_EXTERNAL_DIFF` protocol:
/// the command is invoked as `cmd path old-file old-hex old-mode new-file
/// new-hex new-mode` and its stdout becomes that file's diff. A missing side
/// uses `/dev/null` with `.` for its hex and mode; a new side that is the live
/// working tree reports an all-zero hash (uncommitted), matching Git. The
/// command is run through the shell so a `diff.external` value carrying its own
/// arguments works.
fn apply_external_diff(
    files: &mut [DiffFileStat],
    command: &str,
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &HashMap<PathBuf, ObjectHash>,
) -> Result<(), DiffError> {
    use std::io::Write as _;

    // Materialize one side to a temp file (or `/dev/null` when absent), returning
    // (file-arg, hex-arg, mode-arg, keep-alive temp). The temp must outlive the
    // command run, so the caller holds the returned handle.
    let materialize = |hash: Option<&ObjectHash>,
                       is_worktree: bool,
                       wt_path: &Path,
                       mode: &str|
     -> Result<(String, String, String, Option<NamedTempFile>), DiffError> {
        let Some(hash) = hash else {
            return Ok((
                "/dev/null".to_string(),
                ".".to_string(),
                ".".to_string(),
                None,
            ));
        };
        let content = if is_worktree {
            read_worktree_blob_content(&wt_path.to_path_buf())?
        } else {
            load_repo_blob_content(hash)?
        };
        let mut tmp = NamedTempFile::new().map_err(|e| DiffError::FileRead {
            path: wt_path.display().to_string(),
            detail: format!("failed to create external-diff temp file: {e}"),
        })?;
        tmp.write_all(&content).map_err(|e| DiffError::FileRead {
            path: wt_path.display().to_string(),
            detail: format!("failed to write external-diff temp file: {e}"),
        })?;
        let arg = tmp.path().to_string_lossy().into_owned();
        // For a live working-tree side, read the real mode from disk (accurate for
        // executables/symlinks). For a tree/index side, use the mode carried in
        // the internal patch headers. (Libra's internal diff currently renders a
        // regular-file mode of 100644 even for an executable tree entry, so a
        // tree-side mode can under-report the executable bit — a pre-existing diff
        // limitation, not specific to the external driver.)
        let mode = if is_worktree {
            worktree_file_mode(wt_path)
        } else {
            mode.to_string()
        };
        // An uncommitted working-tree side has no object id yet: Git reports an
        // all-zero hash (of the active hash kind's hex width).
        let hex = if is_worktree {
            "0".repeat(hash.to_string().len())
        } else {
            hash.to_string()
        };
        Ok((arg, hex, mode, Some(tmp)))
    };

    // A side reads from the working tree iff its blob id matches the worktree
    // entry — which can be EITHER side once `-R` swaps them.
    let side_is_worktree = |path: &Path, hash: Option<&ObjectHash>| -> bool {
        hash.is_some_and(|h| worktree_entries.get(path) == Some(h))
    };

    let total = files.len();
    for (index, file) in files.iter_mut().enumerate() {
        let path = PathBuf::from(&file.path);
        // For a detected rename the old side lives at `rename_from`, not at the
        // new path, so the driver sees the renamed source rather than `/dev/null`.
        let old_path = file
            .rename_from
            .as_deref()
            .map(PathBuf::from)
            .unwrap_or_else(|| path.clone());
        let (old_mode, new_mode) = external_diff_modes(&file.raw_diff);

        let (old_file, old_hex, old_mode_arg, _old_tmp) = materialize(
            first_map.get(&old_path),
            side_is_worktree(&old_path, first_map.get(&old_path)),
            &old_path,
            &old_mode,
        )?;
        let (new_file, new_hex, new_mode_arg, _new_tmp) = materialize(
            second_map.get(&path),
            side_is_worktree(&path, second_map.get(&path)),
            &path,
            &new_mode,
        )?;

        let result = std::process::Command::new("sh")
            .arg("-c")
            .arg(format!("{command} \"$@\""))
            .arg(command)
            .arg(&file.path)
            .arg(&old_file)
            .arg(&old_hex)
            .arg(&old_mode_arg)
            .arg(&new_file)
            .arg(&new_hex)
            .arg(&new_mode_arg)
            // Git exports the per-path counters so drivers can show progress.
            .env("GIT_DIFF_PATH_COUNTER", (index + 1).to_string())
            .env("GIT_DIFF_PATH_TOTAL", total.to_string())
            .output()
            .map_err(|e| DiffError::FileRead {
                path: file.path.clone(),
                detail: format!("failed to run external diff driver '{command}': {e}"),
            })?;
        // A non-zero exit is fatal in Git; surface it with the driver's stderr.
        if !result.status.success() {
            let stderr = String::from_utf8_lossy(&result.stderr);
            return Err(DiffError::FileRead {
                path: file.path.clone(),
                detail: format!(
                    "external diff driver '{command}' failed ({}){}",
                    result.status,
                    if stderr.trim().is_empty() {
                        String::new()
                    } else {
                        format!(": {}", stderr.trim())
                    }
                ),
            });
        }
        // Git emits the external command's stdout verbatim as that file's diff.
        file.raw_diff = String::from_utf8_lossy(&result.stdout).into_owned();
        // The internal hunks no longer describe the (external) output.
        file.hunks = Vec::new();
    }
    Ok(())
}

fn record_diff_content_error(slot: &Rc<RefCell<Option<DiffError>>>, error: DiffError) {
    let mut slot = slot.borrow_mut();
    if slot.is_none() {
        *slot = Some(error);
    }
}

/// Identify the first whitespace problem on an added line's content (the text
/// after the leading `+`). Returns `None` for a clean line. Checks Git's two
/// most common defaults: trailing whitespace (`blank-at-eol`) and a space
/// immediately before a tab in the leading indent (`space-before-tab`).
fn whitespace_problem(content: &str) -> Option<&'static str> {
    if content.ends_with(' ') || content.ends_with('\t') {
        return Some("trailing whitespace");
    }
    let indent: String = content
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .collect();
    if indent.contains(" \t") {
        return Some("space before tab in indent");
    }
    None
}

/// Scan one file's unified diff for whitespace errors on added (`+`) lines,
/// tracking new-file line numbers from each hunk header. Returns one
/// `path:line: message` string per problem.
fn check_whitespace_in_file(path: &str, raw_diff: &str) -> Vec<String> {
    let mut problems = Vec::new();
    let mut new_lineno = 0usize;
    for line in raw_diff.lines() {
        if line.starts_with("@@") {
            // `@@ -a,b +c,d @@`: the next added/context line is new-file line c.
            if let Some(after_plus) = line.split('+').nth(1)
                && let Some(start) = after_plus
                    .split([',', ' '])
                    .next()
                    .and_then(|s| s.parse::<usize>().ok())
            {
                new_lineno = start;
            }
        } else if line.starts_with("+++") || line.starts_with("---") {
            // File headers — not content; do not advance.
        } else if let Some(content) = line.strip_prefix('+') {
            // Added line: check whitespace, then advance the new-file counter.
            if let Some(msg) = whitespace_problem(content) {
                problems.push(format!("{path}:{new_lineno}: {msg}"));
            }
            new_lineno += 1;
        } else if line.starts_with(' ') {
            // Context line: advances the new-file counter.
            new_lineno += 1;
        }
        // Everything else — removed (`-`) lines, the `\ No newline at end of
        // file` marker, and `diff --git`/`index`/mode headers — is neither an
        // added nor a context line and does not advance the counter.
    }
    problems
}

/// `diff --check`: print whitespace warnings and exit 2 when any are found.
fn render_diff_check(result: &DiffOutput) -> CliResult<()> {
    let problems: Vec<String> = result
        .files
        .iter()
        .flat_map(|file| check_whitespace_in_file(&file.path, &file.raw_diff))
        .collect();
    if problems.is_empty() {
        return Ok(());
    }
    println!("{}", problems.join("\n"));
    Err(CliError::silent_exit(2))
}

fn render_diff_output(
    args: &DiffArgs,
    result: &DiffOutput,
    output: &OutputConfig,
) -> CliResult<()> {
    // Validate `--color-moved[=<mode>]` up front (even for non-colored paths, so a
    // bad mode is rejected like Git does at parse time).
    let color_moved = color_moved_active(args)?;
    // `--check` replaces the normal diff output with whitespace-error warnings.
    if args.check {
        return render_diff_check(result);
    }
    if output.is_json() {
        emit_json_data("diff", result, output)?;
        // `--exit-code` applies regardless of output format: emit the JSON, then
        // signal differences via the process status.
        return diff_exit_result(args, result);
    }

    if output.quiet && args.output.is_none() {
        return if result.files_changed > 0 {
            Err(CliError::silent_exit(1))
        } else {
            Ok(())
        };
    }

    // --output writes are an explicit side-effect and must be honored even
    // when --quiet is set (quiet only suppresses stdout, not file writes).
    // `-z` NUL-terminates each record; `--name-status` then separates the
    // status and path with a NUL instead of a tab.
    let rendered = if args.name_only {
        join_diff_records(result.files.iter().map(|file| file.path.clone()), args.null)
    } else if args.name_status {
        let field_sep = if args.null { '\0' } else { '\t' };
        join_diff_records(
            result.files.iter().map(|file| {
                if file.status == "renamed" {
                    // `R<score>` then old + new paths (Git pads the score to 3 digits).
                    format!(
                        "R{:03}{sep}{}{sep}{}",
                        file.similarity.unwrap_or(0),
                        file.rename_from.as_deref().unwrap_or(""),
                        file.path,
                        sep = field_sep,
                    )
                } else {
                    format!(
                        "{}{}{}",
                        diff_status_letter(&file.status),
                        field_sep,
                        file.path
                    )
                }
            }),
            args.null,
        )
    } else if args.numstat {
        join_diff_records(
            result.files.iter().map(|file| {
                // Binary files report `-` for both counts (matching Git).
                let (ins, del) = if file.binary.is_some() {
                    ("-".to_string(), "-".to_string())
                } else {
                    (file.insertions.to_string(), file.deletions.to_string())
                };
                if file.status == "renamed" {
                    let from = file.rename_from.as_deref().unwrap_or("");
                    if args.null {
                        // `<ins>\t<del>\t\0<old>\0<new>` (empty path column, then NUL-separated).
                        format!("{ins}\t{del}\t\0{from}\0{}", file.path)
                    } else {
                        format!("{ins}\t{del}\t{}", rename_display(from, &file.path))
                    }
                } else {
                    format!("{ins}\t{del}\t{}", file.path)
                }
            }),
            args.null,
        )
    } else if args.stat {
        format_diff_stat_output(result)
    } else if args.shortstat {
        format_diff_shortstat_output(result)
    } else if args.summary {
        format_diff_summary(result)
    } else if args.no_patch {
        // `-s` / `--no-patch`: suppress the patch body (used for status-only
        // checks, typically with `--exit-code`).
        String::new()
    } else if result.external_diff_applied || result.binary_patch {
        // External-driver and `--binary` output is emitted verbatim — exact
        // concatenation, no trailing-newline normalization (a `GIT binary patch`
        // ends with a blank line that Git's parser requires), no coloring.
        result
            .files
            .iter()
            .map(|file| file.raw_diff.as_str())
            .collect()
    } else {
        format_unified_diff(result)
    };

    if let Some(path) = &args.output {
        std::fs::write(path, rendered.as_bytes())
            .map_err(|e| DiffError::OutputWrite {
                path: path.clone(),
                detail: e.to_string(),
            })
            .map_err(CliError::from)?;
        if output.quiet && result.files_changed > 0 {
            return Err(CliError::silent_exit(1));
        }
        return diff_exit_result(args, result);
    }

    if output.quiet {
        if result.files_changed > 0 {
            return Err(CliError::silent_exit(1));
        }
        return Ok(());
    }

    if rendered.is_empty() {
        return diff_exit_result(args, result);
    }
    let mut pager = Pager::with_config(output)?;
    let rendered = if args.name_only
        || args.name_status
        || args.numstat
        || args.stat
        || args.shortstat
        || args.summary
        || word_diff_active(args)
        || result.external_diff_applied
        || result.binary_patch
    {
        rendered
    } else {
        // Honor `--color`: `always` forces color even when piped (the global
        // `colored` override is already set), `never` disables it, `auto` follows
        // the terminal. (Previously this only checked the terminal, so
        // `--color=always | pipe` produced no color — and no moved-line color.)
        let should_colorize = match output.color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => io::stdout().is_terminal(),
        };
        maybe_colorize_diff(&rendered, should_colorize, color_moved)
    };
    // `-z` records carry their own NUL terminators, and external-driver output is
    // emitted byte-for-byte, so neither gets an appended trailing newline.
    let z_records = args.null && (args.name_only || args.name_status || args.numstat);
    // The verbatim (no trailing-newline) write path applies only when the PATCH
    // body is actually rendered — `--binary --stat`/`--numstat` still get the
    // normal trailing newline even though `binary_patch` is set.
    let verbatim_patch =
        result.external_diff_applied || (result.binary_patch && patch_body_is_shown(args));
    if z_records || verbatim_patch {
        pager.write_str(&rendered)?;
    } else {
        pager.write_str(&format!("{rendered}\n"))?;
    }
    pager.finish()?;
    diff_exit_result(args, result)
}

/// Join name/numstat records: NUL-terminate each record under `-z`, otherwise
/// newline-separate them (the trailing newline is added by the caller).
fn join_diff_records(records: impl Iterator<Item = String>, null: bool) -> String {
    if null {
        records.map(|r| format!("{r}\0")).collect()
    } else {
        records.collect::<Vec<_>>().join("\n")
    }
}

/// `--exit-code`: exit 1 when the diff is non-empty, 0 otherwise. The diff
/// output (if any) has already been emitted by the time this is called, so the
/// silent exit only sets the process status (unlike `--quiet`, which also
/// suppresses output).
fn diff_exit_result(args: &DiffArgs, result: &DiffOutput) -> CliResult<()> {
    if args.exit_code && result.files_changed > 0 {
        Err(CliError::silent_exit(1))
    } else {
        Ok(())
    }
}

/// Render `--summary`: one line per created file, deleted file, or detected
/// rename (`-M`); plain content modifications produce no line, matching
/// `git diff --summary`. Mode-only changes are not surfaced.
fn format_diff_summary(result: &DiffOutput) -> String {
    result
        .files
        .iter()
        .filter_map(summary_line)
        .collect::<Vec<_>>()
        .join("\n")
}

fn summary_line(file: &DiffFileStat) -> Option<String> {
    if file.status == "renamed" {
        return Some(format!(
            " rename {} ({}%)",
            rename_display(file.rename_from.as_deref().unwrap_or(""), &file.path),
            file.similarity.unwrap_or(0),
        ));
    }
    let find = |prefix: &str| {
        file.raw_diff
            .lines()
            .find_map(|l| l.strip_prefix(prefix))
            .map(str::trim)
    };
    if let Some(mode) = find("new file mode ") {
        return Some(format!(" create mode {} {}", mode, file.path));
    }
    if let Some(mode) = find("deleted file mode ") {
        return Some(format!(" delete mode {} {}", mode, file.path));
    }
    None
}

fn diff_status_letter(status: &str) -> &'static str {
    match status {
        "added" => "A",
        "deleted" => "D",
        _ => "M",
    }
}

/// Render a rename path pair the way Git's `pprint_rename` does for `--stat` /
/// `--numstat` / `--summary`: factor out the common leading directory and the
/// common trailing component (both cut at `/` boundaries) into
/// `prefix{old => new}suffix`, or `old => new` when nothing is shared.
fn rename_display(old: &str, new: &str) -> String {
    let oa = old.as_bytes();
    let nb = new.as_bytes();
    let mut pfx = 0;
    let mut i = 0;
    while i < oa.len() && i < nb.len() && oa[i] == nb[i] {
        if oa[i] == b'/' {
            pfx = i + 1;
        }
        i += 1;
    }
    let mut sfx = 0;
    let (mut oi, mut ni) = (oa.len(), nb.len());
    while oi > pfx && ni > pfx && oa[oi - 1] == nb[ni - 1] {
        oi -= 1;
        ni -= 1;
        if oa[oi] == b'/' {
            sfx = oa.len() - oi;
        }
    }
    if pfx == 0 && sfx == 0 {
        format!("{old} => {new}")
    } else {
        format!(
            "{}{{{} => {}}}{}",
            &old[..pfx],
            &old[pfx..oa.len() - sfx],
            &new[pfx..nb.len() - sfx],
            &old[oa.len() - sfx..],
        )
    }
}

fn format_unified_diff(result: &DiffOutput) -> String {
    result
        .files
        .iter()
        .map(|file| file.raw_diff.trim_end_matches('\n'))
        .collect::<Vec<_>>()
        .join("\n")
}

/// git_internal's `Diff::diff` hard-codes 3 context lines. For `-U<n>` with a
/// different `n`, replace a single file's hunk body with one regenerated at `n`
/// context lines while keeping git_internal's header (`diff --git` / mode /
/// `index` / `---` / `+++`). A diff with no hunk line (binary marker or
/// identical content) is returned unchanged.
fn rewrite_unified_diff_context(
    raw_diff: &str,
    old_text: &str,
    new_text: &str,
    context: usize,
) -> String {
    splice_unified_body(
        raw_diff,
        &compute_unified_hunks(old_text, new_text, context),
    )
}

/// Replace a single file's hunk body with `body`, keeping git_internal's header
/// (`diff --git` / mode / `index` / `---` / `+++`). A diff with no hunk line
/// (binary marker or identical content) is returned unchanged.
fn splice_unified_body(raw_diff: &str, body: &str) -> String {
    // The header runs up to and including the newline before the first hunk.
    let Some(nl_before_hunk) = raw_diff.find("\n@@ ") else {
        return raw_diff.to_string();
    };
    format!("{}{}", &raw_diff[..=nl_before_hunk], body)
}

/// Drop the unified diff (the `--- …`/`+++ …`/`@@`/body) from a file diff, keeping
/// only the extended header (`diff --git`, `new file mode` / `deleted file mode`,
/// `index`). Matches Git's output for an added/deleted file whose only content is
/// blank lines under `--ignore-blank-lines`: the file-level change is still listed
/// (in `--name-only`/`--stat`/`--summary` and the patch header) but carries no hunk.
fn strip_unified_diff_body(raw_diff: &str) -> String {
    let cut = raw_diff.find("\n--- ").or_else(|| raw_diff.find("\n@@ "));
    match cut {
        Some(pos) => raw_diff[..pos].to_string(),
        None => raw_diff.trim_end_matches('\n').to_string(),
    }
}

/// Internal representation of diff lines used while assembling unified hunks.
/// Ported from git_internal's private `compute_unified_diff` so `-U<n>` matches
/// its (git-faithful) hunk layout for any context width.
#[derive(Debug, Clone, Copy)]
enum UnifiedEditLine<'a> {
    Context(Option<usize>, Option<usize>, &'a str),
    Delete(usize, &'a str),
    Insert(usize, &'a str),
}

/// Compute the unified-diff hunk body (the `@@ … @@` blocks, no file header)
/// for `old_text` vs `new_text` at `context` lines of surrounding context.
/// Myers line diff with a rolling-context assembler — a context-parameterized
/// copy of git_internal's `compute_unified_diff` so the output matches its
/// default (3-context) layout that is already validated against real Git.
fn compute_unified_hunks(old_text: &str, new_text: &str, context: usize) -> String {
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_lines(old_text, new_text);
    let changes: Vec<(ChangeTag, &str)> = diff
        .iter_all_changes()
        .map(|c| (c.tag(), c.value().trim_end_matches(['\r', '\n'])))
        .collect();
    assemble_unified_hunks(&changes, context, old_text.len() + new_text.len())
}

/// Normalizer for `-w` / `--ignore-all-space`: drop every whitespace character
/// so two lines compare equal iff they match after all whitespace is removed.
fn normalize_ignore_all_space(line: &str) -> String {
    line.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Normalizer for `-b` / `--ignore-space-change`: ignore changes in the AMOUNT
/// of whitespace — every maximal run of whitespace collapses to a single space,
/// and trailing whitespace is dropped. The PRESENCE of whitespace still matters,
/// so `"a  b"` ≡ `"a b"` and `"\ta"` ≡ `"  a"` (both `" a"`), but `"a b"` ≠ `"ab"`
/// and `"a"` ≠ `"  a"`. Matches `git diff -b` (verified empirically).
fn normalize_ignore_space_change(line: &str) -> String {
    let trimmed = line.trim_end();
    let mut out = String::with_capacity(trimmed.len());
    let mut in_ws = false;
    for c in trimmed.chars() {
        if c.is_whitespace() {
            in_ws = true;
        } else {
            if in_ws {
                out.push(' ');
                in_ws = false;
            }
            out.push(c);
        }
    }
    out
}

/// Normalizer for `--ignore-space-at-eol`: ignore only trailing whitespace;
/// leading and internal whitespace compare exactly. Matches `git diff
/// --ignore-space-at-eol` (verified empirically).
fn normalize_ignore_space_at_eol(line: &str) -> String {
    line.trim_end().to_string()
}

/// Normalizer for `--ignore-cr-at-eol`: strip ALL trailing carriage returns so
/// a CRLF↔LF-only change compares equal; anything else (mid-line `\r`, trailing
/// spaces) still compares exactly. Stripping all — not one — keeps the two
/// record-splitting paths consistent: the main re-diff path splits with
/// `str::lines()` (which already drops the terminator's `\r` before the
/// normalizer runs), while the `--ignore-blank-lines` composition path
/// raw-splits on `\n` keeping `\r` bytes; with strip-all both paths equate
/// exactly the same line pairs. See the flag's doc for the documented
/// approximation vs Git's non-transitive allow-one-remaining-CR comparison.
fn normalize_ignore_cr_at_eol(line: &str) -> String {
    line.trim_end_matches('\r').to_string()
}

/// Compute the unified-diff hunk body for `old_text` vs `new_text` at `context`
/// lines, comparing lines through `normalize` (e.g. whitespace-insensitive for
/// `-w`) while EMITTING the original line text. Returns an empty string when the
/// two sides are equal under `normalize` (so the caller drops the file, matching
/// `git diff -w`). Context lines are emitted from the new (post-image) side, as
/// Git does; deletes from the old side, inserts from the new side.
fn compute_unified_hunks_normalized(
    old_text: &str,
    new_text: &str,
    context: usize,
    normalize: fn(&str) -> String,
) -> String {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let old_norm: Vec<String> = old_lines.iter().map(|l| normalize(l)).collect();
    let new_norm: Vec<String> = new_lines.iter().map(|l| normalize(l)).collect();
    // `diff_slices` compares `&[&str]` elements; borrow the normalized strings.
    let old_norm_ref: Vec<&str> = old_norm.iter().map(String::as_str).collect();
    let new_norm_ref: Vec<&str> = new_norm.iter().map(String::as_str).collect();
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_slices(&old_norm_ref, &new_norm_ref);
    let mut changes: Vec<(ChangeTag, &str)> = Vec::with_capacity(old_lines.len() + new_lines.len());
    for change in diff.iter_all_changes() {
        let tag = change.tag();
        let text = match tag {
            ChangeTag::Delete => change.old_index().map(|i| old_lines[i]).unwrap_or(""),
            ChangeTag::Insert => change.new_index().map(|i| new_lines[i]).unwrap_or(""),
            // Context: both sides are equal under `normalize`; Git emits the
            // post-image (new) line, falling back to the old side.
            ChangeTag::Equal => change
                .new_index()
                .map(|i| new_lines[i])
                .or_else(|| change.old_index().map(|i| old_lines[i]))
                .unwrap_or(""),
        };
        changes.push((tag, text));
    }
    assemble_unified_hunks(&changes, context, old_text.len() + new_text.len())
}

/// A contiguous change group of a diff: `chg1` old lines starting at 0-based old
/// index `i1` are replaced by `chg2` new lines starting at 0-based new index `i2`.
/// `ignore` is set when every line the group touches is blank (truly empty) — the
/// unit `--ignore-blank-lines` operates on.
struct DiffChangeGroup {
    i1: usize,
    chg1: usize,
    i2: usize,
    chg2: usize,
    ignore: bool,
}

/// Compute the unified-diff hunk body for `--ignore-blank-lines`, faithfully
/// porting Git's `xdl_get_hunk` blank-aware hunk selection (xdiff/xemit.c).
///
/// A blank-only change group does not anchor a hunk: a leading blank-only group
/// that is `>= ctxlen` lines before the next change is dropped, and a blank-only
/// group `>= ctxlen` after the previous change is not pulled in — so a blank far
/// from any real change vanishes (its own hunk would be empty of real changes and
/// is never emitted). A blank within `< ctxlen` of a real change rides along and
/// is shown in full, extending the hunk like any change. "Blank" means a TRULY
/// EMPTY line — a whitespace-only line is not blank. Returns "" when no hunk
/// survives (the caller drops the file).
///
/// Verified line-for-line against real Git across the merge/no-merge boundary: a
/// far leading blank yields the content hunk only (`@@ -5,4 +6,4 @@`); an
/// in-window blank merges (`@@ -1,4 +1,5 @@`, blank shown); two real changes that
/// bracket a blank merge and show it; and the gap threshold is exactly `< ctxlen`.
///
/// `normalize` composes a whitespace-ignoring flag with `--ignore-blank-lines`
/// (e.g. `git diff -w --ignore-blank-lines`): when `Some`, lines are diffed and
/// classified-as-blank through the normalizer (so a whitespace-only line counts as
/// blank under `-w`) while the ORIGINAL line text is emitted; when `None`, raw
/// lines are used and "blank" means a byte-empty line (a `\r`-only CRLF line is NOT
/// blank).
///
/// LIMITATION (pre-existing, shared by every Libra diff mode): Libra's diff models
/// lines by content only and does not track line terminators, so it cannot emit
/// Git's `\ No newline at end of file` marker, cannot detect a terminator-only
/// change (`a\n` vs `a` compare equal), and does not emulate Git's
/// terminator-dependent `xdl_blankline` `size<=1` blanking of an unterminated final
/// line. For files whose final line lacks a trailing newline this may diverge from
/// Git — exactly as `libra diff` / `-w` / `-U<n>` already do. The flag is faithful
/// for all newline-terminated files (the domain Libra models).
fn compute_unified_hunks_ignore_blank(old_text: &str, new_text: &str, context: usize) -> String {
    compute_unified_hunks_ignore_blank_inner(old_text, new_text, context, None)
}

/// `--ignore-blank-lines` composed with a whitespace normalizer (see
/// [`compute_unified_hunks_ignore_blank`]).
fn compute_unified_hunks_ignore_blank_normalized(
    old_text: &str,
    new_text: &str,
    context: usize,
    normalize: fn(&str) -> String,
) -> String {
    compute_unified_hunks_ignore_blank_inner(old_text, new_text, context, Some(normalize))
}

fn compute_unified_hunks_ignore_blank_inner(
    old_text: &str,
    new_text: &str,
    context: usize,
    normalize: Option<fn(&str) -> String>,
) -> String {
    // Raw records: split on '\n' WITHOUT trimming '\r', so a `\r`-only CRLF blank
    // line is non-empty (Git does not treat it as blank without a whitespace flag),
    // and so emitted lines keep their original bytes.
    let old_lines: Vec<&str> = if old_text.is_empty() {
        Vec::new()
    } else {
        old_text.split('\n').collect()
    };
    let new_lines: Vec<&str> = if new_text.is_empty() {
        Vec::new()
    } else {
        new_text.split('\n').collect()
    };
    // `split('\n')` leaves a trailing "" when the text ends in a newline; drop it so
    // the record counts match the real line counts.
    let nrec1 = old_lines
        .len()
        .saturating_sub(old_text.ends_with('\n') as usize);
    let nrec2 = new_lines
        .len()
        .saturating_sub(new_text.ends_with('\n') as usize);
    let old_recs = &old_lines[..nrec1];
    let new_recs = &new_lines[..nrec2];

    // Comparison lines: normalized when composing a whitespace flag, else a copy of
    // the raw records. The diff and blank classification run on these; emission uses
    // the original `old_recs`/`new_recs`. `cmp_*`/`*_ref` live to function scope so
    // the borrowed `diff` outlives them.
    let to_cmp = |recs: &[&str]| -> Vec<String> {
        match normalize {
            Some(normalize) => recs.iter().map(|l| normalize(l)).collect(),
            None => recs.iter().map(|l| l.to_string()).collect(),
        }
    };
    let cmp_old = to_cmp(old_recs);
    let cmp_new = to_cmp(new_recs);
    let old_ref: Vec<&str> = cmp_old.iter().map(String::as_str).collect();
    let new_ref: Vec<&str> = cmp_new.iter().map(String::as_str).collect();
    let diff = TextDiff::configure()
        .algorithm(Algorithm::Myers)
        .diff_slices(&old_ref, &new_ref);

    // Build change groups (maximal runs of insert/delete), tracking 0-based old/new
    // positions exactly as Git records i1/i2/chg1/chg2.
    let mut groups: Vec<DiffChangeGroup> = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;
    let mut cur: Option<DiffChangeGroup> = None;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                if let Some(g) = cur.take() {
                    groups.push(g);
                }
                old_pos += 1;
                new_pos += 1;
            }
            ChangeTag::Delete => {
                let g = cur.get_or_insert(DiffChangeGroup {
                    i1: old_pos,
                    chg1: 0,
                    i2: new_pos,
                    chg2: 0,
                    ignore: true,
                });
                g.chg1 += 1;
                old_pos += 1;
            }
            ChangeTag::Insert => {
                let g = cur.get_or_insert(DiffChangeGroup {
                    i1: old_pos,
                    chg1: 0,
                    i2: new_pos,
                    chg2: 0,
                    ignore: true,
                });
                g.chg2 += 1;
                new_pos += 1;
            }
        }
    }
    if let Some(g) = cur.take() {
        groups.push(g);
    }
    // Mark groups whose every touched line is blank. Without a whitespace flag,
    // blank = byte-empty (Git does not treat a `\r`-only CRLF line as blank).
    // Under ANY whitespace flag, Git's `xdl_blankline` classifies an
    // all-whitespace line as blank — equivalent to empty-after-normalize for
    // `-w`/`-b`/`--ignore-space-at-eol`, but NOT for `--ignore-cr-at-eol`
    // (`"  \r"` normalizes to `"  "`, non-empty, yet Git counts it blank), so
    // classify on the RAW record's all-whitespace test when composing. Libra's
    // diff models lines by content only and does not track line terminators, so
    // Git's terminator-dependent `size<=1` quirk for an unterminated final line
    // is intentionally NOT emulated — see the limitation note below.
    let raw_blank = |recs: &[&str]| recs.iter().all(|l| l.trim().is_empty());
    for g in groups.iter_mut() {
        let (old_blank, new_blank) = if normalize.is_some() {
            (
                raw_blank(&old_recs[g.i1..g.i1 + g.chg1]),
                raw_blank(&new_recs[g.i2..g.i2 + g.chg2]),
            )
        } else {
            (
                cmp_old[g.i1..g.i1 + g.chg1].iter().all(|l| l.is_empty()),
                cmp_new[g.i2..g.i2 + g.chg2].iter().all(|l| l.is_empty()),
            )
        };
        g.ignore = old_blank && new_blank;
    }

    let max_common = context.saturating_mul(2);
    let max_ignorable = context;
    let mut out = String::with_capacity(((old_text.len() + new_text.len()) / 16).max(256));

    // Emit loop: mirrors `xdl_emit_diff`'s hunk iteration over `xdl_get_hunk`.
    let mut start = 0usize;
    while start < groups.len() {
        // Prelude: "remove ignorable changes that are too far before other changes"
        // (Git's xdl_get_hunk). Walk `p` through every consecutive leading ignorable
        // group; whenever the next change is `>= max_ignorable` away or absent,
        // advance `start` past it. Walking past a close ignorable group without
        // advancing `start` lets a run of blank-only changes with no nearby real
        // change collapse to nothing (start reaches `groups.len()` → no hunk).
        let mut p = start;
        while p < groups.len() && groups[p].ignore {
            let cur = &groups[p];
            let far_or_end = match groups.get(p + 1) {
                None => true,
                Some(next) => next.i1 - (cur.i1 + cur.chg1) >= max_ignorable,
            };
            if far_or_end {
                start = p + 1;
            }
            p += 1;
        }
        if start >= groups.len() {
            break;
        }

        // `xdl_get_hunk`: find `lxch` (last group in this hunk).
        let mut lxch = start;
        let mut ignored = 0usize;
        let mut prev = start;
        let mut idx = start + 1;
        while idx < groups.len() {
            let distance = groups[idx].i1 - (groups[prev].i1 + groups[prev].chg1);
            if distance > max_common {
                break;
            }
            if distance < max_ignorable && (!groups[idx].ignore || lxch == prev) {
                lxch = idx;
                ignored = 0;
            } else if distance < max_ignorable && groups[idx].ignore {
                ignored += groups[idx].chg2;
            } else if lxch != prev
                && groups[idx].i1 + ignored > groups[lxch].i1 + groups[lxch].chg1 + max_common
            {
                break;
            } else if !groups[idx].ignore {
                lxch = idx;
                ignored = 0;
            } else {
                ignored += groups[idx].chg2;
            }
            prev = idx;
            idx += 1;
        }

        // Context calculation (non-funccontext path of `xdl_emit_diff`).
        let first = &groups[start];
        let last = &groups[lxch];
        let s1 = first.i1.saturating_sub(context);
        let s2 = first.i2.saturating_sub(context);
        let tail1 = nrec1 - (last.i1 + last.chg1);
        let tail2 = nrec2 - (last.i2 + last.chg2);
        let lctx = context.min(tail1).min(tail2);
        let e1 = last.i1 + last.chg1 + lctx;
        let e2 = last.i2 + last.chg2 + lctx;

        // Header (Libra format: always `-s,c +s,c`, no section heading). A
        // zero-count side anchors at its position rather than position+1.
        let old_count = e1 - s1;
        let new_count = e2 - s2;
        let old_start = if old_count == 0 { s1 } else { s1 + 1 };
        let new_start = if new_count == 0 { s2 } else { s2 + 1 };
        let _ = writeln!(
            out,
            "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
        );

        // Emit body: context, then each group's deletions and insertions in order.
        // Context lines come from the NEW (post-image) side — identical to the old
        // side for a raw diff, and the side Git shows when composing a whitespace
        // normalizer (where the equal-under-normalize lines may differ verbatim).
        let mut pos2 = s2;
        for g in &groups[start..=lxch] {
            for line in &new_recs[pos2..g.i2] {
                let _ = writeln!(out, " {line}");
            }
            for line in &old_recs[g.i1..g.i1 + g.chg1] {
                let _ = writeln!(out, "-{line}");
            }
            for line in &new_recs[g.i2..g.i2 + g.chg2] {
                let _ = writeln!(out, "+{line}");
            }
            pos2 = g.i2 + g.chg2;
        }
        for line in &new_recs[pos2..e2] {
            let _ = writeln!(out, " {line}");
        }

        start = lxch + 1;
    }

    out
}

/// Count added (`+`) and removed (`-`) lines in a unified-diff hunk BODY (no file
/// header). Used to recompute per-file insertion/deletion counts after a `-w`
/// re-diff drops whitespace-only changes. Hunk headers (`@@`) and context lines
/// (leading space) are ignored.
fn count_body_changes(body: &str) -> (usize, usize) {
    let mut insertions = 0;
    let mut deletions = 0;
    for line in body.lines() {
        match line.as_bytes().first() {
            Some(b'+') => insertions += 1,
            Some(b'-') => deletions += 1,
            _ => {}
        }
    }
    (insertions, deletions)
}

/// Assemble a unified-diff hunk body (the `@@ … @@` blocks, no file header) from
/// an ordered edit list of `(tag, line)` pairs at `context` lines of surrounding
/// context — a context-parameterized port of git_internal's private
/// `compute_unified_diff` rolling-context assembler. Shared by the plain `-U<n>`
/// path (lines from a normal line diff) and the whitespace-ignoring `-w` path
/// (the diff is computed on a normalized view but the ORIGINAL line text is
/// emitted). `size_hint` is the combined input length for output preallocation.
fn assemble_unified_hunks(
    changes: &[(ChangeTag, &str)],
    context: usize,
    size_hint: usize,
) -> String {
    let mut out = String::with_capacity((size_hint / 16).max(256));
    // Not `with_capacity(context)`: `context` is caller-supplied (`-U<n>`) and may
    // be arbitrarily large; preallocating it would let `-U99999999999` OOM/panic.
    let mut prefix_ctx: VecDeque<UnifiedEditLine> = VecDeque::new();
    let mut cur_hunk: Vec<UnifiedEditLine> = Vec::new();
    let mut eq_run: Vec<UnifiedEditLine> = Vec::new();
    let mut in_hunk = false;
    let mut last_old_seen = 0usize;
    let mut last_new_seen = 0usize;
    let mut old_line_no = 1usize;
    let mut new_line_no = 1usize;

    for &(tag, line) in changes {
        match tag {
            ChangeTag::Equal => {
                let entry = UnifiedEditLine::Context(Some(old_line_no), Some(new_line_no), line);
                if in_hunk {
                    eq_run.push(entry);
                    // Flush once trailing equal lines exceed 2*context (saturating
                    // so a huge caller-supplied `context` cannot overflow).
                    if eq_run.len() > context.saturating_mul(2) {
                        flush_unified_hunk(
                            &mut out,
                            &mut cur_hunk,
                            &mut eq_run,
                            &mut prefix_ctx,
                            context,
                            &mut last_old_seen,
                            &mut last_new_seen,
                        );
                        in_hunk = false;
                    }
                } else {
                    // Keep only the last `context` equal lines as rolling prefix
                    // context. `push then trim` is correct for any `context`,
                    // including 0 (git_internal's original `len == context` check
                    // only worked for its hard-coded 3 — at 0 it never trimmed).
                    prefix_ctx.push_back(entry);
                    while prefix_ctx.len() > context {
                        prefix_ctx.pop_front();
                    }
                }
                // Record this equal line as the last consumed position on both
                // sides, AFTER any flush above. A flush therefore anchors the
                // just-closed hunk at the pre-line state, while the next zero-count
                // hunk side (a pure insert/delete) anchors just after this line.
                // This is essential at -U0, where the equal line separating two
                // pure hunks is dropped rather than emitted as context — without
                // it the second hunk would fall back to a stale anchor.
                last_old_seen = old_line_no;
                last_new_seen = new_line_no;
                old_line_no += 1;
                new_line_no += 1;
            }
            ChangeTag::Delete => {
                let entry = UnifiedEditLine::Delete(old_line_no, line);
                old_line_no += 1;
                if !in_hunk {
                    cur_hunk.extend(prefix_ctx.iter().copied());
                    prefix_ctx.clear();
                    in_hunk = true;
                }
                if !eq_run.is_empty() {
                    cur_hunk.append(&mut eq_run);
                }
                cur_hunk.push(entry);
            }
            ChangeTag::Insert => {
                let entry = UnifiedEditLine::Insert(new_line_no, line);
                new_line_no += 1;
                if !in_hunk {
                    cur_hunk.extend(prefix_ctx.iter().copied());
                    prefix_ctx.clear();
                    in_hunk = true;
                }
                if !eq_run.is_empty() {
                    cur_hunk.append(&mut eq_run);
                }
                cur_hunk.push(entry);
            }
        }
    }

    if in_hunk {
        flush_unified_hunk(
            &mut out,
            &mut cur_hunk,
            &mut eq_run,
            &mut prefix_ctx,
            context,
            &mut last_old_seen,
            &mut last_new_seen,
        );
    }

    out
}

/// Flush the current hunk to `out`, taking up to `context` trailing equal lines
/// and preserving up to `context` of them as the prefix of the next hunk.
fn flush_unified_hunk<'a>(
    out: &mut String,
    cur_hunk: &mut Vec<UnifiedEditLine<'a>>,
    eq_run: &mut Vec<UnifiedEditLine<'a>>,
    prefix_ctx: &mut VecDeque<UnifiedEditLine<'a>>,
    context: usize,
    last_old_seen: &mut usize,
    last_new_seen: &mut usize,
) {
    let trail_to_take = eq_run.len().min(context);
    for entry in eq_run.iter().take(trail_to_take) {
        cur_hunk.push(*entry);
    }

    let mut old_first: Option<usize> = None;
    let mut old_count: usize = 0;
    let mut new_first: Option<usize> = None;
    let mut new_count: usize = 0;
    for e in cur_hunk.iter() {
        match *e {
            UnifiedEditLine::Context(o, n, _) => {
                if let Some(o) = o {
                    old_first.get_or_insert(o);
                    old_count += 1;
                }
                if let Some(n) = n {
                    new_first.get_or_insert(n);
                    new_count += 1;
                }
            }
            UnifiedEditLine::Delete(o, _) => {
                old_first.get_or_insert(o);
                old_count += 1;
            }
            UnifiedEditLine::Insert(n, _) => {
                new_first.get_or_insert(n);
                new_count += 1;
            }
        }
    }

    if old_count == 0 && new_count == 0 {
        cur_hunk.clear();
        eq_run.clear();
        return;
    }

    // For a zero-count side (pure insert → no old lines, pure delete → no new
    // lines, including whole new/deleted files) anchor at the last consumed line
    // on that side, matching Git: `@@ -k,0 …` after old line k, `… +k,0 @@` after
    // new line k, and `-0,0` / `+0,0` at the start of file. `last_*_seen` is
    // advanced both by emitted hunk lines and by equal lines scanned outside a
    // hunk, so the anchor is correct even at -U0 (where no context enters a hunk).
    let old_start = old_first.unwrap_or(*last_old_seen);
    let new_start = new_first.unwrap_or(*last_new_seen);
    let _ = writeln!(
        out,
        "@@ -{old_start},{old_count} +{new_start},{new_count} @@"
    );

    for &e in cur_hunk.iter() {
        match e {
            UnifiedEditLine::Context(o, n, txt) => {
                let _ = writeln!(out, " {txt}");
                if let Some(o) = o {
                    *last_old_seen = (*last_old_seen).max(o);
                }
                if let Some(n) = n {
                    *last_new_seen = (*last_new_seen).max(n);
                }
            }
            UnifiedEditLine::Delete(o, txt) => {
                let _ = writeln!(out, "-{txt}");
                *last_old_seen = (*last_old_seen).max(o);
            }
            UnifiedEditLine::Insert(n, txt) => {
                let _ = writeln!(out, "+{txt}");
                *last_new_seen = (*last_new_seen).max(n);
            }
        }
    }

    prefix_ctx.clear();
    if context > 0 {
        let keep_start = eq_run.len().saturating_sub(context);
        for entry in eq_run.iter().skip(keep_start) {
            prefix_ctx.push_back(*entry);
        }
    }

    cur_hunk.clear();
    eq_run.clear();
}

/// Render the staged (index-vs-HEAD) changes as an uncolorized unified diff.
/// Used by `commit -v` to embed the diff into the editor template / stderr.
pub(crate) async fn staged_diff_text() -> Result<String, DiffError> {
    let args = DiffArgs {
        old: None,
        new: None,
        staged: true,
        pathspec: Vec::new(),
        algorithm: Some("histogram".to_string()),
        output: None,
        name_only: false,
        name_status: false,
        word_diff: None,
        numstat: false,
        stat: false,
        unified: None,
        ignore_all_space: false,
        ignore_space_change: false,
        ignore_space_at_eol: false,
        ignore_cr_at_eol: false,
        after_dashdash: Vec::new(),
        ignore_blank_lines: false,
        summary: false,
        shortstat: false,
        exit_code: false,
        no_patch: false,
        null: false,
        check: false,
        reverse: false,
        text: false,
        binary: false,
        no_ext_diff: false,
        color_moved: None,
        no_color_moved: false,
        find_renames: None,
        no_renames: false,
        no_relative: false,
        relative: None,
        no_indent_heuristic: false,
        textconv: false,
        no_textconv: false,
        ext_diff: false,
    };
    let result = run_diff(&args, &OutputConfig::default()).await?;
    Ok(format_unified_diff(&result))
}

fn maybe_colorize_diff(diff_text: &str, should_colorize: bool, color_moved: bool) -> String {
    if should_colorize {
        colorize_diff(diff_text, color_moved)
    } else {
        diff_text.to_string()
    }
}

/// Collect the set of moved-line bodies for `--color-moved`: a body that appears
/// as BOTH a removed (`-`) and an added (`+`) line somewhere in the patch is
/// "moved". (Git's `plain` semantics — Libra approximates the block modes with
/// this.) File-header lines (`---`/`+++`) are excluded.
fn moved_line_bodies(diff_text: &str) -> std::collections::HashSet<&str> {
    let mut removed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    let mut added: std::collections::HashSet<&str> = std::collections::HashSet::new();
    // Only `-`/`+` lines INSIDE a hunk are real removals/additions. Tracking hunk
    // state avoids mistaking a body line like `---foo` (a removed `--foo`) for the
    // `--- a/<path>` file header, which precedes the first `@@`.
    let mut in_hunk = false;
    for line in diff_text.lines() {
        if line.starts_with("diff --git") {
            in_hunk = false;
        } else if line.starts_with("@@") {
            in_hunk = true;
        } else if in_hunk {
            if let Some(body) = line.strip_prefix('-') {
                removed.insert(body);
            } else if let Some(body) = line.strip_prefix('+') {
                added.insert(body);
            }
        }
    }
    removed.intersection(&added).copied().collect()
}

/// Render `--shortstat`: just the trailing summary line of `--stat`, omitting
/// the insertion/deletion clause when its count is zero (matching Git, which
/// shows e.g. ` 1 file changed, 2 insertions(+)` with no deletions clause).
fn format_diff_shortstat_output(result: &DiffOutput) -> String {
    if result.files.is_empty() {
        return String::new();
    }
    let mut line = format!(
        " {} file{} changed",
        result.files_changed,
        if result.files_changed == 1 { "" } else { "s" }
    );
    if result.total_insertions > 0 {
        line.push_str(&format!(
            ", {} insertion{}(+)",
            result.total_insertions,
            if result.total_insertions == 1 {
                ""
            } else {
                "s"
            }
        ));
    }
    if result.total_deletions > 0 {
        line.push_str(&format!(
            ", {} deletion{}(-)",
            result.total_deletions,
            if result.total_deletions == 1 { "" } else { "s" }
        ));
    }
    line
}

fn format_diff_stat_output(result: &DiffOutput) -> String {
    if result.files.is_empty() {
        return String::new();
    }

    let mut lines = result
        .files
        .iter()
        .map(|file| {
            let name = if file.status == "renamed" {
                rename_display(file.rename_from.as_deref().unwrap_or(""), &file.path)
            } else {
                file.path.clone()
            };
            // Binary files show `Bin <old> -> <new> bytes` instead of a graph; an
            // UNCHANGED binary (an exact rename, which keeps a header-only body
            // with no `Binary files`/`GIT binary patch`) shows a bare `Bin`,
            // matching Git.
            if let Some((old_size, new_size)) = file.binary {
                let changed = file.raw_diff.contains("Binary files ")
                    || file.raw_diff.contains("GIT binary patch");
                return if changed {
                    format!(" {name} | Bin {old_size} -> {new_size} bytes")
                } else {
                    format!(" {name} | Bin")
                };
            }
            let total = file.insertions + file.deletions;
            let bar = format!(
                "{}{}",
                "+".repeat(file.insertions.min(40)),
                "-".repeat(file.deletions.min(40))
            );
            // Git omits the trailing space when the change graph is empty
            // (e.g. a pure rename with 0 line changes shows `name | 0`).
            if bar.is_empty() {
                format!(" {} | {}", name, total)
            } else {
                format!(" {} | {} {}", name, total, bar)
            }
        })
        .collect::<Vec<_>>();
    lines.push(format!(
        " {} file{} changed, {} insertion{}(+), {} deletion{}(-)",
        result.files_changed,
        if result.files_changed == 1 { "" } else { "s" },
        result.total_insertions,
        if result.total_insertions == 1 {
            ""
        } else {
            "s"
        },
        result.total_deletions,
        if result.total_deletions == 1 { "" } else { "s" }
    ));
    lines.join("\n")
}

fn parse_diff_item(item: &git_internal::diff::DiffItem) -> DiffFileStat {
    let status = parse_diff_status(&item.data);
    let (insertions, deletions) = count_hunk_line_changes(&item.data);

    DiffFileStat {
        path: item.path.clone(),
        status: status.to_string(),
        insertions,
        deletions,
        hunks: parse_diff_hunks(&item.data),
        raw_diff: item.data.clone(),
        rename_from: None,
        similarity: None,
        binary: None,
    }
}

fn parse_diff_status(diff_text: &str) -> &'static str {
    for line in diff_text.lines() {
        if line.starts_with("@@ ") || line == "Binary files differ" {
            break;
        }
        if line.starts_with("new file mode ") || line == "--- /dev/null" {
            return "added";
        }
        if line.starts_with("deleted file mode ") || line == "+++ /dev/null" {
            return "deleted";
        }
    }

    "modified"
}

fn count_hunk_line_changes(diff_text: &str) -> (usize, usize) {
    let mut insertions = 0;
    let mut deletions = 0;
    let mut in_hunk = false;

    for line in diff_text.lines() {
        if line.starts_with("@@ ") {
            in_hunk = true;
            continue;
        }

        if !in_hunk {
            continue;
        }

        if line.starts_with('+') {
            insertions += 1;
        } else if line.starts_with('-') {
            deletions += 1;
        }
    }

    (insertions, deletions)
}

fn parse_diff_hunks(diff_text: &str) -> Vec<DiffHunk> {
    let mut hunks = Vec::new();
    let mut current: Option<DiffHunk> = None;

    for line in diff_text.lines() {
        if let Some(header) = line.strip_prefix("@@ ") {
            if let Some(hunk) = current.take() {
                hunks.push(hunk);
            }
            current =
                parse_hunk_header(header).map(|(old_start, old_lines, new_start, new_lines)| {
                    DiffHunk {
                        old_start,
                        old_lines,
                        new_start,
                        new_lines,
                        lines: Vec::new(),
                    }
                });
            continue;
        }

        if let Some(hunk) = &mut current
            && (line.starts_with('+')
                || line.starts_with('-')
                || line.starts_with(' ')
                || line.starts_with("\\ No newline"))
        {
            hunk.lines.push(line.to_string());
        }
    }

    if let Some(hunk) = current {
        hunks.push(hunk);
    }

    hunks
}

fn parse_hunk_header(header: &str) -> Option<(usize, usize, usize, usize)> {
    let before_suffix = header.split(" @@").next()?;
    let mut parts = before_suffix.split(' ');
    let old = parts.next()?.strip_prefix('-')?;
    let new = parts.next()?.strip_prefix('+')?;
    Some((
        parse_hunk_range(old)?,
        parse_hunk_range_count(old)?,
        parse_hunk_range(new)?,
        parse_hunk_range_count(new)?,
    ))
}

fn parse_hunk_range(value: &str) -> Option<usize> {
    value.split(',').next()?.parse().ok()
}

fn parse_hunk_range_count(value: &str) -> Option<usize> {
    match value.split_once(',') {
        Some((_, count)) => count.parse().ok(),
        None => Some(1),
    }
}

fn colorize_diff(diff_text: &str, color_moved: bool) -> String {
    let mut output = String::with_capacity(diff_text.len() + 500);
    // For `--color-moved`, precompute which line bodies are moved (appear as both
    // a removed and an added line). Moved lines get a distinct color.
    let moved = if color_moved {
        moved_line_bodies(diff_text)
    } else {
        std::collections::HashSet::new()
    };

    // Track hunk state so `-`/`+` are only treated as removals/additions inside a
    // hunk — a body line like `---foo` is a removed `--foo`, not the `--- a/<path>`
    // file header (which precedes the first `@@`).
    let mut in_hunk = false;
    for line in diff_text.lines() {
        let colored_line = if line.starts_with("diff --git") {
            in_hunk = false;
            line.bold().to_string()
        } else if line.starts_with("@@") {
            in_hunk = true;
            line.cyan().to_string()
        } else if in_hunk && line.starts_with('-') {
            // A moved removed line → bold magenta (Git's `oldMoved`); else red.
            if color_moved && moved.contains(&line[1..]) {
                line.magenta().bold().to_string()
            } else {
                line.red().to_string()
            }
        } else if in_hunk && line.starts_with('+') {
            // A moved added line → bold cyan (Git's `newMoved`); else green.
            if color_moved && moved.contains(&line[1..]) {
                line.cyan().bold().to_string()
            } else {
                line.green().to_string()
            }
        } else {
            line.to_string()
        };

        output.push_str(&colored_line);
        output.push('\n');
    }
    output
}

#[cfg(test)]
mod test {
    use std::{fs, io::Write};

    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::test;

    #[test]
    fn parse_rename_score_matches_git_semantics() {
        // Bare integer = digits after an implied `0.` (Git's reading).
        assert_eq!(parse_rename_score("5").unwrap(), 30000); // 0.5 = 50%
        assert_eq!(parse_rename_score("50").unwrap(), 30000); // 0.50 = 50%
        assert_eq!(parse_rename_score("90").unwrap(), 54000); // 0.90 = 90%
        assert_eq!(parse_rename_score("87").unwrap(), 52200); // 0.87 = 87%
        assert_eq!(parse_rename_score("100").unwrap(), 6000); // 0.100 = 10%
        assert_eq!(parse_rename_score("9").unwrap(), 54000); // 0.9 = 90%
        // Explicit percent.
        assert_eq!(parse_rename_score("50%").unwrap(), 30000);
        assert_eq!(parse_rename_score("100%").unwrap(), 60000); // exact-only
        assert_eq!(parse_rename_score("5%").unwrap(), 3000);
        // Explicit decimal fraction.
        assert_eq!(parse_rename_score("0.9").unwrap(), 54000);
        assert_eq!(parse_rename_score("0.5").unwrap(), 30000);
        // Integer truncation (no float rounding), e.g. 33.333% -> 19999.
        assert_eq!(parse_rename_score("33.333%").unwrap(), 19999);
        // Zero parses to 0 here (the 50% fallback is applied in
        // `resolve_rename_threshold`, matching Git's `diffcore_rename`).
        assert_eq!(parse_rename_score("0").unwrap(), 0);
        assert_eq!(parse_rename_score("0%").unwrap(), 0);
        // An empty value parses to 0 (→ the 50% fallback in resolve, matching
        // Git's empty `--find-renames=`).
        assert_eq!(parse_rename_score("").unwrap(), 0);
        // Malformed (non-numeric) values are a usage error, never a silent default.
        assert!(parse_rename_score("abc").is_err());
        assert!(parse_rename_score("9x").is_err());
        // Pathological lengths must not overflow (cap on both num and denom).
        let _ = parse_rename_score(&"9".repeat(64)).unwrap();
        let _ = parse_rename_score(&format!("0.{}", "0".repeat(64))).unwrap();
        let _ = parse_rename_score(&format!("{}%", "9".repeat(64))).unwrap();
    }

    struct ColorOverrideReset;

    impl Drop for ColorOverrideReset {
        fn drop(&mut self) {
            colored::control::unset_override();
        }
    }
    /// Count the `@@` hunk headers in a unified-diff body.
    fn hunk_count(body: &str) -> usize {
        body.lines().filter(|l| l.starts_with("@@")).count()
    }

    #[test]
    fn test_ignore_blank_lines_far_blank_is_suppressed() {
        // `a..h` -> `a,<blank>,b..g,H`. The blank (old~1) and h->H (old-8) are
        // distance 7 apart > 2*ctx(6), so they do NOT merge: the blank-only hunk
        // is suppressed and only the content hunk survives (Git: `@@ -5,4 +6,4 @@`).
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\n\nb\nc\nd\ne\nf\ng\nH\n";
        let body = compute_unified_hunks_ignore_blank(old, new, 3);
        assert_eq!(
            hunk_count(&body),
            1,
            "only the content hunk survives:\n{body}"
        );
        assert!(
            body.contains("@@ -5,4 +6,4 @@"),
            "content hunk header:\n{body}"
        );
        assert!(
            body.contains("-h") && body.contains("+H"),
            "real change shown:\n{body}"
        );
        assert!(
            !body.lines().any(|l| l == "+"),
            "the far blank line is not emitted:\n{body}"
        );
        assert!(
            !body.contains(" a\n") && !body.contains("@@ -1"),
            "the blank's region is gone entirely:\n{body}"
        );
    }

    #[test]
    fn test_ignore_blank_lines_in_window_blank_rides_along() {
        // `a,b,c,d` -> `A,b,<blank>,c,d` with -U2: the blank is within the a->A
        // change's window, so they merge and the blank is shown; the merged hunk
        // extends to d (Git: `@@ -1,4 +1,5 @@`).
        let old = "a\nb\nc\nd\n";
        let new = "A\nb\n\nc\nd\n";
        let body = compute_unified_hunks_ignore_blank(old, new, 2);
        assert_eq!(hunk_count(&body), 1, "single merged hunk:\n{body}");
        assert!(
            body.contains("@@ -1,4 +1,5 @@"),
            "merged hunk header:\n{body}"
        );
        assert!(
            body.contains("-a") && body.contains("+A"),
            "real change shown:\n{body}"
        );
        assert!(
            body.lines().any(|l| l == "+"),
            "the in-window blank IS shown:\n{body}"
        );
        assert!(body.contains(" d"), "context extends to d:\n{body}");
    }

    #[test]
    fn test_ignore_blank_lines_two_changes_bracket_blank() {
        // `a..h` -> `A,b,c,<blank>,d,e,f,g,H`: two real changes (A@1, H@8) merge
        // (distances 2 and 5, both <= 6) into one hunk that shows the blank between
        // them (Git: `@@ -1,8 +1,9 @@`).
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "A\nb\nc\n\nd\ne\nf\ng\nH\n";
        let body = compute_unified_hunks_ignore_blank(old, new, 3);
        assert_eq!(
            hunk_count(&body),
            1,
            "two changes merge to one hunk:\n{body}"
        );
        assert!(
            body.contains("@@ -1,8 +1,9 @@"),
            "merged hunk header:\n{body}"
        );
        assert!(
            body.contains("-a") && body.contains("+A"),
            "first change:\n{body}"
        );
        assert!(
            body.contains("-h") && body.contains("+H"),
            "second change:\n{body}"
        );
        assert!(
            body.lines().any(|l| l == "+"),
            "the bracketed blank is shown:\n{body}"
        );
    }

    #[test]
    fn test_ignore_blank_lines_far_change_no_blank_extension() {
        // `a..f` -> `A,b,c,d,e,<blank>,f`, -U3: the blank (new-6) is far from a->A
        // (old-1) so it is not shown; only the a->A hunk survives, with normal -U3
        // context (Git: `@@ -1,4 +1,4 @@`, no blank).
        let old = "a\nb\nc\nd\ne\nf\n";
        let new = "A\nb\nc\nd\ne\n\nf\n";
        let body = compute_unified_hunks_ignore_blank(old, new, 3);
        assert_eq!(hunk_count(&body), 1, "only the content hunk:\n{body}");
        assert!(
            body.contains("@@ -1,4 +1,4 @@"),
            "content hunk header:\n{body}"
        );
        assert!(
            !body.lines().any(|l| l == "+"),
            "the far blank is not shown:\n{body}"
        );
    }

    #[test]
    fn test_ignore_blank_lines_drops_blank_only_and_keeps_ws() {
        // A change that is only an added blank line -> empty body (file drops out).
        assert!(
            compute_unified_hunks_ignore_blank("x\ny\n", "x\n\ny\n", 3)
                .trim()
                .is_empty(),
            "blank-only change yields no hunks"
        );
        // A whitespace-only added line is NOT blank -> a hunk survives.
        let ws = compute_unified_hunks_ignore_blank("a\nb\n", "a\n  \nb\n", 3);
        assert!(
            !ws.trim().is_empty(),
            "whitespace-only line is not blank: {ws}"
        );
        assert!(
            ws.lines().any(|l| l == "+  "),
            "the whitespace-only line is shown verbatim: {ws}"
        );
    }

    #[test]
    fn test_ignore_blank_lines_crlf_empty_is_not_blank() {
        // A `\r`-only (CRLF) empty line is NOT blank to Git's xdl_blankline without
        // a whitespace flag (size <= 1 means LF-only), so its insertion is shown.
        let body = compute_unified_hunks_ignore_blank("a\nb\n", "a\n\r\nb\n", 3);
        // `split('\n')` (unlike `lines()`) keeps the `\r`, so the emitted `+\r` line
        // is visible verbatim.
        assert!(
            body.split('\n').any(|l| l == "+\r"),
            "a CRLF empty line is shown, not suppressed:\n{body:?}"
        );
    }

    #[test]
    fn test_ignore_blank_lines_composes_with_whitespace_normalizer() {
        // `-w --ignore-blank-lines`: a whitespace-only inserted line normalizes to
        // empty under `-w`, so it counts as blank and is suppressed (matches Git).
        let composed = compute_unified_hunks_ignore_blank_normalized(
            "a\nb\n",
            "a\n  \nb\n",
            3,
            normalize_ignore_all_space,
        );
        assert!(
            composed.trim().is_empty(),
            "-w makes the whitespace-only line blank, so it is suppressed:\n{composed}"
        );
        // Without the normalizer, a whitespace-only line is NOT blank -> shown.
        let plain = compute_unified_hunks_ignore_blank("a\nb\n", "a\n  \nb\n", 3);
        assert!(
            plain.lines().any(|l| l == "+  "),
            "without -w the whitespace-only line is shown:\n{plain}"
        );
    }

    #[test]
    fn test_ignore_blank_lines_multiple_close_blanks_no_real_change() {
        // Two adjacent blank-only inserts with NO real change anywhere: Git's
        // prelude walks past both ignorable groups (the second's next is the end),
        // collapsing the whole run to nothing. Regression for an early-`break`
        // prelude that stopped at the first close pair and emitted the blanks.
        let old = "a\nc\nd\ne\ne\nf\ng\nf\ng\nc\ne\nf\n";
        let new = "a\nc\n\nd\n\ne\ne\nf\ng\nf\ng\nc\ne\nf\n";
        assert!(
            compute_unified_hunks_ignore_blank(old, new, 3)
                .trim()
                .is_empty(),
            "blank-only inserts (even adjacent) with no real change produce no hunks"
        );
    }

    #[test]
    /// Tests command line argument parsing for the diff command with various parameter combinations.
    /// Verifies parameter requirements, conflicts and default values are handled correctly.
    fn test_args() {
        {
            let args = DiffArgs::try_parse_from(["diff", "--old", "old", "--new", "new", "paths"]);
            assert!(args.is_ok());
            let args = args.unwrap();
            assert_eq!(args.old, Some("old".to_string()));
            assert_eq!(args.new, Some("new".to_string()));
            assert_eq!(args.pathspec, vec!["paths".to_string()]);
        }
        {
            // --staged didn't require --old
            let args =
                DiffArgs::try_parse_from(["diff", "--staged", "pathspec", "--output", "output"]);
            let args = args.unwrap();
            assert_eq!(args.old, None);
            assert!(args.staged);
        }
        {
            // --cached is a Git-compatible alias for --staged
            let args = DiffArgs::try_parse_from(["diff", "--cached"]).unwrap();
            assert!(args.staged);
        }
        {
            // --staged conflicts with --new
            let args = DiffArgs::try_parse_from([
                "diff", "--old", "old", "--new", "new", "--staged", "paths",
            ]);
            assert!(args.is_err());
            assert!(args.err().unwrap().kind() == clap::error::ErrorKind::ArgumentConflict);
        }
        {
            // --new requires --old
            let args = DiffArgs::try_parse_from([
                "diff", "--new", "new", "pathspec", "--output", "output",
            ]);
            assert!(args.is_err());
            assert!(args.err().unwrap().kind() == clap::error::ErrorKind::MissingRequiredArgument);
        }
        {
            // --algorithm arg is parsed separately from execution-time support.
            let args = DiffArgs::try_parse_from([
                "diff",
                "--old",
                "old",
                "--new",
                "new",
                "--algorithm",
                "myers",
                "target paths",
            ])
            .unwrap();
            assert_eq!(args.algorithm, Some("myers".to_string()));
        }
        {
            // --algorithm defaults to the only currently supported backend.
            let args = DiffArgs::try_parse_from(["diff", "--old", "old", "target paths"]).unwrap();
            assert_eq!(args.algorithm, Some("histogram".to_string()));
        }
        {
            let args = DiffArgs::try_parse_from([
                "diff",
                "--old",
                "old",
                "--new",
                "new",
                "--algorithm",
                "myers",
                "target paths",
            ])
            .unwrap();
            let err = validate_diff_algorithm(&args).expect_err("myers is not wired yet");
            assert!(matches!(err, DiffError::UnsupportedAlgorithm(value) if value == "myers"));
        }
    }

    #[test]
    #[serial]
    fn test_maybe_colorize_diff_respects_flag() {
        let diff = "diff --git a/file.txt b/file.txt\n--- /dev/null\n+++ b/file.txt\n+line\n";
        let _guard = ColorOverrideReset;
        colored::control::set_override(true);

        let plain = maybe_colorize_diff(diff, false, false);
        let colored = maybe_colorize_diff(diff, true, false);

        assert!(
            !plain.contains("\u{1b}["),
            "plain output should not contain ANSI escapes"
        );
        assert!(
            colored.contains("\u{1b}["),
            "colored output should contain ANSI escapes"
        );
    }

    #[test]
    #[serial]
    fn test_color_moved_uses_distinct_colors() {
        let _guard = ColorOverrideReset;
        colored::control::set_override(true);
        // `keepA` is removed in one place and added in another → moved.
        let diff =
            "diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -1,3 +1,3 @@\n+keepA\n block\n-keepA\n";
        let with_moved = colorize_diff(diff, true);
        let without = colorize_diff(diff, false);
        // Without --color-moved, the moved lines use the normal red/green (31/32).
        assert!(without.contains("\u{1b}[32m") && without.contains("\u{1b}[31m"));
        // With it, the moved added line is bold cyan (1;36) and removed bold
        // magenta (1;35), distinct from plain red/green.
        assert!(
            with_moved.contains("1;36") && with_moved.contains("1;35"),
            "moved lines get bold cyan/magenta: {with_moved:?}"
        );
    }

    #[tokio::test]
    #[serial]
    /// Tests that the get_files_blobs function properly respects .libraignore patterns.
    /// Verifies ignored files are correctly excluded from the blob collection process.
    async fn test_get_files_blob_gitignore() {
        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        let mut gitignore_file = fs::File::create(".libraignore").unwrap();
        gitignore_file.write_all(b"should_ignore").unwrap();

        fs::File::create("should_ignore").unwrap();
        fs::File::create("not_ignore").unwrap();

        let index = Index::load(path::index()).unwrap();
        let blob = get_files_blobs(
            &[PathBuf::from("should_ignore"), PathBuf::from("not_ignore")],
            &index,
            IgnorePolicy::Respect,
        )
        .unwrap();
        assert_eq!(blob.len(), 1);
        assert_eq!(blob[0].0, PathBuf::from("not_ignore"));
    }

    #[tokio::test]
    #[serial]
    async fn test_get_files_blobs_reuses_index_hash_when_stat_matches() {
        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        fs::write("tracked.txt", "worktree content").unwrap();
        let indexed_content = b"indexed content".to_vec();
        let worktree_content = b"worktree content".to_vec();
        let indexed_hash = calculate_object_hash(ObjectType::Blob, &indexed_content);
        let worktree_hash = calculate_object_hash(ObjectType::Blob, &worktree_content);
        assert_ne!(indexed_hash, worktree_hash);

        let mut index = Index::new();
        index.add(
            IndexEntry::new_from_file(Path::new("tracked.txt"), indexed_hash, temp_path.path())
                .unwrap(),
        );

        let blobs = get_files_blobs(
            &[PathBuf::from("tracked.txt")],
            &index,
            IgnorePolicy::Respect,
        )
        .unwrap();

        assert_eq!(blobs, vec![(PathBuf::from("tracked.txt"), indexed_hash)]);
    }
}
