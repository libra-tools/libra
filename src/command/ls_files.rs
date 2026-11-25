//! Implements `ls-files` to list files in the index with basic filters.

use std::{collections::HashSet, fs, io::Write, path::Path};

use clap::Parser;
use git_internal::internal::{index::Index, object::blob::Blob};
use serde::Serialize;

use crate::{
    command::{load_object, read_worktree_blob_bytes},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        ignore,
        output::{OutputConfig, emit_json_data, stdout_write_error},
        path,
        pathspec::{PathspecError, PathspecSet},
        util,
    },
};

/// `--help` examples for ls-files
pub const LS_FILES_EXAMPLES: &str = "\
EXAMPLES:
    libra ls-files                      List all files in the index (cached)
    libra ls-files --cached             Show only files staged in the index
    libra ls-files -d                   Show only deleted files (-d = --deleted)
    libra ls-files -m                   Show only modified files (-m = --modified)
    libra ls-files --stage              Include stage information (for conflicts)
    libra ls-files --others             Show untracked files
    libra ls-files --exclude-standard   Exclude files matching standard ignore sources
    libra ls-files -o -x '*.log'        Show untracked files except ones matching a pattern
    libra ls-files -o -X .gitignore-extra  Read extra exclude patterns from a file
    libra ls-files -i -o --exclude-standard  List only the ignored untracked files
    libra ls-files tracked-dir          Limit output to a pathspec
    libra ls-files --error-unmatch src  Fail if a pathspec matches nothing
    libra ls-files -z --others          Emit NUL-delimited records for scripts
    libra ls-files -s                   Short output with stage info
    libra ls-files -s --abbrev=8        Abbreviate object names to 8 digits
    libra ls-files -t                   Prefix each path with a status tag
    libra ls-files -u                   Show only unmerged (conflict) entries
    libra ls-files --eol                Show index/worktree line-ending info per file";

#[derive(Parser, Debug)]
#[command(after_help = LS_FILES_EXAMPLES)]
pub struct LsFilesArgs {
    /// Show only staged (cached) files in the index
    #[clap(long, short = 'c')]
    pub cached: bool,

    /// Show only deleted files
    #[clap(long, short = 'd')]
    pub deleted: bool,

    /// Show only modified files
    #[clap(long, short = 'm')]
    pub modified: bool,

    /// Include stage information for conflict resolution
    #[clap(long)]
    pub stage: bool,

    /// Show untracked files (not in index)
    #[clap(long, short = 'o')]
    pub others: bool,

    /// Show only ignored files (the inverse of the usual listing). Must be combined
    /// with `--others` (ignored untracked files) and/or `--cached` (tracked files
    /// that match an exclude pattern), and needs an exclude source —
    /// `--exclude-standard` or an explicit `-x`/`-X` pattern.
    #[clap(long = "ignored", short = 'i')]
    pub ignored: bool,

    /// Exclude files matching standard Git/Libra ignore sources
    #[clap(long)]
    pub exclude_standard: bool,

    /// Skip untracked files matching `<pattern>` (gitignore syntax) in the
    /// `--others` listing. Repeatable; supplements `--exclude-standard`.
    #[clap(long = "exclude", short = 'x', value_name = "PATTERN")]
    pub exclude: Vec<String>,

    /// Read additional exclude patterns from `<file>` (one per line, `#`
    /// comments and blank lines ignored) and apply them like `-x`. Repeatable.
    #[clap(long = "exclude-from", short = 'X', value_name = "FILE")]
    pub exclude_from: Vec<String>,

    /// Exit with an error when any pathspec matches no files
    #[clap(long)]
    pub error_unmatch: bool,

    /// Separate records with NUL instead of newline
    #[clap(short = 'z')]
    pub nul_terminate: bool,

    /// Short output format with mode and hash
    #[clap(short = 's')]
    pub short: bool,

    /// Prefix each path with a status tag (H=cached, R=removed/deleted,
    /// C=modified/changed, ?=other/untracked, M=unmerged, S=skip-worktree)
    #[clap(short = 't')]
    pub tag: bool,

    /// Like `-t`: prefix each path with a status tag. Git's `-v` spells
    /// assume-unchanged files in lowercase, which Libra does not support
    /// (DEFER with issues/479), so `-v` output equals `-t` output.
    #[clap(short = 'v')]
    pub show_valid: bool,

    /// Show only unmerged (conflict) entries — index stages 1/2/3 — in
    /// stage-style output
    #[clap(short = 'u', long)]
    pub unmerged: bool,

    /// Force repository-root-relative paths. Accepted for Git compatibility;
    /// Libra always prints repo-root-relative paths, so this is a no-op
    #[clap(long)]
    pub full_name: bool,

    /// Abbreviate object names to N hex digits in `-s`/`--stage` output
    /// (bare `--abbrev` uses 7). Libra truncates to a fixed length rather than
    /// computing the shortest unique prefix.
    #[clap(long, value_name = "N", num_args = 0..=1, require_equals = true, default_missing_value = "7")]
    pub abbrev: Option<usize>,

    /// Show line-ending info for each cached file: `i/<eol> w/<eol> attr/<attr>`
    /// before the path, where `<eol>` is `lf`/`crlf`/`mixed`/`none`/`-text` for
    /// the index blob (`i/`) and the worktree file (`w/`). Line-ending
    /// attribute reporting is not implemented, so `attr/` is always empty.
    #[clap(long)]
    pub eol: bool,

    /// Show the cached stat data (ctime/mtime/dev/ino/uid/gid/size/flags) for
    /// each cached entry, matching Git's `ls-files --debug` block format.
    /// Composes with `-s`/`-t`/`--eol` (the block follows each record).
    #[clap(long = "debug")]
    pub debug: bool,

    /// Limit output to files matching the given pathspec(s)
    #[clap(value_name = "pathspec")]
    pub pathspec: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FileEntry {
    path: String,
    hash: Option<String>,
    mode: Option<String>,
    stage: Option<u32>,
    status: String,
    /// Index v3 extended flag; omitted from JSON so the documented output
    /// shape stays stable (visible through `-t`/`-v` instead).
    #[serde(skip)]
    pub skip_worktree: bool,
    /// Cached stat data for `--debug`; `None` for worktree-only (`-o`) rows.
    /// Omitted from JSON for the same reason as `skip_worktree`.
    #[serde(skip)]
    pub stat: Option<CachedStat>,
}

/// The index's cached stat fields, as `ls-files --debug` prints them.
#[derive(Debug, Clone, Default, serde::Serialize)]
pub struct CachedStat {
    /// `<seconds>:<nanoseconds>`, exactly as `Time` renders.
    pub ctime: String,
    pub mtime: String,
    pub dev: u32,
    pub ino: u32,
    pub uid: u32,
    pub gid: u32,
    pub size: u32,
    pub flags: u32,
}

impl CachedStat {
    fn from_entry(entry: &git_internal::internal::index::IndexEntry) -> Self {
        // Git's `ls-files --debug` prints the cached stat block and the 16-bit
        // flags word: 12 name-length bits, 2 stage bits, then the assume-valid
        // and extended markers. `skip-worktree`/`intent-to-add` live in the v3
        // EXTENDED word (which Git does not print either); an entry carrying
        // them sets the extended marker here.
        let extended = entry.flags.skip_worktree || entry.flags.intent_to_add;
        let flags = u32::from(entry.flags.name_length & 0x0fff)
            | (u32::from(entry.flags.stage) & 0x3) << 12
            | u32::from(extended) << 14
            | u32::from(entry.flags.assume_valid) << 15;
        Self {
            // `Time` renders as `<seconds>:<nanos>` (Git's `--debug` shape).
            ctime: entry.ctime.to_string(),
            mtime: entry.mtime.to_string(),
            dev: entry.dev,
            ino: entry.ino,
            uid: entry.uid,
            gid: entry.gid,
            size: entry.size,
            flags,
        }
    }

    /// Git's `ls-files --debug` block (the path line is written by the caller).
    fn render(&self) -> String {
        format!(
            "  ctime: {c}\n  mtime: {m}\n  dev: {dev}\tino: {ino}\n  uid: {uid}\tgid: {gid}\n  size: {size}\tflags: {flags}",
            c = self.ctime,
            m = self.mtime,
            dev = self.dev,
            ino = self.ino,
            uid = self.uid,
            gid = self.gid,
            size = self.size,
            flags = self.flags,
        )
    }
}

pub async fn execute(args: LsFilesArgs) -> CliResult<()> {
    let output = OutputConfig::default();
    // W1 §C.4.1.1: the sparse view is per-worktree — filter with THIS
    // worktree's view.
    let scope = crate::internal::worktree_scope::WorktreeScope::for_request();
    let view = crate::internal::sparse::SparseView::load(&scope).await;
    let result = run_ls_files(&args, &view, output.is_json())?;
    render_output(&result, &args, &output)?;
    Ok(())
}

pub async fn execute_safe(args: LsFilesArgs, output: &OutputConfig) -> CliResult<()> {
    let scope = crate::internal::worktree_scope::WorktreeScope::for_request();
    let view = crate::internal::sparse::SparseView::load(&scope).await;
    let result = run_ls_files(&args, &view, output.is_json())?;
    render_output(&result, &args, output)?;
    Ok(())
}

fn run_ls_files(
    _args: &LsFilesArgs,
    view: &crate::internal::sparse::SparseView,
    json: bool,
) -> CliResult<Vec<FileEntry>> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let workdir = util::working_dir();
    let current_dir = util::cur_dir();
    let pathspecs = PathspecSet::from_workdir(&_args.pathspec, &current_dir, &workdir)
        .map_err(pathspec_error_to_cli)?;

    let index = Index::load(path::index()).map_err(|source| {
        CliError::fatal(format!("failed to load index: {source}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;

    // Explicit `-x <pattern>` / `-X <file>` exclude sources (gitignore syntax),
    // compiled once into an in-memory matcher. They supplement `--exclude-standard`
    // for the `--others` listing and count toward the `-i` ignored set.
    let exclude_patterns = collect_ls_files_exclude_patterns(_args)?;
    let custom_excludes =
        util::build_exclude_matcher(&workdir, &exclude_patterns).map_err(|e| {
            CliError::command_usage(e).with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
    // Whether a path is excluded, with Git source precedence: the explicit
    // `-x`/`-X` matcher (higher precedence) decides first — an explicit negation
    // (`Some(false)`) re-includes the path even if a standard ignore source
    // would exclude it; only when no custom pattern matches (`None`) do the standard
    // `--exclude-standard` rules apply.
    let is_excluded = |abs: &Path| {
        let custom = custom_excludes
            .as_ref()
            .and_then(|m| util::exclude_matcher_verdict(m, &workdir, abs, abs.is_dir()));
        match custom {
            Some(verdict) => verdict,
            None => _args.exclude_standard && ignore::path_matches_ignore_pattern(abs, &workdir),
        }
    };

    // `-i`/`--ignored` flips the listing to the ignored set. Like Git, it must be
    // paired with `-o`/`-c` and needs an exclude source (`--exclude-standard` or
    // an explicit `-x`/`-X` pattern).
    if _args.ignored {
        if !_args.others && !_args.cached {
            return Err(CliError::fatal(
                "ls-files -i must be used with either -o or -c".to_string(),
            )
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
        if !_args.exclude_standard && exclude_patterns.is_empty() {
            return Err(CliError::fatal(
                "ls-files --ignored needs some exclude pattern".to_string(),
            )
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidArguments));
        }
    }

    let mut entries = Vec::new();
    let include_cached = _args.cached || (!_args.deleted && !_args.modified && !_args.others);

    // In `-i`/`--ignored` mode the cached (tracked-matching-exclude) listing is
    // requested ONLY by an explicit `-c`/`--cached`; `-i -o` (even with display
    // flags like `-s`/`-t`/`-u`) must stay others-only, matching Git.
    let run_cached_block = if _args.ignored {
        _args.cached
    } else {
        include_cached
            || _args.deleted
            || _args.modified
            || _args.stage
            || _args.short
            || _args.unmerged
            || _args.tag
            || _args.show_valid
    };

    if run_cached_block {
        // `-u`/`--unmerged` restricts the listing to conflict stages 1/2/3.
        // Every other cached listing (plain, `-c`, `-z`, `--json`, `-v`) walks
        // stages 0/1/2/3, so an unmerged path prints one line per stage, like
        // Git's `show_files`.
        let stages: &[u8] = if _args.unmerged {
            &[1, 2, 3]
        } else {
            &[0, 1, 2, 3]
        };
        // Worktree state is only needed to *filter* (`-d`/`-m`), to *label* (`-t`),
        // or to fill the `status` field the JSON shape always carries. Plain
        // `ls-files` is a pure index dump — Git never touches the worktree for it.
        // Computing it unconditionally made every entry stat, and every file get
        // read and hashed, which turns this command into minutes on a large or
        // remotely-backed worktree.
        let needs_worktree_state =
            _args.deleted || _args.modified || _args.tag || _args.show_valid || json;
        for stage in stages {
            for entry in index.tracked_entries(*stage) {
                let worktree_path = workdir.join(&entry.name);
                // `-i`: among cached entries, list only those matching an exclude
                // pattern — standard sources under `--exclude-standard` or an
                // explicit `-x`/`-X` pattern (a tracked file that would be ignored).
                if _args.ignored && !is_excluded(&worktree_path) {
                    continue;
                }
                let skip_worktree = entry.flags.skip_worktree;
                // Git never consults the working tree for a skip-worktree entry:
                // `builtin/ls-files.c` skips the -d/-m lstat via
                // `if (ce_skip_worktree(ce)) continue;`, so the entry is never
                // reported deleted/modified and always tags `S`.
                let (is_deleted, is_modified) = if skip_worktree {
                    (false, false)
                } else if needs_worktree_state {
                    let exists = fs::symlink_metadata(&worktree_path).is_ok();
                    let modified = exists
                        && entry_modified(
                            &worktree_path,
                            &entry.name,
                            &entry.hash.to_string(),
                            entry.mode,
                        )?;
                    (!exists, modified)
                } else {
                    (false, false)
                };

                if _args.deleted && !is_deleted {
                    continue;
                }
                if _args.modified && !is_modified {
                    continue;
                }

                // Conflict-stage entries are "unmerged" regardless of worktree state.
                let status = if *stage > 0 {
                    "unmerged"
                } else if is_deleted {
                    "deleted"
                } else if is_modified {
                    "modified"
                } else {
                    "cached"
                };
                entries.push(FileEntry {
                    path: entry.name.clone(),
                    hash: Some(entry.hash.to_string()),
                    mode: Some(format!("{:06o}", entry.mode)),
                    stage: Some(*stage as u32),
                    status: status.to_string(),
                    skip_worktree,
                    stat: Some(CachedStat::from_entry(entry)),
                });
            }
        }
    }

    if _args.others {
        // Git's `index_name_is_other`: a path that exists in the index — even as
        // an unmerged conflict stage — is never "other". Libra's index exposes
        // entries per stage, so union stages 0..=3.
        let tracked: HashSet<String> = (0u8..=3)
            .flat_map(|stage| index.tracked_entries(stage))
            .map(|entry| entry.name.clone())
            .collect();
        // When only the standard exclude is in play (no explicit `-x`/`-X` and not
        // `-i`), the standard-filtered listing is enough. Otherwise list every
        // untracked file and decide per-file below so custom patterns and the
        // ignored set are honored uniformly.
        let needs_per_file = _args.ignored || custom_excludes.is_some();
        let all_files = if !needs_per_file && _args.exclude_standard {
            util::list_workdir_files()
        } else {
            util::list_workdir_files_unfiltered()
        }
        .map_err(|source| {
            CliError::fatal(format!("failed to list working tree files: {source}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?;

        for file in all_files {
            let display = file.to_string_lossy().replace('\\', "/");
            if tracked.contains(&display) {
                continue;
            }
            let abs = workdir.join(&file);
            // An untracked file is "excluded" per Git source precedence: explicit
            // `-x`/`-X` first, then the standard sources under `--exclude-standard`.
            let excluded = is_excluded(&abs);
            if _args.ignored {
                // `-i -o`: list ONLY the excluded (ignored) untracked files.
                if !excluded {
                    continue;
                }
            } else if excluded {
                // Normal `-o`: drop excluded untracked files.
                continue;
            }
            entries.push(FileEntry {
                path: display,
                hash: None,
                mode: None,
                stage: None,
                status: "other".to_string(),
                skip_worktree: false,
                stat: None,
            });
        }
    }

    // lore.md 2.2: read-only sparse view — scope the listing to in-view paths.
    // An UNMERGED entry (stage > 0) is ALWAYS shown so the view never hides an
    // unresolved conflict. Applied before pathspec/error-unmatch so an
    // out-of-view pathspec is treated as unmatched.
    if view.is_active() {
        entries.retain(|entry| entry.stage.unwrap_or(0) > 0 || view.contains_str(&entry.path));
    }

    // Git scopes a pathspec-less listing to the current directory (I668-01);
    // explicit pathspecs keep their own cwd-relative resolution and may select
    // entries outside the implicit cwd tree.
    if pathspecs.is_empty() {
        let relative = util::cur_dir()
            .strip_prefix(&workdir)
            .map(Path::to_path_buf)
            .unwrap_or_default();
        if !relative.as_os_str().is_empty() {
            entries.retain(|entry| Path::new(&entry.path).starts_with(&relative));
        }
    }

    entries = filter_entries_by_pathspec(entries, &pathspecs);
    if _args.error_unmatch {
        ensure_error_unmatch(&pathspecs, &entries)?;
    }

    entries.sort_by(|a, b| a.path.cmp(&b.path).then(a.stage.cmp(&b.stage)));
    Ok(entries)
}

/// Collect the explicit exclude patterns for `-x`/`-X` in Git's precedence order.
/// Later patterns win (gitignore last-match-wins), and Git ranks command-line
/// `--exclude` (`-x`) above `--exclude-from` (`-X`) files, so the `-X` file lines
/// (in file then line order) are collected FIRST and the inline `-x` patterns
/// LAST. Blank and `#`-comment lines in `-X` files are skipped (gitignore file
/// semantics).
fn collect_ls_files_exclude_patterns(args: &LsFilesArgs) -> CliResult<Vec<String>> {
    let mut patterns: Vec<String> = Vec::new();
    for file in &args.exclude_from {
        let contents = fs::read_to_string(file).map_err(|source| {
            CliError::fatal(format!("failed to read exclude file '{file}': {source}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
                .with_hint("check the --exclude-from path")
        })?;
        for line in contents.lines() {
            let trimmed = line.trim_end_matches('\r');
            if trimmed.is_empty() || trimmed.trim_start().starts_with('#') {
                continue;
            }
            patterns.push(trimmed.to_string());
        }
    }
    // Command-line `-x` patterns rank above `-X` files, so they go last (wins).
    patterns.extend(args.exclude.iter().cloned());
    Ok(patterns)
}

fn filter_entries_by_pathspec(entries: Vec<FileEntry>, pathspecs: &PathspecSet) -> Vec<FileEntry> {
    if pathspecs.is_empty() {
        return entries;
    }

    entries
        .into_iter()
        .filter(|entry| pathspecs.matches_path(&entry.path))
        .collect()
}

fn ensure_error_unmatch(pathspecs: &PathspecSet, entries: &[FileEntry]) -> CliResult<()> {
    if let Some(unmatched) =
        pathspecs.unmatched_positive(entries.iter().map(|entry| Path::new(&entry.path)))
    {
        return Err(
            CliError::fatal(format!("pathspec '{unmatched}' did not match any files"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_exit_code(1)
                .with_hint("check the path and try again.")
                .with_hint("use 'libra ls-files' to inspect visible paths."),
        );
    }

    Ok(())
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

fn entry_modified(
    worktree_path: &Path,
    display_path: &str,
    indexed_hash: &str,
    indexed_mode: u32,
) -> CliResult<bool> {
    if indexed_mode == 0o160000 {
        let metadata = fs::symlink_metadata(worktree_path).map_err(|source| {
            CliError::fatal(format!(
                "failed to inspect gitlink working tree path '{display_path}': {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;
        return Ok(!metadata.is_dir() || metadata.file_type().is_symlink());
    }

    let data = match read_worktree_blob_bytes(worktree_path) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(source) => {
            return Err(CliError::fatal(format!(
                "failed to read working tree file '{display_path}': {source}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed));
        }
    };
    let blob = Blob::from_content_bytes(data);
    Ok(blob.id.to_string() != indexed_hash)
}

/// Map an entry's `status` to its `git ls-files -t` tag letter.
fn status_tag(status: &str) -> char {
    match status {
        "deleted" => 'R',
        "modified" => 'C',
        "other" => '?',
        "unmerged" => 'M',
        // "cached" and anything else default to H (in the index).
        _ => 'H',
    }
}

/// Project a repo-root path for text output: cwd-relative by default,
/// repo-root-relative with `--full-name`, and a `../`-prefixed path for an
/// explicitly selected entry outside the cwd (never silently falling back to the
/// root form).
fn display_path(path: &str, cwd_relative: &Path, full_name: bool) -> String {
    if full_name || cwd_relative.as_os_str().is_empty() {
        return path.to_string();
    }
    if let Ok(rest) = Path::new(path).strip_prefix(cwd_relative) {
        return rest.to_string_lossy().into_owned();
    }
    let depth = cwd_relative.components().count();
    format!("{}{}", "../".repeat(depth), path)
}

fn render_output(
    entries: &[FileEntry],
    args: &LsFilesArgs,
    output: &OutputConfig,
) -> CliResult<()> {
    if args.nul_terminate && output.is_json() {
        return Err(
            CliError::fatal("ls-files -z cannot be combined with --json or --machine")
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_hint("choose either NUL-delimited text output or JSON/machine output"),
        );
    }
    if output.is_json() {
        return emit_json_data("ls-files", &entries.to_vec(), output);
    }
    if output.quiet {
        return Ok(());
    }

    // `--eol` inserts a line-ending-info column (`i/<eol> w/<eol> attr/<attr>\t`)
    // immediately before the path. It composes with `-t`/`-s`/`--stage` exactly
    // like Git: the column sits after any tag/stage prefix and before the path.
    let workdir = util::working_dir();
    let cwd_relative = util::cur_dir()
        .strip_prefix(&workdir)
        .map(Path::to_path_buf)
        .unwrap_or_default();

    let mut stdout = std::io::stdout().lock();
    for entry in entries {
        let display = display_path(&entry.path, &cwd_relative, args.full_name);
        let eol_col = if args.eol {
            eol_column(entry, &workdir)
        } else {
            String::new()
        };
        let mut record = if args.short || args.stage || args.unmerged {
            let hash = entry
                .hash
                .as_deref()
                .unwrap_or("0000000000000000000000000000000000000000");
            // `--abbrev[=N]` truncates the object name to N hex digits.
            let hash = match args.abbrev {
                Some(n) => hash.get(..n.min(hash.len())).unwrap_or(hash),
                None => hash,
            };
            format!(
                "{} {} {}\t{}{}",
                entry.mode.as_deref().unwrap_or("000000"),
                hash,
                entry.stage.unwrap_or(0),
                eol_col,
                display
            )
        } else {
            format!("{}{}", eol_col, display)
        };
        // `-t`/`-v` prefix a status tag (matching `git ls-files -t`).
        // Unmerged wins over skip-worktree, which wins over the worktree-state
        // tags (git's `ce_stage ? M : ce_skip_worktree ? S : H/C/R/...`).
        if args.tag || args.show_valid {
            let tag = if entry.stage.unwrap_or(0) > 0 {
                'M'
            } else if entry.skip_worktree {
                'S'
            } else {
                status_tag(&entry.status)
            };
            record = format!("{} {}", tag, record);
        }

        stdout
            .write_all(record.as_bytes())
            .map_err(|source| stdout_write_error("write ls-files output", source))?;
        // `--debug` appends Git's cached-stat block after the record.
        if args.debug
            && let Some(stat) = entry.stat.as_ref()
        {
            stdout
                .write_all(b"\n")
                .and_then(|()| stdout.write_all(stat.render().as_bytes()))
                .map_err(|source| stdout_write_error("write ls-files --debug output", source))?;
        }
        stdout
            .write_all(if args.nul_terminate { b"\0" } else { b"\n" })
            .map_err(|source| stdout_write_error("write ls-files output", source))?;
    }

    Ok(())
}

/// Classify the end-of-line style of a byte buffer, matching Git's `i/`/`w/`
/// eol labels (Git's `convert.c` text stats). A buffer is BINARY (`-text`) when
/// it has a NUL, a lone CR (CR not part of CRLF), or too many non-printable
/// bytes (`printable >> 7 < nonprintable`, Git's heuristic). Otherwise it is
/// `mixed` (both CRLF and lone LF), `crlf`, `lf`, or `none`.
fn classify_eol(data: &[u8]) -> &'static str {
    let mut nul = 0usize;
    let mut cr = 0usize;
    let mut crlf = 0usize;
    let mut lf = 0usize;
    let mut printable = 0usize;
    let mut nonprintable = 0usize;
    for (i, &c) in data.iter().enumerate() {
        match c {
            b'\r' => {
                cr += 1;
                if data.get(i + 1) == Some(&b'\n') {
                    crlf += 1;
                }
            }
            b'\n' => lf += 1,
            127 => nonprintable += 1, // DEL
            0 => {
                nul += 1;
                nonprintable += 1;
            }
            // BS, TAB, ESC, FF are treated as printable text controls.
            8 | 9 | 27 | 12 => printable += 1,
            c if c < 32 => nonprintable += 1,
            _ => printable += 1,
        }
    }
    let lonecr = cr - crlf;
    let lonelf = lf - crlf;
    if nul > 0 || lonecr > 0 || (printable >> 7) < nonprintable {
        return "-text";
    }
    match (crlf > 0, lonelf > 0) {
        (true, true) => "mixed",
        (true, false) => "crlf",
        (false, true) => "lf",
        (false, false) => "none",
    }
}

/// Build the `git ls-files --eol` column (`i/<eol> w/<eol> attr/<attr>\t`) for
/// one entry: the index-blob eol (loaded by object id) and the worktree-file eol
/// (read from disk). A missing blob/file leaves that field empty (Git's
/// `lstat`-failed case). `attr/` is always empty because line-ending attribute
/// reporting is not implemented.
/// The format (`i/%-5s w/%-5s attr/%-17s\t`) is byte-compatible with Git, and
/// the caller inserts it immediately before the path (composing with `-t`/`-s`).
fn eol_column(entry: &FileEntry, workdir: &Path) -> String {
    let i_eol = match entry
        .hash
        .as_deref()
        .and_then(|h| crate::internal::object_format::parse_repo_oid(h).ok())
    {
        Some(hash) => match load_object::<Blob>(&hash) {
            Ok(blob) => classify_eol(&blob.data),
            Err(_) => "",
        },
        None => "",
    };
    let w_eol = match fs::read(workdir.join(&entry.path)) {
        Ok(data) => classify_eol(&data),
        Err(_) => "",
    };
    format!("i/{i_eol:<5} w/{w_eol:<5} attr/{:<17}\t", "")
}
