//! Provides diff command logic comparing commits, the index, and the working tree with algorithm selection, pathspec filtering, and optional file output.

pub(crate) mod compare;
pub(crate) mod options;
mod render;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
#[cfg(test)]
use std::sync::Arc;
use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    fmt::Write as _,
    io::{self, IsTerminal},
    ops::Range,
    path::{Path, PathBuf},
    rc::Rc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use clap::Parser;
pub(crate) use compare::*;
use git_internal::{
    hash::{HashKind, ObjectHash, set_hash_kind},
    internal::{
        index::{Index, IndexEntry, Time},
        object::{ObjectTrait, blob::Blob, tree::TreeItemMode, types::ObjectType},
        pack::utils::calculate_object_hash,
    },
};
use serde::Serialize;
use similar::{Algorithm, ChangeTag, TextDiff};
use tempfile::NamedTempFile;

#[cfg(test)]
use self::options::parse_rename_score;
use self::{
    options::{DiffPrefixes, resolve_diff_config},
    render::*,
};
use crate::{
    command::{
        load_object, read_worktree_blob_bytes,
        unmerged::{self, UnmergedEntry},
    },
    internal::config::ConfigKv,
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        ignore::{self, IgnorePolicy},
        output::{ColorChoice, OutputConfig, ProgressMode},
        path,
        pathspec::PathspecError,
        preview_object, util,
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
    libra diff --raw -z                     NUL-safe object/mode records for scripts
    libra diff --diff-filter=AM --name-only Show only added/modified paths
    libra diff -S'old_api' --name-only      Find files changing a string's occurrence count
    libra diff -G'unsafe\\('                 Find files with matching added/removed lines
    libra diff --shortstat                  Show just the files-changed/insertions/deletions line
    libra diff --full-index                 Show full object ids in patch index headers
    libra diff --word-diff                   Word-level diff ([-removed-]{+added+} inline)
    libra diff --word-diff-regex='[A-Za-z]+' Compare custom regex-defined words
    libra diff --color-words                 Word-level diff with colored changes
    libra diff --patience                    Use unique-line anchors for reordered code
    libra diff --anchored='fn '              Keep unique matching function lines as anchors
    libra diff -U0                          Patch with no surrounding context (default is 3)
    libra diff -w                           Ignore whitespace-only changes
    libra diff -b                           Ignore changes in the amount of whitespace
    libra diff --ignore-blank-lines         Ignore changes that are only blank lines
    libra diff -s --exit-code               Status-only check: no output, exit 1 if changes
    libra diff --name-only -z               NUL-terminated changed-file list for scripts
    libra diff --cached --check             Warn about whitespace/conflict-marker errors
    libra diff -R                           Reverse diff (swap additions and deletions)
    libra --json diff --staged              Structured JSON output for agents";

#[derive(Parser, Debug, Default, Clone)]
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

    /// Select the diff algorithm. Myers is the default; `myersMinimal` uses the
    /// same shortest-edit implementation without a deadline, while `patience`
    /// and `histogram` prefer readability-oriented anchors.
    #[clap(
        long,
        value_name = "NAME",
        overrides_with_all = ["patience", "histogram", "anchored"],
    )]
    pub algorithm: Option<String>,

    /// Request the smallest Myers edit script. Libra's Myers
    /// backend already runs without a deadline, so this selects its guaranteed
    /// shortest-edit mode and is output-equivalent to `--algorithm=myersMinimal`.
    #[clap(long)]
    pub minimal: bool,

    /// Generate a diff using the patience algorithm.
    #[clap(
        long,
        overrides_with_all = ["algorithm", "histogram", "anchored"],
    )]
    pub patience: bool,

    /// Generate a diff using the histogram algorithm.
    #[clap(
        long,
        overrides_with_all = ["algorithm", "patience", "anchored"],
    )]
    pub histogram: bool,

    /// Generate an anchored patience diff. May be repeated; a line qualifies
    /// when it is unique on both sides and starts with any supplied text.
    #[clap(
        long,
        value_name = "TEXT",
        action = clap::ArgAction::Append,
        overrides_with_all = ["algorithm", "patience", "histogram"],
    )]
    pub anchored: Vec<String>,

    /// Raw selector order captured by the top-level parser. `clap`'s final
    /// fields cannot represent Git's retained-anchor precedence by themselves.
    #[clap(skip)]
    algorithm_events: Vec<DiffAlgorithmEvent>,

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

    /// Show a color word diff. Equivalent to `--word-diff=color` plus an
    /// optional `--word-diff-regex=<REGEX>`. Unlike the general color mode, it
    /// enables word colors when color selection is automatic even if stdout is
    /// redirected. Use global `--color=never` to suppress ANSI.
    #[clap(long = "color-words", value_name = "REGEX", num_args = 0..=1, require_equals = true)]
    pub color_words: Option<Option<String>>,

    /// Use each non-overlapping REGEX match as a word. Text between matches is
    /// ignored for comparison; new-side delimiters remain visible. Implies
    /// `--word-diff=plain` when no word mode is otherwise selected.
    #[clap(long = "word-diff-regex", value_name = "REGEX")]
    pub word_diff_regex: Option<String>,

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

    /// Show a condensed summary of created/deleted files, detected renames, and
    /// mode changes. Plain content-only edits produce no line.
    #[clap(long)]
    pub summary: bool,

    /// Show raw object/mode/status records for scripts. Use `-z` for arbitrary
    /// path names and unambiguous rename fields.
    #[clap(long)]
    pub raw: bool,

    /// Add creation, deletion, symlink, and executable-mode metadata to the
    /// diffstat. Implies `--stat`.
    #[clap(long = "compact-summary")]
    pub compact_summary: bool,

    /// Select change kinds by Git status letters. Uppercase letters include;
    /// lowercase letters exclude. Supported letters are A,C,D,M,R,T,U,X,B; `*`
    /// retains the whole set when any record matches the other criteria.
    #[clap(long = "diff-filter", value_name = "FILTER")]
    pub diff_filter: Option<String>,

    /// Show only file pairs where the number of non-overlapping occurrences of
    /// STRING changes between the two sides (Git pickaxe `-S`). Matching uses
    /// textconv output when textconv is active and raw bytes otherwise.
    #[clap(short = 'S', value_name = "STRING", conflicts_with = "pickaxe_regex")]
    pub pickaxe_string: Option<String>,

    /// Show only file pairs whose added or removed patch lines match REGEX (Git
    /// pickaxe `-G`). The regex is validated before scanning the working tree.
    #[clap(short = 'G', value_name = "REGEX")]
    pub pickaxe_regex: Option<String>,

    /// Diff two filesystem paths directly, without using the index or a
    /// repository (Git's `--no-index`). Works outside a repository; recurses
    /// directories and exits 1 when anything differs.
    #[clap(long = "no-index")]
    pub no_index: bool,

    /// Show a per-directory change distribution counted by FILE rather than by
    /// changed line (`git diff --dirstat-by-file[=<pct>][,cumulative]`). The
    /// optional value is the same parameter list `--dirstat` accepts.
    #[clap(long = "dirstat-by-file", value_name = "PARAM", num_args = 0..=1, require_equals = true, default_missing_value = "", conflicts_with = "stat")]
    pub dirstat_by_file: Option<String>,

    /// Recognised Git diff options that Libra does not implement. They are
    /// declared so the request is classified (`LBR-UNSUPPORTED-001`) instead of
    /// reported as an unknown option (issues/524).
    #[clap(long = "stat-name-width", value_name = "N", hide = true)]
    pub stat_name_width: Option<usize>,

    #[clap(long = "inter-hunk-context", value_name = "N", hide = true)]
    pub inter_hunk_context: Option<usize>,

    #[clap(
        short = 'I',
        long = "ignore-matching-lines",
        value_name = "REGEX",
        hide = true
    )]
    pub ignore_matching_lines: Option<String>,

    #[clap(short = 'B', long = "break-rewrites", value_name = "N[/M]", num_args = 0..=1, default_missing_value = "", hide = true)]
    pub break_rewrites: Option<String>,

    #[clap(long = "cc", hide = true)]
    pub combined_diff: bool,

    #[clap(short = 'W', long = "function-context", hide = true)]
    pub function_context: bool,

    /// Show a per-directory change distribution (`--dirstat[=<param>,...]`, `-X`).
    /// Parameters: `changes` (default), `lines`, `files`, `noncumulative`,
    /// `cumulative`, and a percentage threshold (default 3%).
    #[clap(short = 'X', long, value_name = "PARAM", num_args = 0..=1, default_missing_value = "", require_equals = true)]
    pub dirstat: Option<String>,

    /// Synonym for `--dirstat=cumulative` (enables `--dirstat` when given alone).
    #[clap(long)]
    pub cumulative: bool,

    /// Output only the last line of `--stat`: the files-changed / insertions /
    /// deletions summary.
    #[clap(long)]
    pub shortstat: bool,

    /// Exit with code 1 when there are differences, 0 when there are none
    /// (the diff is still printed, unlike `--quiet`).
    #[clap(long = "exit-code")]
    pub exit_code: bool,

    /// Plumbing `diff-files`/`diff-index`/`diff-tree --quiet`: print nothing,
    /// including a `--json` envelope. Hidden because porcelain `diff` uses the
    /// global quiet flag instead.
    #[clap(long = "plumbing-quiet", hide = true)]
    pub plumbing_quiet: bool,

    /// Suppress the patch (diff body) output. Combine with `--exit-code` for a
    /// status-only check.
    #[clap(short = 's', long = "no-patch")]
    pub no_patch: bool,

    /// NUL-terminate output records (for `--raw`/`--name-only`/`--name-status`/
    /// `--numstat`); raw renames and name-status fields are emitted separately.
    #[clap(short = 'z', long = "null")]
    pub null: bool,

    /// Warn about safety problems on added lines instead of printing the diff.
    /// Detects trailing whitespace, space-before-tab in the indent, leftover
    /// conflict markers, and new blank lines at EOF; exits 2 when any problem is found.
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
    /// differ". Implies `--full-index`. The patch is valid and appliable, but its
    /// compressed bytes are not byte-identical to Git's (Libra deflates with
    /// `flate2`, and always emits a `literal` chunk rather than Git's
    /// smaller-of-literal/delta choice).
    #[clap(long = "binary")]
    pub binary: bool,

    /// Show full pre-image and post-image object ids on patch `index` lines.
    #[clap(long = "full-index")]
    pub full_index: bool,

    /// Use this source prefix instead of `a/` (or the configured source prefix).
    #[clap(long = "src-prefix", value_name = "PREFIX")]
    pub src_prefix: Option<String>,

    /// Use this destination prefix instead of `b/` (or the configured destination prefix).
    #[clap(long = "dst-prefix", value_name = "PREFIX")]
    pub dst_prefix: Option<String>,

    /// Disable the external diff driver (`diff.external`) for this run, forcing
    /// the built-in diff engine even when one is configured.
    #[clap(long = "no-ext-diff")]
    pub no_ext_diff: bool,

    /// Hidden plumbing mode: `diff-files`/`diff-index` (worktree comparison)
    /// are stat-based like git — a stat-dirty entry is reported modified with
    /// an all-zero worktree sha WITHOUT hashing its content (issues/479
    /// DF-01 / TC-0044 contract).
    #[clap(long = "plumbing-stat-only", hide = true)]
    pub plumbing_stat_only: bool,

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

    /// Turn off rename detection, overriding Git's default, `diff.renames`, and
    /// an earlier `-M`/`--find-renames`.
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
    /// `diff=<driver>` attribute from Git/Libra attribute sources names a driver
    /// with a `diff.<driver>.textconv` command has each side converted by that
    /// command before diffing. Like Git, textconv is ON by default for `diff`;
    /// this flag is the explicit opposite of `--no-textconv`. The resulting
    /// patch is for reading, not applying.
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
    /// First new-side line in a trailing blank run, used only by `diff --check`.
    #[serde(skip)]
    check_trailing_blank_start: Option<usize>,
    /// Directional object/mode metadata used by `--raw`, compact summaries, and
    /// `--diff-filter`. Worktree object ids remain `None`, matching Git raw output.
    #[serde(skip)]
    old_id: Option<ObjectHash>,
    #[serde(skip)]
    new_id: Option<ObjectHash>,
    #[serde(skip)]
    old_mode: Option<u32>,
    #[serde(skip)]
    new_mode: Option<u32>,
}

impl DiffArgs {
    /// Build the diff selection an in-process service caller needs
    /// (plan-20260818 LB-04), with every presentation and extension flag off.
    ///
    /// Lives here because `pathspec` is a private field: a service caller must
    /// go through this constructor rather than assemble a `DiffArgs` literal,
    /// so the two execution-capable knobs can never be left on by accident.
    ///
    /// # Arguments
    ///
    /// * `staged` - Compare the index against HEAD instead of the working tree.
    /// * `old` - The old side, when comparing against a specific commit.
    /// * `pathspec` - Path selectors, already validated by the caller.
    ///
    /// # Returns
    ///
    /// A `DiffArgs` with `--no-ext-diff` and `--no-textconv` forced on, so
    /// repository configuration can never turn the diff into process execution.
    pub(crate) fn for_service(staged: bool, old: Option<String>, pathspec: Vec<String>) -> Self {
        Self {
            staged,
            old,
            pathspec,
            no_ext_diff: true,
            no_textconv: true,
            ..Self::default()
        }
    }
}

impl DiffFileStat {
    /// The raw unified patch body for this file, exactly as `run_diff`
    /// produced it. Exposed for in-process service callers (the agent bridge's
    /// `diff.get`, plan-20260818 LB-04) that render their own bounded
    /// projection instead of the CLI's output.
    pub fn raw_patch(&self) -> &str {
        &self.raw_diff
    }
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

    #[error("failed to read file '{path}': {detail}")]
    FileRead { path: String, detail: String },

    #[error("failed to write output file '{path}': {detail}")]
    OutputWrite { path: String, detail: String },

    #[error("invalid diff algorithm '{0}'; expected myers, myersMinimal, patience, or histogram")]
    InvalidAlgorithm(String),

    #[error("invalid argument to find-renames: '{0}'")]
    InvalidRenameScore(String),

    #[error("bad config value '{value}' for '{key}'")]
    InvalidDiffConfig { key: &'static str, value: String },

    /// A shared configuration read failed (for example an invalid
    /// `core.fileMode`); the message is already user-facing.
    #[error("{0}")]
    InvalidConfig(String),

    #[error("failed to read config '{key}': {detail}")]
    DiffConfigRead { key: &'static str, detail: String },

    #[error("invalid argument to color-moved: '{0}'")]
    InvalidColorMoved(String),

    #[error("invalid argument to diff-filter: '{0}'")]
    InvalidDiffFilter(String),

    #[error("invalid -G regex '{pattern}': {detail}")]
    InvalidPickaxeRegex { pattern: String, detail: String },

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

    #[error("{0}")]
    Pathspec(String),
}

impl From<std::io::Error> for DiffError {
    fn from(error: std::io::Error) -> Self {
        DiffError::ObjectLoad {
            kind: "object",
            object_id: "storage".to_string(),
            detail: error.to_string(),
        }
    }
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
            DiffError::FileRead { .. } => {
                CliError::fatal(message).with_stable_code(StableErrorCode::IoReadFailed)
            }
            DiffError::OutputWrite { .. } => {
                CliError::fatal(message).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            DiffError::InvalidAlgorithm(_) => CliError::command_usage(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("choose --minimal, --patience, --histogram, --anchored=<text>, or a supported --algorithm value"),
            DiffError::InvalidRenameScore(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(
                    "use -M, -M<n> (e.g. -M90%), or --find-renames=<n>; a pathspec after a bare -M must follow '--'",
                ),
            DiffError::InvalidDiffConfig { key, .. } => CliError::command_usage(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint(format!("fix the offending value with 'libra config {key} <value>'")),
            DiffError::InvalidConfig(_) => CliError::command_usage(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments),
            DiffError::DiffConfigRead { .. } => CliError::fatal(message)
                .with_stable_code(StableErrorCode::IoReadFailed),
            DiffError::InvalidColorMoved(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("expected no, default, plain, blocks, zebra, or dimmed-zebra"),
            DiffError::InvalidDiffFilter(_) => CliError::command_usage(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use status letters A,C,D,M,R,T,U,X,B; lowercase excludes, and '*' selects all when any requested kind matches"),
            DiffError::InvalidPickaxeRegex { .. } => CliError::command_usage(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use a valid regular expression after -G, for example: libra diff -G'handler_[0-9]+'"),
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
            DiffError::Pathspec(_) => CliError::fatal(message)
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("use supported pathspec magic: top, exclude, icase, literal, glob"),
        }
    }
}

pub async fn execute(args: DiffArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

pub async fn execute_safe(args: DiffArgs, output: &OutputConfig) -> CliResult<()> {
    // Recognised-but-unimplemented Git options: classify them explicitly
    // (`LBR-UNSUPPORTED-001`) instead of letting clap report an unknown option
    // (issues/524).
    for (flag, detail) in [
        (
            "--stat-name-width",
            "the diffstat name column has a fixed-width layout; per-file name widths are not implemented",
        ),
        (
            "--inter-hunk-context",
            "hunk coalescing distance is fixed at the -U context; --inter-hunk-context is not implemented",
        ),
        (
            "--ignore-matching-lines/-I",
            "hunk suppression by changed-line pattern is not implemented",
        ),
        (
            "--break-rewrites/-B",
            "diffcore-break (rewrite detection) is not implemented",
        ),
        (
            "--cc",
            "combined (merge) diff output is not implemented; use '--raw' or a per-parent diff",
        ),
        (
            "--function-context/-W",
            "function-boundary context expansion is not implemented",
        ),
    ] {
        let requested = match flag {
            "--stat-name-width" => args.stat_name_width.is_some(),
            "--inter-hunk-context" => args.inter_hunk_context.is_some(),
            "--ignore-matching-lines/-I" => args.ignore_matching_lines.is_some(),
            "--cc" => args.combined_diff,
            "--function-context/-W" => args.function_context,
            _ => args.break_rewrites.is_some(),
        };
        if requested {
            return Err(
                CliError::fatal(format!("diff {flag} is not implemented: {detail}"))
                    .with_stable_code(StableErrorCode::Unsupported)
                    .with_hint("drop the option, or run 'git diff' for this comparison"),
            );
        }
    }

    if args.no_index {
        return execute_no_index_safe(&args, output).await;
    }
    if util::require_repo().is_err() {
        return Err(CliError::from(DiffError::NotInRepo));
    }
    crate::command::status::warn_sparse_checkout_unsupported_once().await;
    let mut args = args;
    // `--dirstat-by-file[=<param>]` is `--dirstat=files[,<param>]`.
    if let Some(param) = args.dirstat_by_file.clone() {
        let merged = if param.is_empty() {
            "files".to_string()
        } else {
            format!("files,{param}")
        };
        args.dirstat = Some(merged);
    }
    // `--cumulative` is Git's synonym for `--dirstat=cumulative`.
    if args.cumulative {
        let merged = match args.dirstat.take() {
            Some(param) if !param.is_empty() => format!("{param},cumulative"),
            _ => "cumulative".to_string(),
        };
        args.dirstat = Some(merged);
    }
    resolve_positional_revisions(&mut args)
        .await
        .map_err(CliError::from)?;
    let diff_algorithm = resolve_diff_algorithm(&args).map_err(CliError::from)?;
    parse_diff_filter(args.diff_filter.as_deref()).map_err(CliError::from)?;
    let pickaxe = parse_diff_pickaxe(&args).map_err(CliError::from)?;
    let word_diff = resolve_word_diff_options(&args)?;
    let config = resolve_diff_config(&args).await.map_err(CliError::from)?;
    let mut result = run_diff(&args, output, &config, pickaxe.as_ref(), &diff_algorithm)
        .await
        .map_err(CliError::from)?;
    // lore.md 2.2: read-only sparse view — scope ONLY the working-tree diff
    // (unstaged: new side is the worktree, not `--staged`, not rev-vs-rev), the
    // one that is pure browsing. `--staged` (index-vs-HEAD, commit-authoritative)
    // and `A..B` (rev-vs-rev) are NEVER filtered, so diff never hides what a
    // commit will record. Applied on repo-root-relative paths BEFORE the
    // `--relative` prefix strip.
    if !result.external_diff_applied && !args.staged && args.new.is_none() {
        apply_sparse_view_filter(&mut result).await;
    }
    if !result.external_diff_applied
        && let Some(filter) =
            parse_diff_filter(args.diff_filter.as_deref()).map_err(CliError::from)?
    {
        // Re-apply after the sparse-view projection so `*` all-or-none is based
        // only on the visible result set, not on a hidden out-of-view change.
        apply_diff_filter(&mut result.files, &filter);
        refresh_diff_totals(&mut result);
    }
    // External-driver output is verbatim: skip the internal relative-path rewrite
    // and word-diff transforms (they would mangle the driver's own format).
    if !result.external_diff_applied {
        apply_relative_filter(&args, &mut result);
        apply_word_diff(
            &args,
            &word_diff,
            &mut result,
            output,
            io::stdout().is_terminal(),
        )?;
        apply_diff_prefixes(&mut result, &config.prefixes);
    }
    render_diff_output(&args, &result, output)
}

/// `--no-index`: diff two filesystem paths directly, outside any repository.
///
/// Recurses directories, pairs same-named entries, and exits 1 when anything
/// differs (Git's `--no-index` implies `--exit-code`).
async fn execute_no_index_safe(args: &DiffArgs, output: &OutputConfig) -> CliResult<()> {
    let mut paths = args.pathspec.clone();
    paths.extend(args.after_dashdash.iter().cloned());
    if paths.len() != 2 {
        return Err(CliError::command_usage(format!(
            "--no-index requires exactly two paths (got {})",
            paths.len()
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }
    let diff_algorithm = resolve_diff_algorithm(args).map_err(CliError::from)?;
    let context = args.unified.unwrap_or(3);
    let left = PathBuf::from(&paths[0]);
    let right = PathBuf::from(&paths[1]);

    let mut pairs: BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)> = BTreeMap::new();
    collect_no_index_pairs(Some(&left), Some(&right), Path::new(""), &mut pairs)?;

    let mut result = DiffOutput {
        old_ref: paths[0].clone(),
        new_ref: paths[1].clone(),
        files: Vec::new(),
        total_insertions: 0,
        total_deletions: 0,
        files_changed: 0,
        external_diff_applied: false,
        binary_patch: false,
    };
    for (label, (a, b)) in pairs {
        let a_bytes = a
            .as_deref()
            .map(std::fs::read)
            .transpose()
            .map_err(|error| {
                CliError::fatal(format!(
                    "cannot read '{}': {error}",
                    a.as_deref().unwrap_or(Path::new("")).display()
                ))
            })?;
        let b_bytes = b
            .as_deref()
            .map(std::fs::read)
            .transpose()
            .map_err(|error| {
                CliError::fatal(format!(
                    "cannot read '{}': {error}",
                    b.as_deref().unwrap_or(Path::new("")).display()
                ))
            })?;
        let a_bytes = a_bytes.unwrap_or_default();
        let b_bytes = b_bytes.unwrap_or_default();
        let status = match (&a, &b) {
            (Some(_), None) => "deleted",
            (None, Some(_)) => "added",
            _ => "modified",
        };
        let binary = a_bytes.contains(&0) || b_bytes.contains(&0);
        let (raw, insertions, deletions) = if binary {
            (
                format!(
                    "diff --git a/{label} b/{label}\nBinary files a/{label} and b/{label} differ\n"
                ),
                0,
                0,
            )
        } else {
            let a_text = String::from_utf8_lossy(&a_bytes);
            let b_text = String::from_utf8_lossy(&b_bytes);
            let hunks = compute_unified_hunks(&a_text, &b_text, context, &diff_algorithm);
            let mut raw = format!("diff --git a/{label} b/{label}\n");
            if a.is_none() {
                raw.push_str("new file mode 100644\n");
            }
            if b.is_none() {
                raw.push_str("deleted file mode 100644\n");
            }
            raw.push_str(&format!(
                "--- {}\n+++ {}\n",
                if a.is_some() {
                    format!("a/{label}")
                } else {
                    "/dev/null".to_string()
                },
                if b.is_some() {
                    format!("b/{label}")
                } else {
                    "/dev/null".to_string()
                }
            ));
            raw.push_str(&hunks);
            let (insertions, deletions) = count_body_changes(&hunks);
            (raw, insertions, deletions)
        };
        result.total_insertions += insertions;
        result.total_deletions += deletions;
        result.files_changed += 1;
        result.files.push(DiffFileStat {
            path: label,
            status: status.to_string(),
            insertions,
            deletions,
            hunks: parse_diff_hunks(&raw),
            raw_diff: raw,
            rename_from: None,
            similarity: None,
            binary: None,
            check_trailing_blank_start: None,
            old_id: None,
            new_id: None,
            old_mode: None,
            new_mode: None,
        });
    }

    let mut local_args = args.clone();
    local_args.exit_code = true;
    render_diff_output(&local_args, &result, output)
}

/// Recursively pair entries of two `--no-index` roots. A file paired with a
/// directory maps onto the directory's same-named child; a missing side yields
/// an add/delete pair.
fn collect_no_index_pairs(
    left: Option<&Path>,
    right: Option<&Path>,
    rel: &Path,
    pairs: &mut BTreeMap<String, (Option<PathBuf>, Option<PathBuf>)>,
) -> CliResult<()> {
    let left_meta = left.and_then(|path| std::fs::symlink_metadata(path).ok());
    let right_meta = right.and_then(|path| std::fs::symlink_metadata(path).ok());
    let left_dir = left_meta.as_ref().is_some_and(|meta| meta.is_dir());
    let right_dir = right_meta.as_ref().is_some_and(|meta| meta.is_dir());

    if left_dir || right_dir {
        // A plain file against a directory pairs with the directory child.
        if !left_dir && left_meta.is_some() {
            let name = left.and_then(Path::file_name).unwrap_or_default();
            let label = no_index_label(rel, name);
            let right_child = right.map(|path| path.join(name));
            pairs.insert(label, (left.map(Path::to_path_buf), right_child));
            return Ok(());
        }
        if !right_dir && right_meta.is_some() {
            let name = right.and_then(Path::file_name).unwrap_or_default();
            let label = no_index_label(rel, name);
            let left_child = left.map(|path| path.join(name));
            pairs.insert(label, (left_child, right.map(Path::to_path_buf)));
            return Ok(());
        }
        let mut names: std::collections::BTreeSet<std::ffi::OsString> =
            std::collections::BTreeSet::new();
        for side in [left_dir.then_some(left), right_dir.then_some(right)]
            .into_iter()
            .flatten()
            .flatten()
        {
            let entries = std::fs::read_dir(side).map_err(|error| {
                CliError::fatal(format!("cannot list '{}': {error}", side.display()))
            })?;
            for entry in entries {
                let entry = entry.map_err(|error| {
                    CliError::fatal(format!("cannot list '{}': {error}", side.display()))
                })?;
                names.insert(entry.file_name());
            }
        }
        for name in names {
            let child_rel = rel.join(&name);
            let left_child = left.map(|path| path.join(&name));
            let right_child = right.map(|path| path.join(&name));
            collect_no_index_pairs(
                left_child.as_deref(),
                right_child.as_deref(),
                &child_rel,
                pairs,
            )?;
        }
        return Ok(());
    }

    let name = right.or(left).and_then(Path::file_name);
    let label = no_index_label(rel, name.unwrap_or_default());
    pairs.insert(
        label,
        (left.map(Path::to_path_buf), right.map(Path::to_path_buf)),
    );
    Ok(())
}

fn no_index_label(rel: &Path, name: &std::ffi::OsStr) -> String {
    let joined = if rel.as_os_str().is_empty() {
        PathBuf::from(name)
    } else {
        rel.join(name)
    };
    joined.to_string_lossy().replace('\\', "/")
}

/// Run the diff pipeline for an in-process service caller and return the
/// structured result without rendering anything (plan-20260818 LB-04).
///
/// This is the seam the agent bridge's `diff.get` uses: it performs the same
/// revision resolution, config resolution and diff computation as
/// [`execute_safe`], including the read-only sparse-view projection, but it
/// never writes to stdout/stderr and never applies the CLI-only presentation
/// passes (`--relative` rewriting, word-diff rendering, prefixes). Keeping the
/// bridge on this seam is what makes GC-LB-04 stdout purity provable: the
/// caller owns every byte it emits.
///
/// The caller is responsible for pinning the fields it cares about on `args`
/// (e.g. `staged`, `old`, `pathspec`) and for disabling any flag whose
/// behaviour it does not want; [`DiffArgs::default()`] is the all-off starting
/// point.
///
/// # Arguments
///
/// * `args` - The diff selection. Consumed because revision resolution
///   rewrites the positional/`--old`/`--new` fields in place.
///
/// # Returns
///
/// The computed [`DiffOutput`], or a [`DiffError`] describing the first
/// failure (not in a repository, unresolvable revision, bad pathspec, …).
pub(crate) async fn run_diff_for_service(mut args: DiffArgs) -> Result<DiffOutput, DiffError> {
    if util::require_repo().is_err() {
        return Err(DiffError::NotInRepo);
    }
    resolve_positional_revisions(&mut args).await?;
    let diff_algorithm = resolve_diff_algorithm(&args)?;
    let pickaxe = parse_diff_pickaxe(&args)?;
    let config = resolve_diff_config(&args).await?;
    // A quiet, non-paged, machine-mode config: `run_diff` uses it only for
    // progress/hook I/O decisions, and every one of those must stay silent for
    // a service caller.
    let output = OutputConfig {
        json_format: Some(crate::utils::output::JsonFormat::Compact),
        color: ColorChoice::Never,
        pager: false,
        quiet: true,
        exit_code_on_warning: false,
        progress: ProgressMode::None,
        progress_preference: crate::utils::output::ProgressPreference::None,
    };
    let mut result = run_diff(&args, &output, &config, pickaxe.as_ref(), &diff_algorithm).await?;
    // Same read-only sparse-view scoping as the CLI: only the pure-browsing
    // working-tree diff is filtered, never `--staged` or rev-vs-rev.
    if !result.external_diff_applied && !args.staged && args.new.is_none() {
        apply_sparse_view_filter(&mut result).await;
    }
    Ok(result)
}

/// Whether `--word-diff` is set to a rendering mode (i.e. not `none`/absent), in
/// which case the diff body is already fully rendered and must not be re-colored.
fn word_diff_active(args: &DiffArgs) -> bool {
    if args.color_words.is_some() {
        return true;
    }
    match args.word_diff.as_deref() {
        Some("none") => false,
        Some(_) => true,
        None => args.word_diff_regex.is_some(),
    }
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
    // W1 §C.4.1.1: per-worktree view (read-only display filter).
    let scope = crate::internal::worktree_scope::WorktreeScope::for_request();
    let view = crate::internal::sparse::SparseView::load(&scope).await;
    if !view.is_active() {
        return;
    }
    result.files.retain(|file| {
        // A rename's old path counts as in-view too (either side visible).
        file.raw_diff.starts_with("diff --cc ")
            || view.contains_str(&file.path)
            || file
                .rename_from
                .as_deref()
                .is_some_and(|from| view.contains_str(from))
    });
    refresh_diff_totals(result);
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
    refresh_diff_totals(result);
}

fn refresh_diff_totals(result: &mut DiffOutput) {
    result.files_changed = result.files.len();
    result.total_insertions = result.files.iter().map(|file| file.insertions).sum();
    result.total_deletions = result.files.iter().map(|file| file.deletions).sum();
}

fn apply_diff_prefixes(result: &mut DiffOutput, prefixes: &DiffPrefixes) {
    if prefixes.source == "a/" && prefixes.destination == "b/" {
        return;
    }
    for file in &mut result.files {
        let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
        let replacements = [
            (
                format!("diff --git a/{old_path} b/{}", file.path),
                format!(
                    "diff --git {}{old_path} {}{}",
                    prefixes.source, prefixes.destination, file.path
                ),
            ),
            (
                format!("--- a/{old_path}"),
                format!("--- {}{old_path}", prefixes.source),
            ),
            (
                format!("+++ b/{}", file.path),
                format!("+++ {}{}", prefixes.destination, file.path),
            ),
            (
                format!("Binary files a/{old_path} and b/{} differ", file.path),
                format!(
                    "Binary files {}{old_path} and {}{} differ",
                    prefixes.source, prefixes.destination, file.path
                ),
            ),
            (
                format!("Binary files /dev/null and b/{} differ", file.path),
                format!(
                    "Binary files /dev/null and {}{} differ",
                    prefixes.destination, file.path
                ),
            ),
            (
                format!("Binary files a/{old_path} and /dev/null differ"),
                format!(
                    "Binary files {}{old_path} and /dev/null differ",
                    prefixes.source
                ),
            ),
        ];
        let mut before_hunk = true;
        let mut rewritten = String::with_capacity(file.raw_diff.len());
        for segment in file.raw_diff.split_inclusive('\n') {
            let (line, ending) = split_diff_line_ending(segment);
            if line.starts_with("@@") {
                before_hunk = false;
            }
            if before_hunk {
                rewritten.push_str(&apply_diff_prefixes_to_line(line, &replacements));
                rewritten.push_str(ending);
            } else {
                rewritten.push_str(segment);
            }
        }
        file.raw_diff = rewritten;
    }
}

fn split_diff_line_ending(segment: &str) -> (&str, &str) {
    let Some(without_lf) = segment.strip_suffix('\n') else {
        return (segment, "");
    };
    match without_lf.strip_suffix('\r') {
        Some(line) => (line, "\r\n"),
        None => (without_lf, "\n"),
    }
}

fn apply_diff_prefixes_to_line(line: &str, replacements: &[(String, String); 6]) -> String {
    replacements
        .iter()
        .find_map(|(from, to)| (line == from).then(|| to.clone()))
        .unwrap_or_else(|| line.to_string())
}

/// Word-diff rendering mode (`--word-diff=<MODE>`), excluding `none` (which
/// disables the transform entirely).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum WordDiffMode {
    Plain,
    Color,
    Porcelain,
}

/// Prevalidated word-diff controls. Regex compilation happens before config,
/// progress, textconv, or external-driver work, and the compiled automaton is
/// reused for every file/hunk in the result.
struct ResolvedWordDiff {
    mode: Option<WordDiffMode>,
    regex: Option<regex::Regex>,
    force_auto_color: bool,
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

fn resolve_word_diff_options(args: &DiffArgs) -> CliResult<ResolvedWordDiff> {
    // Preserve the distinction between an absent mode and explicit `none`:
    // --word-diff-regex alone implies plain, while explicit none disables it.
    let explicit_mode = match args.word_diff.as_deref() {
        Some(value) => Some(resolve_word_diff_mode(value)?),
        None => None,
    };
    let mode = if args.color_words.is_some() {
        Some(WordDiffMode::Color)
    } else {
        match explicit_mode {
            Some(mode) => mode,
            None if args.word_diff_regex.is_some() => Some(WordDiffMode::Plain),
            None => None,
        }
    };
    // An explicit --word-diff-regex deterministically overrides the optional
    // regex carried by --color-words. This avoids silently compiling two
    // different tokenizers when both forms are present.
    let regex_source = args
        .word_diff_regex
        .as_deref()
        .or_else(|| args.color_words.as_ref().and_then(|value| value.as_deref()));
    let regex = regex_source
        .map(|pattern| {
            regex::Regex::new(pattern).map_err(|error| {
                CliError::command_usage(format!("invalid --word-diff-regex '{pattern}': {error}"))
                    .with_stable_code(StableErrorCode::CliInvalidArguments)
                    .with_hint("use a valid Rust regular expression for word matching")
            })
        })
        .transpose()?;
    Ok(ResolvedWordDiff {
        mode,
        regex,
        force_auto_color: args.color_words.is_some(),
    })
}

/// Apply `--word-diff`: rewrite each file's unified diff body into word-diff
/// form (the headers/`@@` lines are kept; each hunk's old side vs new side is
/// re-diffed at word granularity). `none`/absent is a no-op.
fn apply_word_diff(
    args: &DiffArgs,
    resolved: &ResolvedWordDiff,
    result: &mut DiffOutput,
    output: &OutputConfig,
    stdout_is_terminal: bool,
) -> CliResult<()> {
    let Some(mode) = resolved.mode else {
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
        || args.compact_summary
        || args.shortstat
        || args.summary
        || args.dirstat.is_some()
        || args.raw
        || args.no_patch
        || output.is_json()
        || (output.quiet && args.output.is_none())
    {
        return Ok(());
    }
    // `plain` must stay bracketed even on a terminal or under `--color=always`.
    // `--word-diff=color` follows the global color policy, while the dedicated
    // `--color-words` shorthand enables color under `auto` even when redirected
    // (matching Git's shorthand); an explicit global `--color=never` still wins.
    let color = mode == WordDiffMode::Color
        && match output.color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => resolved.force_auto_color || stdout_is_terminal,
        };
    for file in &mut result.files {
        file.raw_diff = word_diff_transform(&file.raw_diff, mode, color, resolved.regex.as_ref());
    }
    Ok(())
}

/// Rewrite one file's unified diff text into the chosen word-diff mode. Header
/// lines (`diff --git`, `index`, `---`, `+++`, `@@`) are preserved; each hunk's
/// body is reconstructed into its old side (context + removed lines) and new
/// side (context + added lines), word-diffed, and re-rendered.
fn word_diff_transform(
    raw_diff: &str,
    mode: WordDiffMode,
    color: bool,
    regex: Option<&regex::Regex>,
) -> String {
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
        out.push_str(&render_word_diff(&old_side, &new_side, mode, color, regex));
    }
    out
}

/// Split text into word-diff tokens: a single newline, a run of non-newline
/// whitespace, or a run of non-whitespace (a "word"). Matches Git's default
/// whitespace-delimited tokenization when no custom word regex is selected.
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

#[derive(Clone, Copy)]
struct RegexWordToken<'a> {
    text: &'a str,
    start: usize,
    end: usize,
    line: usize,
    newline: bool,
}

/// Tokenize only regex matches plus explicit newline boundaries. A regex match
/// that crosses a newline contributes only its prefix before the first newline,
/// matching Git's documented truncation rule; the match still consumes its
/// full range, while every consumed newline remains a hard render boundary.
fn regex_word_tokens<'a>(text: &'a str, regex: &regex::Regex) -> Vec<RegexWordToken<'a>> {
    let mut tokens = Vec::new();
    let mut cursor = 0;
    let mut line = 0;
    let push_newlines = |range_start: usize,
                         range: &'a str,
                         tokens: &mut Vec<RegexWordToken<'a>>,
                         line: &mut usize| {
        for (offset, byte) in range.bytes().enumerate() {
            if byte == b'\n' {
                let start = range_start + offset;
                tokens.push(RegexWordToken {
                    text: &text[start..start + 1],
                    start,
                    end: start + 1,
                    line: *line,
                    newline: true,
                });
                *line += 1;
            }
        }
    };
    for matched in regex.find_iter(text) {
        push_newlines(
            cursor,
            &text[cursor..matched.start()],
            &mut tokens,
            &mut line,
        );
        let matched_text = matched.as_str();
        let word_end = matched_text
            .find('\n')
            .map(|offset| matched.start() + offset)
            .unwrap_or(matched.end());
        if word_end > matched.start() {
            tokens.push(RegexWordToken {
                text: &text[matched.start()..word_end],
                start: matched.start(),
                end: word_end,
                line,
                newline: false,
            });
        }
        push_newlines(matched.start(), matched_text, &mut tokens, &mut line);
        cursor = matched.end();
    }
    push_newlines(cursor, &text[cursor..], &mut tokens, &mut line);
    tokens
}

#[derive(Clone, Copy)]
struct IndexedWordChange {
    tag: ChangeTag,
    old_index: Option<usize>,
    new_index: Option<usize>,
}

fn push_owned_change(changes: &mut Vec<(ChangeTag, String)>, tag: ChangeTag, text: &str) {
    if !text.is_empty() {
        changes.push((tag, text.to_string()));
    }
}

/// Build Git-shaped regex-word changes. Regex matches are the comparison keys;
/// unmatched old-side delimiters are ignored, unmatched new-side delimiters are
/// displayed, and a line with no equal word keeps its complete punctuation
/// inside the insertion/deletion marker.
fn regex_word_changes(old: &str, new: &str, regex: &regex::Regex) -> Vec<(ChangeTag, String)> {
    let old_tokens = regex_word_tokens(old, regex);
    let new_tokens = regex_word_tokens(new, regex);
    let old_keys: Vec<&str> = old_tokens.iter().map(|token| token.text).collect();
    let new_keys: Vec<&str> = new_tokens.iter().map(|token| token.text).collect();
    let diff = TextDiff::from_slices(&old_keys, &new_keys);
    let indexed: Vec<IndexedWordChange> = diff
        .iter_all_changes()
        .map(|change| IndexedWordChange {
            tag: change.tag(),
            old_index: change.old_index(),
            new_index: change.new_index(),
        })
        .collect();

    let old_line_count = old.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let new_line_count = new.bytes().filter(|byte| *byte == b'\n').count() + 1;
    let mut old_equal = vec![false; old_line_count];
    let mut new_equal = vec![false; new_line_count];
    let mut old_changed = vec![false; old_line_count];
    let mut new_changed = vec![false; new_line_count];
    let mut old_newline_deleted = vec![false; old_line_count];
    let mut new_newline_inserted = vec![false; new_line_count];
    for change in &indexed {
        if let Some(index) = change.old_index {
            let token = old_tokens[index];
            match change.tag {
                ChangeTag::Equal if !token.newline => old_equal[token.line] = true,
                ChangeTag::Delete if token.newline => old_newline_deleted[token.line] = true,
                ChangeTag::Delete => old_changed[token.line] = true,
                _ => {}
            }
        }
        if let Some(index) = change.new_index {
            let token = new_tokens[index];
            match change.tag {
                ChangeTag::Equal if !token.newline => new_equal[token.line] = true,
                ChangeTag::Insert if token.newline => new_newline_inserted[token.line] = true,
                ChangeTag::Insert => new_changed[token.line] = true,
                _ => {}
            }
        }
    }
    let old_full_change: Vec<bool> = old_changed
        .iter()
        .zip(&old_equal)
        .zip(&old_newline_deleted)
        .map(|((&changed, &equal), &newline_deleted)| newline_deleted || (changed && !equal))
        .collect();
    let new_full_change: Vec<bool> = new_changed
        .iter()
        .zip(&new_equal)
        .zip(&new_newline_inserted)
        .map(|((&changed, &equal), &newline_inserted)| newline_inserted || (changed && !equal))
        .collect();

    // Avoid rescanning the remainder of the change list for every deletion:
    // an all-delete input would otherwise turn delimiter placement into O(n²).
    let mut next_new_indices = vec![None; indexed.len()];
    let mut next_new_index = None;
    for position in (0..indexed.len()).rev() {
        if indexed[position].new_index.is_some() {
            next_new_index = indexed[position].new_index;
        }
        next_new_indices[position] = next_new_index;
    }

    let mut changes = Vec::new();
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    for (position, change) in indexed.iter().enumerate() {
        match change.tag {
            ChangeTag::Equal => {
                // INVARIANT: similar supplies both indices for an Equal change.
                let old_token = old_tokens[change
                    .old_index
                    .expect("INVARIANT: equal word change has an old index")];
                let new_token = new_tokens[change
                    .new_index
                    .expect("INVARIANT: equal word change has a new index")];
                push_owned_change(
                    &mut changes,
                    ChangeTag::Equal,
                    &new[new_cursor..new_token.start],
                );
                push_owned_change(&mut changes, ChangeTag::Equal, new_token.text);
                old_cursor = old_token.end;
                new_cursor = new_token.end;
            }
            ChangeTag::Delete => {
                // INVARIANT: similar supplies an old index for Delete.
                let old_token = old_tokens[change
                    .old_index
                    .expect("INVARIANT: delete word change has an old index")];
                if old_full_change[old_token.line] {
                    push_owned_change(
                        &mut changes,
                        ChangeTag::Delete,
                        &old[old_cursor..old_token.start],
                    );
                } else if let Some(next_new) =
                    next_new_indices[position].map(|index| new_tokens[index])
                    && new_cursor < next_new.start
                {
                    // New-side delimiter belongs before the whole replacement
                    // run, including its leading deletions (`foo,[-bar-]{+baz+}`).
                    push_owned_change(
                        &mut changes,
                        ChangeTag::Equal,
                        &new[new_cursor..next_new.start],
                    );
                    new_cursor = next_new.start;
                }
                push_owned_change(&mut changes, ChangeTag::Delete, old_token.text);
                old_cursor = old_token.end;
            }
            ChangeTag::Insert => {
                // INVARIANT: similar supplies a new index for Insert.
                let new_token = new_tokens[change
                    .new_index
                    .expect("INVARIANT: insert word change has a new index")];
                let gap_tag = if new_full_change[new_token.line] {
                    ChangeTag::Insert
                } else {
                    ChangeTag::Equal
                };
                push_owned_change(&mut changes, gap_tag, &new[new_cursor..new_token.start]);
                push_owned_change(&mut changes, ChangeTag::Insert, new_token.text);
                new_cursor = new_token.end;
            }
        }
    }
    push_owned_change(&mut changes, ChangeTag::Equal, &new[new_cursor..]);
    changes
}

/// Word-diff `old` vs `new` and render the body in the chosen mode (ending with
/// a trailing newline). Newlines always break lines and close any open marker.
/// Render the porcelain word-diff body: consecutive same-tag tokens become one
/// line prefixed by ` ` (context), `-` (removed), or `+` (added); each newline
/// becomes a `~` line.
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
    // `args_os`: see `notes` — `env::args()` panics on a non-UTF-8
    // argument, and only ASCII flags are matched here.
    let argv: Vec<String> = std::env::args_os()
        .filter_map(|arg| arg.into_string().ok())
        .collect();
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

/// Whether `tok` carries pathspec syntax that should bypass the
/// unknown-revision-or-path precheck. Git accepts wildcard pathspecs and magic
/// pathspecs without a literal matching file; let the shared pathspec parser
/// validate magic support and pattern details after revision disambiguation.
fn has_pathspec_syntax(tok: &str) -> bool {
    tok.contains(['*', '?', '['])
        || tok.starts_with(":(")
        || tok.starts_with(":/")
        || tok.starts_with(":!")
        || tok.starts_with(":^")
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
        let resolves = crate::command::get_target_commit(&tok).await.is_ok()
            || crate::utils::util::resolve_tree_ish_with_auto_merge_typed(&tok)
                .await
                .is_ok();
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
        if !is_path && !has_pathspec_syntax(&tok) {
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

#[derive(Debug, Clone, PartialEq, Eq)]
enum DiffAlgorithmEvent {
    Named(String),
    Patience,
    Histogram,
    Anchored(String),
}

/// Preserve selector order that `clap`'s final-state fields cannot express.
/// Git retains anchors across named/Myers/Histogram selectors, clears them only
/// for the `--patience` shorthand, and reactivates the retained set if a later
/// `--anchored` selects patience again.
pub(crate) fn record_algorithm_selector_events(args: &mut DiffArgs, argv: &[String]) {
    args.algorithm_events.clear();
    let Some(diff_idx) = argv.iter().position(|arg| arg == "diff") else {
        return;
    };
    let mut idx = diff_idx + 1;
    while idx < argv.len() {
        let arg = &argv[idx];
        if arg == "--" {
            break;
        }
        if let Some(value) = arg.strip_prefix("--algorithm=") {
            args.algorithm_events
                .push(DiffAlgorithmEvent::Named(value.to_string()));
            idx += 1;
            continue;
        }
        if arg == "--algorithm" {
            if let Some(value) = argv.get(idx + 1) {
                args.algorithm_events
                    .push(DiffAlgorithmEvent::Named(value.clone()));
            }
            idx += 2;
            continue;
        }
        if let Some(value) = arg.strip_prefix("--anchored=") {
            args.algorithm_events
                .push(DiffAlgorithmEvent::Anchored(value.to_string()));
            idx += 1;
            continue;
        }
        if arg == "--anchored" {
            if let Some(value) = argv.get(idx + 1) {
                args.algorithm_events
                    .push(DiffAlgorithmEvent::Anchored(value.clone()));
            }
            idx += 2;
            continue;
        }
        match arg.as_str() {
            "--patience" => args.algorithm_events.push(DiffAlgorithmEvent::Patience),
            "--histogram" => args.algorithm_events.push(DiffAlgorithmEvent::Histogram),
            _ => {}
        }
        idx += 1;
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DiffAlgorithm {
    Myers,
    MyersMinimal,
    Patience,
    Histogram,
    Anchored(Vec<String>),
}

impl DiffAlgorithm {
    fn backend(&self) -> Algorithm {
        match self {
            Self::Myers | Self::MyersMinimal => Algorithm::Myers,
            Self::Patience | Self::Anchored(_) => Algorithm::Patience,
            Self::Histogram => Algorithm::Histogram,
        }
    }

    /// git_internal already renders Myers with the same dependency backend.
    /// Other algorithms must replace that initial body in the post-pass.
    fn needs_backend_rediff(&self) -> bool {
        matches!(self, Self::Patience | Self::Histogram | Self::Anchored(_))
    }
}

fn named_diff_algorithm(value: &str) -> Result<DiffAlgorithm, DiffError> {
    match value {
        "myers" => Ok(DiffAlgorithm::Myers),
        "myersMinimal" => Ok(DiffAlgorithm::MyersMinimal),
        "patience" => Ok(DiffAlgorithm::Patience),
        "histogram" => Ok(DiffAlgorithm::Histogram),
        value => Err(DiffError::InvalidAlgorithm(value.to_string())),
    }
}

fn resolve_diff_algorithm(args: &DiffArgs) -> Result<DiffAlgorithm, DiffError> {
    let selected = if args.algorithm_events.is_empty() {
        if !args.anchored.is_empty() {
            DiffAlgorithm::Anchored(args.anchored.clone())
        } else if args.patience {
            DiffAlgorithm::Patience
        } else if args.histogram {
            DiffAlgorithm::Histogram
        } else {
            match args.algorithm.as_deref() {
                None => DiffAlgorithm::Myers,
                Some(value) => named_diff_algorithm(value)?,
            }
        }
    } else {
        let mut selected = DiffAlgorithm::Myers;
        let mut anchors = Vec::new();
        for event in &args.algorithm_events {
            match event {
                DiffAlgorithmEvent::Named(value) => selected = named_diff_algorithm(value)?,
                DiffAlgorithmEvent::Patience => {
                    anchors.clear();
                    selected = DiffAlgorithm::Patience;
                }
                DiffAlgorithmEvent::Histogram => selected = DiffAlgorithm::Histogram,
                DiffAlgorithmEvent::Anchored(value) => {
                    anchors.push(value.clone());
                    selected = DiffAlgorithm::Patience;
                }
            }
        }
        if selected == DiffAlgorithm::Patience && !anchors.is_empty() {
            DiffAlgorithm::Anchored(anchors)
        } else {
            selected
        }
    };
    Ok(if args.minimal && selected == DiffAlgorithm::Myers {
        DiffAlgorithm::MyersMinimal
    } else {
        selected
    })
}

#[derive(Debug)]
pub(crate) struct DiffFilter {
    include: HashSet<char>,
    exclude: HashSet<char>,
    all_or_none: bool,
}

fn parse_diff_filter(raw: Option<&str>) -> Result<Option<DiffFilter>, DiffError> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    if raw.is_empty() {
        return Err(DiffError::InvalidDiffFilter(raw.to_string()));
    }
    let mut include = HashSet::new();
    let mut exclude = HashSet::new();
    let mut all_or_none = false;
    for value in raw.chars() {
        if value == '*' {
            all_or_none = true;
            continue;
        }
        let normalized = value.to_ascii_uppercase();
        if !matches!(
            normalized,
            'A' | 'C' | 'D' | 'M' | 'R' | 'T' | 'U' | 'X' | 'B'
        ) {
            return Err(DiffError::InvalidDiffFilter(raw.to_string()));
        }
        if value.is_ascii_lowercase() {
            exclude.insert(normalized);
        } else if value.is_ascii_uppercase() {
            include.insert(normalized);
        } else {
            return Err(DiffError::InvalidDiffFilter(raw.to_string()));
        }
    }
    Ok(Some(DiffFilter {
        include,
        exclude,
        all_or_none,
    }))
}

fn apply_diff_filter(files: &mut Vec<DiffFileStat>, filter: &DiffFilter) {
    let matches = |file: &DiffFileStat| {
        let status = diff_status_code(file);
        !filter.exclude.contains(&status)
            && (filter.include.is_empty() || filter.include.contains(&status))
    };
    if filter.all_or_none {
        // Git's `*` is a true all-or-none selector: once any record matches the
        // other criteria, the original set is retained in full (even records
        // named by a lowercase exclusion).
        if !files.iter().any(matches) {
            files.clear();
        }
    } else {
        files.retain(matches);
    }
}

#[derive(Debug)]
pub(crate) enum DiffPickaxe {
    StringCount(Vec<u8>),
    DiffRegex(regex::Regex),
}

fn parse_diff_pickaxe(args: &DiffArgs) -> Result<Option<DiffPickaxe>, DiffError> {
    if let Some(string) = &args.pickaxe_string {
        return Ok(Some(DiffPickaxe::StringCount(string.as_bytes().to_vec())));
    }
    let Some(pattern) = &args.pickaxe_regex else {
        return Ok(None);
    };
    regex::Regex::new(pattern)
        .map(DiffPickaxe::DiffRegex)
        .map(Some)
        .map_err(|error| DiffError::InvalidPickaxeRegex {
            pattern: pattern.clone(),
            detail: error.to_string(),
        })
}

/// Count non-overlapping byte-string occurrences in linear time. Git's `-S`
/// compares occurrence counts, not merely presence, and must also work for
/// binary files whose content is not valid UTF-8.
fn count_literal_occurrences(haystack: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || haystack.len() < needle.len() {
        return 0;
    }

    let mut prefix = vec![0usize; needle.len()];
    let mut matched = 0usize;
    for idx in 1..needle.len() {
        while matched > 0 && needle[idx] != needle[matched] {
            matched = prefix[matched - 1];
        }
        if needle[idx] == needle[matched] {
            matched += 1;
            prefix[idx] = matched;
        }
    }

    let mut count = 0usize;
    matched = 0;
    for &byte in haystack {
        while matched > 0 && byte != needle[matched] {
            matched = prefix[matched - 1];
        }
        if byte == needle[matched] {
            matched += 1;
            if matched == needle.len() {
                count += 1;
                // `str::matches`/Git pickaxe count non-overlapping matches.
                matched = 0;
            }
        }
    }
    count
}

/// Match `-G` only against added/removed hunk content. Header lines (`---`/`+++`)
/// and `\ No newline at end of file` are deliberately excluded. Combined hunks
/// carry one prefix column per parent; any `+`/`-` prefix marks a changed line.
fn changed_diff_line_matches(raw_diff: &str, regex: &regex::Regex) -> bool {
    let mut prefix_columns = 0usize;
    for line in raw_diff.lines() {
        let leading_ats = line.bytes().take_while(|byte| *byte == b'@').count();
        if leading_ats >= 2 && line.as_bytes().get(leading_ats) == Some(&b' ') {
            prefix_columns = leading_ats - 1;
            continue;
        }
        if prefix_columns == 0 || line.len() < prefix_columns {
            continue;
        }
        let prefixes = &line.as_bytes()[..prefix_columns];
        if prefixes
            .iter()
            .all(|byte| matches!(byte, b' ' | b'+' | b'-'))
            && prefixes.iter().any(|byte| matches!(byte, b'+' | b'-'))
            && regex.is_match(&line[prefix_columns..])
        {
            return true;
        }
    }
    false
}

#[allow(clippy::too_many_arguments)]
fn apply_pickaxe(
    files: &mut Vec<DiffFileStat>,
    pickaxe: Option<&DiffPickaxe>,
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &WorktreeContents,
    textconv_counts: &HashMap<String, (usize, usize)>,
) -> Result<(), DiffError> {
    let Some(pickaxe) = pickaxe else {
        return Ok(());
    };
    if let DiffPickaxe::DiffRegex(regex) = pickaxe {
        files.retain(|file| changed_diff_line_matches(&file.raw_diff, regex));
        return Ok(());
    }
    let DiffPickaxe::StringCount(needle) = pickaxe else {
        return Ok(());
    };
    if needle.is_empty() {
        files.clear();
        return Ok(());
    }

    let load = |path: &str, map: &HashMap<PathBuf, ObjectHash>| -> Result<Vec<u8>, DiffError> {
        let path = PathBuf::from(path);
        let Some(hash) = map.get(&path) else {
            return Ok(Vec::new());
        };
        if worktree_entries.get(&path) == Some(hash) {
            worktree_entries.read(&path, hash)
        } else {
            load_repo_blob_content(hash)
        }
    };

    let mut keep = Vec::with_capacity(files.len());
    for file in files.iter() {
        if let Some((old, new)) = textconv_counts.get(&file.path) {
            keep.push(old != new);
            continue;
        }
        let old_path = file.rename_from.as_deref().unwrap_or(&file.path);
        let old_hash = first_map.get(&PathBuf::from(old_path));
        let new_hash = second_map.get(&PathBuf::from(&file.path));
        if old_hash == new_hash {
            keep.push(false);
            continue;
        }
        let old = load(old_path, first_map)?;
        let new = load(&file.path, second_map)?;
        keep.push(
            count_literal_occurrences(&old, needle) != count_literal_occurrences(&new, needle),
        );
    }
    let mut index = 0usize;
    files.retain(|_| {
        let retain = keep[index];
        index += 1;
        retain
    });
    Ok(())
}

/// The scan-progress line is only a liveness cue: `diff` is a fast local read
/// (a normal tree finishes in tens of milliseconds) and git shows no progress
/// for diff at all (#466). Emit nothing for quick scans; surface the line only
/// after the scan has genuinely been running a while (very large working
/// trees, cf. #372), and erase it when the scan completes so it never lingers
/// in scrollback.
const WORKTREE_SCAN_QUIET_PERIOD: Duration = Duration::from_secs(2);

/// A deferred single-line liveness hint for the worktree scan.
///
/// The hint is gated on the progress mode only (auto resolves to Text on a
/// TTY and None when stderr is redirected — the TTY check lives in
/// `OutputConfig::resolve`). The old unconditional startup print also skipped
/// `--quiet`/`--staged`/`--new`; that suppression is restored by the caller
/// constructing the hint only for the unstaged working-tree comparison.
struct WorktreeScanHint {
    enabled: bool,
    done: bool,
    shown: bool,
    /// Test-only sink override: `None` in production writes to stderr.
    #[cfg(test)]
    sink_override: Option<Arc<std::sync::Mutex<Vec<u8>>>>,
    /// Test-only TTY simulation for the sink override.
    #[cfg(test)]
    test_tty: Option<bool>,
}

impl WorktreeScanHint {
    fn start(output: &OutputConfig) -> Self {
        let enabled = match output.progress {
            // `--progress=json` is an explicit machine opt-in served by the
            // immediate `diff_scan.start` event; the text hint must not
            // double-report there.
            ProgressMode::Json => false,
            ProgressMode::Text => true,
            // Auto resolves to Text on a TTY and None otherwise (redirected
            // stderr/CI must never receive the line — #466); None stays off.
            ProgressMode::None => false,
        };
        Self {
            enabled,
            done: false,
            shown: false,
            #[cfg(test)]
            sink_override: None,
            #[cfg(test)]
            test_tty: None,
        }
    }

    /// Reveal the hint mid-scan. On a TTY the line is drawn without a
    /// trailing newline so the cursor stays on it and finish() can erase it;
    /// with stderr redirected (explicit `--progress=text` through a pipe or
    /// file) it is a single plain text line ending in a newline — escape
    /// bytes never enter captured logs.
    fn reveal(&mut self) {
        if self.enabled && !self.done && !self.shown {
            self.write_hint_line("Scanning working tree ...");
            self.shown = true;
        }
    }

    /// Erase the hint line so a completed scan leaves no residue on the
    /// terminal; a no-op when it was never shown or stderr is redirected.
    fn finish(&mut self) {
        self.done = true;
        if self.shown {
            self.write_hint_line("");
        }
        self.shown = false;
    }

    fn write_hint_line(&self, text: &str) {
        #[cfg(test)]
        if let Some(sink) = &self.sink_override {
            let is_terminal = matches!(
                self.test_tty,
                Some(tty) if tty
            );
            write_scan_hint_line_into(
                &mut *sink.lock().expect("test sink lock"),
                text,
                is_terminal,
            );
            return;
        }
        write_scan_hint_line(text, io::stderr().is_terminal());
    }
}

impl Drop for WorktreeScanHint {
    fn drop(&mut self) {
        self.finish();
    }
}

/// Restore the calling thread's previous hash kind on drop (Drop-guard
/// version of git-internal's `set_hash_kind`, which is test-only there).
struct SetHashKindGuard {
    previous: HashKind,
}

impl Drop for SetHashKindGuard {
    fn drop(&mut self) {
        set_hash_kind(self.previous);
    }
}

fn write_scan_hint_line(text: &str, is_terminal: bool) {
    write_scan_hint_line_into(&mut io::stderr(), text, is_terminal);
}

/// Race the blocking-pool scan handle against the quiet period. On a fast
/// scan the completed handle's result is taken from the timeout (the
/// JoinHandle is NEVER awaited twice); on a slow scan the hint is revealed
/// first and the same handle is awaited to completion. Separated from
/// `run_diff` so the slow-scan Err arm is deterministically testable with a
/// short quiet period instead of the production 2-second value.
async fn race_scan_with_hint<T>(
    scan_handle: &mut tokio::task::JoinHandle<T>,
    quiet_period: Duration,
    hint: &mut WorktreeScanHint,
) -> Result<T, tokio::task::JoinError>
where
    T: Send + 'static,
{
    match tokio::time::timeout(quiet_period, &mut *scan_handle).await {
        Ok(result) => result,
        Err(_elapsed) => {
            hint.reveal();
            scan_handle.await
        }
    }
}

fn write_scan_hint_line_into<W: std::io::Write>(sink: &mut W, text: &str, is_terminal: bool) {
    if is_terminal {
        // `\r` + EL (erase line) redraws the single line in place; an empty
        // `text` therefore erases the previously shown hint. No trailing
        // newline: the cursor stays on the line until the next rewrite.
        let _ = write!(sink, "\r\x1b[2K{text}");
    } else if text.is_empty() {
        // Nothing to erase in a redirected stream.
        return;
    } else {
        // Redirected explicit text mode: bare line + newline, no escapes.
        let _ = writeln!(sink, "{text}");
    }
    let _ = sink.flush();
}

fn preflight_preview_object_sizes(
    hashes: Vec<ObjectHash>,
    size_batch: impl FnOnce(&[ObjectHash]) -> Result<Vec<Option<u64>>, git_internal::errors::GitError>,
) -> Result<Vec<(ObjectHash, u64)>, DiffError> {
    preview_object::ensure_object_capacity(hashes.len()).map_err(|error| DiffError::FileRead {
        path: "commit preview object batch".to_string(),
        detail: error.to_string(),
    })?;
    let sizes = size_batch(&hashes).map_err(|error| DiffError::FileRead {
        path: "commit preview object batch".to_string(),
        detail: format!(
            "failed to size commit preview objects before loading them: {error}; rerun without --verbose"
        ),
    })?;
    if sizes.len() != hashes.len() {
        return Err(DiffError::FileRead {
            path: "commit preview object batch".to_string(),
            detail:
                "the storage backend returned an incomplete size batch; rerun without --verbose"
                    .to_string(),
        });
    }
    hashes
        .into_iter()
        .zip(sizes)
        .map(|(hash, size)| {
            size.map(|size| (hash, size)).ok_or_else(|| DiffError::FileRead {
                path: format!("commit preview object {hash}"),
                detail: "the storage backend cannot safely size this object before loading it; rerun without --verbose"
                    .to_string(),
            })
        })
        .collect()
}

fn pathspec_error_to_diff(error: PathspecError) -> DiffError {
    match error {
        PathspecError::OutsideRepository { .. }
        | PathspecError::UnsupportedMagic { .. }
        | PathspecError::InvalidPattern { .. } => DiffError::Pathspec(error.to_string()),
    }
}

fn append_mode_only_changes(
    files: &mut Vec<DiffFileStat>,
    old_blobs: &HashMap<PathBuf, ObjectHash>,
    new_blobs: &HashMap<PathBuf, ObjectHash>,
    old_modes: &HashMap<PathBuf, u32>,
    new_modes: &HashMap<PathBuf, u32>,
) {
    let existing: HashSet<PathBuf> = files.iter().map(|file| PathBuf::from(&file.path)).collect();
    for (path, old_hash) in old_blobs {
        let Some(new_hash) = new_blobs.get(path) else {
            continue;
        };
        let (Some(old_mode), Some(new_mode)) = (old_modes.get(path), new_modes.get(path)) else {
            continue;
        };
        if old_hash != new_hash || old_mode == new_mode || existing.contains(path) {
            continue;
        }
        let display = path.to_string_lossy();
        files.push(DiffFileStat {
            path: display.to_string(),
            status: "modified".to_string(),
            insertions: 0,
            deletions: 0,
            hunks: Vec::new(),
            raw_diff: format!(
                "diff --git a/{display} b/{display}\nold mode {old_mode:06o}\nnew mode {new_mode:06o}\n"
            ),
            rename_from: None,
            similarity: None,
            binary: None,
            check_trailing_blank_start: None,
            old_id: Some(*old_hash),
            new_id: Some(*new_hash),
            old_mode: Some(*old_mode),
            new_mode: Some(*new_mode),
        });
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
}

/// Select the worktree representation explicitly. Raw plumbing, external
/// drivers and textconv retain their inputs; built-in content comparison uses
/// the same clean bytes as add. Recorded objects never pass through this view.
#[derive(Clone, Debug, Default)]
pub(crate) struct WorktreeContents {
    entries: HashMap<PathBuf, ObjectHash>,
    selected_index: HashMap<PathBuf, ObjectHash>,
    clean: bool,
}

impl WorktreeContents {
    fn get(&self, path: &Path) -> Option<&ObjectHash> {
        self.entries.get(path)
    }

    fn read(&self, path: &Path, expected: &ObjectHash) -> Result<Vec<u8>, DiffError> {
        if !self.clean {
            return read_worktree_blob_content(&path.to_path_buf());
        }
        let absolute = util::workdir_to_absolute(path);
        let bytes = crate::utils::worktree_conversion::read_clean_blob_bytes(
            &absolute,
            crate::utils::worktree_conversion::CleanMode::Normal,
            || {
                crate::command::status_io_worker::index_has_text_crlf(
                    self.selected_index.get(path).copied(),
                )
            },
        )
        .map_err(|error| DiffError::FileRead {
            path: absolute.display().to_string(),
            detail: error.to_string(),
        })?;
        // Zero denotes a stat-only plumbing candidate, not a content hash.
        if *expected != ObjectHash::zero_for_kind(git_internal::hash::get_hash_kind())
            && calculate_object_hash(ObjectType::Blob, &bytes) != *expected
        {
            return Err(DiffError::FileRead {
                path: absolute.display().to_string(),
                detail: "worktree content or attributes changed during diff; retry the command"
                    .to_string(),
            });
        }
        Ok(bytes)
    }
}

type WorktreeContentCache = Rc<RefCell<HashMap<(PathBuf, ObjectHash), Vec<u8>>>>;

#[derive(Debug)]
pub(crate) struct DiffSide {
    label: String,
    blobs: Vec<(PathBuf, ObjectHash)>,
    modes: HashMap<PathBuf, u32>,
    worktree_entries: WorktreeContents,
    is_worktree: bool,
}

/// diff needs to print hashes even if the files have not been staged yet.
/// This helper maps workdir paths to blob ids while applying the shared ignore policy.
fn get_files_blobs(
    files: &[PathBuf],
    index: &Index,
    policy: IgnorePolicy,
) -> Result<Vec<(PathBuf, ObjectHash)>, DiffError> {
    // ONE index snapshot timestamp for the whole batch: per-candidate
    // re-stats were O(files) syscalls and could evaluate one loaded Index
    // against different mtimes during a concurrent rewrite (2026-08-06
    // R0-8 review).
    let index_file_mtime = crate::utils::path::try_index()
        .ok()
        .and_then(|index_path| std::fs::metadata(index_path).ok())
        .and_then(|meta| meta.modified().ok());
    let workdir = util::working_dir();
    let conversion = crate::command::status_io_worker::ConversionScan::new(&workdir, index)
        .map_err(|error| DiffError::FileRead {
            path: workdir.display().to_string(),
            detail: error.to_string(),
        })?;
    let (sources_before, policy_valid) =
        conversion
            .probe(None)
            .map_err(|error| DiffError::FileRead {
                path: workdir.display().to_string(),
                detail: error.to_string(),
            })?;
    let blobs = files
        .iter()
        .filter(|path| !ignore::should_ignore(path, policy, index))
        .map(|p| {
            if policy_valid
                && let Some(hash) = index_hash_if_worktree_stat_matches(p, index, index_file_mtime)
            {
                return Ok((p.to_owned(), hash));
            }
            let path = util::workdir_to_absolute(p);
            let hash = conversion
                .hash(&path)
                .map_err(|error| DiffError::FileRead {
                    path: path.display().to_string(),
                    detail: error.to_string(),
                })?;
            Ok((p.to_owned(), hash))
        })
        .collect::<Result<Vec<_>, DiffError>>()?;
    let (sources_after, _) = conversion
        .probe(None)
        .map_err(|error| DiffError::FileRead {
            path: workdir.display().to_string(),
            detail: error.to_string(),
        })?;
    if sources_after != sources_before {
        return Err(DiffError::FileRead {
            path: workdir.display().to_string(),
            detail: "attributes changed during diff; retry the command".to_string(),
        });
    }
    Ok(blobs)
}

/// Stat-based worktree side for the plumbing `diff-files`/`diff-index`
/// (issues/479 DF-01, TC-0044): git's plumbing never hashes the worktree.
/// A stat-dirty entry is reported modified with an all-zero worktree sha;
/// a clean entry reuses the index hash; an intent-to-add placeholder (no
/// trustworthy stat) falls back to hashing its content.
fn get_files_blobs_stat_only(
    files: &[PathBuf],
    index: &Index,
    policy: IgnorePolicy,
) -> Result<Vec<(PathBuf, ObjectHash)>, DiffError> {
    let index_file_mtime = crate::utils::path::try_index()
        .ok()
        .and_then(|index_path| std::fs::metadata(index_path).ok())
        .and_then(|meta| meta.modified().ok());
    let zero = ObjectHash::zero_for_kind(git_internal::hash::get_hash_kind());
    files
        .iter()
        .filter(|path| !ignore::should_ignore(path, policy, index))
        .map(|p| {
            if let Some(hash) = index_hash_if_worktree_stat_matches(p, index, index_file_mtime) {
                return Ok((p.to_owned(), hash));
            }
            if let Some(entry) = p.to_str().and_then(|name| index.get(name, 0))
                && entry.flags.intent_to_add
            {
                let absolute = util::workdir_to_absolute(p);
                let data = crate::utils::worktree_conversion::read_clean_blob_bytes(
                    &absolute,
                    crate::utils::worktree_conversion::CleanMode::Normal,
                    || crate::command::status_io_worker::index_has_text_crlf(Some(entry.hash)),
                )
                .map_err(|e| DiffError::FileRead {
                    path: absolute.display().to_string(),
                    detail: e.to_string(),
                })?;
                return Ok((p.to_owned(), calculate_object_hash(ObjectType::Blob, &data)));
            }
            // Stat differs: report the all-zero worktree sha without hashing.
            Ok((p.to_owned(), zero))
        })
        .collect()
}

fn index_hash_if_worktree_stat_matches(
    path: &Path,
    index: &Index,
    index_file_mtime: Option<std::time::SystemTime>,
) -> Option<ObjectHash> {
    let entry = index.get(path.to_str()?, 0)?;
    let absolute = util::workdir_to_absolute(path);
    let metadata = std::fs::symlink_metadata(&absolute).ok()?;
    index_entry_matches_worktree_stat(entry, &metadata, index_file_mtime).then_some(entry.hash)
}

fn index_entry_matches_worktree_stat(
    entry: &IndexEntry,
    metadata: &std::fs::Metadata,
    index_file_mtime: Option<std::time::SystemTime>,
) -> bool {
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
        // Racily-clean guard (Git parity): the shortcut is only earned by
        // files strictly OLDER than the index snapshot — an entry written
        // in the same instant the file changed can pair a post-edit stat
        // with a pre-edit hash (2026-08-06 R0-8 review; same guard as
        // status's index_stat_differs).
        && index_file_mtime.is_some_and(|snapshot| index_mtime(metadata) < snapshot)
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

/// Returns (path, hash) pairs from the index's stored entries (stage 0).
/// Unlike `get_files_blobs`, this uses the hash already recorded in the index
/// rather than reading the current file on disk, which is essential for
/// producing a correct working-directory diff (index vs working tree).
/// The unstaged working-tree side: pure synchronous index/worktree reads
/// (no `.await`), so it must run on the blocking pool to keep the async
/// runtime responsive while a large tree is scanned (#372) and to let the
/// `WorktreeScanHint` timer actually win the race on slow scans (#466).
pub(crate) type DiffTreeEntries = (Vec<(PathBuf, ObjectHash)>, HashMap<PathBuf, u32>);

fn tree_mode_to_index_mode(mode: TreeItemMode) -> u32 {
    match mode {
        TreeItemMode::Blob => 0o100644,
        TreeItemMode::BlobExecutable => 0o100755,
        TreeItemMode::Link => 0o120000,
        TreeItemMode::Commit => 0o160000,
        TreeItemMode::Tree => 0o040000,
    }
}

/// Render a Git-style `--stat` block for the changes between two commits'
/// trees, reusing the same diff engine and `--stat` formatter as `libra diff
/// --stat`. Used by `libra merge --stat` to show what a merge changed. Returns
/// an empty string when the two trees are identical.
fn load_repo_blob_content(hash: &ObjectHash) -> Result<Vec<u8>, DiffError> {
    if let Some(content) = preview_object::read(hash).map_err(|error| DiffError::FileRead {
        path: format!("temporary preview object {hash}"),
        detail: error.to_string(),
    })? {
        return Ok(content);
    }
    if preview_object::is_active() {
        let storage = ClientStorage::try_init(path::objects())?;
        let data = storage
            .get_with_limit(hash, preview_object::MAX_OBJECT_BYTES)
            .map_err(|error| DiffError::ObjectLoad {
                kind: "blob",
                object_id: hash.to_string(),
                detail: format!("bounded commit preview read failed: {error}"),
            })?;
        return Blob::from_bytes(&data, *hash)
            .map(|blob| blob.data)
            .map_err(|error| DiffError::ObjectLoad {
                kind: "blob",
                object_id: hash.to_string(),
                detail: error.to_string(),
            });
    }
    let blob = load_object::<Blob>(hash).map_err(|e| DiffError::ObjectLoad {
        kind: "blob",
        object_id: hash.to_string(),
        detail: e.to_string(),
    })?;
    Ok(blob.data)
}

fn read_worktree_blob_content(path_buf: &PathBuf) -> Result<Vec<u8>, DiffError> {
    let absolute = util::workdir_to_absolute(path_buf);
    read_worktree_blob_bytes(&absolute).map_err(|e| DiffError::FileRead {
        path: absolute.display().to_string(),
        detail: e.to_string(),
    })
}

fn apply_unmerged_worktree_diff(
    files: &mut Vec<DiffFileStat>,
    index: &Index,
    pathspecs: &[PathBuf],
) -> Result<(), DiffError> {
    let entries = unmerged::collect(index)
        .into_iter()
        .filter(|entry| unmerged::path_matches(&entry.path, pathspecs))
        .collect::<Vec<_>>();
    if entries.is_empty() {
        return Ok(());
    }

    let unmerged_paths = entries
        .iter()
        .map(|entry| entry.path.to_string_lossy().into_owned())
        .collect::<HashSet<_>>();
    files.retain(|file| !unmerged_paths.contains(&file.path));
    for entry in entries {
        files.push(build_unmerged_diff_file(&entry)?);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(())
}

fn build_unmerged_diff_file(entry: &UnmergedEntry) -> Result<DiffFileStat, DiffError> {
    let path = entry.path.to_string_lossy().into_owned();
    let raw_diff = render_unmerged_combined_diff(entry)?;
    Ok(DiffFileStat {
        path,
        status: "modified".to_string(),
        insertions: 0,
        deletions: 0,
        hunks: Vec::new(),
        raw_diff,
        rename_from: None,
        similarity: None,
        binary: None,
        check_trailing_blank_start: None,
        old_id: None,
        new_id: None,
        old_mode: None,
        new_mode: None,
    })
}

fn render_unmerged_combined_diff(entry: &UnmergedEntry) -> Result<String, DiffError> {
    let path = entry.path.to_string_lossy();
    let ours = entry.stage(2);
    let theirs = entry.stage(3);
    let ours_text = stage_text(ours.as_ref())?;
    let theirs_text = stage_text(theirs.as_ref())?;
    let worktree_text = read_optional_worktree_text(&entry.path)?;
    let ours_lines = line_count(&ours_text);
    let theirs_lines = line_count(&theirs_text);
    let worktree_lines = line_count(&worktree_text);
    let ours_hash = abbreviated_stage_hash(ours.as_ref());
    let theirs_hash = abbreviated_stage_hash(theirs.as_ref());

    let mut rendered = format!(
        "diff --cc {path}\nindex {ours_hash},{theirs_hash}..0000000\n--- a/{path}\n+++ b/{path}\n@@@ -1,{ours_lines} -1,{theirs_lines} +1,{worktree_lines} @@@\n"
    );
    for (prefix, line) in combined_worktree_lines(&worktree_text, &ours_text, &theirs_text) {
        rendered.push_str(prefix);
        rendered.push_str(line);
        rendered.push('\n');
    }
    Ok(rendered)
}

fn combined_worktree_lines<'a>(
    worktree: &'a str,
    ours: &'a str,
    theirs: &'a str,
) -> Vec<(&'static str, &'a str)> {
    let ours_lines = ours.lines().collect::<Vec<_>>();
    let theirs_lines = theirs.lines().collect::<Vec<_>>();
    let mut ours_pos = 0;
    let mut theirs_pos = 0;

    worktree
        .lines()
        .map(|line| {
            let matches_ours = ours_lines.get(ours_pos).is_some_and(|ours| *ours == line);
            let matches_theirs = theirs_lines
                .get(theirs_pos)
                .is_some_and(|theirs| *theirs == line);
            match (matches_ours, matches_theirs) {
                (true, true) => {
                    ours_pos += 1;
                    theirs_pos += 1;
                    ("  ", line)
                }
                (true, false) => {
                    ours_pos += 1;
                    (" +", line)
                }
                (false, true) => {
                    theirs_pos += 1;
                    ("+ ", line)
                }
                (false, false) => ("++", line),
            }
        })
        .collect()
}

fn stage_text(
    stage: Option<&crate::command::unmerged::UnmergedStage>,
) -> Result<String, DiffError> {
    match stage {
        Some(stage) => {
            Ok(String::from_utf8_lossy(&load_repo_blob_content(&stage.hash)?).into_owned())
        }
        None => Ok(String::new()),
    }
}

fn read_optional_worktree_text(path: &PathBuf) -> Result<String, DiffError> {
    let absolute = util::workdir_to_absolute(path);
    match read_worktree_blob_bytes(&absolute) {
        Ok(data) => Ok(String::from_utf8_lossy(&data).into_owned()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(String::new()),
        Err(error) => Err(DiffError::FileRead {
            path: absolute.display().to_string(),
            detail: error.to_string(),
        }),
    }
}

fn abbreviated_stage_hash(stage: Option<&crate::command::unmerged::UnmergedStage>) -> String {
    stage
        .map(|stage| {
            let full = stage.hash.to_string();
            full.get(..7).unwrap_or(&full).to_string()
        })
        .unwrap_or_else(|| "0000000".to_string())
}

fn line_count(text: &str) -> usize {
    text.lines().count().max(1)
}

/// Whether the textual patch body is shown for this invocation. The
/// `--stat`/`--compact-summary`/`--numstat`/`--shortstat`/`--raw`/name/summary/
/// `-s`/`--check` modes render from the internal diff and bypass external
/// drivers (matching Git, which never runs `diff.external` for those modes).
fn patch_body_is_shown(args: &DiffArgs) -> bool {
    !(args.stat
        || args.compact_summary
        || args.numstat
        || args.shortstat
        || args.name_only
        || args.name_status
        || args.summary
        || args.dirstat.is_some()
        || args.raw
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

// Rename similarity scoring (spanhash + Git's 0..60000 score) lives in the
// shared engine module (plan-20260714 §B.4.2) so `status` and `diff` cannot
// drift apart.
use super::rename_detect;

/// Detect renames among the deleted + added files and fold each matched pair into
/// a single rename entry (`-M`). Exact (same blob id) pairs are matched first,
/// then the best inexact pairs whose similarity meets the threshold. Candidates
/// must retain their file type (regular file, symlink, and so on), and each side
/// is used at most once.
#[allow(clippy::too_many_arguments)]
/// Which inexact-rename degradations fired during [`apply_rename_detection`];
/// exact renames are always still detected.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct RenameDetectionSkips {
    /// A side exceeded `diff.renameLimit` (per-side OR, Git semantics).
    inexact_skipped_by_limit: bool,
    /// `diff.renameComparisonBudget` was exhausted; every scored inexact
    /// edge was discarded (§B.4.2.5 触顶规则).
    inexact_discarded_by_budget: bool,
}

pub(super) const DEFAULT_RENAME_LIMIT: usize = 1000;

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
    diff_algorithm: &DiffAlgorithm,
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
                    &old_text,
                    &new_text,
                    context,
                    diff_algorithm,
                    normalize,
                ),
                None => compute_unified_hunks_ignore_blank(
                    &old_text,
                    &new_text,
                    context,
                    diff_algorithm,
                ),
            }
        } else if let Some(normalize) = ws_normalize {
            compute_unified_hunks_normalized(
                &old_text,
                &new_text,
                context,
                diff_algorithm,
                normalize,
            )
        } else {
            compute_unified_hunks(&old_text, &new_text, context, diff_algorithm)
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
        check_trailing_blank_start: None,
        old_id: None,
        new_id: None,
        old_mode: None,
        new_mode: None,
    }
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
/// content is unchanged is dropped (like a whitespace-only change), unless its
/// mode changed; created/deleted/renamed/mode-changed files keep their header.
/// Returns the set of textconv'd paths so the later context/whitespace post-pass
/// skips them. When `pickaxe_needle` is present, the old/new occurrence counts
/// are computed while the converted strings already exist and retained without
/// keeping either potentially-large string alive or executing a command again.
/// Blob read failures surface as errors (not empty content).
#[derive(Default)]
struct TextconvOutcome {
    paths: HashSet<String>,
    pickaxe_counts: HashMap<String, (usize, usize)>,
}

#[allow(clippy::too_many_arguments)]
fn apply_textconv(
    files: &mut Vec<DiffFileStat>,
    path_commands: &HashMap<String, (Option<String>, Option<String>)>,
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &WorktreeContents,
    context: usize,
    ws_normalize: Option<fn(&str) -> String>,
    ignore_blank: bool,
    diff_algorithm: &DiffAlgorithm,
    pickaxe_needle: Option<&[u8]>,
) -> Result<TextconvOutcome, DiffError> {
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
                Some(n) => compute_unified_hunks_ignore_blank_normalized(
                    old,
                    new,
                    context,
                    diff_algorithm,
                    n,
                ),
                None => compute_unified_hunks_ignore_blank(old, new, context, diff_algorithm),
            }
        } else if let Some(n) = ws_normalize {
            compute_unified_hunks_normalized(old, new, context, diff_algorithm, n)
        } else {
            compute_unified_hunks(old, new, context, diff_algorithm)
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

    let pickaxe_counts = pickaxe_needle
        .map(|needle| {
            converted
                .iter()
                .map(|(path, (old, new))| {
                    (
                        path.clone(),
                        (
                            count_literal_occurrences(old.as_bytes(), needle),
                            count_literal_occurrences(new.as_bytes(), needle),
                        ),
                    )
                })
                .collect()
        })
        .unwrap_or_default();

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
            let mode_changed = matches!(
                (file.old_mode, file.new_mode),
                (Some(old), Some(new)) if old != new
            );
            let keep_header =
                matches!(file.status.as_str(), "added" | "deleted" | "renamed") || mode_changed;
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
            let index_mode = match (file.old_mode, file.new_mode) {
                (Some(old), Some(new)) if old == new => format!(" {new:06o}"),
                _ => String::new(),
            };
            raw.push_str(&format!(
                "index {}..{}{index_mode}\n--- a/{old_path}\n+++ b/{}\n",
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
    Ok(TextconvOutcome {
        paths: done,
        pickaxe_counts,
    })
}

/// zlib-deflate `data` (for `--binary` literal chunks). Uses `flate2` at the
/// default level; the bytes are valid zlib but NOT byte-identical to Git's own
/// `zlib` output (a documented divergence).
pub(crate) fn zlib_deflate(data: &[u8]) -> Vec<u8> {
    use std::io::Write as _;
    let mut encoder = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
    let _ = encoder.write_all(data);
    encoder.finish().unwrap_or_default()
}

/// Encode `data` with Git's base85 (the `binary-patch` line format): each line
/// carries up to 52 bytes, prefixed by a length char (`A`-`Z` for 1-26 bytes,
/// `a`-`z` for 27-52), then 5 base85 digits per 4 bytes (zero-padded), big-endian.
pub(crate) fn git_base85(data: &[u8]) -> String {
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
    worktree_entries: &WorktreeContents,
    context: usize,
    diff_algorithm: &DiffAlgorithm,
) -> Result<(), DiffError> {
    let load = |path: &str, map: &HashMap<PathBuf, ObjectHash>| -> Result<Vec<u8>, DiffError> {
        let pb = PathBuf::from(path);
        let Some(hash) = map.get(&pb) else {
            return Ok(Vec::new());
        };
        if worktree_entries.get(&pb) == Some(hash) {
            worktree_entries.read(&pb, hash)
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
        let hunks = compute_unified_hunks(&old_text, &new_text, context, diff_algorithm);
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
    worktree_entries: &WorktreeContents,
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
            worktree_entries.read(&pb, hash)
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
/// uses `/dev/null` with `.` for its hex and mode; whichever directional side is
/// the live working tree reports an all-zero hash (including under `-R`), matching Git. The
/// command is run through the shell so a `diff.external` value carrying its own
/// arguments works.
fn apply_external_diff(
    files: &mut [DiffFileStat],
    command: &str,
    first_map: &HashMap<PathBuf, ObjectHash>,
    second_map: &HashMap<PathBuf, ObjectHash>,
    first_is_worktree: bool,
    second_is_worktree: bool,
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
        let (fallback_old_mode, fallback_new_mode) = external_diff_modes(&file.raw_diff);
        let old_mode = file
            .old_mode
            .map(|mode| format!("{mode:06o}"))
            .unwrap_or(fallback_old_mode);
        let new_mode = file
            .new_mode
            .map(|mode| format!("{mode:06o}"))
            .unwrap_or(fallback_new_mode);

        let (old_file, old_hex, old_mode_arg, _old_tmp) = materialize(
            first_map.get(&old_path),
            first_is_worktree,
            &old_path,
            &old_mode,
        )?;
        let (new_file, new_hex, new_mode_arg, _new_tmp) =
            materialize(second_map.get(&path), second_is_worktree, &path, &new_mode)?;

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
/// after the leading `+`). Returns `None` for a clean line. Checks Git's
/// blank-at-eol and space-before-tab defaults.
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

fn is_leftover_conflict_marker(content: &str) -> bool {
    content.starts_with("<<<<<<<")
        || content.starts_with("|||||||")
        || content.starts_with("=======")
        || content.starts_with(">>>>>>>")
}

fn first_trailing_blank_line(text: &str) -> Option<usize> {
    let mut first_blank = None;
    for (index, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            if first_blank.is_none() {
                first_blank = Some(index + 1);
            }
        } else {
            first_blank = None;
        }
    }
    first_blank
}

fn annotate_diff_check_trailing_blanks(
    files: &mut [DiffFileStat],
    second_map: &HashMap<PathBuf, ObjectHash>,
    worktree_entries: &WorktreeContents,
    worktree_cache: &WorktreeContentCache,
    repo_cache: &Rc<RefCell<HashMap<ObjectHash, Vec<u8>>>>,
) -> Result<(), DiffError> {
    for file in files {
        let path = PathBuf::from(&file.path);
        let Some(hash) = second_map.get(&path) else {
            file.check_trailing_blank_start = None;
            continue;
        };
        let bytes = worktree_cache
            .borrow()
            .get(&(path.clone(), *hash))
            .cloned()
            .or_else(|| repo_cache.borrow().get(hash).cloned())
            .map(Ok)
            .unwrap_or_else(|| {
                if worktree_entries.get(&path) == Some(hash) {
                    worktree_entries.read(&path, hash)
                } else {
                    load_repo_blob_content(hash)
                }
            })?;
        file.check_trailing_blank_start =
            first_trailing_blank_line(&String::from_utf8_lossy(&bytes));
    }
    Ok(())
}

/// Scan one file's unified diff for `--check` problems on added (`+`) lines,
/// tracking new-file line numbers from each hunk header. Returns one
/// `path:line: message` string per problem.
fn check_whitespace_in_file(
    path: &str,
    raw_diff: &str,
    trailing_blank_start: Option<usize>,
) -> Vec<String> {
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
            if is_leftover_conflict_marker(content) {
                problems.push(format!("{path}:{new_lineno}: leftover conflict marker"));
            }
            if trailing_blank_start.is_some_and(|start| new_lineno >= start)
                && content.trim().is_empty()
            {
                problems.push(format!("{path}:{new_lineno}: new blank line at EOF."));
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

/// `diff --check`: print whitespace warnings, reporting whether any were found.
///
/// The caller turns that into a status code, because `--check` is not the only
/// contributor: Git *adds* the bits, so `--check --exit-code` over a change with
/// whitespace damage is `1 + 2 = 3`, not `2`.
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

/// Render `--summary`: one line per created/deleted file, detected rename, or
/// mode change; plain content-only modifications produce no line.
fn abbreviated_raw_id(id: Option<ObjectHash>) -> String {
    id.map(|hash| {
        let full = hash.to_string();
        full.get(..7).unwrap_or(&full).to_string()
    })
    .unwrap_or_else(|| "0000000".to_string())
}

fn raw_mode(mode: Option<u32>) -> String {
    mode.map(|mode| format!("{mode:06o}"))
        .unwrap_or_else(|| "000000".to_string())
}

/// Render a rename path pair the way Git's `pprint_rename` does for `--stat` /
/// `--numstat` / `--summary`: factor out the common leading directory and the
/// common trailing component (both cut at `/` boundaries) into
/// `prefix{old => new}suffix`, or `old => new` when nothing is shared.
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
    diff_algorithm: &DiffAlgorithm,
) -> String {
    splice_unified_body(
        raw_diff,
        &compute_unified_hunks(old_text, new_text, context, diff_algorithm),
    )
}

/// Re-render a commit patch's text hunks with the histogram backend while
/// preserving the object/mode/path headers produced by `git_internal`.
///
/// `format-patch --histogram` uses this focused seam instead of constructing a
/// full working-tree [`DiffArgs`] value. Binary and header-only diffs have no
/// hunk body, so [`splice_unified_body`] leaves them unchanged.
pub(crate) fn rewrite_unified_diff_histogram(
    raw_diff: &str,
    old_text: &str,
    new_text: &str,
) -> String {
    splice_unified_body(
        raw_diff,
        &compute_unified_hunks(old_text, new_text, 3, &DiffAlgorithm::Histogram),
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

#[derive(Debug, Clone, Copy)]
enum IndexedLineChange {
    Equal { new_index: usize },
    Delete { old_index: usize },
    Insert { new_index: usize },
}

impl IndexedLineChange {
    fn tag(self) -> ChangeTag {
        match self {
            Self::Equal { .. } => ChangeTag::Equal,
            Self::Delete { .. } => ChangeTag::Delete,
            Self::Insert { .. } => ChangeTag::Insert,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PatienceCandidate {
    old_index: usize,
    new_index: usize,
    previous: Option<usize>,
}

fn unique_line_positions<'a>(
    lines: &[&'a str],
    range: Range<usize>,
) -> HashMap<&'a str, Option<usize>> {
    let mut positions = HashMap::with_capacity(range.len());
    for index in range {
        positions
            .entry(lines[index])
            .and_modify(|position| *position = None)
            .or_insert(Some(index));
    }
    positions
}

/// Split Git-style raw records while preserving each line terminator. Anchored
/// comparison and prefix matching use these records; emission strips the
/// terminator to match the existing unified-hunk assembler.
fn raw_anchor_records(text: &str) -> Vec<&str> {
    if text.is_empty() {
        Vec::new()
    } else {
        text.split_inclusive('\n').collect()
    }
}

/// Git's anchored mode is patience diff with an anchor-aware LIS. All lines
/// unique on both sides remain candidates; an anchored candidate locks its LIS
/// position so a later candidate cannot displace it. This is the key behavior
/// that prevents a qualifying line from surfacing as delete+insert.
fn anchored_patience_sequence(
    old: &[&str],
    old_range: Range<usize>,
    new: &[&str],
    new_range: Range<usize>,
    anchor_lines: &[&str],
    anchors: &[String],
) -> Vec<(usize, usize)> {
    let old_unique = unique_line_positions(old, old_range.clone());
    let new_unique = unique_line_positions(new, new_range);
    let mut candidates = Vec::new();
    for old_index in old_range {
        let line = old[old_index];
        if old_unique.get(line).copied().flatten() != Some(old_index) {
            continue;
        }
        let Some(new_index) = new_unique.get(line).copied().flatten() else {
            continue;
        };
        candidates.push(PatienceCandidate {
            old_index,
            new_index,
            previous: None,
        });
    }

    let mut sequence: Vec<usize> = Vec::with_capacity(candidates.len());
    let mut longest = 0usize;
    let mut locked_anchor: Option<usize> = None;
    for candidate_index in 0..candidates.len() {
        let new_index = candidates[candidate_index].new_index;
        let insert_at =
            sequence[..longest].partition_point(|index| candidates[*index].new_index < new_index);
        candidates[candidate_index].previous = insert_at
            .checked_sub(1)
            .and_then(|index| sequence.get(index).copied());
        if locked_anchor.is_some_and(|locked| insert_at <= locked) {
            continue;
        }
        if insert_at == sequence.len() {
            sequence.push(candidate_index);
        } else {
            sequence[insert_at] = candidate_index;
        }

        let is_anchor = anchor_lines
            .get(new_index)
            .is_some_and(|line| anchors.iter().any(|prefix| line.starts_with(prefix)));
        if is_anchor {
            locked_anchor = Some(insert_at);
            longest = insert_at + 1;
        } else if insert_at == longest {
            longest += 1;
        }
    }

    let Some(mut candidate_index) = longest
        .checked_sub(1)
        .and_then(|index| sequence.get(index).copied())
    else {
        return Vec::new();
    };
    let mut result = Vec::with_capacity(longest);
    loop {
        let candidate = candidates[candidate_index];
        result.push((candidate.old_index, candidate.new_index));
        let Some(previous) = candidate.previous else {
            break;
        };
        candidate_index = previous;
    }
    result.reverse();
    result
}

fn append_similar_range(
    old: &[&str],
    old_range: Range<usize>,
    new: &[&str],
    new_range: Range<usize>,
    algorithm: Algorithm,
    out: &mut Vec<IndexedLineChange>,
) {
    let diff = TextDiff::configure()
        .algorithm(algorithm)
        .diff_slices(&old[old_range.clone()], &new[new_range.clone()]);
    let mut old_index = old_range.start;
    let mut new_index = new_range.start;
    for change in diff.iter_all_changes() {
        match change.tag() {
            ChangeTag::Equal => {
                out.push(IndexedLineChange::Equal { new_index });
                old_index += 1;
                new_index += 1;
            }
            ChangeTag::Delete => {
                out.push(IndexedLineChange::Delete { old_index });
                old_index += 1;
            }
            ChangeTag::Insert => {
                out.push(IndexedLineChange::Insert { new_index });
                new_index += 1;
            }
        }
    }
}

fn anchored_diff_range(
    old: &[&str],
    old_range: Range<usize>,
    new: &[&str],
    new_range: Range<usize>,
    anchor_lines: &[&str],
    anchors: &[String],
    out: &mut Vec<IndexedLineChange>,
) {
    enum Task {
        Diff(Range<usize>, Range<usize>),
        Equal(Range<usize>, Range<usize>),
    }

    let mut stack = vec![Task::Diff(old_range, new_range)];
    while let Some(task) = stack.pop() {
        let (old_range, new_range) = match task {
            Task::Diff(old_range, new_range) => (old_range, new_range),
            Task::Equal(old_range, new_range) => {
                out.extend(
                    old_range
                        .zip(new_range)
                        .map(|(_, new_index)| IndexedLineChange::Equal { new_index }),
                );
                continue;
            }
        };

        if old_range.is_empty() {
            out.extend(new_range.map(|new_index| IndexedLineChange::Insert { new_index }));
            continue;
        }
        if new_range.is_empty() {
            out.extend(old_range.map(|old_index| IndexedLineChange::Delete { old_index }));
            continue;
        }

        let sequence = anchored_patience_sequence(
            old,
            old_range.clone(),
            new,
            new_range.clone(),
            anchor_lines,
            anchors,
        );
        if sequence.is_empty() {
            append_similar_range(old, old_range, new, new_range, Algorithm::Myers, out);
            continue;
        }

        // Build the recursive walk as ordered tasks, then push them in reverse.
        // This preserves Git's patience recursion exactly without exposing the
        // process to stack exhaustion on adversarially nested inputs.
        let mut tasks = Vec::with_capacity(sequence.len().saturating_mul(3).saturating_add(2));
        let mut old_current = old_range.start;
        let mut new_current = new_range.start;
        for (anchor_old, anchor_new) in sequence {
            let mut common_old = anchor_old;
            let mut common_new = anchor_new;
            while common_old > old_current
                && common_new > new_current
                && old[common_old - 1] == new[common_new - 1]
            {
                common_old -= 1;
                common_new -= 1;
            }

            let prefix_old = old_current;
            let prefix_new = new_current;
            while old_current < common_old
                && new_current < common_new
                && old[old_current] == new[new_current]
            {
                old_current += 1;
                new_current += 1;
            }
            if old_current > prefix_old {
                tasks.push(Task::Equal(
                    prefix_old..old_current,
                    prefix_new..new_current,
                ));
            }
            if old_current < common_old || new_current < common_new {
                tasks.push(Task::Diff(old_current..common_old, new_current..common_new));
            }
            tasks.push(Task::Equal(
                common_old..anchor_old + 1,
                common_new..anchor_new + 1,
            ));
            old_current = anchor_old + 1;
            new_current = anchor_new + 1;
        }

        let prefix_old = old_current;
        let prefix_new = new_current;
        while old_current < old_range.end
            && new_current < new_range.end
            && old[old_current] == new[new_current]
        {
            old_current += 1;
            new_current += 1;
        }
        if old_current > prefix_old {
            tasks.push(Task::Equal(
                prefix_old..old_current,
                prefix_new..new_current,
            ));
        }
        if old_current < old_range.end || new_current < new_range.end {
            tasks.push(Task::Diff(
                old_current..old_range.end,
                new_current..new_range.end,
            ));
        }
        stack.extend(tasks.into_iter().rev());
    }
}

fn anchored_indexed_changes(
    old: &[&str],
    new: &[&str],
    anchor_lines: &[&str],
    anchors: &[String],
) -> Vec<IndexedLineChange> {
    let mut changes = Vec::with_capacity(old.len() + new.len());
    anchored_diff_range(
        old,
        0..old.len(),
        new,
        0..new.len(),
        anchor_lines,
        anchors,
        &mut changes,
    );
    changes
}

fn materialize_indexed_changes<'a>(
    changes: &[IndexedLineChange],
    old_lines: &[&'a str],
    new_lines: &[&'a str],
) -> Vec<(ChangeTag, &'a str)> {
    changes
        .iter()
        .map(|change| {
            let line = match *change {
                IndexedLineChange::Delete { old_index } => old_lines[old_index],
                IndexedLineChange::Insert { new_index } => new_lines[new_index],
                // Context comes from the post-image, matching Git and the
                // normalized non-anchored path below.
                IndexedLineChange::Equal { new_index } => new_lines[new_index],
            };
            (change.tag(), line)
        })
        .collect()
}

/// Compute the unified-diff hunk body (the `@@ … @@` blocks, no file header)
/// for `old_text` vs `new_text` at `context` lines of surrounding context.
/// Selected line diff with a rolling-context assembler — a context-parameterized
/// copy of git_internal's `compute_unified_diff`. Myers matches git_internal's
/// initial body; Patience/Histogram/Anchored replace it with their selected
/// anchors.
pub(crate) fn compute_unified_hunks(
    old_text: &str,
    new_text: &str,
    context: usize,
    diff_algorithm: &DiffAlgorithm,
) -> String {
    match diff_algorithm {
        DiffAlgorithm::Anchored(anchors) => {
            let old_records = raw_anchor_records(old_text);
            let new_records = raw_anchor_records(new_text);
            let old_lines: Vec<&str> = old_records
                .iter()
                .map(|line| line.trim_end_matches(['\r', '\n']))
                .collect();
            let new_lines: Vec<&str> = new_records
                .iter()
                .map(|line| line.trim_end_matches(['\r', '\n']))
                .collect();
            let indexed =
                anchored_indexed_changes(&old_records, &new_records, &new_records, anchors);
            let changes = materialize_indexed_changes(&indexed, &old_lines, &new_lines);
            assemble_unified_hunks(&changes, context, old_text.len() + new_text.len())
        }
        _ => {
            let diff = TextDiff::configure()
                .algorithm(diff_algorithm.backend())
                .diff_lines(old_text, new_text);
            let changes: Vec<(ChangeTag, &str)> = diff
                .iter_all_changes()
                .map(|c| (c.tag(), c.value().trim_end_matches(['\r', '\n'])))
                .collect();
            assemble_unified_hunks(&changes, context, old_text.len() + new_text.len())
        }
    }
}

/// Normalizer for `-w` / `--ignore-all-space`: drop every whitespace character
/// so two lines compare equal iff they match after all whitespace is removed.
pub(crate) fn normalize_ignore_all_space(line: &str) -> String {
    line.chars().filter(|c| !c.is_whitespace()).collect()
}

/// Normalizer for `-b` / `--ignore-space-change`: ignore changes in the AMOUNT
/// of whitespace — every maximal run of whitespace collapses to a single space,
/// and trailing whitespace is dropped. The PRESENCE of whitespace still matters,
/// so `"a  b"` ≡ `"a b"` and `"\ta"` ≡ `"  a"` (both `" a"`), but `"a b"` ≠ `"ab"`
/// and `"a"` ≠ `"  a"`. Matches `git diff -b` (verified empirically).
pub(crate) fn normalize_ignore_space_change(line: &str) -> String {
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
pub(crate) fn normalize_ignore_space_at_eol(line: &str) -> String {
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
pub(crate) fn normalize_ignore_cr_at_eol(line: &str) -> String {
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
    diff_algorithm: &DiffAlgorithm,
    normalize: fn(&str) -> String,
) -> String {
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();
    let old_norm: Vec<String> = old_lines.iter().map(|l| normalize(l)).collect();
    let new_norm: Vec<String> = new_lines.iter().map(|l| normalize(l)).collect();
    // `diff_slices` compares `&[&str]` elements; borrow the normalized strings.
    let old_norm_ref: Vec<&str> = old_norm.iter().map(String::as_str).collect();
    let new_norm_ref: Vec<&str> = new_norm.iter().map(String::as_str).collect();
    let changes: Vec<(ChangeTag, &str)> = match diff_algorithm {
        DiffAlgorithm::Anchored(anchors) => {
            let anchor_lines = raw_anchor_records(new_text);
            let indexed =
                anchored_indexed_changes(&old_norm_ref, &new_norm_ref, &anchor_lines, anchors);
            materialize_indexed_changes(&indexed, &old_lines, &new_lines)
        }
        _ => {
            let diff = TextDiff::configure()
                .algorithm(diff_algorithm.backend())
                .diff_slices(&old_norm_ref, &new_norm_ref);
            let mut changes = Vec::with_capacity(old_lines.len() + new_lines.len());
            for change in diff.iter_all_changes() {
                let tag = change.tag();
                let text = match tag {
                    ChangeTag::Delete => change.old_index().map(|i| old_lines[i]).unwrap_or(""),
                    ChangeTag::Insert => change.new_index().map(|i| new_lines[i]).unwrap_or(""),
                    // Context: both sides are equal under `normalize`; Git emits
                    // the post-image (new) line, falling back to the old side.
                    ChangeTag::Equal => change
                        .new_index()
                        .map(|i| new_lines[i])
                        .or_else(|| change.old_index().map(|i| old_lines[i]))
                        .unwrap_or(""),
                };
                changes.push((tag, text));
            }
            changes
        }
    };
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
fn compute_unified_hunks_ignore_blank(
    old_text: &str,
    new_text: &str,
    context: usize,
    diff_algorithm: &DiffAlgorithm,
) -> String {
    compute_unified_hunks_ignore_blank_inner(old_text, new_text, context, diff_algorithm, None)
}

/// `--ignore-blank-lines` composed with a whitespace normalizer (see
/// [`compute_unified_hunks_ignore_blank`]).
fn compute_unified_hunks_ignore_blank_normalized(
    old_text: &str,
    new_text: &str,
    context: usize,
    diff_algorithm: &DiffAlgorithm,
    normalize: fn(&str) -> String,
) -> String {
    compute_unified_hunks_ignore_blank_inner(
        old_text,
        new_text,
        context,
        diff_algorithm,
        Some(normalize),
    )
}

fn compute_unified_hunks_ignore_blank_inner(
    old_text: &str,
    new_text: &str,
    context: usize,
    diff_algorithm: &DiffAlgorithm,
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
    let indexed_changes = match diff_algorithm {
        DiffAlgorithm::Anchored(anchors) => {
            let anchor_lines = raw_anchor_records(new_text);
            anchored_indexed_changes(&old_ref, &new_ref, &anchor_lines, anchors)
        }
        _ => {
            let mut changes = Vec::with_capacity(old_ref.len() + new_ref.len());
            append_similar_range(
                &old_ref,
                0..old_ref.len(),
                &new_ref,
                0..new_ref.len(),
                diff_algorithm.backend(),
                &mut changes,
            );
            changes
        }
    };

    // Build change groups (maximal runs of insert/delete), tracking 0-based old/new
    // positions exactly as Git records i1/i2/chg1/chg2.
    let mut groups: Vec<DiffChangeGroup> = Vec::new();
    let mut old_pos = 0usize;
    let mut new_pos = 0usize;
    let mut cur: Option<DiffChangeGroup> = None;
    for change in indexed_changes {
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
            "{}",
            crate::utils::diff_format::format_hunk_header(
                old_start, old_count, new_start, new_count,
            )
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
        "{}",
        crate::utils::diff_format::format_hunk_header(old_start, old_count, new_start, new_count)
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
        no_index: false,
        pathspec: Vec::new(),
        algorithm: None,
        minimal: false,
        patience: false,
        histogram: false,
        anchored: Vec::new(),
        algorithm_events: Vec::new(),
        output: None,
        name_only: false,
        name_status: false,
        word_diff: None,
        color_words: None,
        word_diff_regex: None,
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
        dirstat: None,
        dirstat_by_file: None,
        cumulative: false,
        stat_name_width: None,
        inter_hunk_context: None,
        ignore_matching_lines: None,
        break_rewrites: None,
        combined_diff: false,
        function_context: false,
        raw: false,
        compact_summary: false,
        diff_filter: None,
        pickaxe_string: None,
        pickaxe_regex: None,
        shortstat: false,
        exit_code: false,
        plumbing_quiet: false,
        no_patch: false,
        null: false,
        check: false,
        reverse: false,
        text: false,
        binary: false,
        full_index: false,
        src_prefix: None,
        dst_prefix: None,
        // Git's commit-verbose helper always renders the built-in staged diff;
        // `diff.external` must not replace the editor template or stderr patch.
        no_ext_diff: true,
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
        plumbing_stat_only: false,
    };
    let config = resolve_diff_config(&args).await?;
    let mut result = run_diff(
        &args,
        &OutputConfig::default(),
        &config,
        None,
        &DiffAlgorithm::Myers,
    )
    .await?;
    apply_diff_prefixes(&mut result, &config.prefixes);
    Ok(format_unified_diff(&result))
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
/// Rewrite `@@ -a,b +c,d @@` lines to Git's canonical form (ADR-LG-04).
///
/// `git_internal` always prints explicit counts. Git omits `,1` and, for an
/// empty side at the start of the file, anchors at 0 (`+0,0`). A zero-count
/// side is anchored at the line before the hunk; when the hunk starts at line
/// 1 on the other side, that anchor is 0.
pub(crate) fn canonicalize_hunk_headers(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for line in raw.split_inclusive('\n') {
        let (content, newline) = match line.strip_suffix('\n') {
            Some(content) => (content, "\n"),
            None => (line, ""),
        };
        if let Some(rebuilt) = rebuild_hunk_header(content) {
            out.push_str(&rebuilt);
            out.push_str(newline);
        } else {
            out.push_str(content);
            out.push_str(newline);
        }
    }
    out
}

fn parse_hunk_span(spec: &str, sign: char) -> Option<(usize, usize)> {
    let spec = spec.strip_prefix(sign)?;
    match spec.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((spec.parse().ok()?, 1)),
    }
}

fn rebuild_hunk_header(line: &str) -> Option<String> {
    let rest = line.strip_prefix("@@ ")?;
    let (spans, suffix) = rest.split_once(" @@")?;
    let mut parts = spans.split_whitespace();
    let old = parts.next()?;
    let new = parts.next()?;
    let (old_start, old_count) = parse_hunk_span(old, '-')?;
    let (new_start, new_count) = parse_hunk_span(new, '+')?;
    let anchor = |start: usize, count: usize, other_start: usize| -> usize {
        if count == 0 && start == 1 && other_start == 1 {
            0
        } else {
            start
        }
    };
    let old_start = anchor(old_start, old_count, new_start);
    let new_start = anchor(new_start, new_count, old_start);
    Some(format!(
        "{}{suffix}",
        crate::utils::diff_format::format_hunk_header(old_start, old_count, new_start, new_count)
    ))
}

fn parse_diff_item(item: &git_internal::diff::DiffItem) -> DiffFileStat {
    let data = canonicalize_hunk_headers(&item.data);
    let status = parse_diff_status(&data);
    let (insertions, deletions) = count_hunk_line_changes(&data);

    DiffFileStat {
        path: item.path.clone(),
        status: status.to_string(),
        insertions,
        deletions,
        hunks: parse_diff_hunks(&data),
        raw_diff: data,
        rename_from: None,
        similarity: None,
        binary: None,
        check_trailing_blank_start: None,
        old_id: None,
        new_id: None,
        old_mode: None,
        new_mode: None,
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

#[cfg(test)]
mod test {
    use std::{cell::Cell, fs, io::Write};

    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::utils::test;

    // Issue #466 unit coverage: the hint is TTY-gated via auto resolution,
    // shows at most once, and is inert after finish(). The reveal/erase
    // mechanics are tested through the `write_scan_hint_line` seam against
    // buffers, never the real stderr.
    fn make_output(progress: ProgressMode) -> OutputConfig {
        OutputConfig {
            progress,
            ..OutputConfig::default()
        }
    }

    #[test]
    fn worktree_scan_hint_disabled_for_auto_without_tty() {
        // Under the repo's acceptance gate (`cargo test --all` run in a real
        // terminal) stderr CAN be a TTY, so auto may resolve to Text. Assert
        // only the invariant: the resolved mode and the hint agree.
        let config = OutputConfig::resolve(None, false, false, "auto", false, false, "auto");
        let expected_enabled = config.progress == ProgressMode::Text;
        let mut hint = WorktreeScanHint::start(&config);
        assert_eq!(hint.enabled, expected_enabled);
        hint.reveal();
        assert_eq!(hint.shown, expected_enabled);
        hint.finish();
    }

    #[test]
    fn worktree_scan_hint_shows_once_and_finishes_clean() {
        let sink = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut hint = WorktreeScanHint {
            enabled: true,
            done: false,
            shown: false,
            sink_override: Some(Arc::clone(&sink)),
            test_tty: Some(true),
        };

        hint.reveal();
        assert!(hint.shown);
        // Repeat reveals are single-shot.
        hint.reveal();
        assert!(hint.shown);

        hint.finish();
        assert!(!hint.shown);
        assert!(hint.done, "finish must latch done");
        // After finish the hint stays inert.
        hint.reveal();
        assert!(!hint.shown);
        hint.finish();

        // TTY stream: reveal line, then the erase rewrites it in place; the
        // final buffer must be exactly one erase sequence (no residue).
        let out = sink.lock().expect("test sink lock").clone();
        let mut expected = b"\r\x1b[2KScanning working tree ...".to_vec();
        expected.extend_from_slice(b"\r\x1b[2K");
        assert_eq!(
            out, expected,
            "TTY reveal+erase must leave only the erase sequence"
        );
    }

    #[test]
    fn worktree_scan_hint_redirected_text_gets_bare_line_and_no_ansi() {
        let sink = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut hint = WorktreeScanHint {
            enabled: true,
            done: false,
            shown: false,
            sink_override: Some(Arc::clone(&sink)),
            test_tty: Some(false),
        };
        hint.reveal();
        hint.finish();
        let out = sink.lock().expect("test sink lock").clone();
        assert_eq!(
            out,
            b"Scanning working tree ...\n".to_vec(),
            "redirected stderr must receive the bare line only, no ANSI escapes"
        );
    }

    #[test]
    fn worktree_scan_hint_disabled_for_json_mode() {
        // JSON consumers get the immediate diff_scan.start event instead; the
        // deferred text hint must not double-report in JSON mode.
        let mut hint = WorktreeScanHint::start(&make_output(ProgressMode::Json));
        assert!(!hint.enabled);
        hint.reveal();
        assert!(!hint.shown);
    }

    #[test]
    fn worktree_scan_hint_disabled_for_explicit_none() {
        let mut hint = WorktreeScanHint::start(&make_output(ProgressMode::None));
        assert!(!hint.enabled);
        hint.reveal();
        assert!(!hint.shown);
    }

    #[test]
    fn scan_hint_line_uses_ansi_on_tty_and_plain_text_when_redirected() {
        // TTY: single-line redraw with no trailing newline (so the erase can
        // rewrite it), and the erase rewrites the same line.
        let mut tty: Vec<u8> = Vec::new();
        write_scan_hint_line_into(&mut tty, "Scanning working tree ...", true);
        assert_eq!(tty, b"\r\x1b[2KScanning working tree ...");
        let mut tty_erase: Vec<u8> = Vec::new();
        write_scan_hint_line_into(&mut tty_erase, "", true);
        assert_eq!(tty_erase, b"\r\x1b[2K");

        // Redirected: bare line + newline, no escape bytes; erase is a no-op.
        let mut redirected: Vec<u8> = Vec::new();
        write_scan_hint_line_into(&mut redirected, "Scanning working tree ...", false);
        assert_eq!(redirected, b"Scanning working tree ...\n");
        let mut redirected_erase: Vec<u8> = Vec::new();
        write_scan_hint_line_into(&mut redirected_erase, "", false);
        assert!(redirected_erase.is_empty());
    }

    #[tokio::test]
    async fn race_scan_with_hint_fast_scan_is_silent_and_single_poll() {
        let sink = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut hint = WorktreeScanHint {
            enabled: true,
            done: false,
            shown: false,
            sink_override: Some(Arc::clone(&sink)),
            test_tty: Some(false),
        };
        // A scan that completes immediately: the Ok arm must take the result
        // out of the timeout without ever awaiting the completed handle
        // again (a second poll would panic).
        let mut handle = tokio::task::spawn_blocking(|| 42_u8);
        // The production 2 s quiet period is used deliberately: the blocking
        // task finishes in microseconds, so the race is deterministic
        // regardless of runner load (a longer timer only delays completion
        // until the handle resolves).
        let result = race_scan_with_hint(&mut handle, Duration::from_secs(2), &mut hint)
            .await
            .expect("fast scan should succeed");
        assert_eq!(result, 42);
        assert!(!hint.shown, "fast scan must stay silent");
        assert!(sink.lock().expect("test sink lock").is_empty());
    }

    #[tokio::test]
    async fn race_scan_with_hint_slow_scan_reveals_once_and_completes() {
        let sink = Arc::new(std::sync::Mutex::new(Vec::new()));
        let mut hint = WorktreeScanHint {
            enabled: true,
            done: false,
            shown: false,
            sink_override: Some(Arc::clone(&sink)),
            test_tty: Some(false),
        };
        // A scan that completes only after real time passes the (short) test
        // quiet period: the Err arm must reveal the hint, then await the
        // SAME handle (one scan, no re-run, no truncation). The quiet period
        // (200 ms) is far below the blocking sleep (1200 ms) so the timer
        // deterministically wins.
        let mut handle = tokio::task::spawn_blocking(|| {
            std::thread::sleep(std::time::Duration::from_millis(1200));
            7_u8
        });
        let result = race_scan_with_hint(&mut handle, Duration::from_millis(200), &mut hint)
            .await
            .expect("slow scan should complete");
        assert_eq!(result, 7);
        assert!(hint.shown, "slow scan must reveal the hint");
        // Redirected stderr simulation: exactly one bare line, no ANSI.
        assert_eq!(
            sink.lock().expect("test sink lock").clone(),
            b"Scanning working tree ...\n".to_vec()
        );
    }

    #[test]
    fn regex_word_diff_matches_git_delimiter_and_whole_line_semantics() {
        let regex = regex::Regex::new("[A-Za-z]+").expect("test regex is valid");
        assert_eq!(
            render_word_diff(
                "foo.bar\n",
                "foo,baz\n",
                WordDiffMode::Plain,
                false,
                Some(&regex),
            ),
            "foo,[-bar-]{+baz+}\n"
        );
        assert_eq!(
            render_word_diff("foo.bar\n", "\n", WordDiffMode::Plain, false, Some(&regex),),
            "[-foo.bar-]\n"
        );
        assert_eq!(
            render_word_diff("\n", "foo.bar\n", WordDiffMode::Plain, false, Some(&regex),),
            "{+foo.bar+}\n"
        );
        assert_eq!(
            render_word_diff(
                "foo...bar\n",
                "foo+++bar\n",
                WordDiffMode::Plain,
                false,
                Some(&regex),
            ),
            "foo+++bar\n"
        );
        assert_eq!(
            render_word_diff(
                "foo.bar\n",
                "foo,baz\n",
                WordDiffMode::Porcelain,
                false,
                Some(&regex),
            ),
            " foo,\n-bar\n+baz\n~\n"
        );
    }

    #[test]
    fn regex_word_tokens_truncate_cross_newline_match() {
        let regex = regex::Regex::new("(?s)foo.*baz").expect("test regex is valid");
        let tokens = regex_word_tokens("foo\nbar\nbaz", &regex);
        assert_eq!(
            tokens.iter().map(|token| token.text).collect::<Vec<_>>(),
            vec!["foo", "\n", "\n"]
        );
    }

    #[test]
    fn word_diff_regex_resolution_preserves_none_and_explicit_precedence() {
        let disabled =
            DiffArgs::try_parse_from(["diff", "--word-diff=none", "--word-diff-regex=[A-Za-z]+"])
                .expect("valid word diff arguments");
        let disabled = resolve_word_diff_options(&disabled).expect("valid word regex");
        assert!(disabled.mode.is_none());

        let explicit = DiffArgs::try_parse_from([
            "diff",
            "--color-words=[0-9]+",
            "--word-diff-regex=[A-Za-z]+",
        ])
        .expect("valid word diff arguments");
        let explicit = resolve_word_diff_options(&explicit).expect("valid word regex");
        assert!(matches!(explicit.mode, Some(WordDiffMode::Color)));
        assert_eq!(
            explicit.regex.as_ref().map(regex::Regex::as_str),
            Some("[A-Za-z]+")
        );
        assert!(explicit.force_auto_color);
    }

    #[tokio::test]
    async fn preview_object_count_is_rejected_before_batch_sizing() {
        let temp = tempfile::tempdir().expect("create preview-count fixture");
        crate::utils::preview_object::with_objects(temp.path().join("objects"), async {
            let hashes = (0..=4_096u32)
                .map(|number| {
                    ObjectHash::from_type_and_data(ObjectType::Blob, &number.to_be_bytes())
                })
                .collect::<Vec<_>>();
            let sizing_called = Cell::new(false);
            let error = preflight_preview_object_sizes(hashes, |_| {
                sizing_called.set(true);
                Ok(Vec::new())
            })
            .expect_err("4,097 changed objects must fail before storage sizing");

            assert!(error.to_string().contains("object count exceeds 4096"));
            assert!(!sizing_called.get(), "storage sizing must not be invoked");
        })
        .await;
    }

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

    fn filter_fixture(path: &str, status: &str) -> DiffFileStat {
        DiffFileStat {
            path: path.to_string(),
            status: status.to_string(),
            insertions: 0,
            deletions: 0,
            hunks: Vec::new(),
            raw_diff: String::new(),
            rename_from: None,
            similarity: None,
            binary: None,
            check_trailing_blank_start: None,
            old_id: None,
            new_id: None,
            old_mode: None,
            new_mode: None,
        }
    }

    #[test]
    fn diff_filter_parsing_and_all_or_none_match_git_semantics() {
        let filter = parse_diff_filter(Some("ad*"))
            .expect("valid filter")
            .unwrap();
        assert!(filter.exclude.contains(&'A'));
        assert!(filter.exclude.contains(&'D'));
        assert!(filter.all_or_none);

        let mut files = vec![
            filter_fixture("added", "added"),
            filter_fixture("deleted", "deleted"),
            filter_fixture("modified", "modified"),
        ];
        apply_diff_filter(&mut files, &filter);
        assert_eq!(files.len(), 3, "`*` retains the original set after a match");

        let filter = parse_diff_filter(Some("T*"))
            .expect("valid filter")
            .unwrap();
        apply_diff_filter(&mut files, &filter);
        assert!(files.is_empty(), "no T means the all-or-none set is empty");
        assert!(matches!(
            parse_diff_filter(Some("Q")),
            Err(DiffError::InvalidDiffFilter(value)) if value == "Q"
        ));
        assert!(matches!(
            parse_diff_filter(Some("")),
            Err(DiffError::InvalidDiffFilter(value)) if value.is_empty()
        ));
    }

    #[test]
    fn pickaxe_literal_count_is_non_overlapping_and_byte_safe() {
        assert_eq!(count_literal_occurrences(b"aaaa", b"aa"), 2);
        assert_eq!(count_literal_occurrences(b"ababa", b"aba"), 1);
        assert_eq!(
            count_literal_occurrences(b"a\0needle\xffneedle", b"needle"),
            2
        );
        assert_eq!(count_literal_occurrences(b"anything", b""), 0);
    }

    #[test]
    fn pickaxe_regex_matches_only_changed_hunk_content() {
        let patch = "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1,2 +1,2 @@\n context needle\n-old\n+changed needle\n";
        assert!(changed_diff_line_matches(
            patch,
            &regex::Regex::new("changed needle").expect("valid regex")
        ));
        assert!(!changed_diff_line_matches(
            patch,
            &regex::Regex::new("a/file|context needle").expect("valid regex")
        ));

        let combined = "diff --cc conflict\n@@@ -1,1 -1,1 +1,1 @@@\n -handler_v7\n++handler_v8\n";
        assert!(changed_diff_line_matches(
            combined,
            &regex::Regex::new("handler_v[0-9]").expect("valid regex")
        ));
    }

    #[test]
    fn pickaxe_arguments_conflict_and_invalid_regex_is_rejected() {
        let args = DiffArgs::try_parse_from(["diff", "-G", "["]).expect("clap accepts regex");
        assert!(matches!(
            parse_diff_pickaxe(&args),
            Err(DiffError::InvalidPickaxeRegex { pattern, .. }) if pattern == "["
        ));
        assert!(
            DiffArgs::try_parse_from(["diff", "-S", "needle", "-G", "needle"]).is_err(),
            "-S and -G are mutually exclusive"
        );
    }

    #[test]
    fn compact_summary_labels_mode_metadata() {
        let mut created = filter_fixture("created", "added");
        created.new_mode = Some(0o100755);
        assert_eq!(compact_summary_label(&created).as_deref(), Some("new +x"));

        let mut type_change = filter_fixture("link", "modified");
        type_change.old_mode = Some(0o100644);
        type_change.new_mode = Some(0o120000);
        assert_eq!(compact_summary_label(&type_change).as_deref(), Some("+l"));
        assert_eq!(diff_status_code(&type_change), 'T');

        type_change.raw_diff =
            "diff --git a/link b/link\nindex 1111111..2222222 100644\n--- a/link\n+++ b/link\n"
                .to_string();
        apply_mode_metadata_to_patch(&mut type_change);
        assert!(
            type_change
                .raw_diff
                .contains("old mode 100644\nnew mode 120000")
        );
        assert!(type_change.raw_diff.contains("index 1111111..2222222\n"));

        let mut executable = filter_fixture("script", "modified");
        executable.old_mode = Some(0o100755);
        executable.new_mode = Some(0o100755);
        executable.raw_diff =
            "diff --git a/script b/script\nindex 1111111..2222222 100644\n".to_string();
        apply_mode_metadata_to_patch(&mut executable);
        assert!(
            executable
                .raw_diff
                .contains("index 1111111..2222222 100755")
        );
    }

    #[test]
    fn inexact_rename_detection_obeys_git_default_limit() {
        use std::{path::PathBuf, rc::Rc};

        use super::rename_detect::{
            BlobEvidence, BlobKind, BlobRef, ContentOutcome, RenameContentSource,
            RenameDetectConfig, RenameSnapshot, match_pairs,
        };

        // `diff` no longer owns a limit gate: it delegates to the shared
        // engine, so the per-side OR semantics are asserted THERE — through
        // the same call `diff` makes. Duplicating the rule in a local helper
        // is exactly how the two commands drifted apart before.
        struct Bodies;
        impl RenameContentSource for Bodies {
            fn old_content(&mut self, path: &Path, _blob: &BlobRef) -> ContentOutcome {
                ContentOutcome::Content(Rc::new(
                    format!("shared body\ncommon line\n{}\n", path.display()).into_bytes(),
                ))
            }
            fn new_content(&mut self, path: &Path, _blob: &BlobRef) -> ContentOutcome {
                ContentOutcome::Content(Rc::new(
                    format!("shared body\ncommon line\n{}\n", path.display()).into_bytes(),
                ))
            }
        }

        let blob = |seed: u8| BlobRef {
            kind: BlobKind::Regular,
            mode: 0o100644,
            size: Some(32),
            evidence: BlobEvidence::KnownObjectId {
                oid: git_internal::internal::object::blob::Blob::from_content(&format!(
                    "distinct {seed}"
                ))
                .id,
            },
        };
        let snapshot = || {
            let mut snapshot = RenameSnapshot::default();
            for i in 0..2u8 {
                snapshot
                    .old_map
                    .insert(PathBuf::from(format!("old{i}.txt")), blob(i));
                snapshot
                    .new_map
                    .insert(PathBuf::from(format!("new{i}.txt")), blob(i + 100));
            }
            snapshot
        };
        let config = |rename_limit| RenameDetectConfig {
            threshold: 30_000,
            rename_limit,
            comparison_budget: None,
        };

        // Per-side limit 1 with 2 sources and 2 destinations: the exhaustive
        // stage is gated.
        let gated = match_pairs(&snapshot(), &config(1), &mut Bodies);
        assert!(
            gated.stats.skipped_by_limit,
            "a side above the limit gates the exhaustive stage"
        );
        // The Git default comfortably admits this candidate set…
        let allowed = match_pairs(&snapshot(), &config(DEFAULT_RENAME_LIMIT), &mut Bodies);
        assert!(!allowed.stats.skipped_by_limit, "1000 admits 2 vs 2");
        // …and `0` disables the cap entirely.
        let uncapped = match_pairs(&snapshot(), &config(0), &mut Bodies);
        assert!(
            !uncapped.stats.skipped_by_limit,
            "renameLimit = 0 means no per-side cap"
        );
        assert_eq!(DEFAULT_RENAME_LIMIT, 1000, "Git's default is 1000");
    }

    #[test]
    fn prefix_rewrite_preserves_crlf_hunk_bytes() {
        let raw_diff = "diff --git a/f.txt b/f.txt\r\nindex 1111111..2222222 100644\r\n--- a/f.txt\r\n+++ b/f.txt\r\n@@ -1 +1 @@\r\n-old\r\n+new\r\n";
        let mut result = DiffOutput {
            old_ref: "index".to_string(),
            new_ref: "worktree".to_string(),
            files: vec![DiffFileStat {
                path: "f.txt".to_string(),
                status: "modified".to_string(),
                insertions: 1,
                deletions: 1,
                hunks: Vec::new(),
                raw_diff: raw_diff.to_string(),
                rename_from: None,
                similarity: None,
                binary: None,
                check_trailing_blank_start: None,
                old_id: None,
                new_id: None,
                old_mode: None,
                new_mode: None,
            }],
            total_insertions: 1,
            total_deletions: 1,
            files_changed: 1,
            external_diff_applied: false,
            binary_patch: false,
        };
        apply_diff_prefixes(
            &mut result,
            &DiffPrefixes {
                source: "OLD/".to_string(),
                destination: "NEW/".to_string(),
            },
        );

        assert_eq!(
            result.files[0].raw_diff,
            "diff --git OLD/f.txt NEW/f.txt\r\nindex 1111111..2222222 100644\r\n--- OLD/f.txt\r\n+++ NEW/f.txt\r\n@@ -1 +1 @@\r\n-old\r\n+new\r\n"
        );
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
    #[serial_test::serial(hash_kind)]
    fn test_diff_algorithms_use_selected_line_anchors() {
        let old = "void alpha() {\n    one();\n}\n\nvoid beta() {\n    two();\n}\n";
        let new = "void beta() {\n    two();\n}\n\nvoid alpha() {\n    one();\n}\n";
        let myers = compute_unified_hunks(old, new, 3, &DiffAlgorithm::Myers);
        let minimal = compute_unified_hunks(old, new, 3, &DiffAlgorithm::MyersMinimal);
        let patience = compute_unified_hunks(old, new, 3, &DiffAlgorithm::Patience);
        let histogram = compute_unified_hunks(old, new, 3, &DiffAlgorithm::Histogram);

        assert_eq!(minimal, myers, "minimal uses the no-deadline Myers backend");
        assert_ne!(patience, myers, "patience must use its own anchors");
        assert!(
            !histogram.is_empty() && DiffAlgorithm::Histogram.backend() == Algorithm::Histogram,
            "histogram must select the histogram backend"
        );
    }

    #[test]
    #[serial_test::serial(hash_kind)]
    fn anchored_patience_locks_qualifying_crossing_line() {
        let old = ["ANCHOR", "b", "c"];
        let new = ["b", "c", "ANCHOR"];

        assert_eq!(
            anchored_patience_sequence(&old, 0..old.len(), &new, 0..new.len(), &new, &[]),
            vec![(1, 0), (2, 1)],
            "ordinary patience chooses the longest b/c sequence"
        );
        assert_eq!(
            anchored_patience_sequence(
                &old,
                0..old.len(),
                &new,
                0..new.len(),
                &new,
                &["ANCH".to_string()],
            ),
            vec![(0, 2)],
            "the qualifying unique line locks its LIS position"
        );

        let anchored = compute_unified_hunks(
            "ANCHOR\nb\nc\n",
            "b\nc\nANCHOR\n",
            3,
            &DiffAlgorithm::Anchored(vec!["ANCH".to_string()]),
        );
        assert!(
            anchored.lines().any(|line| line == " ANCHOR"),
            "the anchor must be context, not delete+insert:\n{anchored}"
        );
        assert!(!anchored.lines().any(|line| line == "-ANCHOR"));
        assert!(!anchored.lines().any(|line| line == "+ANCHOR"));

        let crlf_anchored = compute_unified_hunks(
            "ANCHOR\r\nb\r\nc\r\n",
            "b\r\nc\r\nANCHOR\r\n",
            3,
            &DiffAlgorithm::Anchored(vec!["ANCHOR\r\n".to_string()]),
        );
        assert!(
            crlf_anchored.lines().any(|line| line == " ANCHOR"),
            "anchor prefixes are matched against the raw CRLF record:\n{crlf_anchored}"
        );
        let crlf_change = compute_unified_hunks(
            "a\r\n",
            "a\n",
            3,
            &DiffAlgorithm::Anchored(vec!["a".to_string()]),
        );
        assert!(
            crlf_change.lines().any(|line| line == "-a")
                && crlf_change.lines().any(|line| line == "+a"),
            "raw record comparison must preserve a CRLF-to-LF change:\n{crlf_change}"
        );

        let duplicate_old = ["ANCHOR", "ANCHOR", "b", "c"];
        let duplicate_new = ["b", "c", "ANCHOR", "ANCHOR"];
        assert_eq!(
            anchored_patience_sequence(
                &duplicate_old,
                0..duplicate_old.len(),
                &duplicate_new,
                0..duplicate_new.len(),
                &duplicate_new,
                &["ANCH".to_string()],
            ),
            vec![(2, 0), (3, 1)],
            "a matching prefix cannot anchor a line repeated on either side"
        );

        let no_unique_old = "x\nx\na\n";
        let no_unique_new = "x\nx\nb\n";
        assert_eq!(
            compute_unified_hunks(
                no_unique_old,
                no_unique_new,
                3,
                &DiffAlgorithm::Anchored(vec!["x".to_string()]),
            ),
            compute_unified_hunks(no_unique_old, no_unique_new, 3, &DiffAlgorithm::Myers),
            "a range without a unique common candidate falls back to Myers"
        );
    }

    #[test]
    fn anchored_selector_events_match_git_retention_and_clear_rules() {
        let resolve = |tail: &[&str]| {
            let mut parse_argv = vec!["diff"];
            parse_argv.extend_from_slice(tail);
            let mut args = DiffArgs::try_parse_from(parse_argv).expect("selectors should parse");
            let mut raw_argv = vec!["libra".to_string(), "diff".to_string()];
            raw_argv.extend(tail.iter().map(|value| value.to_string()));
            record_algorithm_selector_events(&mut args, &raw_argv);
            resolve_diff_algorithm(&args).expect("selectors should resolve")
        };

        assert_eq!(
            resolve(&["--anchored=A", "--histogram", "--anchored", "B",]),
            DiffAlgorithm::Anchored(vec!["A".to_string(), "B".to_string()]),
            "histogram leaves earlier anchors dormant for later reactivation"
        );
        assert_eq!(
            resolve(&["--anchored=A", "--patience", "--anchored=B"]),
            DiffAlgorithm::Anchored(vec!["B".to_string()]),
            "the patience shorthand clears retained anchors"
        );
        assert_eq!(
            resolve(&["--anchored=A", "--algorithm=myers"]),
            DiffAlgorithm::Myers,
            "a later named Myers selector wins"
        );
        assert_eq!(
            resolve(&["--anchored=A", "--algorithm=patience"]),
            DiffAlgorithm::Anchored(vec!["A".to_string()]),
            "named patience retains and reactivates existing anchors"
        );
        assert_eq!(
            resolve(&["--algorithm=histogram", "--anchored=A", "--minimal"]),
            DiffAlgorithm::Anchored(vec!["A".to_string()]),
            "minimal does not replace an anchored selection"
        );
    }

    #[test]
    fn anchored_uses_git_patience_tie_break_without_a_matching_prefix() {
        let old = ["b", "a"];
        let new = ["a", "b"];
        let tags: Vec<ChangeTag> =
            anchored_indexed_changes(&old, &new, &new, &["never".to_string()])
                .into_iter()
                .map(IndexedLineChange::tag)
                .collect();

        // Upstream xpatience replaces the length-1 LIS tail when it encounters
        // `a`, so `a` is the common line. `similar` chooses the other valid tie;
        // anchored mode intentionally follows Git's ordering.
        assert_eq!(
            tags,
            vec![ChangeTag::Delete, ChangeTag::Equal, ChangeTag::Insert]
        );
    }

    #[test]
    fn test_ignore_blank_lines_far_blank_is_suppressed() {
        // `a..h` -> `a,<blank>,b..g,H`. The blank (old~1) and h->H (old-8) are
        // distance 7 apart > 2*ctx(6), so they do NOT merge: the blank-only hunk
        // is suppressed and only the content hunk survives (Git: `@@ -5,4 +6,4 @@`).
        let old = "a\nb\nc\nd\ne\nf\ng\nh\n";
        let new = "a\n\nb\nc\nd\ne\nf\ng\nH\n";
        let body = compute_unified_hunks_ignore_blank(old, new, 3, &DiffAlgorithm::Myers);
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
        let body = compute_unified_hunks_ignore_blank(old, new, 2, &DiffAlgorithm::Myers);
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
        let body = compute_unified_hunks_ignore_blank(old, new, 3, &DiffAlgorithm::Myers);
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
        let body = compute_unified_hunks_ignore_blank(old, new, 3, &DiffAlgorithm::Myers);
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
            compute_unified_hunks_ignore_blank("x\ny\n", "x\n\ny\n", 3, &DiffAlgorithm::Myers,)
                .trim()
                .is_empty(),
            "blank-only change yields no hunks"
        );
        // A whitespace-only added line is NOT blank -> a hunk survives.
        let ws =
            compute_unified_hunks_ignore_blank("a\nb\n", "a\n  \nb\n", 3, &DiffAlgorithm::Myers);
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
        let body =
            compute_unified_hunks_ignore_blank("a\nb\n", "a\n\r\nb\n", 3, &DiffAlgorithm::Myers);
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
            &DiffAlgorithm::Myers,
            normalize_ignore_all_space,
        );
        assert!(
            composed.trim().is_empty(),
            "-w makes the whitespace-only line blank, so it is suppressed:\n{composed}"
        );
        // Without the normalizer, a whitespace-only line is NOT blank -> shown.
        let plain =
            compute_unified_hunks_ignore_blank("a\nb\n", "a\n  \nb\n", 3, &DiffAlgorithm::Myers);
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
            compute_unified_hunks_ignore_blank(old, new, 3, &DiffAlgorithm::Myers)
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
            // --algorithm selects a real backend.
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
            assert_eq!(resolve_diff_algorithm(&args).unwrap(), DiffAlgorithm::Myers);
        }
        {
            // Git-compatible default: Myers.
            let args = DiffArgs::try_parse_from(["diff", "--old", "old", "target paths"]).unwrap();
            assert_eq!(args.algorithm, None);
            assert_eq!(resolve_diff_algorithm(&args).unwrap(), DiffAlgorithm::Myers);
        }
        {
            let args = DiffArgs::try_parse_from([
                "diff",
                "--minimal",
                "--patience",
                "--histogram",
                "--algorithm",
                "patience",
            ])
            .unwrap();
            assert_eq!(
                resolve_diff_algorithm(&args).unwrap(),
                DiffAlgorithm::Patience
            );
            assert!(
                args.minimal,
                "--minimal remains an independent quality request"
            );
            assert!(!args.patience && !args.histogram, "last selector wins");
        }
        {
            let args = DiffArgs::try_parse_from(["diff", "--minimal"]).unwrap();
            assert_eq!(
                resolve_diff_algorithm(&args).unwrap(),
                DiffAlgorithm::MyersMinimal
            );
        }
        {
            let args = DiffArgs::try_parse_from(["diff", "--algorithm", "bogus"]).unwrap();
            let err = resolve_diff_algorithm(&args).expect_err("invalid backend must fail closed");
            assert_eq!(
                err.to_string(),
                "invalid diff algorithm 'bogus'; expected myers, myersMinimal, patience, or histogram"
            );
            assert!(matches!(err, DiffError::InvalidAlgorithm(value) if value == "bogus"));
        }
    }

    #[test]
    #[serial(cwd, env)]
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
    #[serial(cwd, env)]
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
    #[serial(cwd, env)]
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
    #[serial(cwd, env)]
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

        // The racily-clean guard only trusts the shortcut for files
        // strictly OLDER than the on-disk index snapshot (2026-08-06
        // R0-8 review): rewind the file's mtime, build the entry from
        // the rewound stat, THEN persist the index so its snapshot
        // timestamp is later than the file.
        let earlier = std::time::SystemTime::now() - std::time::Duration::from_secs(5);
        let file = fs::OpenOptions::new()
            .write(true)
            .open("tracked.txt")
            .unwrap();
        file.set_times(fs::FileTimes::new().set_modified(earlier))
            .unwrap();
        drop(file);

        let mut index = Index::new();
        index.add(
            IndexEntry::new_from_file(Path::new("tracked.txt"), indexed_hash, temp_path.path())
                .unwrap(),
        );
        index.save(crate::utils::path::index()).unwrap();

        // Stat alone cannot prove equivalence under the current attributes.
        let paths = [PathBuf::from("tracked.txt")];
        assert_eq!(
            get_files_blobs(&paths, &index, IgnorePolicy::Respect).unwrap(),
            vec![(PathBuf::from("tracked.txt"), worktree_hash)]
        );
        // Install the certificate normally recorded by a clean status scan.
        // The deliberately different hash above makes reuse observable here.
        let conversion =
            crate::command::status_io_worker::ConversionScan::new(&util::working_dir(), &index)
                .unwrap();
        let (sources, _) = conversion.probe(None).unwrap();
        conversion.probe(Some(&sources)).unwrap();
        assert!(conversion.probe(None).unwrap().1);

        let blobs = get_files_blobs(
            &[PathBuf::from("tracked.txt")],
            &index,
            IgnorePolicy::Respect,
        )
        .unwrap();

        assert_eq!(blobs, vec![(PathBuf::from("tracked.txt"), indexed_hash)]);

        fs::write(".gitattributes", "*.txt text\n").unwrap();
        assert_eq!(
            get_files_blobs(&paths, &index, IgnorePolicy::Respect).unwrap(),
            vec![(PathBuf::from("tracked.txt"), worktree_hash)]
        );
    }
}
