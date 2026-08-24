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
    internal::object::commit::Commit,
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
    /// from that commit are included (equivalent to <commit>..HEAD).
    #[arg(value_name = "revision-range")]
    pub revision_range: Option<String>,

    /// Generate only the named commit, or HEAD when no revision is given.
    #[arg(short = '1')]
    pub last_one: bool,

    /// Include the root commit and all reachable non-merge ancestors.
    #[arg(long = "root")]
    pub root: bool,

    /// Write patch files into DIR instead of the current working directory.
    #[arg(short = 'o', long = "output-directory", value_name = "DIR")]
    pub output_directory: Option<String>,

    /// Print all patches to stdout instead of individual files.
    #[arg(long = "stdout")]
    pub stdout: bool,

    /// Name output files in [PATCH n/m] order with a leading sequence number
    /// (e.g. "0001-subject.patch").
    #[arg(short = 'n', long = "numbered")]
    pub numbered: bool,

    /// Start numbering patches at N instead of 1.
    #[arg(long = "start-number", value_name = "N", default_value = "1")]
    pub start_number: usize,

    /// Use PREFIX instead of "PATCH" in the Subject: line.
    #[arg(long = "subject-prefix", value_name = "PREFIX")]
    pub subject_prefix: Option<String>,

    /// Generate a cover-letter template before the actual patches (named
    /// `0000-cover-letter<suffix>`, or `0` under `--numbered-files`).
    #[arg(long = "cover-letter")]
    pub cover_letter: bool,

    /// Add In-Reply-To and References headers so mailers thread the series
    /// (default on).
    #[arg(long = "thread", default_value_t = true)]
    pub thread: bool,

    /// Disable threading headers.
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
    #[arg(long = "from", value_name = "IDENT", num_args = 0..=1, require_equals = true)]
    pub from: Option<Option<String>>,

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

    /// Spend extra time finding the smallest Myers diff. Libra's default Myers
    /// backend already guarantees a shortest edit script, so this is output-
    /// equivalent to the default.
    #[arg(long = "minimal")]
    pub minimal: bool,

    /// Generate patch hunks with the histogram diff algorithm.
    #[arg(long = "histogram")]
    pub histogram: bool,

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

    /// Do not show the diffstat summary before the diff.
    #[arg(long = "no-stat")]
    pub no_stat: bool,

    /// Keep the original `[PATCH]` prefix if present in the commit subject,
    /// instead of stripping it (default behaviour in Git).
    #[arg(
        long = "keep-subject",
        help = "Keep the original [PATCH] prefix if present in the commit subject, instead of stripping it (default behaviour in Git)"
    )]
    pub keep_subject: bool,

    /// Filename suffix for generated patches (default ".patch"); e.g. ".txt".
    /// Ignored under `--numbered-files` (which uses bare sequence numbers).
    #[arg(long = "suffix", value_name = "SFX")]
    pub suffix: Option<String>,

    /// Output an all-zero hash in each patch's "From <hash>" line instead of
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

    resolve_format_config(&mut args).await?;

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
    if commits.is_empty() {
        if args.ignore_if_in_upstream {
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
        return Err(CliError::failure("no patches to generate")
            .with_stable_code(StableErrorCode::CliInvalidTarget));
    }

    // `--base`: compute the base-commit / prerequisite-patch-id trailer. It is
    // attached to the cover letter when present, otherwise to the last patch.
    let base_block = build_base_block(&args, &commits).await?;

    // 3. Build thread Message-ID (used for --thread / --in-reply-to)
    let thread_id = build_thread_id(&args, &commits);

    // Resolve the `--from` identity once (shared across all patches).
    let from_identity = resolve_from_identity(&args).await?;

    // 4. Determine numbering
    let total = commits.len();
    let start_num = args.start_number;

    // 5. Render the complete series before creating any patch file. A later
    // object/config error therefore cannot leave a partially generated series.
    let mut rendered = Vec::with_capacity(total + usize::from(args.cover_letter));
    if args.cover_letter {
        let cover_body = format_cover_letter(
            &args,
            &commits,
            &thread_id,
            from_identity.as_ref(),
            base_block.as_deref(),
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
    } else if args.root || args.last_one {
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

    // `-1` limits the newest-first walk before output ordering is reversed.
    if args.last_one {
        commits.truncate(1);
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
    // `--zero-commit` zeroes only this envelope hash (matching its hex length),
    // leaving the rest of the patch untouched, like `git format-patch`.
    let envelope_hash = if args.zero_commit {
        "0".repeat(commit.id.to_string().len())
    } else {
        commit.id.to_string()
    };
    out.push_str(&format!(
        "From {} {}\n",
        envelope_hash,
        ts.format("%a %b %e %H:%M:%S %Y")
    ));

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
    let prefix = format!(
        "{prefix}{version}",
        prefix = sanitize_header_value(subject_prefix(args))
    );

    let num_str = if args.numbered || args.cover_letter {
        let width = number_width(total);
        format!(" {:0width$}/{}", patch_num, total + start_num - 1)
    } else {
        String::new()
    };

    out.push_str(&format!("Subject: [{prefix}{num_str}] {subject}\n"));

    // ---- Threading headers ----
    push_threading_headers(
        &mut out,
        threading,
        patch_num,
        start_num,
        total,
        args.cover_letter,
    );

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
    message_section.push_str("---\n");
    if let Some(block) = render_notes_block(&args.notes, &commit.id.to_string()).await? {
        message_section.push_str(&block);
    }
    if !args.no_stat
        && let Ok(stats) = log::compute_commit_stat(commit, vec![]).await
    {
        let stat_text = format_diffstat_plain(&stats);
        if !stat_text.is_empty() {
            message_section.push_str(&stat_text);
            message_section.push('\n');
        }
    }

    // ---- Unified diff ----
    let diff = log::generate_diff_with_options(commit, vec![], &commit_diff_options(args)).await?;

    if let Some(disposition) = attach_disposition {
        // MIME multipart assembly (byte-compatible with `git format-patch --attach`).
        let slug = patch_slug(commit, args);
        let filename = patch_filename(
            args.numbered,
            patch_num,
            total,
            start_num,
            &slug,
            suffix(args),
            args.numbered_files,
        );
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

#[derive(Clone, Debug)]
struct Threading {
    series_id: String,
    external_reply_to: Option<String>,
}

/// Build deterministic message IDs for the series and retain an optional
/// external parent separately. `--in-reply-to` is never reused as a generated
/// Message-ID, which avoids duplicate IDs and accidental `<<id>>` headers.
fn build_thread_id(args: &FormatPatchArgs, commits: &[Commit]) -> Option<Threading> {
    if !args.thread || args.no_thread {
        return None;
    }
    commits.first().map(|c| {
        let ts = c.committer.timestamp;
        let id = c.id.to_string();
        let short = &id[..8.min(id.len())];
        let series_id = format!("{short}-{ts}@libra");
        let external_reply_to = args.in_reply_to.as_deref().map(normalize_message_id);
        Threading {
            series_id,
            external_reply_to,
        }
    })
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
    let is_first = patch_num == start_num;
    let message_id = if is_first {
        threading.series_id.clone()
    } else {
        format!("{}-p{patch_num}", threading.series_id)
    };
    out.push_str(&format!("Message-ID: <{message_id}>\n"));

    let cover_id = format!("{}-cover", threading.series_id);
    let reply_to = if has_cover_letter {
        Some(cover_id.as_str())
    } else if let Some(external) = threading.external_reply_to.as_deref() {
        Some(external)
    } else if total > 1 && !is_first {
        Some(threading.series_id.as_str())
    } else {
        None
    };
    if let Some(reply_to) = reply_to {
        out.push_str(&format!("In-Reply-To: <{reply_to}>\n"));
        let reference = threading.external_reply_to.as_deref().unwrap_or(reply_to);
        out.push_str(&format!("References: <{reference}>\n"));
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
) -> Result<String, CliError> {
    let now = Utc::now();

    let mut out = String::new();

    // "From " envelope
    out.push_str(&format!(
        "From 0000000000000000000000000000000000000000 {}\n",
        now.format("%a %b %e %H:%M:%S %Y")
    ));

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
    out.push_str(&format!(
        "Subject: [{prefix} 0/{total}] *** SUBJECT HERE ***\n",
        total = commits.len()
    ));
    if let Some(threading) = threading {
        out.push_str(&format!("Message-ID: <{}-cover>\n", threading.series_id));
        if let Some(parent) = threading.external_reply_to.as_deref() {
            out.push_str(&format!("In-Reply-To: <{parent}>\n"));
            out.push_str(&format!("References: <{parent}>\n"));
        }
    }
    out.push_str("MIME-Version: 1.0\n");
    out.push_str("Content-Type: text/plain; charset=UTF-8\n");
    out.push_str("Content-Transfer-Encoding: 8bit\n");
    let (to, cc) = resolve_recipients(args);
    push_recipient_headers(&mut out, &to, &cc);
    out.push('\n');

    out.push_str("*** SUBJECT HERE ***\n\n");
    out.push_str("*** BLURB HERE ***\n\n");

    // Shortlog of the patches
    for (i, commit) in commits.iter().enumerate() {
        let (msg, _) = parse_commit_msg(&commit.message);
        let _subject = msg.lines().next().unwrap_or("");
        out.push_str(&format!(
            "{:0width$}-{}{}\n",
            i + 1,
            patch_slug(commit, args),
            suffix(args),
            width = number_width(commits.len())
        ));
    }
    out.push('\n');

    // `--base` trailer rides on the cover letter when one is present (the blank
    // line above separates it from the shortlog), before the signature.
    if let Some(block) = base_block {
        out.push_str(block);
    }

    push_signature(&mut out, args);

    Ok(out)
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
fn patch_filename(
    _numbered: bool,
    patch_num: usize,
    total: usize,
    start_num: usize,
    slug: &str,
    suffix: &str,
    numbered_files: bool,
) -> String {
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
        format!("{:0width$}-{}{}", patch_num, slug, suffix)
    }
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
        stdout
            .write_all(b"\n")
            .map_err(|error| stdout_write_error("finish format-patch output", error))?;
        Ok(PathBuf::from("<stdout>"))
    } else {
        let filename = patch_filename(
            args.numbered || args.cover_letter,
            patch_num,
            total,
            start_num,
            slug,
            suffix(args),
            args.numbered_files,
        );
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

fn commit_diff_options(args: &FormatPatchArgs) -> log::CommitDiffOptions {
    log::CommitDiffOptions {
        histogram: args.histogram,
        full_index: args.full_index,
        source_prefix: args.src_prefix.clone().unwrap_or_else(|| "a/".to_string()),
        destination_prefix: args.dst_prefix.clone().unwrap_or_else(|| "b/".to_string()),
    }
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
fn parse_from_ident(ident: &str) -> (String, String) {
    if let (Some(lt), Some(gt)) = (ident.find('<'), ident.rfind('>'))
        && lt < gt
    {
        return (
            ident[..lt].trim().to_string(),
            ident[lt + 1..gt].trim().to_string(),
        );
    }
    (ident.trim().to_string(), String::new())
}

/// Resolve the `--from` identity: `--from=<ident>` is parsed; bare `--from`
/// uses the committer's configured identity; absent returns `None` (the commit
/// author is used unchanged).
async fn resolve_from_identity(
    args: &FormatPatchArgs,
) -> Result<Option<(String, String)>, FormatPatchError> {
    match &args.from {
        None => Ok(None),
        Some(Some(ident)) => Ok(Some(parse_from_ident(ident))),
        Some(None) => Ok(Some(resolve_signoff_identity().await?)),
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
