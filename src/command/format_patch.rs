//! Generate mbox-formatted email patch files from commits.
//!
//! `libra format-patch` walks a revision range, produces one patch file per
//! non-merge commit (named with `--suffix`, default `.patch`, unless
//! `--numbered-files` is set, which uses bare sequence numbers), and formats each
//! as an mbox message with RFC 2822 headers,
//! a plain-text diffstat, and a unified diff.  The output is designed for
//! email-based patch review and is compatible with `git am`.

use std::{
    collections::HashSet,
    io::Write,
    path::{Path, PathBuf},
};

use chrono::{DateTime, Utc};
use clap::Parser;
use git_internal::{
    hash::{HashKind, ObjectHash},
    internal::object::{ObjectTrait, commit::Commit},
};
use serde::Serialize;

use crate::{
    command::log,
    common_utils::parse_commit_msg,
    internal::notes,
    utils::{
        atomic_write,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, stdout_write_error},
        util,
    },
};

const DEFAULT_SUBJECT_PREFIX: &str = "PATCH";
const DEFAULT_SUFFIX: &str = ".patch";

// ---------------------------------------------------------------------------
// EXAMPLES constant for `--help` (required by compat_help_examples_banner)
// ---------------------------------------------------------------------------
pub const FORMAT_PATCH_EXAMPLES: &str = "\
Examples:
  # Generate patches for the last two commits
  libra format-patch HEAD~2..HEAD

  # Generate exactly HEAD, or include all commits through the root
  libra format-patch -1 --stdout
  libra format-patch --root --stdout

  # Numbered output to a directory
  libra format-patch -n -o patches/ main..feature

  # With cover letter and threading
  libra format-patch --cover-letter --thread origin/main..

  # Version 2 of a series, replying to a previous thread
  libra format-patch -v 2 --in-reply-to '<msgid@example>' origin/main..

  # All output to stdout (suitable for piping to `git am`)
  libra format-patch --stdout origin/main..

  # Skip patch-equivalent upstream commits and customize diff path prefixes
  libra format-patch --ignore-if-in-upstream --src-prefix=old/ --dst-prefix=new/ origin/main..HEAD

  # Add recipient headers (To: and Cc:, repeatable)
  libra format-patch --to reviewer@example.com --cc list@example.com origin/main..

  # Rewrite the From: header (original author kept in-body for git am)
  libra format-patch --from='Maintainer <maint@example.com>' origin/main..

  # Custom filename suffix (0001-subject.txt instead of .patch)
  libra format-patch --suffix=.txt HEAD~2..HEAD

  # Signature footer from a file, RFC 2047-encoding non-ASCII headers
  libra format-patch --signature-file SIGNATURE --encode-email-headers HEAD~2..HEAD

  # Append each commit's notes after the --- line (default ref, then a custom ref)
  libra format-patch --notes --stdout HEAD~2..HEAD
  libra format-patch --notes=review --stdout HEAD~2..HEAD

  # Send the patch as a MIME attachment (or inline) part
  libra format-patch --attach --stdout HEAD~2..HEAD
  libra format-patch --inline --stdout HEAD~2..HEAD

  # Record the base commit (and prerequisite patch-ids) for `git am --base`
  libra format-patch --base=origin/main --stdout origin/main..HEAD
";

// ---------------------------------------------------------------------------
// Args
// ---------------------------------------------------------------------------

/// Generate mbox-formatted email patch files from commits.
#[derive(Parser, Debug)]
#[command(after_help = FORMAT_PATCH_EXAMPLES)]
pub struct FormatPatchArgs {
    /// Revision range expression: a single commit, or a range like A..B.
    /// When a single commit is given, all commits reachable from HEAD but not
    #[cfg_attr(
        doc,
        doc = " from that commit are included (equivalent to `<commit>`..HEAD)."
    )]
    #[cfg_attr(
        not(doc),
        doc = " from that commit are included (equivalent to <commit>..HEAD)."
    )]
    #[arg(value_name = "revision-range")]
    pub revision_range: Option<String>,

    /// Generate only the named commit, or HEAD when no revision is given.
    #[arg(short = '1')]
    pub last_one: bool,

    /// Generate patches for the last `<n>` commits (Git's `-<n>` shorthand,
    /// e.g. `-3`). `-1` is the same flag, kept for its dedicated meaning.
    #[arg(long = "last-n", value_name = "N", hide = true)]
    pub last_n: Option<usize>,

    /// Include the root commit and all reachable non-merge ancestors.
    #[arg(long = "root")]
    pub root: bool,

    /// Write patch files into DIR instead of the current working directory.
    #[arg(short = 'o', long = "output-directory", value_name = "DIR")]
    pub output_directory: Option<String>,

    /// Print all patches to stdout instead of individual files.
    #[arg(long = "stdout")]
    pub stdout: bool,

    /// Add the [PATCH n/m] counter to commit patch Subject lines.
    #[arg(short = 'n', long = "numbered")]
    pub numbered: bool,

    /// Start numbering patches at N instead of 1.
    #[arg(long = "start-number", value_name = "N", default_value = "1")]
    pub start_number: usize,

    /// Prefix the subject with `RFC` (Git's `--rfc[=<text>]`, e.g.
    /// `[RFC PATCH]`). With a value, that text replaces `RFC`
    /// (`--rfc=WIP` → `[WIP PATCH]`).
    #[arg(long = "rfc", value_name = "TEXT", num_args = 0..=1, require_equals = true, default_missing_value = "RFC", overrides_with = "no_rfc")]
    pub rfc: Option<String>,

    /// Do not add the `RFC` marker (Git's `--no-rfc`).
    #[arg(long = "no-rfc", overrides_with = "rfc")]
    pub no_rfc: bool,

    /// Do not add [PATCH n/m] counters to commit patch or cover-letter Subject lines.
    #[arg(short = 'N', long = "no-numbered", overrides_with = "numbered")]
    pub no_numbered: bool,

    /// Do not emit the `base-commit:` trailer, overriding `--base` (Git's
    /// `--no-base`; Libra has no `format.useAutoBase` config).
    #[arg(long = "no-base", overrides_with = "base")]
    pub no_base: bool,

    /// Truncate output filenames to at most `<n>` bytes (Git's
    /// `--filename-max-length`). The `0001-` sequence prefix counts toward the
    /// limit; a trailing `-` left by truncation is dropped.
    #[arg(long = "filename-max-length", value_name = "N")]
    pub filename_max_length: Option<usize>,

    /// Format of the cover letter's commit list (Git's
    /// `--commit-list-format`). `modern` renders `[i/N] subject` lines; any
    /// other value is a pretty-format string supporting `%h`, `%H`, `%s`,
    /// `%an`, `%ae`, `%n` and `%%`.
    #[arg(long = "commit-list-format", value_name = "FORMAT")]
    pub commit_list_format: Option<String>,

    /// Use PREFIX instead of "PATCH" in the Subject: line.
    #[arg(long = "subject-prefix", value_name = "PREFIX")]
    pub subject_prefix: Option<String>,

    /// Generate a cover-letter template before the actual patches (named
    /// `0000-cover-letter<suffix>`, or `0` under `--numbered-files`).
    #[arg(long = "cover-letter", overrides_with = "no_cover_letter")]
    pub cover_letter: bool,

    /// Do not generate a cover letter (overrides `format.coverLetter` and the
    /// implicit cover letter requested by `--commit-list-format`).
    #[arg(long = "no-cover-letter", overrides_with = "cover_letter")]
    pub no_cover_letter: bool,

    /// Thread the series: bare `--thread` (or `--thread=shallow`) makes every
    /// patch a reply to the cover letter/first patch; `--thread=deep` chains
    /// each patch as a reply to its predecessor.
    #[arg(
        long = "thread",
        value_name = "STYLE",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "shallow",
        overrides_with = "no_thread"
    )]
    pub thread: Option<String>,

    /// Disable generated threading headers (an explicit `--in-reply-to` still
    /// adds the external parent to every mail).
    #[arg(long = "no-thread", overrides_with = "thread")]
    pub no_thread: bool,

    /// Make the first mail appear as a reply to the given Message-ID.
    #[arg(long = "in-reply-to", value_name = "MESSAGE_ID")]
    pub in_reply_to: Option<String>,

    /// Add a `To:` header with the given address (repeatable).
    #[arg(long = "to", value_name = "ADDRESS")]
    pub to: Vec<String>,

    /// Add a `Cc:` header with the given address (repeatable).
    #[arg(long = "cc", value_name = "ADDRESS")]
    pub cc: Vec<String>,

    /// Suppress all `To:` headers. Libra has no `format.to` config, so this
    /// simply omits the `To:` header (there is no config list to reset).
    #[arg(long = "no-to")]
    pub no_to: bool,

    /// Suppress all `Cc:` headers. Libra has no `format.cc` config, so this
    /// simply omits the `Cc:` header (there is no config list to reset).
    #[arg(long = "no-cc")]
    pub no_cc: bool,

    /// Use IDENT in the `From:` header (instead of the commit author). With no
    /// value, the committer's configured identity is used. When the identity
    /// differs from the commit author, the original author is preserved as an
    /// in-body `From:` line so `git am` can restore it.
    #[arg(
        long = "from",
        value_name = "IDENT",
        num_args = 0..=1,
        require_equals = true,
        overrides_with = "no_from"
    )]
    pub from: Option<Option<String>>,

    /// Do not rewrite the `From:` header (overrides `format.from`; last of
    /// `--from`/`--no-from` wins).
    #[arg(long = "no-from", overrides_with = "from")]
    pub no_from: bool,

    /// Mark the patch series as version N (changes `[PATCH]` to `[PATCH vN]`).
    #[arg(
        short = 'v',
        long = "reroll-count",
        value_name = "N",
        help = "Mark the patch series as version N (changes \"[PATCH]\" to \"[PATCH vN]\")"
    )]
    pub reroll_count: Option<usize>,

    /// Append a Signed-off-by trailer to each commit message.
    #[arg(short = 's', long = "signoff")]
    pub signoff: bool,

    /// Do not add a signoff, overriding `format.signOff` config.
    #[arg(long = "no-signoff", overrides_with = "signoff")]
    pub no_signoff: bool,

    /// Append each commit's notes after the `---` line. With no value the
    /// default notes ref (`refs/notes/commits`) is used; `--notes=<ref>` reads
    /// the given ref. Commits without a note are emitted unchanged.
    #[arg(long = "notes", value_name = "REF", num_args = 0..=1, require_equals = true)]
    pub notes: Option<Option<String>>,

    /// Record the base tree/commit the series applies to (Git's `--base`). Emits
    /// a `base-commit:` trailer (and `prerequisite-patch-id:` lines for commits
    /// between the base and the series) on the last patch, or the cover letter
    /// when `--cover-letter` is given. The base must be an ancestor of the series.
    #[arg(long = "base", value_name = "COMMIT")]
    pub base: Option<String>,

    /// Create multipart/mixed messages with the commit log in the body and the
    /// patch itself as a separate `attachment` part.
    #[arg(long = "attach", conflicts_with = "inline")]
    pub attach: bool,

    /// Like `--attach`, but the patch part uses `Content-Disposition: inline`.
    #[arg(long = "inline")]
    pub inline: bool,

    /// Show full object IDs in diff index header lines.
    #[arg(long = "full-index")]
    pub full_index: bool,

    /// Generate hunks with N lines of context (Git's `-U<n>` / `--unified=<n>`).
    #[arg(short = 'U', long = "unified", value_name = "N")]
    pub unified: Option<usize>,

    /// Show the full diff of each commit (accepted; without a pathspec Git's
    /// output is unchanged, and Libra accepts no `-- <path>` pathspec here).
    #[arg(long = "full-diff")]
    pub full_diff: bool,

    /// Only include changed paths matching this diff-filter (Git's
    /// `--diff-filter=<letters>`, e.g. `A` for added paths).
    #[arg(long = "diff-filter", value_name = "FILTER")]
    pub diff_filter: Option<String>,

    /// Spend extra time finding the smallest Myers diff. Libra's default Myers
    /// backend already guarantees a shortest edit script, so this is output-
    /// equivalent to the default.
    #[arg(long = "minimal")]
    pub minimal: bool,

    /// Generate patch hunks with the histogram diff algorithm.
    #[arg(long = "histogram")]
    pub histogram: bool,

    /// Detect renames at or above the given similarity (Git's `-M`); a bare
    /// `-M` keeps the 50% default and never swallows the revision
    #[arg(
        short = 'M',
        long = "find-renames",
        value_name = "N",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "50%",
        overrides_with = "no_renames"
    )]
    pub find_renames: Option<String>,

    /// Disable rename detection entirely (last of `-M`/`--no-renames` wins)
    #[arg(long = "no-renames", overrides_with = "find_renames")]
    pub no_renames: bool,

    /// Show a numstat block instead of the default diffstat and omit the `---`
    /// separator line
    #[arg(long = "numstat")]
    pub numstat: bool,

    /// Omit commits whose stable patch-id already exists on the excluded side
    /// of the revision range.
    #[arg(long = "ignore-if-in-upstream")]
    pub ignore_if_in_upstream: bool,

    /// Use PREFIX instead of `a/` for old-side paths in patch headers.
    #[arg(long = "src-prefix", value_name = "PREFIX")]
    pub src_prefix: Option<String>,

    /// Use PREFIX instead of `b/` for new-side paths in patch headers.
    #[arg(long = "dst-prefix", value_name = "PREFIX")]
    pub dst_prefix: Option<String>,

    /// Restrict the patch to a directory and show paths relative to it. With a
    /// value, `<path>` is resolved from the repository root (not the current
    /// directory); bare `--relative` uses the current directory.
    #[arg(
        long = "relative",
        value_name = "PATH",
        num_args = 0..=1,
        require_equals = true,
        overrides_with = "no_relative"
    )]
    pub relative: Option<Option<String>>,

    /// Keep repository-relative paths even when `diff.relative` is set.
    #[arg(long = "no-relative", overrides_with = "relative")]
    pub no_relative: bool,

    /// Git's revision-set `--all` selector is not implemented here; accepted
    /// so the refusal names the missing feature.
    #[arg(long = "all", hide = true)]
    pub all_refs: bool,

    /// Do not show the diffstat summary before the diff (Git's `-p` is the
    /// short form of `--no-stat` for `format-patch`).
    #[arg(short = 'p', long = "no-stat")]
    pub no_stat: bool,

    /// Keep the commit subject byte-identical: no `[PATCH]` prefix and no
    /// `n/m` counter is added (Git's `-k`/`--keep-subject`).
    #[arg(
        short = 'k',
        long = "keep-subject",
        help = "Keep the commit subject byte-identical: no [PATCH] prefix or n/m counter is added (Git's -k)"
    )]
    pub keep_subject: bool,

    /// Filename suffix for generated patches (default ".patch"); e.g. ".txt".
    /// Ignored under `--numbered-files` (which uses bare sequence numbers).
    #[arg(long = "suffix", value_name = "SFX")]
    pub suffix: Option<String>,

    #[cfg_attr(
        doc,
        doc = " Output an all-zero hash in each patch's \"From `<hash>`\" line instead of"
    )]
    #[cfg_attr(
        not(doc),
        doc = " Output an all-zero hash in each patch's \"From <hash>\" line instead of"
    )]
    /// the commit hash (stable output for testing/reproducibility).
    #[arg(long = "zero-commit")]
    pub zero_commit: bool,

    /// Signature placed after the `-- ` line of each patch (and the cover
    /// letter). Defaults to the libra version; `--no-signature` takes priority.
    #[arg(long = "signature", value_name = "SIGNATURE")]
    pub signature: Option<String>,

    /// Do not emit the `-- `/signature footer at all.
    #[arg(long = "no-signature")]
    pub no_signature: bool,

    /// Read the signature footer text from a file (mutually exclusive with
    /// `--signature`). `--no-signature` still takes priority.
    #[arg(
        long = "signature-file",
        value_name = "FILE",
        conflicts_with = "signature"
    )]
    pub signature_file: Option<String>,

    /// RFC 2047 Q-encode `From`/`Subject` header values that contain non-ASCII
    /// characters (Git's `--encode-email-headers`). Off by default.
    #[arg(
        long = "encode-email-headers",
        overrides_with = "no_encode_email_headers"
    )]
    pub encode_email_headers: bool,

    /// Disable header encoding (the default); negates `--encode-email-headers`.
    #[arg(
        long = "no-encode-email-headers",
        overrides_with = "encode_email_headers"
    )]
    pub no_encode_email_headers: bool,

    /// Name output files by a plain sequence number (1, 2, …) instead of
    /// `NNNN-subject`; the `--suffix` is not applied in this mode (matches Git).
    #[arg(long = "numbered-files")]
    pub numbered_files: bool,
}

// ---------------------------------------------------------------------------
// Error enum
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
enum FormatPatchError {
    #[error("not a libra repository (or any parent)")]
    NotInRepo,

    #[error("{0}")]
    InvalidTarget(String),

    #[error("identity unknown: {0}")]
    IdentityMissing(String),

    #[error("invalid ident line: {0}")]
    InvalidFromIdent(String),

    #[error("failed to write patch file '{}': {detail}", .path.display())]
    OutputWrite { path: PathBuf, detail: String },

    #[error("failed to create output directory '{}': {detail}", .dir)]
    OutputDirCreate { dir: String, detail: String },

    #[error("failed to read config '{key}': {detail}")]
    ConfigRead { key: &'static str, detail: String },

    #[error("bad config value '{value}' for '{key}' (expected {expected})")]
    InvalidConfig {
        key: &'static str,
        value: String,
        expected: &'static str,
    },
}

impl From<FormatPatchError> for CliError {
    fn from(err: FormatPatchError) -> Self {
        match &err {
            FormatPatchError::NotInRepo => CliError::repo_not_found(),
            FormatPatchError::InvalidTarget(_) => CliError::failure(err.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget),
            FormatPatchError::InvalidFromIdent(_) => CliError::failure(err.to_string())
                .with_stable_code(StableErrorCode::CliInvalidArguments),
            FormatPatchError::IdentityMissing(_) => CliError::fatal(err.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint(
                    "run 'libra config user.name \"Your Name\"' and \
                         'libra config user.email \"you@example.com\"'",
                ),
            FormatPatchError::OutputWrite { .. } => {
                CliError::fatal(err.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            FormatPatchError::OutputDirCreate { .. } => {
                CliError::fatal(err.to_string()).with_stable_code(StableErrorCode::IoWriteFailed)
            }
            FormatPatchError::ConfigRead { .. } => {
                CliError::fatal(err.to_string()).with_stable_code(StableErrorCode::IoReadFailed)
            }
            FormatPatchError::InvalidConfig { .. } => CliError::failure(err.to_string())
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        }
    }
}

// ---------------------------------------------------------------------------
// Public entry points
// ---------------------------------------------------------------------------

/// Convenience wrapper for callers that do not need structured output.
pub async fn execute(args: FormatPatchArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
    }
}

/// Reject CLI-only conflicts before repository preflight can write repair state.
pub(crate) fn validate_cli_args(args: &FormatPatchArgs) -> CliResult<()> {
    // Check CLI-only conflicts before config fills subject_prefix. A configured
    // prefix is harmless under -k; an explicit (even empty) CLI prefix is not.
    if args.keep_subject {
        let conflict = if args.numbered {
            Some("options '-n' and '-k' cannot be used together")
        } else if args.subject_prefix.is_some()
            || args.rfc.as_ref().is_some_and(|marker| !marker.is_empty())
        {
            Some("options '--subject-prefix/--rfc' and '-k' cannot be used together")
        } else {
            None
        };
        if let Some(message) = conflict {
            return Err(
                CliError::fatal(message).with_stable_code(StableErrorCode::CliInvalidArguments)
            );
        }
    }

    Ok(())
}

/// # Side Effects
/// - Reads commit, tree, and blob objects from the object store.
/// - When `--stdout` is not set, creates patch files (suffix from `--suffix`,
///   default `.patch`; bare sequence numbers under `--numbered-files`) in the
///   output
///   directory (or the current working directory).  The working tree is
///   **not** modified.
///
/// # Errors
/// - `CliInvalidTarget` when the revision range resolves to zero commits or
///   a specified reference does not exist.
/// - `IoWriteFailed` when a patch file cannot be written or the output
///   directory cannot be created.
/// - Errors from lower-level object loading are forwarded as `CliError`.
pub async fn execute_safe(mut args: FormatPatchArgs, output: &OutputConfig) -> CliResult<()> {
    // 1. Ensure we are in a repo
    if util::require_repo().is_err() {
        return Err(CliError::from(FormatPatchError::NotInRepo));
    }

    validate_cli_args(&args)?;
    // CLI `--cover-letter`/`--no-cover-letter` override both the config and the
    // implicit cover letter requested by `--commit-list-format`.
    let cover_cli = args.cover_letter;
    let no_cover_cli = args.no_cover_letter;
    // Git validates `--from=<ident>` before revision parsing and the empty-series
    // check, so a malformed identity wins over an empty range.
    if let Some(Some(ident)) = args.from.as_ref()
        && !ident.trim().is_empty()
        && parse_from_ident(ident).is_err()
    {
        return Err(CliError::from(FormatPatchError::InvalidFromIdent(
            ident.clone(),
        )));
    }
    // A malformed `-M` threshold is a usage error before any patch is written.
    if let Some(raw) = args.find_renames.as_deref()
        && crate::command::diff::options::parse_rename_score(raw).is_err()
    {
        return Err(
            CliError::fatal(format!("invalid argument to find-renames: '{raw}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }

    let from_from_cli = args.from.is_some() || args.no_from;
    resolve_format_config(&mut args).await?;
    if !from_from_cli
        && let Some(Some(ident)) = args.from.as_ref()
        && !ident.trim().is_empty()
        && parse_from_ident(ident).is_err()
    {
        return Err(CliError::from(FormatPatchError::InvalidFromIdent(
            ident.clone(),
        )));
    }

    // `--signature-file` reads the footer text from a file; resolve it into the
    // same slot `--signature` uses (the two are mutually exclusive). Trailing
    // newlines are trimmed so the footer is rendered consistently. Skip the read
    // entirely under `--no-signature`, which suppresses the footer regardless.
    if !args.no_signature
        && let Some(path) = args.signature_file.clone()
    {
        let content = std::fs::read_to_string(&path).map_err(|e| {
            CliError::failure(format!("failed to read signature file '{path}': {e}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        args.signature = Some(content.trim_end_matches('\n').to_string());
    }

    // 2. Resolve revision range
    let commits = resolve_range_commits(&args).await?;
    if !cover_cli
        && !no_cover_cli
        && let Some(value) = read_format_config("format.coverLetter").await?
    {
        let trimmed = value.trim();
        args.cover_letter = if trimmed.eq_ignore_ascii_case("auto") {
            commits.len() > 1
        } else {
            crate::internal::config::parse_git_config_bool(trimmed).ok_or(
                FormatPatchError::InvalidConfig {
                    key: "format.coverLetter",
                    value,
                    expected: "true, false, or auto",
                },
            )?
        };
    }
    if !no_cover_cli && args.commit_list_format.is_some() {
        // `--commit-list-format` implies a cover letter (Git's `#909`).
        args.cover_letter = true;
    }
    if args.all_refs {
        return Err(CliError::failure(
            "format-patch --all is not supported: Libra's revision grammar does not expose the all-refs selector",
        )
        .with_stable_code(StableErrorCode::Unsupported)
        .with_hint("name the branches explicitly, for example `libra format-patch main..topic`"));
    }

    // An empty series is not an error: Git exits 0 and prints nothing (only an
    // unresolvable revision or a repository with no commits is fatal), so
    // `format-patch` with no upstream, `A..A`, and `--ignore-if-in-upstream`
    // with everything upstream all succeed silently.
    if commits.is_empty() {
        if output.is_json() {
            crate::utils::output::emit_json_data(
                "format-patch",
                &FormatPatchOutput {
                    patches: Vec::new(),
                },
                output,
            )?;
        }
        return Ok(());
    }

    // `--base`: compute the base-commit / prerequisite-patch-id trailer. It is
    // attached to the cover letter when present, otherwise to the last patch.
    let base_block = build_base_block(&args, &commits).await?;

    // 3. Build thread Message-ID (used for --thread / --in-reply-to)
    let thread_id = build_thread_id(&args, &commits)?;

    // Resolve the `--from` identity once (shared across all patches).
    let from_identity = resolve_from_identity(&args).await?;

    // 4. Determine numbering
    let total = commits.len();
    let start_num = args.start_number;

    // 5. Render the complete series before creating any patch file. A later
    // object/config error therefore cannot leave a partially generated series.
    let mut rendered = Vec::with_capacity(total + usize::from(args.cover_letter));
    if args.cover_letter {
        let series_stat = cover_letter_series_stat(&args, &commits).await?;
        let cover_body = format_cover_letter(
            &args,
            &commits,
            &thread_id,
            from_identity.as_ref(),
            base_block.as_deref(),
            series_stat.as_deref(),
        )?;
        rendered.push(RenderedPatch {
            number: 0,
            commit: "0000000000000000000000000000000000000000".to_string(),
            subject: "*** SUBJECT HERE ***".to_string(),
            slug: String::new(),
            body: cover_body,
        });
    }

    // 6. Render each commit patch.
    for (idx, commit) in commits.iter().enumerate() {
        let patch_num = start_num + idx;
        // Without a cover letter, the base trailer rides on the last patch.
        let patch_base_block = if !args.cover_letter && idx == total - 1 {
            base_block.as_deref()
        } else {
            None
        };
        let patch_body = format_patch_body(
            &args,
            commit,
            patch_num,
            total,
            start_num,
            &thread_id,
            from_identity.as_ref(),
            patch_base_block,
        )
        .await?;
        let slug = patch_slug(commit, &args);
        rendered.push(RenderedPatch {
            number: patch_num,
            commit: commit.id.to_string(),
            subject: commit_subject_line(commit),
            slug,
            body: patch_body,
        });
    }

    // 7. Persist only after every message has rendered successfully. Each file
    // uses temp+rename atomicity; stdout uses the shared BrokenPipe policy.
    let out_dir = resolve_output_dir(&args)?;
    let mut records = Vec::with_capacity(rendered.len());
    for patch in rendered {
        let path = write_patch_file(
            &args,
            &out_dir,
            patch.number,
            total,
            start_num,
            &patch.slug,
            &patch.body,
        )?;
        if !output.quiet {
            eprintln!("{}", path.display());
        }
        records.push(PatchRecord {
            number: patch.number,
            commit: patch.commit,
            subject: patch.subject,
            path: path.display().to_string(),
        });
    }

    // 8. Emit JSON if requested
    if output.is_json() {
        let envelope = FormatPatchOutput { patches: records };
        crate::utils::output::emit_json_data("format-patch", &envelope, output)?;
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Structured output types
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct FormatPatchOutput {
    patches: Vec<PatchRecord>,
}

#[derive(Serialize)]
struct PatchRecord {
    number: usize,
    commit: String,
    subject: String,
    path: String,
}

struct RenderedPatch {
    number: usize,
    commit: String,
    subject: String,
    slug: String,
    body: String,
}

// ---------------------------------------------------------------------------
// Revision range resolution
// ---------------------------------------------------------------------------

/// Resolve `A..B` or a single revision into the ordered list of non-merge
/// commits to export (oldest first).  The excluded side (`A`) is reachable
/// from the included side (`B`); commits equal to or reachable from `A` are
/// stripped.
async fn resolve_range_commits(args: &FormatPatchArgs) -> Result<Vec<Commit>, CliError> {
    let spec = args.revision_range.as_deref().unwrap_or("HEAD");

    // Parse A..B notation
    let (exclude_tip_opt, include_tip) = if let Some((left, right)) = spec.split_once("..") {
        // Both empty sides default to HEAD (Git-compatible: ..HEAD equals
        // HEAD..HEAD producing zero patches).
        let left_spec = if left.is_empty() { "HEAD" } else { left };
        let right_spec = if right.is_empty() { "HEAD" } else { right };
        (
            Some(resolve_single_rev(left_spec).await?),
            resolve_single_rev(right_spec).await?,
        )
    } else if args.root || args.last_one || args.last_n.is_some() {
        // `--root [REV]` walks from REV through the root. `-1 [REV]` names the
        // one commit to emit rather than retaining the usual `<REV>..HEAD`
        // single-revision interpretation.
        (None, resolve_single_rev(spec).await?)
    } else {
        // Single revision: range is <spec>..HEAD
        let head = resolve_current_head().await?;
        (Some(resolve_single_rev(spec).await?), head)
    };

    // Collect the set of excluded OIDs (the exclude tip and all its ancestors)
    let (excluded, upstream_commits): (HashSet<ObjectHash>, Vec<Commit>) =
        if let Some(exclude_tip) = exclude_tip_opt {
            let ancestors = log::get_reachable_commits(exclude_tip.to_string(), None)
                .await
                .map_err(|error| {
                    FormatPatchError::InvalidTarget(format!(
                        "failed to walk excluded commits from '{spec}': {error}"
                    ))
                })?;
            let mut s = HashSet::new();
            s.insert(exclude_tip);
            for commit in &ancestors {
                s.insert(commit.id);
            }
            (s, ancestors)
        } else {
            (HashSet::new(), Vec::new())
        };

    // Walk from the included tip.  get_reachable_commits performs a BFS
    // traversal, returning commits in newest-first order (tip, then
    // parents, then grandparents, …).
    let all_reachable = log::get_reachable_commits(include_tip.to_string(), None)
        .await
        .map_err(|e| {
            FormatPatchError::InvalidTarget(format!("failed to walk commits from '{spec}': {e}"))
        })?;

    // Filter, deduplicate, then reverse so patches are numbered oldest-first.
    let mut commits: Vec<Commit> = all_reachable
        .into_iter()
        .filter(|c| !excluded.contains(&c.id))
        .filter(|c| c.parent_commit_ids.len() <= 1) // skip merge commits
        .collect();

    // Deduplicate (reachable set may already include the tip).
    let mut seen = HashSet::new();
    commits.retain(|c| seen.insert(c.id));

    // `-1` / `-<n>` limit the newest-first walk before output ordering is
    // reversed (`-<n>` is Git's "last n commits" shorthand).
    if args.last_one {
        commits.truncate(1);
    } else if let Some(limit) = args.last_n {
        commits.truncate(limit);
    }

    if args.ignore_if_in_upstream && !upstream_commits.is_empty() {
        let mut upstream_patch_ids = HashSet::new();
        for commit in upstream_commits
            .iter()
            .filter(|commit| commit.parent_commit_ids.len() <= 1)
        {
            upstream_patch_ids.insert(git_patch_id_for_commit(commit).await?);
        }
        let mut retained = Vec::with_capacity(commits.len());
        for commit in commits {
            if !upstream_patch_ids.contains(&git_patch_id_for_commit(&commit).await?) {
                retained.push(commit);
            }
        }
        commits = retained;
    }

    // BFS returns newest-first; reverse gives oldest-first for linear history.
    commits.reverse();

    Ok(commits)
}

/// Resolve a single revision expression to an [`ObjectHash`].
async fn resolve_single_rev(spec: &str) -> Result<ObjectHash, CliError> {
    util::get_commit_base_typed(spec).await.map_err(|e| {
        FormatPatchError::InvalidTarget(format!("cannot resolve '{spec}': {e}")).into()
    })
}

/// Return the OID of `HEAD`.
async fn resolve_current_head() -> Result<ObjectHash, CliError> {
    use crate::internal::head::Head;
    Head::current_commit().await.ok_or_else(|| {
        FormatPatchError::InvalidTarget(
            "HEAD does not point to a commit (unborn branch?)".to_string(),
        )
        .into()
    })
}

// ---------------------------------------------------------------------------
// Patch body formatting
// ---------------------------------------------------------------------------

/// Assemble the complete mbox body for a single patch.
#[allow(clippy::too_many_arguments)]
async fn format_patch_body(
    args: &FormatPatchArgs,
    commit: &Commit,
    patch_num: usize,
    total: usize,
    start_num: usize,
    threading: &Option<Threading>,
    from_identity: Option<&(String, String)>,
    base_block: Option<&str>,
) -> Result<String, CliError> {
    let mut out = String::new();

    // ---- "From " mbox envelope ----
    let ts = timestamp_from_commit(commit);
    // Git's fixed mbox separator timestamp (`builtin/log.c`).
    const MBOX_FIXED_DATE: &str = "Mon Sep 17 00:00:00 2001";
    // `--zero-commit` zeroes only this envelope hash (matching its hex length),
    // leaving the rest of the patch untouched, like `git format-patch`.
    let envelope_hash = if args.zero_commit {
        "0".repeat(commit.id.to_string().len())
    } else {
        commit.id.to_string()
    };
    out.push_str(&format!("From {} {MBOX_FIXED_DATE}\n", envelope_hash));

    // ---- Threading headers (Git emits them before `From:`) ----
    push_threading_headers(
        &mut out,
        threading,
        patch_num,
        start_num,
        total,
        args.cover_letter,
    );

    // ---- From: ----
    // The header shows the `--from` identity when given (else the commit
    // author). If it differs from the author, the original author is preserved
    // as an in-body `From:` line (added after the headers, below).
    let author_name = sanitize_header_value(commit.author.name.trim());
    let author_email = sanitize_header_value(commit.author.email.trim());
    let (header_name, header_email) = match from_identity {
        Some((name, email)) => (
            sanitize_header_value(name.trim()),
            sanitize_header_value(email.trim()),
        ),
        None => (author_name.clone(), author_email.clone()),
    };
    let in_body_from = from_identity
        .map(|_| header_name != author_name || header_email != author_email)
        .unwrap_or(false);
    let header_name_enc = encode_email_header(&header_name, args.encode_email_headers);
    out.push_str(&format!("From: {header_name_enc} <{header_email}>\n"));

    // ---- Date: (RFC 2822) ----
    out.push_str(&format!("Date: {}\n", ts.to_rfc2822()));

    // ---- Subject: ----
    let (raw_msg, _sig) = parse_commit_msg(&commit.message);
    let subject = raw_msg.lines().next().unwrap_or("").to_string();
    let subject = sanitize_header_value(&clean_subject(&subject, args.keep_subject));
    let subject = encode_email_header(&subject, args.encode_email_headers);

    let version = args
        .reroll_count
        .map(|v| format!(" v{v}"))
        .unwrap_or_default();
    // `--rfc[=<text>]` prepends the RFC marker to the subject prefix
    // (`[PATCH]` → `[RFC PATCH]`, or `[WIP PATCH]` for `--rfc=WIP`).
    let rfc = args
        .rfc
        .as_deref()
        .filter(|_| !args.no_rfc)
        .map(|text| format!("{text} "))
        .unwrap_or_default();
    let prefix = format!(
        "{rfc}{prefix}{version}",
        prefix = sanitize_header_value(subject_prefix(args))
    );

    let numbering_enabled = !args.no_numbered;
    let num_str = if numbering_enabled && (args.numbered || args.cover_letter) {
        let displayed_total = total + start_num - 1;
        let width = subject_counter_width(displayed_total);
        format!(" {:0width$}/{displayed_total}", patch_num)
    } else {
        String::new()
    };

    if args.keep_subject {
        // `-k`/`--keep-subject` keeps the subject byte-identical and adds no
        // `[PATCH]` prefix or `n/m` counter (Git's contract).
        out.push_str(&format!("Subject: {subject}\n"));
    } else {
        out.push_str(&format!("Subject: [{prefix}{num_str}] {subject}\n"));
    }

    // `--attach`/`--inline` wrap the patch in a MIME multipart: the log message
    // + diffstat go in a `text/plain` part, the diff in a `text/x-patch` part.
    let attach_disposition = if args.attach {
        Some("attachment")
    } else if args.inline {
        Some("inline")
    } else {
        None
    };
    // Boundary mirrors Git's: twelve dashes + the tool version string.
    let boundary = format!("------------libra {}", env!("CARGO_PKG_VERSION"));

    // ---- MIME headers ----
    // Git puts only the `Content-Type` at the multipart envelope level; the
    // `Content-Transfer-Encoding` lives on each MIME part (emitted below). For a
    // plain (non-attach) patch the transfer-encoding stays at the top level.
    out.push_str("MIME-Version: 1.0\n");
    if attach_disposition.is_some() {
        out.push_str(&format!(
            "Content-Type: multipart/mixed; boundary=\"{boundary}\"\n"
        ));
    } else {
        out.push_str("Content-Type: text/plain; charset=UTF-8\n");
        out.push_str("Content-Transfer-Encoding: 8bit\n");
    }

    // ---- To: / Cc: (after the MIME block, matching Git's header order) ----
    let (to, cc) = resolve_recipients(args);
    push_recipient_headers(&mut out, &to, &cc);

    // ---- Blank line: headers -> body ----
    out.push('\n');

    // ---- Message section: in-body `From:`, commit body, Signed-off-by, the
    // `---` separator, notes, and the diffstat (everything above the diff). ----
    let mut message_section = String::new();
    if in_body_from {
        message_section.push_str(&format!("From: {author_name} <{author_email}>\n\n"));
    }
    let body = raw_msg
        .lines()
        .skip(1) // skip the subject line
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if !body.is_empty() {
        message_section.push_str(&body);
        message_section.push('\n');
    }
    if args.signoff {
        let (name, email) = resolve_signoff_identity().await?;
        message_section.push_str(&format!("Signed-off-by: {name} <{email}>\n"));
    }
    let notes_block = render_notes_block(&args.notes, &commit.id.to_string()).await?;
    // The body and diffstat come from the shared `libra diff` engine so rename
    // detection (and `diff.renames`) applies by default (ADR-FP-03).
    let (stat_text, numstat_text, diff) = diff_engine_patch_parts(commit, args).await?;
    let has_numstat = args.numstat && !numstat_text.is_empty();
    let has_stat = !args.no_stat && !stat_text.is_empty();
    let has_diff = !diff.is_empty();
    // Git emits the `---` separator only when there is something after it; an
    // empty view (`--relative` with no matching files) ends after the message.
    if notes_block.is_some() || (!has_numstat && (has_stat || has_diff)) {
        message_section.push_str("---\n");
        if let Some(block) = notes_block {
            message_section.push_str(&block);
        }
    }
    if has_numstat {
        // `--numstat` replaces the stat block, drops the `---` separator, and is
        // not suppressed by `-p`/`--no-stat` (its own flag is the only switch).
        message_section.push_str(&numstat_text);
        message_section.push('\n');
    } else if has_stat {
        message_section.push_str(&stat_text);
        message_section.push('\n');
        // Git separates the diffstat from the unified diff with a blank line.
        if has_diff {
            message_section.push('\n');
        }
    }

    if let Some(disposition) = attach_disposition {
        // MIME multipart assembly (byte-compatible with `git format-patch --attach`).
        let slug = patch_slug(commit, args);
        let filename = patch_filename(&FilenameSpec {
            patch_num,
            total,
            start_num,
            slug: &slug,
            suffix: suffix(args),
            numbered_files: args.numbered_files,
            max_length: args.filename_max_length,
        });
        out.push_str("This is a multi-part message in MIME format.\n");
        out.push_str(&format!("--{boundary}\n"));
        out.push_str("Content-Type: text/plain; charset=UTF-8; format=fixed\n");
        out.push_str("Content-Transfer-Encoding: 8bit\n\n");
        out.push('\n');
        // Normalize the message-part's trailing newlines so exactly two blank
        // lines separate it from the next boundary (matching Git), regardless of
        // how many trailing newlines the diffstat left.
        out.push_str(message_section.trim_end_matches('\n'));
        out.push_str(&format!("\n\n\n--{boundary}\n"));
        out.push_str(&format!(
            "Content-Type: text/x-patch; name=\"{filename}\"\n"
        ));
        out.push_str("Content-Transfer-Encoding: 8bit\n");
        out.push_str(&format!(
            "Content-Disposition: {disposition}; filename=\"{filename}\"\n\n"
        ));
        out.push_str(&diff);
        // The `--base` trailer (last patch only) goes inside the patch part,
        // after the diff and before the closing boundary, matching Git.
        if let Some(block) = base_block {
            out.push('\n');
            out.push_str(block);
        }
        out.push_str(&format!("\n--{boundary}--\n"));
    } else {
        out.push_str(&message_section);
        out.push_str(&diff);
        // `--base` trailer (last patch only): a blank line, then the
        // base-commit / prerequisite-patch-id block, before the signature.
        if let Some(block) = base_block {
            out.push('\n');
            out.push_str(block);
        }
        push_signature(&mut out, args);
        // Git ends a patch message with exactly one newline; the diff engine's
        // trailing blank line is not part of the mailable patch.
        while out.ends_with("\n\n") {
            out.pop();
        }
        if !out.ends_with('\n') {
            out.push('\n');
        }
    }

    Ok(out)
}

/// Render the `--notes` block for one commit, or `None` when notes are not
/// requested or the commit has no note on the resolved ref.
///
/// Matches Git's format-patch output exactly: a blank line, a `Notes:` header
/// (or `Notes (<short-ref>):` for any ref other than the default
/// `refs/notes/commits`), each note line indented by four spaces (blank lines
/// become the bare indent, as Git does), then a trailing blank line. The caller
/// emits this immediately after the `---` separator and before the diffstat.
async fn render_notes_block(
    notes_arg: &Option<Option<String>>,
    commit_id: &str,
) -> Result<Option<String>, CliError> {
    // `--notes` not given → nothing; `--notes` (no value) → default ref.
    let Some(opt) = notes_arg else {
        return Ok(None);
    };
    let raw_ref = opt.as_deref().unwrap_or("commits");
    let notes_ref = notes::normalize_notes_ref(raw_ref)
        .map_err(|e| CliError::command_usage(format!("invalid --notes ref: {e}")))?;

    // `normalize_notes_ref` only checks the `refs/notes/` prefix, so a malformed
    // value (`--notes=`, `--notes='bad ref'`, `--notes=bad..ref`,
    // `--notes=refs/notes/.hidden`, …) would expand to an invalid ref and then
    // read as `NotFound`, silently producing an ordinary patch. Reject anything
    // that fails Git's `check-ref-format` rules as a usage error instead.
    if !util::is_valid_refname(&notes_ref) {
        return Err(CliError::command_usage(format!(
            "invalid --notes ref '{raw_ref}'"
        )));
    }

    match notes::show(&notes_ref, Some(commit_id)).await {
        Ok((_, _, text)) => {
            let short = notes_ref.strip_prefix("refs/notes/").unwrap_or(&notes_ref);
            let header = if short == "commits" {
                "Notes:".to_string()
            } else {
                format!("Notes ({short}):")
            };
            // Git trims the note's trailing newline, then indents every line
            // (including blanks) by four spaces.
            let indented = text
                .trim_end_matches('\n')
                .split('\n')
                .map(|line| format!("    {line}"))
                .collect::<Vec<_>>()
                .join("\n");
            Ok(Some(format!("\n{header}\n{indented}\n\n")))
        }
        // No note for this commit (or the ref is empty) → emit nothing, like Git.
        Err(notes::NotesError::NotFound { .. }) => Ok(None),
        Err(e) => Err(CliError::fatal(format!("failed to read notes: {e}"))),
    }
}

/// Append the patch signature footer (`-- \n<sig>\n`), mirroring Git:
/// `--no-signature` omits it entirely; `--signature <s>` sets the text;
/// otherwise the libra version is used (Git uses its own version here).
fn push_signature(out: &mut String, args: &FormatPatchArgs) {
    if args.no_signature {
        return;
    }
    out.push_str("-- \n");
    match &args.signature {
        Some(sig) => out.push_str(&format!("{sig}\n")),
        None => out.push_str(&format!("{}\n", env!("CARGO_PKG_VERSION"))),
    }
}

// ---------------------------------------------------------------------------
// `--base`: base-commit / prerequisite-patch-id trailer
// ---------------------------------------------------------------------------

/// Hash a byte buffer with the repository's object hash algorithm, returning the
/// raw digest bytes (20 for SHA-1, 32 for SHA-256).
fn patch_id_digest(kind: HashKind, bytes: &[u8]) -> Vec<u8> {
    use sha1::Digest as _;
    match kind {
        HashKind::Sha1 => sha1::Sha1::digest(bytes).to_vec(),
        HashKind::Sha256 => {
            use sha2::Digest as _;
            sha2::Sha256::digest(bytes).to_vec()
        }
        HashKind::Blake3 => {
            let mut hasher = git_internal::utils::HashAlgorithm::new_for_kind(HashKind::Blake3);
            hasher.update(bytes);
            hasher.finalize_object_hash().as_ref().to_vec()
        }
    }
}

/// `git`'s `remove_space`: drop every ASCII whitespace byte (space, tab, the
/// vertical-tab/form-feed/carriage-return controls, and newline) from a line, so
/// the patch-id is whitespace-insensitive exactly like Git's default.
fn strip_whitespace(line: &str) -> Vec<u8> {
    line.bytes()
        .filter(|b| !matches!(b, b' ' | b'\t' | b'\n' | b'\x0b' | b'\x0c' | b'\r'))
        .collect()
}

/// Parse a hunk header `@@ -a,b +c,d @@` into the old/new line counts (`b`/`d`),
/// defaulting an omitted count to 1 (`@@ -a +c @@`). Matches Git's
/// `scan_hunk_header`, which uses only the counts (not the line numbers) so the
/// patch-id is independent of where the hunk sits in the file.
fn scan_hunk_header(line: &str) -> (i64, i64) {
    let count = |seg: &str| -> i64 {
        seg.split_once(',')
            .map(|(_, c)| c.parse::<i64>().unwrap_or(1))
            .unwrap_or(1)
    };
    let mut parts = line.trim_start_matches("@@ ").split_whitespace();
    let old = parts.next().unwrap_or("").trim_start_matches('-');
    let new = parts.next().unwrap_or("").trim_start_matches('+');
    (count(old), count(new))
}

/// Compute Git's (stable) patch-id for a single commit: the diff against its
/// first parent, canonicalised line-by-line (skip `index` lines, ignore hunk
/// line numbers, strip whitespace), hashed per-file and combined with a
/// byte-wise add-with-carry so the result is independent of file order.
/// Byte-for-byte compatible with `git patch-id --stable` / `format-patch`'s
/// `prerequisite-patch-id`.
async fn git_patch_id_for_commit(commit: &Commit) -> Result<String, CliError> {
    // NOTE: text diffs are byte-exact with `git patch-id --stable`. BINARY-file
    // prerequisites are a documented limitation: Libra's diff renderer emits a
    // bare `Binary files differ` marker (Git writes `Binary files a/X and b/X
    // differ`), and Git's format-patch even computes binary prerequisite ids via
    // an internal path that differs from its own `git patch-id`, so a binary
    // prerequisite's id is not guaranteed to match Git. Binary blobs in mailed
    // patch series are rare; the text case (the real-world use) is exact.
    let diff = log::generate_diff(commit, vec![]).await?;
    let kind = commit.id.kind();

    let mut files: Vec<Vec<u8>> = Vec::new();
    let mut cur: Vec<u8> = Vec::new();
    let mut before: i64 = -1;
    let mut after: i64 = -1;
    let mut patchlen = 0usize;

    for line in diff.split_inclusive('\n') {
        // Skip the leading commit comments until the first file header.
        if patchlen == 0 && !line.starts_with("diff ") {
            continue;
        }
        // Parsing the per-file header.
        if before == -1 {
            if line.starts_with("index ") {
                continue;
            } else if line.starts_with("--- ") {
                before = 1;
                after = 1;
            } else if !line.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
                break;
            }
        }
        // End of the current hunk: either the next hunk header or the next file.
        if before == 0 && after == 0 {
            if line.starts_with("@@ -") {
                (before, after) = scan_hunk_header(line);
                continue;
            }
            if !line.starts_with("diff ") {
                break;
            }
            // A new file: finalise the current file's bytes and reset.
            files.push(std::mem::take(&mut cur));
            before = -1;
            after = -1;
        }
        match line.as_bytes().first() {
            Some(b'-') => before -= 1,
            Some(b'+') => after -= 1,
            Some(b' ') => {
                before -= 1;
                after -= 1;
            }
            _ => {}
        }
        let chunk = strip_whitespace(line);
        patchlen += chunk.len();
        cur.extend_from_slice(&chunk);
    }
    if !cur.is_empty() {
        files.push(cur);
    }

    // Stable mode combines the per-file digests with a byte-wise addition and
    // carry (Git's `flush_one_hunk`), so the result is independent of file order
    // but — unlike a plain XOR — matches `git patch-id --stable` byte-for-byte on
    // multi-file diffs.
    let mut acc = vec![0u8; kind.size()];
    for file in &files {
        let digest = patch_id_digest(kind, file);
        let mut carry: u16 = 0;
        for (a, b) in acc.iter_mut().zip(digest) {
            carry += u16::from(*a) + u16::from(b);
            *a = (carry & 0xff) as u8;
            carry >>= 8;
        }
    }
    Ok(acc.iter().map(|b| format!("{b:02x}")).collect())
}

/// Build the `--base` trailer block (`base-commit:` plus any
/// `prerequisite-patch-id:` lines), or `None` when `--base` is not given.
///
/// The base must be an ancestor of the series; the prerequisites are the commits
/// between the base and the parent of the first patch, oldest-first — each
/// rendered as its Git patch-id. Returns a usage/fatal error for a non-ancestor
/// base or the unsupported `auto` form, matching Git's diagnostics.
async fn build_base_block(
    args: &FormatPatchArgs,
    commits: &[Commit],
) -> Result<Option<String>, CliError> {
    if args.no_base {
        return Ok(None);
    }
    let Some(base_spec) = args.base.as_deref() else {
        return Ok(None);
    };

    // `--base=auto` infers the base from the upstream of the range; that requires
    // upstream tracking Libra does not resolve here, so it is rejected (matching
    // Git, which also fails when it cannot determine the upstream).
    if base_spec == "auto" {
        return Err(CliError::command_usage(
            "--base=auto is not supported; specify the base commit explicitly with \
             --base=<commit>",
        ));
    }

    let base = resolve_single_rev(base_spec).await?;

    // The series applies on top of the first patch's parent.
    let Some(series_parent) = commits[0].parent_commit_ids.first().copied() else {
        return Err(CliError::fatal(
            "base commit should be the ancestor of revision list".to_string(),
        )
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    };

    // Commits reachable from the series parent (newest-first, inclusive).
    let reach_parent = log::get_reachable_commits(series_parent.to_string(), None)
        .await
        .map_err(|e| CliError::fatal(format!("failed to walk commits: {e}")))?;
    if !reach_parent.iter().any(|c| c.id == base) {
        return Err(CliError::fatal(
            "base commit should be the ancestor of revision list".to_string(),
        )
        .with_exit_code(128)
        .with_stable_code(StableErrorCode::CliInvalidTarget));
    }

    // Prerequisites = reachable(series_parent) \ reachable(base), oldest-first.
    let base_reach = log::get_reachable_commits(base.to_string(), None)
        .await
        .map_err(|e| CliError::fatal(format!("failed to walk commits: {e}")))?;
    let base_set: HashSet<ObjectHash> = base_reach.iter().map(|c| c.id).collect();
    let mut prereqs: Vec<&Commit> = reach_parent
        .iter()
        .filter(|c| !base_set.contains(&c.id))
        // Git prepares prerequisites with a non-merge walk (`max_parents = 1`),
        // so merge commits are not emitted as prerequisite patch-ids.
        .filter(|c| c.parent_commit_ids.len() <= 1)
        .collect();
    prereqs.reverse(); // newest-first -> oldest-first

    let mut block = format!("base-commit: {base}\n");
    for commit in prereqs {
        let id = git_patch_id_for_commit(commit).await?;
        block.push_str(&format!("prerequisite-patch-id: {id}\n"));
    }
    Ok(Some(block))
}

// ---------------------------------------------------------------------------
// Threading helpers
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ThreadMode {
    /// No generated Message-ID/threading; only an external parent may be added.
    None,
    Shallow,
    Deep,
}

#[derive(Clone, Debug)]
struct Threading {
    series_id: String,
    external_reply_to: Option<String>,
    mode: ThreadMode,
}

/// Build deterministic message IDs for the series and retain an optional
/// external parent separately. `--in-reply-to` is never reused as a generated
/// Message-ID, which avoids duplicate IDs and accidental `<<id>>` headers.
fn build_thread_id(
    args: &FormatPatchArgs,
    commits: &[Commit],
) -> Result<Option<Threading>, CliError> {
    let mode = if args.no_thread {
        ThreadMode::None
    } else {
        match args.thread.as_deref() {
            None => ThreadMode::None,
            Some("shallow") => ThreadMode::Shallow,
            Some("deep") => ThreadMode::Deep,
            Some(other) => {
                return Err(CliError::failure(format!(
                    "invalid --thread value '{other}' (expected shallow or deep)"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
        }
    };
    let external_reply_to = args.in_reply_to.as_deref().map(normalize_message_id);
    if mode == ThreadMode::None && external_reply_to.is_none() {
        return Ok(None);
    }
    Ok(commits.first().map(|c| {
        let ts = c.committer.timestamp;
        let id = c.id.to_string();
        let short = &id[..8.min(id.len())];
        let series_id = format!("{short}-{ts}@libra");
        Threading {
            series_id,
            external_reply_to,
            mode,
        }
    }))
}

fn normalize_message_id(value: &str) -> String {
    sanitize_header_value(value)
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_string()
}

/// Push `Message-ID`, `In-Reply-To`, and `References` headers.
fn push_threading_headers(
    out: &mut String,
    threading: &Option<Threading>,
    patch_num: usize,
    start_num: usize,
    total: usize,
    has_cover_letter: bool,
) {
    let Some(threading) = threading else {
        return;
    };
    if threading.mode == ThreadMode::None {
        // `--no-thread` keeps only the external parent, on every mail.
        if let Some(external) = threading.external_reply_to.as_deref() {
            out.push_str(&format!("In-Reply-To: <{external}>\n"));
            out.push_str(&format!("References: <{external}>\n"));
        }
        return;
    }
    let is_first = patch_num == start_num;
    // Every patch ID carries a fixed-width numeric suffix, and the cover uses
    // `-cover`, so no generated value is a substring of another (the old bare
    // series id was a prefix of `-p2`/`-cover`).
    let message_id = format!("{}-p{patch_num:08}", threading.series_id);
    let first_id = format!("{}-p{start_num:08}", threading.series_id);
    out.push_str(&format!("Message-ID: <{message_id}>\n"));

    let cover_id = format!("{}-cover", threading.series_id);
    let external = threading.external_reply_to.as_deref();
    let (reply_to, reference) = match threading.mode {
        ThreadMode::None => (None, None),
        ThreadMode::Shallow => {
            let reply_to = if has_cover_letter {
                Some(cover_id.clone())
            } else if let Some(external) = external {
                Some(external.to_string())
            } else if total > 1 && !is_first {
                Some(first_id.clone())
            } else {
                None
            };
            let reference = external
                .map(str::to_string)
                .or(if has_cover_letter {
                    Some(cover_id.clone())
                } else {
                    None
                })
                .or_else(|| reply_to.clone());
            (reply_to, reference)
        }
        ThreadMode::Deep => {
            let reply_to = if patch_num == start_num {
                if has_cover_letter {
                    Some(cover_id.clone())
                } else {
                    external.map(str::to_string)
                }
            } else {
                Some(format!("{}-p{:08}", threading.series_id, patch_num - 1))
            };
            // Git's deep mode chains In-Reply-To but keeps References at the
            // thread root (external parent, cover, or first patch).
            let root = external.map(str::to_string).or(if has_cover_letter {
                Some(cover_id)
            } else {
                Some(first_id)
            });
            (reply_to, root)
        }
    };
    if let Some(reply_to) = reply_to {
        out.push_str(&format!("In-Reply-To: <{reply_to}>\n"));
        if let Some(reference) = reference {
            out.push_str(&format!("References: <{reference}>\n"));
        }
    }
}

// ---------------------------------------------------------------------------
// Cover letter
// ---------------------------------------------------------------------------

/// Generate a cover-letter template (named `0000-cover-letter<suffix>`, or `0`
/// under `--numbered-files`).
fn format_cover_letter(
    args: &FormatPatchArgs,
    commits: &[Commit],
    threading: &Option<Threading>,
    from_identity: Option<&(String, String)>,
    base_block: Option<&str>,
    series_stat: Option<&str>,
) -> Result<String, CliError> {
    let now = Utc::now();

    let mut out = String::new();

    // "From " envelope: Git uses the series' last commit oid and the fixed
    // separator timestamp; `--zero-commit` zeroes that hash.
    const MBOX_FIXED_DATE: &str = "Mon Sep 17 00:00:00 2001";
    let envelope_hash = commits.last().map_or_else(
        || "0".repeat(40),
        |commit| {
            if args.zero_commit {
                "0".repeat(commit.id.to_string().len())
            } else {
                commit.id.to_string()
            }
        },
    );
    out.push_str(&format!("From {envelope_hash} {MBOX_FIXED_DATE}\n"));

    // Cover thread headers also follow the envelope, before `From:`.
    if let Some(threading) = threading {
        out.push_str(&format!("Message-ID: <{}-cover>\n", threading.series_id));
        if let Some(parent) = threading.external_reply_to.as_deref() {
            out.push_str(&format!("In-Reply-To: <{parent}>\n"));
            out.push_str(&format!("References: <{parent}>\n"));
        }
    }

    let version = args
        .reroll_count
        .map(|v| format!(" v{v}"))
        .unwrap_or_default();
    let prefix = format!("{}{}", sanitize_header_value(subject_prefix(args)), version);

    // `From:` shows the `--from` identity when given (the cover letter has no
    // author of its own, so the template's `From:` is otherwise left blank).
    match from_identity {
        Some((name, email)) => {
            let name = encode_email_header(
                &sanitize_header_value(name.trim()),
                args.encode_email_headers,
            );
            let email = sanitize_header_value(email.trim());
            out.push_str(&format!("From: {name} <{email}>\n"));
        }
        None => out.push_str("From: \n"),
    }
    out.push_str(&format!("Date: {}\n", now.to_rfc2822()));
    let counter = if args.no_numbered {
        String::new()
    } else {
        let displayed_total = commits.len() + args.start_number - 1;
        let width = subject_counter_width(displayed_total);
        format!(" {:0width$}/{displayed_total}", 0)
    };
    out.push_str(&format!(
        "Subject: [{prefix}{counter}] *** SUBJECT HERE ***\n"
    ));
    // Git's `make_cover_letter` emits MIME headers only when any commit's raw
    // object buffer (headers + message) contains non-ASCII bytes.
    if commits.iter().any(|commit| {
        commit
            .to_data()
            .map_or(true, |data| data.iter().any(|byte| !byte.is_ascii()))
    }) {
        out.push_str("MIME-Version: 1.0\n");
        out.push_str("Content-Type: text/plain; charset=UTF-8\n");
        out.push_str("Content-Transfer-Encoding: 8bit\n");
    }
    let (to, cc) = resolve_recipients(args);
    push_recipient_headers(&mut out, &to, &cc);
    out.push('\n');

    // Git's `prepare_cover_text` writes the subject placeholder only into the
    // `Subject:` header; the body starts with the blurb placeholder.
    out.push_str("*** BLURB HERE ***\n\n");

    // Commit list. Git's default is a shortlog grouped by author
    // (`Name (N):` + indented subjects); `--commit-list-format=modern` uses
    // `[i/N] subject` lines and a pretty-format string renders each commit.
    let commit_list = render_commit_list(args, commits);
    if let Some(invalid) = commit_list.strip_prefix("\u{0}invalid:") {
        return Err(CliError::command_usage(format!(
            "'{invalid}' is not a valid format string"
        )));
    }
    out.push_str(&commit_list);

    // Series diffstat: Git computes it from the unique boundary commit (the
    // oldest patch's parent) to the last patch, independent of `--no-stat`/`-p`
    // (those only affect the individual patches).
    if let Some(stat) = series_stat {
        out.push_str(stat);
        out.push('\n');
        // Git separates the diffstat from a following base trailer or
        // signature with a blank line.
        if !args.no_signature || base_block.is_some() {
            out.push('\n');
        }
    }

    // `--base` trailer rides on the cover letter when one is present (the blank
    // line above separates it from the shortlog), before the signature.
    if let Some(block) = base_block {
        out.push_str(block);
    }

    push_signature(&mut out, args);

    Ok(out)
}

/// Series diffstat for the cover letter: the tree difference from the unique
/// boundary commit (the oldest patch's parent) to the last patch. Git omits it
/// when the series starts at a root commit (no boundary) or has no unique one.
async fn cover_letter_series_stat(
    args: &FormatPatchArgs,
    commits: &[Commit],
) -> Result<Option<String>, CliError> {
    let Some(first) = commits.first() else {
        return Ok(None);
    };
    let Some(last) = commits.last() else {
        return Ok(None);
    };
    let [origin] = first.parent_commit_ids.as_slice() else {
        return Ok(None);
    };
    if commits.iter().any(|commit| commit.id == *origin) {
        return Ok(None);
    }
    let mut argv: Vec<String> = vec!["diff".to_string(), "--stat".to_string()];
    // The cover diffstat inherits the patch rename options (t4014 "cover-letter
    // inherits diff options").
    if args.no_renames {
        argv.push("--no-renames".to_string());
    } else if let Some(score) = &args.find_renames {
        argv.push(format!("--find-renames={score}"));
    }
    argv.push(origin.to_string());
    argv.push(last.id.to_string());
    let diff_args = crate::command::diff::DiffArgs::try_parse_from(&argv)
        .map_err(|error| CliError::command_usage(error.to_string()))?;
    let output = crate::command::diff::run_diff_for_service(diff_args).await?;
    let stat = crate::command::diff::render::format_diff_stat_output(&output);
    if stat.is_empty() {
        Ok(None)
    } else {
        Ok(Some(stat))
    }
}

// ---------------------------------------------------------------------------
// Diffstat (plain-text, no ANSI colours)
// ---------------------------------------------------------------------------

/// Render a plain-text diffstat summary from [`log::FileStat`] records.
fn format_diffstat_plain(stats: &[log::FileStat]) -> String {
    const MAX_BAR: usize = 40;

    if stats.is_empty() {
        return String::new();
    }

    let mut out = String::new();
    let total_ins: usize = stats.iter().map(|s| s.insertions).sum();
    let total_del: usize = stats.iter().map(|s| s.deletions).sum();
    let total_files = stats.len();

    for stat in stats {
        let changes = stat.insertions + stat.deletions;
        let bar = if changes > MAX_BAR { MAX_BAR } else { changes };
        let plus = (stat.insertions * bar).checked_div(changes).unwrap_or(0);
        let minus = bar.saturating_sub(plus);

        out.push_str(&format!(
            " {} | {:>3} {}{}\n",
            stat.path,
            changes,
            "+".repeat(plus),
            "-".repeat(minus),
        ));
    }

    out.push_str(&format!(
        " {} file{} changed, {} insertion{}(+), {} deletion{}(-)\n",
        total_files,
        if total_files == 1 { "" } else { "s" },
        total_ins,
        if total_ins == 1 { "" } else { "s" },
        total_del,
        if total_del == 1 { "" } else { "s" },
    ));

    out
}

// ---------------------------------------------------------------------------
// Subject cleaning
// ---------------------------------------------------------------------------

/// Extract the first line of the commit message (subject).
fn commit_subject_line(commit: &Commit) -> String {
    let (msg, _) = parse_commit_msg(&commit.message);
    msg.lines().next().unwrap_or("").to_string()
}

/// Clean a commit subject for use in the `Subject:` header.
///
/// Strips leading `[PATCH ...]` / `[RFC ...]` bracketed prefixes unless
/// `--keep-subject` is set, and trims whitespace.
fn clean_subject(subject: &str, keep: bool) -> String {
    let s = subject.trim();
    if keep {
        return s.to_string();
    }
    // Strip leading bracket group(s), e.g. "[PATCH v2 3/5]" or "[RFC]"
    let mut cleaned = s;
    while let Some(rest) = cleaned
        .strip_prefix('[')
        .and_then(|t| t.split_once(']'))
        .map(|(_, rest)| rest.trim_start())
    {
        cleaned = rest;
    }
    cleaned.to_string()
}

// ---------------------------------------------------------------------------
// File naming
// ---------------------------------------------------------------------------

/// Build a safe filename slug from the commit subject.
/// Render the cover letter's commit list the way Git does.
///
/// * default / `--commit-list-format=short`/`shortlog`: grouped by author, in
///   first-appearance order, subjects wrapped at 72 columns with a 4-space
///   continuation indent.
/// * `--commit-list-format=modern`: `[i/N] subject` per commit.
/// * anything else: a pretty-format string over a small placeholder set.
fn render_commit_list(args: &FormatPatchArgs, commits: &[Commit]) -> String {
    let format = args.commit_list_format.as_deref().unwrap_or("short");
    let mut out = String::new();
    match format {
        "short" | "shortlog" => {
            // Group consecutive first-appearance authors, like `git shortlog`.
            let mut groups: Vec<(String, Vec<&Commit>)> = Vec::new();
            for commit in commits {
                let name = commit.author.name.trim().to_string();
                match groups.iter_mut().find(|(author, _)| *author == name) {
                    Some((_, list)) => list.push(commit),
                    None => groups.push((name, vec![commit])),
                }
            }
            for (author, group) in groups {
                out.push_str(&format!("{author} ({}):\n", group.len()));
                for commit in group {
                    let subject = commit_subject_line(commit);
                    out.push_str(&wrap_shortlog_subject(&subject));
                }
                out.push('\n');
            }
        }
        "modern" => {
            let total = commits.len();
            for (index, commit) in commits.iter().enumerate() {
                out.push_str(&format!(
                    "[{}/{}] {}\n",
                    index + 1,
                    total,
                    commit_subject_line(commit)
                ));
            }
        }
        custom => {
            for commit in commits {
                match render_pretty_format_commit(custom, commit) {
                    Some(rendered) => out.push_str(&rendered),
                    // Git reports an unusable format string as a fatal error
                    // (`fatal: '<fmt>' is not a valid format string`).
                    None => return format!("\u{0}invalid:{custom}"),
                }
                out.push('\n');
            }
        }
    }
    out
}

/// Wrap a shortlog subject at 72 columns: 2-space indent on the first line and
/// a 4-space continuation indent, matching `git shortlog`'s default width.
fn wrap_shortlog_subject(subject: &str) -> String {
    const WIDTH: usize = 72;
    const FIRST_INDENT: &str = "  ";
    const NEXT_INDENT: &str = "    ";

    let mut lines = String::new();
    let mut current = String::from(FIRST_INDENT);
    let mut has_word = false;
    for word in subject.split_whitespace() {
        let extra = if has_word {
            1 + word.chars().count()
        } else {
            word.chars().count()
        };
        if has_word && current.chars().count() + extra > WIDTH {
            lines.push_str(current.trim_end());
            lines.push('\n');
            current = String::from(NEXT_INDENT);
            current.push_str(word);
        } else {
            if has_word {
                current.push(' ');
            }
            current.push_str(word);
        }
        has_word = true;
    }
    lines.push_str(current.trim_end());
    lines.push('\n');
    lines
}

/// Expand a small `git log --format` subset for `--commit-list-format`.
/// Unsupported conversions yield `None`, which the caller reports as a
/// Git-style `fatal: ... is not a valid format string` error.
fn render_pretty_format_commit(format: &str, commit: &Commit) -> Option<String> {
    let mut out = String::new();
    let mut chars = format.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '%' {
            out.push(c);
            continue;
        }
        match chars.next()? {
            'h' => out.push_str(&commit.id.to_string()[..7.min(commit.id.to_string().len())]),
            'H' => out.push_str(&commit.id.to_string()),
            's' => out.push_str(&commit_subject_line(commit)),
            'a' => match chars.next()? {
                'n' => out.push_str(commit.author.name.trim()),
                'e' => out.push_str(commit.author.email.trim()),
                _ => return None,
            },
            'n' => out.push('\n'),
            '%' => out.push('%'),
            _ => return None,
        }
    }
    Some(out)
}

fn patch_slug(commit: &Commit, _args: &FormatPatchArgs) -> String {
    let slug = commit_subject_line(commit)
        .to_lowercase()
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '-' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>();

    // Collapse consecutive dashes
    let slug = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    truncate_slug(&slug)
}

/// Truncate a slug to at most `MAX_CHARS` characters without splitting a
/// word segment.  Uses [`char`] iteration to stay on UTF-8 boundaries so
/// non-ASCII subjects never panic on a mid-codepoint slice.
fn truncate_slug(slug: &str) -> String {
    const MAX_CHARS: usize = 52;
    let prefix: String = slug.chars().take(MAX_CHARS).collect();
    if prefix.len() == slug.len() {
        return slug.to_string();
    }
    // Walk back from the end of `prefix` to the last `-`.
    let end = prefix.rfind('-').unwrap_or(prefix.len());
    prefix[..end].to_string()
}

/// Build the output file path for a single patch.
/// When `slug` is empty the file is treated as the cover letter.
/// The sequence number is always included so that commits with identical
/// subjects never overwrite each other (matching Git's default behaviour).
/// Filename inputs that travel together: the patch's position in the series
/// and the naming controls that reshape the stem.
struct FilenameSpec<'a> {
    patch_num: usize,
    total: usize,
    start_num: usize,
    slug: &'a str,
    suffix: &'a str,
    numbered_files: bool,
    max_length: Option<usize>,
}

fn patch_filename(spec: &FilenameSpec<'_>) -> String {
    let FilenameSpec {
        patch_num,
        total,
        start_num,
        slug,
        suffix,
        numbered_files,
        max_length,
    } = *spec;
    if numbered_files {
        // `--numbered-files`: bare sequence number, no slug and no suffix
        // (the cover letter, with patch_num 0, becomes "0"), matching Git.
        return patch_num.to_string();
    }
    if slug.is_empty() {
        // Cover letter (only the cover-letter code path passes an empty slug)
        format!("0000-cover-letter{suffix}")
    } else {
        let width = number_width(total + start_num - 1);
        let number = format!("{patch_num:0width$}");
        let stem = format!("{number}-{slug}");
        // `--filename-max-length` caps the whole file name, so the stem gets
        // `limit - suffix.len() - 1` bytes (Git reserves the extra byte) and
        // may collapse to just the sequence number.
        let stem = match max_length {
            Some(limit) => {
                let budget = limit.saturating_sub(suffix.len() + 1);
                if budget < number.len() {
                    number
                } else {
                    truncate_filename(&stem, budget)
                }
            }
            None => stem,
        };
        format!("{stem}{suffix}")
    }
}

/// Truncate a filename stem to `limit` bytes on a char boundary (Git cuts
/// mid-word and keeps a trailing `-` if that is where the budget ends).
fn truncate_filename(stem: &str, limit: usize) -> String {
    if stem.len() <= limit {
        return stem.to_string();
    }
    let mut end = limit;
    while end > 0 && !stem.is_char_boundary(end) {
        end -= 1;
    }
    stem[..end].to_string()
}

/// Number of decimal digits needed to represent `n`.
fn number_width(n: usize) -> usize {
    if n == 0 {
        4
    } else {
        (n as f64).log10().floor() as usize + 1
    }
    .max(4)
}

fn subject_counter_width(displayed_total: usize) -> usize {
    displayed_total.to_string().len()
}

// ---------------------------------------------------------------------------
// Output
// ---------------------------------------------------------------------------

/// Resolve the output directory: `--output-directory` value, or CWD.
/// Creates the directory if it does not already exist.
fn resolve_output_dir(args: &FormatPatchArgs) -> Result<PathBuf, CliError> {
    if let Some(dir) = &args.output_directory {
        let path = PathBuf::from(dir);
        std::fs::create_dir_all(&path).map_err(|e| FormatPatchError::OutputDirCreate {
            dir: dir.clone(),
            detail: e.to_string(),
        })?;
        Ok(path)
    } else {
        std::env::current_dir().map_err(|e| {
            FormatPatchError::OutputDirCreate {
                dir: "<current directory>".to_string(),
                detail: e.to_string(),
            }
            .into()
        })
    }
}

/// Write `body` to a patch file (suffix from `--suffix`, default `.patch`;
/// bare sequence number under `--numbered-files`) or stdout when `--stdout`
/// is set.
/// Returns the display path.
fn write_patch_file(
    args: &FormatPatchArgs,
    out_dir: &Path,
    patch_num: usize,
    total: usize,
    start_num: usize,
    slug: &str,
    body: &str,
) -> Result<PathBuf, CliError> {
    if args.stdout {
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(body.as_bytes())
            .map_err(|error| stdout_write_error("write format-patch output", error))?;
        // Git separates consecutive patches with one blank line but does not
        // append an extra newline after the final patch.
        let is_last = total > 0 && patch_num >= start_num + total - 1;
        if !is_last {
            stdout
                .write_all(b"\n")
                .map_err(|error| stdout_write_error("finish format-patch output", error))?;
        }
        Ok(PathBuf::from("<stdout>"))
    } else {
        let filename = patch_filename(&FilenameSpec {
            patch_num,
            total,
            start_num,
            slug,
            suffix: suffix(args),
            numbered_files: args.numbered_files,
            max_length: args.filename_max_length,
        });
        let path = out_dir.join(&filename);
        atomic_write::write_atomic(&path, body.as_bytes(), atomic_write::sync_data_enabled())
            .map_err(|error| FormatPatchError::OutputWrite {
                path: path.clone(),
                detail: error.to_string(),
            })?;
        Ok(path)
    }
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

/// Convert a commit's committer timestamp to a `DateTime<Utc>`.
fn timestamp_from_commit(commit: &Commit) -> DateTime<Utc> {
    DateTime::from_timestamp(commit.committer.timestamp as i64, 0).unwrap_or(DateTime::UNIX_EPOCH)
}

fn subject_prefix(args: &FormatPatchArgs) -> &str {
    args.subject_prefix
        .as_deref()
        .unwrap_or(DEFAULT_SUBJECT_PREFIX)
}

fn suffix(args: &FormatPatchArgs) -> &str {
    args.suffix.as_deref().unwrap_or(DEFAULT_SUFFIX)
}

/// Render a commit's diffstat and patch body through the `libra diff` engine.
///
/// Renames are detected by default and follow `diff.renames`; root commits keep
/// the previous generator because the engine has no empty-tree side.
async fn diff_engine_patch_parts(
    commit: &Commit,
    args: &FormatPatchArgs,
) -> Result<(String, String, String), CliError> {
    if relative_prefix(args)?.as_deref() == Some(RELATIVE_OUTSIDE) {
        // A prefix outside the repository has an empty view; do not run the
        // diff engine (which would otherwise render the whole commit).
        return Ok((String::new(), String::new(), String::new()));
    }
    if let Some(parent) = commit.parent_commit_ids.first() {
        // `DiffArgs` keeps private bookkeeping fields, so build it through its
        // clap definition like the CLI does.
        let mut argv: Vec<String> = vec!["diff".to_string()];
        if args.histogram {
            argv.push("--histogram".to_string());
        }
        if args.full_index {
            argv.push("--full-index".to_string());
        }
        if let Some(context) = args.unified {
            argv.push(format!("--unified={context}"));
        }
        if let Some(filter) = &args.diff_filter {
            argv.push(format!("--diff-filter={filter}"));
        }
        if args.no_renames {
            argv.push("--no-renames".to_string());
        } else if let Some(score) = &args.find_renames {
            argv.push(format!("--find-renames={score}"));
        }
        if let Some(prefix) = &args.src_prefix {
            argv.push(format!("--src-prefix={prefix}"));
        }
        if let Some(prefix) = &args.dst_prefix {
            argv.push(format!("--dst-prefix={prefix}"));
        }
        if let Some(relative) = relative_diff_arg(args)? {
            argv.push(relative);
        }
        argv.push(parent.to_string());
        argv.push(commit.id.to_string());
        let diff_args = crate::command::diff::DiffArgs::try_parse_from(&argv)
            .map_err(|error| CliError::command_usage(error.to_string()))?;
        let output = crate::command::diff::run_diff_for_service(diff_args).await?;
        let stat = crate::command::diff::render::format_diff_stat_output(&output);
        let numstat = crate::command::diff::render::format_numstat_output(&output, false);
        let mut patch = crate::command::diff::render::format_unified_diff(&output);
        if !patch.is_empty() && !patch.ends_with('\n') {
            patch.push('\n');
        }
        return Ok((stat, numstat, patch));
    }
    let mut stat = String::new();
    if let Ok(mut stats) = log::compute_commit_stat(commit, vec![]).await {
        if let Some(prefix) = relative_prefix(args)? {
            stats.retain(|record| record.path.starts_with(&prefix));
            for record in &mut stats {
                record.path = record.path[prefix.len()..].to_string();
            }
        }
        stat = format_diffstat_plain(&stats);
    }
    let patch =
        log::generate_diff_with_options(commit, vec![], &commit_diff_options(args)?).await?;
    Ok((stat, String::new(), patch))
}

/// Internal marker for a `--relative` prefix that escapes the repository. Such a
/// view never matches a tracked path, so no diff is rendered (Git exits 0 with
/// an empty patch body).
const RELATIVE_OUTSIDE: &str = "\u{0}";

/// Normalize a repository-root-relative prefix (no leading/trailing slash).
fn normalize_relative_path(raw: &str) -> Option<String> {
    let trimmed = raw.trim().replace('\\', "/");
    let trimmed = trimmed.trim_start_matches("./").trim_matches('/');
    if trimmed.is_empty() || trimmed == "." {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// The repository-root-relative directory prefix selected by `--relative` /
/// `diff.relative`, or `None` when paths stay repository-relative.
fn relative_prefix(args: &FormatPatchArgs) -> Result<Option<String>, CliError> {
    if args.no_relative {
        return Ok(None);
    }
    let root = util::working_dir();
    let cwd = util::cur_dir();
    match &args.relative {
        None => Ok(None),
        Some(None) => {
            // Bare `--relative`: the current directory, which must live inside
            // the repository.
            match cwd.strip_prefix(&root) {
                Ok(relative) => Ok(normalize_relative_path(&relative.to_string_lossy())),
                Err(_) => Err(CliError::failure(format!(
                    "--relative: current directory '{}' is outside the repository",
                    cwd.display()
                ))
                .with_stable_code(StableErrorCode::CliInvalidTarget)),
            }
        }
        Some(Some(raw)) => {
            let normalized = normalize_relative_path(raw).unwrap_or_default();
            if normalized.is_empty() {
                return Ok(None);
            }
            let escapes = normalized == ".."
                || normalized.starts_with("../")
                || std::path::Path::new(&normalized).is_absolute();
            if escapes {
                return Ok(Some(RELATIVE_OUTSIDE.to_string()));
            }
            let target = root.join(&normalized);
            if target.exists() && !target.is_dir() {
                return Err(
                    CliError::failure(format!("--relative: '{raw}' is not a directory"))
                        .with_stable_code(StableErrorCode::CliInvalidTarget),
                );
            }
            Ok(Some(normalized))
        }
    }
}

/// The `--relative` argument to forward to the shared diff engine. The engine
/// resolves its value from the current directory, while `format-patch --relative`
/// resolves the value from the repository root; translate between the two.
fn relative_diff_arg(args: &FormatPatchArgs) -> Result<Option<String>, CliError> {
    let Some(prefix) = relative_prefix(args)? else {
        return Ok(None);
    };
    if prefix == RELATIVE_OUTSIDE {
        // Do not forward a cwd-relative `--relative`: the outside view matches
        // nothing and must not strip the current directory instead.
        return Ok(None);
    }
    let root = util::working_dir();
    let cwd = util::cur_dir();
    let target = root.join(&prefix);
    if target == cwd {
        return Ok(Some("--relative".to_string()));
    }
    if let Ok(relative) = target.strip_prefix(&cwd) {
        let relative = relative.to_string_lossy().replace('\\', "/");
        if relative.is_empty() {
            return Ok(Some("--relative".to_string()));
        }
        return Ok(Some(format!("--relative={relative}")));
    }
    // The target is an ancestor of the current directory: the shared engine's
    // cwd-relative strip cannot express it, so use bare `--relative` at the
    // ancestor only when the current directory is the target root.
    Ok(Some("--relative".to_string()))
}

fn commit_diff_options(args: &FormatPatchArgs) -> Result<log::CommitDiffOptions, CliError> {
    Ok(log::CommitDiffOptions {
        relative: relative_prefix(args)?,
        histogram: args.histogram,
        full_index: args.full_index,
        source_prefix: args.src_prefix.clone().unwrap_or_else(|| "a/".to_string()),
        destination_prefix: args.dst_prefix.clone().unwrap_or_else(|| "b/".to_string()),
        context_lines: args.unified.unwrap_or(3),
        diff_filter: args.diff_filter.clone().unwrap_or_default(),
    })
}

async fn resolve_format_config(args: &mut FormatPatchArgs) -> Result<(), FormatPatchError> {
    if args.subject_prefix.is_none() {
        args.subject_prefix = Some(
            read_format_config("format.subjectPrefix")
                .await?
                .unwrap_or_else(|| DEFAULT_SUBJECT_PREFIX.to_string()),
        );
    }
    if !args.stdout
        && args.output_directory.is_none()
        && let Some(directory) = read_format_config("format.outputDirectory").await?
    {
        if directory.is_empty() {
            return Err(FormatPatchError::InvalidConfig {
                key: "format.outputDirectory",
                value: directory,
                expected: "a non-empty directory path",
            });
        }
        args.output_directory = Some(directory);
    }
    if args.suffix.is_none() {
        args.suffix = Some(
            read_format_config("format.suffix")
                .await?
                .unwrap_or_else(|| DEFAULT_SUFFIX.to_string()),
        );
    }
    if !args.signoff
        && !args.no_signoff
        && let Some(value) = read_format_config("format.signOff").await?
    {
        args.signoff = crate::internal::config::parse_git_config_bool(&value).ok_or(
            FormatPatchError::InvalidConfig {
                key: "format.signOff",
                value,
                expected: "a Git boolean",
            },
        )?;
    }
    // `format.thread` applies only when the CLI says nothing about threading.
    // Each value fully determines the style; the strict cascade already gives
    // local-over-global and last-write-wins at the same layer.
    let thread_cli = args.thread.is_some() || args.no_thread;
    if !thread_cli && let Some(value) = read_format_config("format.thread").await? {
        let trimmed = value.trim();
        args.thread = if trimmed.is_empty() {
            None
        } else if trimmed.eq_ignore_ascii_case("deep") {
            Some("deep".to_string())
        } else if trimmed.eq_ignore_ascii_case("shallow") {
            Some("shallow".to_string())
        } else {
            match crate::internal::config::parse_git_config_bool(trimmed) {
                Some(true) => Some("shallow".to_string()),
                Some(false) => None,
                None => {
                    return Err(FormatPatchError::InvalidConfig {
                        key: "format.thread",
                        value,
                        expected: "true, false, shallow, deep, or empty",
                    });
                }
            }
        };
    }
    if args.relative.is_none()
        && !args.no_relative
        && let Some(value) = read_format_config("diff.relative").await?
    {
        let enabled = crate::internal::config::parse_git_config_bool(&value).ok_or(
            FormatPatchError::InvalidConfig {
                key: "diff.relative",
                value,
                expected: "a Git boolean",
            },
        )?;
        if enabled {
            args.relative = Some(None);
        }
    }
    // `format.from` is applied only when no command-line `--from` was given:
    // `true` uses the committer identity, `false` leaves the author untouched,
    // and any other value is used as an ident line (validated after this
    // resolve pass, so a CLI override wins over an invalid config value).
    if args.from.is_none()
        && !args.no_from
        && let Some(value) = read_format_config("format.from").await?
    {
        let trimmed = value.trim();
        if let Some(enabled) = crate::internal::config::parse_git_config_bool(trimmed) {
            if enabled {
                args.from = Some(None);
            }
        } else if !trimmed.is_empty() {
            args.from = Some(Some(trimmed.to_string()));
        }
    }
    Ok(())
}

async fn read_format_config(key: &'static str) -> Result<Option<String>, FormatPatchError> {
    crate::internal::config::read_cascaded_config_value_strict(
        crate::internal::config::LocalIdentityTarget::CurrentRepo,
        key,
    )
    .await
    .map_err(|error| FormatPatchError::ConfigRead {
        key,
        detail: format!("{error:#}"),
    })
}

/// The recipient lists to emit, after applying `--no-to`/`--no-cc` (which, with
/// no `format.to`/`format.cc` config to reset, simply suppress the header).
fn resolve_recipients(args: &FormatPatchArgs) -> (Vec<String>, Vec<String>) {
    let to = if args.no_to {
        Vec::new()
    } else {
        args.to.clone()
    };
    let cc = if args.no_cc {
        Vec::new()
    } else {
        args.cc.clone()
    };
    (to, cc)
}

/// Fold one or more addresses into a single header value, matching `git
/// format-patch`: addresses are joined with `,` and a 4-space-indented
/// continuation line. Each value is sanitized (control characters stripped, to
/// prevent header injection) but NOT RFC2047-encoded — Git passes recipient
/// addresses through verbatim even under `--encode-email-headers`.
fn fold_addresses(addresses: &[String]) -> String {
    addresses
        .iter()
        .map(|addr| sanitize_header_value(addr.trim()))
        .collect::<Vec<_>>()
        .join(",\n    ")
}

/// Append `To:`/`Cc:` headers (when non-empty). Git emits them after the MIME
/// header block, so callers add them last among the headers.
fn push_recipient_headers(out: &mut String, to: &[String], cc: &[String]) {
    if !to.is_empty() {
        out.push_str(&format!("To: {}\n", fold_addresses(to)));
    }
    if !cc.is_empty() {
        out.push_str(&format!("Cc: {}\n", fold_addresses(cc)));
    }
}

/// Normalize untrusted text before interpolating it into single-line mail
/// headers. This prevents CR/LF/control characters from creating extra mbox
/// headers while preserving readable subject/prefix text.
fn sanitize_header_value(value: &str) -> String {
    value
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// RFC 2047 Q-encode a header value for `--encode-email-headers`.
///
/// When `enable` is false or `value` is pure ASCII the value passes through
/// unchanged (matching Git, which only encodes headers that actually contain
/// non-ASCII). Otherwise the value is Q-encoded: spaces become `_`, ASCII
/// alphanumerics are kept verbatim, and every other byte is `=XX` hex-escaped
/// (this over-approximates Git's per-run encoding but is valid RFC 2047 and
/// decodes to the same text). The output is split into multiple
/// `=?UTF-8?q?...?=` encoded-words so that no single word exceeds the RFC 2047
/// 75-character limit, and a multi-byte character is never split across words;
/// adjacent encoded-words are separated by a space, which a conforming decoder
/// removes when concatenating them.
fn encode_email_header(value: &str, enable: bool) -> String {
    if !enable || value.is_ascii() {
        return value.to_string();
    }
    const PREFIX: &str = "=?UTF-8?q?";
    const SUFFIX: &str = "?=";
    // 75-char encoded-word limit minus the `=?UTF-8?q?` / `?=` delimiters.
    const MAX_PAYLOAD: usize = 75 - PREFIX.len() - SUFFIX.len();

    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for ch in value.chars() {
        // Encode one whole character so its bytes never straddle two words.
        let mut token = String::new();
        if ch == ' ' {
            token.push('_');
        } else if ch.is_ascii_alphanumeric() {
            token.push(ch);
        } else {
            let mut buf = [0u8; 4];
            for &byte in ch.encode_utf8(&mut buf).as_bytes() {
                token.push_str(&format!("={byte:02X}"));
            }
        }
        if !current.is_empty() && current.len() + token.len() > MAX_PAYLOAD {
            words.push(std::mem::take(&mut current));
        }
        current.push_str(&token);
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
        .iter()
        .map(|w| format!("{PREFIX}{w}{SUFFIX}"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parse a `Name <email>` identity into `(name, email)`; a string without
/// angle brackets is treated as a bare name with an empty email.
fn parse_from_ident(ident: &str) -> Result<(String, String), FormatPatchError> {
    if let (Some(lt), Some(gt)) = (ident.find('<'), ident.rfind('>'))
        && lt < gt
    {
        return Ok((
            ident[..lt].trim().to_string(),
            ident[lt + 1..gt].trim().to_string(),
        ));
    }
    Err(FormatPatchError::InvalidFromIdent(ident.to_string()))
}

/// Resolve the `--from` identity: `--from=<ident>` is parsed; bare `--from`
/// uses the committer's configured identity; absent returns `None` (the commit
/// author is used unchanged).
async fn resolve_from_identity(
    args: &FormatPatchArgs,
) -> Result<Option<(String, String)>, FormatPatchError> {
    match &args.from {
        None => Ok(None),
        Some(Some(ident)) => Ok(Some(parse_from_ident(ident)?)),
        Some(None) => {
            // Bare `--from` uses the execution-time committer identity (Git's
            // GIT_COMMITTER_NAME/GIT_COMMITTER_EMAIL aware resolution), not
            // `format.from`.
            let (_, identity) = crate::command::commit::create_committer_signature()
                .await
                .map_err(|error| FormatPatchError::IdentityMissing(error.to_string()))?;
            Ok(Some((identity.name, identity.email)))
        }
    }
}

/// Resolve the Signed-off-by identity from `user.name` / `user.email` config.
/// Returns an error when either key is missing so that `--signoff` never
/// silently writes a false DCO trailer with a fallback identity.
async fn resolve_signoff_identity() -> Result<(String, String), FormatPatchError> {
    let name = read_format_config("user.name")
        .await?
        .filter(|value| !value.trim().is_empty());
    let email = read_format_config("user.email")
        .await?
        .filter(|value| !value.trim().is_empty());

    let detail = match (name, email) {
        (Some(name), Some(email)) => return Ok((name, email)),
        (None, Some(_)) => "user.name is not configured".to_string(),
        (Some(_), None) => "user.email is not configured".to_string(),
        (None, None) => "user.name and user.email are not configured".to_string(),
    };
    Err(FormatPatchError::IdentityMissing(detail))
}

#[cfg(test)]
mod tests {
    use super::FormatPatchError;

    #[test]
    fn config_error_messages_are_stable_and_actionable() {
        assert_eq!(
            FormatPatchError::ConfigRead {
                key: "format.suffix",
                detail: "database is locked".to_string(),
            }
            .to_string(),
            "failed to read config 'format.suffix': database is locked"
        );
        assert_eq!(
            FormatPatchError::InvalidConfig {
                key: "format.signOff",
                value: "sometimes".to_string(),
                expected: "a Git boolean",
            }
            .to_string(),
            "bad config value 'sometimes' for 'format.signOff' (expected a Git boolean)"
        );
    }
}
