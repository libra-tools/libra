//! `libra apply --check` — validate that a unified-diff patch applies cleanly,
//! without writing anything. A focused MVP of `git apply --check` built on the
//! same `diffy` patch engine used elsewhere.
//!
//! Only `--check` is supported in this version: the patch is parsed, every
//! target path is safety-checked, and each file hunk-set is test-applied
//! against the current working tree. Actually writing the result (atomic
//! temp-file + rename) is a documented future extension.

use std::{
    collections::{HashMap, HashSet},
    fs,
    io::{self, Read},
    path::{Component, Path, PathBuf},
};

use clap::Parser;
use serde::Serialize;

use crate::utils::{
    atomic_write::write_atomic,
    error::{CliError, CliResult, StableErrorCode},
    output::{OutputConfig, emit_json_data},
    util,
};

/// Hard cap on a patch input, matching the grit-gap plan's default.
pub(crate) const MAX_PATCH_BYTES: usize = 64 * 1024 * 1024;

/// A fully validated patch result. No worktree write happens while this value
/// is built, so a malformed or non-applicable multi-file patch fails before
/// touching its first target.
/// `(base, ours, theirs)` bytes for a three-way conflict.
pub(crate) type ConflictSides = (Vec<u8>, Vec<u8>, Vec<u8>);

#[derive(Debug)]
pub(crate) struct PreparedPatch {
    files: Vec<PreparedFile>,
    /// PD-09 ③: targets whose three-way fallback left conflict markers
    /// in the prepared content. Empty on a clean preparation.
    conflicted: Vec<String>,
    /// Targets whose `--3way` fallback merged cleanly.
    clean_merges: Vec<String>,
    /// Conflict sides `(base, ours, theirs)` for unmerged index stages.
    conflict_sides: HashMap<String, ConflictSides>,
}

#[derive(Debug)]
struct PreparedFile {
    target: String,
    absolute: PathBuf,
    /// Final bytes (`None` = the target is removed).
    content: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
    /// PD-09 ②: git-mode override (`old mode`/`new mode`/`new file mode`
    /// headers). Applied after the content write; only the executable bit
    /// is honored (100755 vs 100644), matching the tracked-mode model.
    mode: Option<u32>,
}

#[derive(Debug)]
pub(crate) enum PatchPreparationError {
    Invalid(String),
    DoesNotApply(String),
}

impl PreparedPatch {
    pub(crate) fn targets(&self) -> Vec<String> {
        self.files.iter().map(|file| file.target.clone()).collect()
    }

    /// PD-09 ③: targets whose prepared content carries three-way
    /// conflict markers (the caller decides whether to write them and
    /// pause, or treat the patch as failed).
    pub(crate) fn conflicted(&self) -> &[String] {
        &self.conflicted
    }

    pub(crate) fn clean_merges(&self) -> &[String] {
        &self.clean_merges
    }

    pub(crate) fn conflict_sides(&self) -> &HashMap<String, ConflictSides> {
        &self.conflict_sides
    }

    /// Prepared results for index updates: `(target, new content, mode)`.
    /// `content == None` deletes the index entry.
    pub(crate) fn staged_entries(&self) -> Vec<(String, Option<Vec<u8>>, Option<u32>)> {
        self.files
            .iter()
            .map(|file| (file.target.clone(), file.content.clone(), file.mode))
            .collect()
    }

    /// PD-09 ②: surviving targets whose git mode the patch changes.
    /// Callers that stage through the regular add path must ALSO apply
    /// these to the index — a mode-only flip has identical content, so
    /// the worktree-change scan alone would never re-stage it.
    pub(crate) fn mode_overrides(&self) -> Vec<(String, u32)> {
        self.files
            .iter()
            .filter(|file| file.content.is_some())
            .filter_map(|file| file.mode.map(|mode| (file.target.clone(), mode)))
            .collect()
    }

    /// Materialize every prepared result. Each replacement is atomic; callers
    /// that need multi-file rollback must persist sequencer state before this
    /// method and restore the current HEAD/index on failure.
    pub(crate) fn write(self) -> Result<(), String> {
        for file in self.files {
            match file.content {
                Some(content) => {
                    // Symlink sections materialize as REAL symlinks whose
                    // target is the patched content (git mode 120000).
                    if file.mode.is_some_and(|mode| mode & 0o170000 == 0o120000) {
                        #[cfg(unix)]
                        {
                            use std::os::unix::ffi::OsStrExt;
                            match fs::remove_file(&file.absolute) {
                                Ok(()) => {}
                                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                                Err(error) => {
                                    return Err(format!(
                                        "failed to replace symlink target '{}': {error}",
                                        file.target
                                    ));
                                }
                            }
                            let target_path = Path::new(std::ffi::OsStr::from_bytes(&content));
                            std::os::unix::fs::symlink(target_path, &file.absolute).map_err(
                                |error| {
                                    format!("failed to create symlink '{}': {error}", file.target)
                                },
                            )?;
                            continue;
                        }
                        #[cfg(not(unix))]
                        return Err(format!(
                            "symlink patch '{}' is not supported on this platform",
                            file.target
                        ));
                    }
                    write_atomic(&file.absolute, &content, false).map_err(|error| {
                        format!("failed to write patched file '{}': {error}", file.target)
                    })?;
                    // A brand-new file (no preserved permissions) gets the
                    // conventional umask-derived mode like git, instead of
                    // the temp file's private 0600.
                    #[cfg(unix)]
                    if file.permissions.is_none() {
                        use std::os::unix::fs::PermissionsExt;
                        let executable = file.mode.is_some_and(|mode| mode & 0o111 != 0);
                        let base = if executable { 0o777 } else { 0o666 };
                        let mode = base & !process_umask();
                        fs::set_permissions(&file.absolute, fs::Permissions::from_mode(mode))
                            .map_err(|error| {
                                format!(
                                    "failed to set mode on new patched file '{}': {error}",
                                    file.target
                                )
                            })?;
                    }
                    if let Some(permissions) = file.permissions {
                        fs::set_permissions(&file.absolute, permissions).map_err(|error| {
                            format!(
                                "failed to restore permissions on patched file '{}': {error}",
                                file.target
                            )
                        })?;
                    }
                    #[cfg(not(unix))]
                    let _ = file.mode;
                    #[cfg(unix)]
                    if let Some(mode) = file.mode {
                        use std::os::unix::fs::PermissionsExt;
                        let current = fs::symlink_metadata(&file.absolute)
                            .map_err(|error| {
                                format!("cannot inspect patched file '{}': {error}", file.target)
                            })?
                            .permissions()
                            .mode();
                        let wanted = if mode & 0o111 != 0 {
                            current | 0o111
                        } else {
                            current & !0o111
                        };
                        fs::set_permissions(&file.absolute, fs::Permissions::from_mode(wanted))
                            .map_err(|error| {
                                format!(
                                    "failed to set mode on patched file '{}': {error}",
                                    file.target
                                )
                            })?;
                    }
                }
                None => match fs::remove_file(&file.absolute) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => {
                        return Err(format!(
                            "failed to remove patched file '{}': {error}",
                            file.target
                        ));
                    }
                },
            }
        }
        Ok(())
    }
}

/// Read the process umask without changing it (set-and-restore, the
/// only portable POSIX read).
#[cfg(unix)]
fn process_umask() -> u32 {
    // INVARIANT: umask(2) cannot fail; the immediate restore keeps the
    // process value unchanged.
    unsafe {
        let current = libc::umask(0);
        libc::umask(current);
        current as u32
    }
}

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const APPLY_EXAMPLES: &str = "\
EXAMPLES:
    libra apply fix.patch                   Apply a patch to the working tree
    libra apply --check fix.patch           Check whether a patch applies cleanly
    libra apply -p0 fix.patch               Do not strip a leading path component
    libra apply -R fix.patch                Reverse-apply a patch
    libra apply --stat fix.patch            Show a diffstat instead of applying
    libra apply --3way --index fix.patch    Three-way fallback, then stage the result
    cat fix.patch | libra apply --check     Read the patch from stdin
    libra --json apply --check fix.patch    Structured { applies, files }";

/// Apply a unified-diff patch to the working tree (Git's `git apply`).
#[derive(Parser, Debug)]
#[command(after_help = APPLY_EXAMPLES)]
pub struct ApplyArgs {
    /// Validate that the patch applies without writing anything.
    #[clap(long)]
    pub check: bool,

    /// Show a diffstat of the patch and exit without applying.
    #[clap(long)]
    pub stat: bool,

    /// Show a machine-readable `insertions\tdeletions\tpath` report and exit
    /// without applying.
    #[clap(long)]
    pub numstat: bool,

    /// Show a create/delete/mode-change summary and exit without applying.
    #[clap(long)]
    pub summary: bool,

    /// Apply even when a report flag (`--stat`/`--numstat`/`--summary`) is
    /// present: print the report first, then apply (Git's `--apply`).
    #[clap(long)]
    pub apply: bool,

    /// Apply the patch in reverse (swap old/new sides).
    #[clap(short = 'R', long = "reverse")]
    pub reverse: bool,

    /// Fall back to a three-way merge when a text hunk does not apply
    /// cleanly, using the patch's `index` blobs as the base.
    #[clap(long = "3way")]
    pub three_way: bool,

    /// Update the index (stage the patched paths) after writing; requires the
    /// working tree to match the index for every touched path.
    #[clap(long = "index")]
    pub index: bool,

    /// Update only the index; leave the working tree untouched.
    #[clap(long)]
    pub cached: bool,

    /// Only apply sections whose target path matches one of these patterns.
    #[clap(long = "include", value_name = "PATH")]
    pub include: Vec<String>,

    /// Skip sections whose target path matches one of these patterns.
    #[clap(long = "exclude", value_name = "PATH")]
    pub exclude: Vec<String>,

    /// Prepend this directory to every patched path.
    #[clap(long = "directory", value_name = "DIR")]
    pub directory: Option<String>,

    /// Strip `<n>` leading path components from each patched path (like
    /// `git apply -p<n>`; default 1).
    #[clap(short = 'p', value_name = "N", default_value_t = 1)]
    pub strip: u32,

    /// Patch files to read; if none are given, the patch is read from stdin.
    #[clap(value_name = "PATCH")]
    pub patches: Vec<String>,
}

#[derive(Debug, Serialize)]
struct ApplyOutput {
    /// Whether the whole patch applies cleanly.
    applies: bool,
    /// The target paths the patch touches.
    files: Vec<String>,
    /// Whether the working tree was written (false for `--check`/`--stat`).
    applied: bool,
}

pub async fn execute(args: ApplyArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Exit 0 when the patch applies, 1 when it does not, 128 on
/// errors (not a repo, unreadable/oversized/malformed patch, or an unsafe
/// target path).
pub async fn execute_safe(args: ApplyArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let usage_error = |message: String| {
        CliError::fatal(message)
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
    };

    if args.cached && args.index {
        return Err(usage_error(
            "--cached and --index are incompatible".to_string(),
        ));
    }

    let mut patch_text = read_patch(&args.patches).map_err(usage_error)?;
    let workdir = util::working_dir();

    if let Some(directory) = &args.directory {
        patch_text = rewrite_patch_directory(&patch_text, directory).map_err(usage_error)?;
    }
    let filter_rules = ordered_filter_rules();
    patch_text =
        filter_patch_sections(&patch_text, &filter_rules, args.strip).map_err(usage_error)?;

    // Everything was filtered out: Git succeeds without output or writes.
    if patch_text.trim().is_empty() {
        if output.is_json() {
            emit_json_data(
                "apply",
                &ApplyOutput {
                    applies: true,
                    files: Vec::new(),
                    applied: false,
                },
                output,
            )?;
        }
        return Ok(());
    }

    // `--index` requires the working tree to match the index for every path
    // the patch touches (Git checks this before attempting the hunks).
    if args.index {
        let targets = patch_targets(&patch_text, args.strip).map_err(usage_error)?;
        ensure_paths_match_index(&targets).map_err(usage_error)?;
    }

    if args.reverse {
        patch_text = reverse_patch_text(&patch_text, args.strip);
    }

    if args.stat || args.numstat || args.summary {
        let mut report = String::new();
        if args.stat {
            report.push_str(&format_apply_stat(&patch_text, args.strip));
        }
        if args.numstat {
            report.push_str(&format_apply_numstat(&patch_text, args.strip));
        }
        if args.summary {
            report.push_str(&format_apply_summary(&patch_text, args.strip));
        }
        let files = patch_targets(&patch_text, args.strip).map_err(usage_error)?;
        if !args.apply {
            if output.is_json() {
                emit_json_data(
                    "apply",
                    &ApplyOutput {
                        applies: true,
                        files,
                        applied: false,
                    },
                    output,
                )?;
            } else {
                print!("{report}");
            }
            return Ok(());
        }
        if !output.is_json() {
            print!("{report}");
        }
    }

    let resolver = resolve_local_base_blob;
    let prepared = prepare_patch_opts(
        &patch_text,
        args.strip,
        &workdir,
        args.three_way.then_some(&resolver as BaseBlobResolver<'_>),
    );
    let (applies, files) = match prepared {
        Ok(prepared) => (true, prepared.targets()),
        Err(PatchPreparationError::DoesNotApply(detail)) => {
            let files = patch_targets(&patch_text, args.strip).map_err(usage_error)?;
            // Git reports the first non-applying hunk and exits 1 without
            // touching the worktree.
            eprintln!("error: {detail}");
            (false, files)
        }
        Err(PatchPreparationError::Invalid(message)) => return Err(usage_error(message)),
    };

    if !applies {
        if output.is_json() {
            emit_json_data(
                "apply",
                &ApplyOutput {
                    applies: false,
                    files,
                    applied: false,
                },
                output,
            )?;
        }
        return Err(CliError::silent_exit(1));
    }

    // Re-prepare (the previous `Ok` value was consumed by `targets()`).
    let prepared = prepare_patch_opts(
        &patch_text,
        args.strip,
        &workdir,
        args.three_way.then_some(&resolver as BaseBlobResolver<'_>),
    )
    .map_err(|error| match error {
        PatchPreparationError::Invalid(message) => usage_error(message),
        PatchPreparationError::DoesNotApply(message) => usage_error(message),
    })?;

    if args.check {
        if output.is_json() {
            emit_json_data(
                "apply",
                &ApplyOutput {
                    applies: true,
                    files,
                    applied: false,
                },
                output,
            )?;
        }
        return Ok(());
    }

    let conflicted = prepared.conflicted().to_vec();
    let mode_overrides = prepared.mode_overrides();
    let clean_merges = prepared.clean_merges().to_vec();
    let conflict_side_map = prepared.conflict_sides().clone();
    // `--3way` implies `--index` (Git stages the merged result), but only an
    // explicit `--index` requires the worktree to match the index first.
    let implied_index = args.three_way && !args.cached;
    if args.cached {
        // `--cached` writes the new blobs into the index only.
        stage_prepared_index(&prepared)?;
    } else {
        prepared.write().map_err(usage_error)?;
        if args.index || implied_index {
            stage_apply_targets(&files).await?;
            stage_apply_modes(&mode_overrides)?;
            if !conflicted.is_empty() {
                stage_conflict_sides(&conflict_side_map)?;
            }
        }
    }
    if args.three_way {
        for target in &clean_merges {
            eprintln!("Applied patch to '{target}' cleanly.");
        }
    }

    if output.is_json() {
        emit_json_data(
            "apply",
            &ApplyOutput {
                applies: true,
                files,
                applied: true,
            },
            output,
        )?;
    } else if !conflicted.is_empty() {
        eprintln!(
            "Applied patch with conflicts in: {} (resolve the markers)",
            conflicted.join(", ")
        );
    }

    if conflicted.is_empty() {
        Ok(())
    } else {
        // Git exits 1 when the three-way fallback leaves conflicts.
        Err(CliError::silent_exit(1))
    }
}

/// Prefix every patched path with `directory` (Git's `apply --directory`).
fn rewrite_patch_directory(patch_text: &str, directory: &str) -> Result<String, String> {
    let directory = directory.trim_end_matches('/');
    if directory.is_empty() {
        return Ok(patch_text.to_string());
    }
    if Path::new(directory).is_absolute() {
        return Err(format!("--directory '{directory}' must be relative"));
    }
    // Insert the directory AFTER the `a/`/`b/` prefix so `-p` stripping still
    // resolves the intended path; rename/copy paths are bare.
    let prefix = |raw: &str| -> String {
        if raw == "/dev/null" {
            raw.to_string()
        } else if let Some(rest) = raw.strip_prefix("a/") {
            format!("a/{directory}/{rest}")
        } else if let Some(rest) = raw.strip_prefix("b/") {
            format!("b/{directory}/{rest}")
        } else {
            format!("{directory}/{raw}")
        }
    };
    let mut out = String::new();
    for section in split_file_patches(patch_text) {
        for line in section.lines() {
            if let Some(rest) = line.strip_prefix("diff --git ")
                && let Some((left, right)) = rest.split_once(" b/")
            {
                out.push_str(&format!(
                    "diff --git {} b/{}\n",
                    prefix(left),
                    prefix(right)
                ));
                continue;
            }
            if let Some(rest) = line.strip_prefix("--- ") {
                out.push_str(&format!("--- {}\n", prefix(rest)));
                continue;
            }
            if let Some(rest) = line.strip_prefix("+++ ") {
                out.push_str(&format!("+++ {}\n", prefix(rest)));
                continue;
            }
            for keyword in ["rename from ", "rename to ", "copy from ", "copy to "] {
                if let Some(rest) = line.strip_prefix(keyword) {
                    out.push_str(&format!("{keyword}{}\n", prefix(rest)));
                    break;
                }
            }
            if ["rename from ", "rename to ", "copy from ", "copy to "]
                .iter()
                .any(|keyword| line.starts_with(keyword))
            {
                continue;
            }
            out.push_str(line);
            out.push('\n');
        }
    }
    Ok(out)
}

/// Keep only sections matching `--include` (when any) and not matching
/// `--exclude`. Matching uses fnmatch-style globs against the `-p`-stripped
/// target path.
#[derive(Clone, Copy)]
enum FilterAction {
    Include,
    Exclude,
}

/// Reconstruct `--include`/`--exclude` in command-line order. Clap stores the
/// two lists separately, but Git's rule is "the first matching pattern
/// decides", so the interleaved order matters (`--exclude=* --include=a.txt`
/// skips while `--include=a.txt --exclude=*` applies).
fn ordered_filter_rules() -> Vec<(FilterAction, String)> {
    let mut rules = Vec::new();
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        if let Some(value) = arg.strip_prefix("--include=") {
            rules.push((FilterAction::Include, value.to_string()));
        } else if arg == "--include"
            && let Some(value) = argv.get(index + 1)
        {
            rules.push((FilterAction::Include, value.clone()));
            index += 1;
        } else if let Some(value) = arg.strip_prefix("--exclude=") {
            rules.push((FilterAction::Exclude, value.to_string()));
        } else if arg == "--exclude"
            && let Some(value) = argv.get(index + 1)
        {
            rules.push((FilterAction::Exclude, value.clone()));
            index += 1;
        }
        index += 1;
    }
    rules
}

fn filter_patch_sections(
    patch_text: &str,
    rules: &[(FilterAction, String)],
    strip: u32,
) -> Result<String, String> {
    if rules.is_empty() {
        return Ok(patch_text.to_string());
    }
    let compiled: Vec<(FilterAction, ignore::gitignore::Gitignore)> = rules
        .iter()
        .filter_map(|(action, pattern)| {
            let mut builder = ignore::gitignore::GitignoreBuilder::new("");
            builder.add_line(None, pattern).ok()?;
            builder.build().ok().map(|gi| (*action, gi))
        })
        .collect();
    let mut out = String::new();
    for section in split_file_patches(patch_text) {
        let headers = parse_section_headers(&section, strip)?;
        let target = headers
            .single_target(&section, strip)
            .unwrap_or_else(|_| headers.display_target().unwrap_or_default());
        let mut keep = true;
        for (action, matcher) in &compiled {
            let matched = matcher.matched_path_or_any_parents(Path::new(&target), false);
            if matched.is_ignore() || matched.is_whitelist() {
                keep = matches!(action, FilterAction::Include);
                break;
            }
        }
        if keep {
            out.push_str(&section);
        }
    }
    Ok(out)
}

/// Render a `--stat` summary for a patch without applying, matching Git's
/// padded name/count columns and its omission of zero insertion/deletion
/// clauses in the trailing summary line.
fn format_apply_stat(patch_text: &str, strip: u32) -> String {
    struct Entry {
        target: String,
        insertions: usize,
        deletions: usize,
        binary: bool,
    }
    let mut entries = Vec::new();
    for section in split_file_patches(patch_text) {
        let Ok(headers) = parse_section_headers(&section, strip) else {
            continue;
        };
        let target = headers
            .single_target(&section, strip)
            .unwrap_or_else(|_| headers.display_target().unwrap_or_default());
        let (insertions, deletions) = count_section_changes(&section);
        entries.push(Entry {
            target,
            insertions,
            deletions,
            binary: headers.binary.is_some() || headers.binary_marker,
        });
    }
    if entries.is_empty() {
        return String::new();
    }
    let name_width = entries
        .iter()
        .map(|entry| entry.target.len())
        .max()
        .unwrap_or(0);
    let count_width = entries
        .iter()
        .map(|entry| (entry.insertions + entry.deletions).to_string().len())
        .max()
        .unwrap_or(0)
        .max(4);
    let mut out = String::new();
    let mut files = 0usize;
    let mut insertions = 0usize;
    let mut deletions = 0usize;
    for entry in &entries {
        files += 1;
        if entry.binary {
            out.push_str(&format!(
                " {:<name_width$} | Bin\n",
                entry.target,
                name_width = name_width
            ));
            continue;
        }
        insertions += entry.insertions;
        deletions += entry.deletions;
        let total = entry.insertions + entry.deletions;
        out.push_str(&format!(
            " {:<name_width$} | {:>count_width$} {}\n",
            entry.target,
            total,
            "+".repeat(entry.insertions) + &"-".repeat(entry.deletions),
            name_width = name_width,
            count_width = count_width,
        ));
    }
    let mut clauses = vec![format!(
        "{files} file{} changed",
        if files == 1 { "" } else { "s" }
    )];
    if insertions > 0 {
        clauses.push(format!(
            "{insertions} insertion{}(+)",
            if insertions == 1 { "" } else { "s" }
        ));
    }
    if deletions > 0 {
        clauses.push(format!(
            "{deletions} deletion{}(-)",
            if deletions == 1 { "" } else { "s" }
        ));
    }
    out.push_str(&format!(" {}\n", clauses.join(", ")));
    out
}

/// Render `--numstat`: `insertions\tdeletions\tpath`, with `-\t-` for binary
/// sections and the post-image path for renames.
fn format_apply_numstat(patch_text: &str, strip: u32) -> String {
    let mut out = String::new();
    for section in split_file_patches(patch_text) {
        let Ok(headers) = parse_section_headers(&section, strip) else {
            continue;
        };
        let target = headers
            .single_target(&section, strip)
            .unwrap_or_else(|_| headers.display_target().unwrap_or_default());
        let binary = headers.binary.is_some() || headers.binary_marker;
        let (ins, del) = count_section_changes(&section);
        if binary {
            out.push_str(&format!("-\t-\t{target}\n"));
        } else {
            out.push_str(&format!("{ins}\t{del}\t{target}\n"));
        }
    }
    out
}

/// Render `--summary`: created and deleted files plus mode changes.
fn format_apply_summary(patch_text: &str, strip: u32) -> String {
    let mut out = String::new();
    for section in split_file_patches(patch_text) {
        let Ok(headers) = parse_section_headers(&section, strip) else {
            continue;
        };
        let target = headers
            .single_target(&section, strip)
            .unwrap_or_else(|_| headers.display_target().unwrap_or_default());
        if let Some(mode) = headers.new_file_mode {
            out.push_str(&format!(" create mode {mode:06o} {target}\n"));
        } else if let Some(mode) = headers.deleted_file_mode {
            out.push_str(&format!(" delete mode {mode:06o} {target}\n"));
        } else if let (Some(old), Some(new)) = (headers.old_mode, headers.new_mode)
            && old != new
        {
            out.push_str(&format!(" mode change {old:06o} => {new:06o} {target}\n"));
        }
    }
    out
}

/// Count `+`/`-` body lines in a section (excluding headers).
fn count_section_changes(section: &str) -> (usize, usize) {
    let mut ins = 0usize;
    let mut del = 0usize;
    let mut in_hunks = false;
    for line in section.lines() {
        if line.starts_with("@@ ") {
            in_hunks = true;
            continue;
        }
        if !in_hunks {
            continue;
        }
        if line.starts_with('+') && !line.starts_with("+++") {
            ins += 1;
        } else if line.starts_with('-') && !line.starts_with("---") {
            del += 1;
        }
    }
    (ins, del)
}

/// Reverse a patch: swap old/new sides, hunks, modes, rename/copy pairs and
/// binary literal/delta anchors. Binary sections re-anchor their literal to
/// the pre-image blob when it is locally resolvable.
fn reverse_patch_text(patch_text: &str, _strip: u32) -> String {
    let mut out = String::new();
    for section in split_file_patches(patch_text) {
        let mut lines: Vec<String> = Vec::new();
        let mut in_hunks = false;
        let mut binary = false;
        let mut index_old: Option<String> = None;
        let mut pending_minus: Option<String> = None;
        for line in section.lines() {
            if line == "GIT binary patch" {
                binary = true;
                lines.push(line.to_string());
                continue;
            }
            if line.starts_with("@@ ") {
                in_hunks = true;
                lines.push(reverse_hunk_header(line));
                continue;
            }
            if let Some(rest) = line.strip_prefix("diff --git ")
                && let Some((left, right)) = rest.split_once(" b/")
            {
                let left_name = left.strip_prefix("a/").unwrap_or(left);
                lines.push(format!("diff --git a/{right} b/{left_name}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("--- ") {
                // Buffer the minus side; the immediately following `+++` line
                // supplies the swapped value so `---` still precedes `+++`.
                pending_minus = Some(rest.to_string());
                continue;
            }
            if let Some(rest) = line.strip_prefix("+++ ") {
                let minus = pending_minus
                    .take()
                    .unwrap_or_else(|| "/dev/null".to_string());
                lines.push(format!("--- {rest}"));
                lines.push(format!("+++ {minus}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("old mode ") {
                lines.push(format!("new mode {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("new mode ") {
                lines.push(format!("old mode {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("new file mode ") {
                lines.push(format!("deleted file mode {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("deleted file mode ") {
                lines.push(format!("new file mode {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("rename from ") {
                lines.push(format!("rename from {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("rename to ") {
                lines.push(format!("rename to {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("copy from ") {
                lines.push(format!("copy from {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("copy to ") {
                lines.push(format!("copy to {rest}"));
                continue;
            }
            if let Some(rest) = line.strip_prefix("index ")
                && let Some((old, new)) = rest.split_once("..")
            {
                index_old = Some(old.to_string());
                lines.push(format!("index {new}..{old}"));
                continue;
            }
            if in_hunks && !binary {
                if let Some(rest) = line.strip_prefix('+') {
                    lines.push(format!("-{rest}"));
                    continue;
                }
                if let Some(rest) = line.strip_prefix('-') {
                    lines.push(format!("+{rest}"));
                    continue;
                }
            }
            if binary && (line.starts_with("literal ") || line.starts_with("delta ")) {
                let (kind, size) = line.split_once(' ').unwrap_or((line, "0"));
                let kind = if kind == "literal" {
                    "delta"
                } else {
                    "literal"
                };
                lines.push(format!("{kind} {size}"));
                continue;
            }
            lines.push(line.to_string());
        }

        // Binary reverse: replace the whole binary payload with a literal of
        // the pre-image blob when it can be resolved locally.
        if binary
            && let Some(old_oid) = index_old.clone()
            && let Some(pre_image) = resolve_local_base_blob(&old_oid)
        {
            {
                let mut rebuilt: Vec<String> = Vec::new();
                let mut saw_marker = false;
                for line in lines {
                    if line.starts_with("literal ") || line.starts_with("delta ") {
                        if saw_marker {
                            continue;
                        }
                        saw_marker = true;
                        rebuilt.push(format!("literal {}", pre_image.len()));
                        let encoded = crate::command::diff::git_base85(
                            &crate::command::diff::zlib_deflate(&pre_image),
                        );
                        for encoded_line in encoded.lines() {
                            rebuilt.push(encoded_line.to_string());
                        }
                    } else {
                        rebuilt.push(line);
                    }
                }
                lines = rebuilt;
            }
        }

        out.push_str(&lines.join("\n"));
        out.push('\n');
    }
    out
}

/// Swap the ranges in a `@@ -a,b +c,d @@` header, preserving any trailing
/// function context.
fn reverse_hunk_header(line: &str) -> String {
    let Some(start) = line.find("@@ ") else {
        return line.to_string();
    };
    let rest = &line[start + 3..];
    let Some(end) = rest.find(" @@") else {
        return line.to_string();
    };
    let ranges = &rest[..end];
    let tail = &rest[end + 3..];
    let Some((left, right)) = ranges.split_once(" +") else {
        return line.to_string();
    };
    let left = left.strip_prefix('-').unwrap_or(left);
    format!("@@ -{right} +{left} @@{tail}")
}

/// Stage patched targets after a successful apply.
/// `apply --index` precondition: every touched path's working-tree bytes must
/// equal the index blob (Git's `<path>: does not match index`).
fn ensure_paths_match_index(targets: &[String]) -> Result<(), String> {
    use git_internal::internal::{index::Index, object::blob::Blob};

    let index_path = crate::utils::path::index();
    let index =
        Index::load(&index_path).map_err(|error| format!("failed to load the index: {error}"))?;
    let workdir = util::working_dir();
    for target in targets {
        let index_id = index.get(target, 0).map(|entry| entry.hash);
        let path = workdir.join(target);
        let bytes = match fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(format!("failed to read '{target}': {error}")),
        };
        match (index_id, bytes) {
            (Some(id), Some(bytes)) => {
                if Blob::from_content_bytes(bytes).id != id {
                    return Err(format!("{target}: does not match index"));
                }
            }
            (Some(_), None) => return Err(format!("{target}: does not match index")),
            (None, Some(_)) => {
                return Err(format!("{target}: already exists in working directory"));
            }
            (None, None) => {}
        }
    }
    Ok(())
}

/// Stage prepared patch results directly into the index (`--cached`), without
/// touching the working tree.
fn stage_prepared_index(prepared: &PreparedPatch) -> CliResult<()> {
    use git_internal::internal::{
        index::{Index, IndexEntry},
        object::blob::Blob,
    };

    use crate::utils::object_ext::BlobExt;

    let index_path = crate::utils::path::index();
    let mut index = Index::load(&index_path)
        .map_err(|error| CliError::fatal(format!("failed to load the index: {error}")))?;
    let storage = util::objects_storage()
        .map_err(|error| CliError::fatal(format!("failed to open the object store: {error}")))?;
    for (target, content, mode) in prepared.staged_entries() {
        match content {
            Some(bytes) => {
                let blob = Blob::from_content_bytes(bytes);
                blob.try_save().map_err(|error| {
                    CliError::fatal(format!("failed to store blob for '{target}': {error}"))
                })?;
                let mut entry =
                    IndexEntry::new_from_blob(target.clone(), blob.id, blob.data.len() as u32);
                if let Some(mode) = mode {
                    entry.mode = mode;
                }
                crate::utils::index_ext::update_preserving_flags(&mut index, entry);
            }
            None => {
                index.remove(&target, 0);
            }
        }
    }
    storage
        .flush_object_index_batch()
        .map_err(|error| CliError::fatal(format!("failed to flush stored objects: {error}")))?;
    index
        .save(&index_path)
        .map_err(|error| CliError::fatal(format!("failed to save the index: {error}")))?;
    Ok(())
}

/// Write base/ours/theirs conflict stages (1/2/3) for `--3way` conflicts.
fn stage_conflict_sides(sides: &HashMap<String, ConflictSides>) -> CliResult<()> {
    use git_internal::internal::{
        index::{Index, IndexEntry},
        object::blob::Blob,
    };

    use crate::utils::object_ext::BlobExt;

    if sides.is_empty() {
        return Ok(());
    }
    let index_path = crate::utils::path::index();
    let mut index = Index::load(&index_path)
        .map_err(|error| CliError::fatal(format!("failed to load the index: {error}")))?;
    let storage = util::objects_storage()
        .map_err(|error| CliError::fatal(format!("failed to open the object store: {error}")))?;
    for (target, (base, ours, theirs)) in sides {
        for (stage, bytes) in [(1u8, base), (2u8, ours), (3u8, theirs)] {
            let blob = Blob::from_content_bytes(bytes.clone());
            blob.try_save().map_err(|error| {
                CliError::fatal(format!(
                    "failed to store conflict blob for '{target}': {error}"
                ))
            })?;
            let mut entry =
                IndexEntry::new_from_blob(target.clone(), blob.id, blob.data.len() as u32);
            entry.mode = 0o100644;
            entry.flags.stage = stage;
            index.add(entry);
        }
    }
    storage
        .flush_object_index_batch()
        .map_err(|error| CliError::fatal(format!("failed to flush stored objects: {error}")))?;
    index
        .save(&index_path)
        .map_err(|error| CliError::fatal(format!("failed to save the index: {error}")))?;
    Ok(())
}

async fn stage_apply_targets(targets: &[String]) -> CliResult<()> {
    if targets.is_empty() {
        return Ok(());
    }
    let add_args = crate::command::add::AddArgs {
        pathspec: targets.to_vec(),
        force: true,
        ..Default::default()
    };
    crate::command::add::run_add(&add_args)
        .await
        .map(|_| ())
        .map_err(|error| {
            CliError::fatal(format!(
                "failed to stage the applied patch: {}",
                error.message()
            ))
            .with_exit_code(128)
        })
}

/// Force the index modes demanded by `old mode`/`new mode` patch headers.
fn stage_apply_modes(overrides: &[(String, u32)]) -> CliResult<()> {
    if overrides.is_empty() {
        return Ok(());
    }
    let index_path = crate::utils::path::index();
    let mut index = git_internal::internal::index::Index::load(&index_path)
        .map_err(|error| CliError::fatal(format!("failed to load the index: {error}")))?;
    let workdir = util::working_dir();
    let mut changed = false;
    for (target, mode) in overrides {
        let Some(entry) = index.get(target, 0) else {
            continue;
        };
        let current_mode = entry.mode;
        if current_mode & 0o170000 != 0o100000 {
            continue;
        }
        let wanted = if mode & 0o111 != 0 {
            0o100755
        } else {
            0o100644
        };
        if current_mode == wanted {
            continue;
        }
        let hash = entry.hash;
        let mut updated = crate::command::verified_index_entry(
            std::path::Path::new(target),
            hash,
            &workdir,
            None,
        )
        .map_err(|error| CliError::fatal(format!("failed to refresh index entry: {error}")))?;
        updated.mode = wanted;
        index.add(updated);
        changed = true;
    }
    if changed {
        index
            .save(&index_path)
            .map_err(|error| CliError::fatal(format!("failed to save the index: {error}")))?;
    }
    Ok(())
}

/// One file section's in-memory state while preparing a multi-section patch.
#[derive(Debug)]
struct SectionEntry {
    absolute: PathBuf,
    /// `None` = removed by an earlier section.
    content: Option<Vec<u8>>,
    permissions: Option<fs::Permissions>,
    mode: Option<u32>,
}

/// Parse and test-apply a patch against `workdir`, returning the final
/// contents for a later write. File sections targeting the same path are
/// applied in order against the preceding section's in-memory result.
///
/// PD-09 ②: beyond plain unified text diffs, git extended sections are
/// applied — `GIT binary patch` (literal and delta), `rename from`/`to`
/// (with or without content hunks), `copy from`/`to`, mode-only changes
/// (`old mode`/`new mode`), and `new file mode`/`deleted file mode`
/// annotations on content patches.
#[allow(dead_code)]
pub(crate) fn prepare_patch(
    patch_text: &str,
    strip: u32,
    workdir: &Path,
) -> Result<PreparedPatch, PatchPreparationError> {
    prepare_patch_opts(patch_text, strip, workdir, None)
}

/// A resolver from a (possibly abbreviated) blob id in an `index` header
/// to the blob's bytes, when locally present (PD-09 ③).
pub(crate) type BaseBlobResolver<'a> = &'a dyn Fn(&str) -> Option<Vec<u8>>;

/// [`prepare_patch`] with an optional three-way fallback: when a text
/// section does not apply and `resolve_base` finds the `index` header's
/// old blob locally, ours/base/theirs are merged instead — a clean merge
/// applies silently, a conflicting one keeps conflict markers in the
/// prepared content and records the target in `conflicted`.
pub(crate) fn prepare_patch_opts(
    patch_text: &str,
    strip: u32,
    workdir: &Path,
    resolve_base: Option<BaseBlobResolver<'_>>,
) -> Result<PreparedPatch, PatchPreparationError> {
    let invalid = PatchPreparationError::Invalid;
    let mut order: Vec<String> = Vec::new();
    let mut results: HashMap<String, SectionEntry> = HashMap::new();
    let mut conflicted: Vec<String> = Vec::new();
    let mut clean_merges: Vec<String> = Vec::new();
    let mut conflict_side_map: HashMap<String, ConflictSides> = HashMap::new();

    // Load a target's current bytes: the in-memory chain first, then disk.
    // `must_exist` distinguishes modify-style reads from new-file probes.
    fn load_base(
        results: &HashMap<String, SectionEntry>,
        target: &str,
        absolute: &Path,
    ) -> Result<(Vec<u8>, Option<fs::Permissions>), PatchPreparationError> {
        if let Some(entry) = results.get(target) {
            return match &entry.content {
                Some(bytes) => Ok((bytes.clone(), entry.permissions.clone())),
                None => Err(PatchPreparationError::DoesNotApply(format!(
                    "patch target '{target}' was deleted by an earlier section"
                ))),
            };
        }
        let metadata = fs::symlink_metadata(absolute).map_err(|error| {
            PatchPreparationError::DoesNotApply(format!(
                "cannot inspect patch target '{target}': {error}"
            ))
        })?;
        // A symlink's patchable content is its TARGET path, never the
        // linked-to file's bytes.
        let bytes = if metadata.file_type().is_symlink() {
            let link = fs::read_link(absolute).map_err(|error| {
                PatchPreparationError::DoesNotApply(format!(
                    "cannot read symlink patch target '{target}': {error}"
                ))
            })?;
            #[cfg(unix)]
            {
                use std::os::unix::ffi::OsStrExt;
                link.as_os_str().as_bytes().to_vec()
            }
            #[cfg(not(unix))]
            {
                link.display().to_string().into_bytes()
            }
        } else {
            fs::read(absolute).map_err(|error| {
                PatchPreparationError::DoesNotApply(format!(
                    "cannot read patch target '{target}': {error}"
                ))
            })?
        };
        Ok((bytes, Some(metadata.permissions())))
    }

    fn assert_absent(
        results: &HashMap<String, SectionEntry>,
        target: &str,
        absolute: &Path,
    ) -> Result<(), PatchPreparationError> {
        match results.get(target) {
            Some(entry) if entry.content.is_some() => {
                return Err(PatchPreparationError::DoesNotApply(format!(
                    "new-file patch target '{target}' already exists"
                )));
            }
            Some(_) => return Ok(()), // deleted earlier in this patch
            None => {}
        }
        match fs::symlink_metadata(absolute) {
            Ok(_) => Err(PatchPreparationError::DoesNotApply(format!(
                "new-file patch target '{target}' already exists"
            ))),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(PatchPreparationError::Invalid(format!(
                "cannot inspect patch target '{target}': {error}"
            ))),
        }
    }

    for section in split_file_patches(patch_text) {
        let headers = parse_section_headers(&section, strip).map_err(invalid)?;

        if headers.binary_marker && headers.binary.is_none() {
            return Err(invalid(format!(
                "binary patch for '{}' carries no content (regenerate the diff with --binary)",
                headers
                    .display_target()
                    .unwrap_or_else(|| "<unknown>".to_string())
            )));
        }

        // ---- Rename / copy sections (optional content hunks or binary). ----
        if let (Some(from), Some(to)) = headers.rename_pair() {
            let from_abs = validate_patch_target(&from, workdir).map_err(invalid)?;
            let to_abs = validate_patch_target(&to, workdir).map_err(invalid)?;
            let (base, permissions) = load_base(&results, &from, &from_abs)?;
            assert_absent(&results, &to, &to_abs)?;
            let applied = apply_section_content(&section, &headers, base, &to, resolve_base)
                .map_err(map_reason)?;
            let content = applied.content;
            let mode = applied.mode;
            if applied.conflicted {
                conflicted.push(to.clone());
            }
            if applied.clean_merge {
                clean_merges.push(to.clone());
            }
            if let Some(sides) = applied.conflict_sides {
                conflict_side_map.insert(to.clone(), sides);
            }
            if !results.contains_key(&from) {
                order.push(from.clone());
            }
            results.insert(
                from,
                SectionEntry {
                    absolute: from_abs,
                    content: None,
                    permissions: None,
                    mode: None,
                },
            );
            if !results.contains_key(&to) {
                order.push(to.clone());
            }
            results.insert(
                to,
                SectionEntry {
                    absolute: to_abs,
                    content: Some(content),
                    permissions,
                    mode,
                },
            );
            continue;
        }
        if let (Some(from), Some(to)) = headers.copy_pair() {
            let from_abs = validate_patch_target(&from, workdir).map_err(invalid)?;
            let to_abs = validate_patch_target(&to, workdir).map_err(invalid)?;
            let (base, permissions) = load_base(&results, &from, &from_abs)?;
            assert_absent(&results, &to, &to_abs)?;
            let applied = apply_section_content(&section, &headers, base, &to, resolve_base)
                .map_err(map_reason)?;
            let content = applied.content;
            let mode = applied.mode;
            if applied.conflicted {
                conflicted.push(to.clone());
            }
            if applied.clean_merge {
                clean_merges.push(to.clone());
            }
            if let Some(sides) = applied.conflict_sides {
                conflict_side_map.insert(to.clone(), sides);
            }
            if !results.contains_key(&to) {
                order.push(to.clone());
            }
            results.insert(
                to,
                SectionEntry {
                    absolute: to_abs,
                    content: Some(content),
                    permissions,
                    mode,
                },
            );
            continue;
        }

        // ---- Single-target sections. ----
        let target = headers
            .single_target(&section, strip)
            .map_err(PatchPreparationError::Invalid)?;
        let absolute =
            validate_patch_target(&target, workdir).map_err(PatchPreparationError::Invalid)?;

        // Content-less sections: empty-file creation/deletion (git emits
        // no hunks for a zero-byte file) and mode-only changes.
        if !headers.has_hunks && headers.binary.is_none() {
            if let Some(new_file_mode) = headers.new_file_mode {
                if new_file_mode & 0o170000 == 0o120000 {
                    return Err(invalid(format!(
                        "empty symlink patch for '{target}' carries no target"
                    )));
                }
                assert_absent(&results, &target, &absolute)?;
                if !results.contains_key(&target) {
                    order.push(target.clone());
                }
                results.insert(
                    target,
                    SectionEntry {
                        absolute,
                        content: Some(Vec::new()),
                        permissions: None,
                        mode: Some(new_file_mode),
                    },
                );
                continue;
            }
            if headers.deleted_file_mode.is_some() {
                let (bytes, _permissions) = load_base(&results, &target, &absolute)?;
                if !bytes.is_empty() {
                    return Err(PatchPreparationError::DoesNotApply(format!(
                        "deletion patch did not remove all content from '{target}'"
                    )));
                }
                if !results.contains_key(&target) {
                    order.push(target.clone());
                }
                results.insert(
                    target,
                    SectionEntry {
                        absolute,
                        content: None,
                        permissions: None,
                        mode: None,
                    },
                );
                continue;
            }
            let (Some(_old), Some(new_mode)) = (headers.old_mode, headers.new_mode) else {
                return Err(invalid(format!(
                    "patch section for '{target}' carries no content and no mode change"
                )));
            };
            let (bytes, permissions) = load_base(&results, &target, &absolute)?;
            if !results.contains_key(&target) {
                order.push(target.clone());
            }
            results.insert(
                target,
                SectionEntry {
                    absolute,
                    content: Some(bytes),
                    permissions,
                    mode: Some(new_mode),
                },
            );
            continue;
        }

        let is_new_file = headers.is_new_file(&section);
        let is_deletion = headers.is_deletion(&section);
        let (base, permissions) = if is_new_file {
            assert_absent(&results, &target, &absolute)?;
            (Vec::new(), None)
        } else {
            load_base(&results, &target, &absolute)?
        };

        let applied = apply_section_content(&section, &headers, base, &target, resolve_base)
            .map_err(map_reason)?;
        let content = applied.content;
        let mode = applied.mode;
        if applied.conflicted {
            conflicted.push(target.clone());
        }
        if applied.clean_merge {
            clean_merges.push(target.clone());
        }
        if let Some(sides) = applied.conflict_sides {
            conflict_side_map.insert(target.clone(), sides);
        }
        if is_deletion && !content.is_empty() {
            return Err(PatchPreparationError::DoesNotApply(format!(
                "deletion patch did not remove all content from '{target}'"
            )));
        }

        // A later content section on the same path must not drop an
        // earlier section's mode override (chained chmod + edit).
        let mode = mode.or_else(|| results.get(&target).and_then(|entry| entry.mode));
        if !results.contains_key(&target) {
            order.push(target.clone());
        }
        results.insert(
            target,
            SectionEntry {
                absolute,
                content: if is_deletion { None } else { Some(content) },
                permissions,
                mode,
            },
        );
    }

    let files = order
        .into_iter()
        .filter_map(|target| {
            results.remove(&target).map(|entry| PreparedFile {
                target,
                absolute: entry.absolute,
                content: entry.content,
                permissions: entry.permissions,
                mode: entry.mode,
            })
        })
        .collect();
    Ok(PreparedPatch {
        files,
        conflicted,
        clean_merges,
        conflict_sides: conflict_side_map,
    })
}

/// Map a section-application failure string onto the preparation error
/// kind: content mismatches are "does not apply", everything else is
/// invalid input.
fn map_reason(reason: SectionApplyError) -> PatchPreparationError {
    match reason {
        SectionApplyError::DoesNotApply(message) => PatchPreparationError::DoesNotApply(message),
        SectionApplyError::Invalid(message) => PatchPreparationError::Invalid(message),
    }
}

pub(crate) enum SectionApplyError {
    Invalid(String),
    DoesNotApply(String),
}

/// Produce a section's final bytes from `base`: text hunks through the
/// diffy engine (UTF-8 required), binary sections through the literal /
/// delta decoder. Returns the bytes, the git-mode override, and whether
/// the three-way fallback left conflict markers in the bytes.
pub(crate) struct SectionApplied {
    pub content: Vec<u8>,
    pub mode: Option<u32>,
    pub conflicted: bool,
    pub clean_merge: bool,
    pub conflict_sides: Option<ConflictSides>,
}

pub(crate) fn apply_section_content(
    section: &str,
    headers: &SectionHeaders,
    base: Vec<u8>,
    target: &str,
    resolve_base: Option<BaseBlobResolver<'_>>,
) -> Result<SectionApplied, SectionApplyError> {
    let mode = headers
        .new_mode
        .or(headers.new_file_mode)
        .filter(|mode| matches!(mode & 0o170000, 0o100000 | 0o120000));
    if let Some(binary) = &headers.binary {
        let bytes = match binary {
            BinaryHunk::Literal { size, data } => {
                if data.len() as u64 != *size {
                    return Err(SectionApplyError::Invalid(format!(
                        "binary literal for '{target}' declares {size} bytes but carries {}",
                        data.len()
                    )));
                }
                data.clone()
            }
            BinaryHunk::Delta { size, data } => {
                if data.len() as u64 != *size {
                    return Err(SectionApplyError::Invalid(format!(
                        "binary delta for '{target}' declares {size} bytes but carries {}",
                        data.len()
                    )));
                }
                // Preimage verification (git parity): a delta generated
                // from a different base of the SAME length would corrupt
                // silently — the `index` header's old blob id must match
                // the current content.
                if let Some(expected) = headers.index_old.as_deref() {
                    let actual = git_internal::internal::object::blob::Blob::from_content_bytes(
                        base.clone(),
                    )
                    .id
                    .to_string();
                    if !actual.starts_with(expected) {
                        return Err(SectionApplyError::DoesNotApply(format!(
                            "current content of '{target}' does not match the binary patch's \
                             recorded preimage {expected}"
                        )));
                    }
                }
                apply_git_delta(&base, data).map_err(|reason| {
                    SectionApplyError::DoesNotApply(format!(
                        "binary delta does not apply to '{target}': {reason}"
                    ))
                })?
            }
        };
        return Ok(SectionApplied {
            content: bytes,
            mode,
            conflicted: false,
            clean_merge: false,
            conflict_sides: None,
        });
    }
    if !headers.has_hunks {
        // Pure rename/copy without content edits.
        return Ok(SectionApplied {
            content: base,
            mode,
            conflicted: false,
            clean_merge: false,
            conflict_sides: None,
        });
    }
    let base_text = String::from_utf8(base).map_err(|_| {
        SectionApplyError::DoesNotApply(format!(
            "cannot apply a text patch to binary content in '{target}'"
        ))
    })?;
    let patch = diffy::Patch::from_str(section)
        .map_err(|error| SectionApplyError::Invalid(format!("malformed patch: {error}")))?;
    match diffy::apply(&base_text, &patch) {
        Ok(result) => Ok(SectionApplied {
            content: result.into_bytes(),
            mode,
            conflicted: false,
            clean_merge: false,
            conflict_sides: None,
        }),
        Err(_) => {
            // PD-09 ③: three-way fallback — merge ours (current content)
            // against the patch applied to the RECORDED base blob. Only a
            // locally resolvable `index` old id enables it; anything
            // missing keeps the plain does-not-apply refusal.
            let fallback = resolve_base
                .zip(headers.index_old.as_deref())
                .and_then(|(resolver, old_id)| resolver(old_id))
                .and_then(|base_bytes| String::from_utf8(base_bytes).ok())
                .and_then(|recorded_base| {
                    diffy::apply(&recorded_base, &patch)
                        .ok()
                        .map(|theirs| (recorded_base, theirs))
                });
            let Some((recorded_base, theirs)) = fallback else {
                return Err(SectionApplyError::DoesNotApply(format!(
                    "patch does not apply to '{target}'"
                )));
            };
            match diffy::merge_bytes(
                recorded_base.as_bytes(),
                base_text.as_bytes(),
                theirs.as_bytes(),
            ) {
                Ok(merged) => Ok(SectionApplied {
                    content: merged,
                    mode,
                    conflicted: false,
                    clean_merge: true,
                    conflict_sides: None,
                }),
                Err(conflict) => Ok(SectionApplied {
                    content: conflict,
                    mode,
                    conflicted: true,
                    clean_merge: false,
                    conflict_sides: Some((
                        recorded_base.into_bytes(),
                        base_text.into_bytes(),
                        theirs.into_bytes(),
                    )),
                }),
            }
        }
    }
}

/// Return every safe target named by a patch without reading the worktree.
/// Rename sections contribute BOTH sides (the source is deleted); copy
/// sections contribute the destination.
pub(crate) fn patch_targets(patch_text: &str, strip: u32) -> Result<Vec<String>, String> {
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    let mut push = |target: String| {
        if seen.insert(target.clone()) {
            targets.push(target);
        }
    };
    for section in split_file_patches(patch_text) {
        let headers = parse_section_headers(&section, strip)?;
        if let (Some(from), Some(to)) = headers.rename_pair() {
            resolve_safe(&from, &util::working_dir())?;
            resolve_safe(&to, &util::working_dir())?;
            push(from);
            push(to);
            continue;
        }
        if let (Some(from), Some(to)) = headers.copy_pair() {
            resolve_safe(&from, &util::working_dir())?;
            resolve_safe(&to, &util::working_dir())?;
            push(to);
            continue;
        }
        let target = headers.single_target(&section, strip)?;
        resolve_safe(&target, &util::working_dir())?;
        push(target);
    }
    Ok(targets)
}

/// Read the patch from the given files (concatenated) or from stdin, enforcing
/// the size cap.
fn read_patch(patches: &[String]) -> Result<String, String> {
    let mut buffer = Vec::new();
    if patches.is_empty() {
        io::stdin()
            .take(MAX_PATCH_BYTES as u64 + 1)
            .read_to_end(&mut buffer)
            .map_err(|err| format!("failed to read patch from stdin: {err}"))?;
    } else {
        for path in patches {
            let bytes =
                fs::read(path).map_err(|err| format!("cannot read patch '{path}': {err}"))?;
            buffer.extend_from_slice(&bytes);
            if buffer.len() > MAX_PATCH_BYTES {
                break;
            }
        }
    }
    if buffer.len() > MAX_PATCH_BYTES {
        return Err(format!(
            "patch exceeds the {} MiB limit",
            MAX_PATCH_BYTES / (1024 * 1024)
        ));
    }
    String::from_utf8(buffer).map_err(|_| "patch is not valid UTF-8".to_string())
}

/// PD-09 ②: parsed git extended headers of one file section.
#[derive(Debug, Default)]
pub(crate) struct SectionHeaders {
    /// `diff --git a/<old> b/<new>` sides, already `-p`-stripped.
    git_old: Option<String>,
    git_new: Option<String>,
    old_mode: Option<u32>,
    new_mode: Option<u32>,
    new_file_mode: Option<u32>,
    deleted_file_mode: Option<u32>,
    rename_from: Option<String>,
    rename_to: Option<String>,
    copy_from: Option<String>,
    copy_to: Option<String>,
    binary: Option<BinaryHunk>,
    /// `index <old>..<new>[ <mode>]` blob ids (possibly abbreviated) —
    /// the three-way fallback's base/theirs anchors (PD-09 ③).
    index_old: Option<String>,
    /// A content-less `Binary files … differ` marker.
    binary_marker: bool,
    /// The section carries `--- `/`+++ ` unified content hunks.
    has_hunks: bool,
}

impl SectionHeaders {
    fn rename_pair(&self) -> (Option<String>, Option<String>) {
        (self.rename_from.clone(), self.rename_to.clone())
    }

    fn copy_pair(&self) -> (Option<String>, Option<String>) {
        (self.copy_from.clone(), self.copy_to.clone())
    }

    fn display_target(&self) -> Option<String> {
        self.git_new
            .clone()
            .or_else(|| self.git_old.clone())
            .or_else(|| self.rename_to.clone())
            .or_else(|| self.copy_to.clone())
    }

    fn is_new_file(&self, section: &str) -> bool {
        self.new_file_mode.is_some()
            || diffy::Patch::from_str(section)
                .ok()
                .is_some_and(|patch| patch.original() == Some("/dev/null"))
    }

    fn is_deletion(&self, section: &str) -> bool {
        self.deleted_file_mode.is_some()
            || diffy::Patch::from_str(section)
                .ok()
                .is_some_and(|patch| patch.modified() == Some("/dev/null"))
    }

    /// Resolve a non-rename/copy section's single target path. Content
    /// hunks keep the diffy-derived name (the `+++`/`---` lines, which
    /// handle `/dev/null` sides); header-only and binary sections fall
    /// back to the `diff --git` sides.
    fn single_target(&self, section: &str, strip: u32) -> Result<String, String> {
        if self.has_hunks {
            let patch = diffy::Patch::from_str(section)
                .map_err(|error| format!("malformed patch: {error}"))?;
            return patch_target(&patch, strip);
        }
        if self.deleted_file_mode.is_some() {
            return self
                .git_old
                .clone()
                .ok_or_else(|| "patch section is missing its old filename".to_string());
        }
        self.git_new
            .clone()
            .or_else(|| self.git_old.clone())
            .ok_or_else(|| "patch section is missing a target filename".to_string())
    }
}

/// A decoded `GIT binary patch` forward hunk (inflated payload).
#[derive(Debug)]
enum BinaryHunk {
    Literal { size: u64, data: Vec<u8> },
    Delta { size: u64, data: Vec<u8> },
}

/// Parse one file section's git extended headers (`strip` applied to
/// every path). `rename from`/`copy from` lines carry NO `a/`/`b/`
/// prefix, so — like `git apply` — one fewer component is stripped from
/// them.
fn parse_section_headers(section: &str, strip: u32) -> Result<SectionHeaders, String> {
    let mut headers = SectionHeaders::default();
    let bare_strip = strip.saturating_sub(1);
    let mut lines = section.lines().peekable();
    while let Some(line) = lines.next() {
        if line.starts_with("--- ") {
            headers.has_hunks = true;
            break;
        }
        if line.starts_with("@@ ") {
            headers.has_hunks = true;
            break;
        }
        if let Some(rest) = line.strip_prefix("diff --git ") {
            let (old, new) = parse_diff_git_sides(rest, strip)?;
            headers.git_old = old;
            headers.git_new = new;
        } else if let Some(rest) = line.strip_prefix("old mode ") {
            headers.old_mode = Some(parse_git_mode(rest)?);
        } else if let Some(rest) = line.strip_prefix("new mode ") {
            headers.new_mode = Some(parse_git_mode(rest)?);
        } else if let Some(rest) = line.strip_prefix("new file mode ") {
            headers.new_file_mode = Some(parse_git_mode(rest)?);
        } else if let Some(rest) = line.strip_prefix("deleted file mode ") {
            headers.deleted_file_mode = Some(parse_git_mode(rest)?);
        } else if let Some(rest) = line.strip_prefix("rename from ") {
            headers.rename_from = Some(strip_bare_header_path(rest, bare_strip)?);
        } else if let Some(rest) = line.strip_prefix("rename to ") {
            headers.rename_to = Some(strip_bare_header_path(rest, bare_strip)?);
        } else if let Some(rest) = line.strip_prefix("copy from ") {
            headers.copy_from = Some(strip_bare_header_path(rest, bare_strip)?);
        } else if let Some(rest) = line.strip_prefix("copy to ") {
            headers.copy_to = Some(strip_bare_header_path(rest, bare_strip)?);
        } else if line == "GIT binary patch" {
            headers.binary = Some(parse_binary_hunk(&mut lines)?);
            break;
        } else if line.starts_with("Binary files ") && line.ends_with(" differ") {
            headers.binary_marker = true;
        } else if let Some(rest) = line.strip_prefix("index ")
            && let Some((old_id, _)) = rest
                .split_whitespace()
                .next()
                .and_then(|ids| ids.split_once(".."))
            && !old_id.is_empty()
            && old_id.bytes().all(|b| b.is_ascii_hexdigit())
            && !old_id.bytes().all(|b| b == b'0')
        {
            headers.index_old = Some(old_id.to_string());
        }
        // `index`, `similarity index`, `dissimilarity index`, mail noise —
        // ignored.
    }
    if headers.rename_from.is_some() != headers.rename_to.is_some() {
        return Err("rename patch is missing one of its 'rename from'/'rename to' lines".into());
    }
    if headers.copy_from.is_some() != headers.copy_to.is_some() {
        return Err("copy patch is missing one of its 'copy from'/'copy to' lines".into());
    }
    Ok(headers)
}

/// Split `a/<old> b/<new>` from a `diff --git ` line. Quoted paths
/// (special characters) are refused with an actionable message rather
/// than mis-parsed.
fn parse_diff_git_sides(
    rest: &str,
    strip: u32,
) -> Result<(Option<String>, Option<String>), String> {
    if rest.starts_with('"') {
        return Err(
            "quoted (special-character) paths in 'diff --git' headers are not supported".into(),
        );
    }
    let Some((left, right)) = rest.split_once(" b/") else {
        return Ok((None, None));
    };
    if strip == 0 {
        // git -p0 uses the header names verbatim (`a/x`, `b/x`), matching
        // how the same run's `---`/`+++` hunk names resolve.
        let old = Some(strip_header_path(left, 0)?);
        let new = Some(strip_header_path(&format!("b/{right}"), 0)?);
        return Ok((old, new));
    }
    let old = left.strip_prefix("a/").map(str::to_string);
    let old = match old {
        Some(path) => Some(strip_header_path(&path, strip - 1)?),
        None => None,
    };
    let new = Some(strip_header_path(right, strip - 1)?);
    Ok((old, new))
}

/// Strip components from an already-deprefixed header path.
fn strip_header_path(path: &str, remaining_strip: u32) -> Result<String, String> {
    if Path::new(path).is_absolute() || path.contains('\0') {
        return Err(format!("refusing absolute or NUL patch path '{path}'"));
    }
    if remaining_strip == 0 {
        return Ok(path.to_string());
    }
    strip_path(path, remaining_strip)
        .ok_or_else(|| format!("cannot strip {remaining_strip} path component(s) from '{path}'"))
}

/// Strip a bare (`rename from`-style, unprefixed) header path.
fn strip_bare_header_path(path: &str, bare_strip: u32) -> Result<String, String> {
    strip_header_path(path.trim(), bare_strip)
}

fn parse_git_mode(rest: &str) -> Result<u32, String> {
    let mode = u32::from_str_radix(rest.trim(), 8)
        .map_err(|_| format!("invalid git file mode '{}'", rest.trim()))?;
    match mode & 0o170000 {
        0o100000 | 0o120000 | 0o160000 => Ok(mode),
        _ => Err(format!("unsupported git file mode '{}'", rest.trim())),
    }
}

/// Decode the FORWARD hunk after a `GIT binary patch` line: a
/// `literal <n>` / `delta <n>` marker followed by base85 lines up to a
/// blank line. The reverse hunk (used by `apply -R`) is skipped.
fn parse_binary_hunk<'a, I: Iterator<Item = &'a str>>(
    lines: &mut std::iter::Peekable<I>,
) -> Result<BinaryHunk, String> {
    let marker = lines
        .next()
        .ok_or_else(|| "binary patch is missing its literal/delta marker".to_string())?;
    let (kind, size) = marker
        .split_once(' ')
        .ok_or_else(|| format!("malformed binary patch marker '{marker}'"))?;
    let size: u64 = size
        .trim()
        .parse()
        .map_err(|_| format!("malformed binary patch size in '{marker}'"))?;
    if size > MAX_PATCH_BYTES as u64 {
        return Err(format!(
            "binary patch payload exceeds the {} MiB limit",
            MAX_PATCH_BYTES / (1024 * 1024)
        ));
    }
    let mut deflated: Vec<u8> = Vec::new();
    for line in lines.by_ref() {
        if line.is_empty() {
            break;
        }
        decode_base85_line(line, &mut deflated)?;
    }
    let data = inflate_bounded(&deflated, size)?;
    match kind {
        "literal" => Ok(BinaryHunk::Literal { size, data }),
        "delta" => Ok(BinaryHunk::Delta { size, data }),
        other => Err(format!("unsupported binary patch kind '{other}'")),
    }
}

/// Git's base85 alphabet (`base85.c`).
const BASE85_ALPHABET: &[u8; 85] =
    b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz!#$%&()*+-;<=>?@^_`{|}~";

fn base85_value(byte: u8) -> Option<u32> {
    BASE85_ALPHABET
        .iter()
        .position(|&candidate| candidate == byte)
        .map(|index| index as u32)
}

/// Decode one binary-patch line: a length prefix (`A`-`Z` = 1–26,
/// `a`-`z` = 27–52 bytes) followed by base85 groups of five characters
/// encoding four bytes each.
fn decode_base85_line(line: &str, out: &mut Vec<u8>) -> Result<(), String> {
    let bytes = line.as_bytes();
    let Some(&len_char) = bytes.first() else {
        return Err("empty binary patch line".into());
    };
    let byte_len = match len_char {
        b'A'..=b'Z' => (len_char - b'A' + 1) as usize,
        b'a'..=b'z' => (len_char - b'a' + 27) as usize,
        _ => return Err(format!("malformed binary patch line length in '{line}'")),
    };
    let payload = &bytes[1..];
    if payload.len() != byte_len.div_ceil(4) * 5 {
        return Err(format!("malformed binary patch line '{line}'"));
    }
    let mut decoded: Vec<u8> = Vec::with_capacity(byte_len.div_ceil(4) * 4);
    for group in payload.chunks(5) {
        let mut acc: u32 = 0;
        for &ch in group {
            let value = base85_value(ch)
                .ok_or_else(|| format!("invalid base85 character '{}'", ch as char))?;
            acc = acc
                .checked_mul(85)
                .and_then(|acc| acc.checked_add(value))
                .ok_or_else(|| format!("base85 overflow in line '{line}'"))?;
        }
        decoded.extend_from_slice(&acc.to_be_bytes());
    }
    decoded.truncate(byte_len);
    out.extend_from_slice(&decoded);
    Ok(())
}

/// Inflate a zlib stream with a hard output bound (the declared payload
/// size, which itself is capped) so hostile input cannot balloon memory.
fn inflate_bounded(deflated: &[u8], expected: u64) -> Result<Vec<u8>, String> {
    use flate2::read::ZlibDecoder;

    let mut data = Vec::new();
    ZlibDecoder::new(deflated)
        .take(expected + 1)
        .read_to_end(&mut data)
        .map_err(|error| format!("binary patch payload does not inflate: {error}"))?;
    if data.len() as u64 > expected {
        return Err("binary patch payload is larger than declared".into());
    }
    Ok(data)
}

/// Apply a git pack-style delta to `base` (varint base/result sizes, then
/// copy/insert opcodes).
fn apply_git_delta(base: &[u8], delta: &[u8]) -> Result<Vec<u8>, String> {
    fn varint(data: &[u8], pos: &mut usize) -> Result<u64, String> {
        let mut value: u64 = 0;
        let mut shift = 0u32;
        loop {
            let byte = *data
                .get(*pos)
                .ok_or_else(|| "truncated delta header".to_string())?;
            *pos += 1;
            value |= u64::from(byte & 0x7f) << shift;
            if byte & 0x80 == 0 {
                return Ok(value);
            }
            shift += 7;
            if shift > 63 {
                return Err("oversized delta header".into());
            }
        }
    }

    let mut pos = 0usize;
    let base_size = varint(delta, &mut pos)?;
    if base_size != base.len() as u64 {
        return Err(format!(
            "delta expects a {base_size}-byte base but the current content is {} bytes",
            base.len()
        ));
    }
    let result_size = varint(delta, &mut pos)?;
    if result_size > MAX_PATCH_BYTES as u64 {
        return Err(format!(
            "delta result exceeds the {} MiB limit",
            MAX_PATCH_BYTES / (1024 * 1024)
        ));
    }
    let mut result: Vec<u8> = Vec::with_capacity(result_size as usize);
    while pos < delta.len() {
        let opcode = delta[pos];
        pos += 1;
        if opcode & 0x80 != 0 {
            // Copy from base: optional offset/size bytes selected by bits.
            let mut offset: u64 = 0;
            for bit in 0..4 {
                if opcode & (1 << bit) != 0 {
                    let byte = *delta
                        .get(pos)
                        .ok_or_else(|| "truncated delta copy op".to_string())?;
                    pos += 1;
                    offset |= u64::from(byte) << (8 * bit);
                }
            }
            let mut size: u64 = 0;
            for bit in 0..3 {
                if opcode & (0x10 << bit) != 0 {
                    let byte = *delta
                        .get(pos)
                        .ok_or_else(|| "truncated delta copy op".to_string())?;
                    pos += 1;
                    size |= u64::from(byte) << (8 * bit);
                }
            }
            if size == 0 {
                size = 0x10000;
            }
            let start = usize::try_from(offset).map_err(|_| "delta copy overflow".to_string())?;
            let len = usize::try_from(size).map_err(|_| "delta copy overflow".to_string())?;
            let end = start
                .checked_add(len)
                .ok_or_else(|| "delta copy overflow".to_string())?;
            let slice = base
                .get(start..end)
                .ok_or_else(|| "delta copy reads outside the base".to_string())?;
            result.extend_from_slice(slice);
        } else if opcode != 0 {
            let len = opcode as usize;
            let slice = delta
                .get(pos..pos + len)
                .ok_or_else(|| "truncated delta insert op".to_string())?;
            pos += len;
            result.extend_from_slice(slice);
        } else {
            return Err("delta opcode 0 is reserved".into());
        }
        if result.len() as u64 > result_size {
            return Err("delta result is larger than declared".into());
        }
    }
    if result.len() as u64 != result_size {
        return Err("delta result is smaller than declared".into());
    }
    Ok(result)
}

/// PD-09 ③: every `index` header old-blob id named by the patch, for
/// callers that pre-resolve bases through the FULL object store
/// (packs included) before invoking [`prepare_patch_opts`].
pub(crate) fn patch_index_old_ids(patch_text: &str, strip: u32) -> Vec<String> {
    let mut ids = Vec::new();
    for section in split_file_patches(patch_text) {
        if let Ok(headers) = parse_section_headers(&section, strip)
            && let Some(old_id) = headers.index_old
            && !ids.contains(&old_id)
        {
            ids.push(old_id);
        }
    }
    ids
}

/// PD-09 ③: resolve a (possibly abbreviated) blob id from an `index`
/// header against the LOCAL loose-object store and return the blob's
/// bytes. Abbreviations resolve only when unambiguous; anything not
/// locally present returns `None` (the caller keeps the plain refusal).
pub(crate) fn resolve_local_base_blob(oid_prefix: &str) -> Option<Vec<u8>> {
    let storage = util::try_get_storage_path(None).ok()?;
    let full = resolve_loose_object_prefix(&storage, oid_prefix)?;
    let (bytes, truncated) =
        crate::utils::object::read_git_object_bounded(&storage, &full, MAX_PATCH_BYTES as u64)
            .ok()?;
    (!truncated).then_some(bytes)
}

fn resolve_loose_object_prefix(
    storage: &Path,
    prefix: &str,
) -> Option<git_internal::hash::ObjectHash> {
    if prefix.len() < 4 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    if let Ok(full) = crate::internal::object_format::parse_repo_oid(prefix) {
        return Some(full);
    }
    let fanout = storage.join("objects").join(&prefix[..2]);
    let rest = &prefix[2..];
    let mut matched: Option<String> = None;
    for entry in fs::read_dir(fanout).ok()?.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else { continue };
        if name.starts_with(rest) {
            if matched.is_some() {
                return None; // ambiguous
            }
            matched = Some(format!("{}{name}", &prefix[..2]));
        }
    }
    crate::internal::object_format::parse_repo_oid(&matched?).ok()
}

/// Split a (possibly multi-file) unified diff into per-file sections. Git-style
/// diffs split on `diff --git `; plain diffs split on a `--- ` line immediately
/// followed by `+++ ` (so a `--- ...` content-removal line is not mistaken for a
/// file header).
fn split_file_patches(patch: &str) -> Vec<String> {
    let lines: Vec<&str> = patch.lines().collect();
    let git_style = lines.iter().any(|line| line.starts_with("diff --git "));

    let mut starts: Vec<usize> = Vec::new();
    for (index, line) in lines.iter().enumerate() {
        let is_start = if git_style {
            line.starts_with("diff --git ")
        } else {
            line.starts_with("--- ")
                && lines
                    .get(index + 1)
                    .is_some_and(|next| next.starts_with("+++ "))
        };
        if is_start {
            starts.push(index);
        }
    }

    if starts.is_empty() {
        return vec![patch.to_string()];
    }

    let mut sections = Vec::new();
    for (i, &start) in starts.iter().enumerate() {
        let end = starts.get(i + 1).copied().unwrap_or(lines.len());
        sections.push(format!("{}\n", lines[start..end].join("\n")));
    }
    sections
}

/// Resolve the target path of a file patch (the modified side, or the original
/// side for a deletion), stripping `strip` leading components.
fn patch_target(patch: &diffy::Patch<'_, str>, strip: u32) -> Result<String, String> {
    let deleted = patch.modified() == Some("/dev/null");
    let raw = if deleted {
        patch.original()
    } else {
        patch.modified()
    };
    let raw = raw.ok_or_else(|| "patch is missing a target filename".to_string())?;
    // Validate the RAW path before `-p<n>` stripping: an absolute path must be
    // rejected even when stripping would turn it into a relative one (e.g.
    // `/abs/file` with `-p1` -> `abs/file`).
    if Path::new(raw).is_absolute() || raw.contains('\0') {
        return Err(format!("refusing absolute or NUL patch path '{raw}'"));
    }
    strip_path(raw, strip)
        .ok_or_else(|| format!("cannot strip {strip} path component(s) from '{raw}'"))
}

/// Strip `n` leading slash-separated components from a patch path.
fn strip_path(path: &str, n: u32) -> Option<String> {
    let components: Vec<&str> = path.split('/').collect();
    if components.len() <= n as usize {
        return None;
    }
    Some(components[n as usize..].join("/"))
}

/// Resolve a stripped patch path against the worktree and reject anything that
/// escapes it, contains a NUL, or points inside `.libra/`.
fn resolve_safe(target: &str, workdir: &Path) -> Result<PathBuf, String> {
    if target.is_empty() || target.contains('\0') {
        return Err(format!("invalid target path '{target}'"));
    }
    let target_path = Path::new(target);
    if target_path.is_absolute()
        || target
            .split('/')
            .any(|component| component.is_empty() || matches!(component, "." | ".."))
        || target_path.components().any(|component| {
            matches!(
                component,
                Component::Prefix(_)
                    | Component::RootDir
                    | Component::ParentDir
                    | Component::CurDir
            )
        })
    {
        return Err(format!(
            "refusing non-canonical or outside-worktree patch path: '{target}'"
        ));
    }
    let targets_internal_storage = target_path.components().next().is_some_and(|component| {
        matches!(
            component,
            Component::Normal(name)
                if name
                    .to_str()
                    .is_some_and(|name| name.eq_ignore_ascii_case(util::ROOT_DIR))
        )
    });
    if targets_internal_storage {
        return Err(format!(
            "refusing to patch inside {}: '{target}'",
            util::ROOT_DIR
        ));
    }
    let absolute = workdir.join(target_path);
    if !util::is_sub_path(&absolute, workdir) {
        return Err(format!(
            "refusing to patch outside the working tree: '{target}'"
        ));
    }
    Ok(absolute)
}

/// Validate a previously parsed target against the current worktree path
/// topology. Sequencer cleanup re-runs this because an ancestor can be replaced
/// with a symlink while an operation is stopped.
pub(crate) fn validate_patch_target(target: &str, workdir: &Path) -> Result<PathBuf, String> {
    let absolute = resolve_safe(target, workdir)?;
    reject_symlink_components(target, workdir)?;
    Ok(absolute)
}

/// Refuse existing symlinks in a patch path. Lexical containment alone is not
/// sufficient for a writing caller because `dir/link/file` could otherwise
/// escape the worktree through `link`.
fn reject_symlink_components(target: &str, workdir: &Path) -> Result<(), String> {
    let mut current = workdir.to_path_buf();
    for component in Path::new(target).components() {
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "refusing symlink patch path '{}': '{}' is a symlink",
                    target,
                    current.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => {
                return Err(format!(
                    "cannot inspect patch path '{}': {error}",
                    current.display()
                ));
            }
        }
    }
    Ok(())
}
