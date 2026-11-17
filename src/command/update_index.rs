//! `libra update-index` — modify the index directly, a subset of
//! `git update-index`. Companion to `write-tree`: `--cacheinfo` registers an
//! entry from a `(mode, object, path)` triple without reading the working tree,
//! so an index can be built purely from objects.
//!
//! Plan issues/479 (UI-01/02/03) adds the batch-input surface (`--index-info`,
//! `--stdin`, `-z`), the stat-refresh family (`--refresh`,
//! `--really-refresh`, `--ignore-missing`, `--unmerged`), and the entry
//! modifiers (`--chmod=(+|-)x`, `--info-only`, `--verbose`). Like git,
//! option handling is order-sensitive (`-q`/`--ignore-missing`/`--unmerged`
//! must precede `--refresh`; `--chmod` applies to the positional paths that
//! follow it), so [`execute_safe`] reconstructs the invocation order from the
//! raw argv and applies every operation against one in-memory index, saving
//! exactly once (GC-PL-02: a failing batch leaves the on-disk index
//! byte-identical).

use std::{
    fs,
    io::{self, BufRead, Write},
    path::{Path, PathBuf},
};

use clap::Parser;
use git_internal::internal::{
    index::{Index, IndexEntry},
    object::blob::Blob,
};
use serde::Serialize;

use crate::{
    command::{refresh, symlink_target_blob_bytes},
    utils::{
        error::{CliError, CliResult, StableErrorCode},
        lfs,
        object_ext::BlobExt,
        output::{OutputConfig, emit_json_data},
        path,
        text::decode_c_quoted,
        util,
    },
};

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const UPDATE_INDEX_EXAMPLES: &str = "\
EXAMPLES:
    libra update-index --add a.txt b.txt        Stage files from the working tree
    libra update-index --remove old.txt         Drop a path from the index
    libra update-index --force-remove old.txt   Drop a path even when its file is gone
    libra update-index --skip-worktree s.txt    Mark s.txt index-only (status/diff ignore its worktree copy)
    libra update-index --no-skip-worktree s.txt Clear the skip-worktree mark again
    libra update-index --cacheinfo 100644,<oid>,dir/f.txt
                                                 Register an entry directly from an object id
    libra update-index --refresh                Re-stat the index; exit 1 when a file needs update
    libra update-index --chmod=+x run.sh        Flip the executable bit in the index
    printf '100644,<oid>,f.txt' | libra update-index --index-info
                                                 Read entries from stdin, apply all-or-nothing
    libra --json update-index --add a.txt       Structured JSON output for agents";

/// Modify the index directly: stage working-tree files (`--add`), drop paths
/// (`--remove`), or register entries from object ids (`--cacheinfo`).
#[derive(Parser, Debug)]
#[command(after_help = UPDATE_INDEX_EXAMPLES)]
pub struct UpdateIndexArgs {
    /// Use this index file instead of `.libra/index` (a Libra flag standing
    /// in for Git's GIT_INDEX_FILE env). A missing file starts as an empty
    /// index — scratch revision composition never touches real staging state.
    #[clap(long = "index-file", value_name = "PATH")]
    pub index_file: Option<String>,

    /// Allow the positional paths to add files that are not yet in the index
    /// (read from the working tree). Without it, positional paths must already
    /// be tracked.
    #[clap(long)]
    pub add: bool,

    /// Remove the positional paths from the index (rather than (re)staging them).
    #[clap(long)]
    pub remove: bool,

    /// Drop the positional paths from the index regardless of whether the
    /// working-tree file exists; paths the index does not know are a no-op.
    /// Removes every stage of an unmerged path. Wins over --add/--remove.
    #[clap(long = "force-remove")]
    pub force_remove: bool,

    /// Mark tracked paths with the skip-worktree bit (index v3 extended
    /// flags): status/diff then ignore the working-tree copy and no command
    /// restages it. The index entry itself is untouched and the working tree
    /// is never read; paths that are not in the index fail with `Unable to
    /// mark file`. Setting the bit rewrites the index as format v3.
    #[clap(long)]
    pub skip_worktree: bool,

    /// Clear the skip-worktree bit from tracked paths. The index returns to
    /// format v2 once no entry carries extended flags anymore.
    #[clap(long = "no-skip-worktree")]
    pub no_skip_worktree: bool,

    /// Register an index entry directly from `<mode>,<object>,<path>` without
    /// reading the working tree (the object need not exist yet). Repeatable.
    /// `<mode>` is an octal file mode (`100644`, `100755`, `120000`, `160000`).
    #[clap(long, value_name = "<mode>,<object>,<path>")]
    pub cacheinfo: Vec<String>,

    /// Re-stat the index against the working tree (git's `--refresh`):
    /// entries whose stat changed but whose content still matches are
    /// re-statted silently; modified files print `<path>: needs update` and
    /// the command exits 1. `-q`/`--ignore-missing`/`--unmerged` must precede
    /// `--refresh`, exactly as in git (options are processed in order).
    #[clap(long)]
    pub refresh: bool,

    /// Like `--refresh`, but re-checks entries marked assume-unchanged.
    /// Libra has no assume-unchanged bit (DEFER-02), so the two flags are
    /// equivalent.
    #[clap(long = "really-refresh")]
    pub really_refresh: bool,

    /// With `--refresh`: skip index entries whose working-tree file is
    /// missing (must precede `--refresh`).
    #[clap(long)]
    pub ignore_missing: bool,

    /// With `--refresh`: skip unmerged entries instead of reporting
    /// `<path>: needs merge` (must precede `--refresh`).
    #[clap(long)]
    pub unmerged: bool,

    /// Flip the executable bit on the index entries of the positional paths
    /// (and `--stdin` paths) that follow it: `--chmod=+x` / `--chmod=-x`.
    /// Repeatable; each occurrence applies to the paths after it until
    /// overridden (git's t2107 matrix). The working-tree file is untouched.
    #[clap(long = "chmod", value_name = "(+|-)x", require_equals = true, value_parser = ["+x", "-x"])]
    pub chmod: Vec<String>,

    /// Record object ids in the index without writing the objects. With
    /// `--add`, an untracked path gets its index entry (correct oid) but the
    /// blob is never stored (git's `--info-only`).
    #[clap(long = "info-only")]
    pub info_only: bool,

    /// Report each action on stdout: `add '<path>'`, `remove '<path>'`
    /// (force-remove), and `chmod +x '<path>'` / `chmod -x '<path>'`.
    #[clap(long)]
    pub verbose: bool,

    /// Read index entries from stdin, one per line (NUL-terminated with `-z`),
    /// in the three `--index-info` formats:
    /// `<mode> SP <oid> TAB <path>`,
    /// `<mode> SP <type> SP <oid> TAB <path>`, or
    /// `<mode> SP <oid> SP <stage> TAB <path>`.
    /// `mode 0` removes every stage of the path. All lines are parsed first
    /// and applied with one single index save; a malformed line exits 128
    /// with the on-disk index byte-identical. Must be the last argument.
    #[clap(long = "index-info")]
    pub index_info: bool,

    /// Read paths from stdin, one per line (NUL-terminated with `-z`), and
    /// process each like a positional path (current `--add`/`--remove`/
    /// `--chmod` state applies). Must be the last argument.
    #[clap(long = "stdin")]
    pub stdin_list: bool,

    /// With `--stdin`/`--index-info`, records are NUL-terminated instead of
    /// newline-terminated.
    #[clap(short = 'z', long = "null")]
    pub nul_terminated: bool,

    /// Paths to (re)stage from the working tree, or to remove with `--remove`.
    #[clap(value_name = "PATH")]
    pub paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct UpdateIndexOutput {
    /// Number of index entries added/updated.
    updated: usize,
    /// Number of index entries removed.
    removed: usize,
}

pub async fn execute(args: UpdateIndexArgs) {
    if let Err(err) = execute_safe(args, &OutputConfig::default()).await {
        err.print_stderr();
        std::process::exit(err.exit_code());
    }
}

/// Safe entry point. Reconstructs the invocation order from the raw argv
/// (option handling is order-sensitive, as in git), applies every operation
/// against one in-memory index, then saves once. Usage/repository errors exit
/// 128/129; a `--refresh` finding leaves the index saved and exits 1.
pub async fn execute_safe(args: UpdateIndexArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;

    let usage = |message: String| {
        CliError::command_usage(message)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(128)
    };

    let index_path = args
        .index_file
        .clone()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(path::index);
    // GIT_INDEX_FILE parity: an explicit --index-file that doesn't exist yet
    // starts as an EMPTY index (scratch composition).
    let mut index = if args.index_file.is_some() && !index_path.exists() {
        Index::new()
    } else {
        Index::load(&index_path).map_err(|error| {
            CliError::fatal(format!("failed to load index: {error}"))
                .with_stable_code(StableErrorCode::RepoStateInvalid)
        })?
    };

    // ADR-FM-04: `core.fileMode=false` keeps an existing entry's mode and makes
    // new paths plain 100644. (--cacheinfo carries an explicit mode and is
    // unaffected.)
    let file_mode = crate::internal::config::core_file_mode().await?;

    let workdir = util::working_dir();
    let mut updated = 0usize;
    let mut removed = 0usize;
    let mut refresh_errors = false;

    // Option handling is order-sensitive (git's parse_options_step interleaves
    // flags and paths), so the ordered operation list is the single source of
    // truth; the clap fields only validated the surface.
    let ordered = ordered_operations(&usage)?;
    let suppress_refresh_output = ordered.suppress_refresh_output(output);
    for op in ordered.ops {
        match op {
            Op::Cacheinfo(spec) => {
                let entry = parse_cacheinfo(&spec).map_err(usage)?;
                crate::utils::index_ext::update_preserving_flags(&mut index, entry);
                updated += 1;
            }
            Op::Path { path, ctx } => {
                let (added, dropped) =
                    process_one_path(&mut index, &path, &ctx, &workdir, file_mode, output)?;
                updated += added;
                removed += dropped;
            }
            Op::Refresh { switches } => {
                // `--really-refresh` ≡ `--refresh`: it only re-checks git's
                // assume-unchanged (CE_VALID) entries, which Libra does not
                // support (DEFER-02). Both drop a leftover CE_VALID bit from
                // every stage-0 entry they visit (issues/637).
                let options = refresh::RefreshOptions {
                    quiet: switches.quiet,
                    ignore_missing: switches.ignore_missing,
                    allow_unmerged: switches.allow_unmerged,
                    file_mode,
                };
                let candidates = all_refresh_candidates(&index);
                let report =
                    refresh::refresh_candidates(&mut index, &candidates, &workdir, &options);
                render_refresh_report(&report, output, suppress_refresh_output);
                refresh_errors |= report.has_errors;
            }
            Op::IndexInfo { nul } => {
                let (added, dropped) =
                    apply_index_info_stream(&mut index, nul, &ctx_final(&args), output)?;
                updated += added;
                removed += dropped;
            }
            Op::Stdin { nul } => {
                let (added, dropped) = apply_stdin_stream(
                    &mut index,
                    nul,
                    &workdir,
                    file_mode,
                    &ctx_final(&args),
                    output,
                )?;
                updated += added;
                removed += dropped;
            }
        }
    }

    index.save(&index_path).map_err(|error| {
        CliError::fatal(format!("failed to save index: {error}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;

    if output.is_json() && !refresh_errors {
        emit_json_data(
            "update-index",
            &UpdateIndexOutput { updated, removed },
            output,
        )?;
    }

    // Git's refresh contract: `<path>: needs update` / `needs merge` lines
    // were already printed; the index (with refreshed stat fields) IS saved
    // and the process exits 1.
    if refresh_errors {
        return Err(CliError::silent_exit(1));
    }
    Ok(())
}

/// The flags that can be captured per positional path in the ordered walk.
#[derive(Debug, Clone, Copy, Default)]
struct PathContext {
    add: bool,
    remove: bool,
    force_remove: bool,
    skip_worktree: bool,
    no_skip_worktree: bool,
    info_only: bool,
    verbose: bool,
    /// Target mode after the most recent `--chmod=(+|-)x` in argv order.
    chmod: Option<u32>,
}

/// The switches in effect when a `--refresh`/`--really-refresh` is reached.
/// Like git, `-q`/`--ignore-missing`/`--unmerged` only affect `--refresh`
/// occurrences that come AFTER them on the command line.
#[derive(Debug, Clone, Copy, Default)]
struct RefreshSwitches {
    quiet: bool,
    ignore_missing: bool,
    allow_unmerged: bool,
}

#[derive(Debug)]
enum Op {
    Cacheinfo(String),
    Path { path: String, ctx: PathContext },
    Refresh { switches: RefreshSwitches },
    IndexInfo { nul: bool },
    Stdin { nul: bool },
}

#[derive(Debug, Default)]
struct OrderedOperations {
    ops: Vec<Op>,
    global_quiet: bool,
    command_quiet: bool,
}

impl OrderedOperations {
    fn suppress_refresh_output(&self, output: &OutputConfig) -> bool {
        // Clap's inherited quiet flag combines both spellings and positions.
        // Only command-position -q belongs to the ordered refresh semantics.
        // Preserve quiet supplied by direct callers when argv has no such flag.
        self.global_quiet || (output.quiet && !self.command_quiet)
    }
}

/// Reconstruct the ordered update-index operations from the raw argv.
///
/// Clap parses every flag up front, but git's `update-index` is
/// order-sensitive: `-q`/`--ignore-missing`/`--unmerged` must precede
/// `--refresh`, `--chmod` applies to the positional paths after it, and
/// `--stdin`/`--index-info` must be the last argument. The walker therefore
/// reads `std::env::args()` (the flags were already validated by clap) and
/// captures the per-position state. The same walk tracks explicit global
/// quiet, skipping option values and stopping option interpretation at `--`.
fn ordered_operations(usage: &dyn Fn(String) -> CliError) -> CliResult<OrderedOperations> {
    ordered_operations_from_tokens(&std::env::args().skip(1).collect::<Vec<_>>(), usage)
}

fn ordered_operations_from_tokens(
    tokens: &[String],
    usage: &dyn Fn(String) -> CliError,
) -> CliResult<OrderedOperations> {
    let mut ordered = OrderedOperations::default();
    let mut in_command = false;
    let mut ops = Vec::new();
    let mut ctx = PathContext::default();
    let mut switches = RefreshSwitches::default();
    let mut nul = false;
    let mut index = 0usize;

    while index < tokens.len() {
        let token = &tokens[index];
        match token.as_str() {
            "update-index" if !in_command => in_command = true,
            "--quiet" | "--machine" => ordered.global_quiet = true,
            "-q" if in_command => {
                switches.quiet = true;
                ordered.command_quiet = true;
            }
            "-q" => ordered.global_quiet = true,
            // Root-level global flags (already parsed by the root clap).
            "--json"
            | "--color"
            | "--no-pager"
            | "--no-color"
            | "--exit-code-on-warning"
            | "--sync-data"
            | "--offline"
            | "-J"
            | "--literal-pathspecs"
            | "--no-literal-pathspecs" => {}
            // Clap accepts short flag clusters too; -J's optional value must
            // use '=', so it never consumes the next token.
            _ if token.starts_with('-')
                && !token.starts_with("--")
                && token.len() > 2
                && token[1..].split('=').next().is_some_and(|flags| {
                    !flags.is_empty()
                        && (!token.contains('=') || flags.ends_with('J'))
                        && flags
                            .chars()
                            .all(|flag| matches!(flag, 'q' | 'J') || (in_command && flag == 'z'))
                }) =>
            {
                for flag in token[1..].chars().take_while(|flag| *flag != '=') {
                    match flag {
                        'q' if in_command => {
                            switches.quiet = true;
                            ordered.command_quiet = true;
                        }
                        'q' => ordered.global_quiet = true,
                        'z' => nul = true,
                        _ => {}
                    }
                }
            }
            // Global flags with an inline or separate value.
            _ if token.starts_with("-J=")
                || token.starts_with("--json=")
                || token.starts_with("--color=")
                || token.starts_with("--progress=")
                || token.starts_with("--max-connections=") => {}
            "--progress" | "--max-connections" => index += 1,
            _ if !in_command => {}
            "--" => {
                for path in &tokens[index + 1..] {
                    ops.push(Op::Path {
                        path: path.clone(),
                        ctx,
                    });
                }
                break;
            }
            "--index-file" => index += 1,
            _ if token.starts_with("--index-file=") => {}
            "--add" => ctx.add = true,
            "--remove" => ctx.remove = true,
            "--force-remove" => ctx.force_remove = true,
            "--skip-worktree" => ctx.skip_worktree = true,
            "--no-skip-worktree" => ctx.no_skip_worktree = true,
            "--info-only" => ctx.info_only = true,
            "--verbose" => ctx.verbose = true,
            _ if token.starts_with("--chmod=") => {
                let flip = &token["--chmod=".len()..];
                // Clap already validated `+x`/`-x` (value_parser); this is the
                // walker's mirror of git's set_executable_bit state.
                ctx.chmod = match flip {
                    "+x" => Some(0o100755),
                    "-x" => Some(0o100644),
                    other => {
                        return Err(usage(format!(
                            "option 'chmod' expects \"+x\" or \"-x\", got \"{other}\""
                        )));
                    }
                };
            }
            "--cacheinfo" => {
                index += 1;
                let Some(spec) = tokens.get(index) else {
                    return Err(usage(
                        "option 'cacheinfo' expects <mode>,<object>,<path>".into(),
                    ));
                };
                ops.push(Op::Cacheinfo(spec.clone()));
            }
            _ if token.starts_with("--cacheinfo=") => {
                ops.push(Op::Cacheinfo(token["--cacheinfo=".len()..].to_string()));
            }
            "--refresh" => ops.push(Op::Refresh { switches }),
            "--really-refresh" => ops.push(Op::Refresh { switches }),
            "--ignore-missing" => switches.ignore_missing = true,
            "--unmerged" => switches.allow_unmerged = true,
            "-z" | "--null" => nul = true,
            "--index-info" => {
                must_be_last(tokens, index, "--index-info", usage)?;
                ops.push(Op::IndexInfo { nul });
            }
            "--stdin" => {
                must_be_last(tokens, index, "--stdin", usage)?;
                ops.push(Op::Stdin { nul });
            }
            _ => ops.push(Op::Path {
                path: token.clone(),
                ctx,
            }),
        }
        index += 1;
    }
    ordered.ops = ops;
    Ok(ordered)
}

/// git 2.56: `option '<flag>' must be the last argument` → usage error 129.
fn must_be_last(
    tokens: &[String],
    position: usize,
    flag: &str,
    usage: &dyn Fn(String) -> CliError,
) -> CliResult<()> {
    if position + 1 != tokens.len() {
        return Err(
            CliError::command_usage(format!("option '{flag}' must be the last argument"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
                .with_exit_code(129),
        );
    }
    let _ = usage;
    Ok(())
}

/// The final flag state (used by the `--stdin`/`--index-info` stream, which
/// sits at the end of the argv and therefore sees every flag).
fn ctx_final(args: &UpdateIndexArgs) -> PathContext {
    PathContext {
        add: args.add,
        remove: args.remove,
        force_remove: args.force_remove,
        skip_worktree: args.skip_worktree,
        no_skip_worktree: args.no_skip_worktree,
        info_only: args.info_only,
        verbose: args.verbose,
        chmod: args.chmod.last().and_then(|flip| match flip.as_str() {
            "+x" => Some(0o100755),
            "-x" => Some(0o100644),
            _ => None,
        }),
    }
}

/// Process one path the way git's `update_one` + trailing `chmod_path` do,
/// under the flag state captured at that argv position. Returns the number of
/// entries added/updated and removed.
#[allow(clippy::too_many_arguments)]
fn process_one_path(
    index: &mut Index,
    path_str: &str,
    ctx: &PathContext,
    workdir: &Path,
    file_mode: bool,
    output: &OutputConfig,
) -> CliResult<(usize, usize)> {
    let mut updated = 0usize;
    let mut removed = 0usize;
    let usage = |message: String| {
        CliError::command_usage(message)
            .with_stable_code(StableErrorCode::CliInvalidArguments)
            .with_exit_code(128)
    };

    // `--skip-worktree`/`--no-skip-worktree` mark existing stage-0 entries
    // without reading the working tree, and win over `--force-remove`
    // (git's update_one handles the mark modes before the force-remove
    // branch). A path the index does not know is git's
    // `fatal: Unable to mark file <path>` (exit 128); nothing is saved,
    // so earlier marks in the same invocation do not persist either.
    if ctx.skip_worktree || ctx.no_skip_worktree {
        if !crate::utils::index_ext::set_skip_worktree(index, path_str, ctx.skip_worktree) {
            return Err(CliError::fatal(format!("Unable to mark file {path_str}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::CliInvalidTarget));
        }
        updated += 1;
        return Ok((updated, removed));
    }

    // `--force-remove` is unconditional and wins over `--add`/`--remove`
    // (git 2.55: `update-index --force-remove --add <present-file>`
    // removes the entry). Every stage of an unmerged path goes away.
    if ctx.force_remove {
        let mut removed_any = false;
        for stage in [0u8, 1, 2, 3] {
            if index.remove(path_str, stage).is_some() {
                removed_any = true;
            }
        }
        if removed_any {
            removed += 1;
            verbose_line(output, ctx, &format!("remove '{path_str}'"));
        }
        return Ok((updated, removed));
    }

    // A skip-worktree entry short-circuits the add/remove handling (git
    // `process_path`): its working-tree copy is assumed good, so plain
    // update is a no-op — the file is never restaged, present or not —
    // and only an explicit `--remove` drops the entry.
    if index
        .get(path_str, 0)
        .is_some_and(|entry| entry.flags.skip_worktree)
    {
        if ctx.remove && index.remove(path_str, 0).is_some() {
            removed += 1;
            verbose_line(output, ctx, &format!("remove '{path_str}'"));
        }
        return Ok((updated, removed));
    }

    // `--add` and `--remove` are separate permissions, not alternatives:
    // Git reads `--add` as "a path the index does not know may be added"
    // and `--remove` as "a path missing from the work tree may be dropped".
    // Given both, presence on disk decides which applies — so a call that
    // passes both must still stage a file that exists.
    if ctx.remove {
        // `join` with an absolute argument replaces the base, which matches
        // what `resolve_within_worktree` does below, so the two agree on
        // which file is being talked about.
        let present = workdir.join(path_str).symlink_metadata().is_ok();
        if !ctx.add || !present {
            if index.remove(path_str, 0).is_some() {
                removed += 1;
                verbose_line(output, ctx, &format!("remove '{path_str}'"));
            }
            return Ok((updated, removed));
        }
    }

    let tracked = index.tracked(path_str, 0);
    if !tracked && !ctx.add {
        return Err(usage(format!(
            "cannot add '{path_str}' to the index without --add (it is not already tracked)"
        )));
    }

    let absolute = resolve_within_worktree(path_str, workdir).map_err(usage)?;
    let entry = stage_working_tree_path(path_str, &absolute, workdir, ctx.info_only)?;
    crate::utils::index_ext::update_preserving_file_mode(index, entry, file_mode);
    updated += 1;
    verbose_line(output, ctx, &format!("add '{path_str}'"));

    // git runs `chmod_path` right after every successful `update_one`.
    if let Some(target_mode) = ctx.chmod {
        match refresh::chmod_entry(index, path_str, target_mode) {
            Ok(changed) => {
                updated += usize::from(changed);
                let flip = if target_mode & 0o111 != 0 { '+' } else { '-' };
                verbose_line(output, ctx, &format!("chmod {flip}x '{path_str}'"));
            }
            Err(reason) => {
                let flip = if target_mode & 0o111 != 0 { '+' } else { '-' };
                return Err(CliError::fatal(format!(
                    "git update-index: cannot chmod {flip}x '{path_str}'"
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint(reason));
            }
        }
    }
    Ok((updated, removed))
}

/// Emit one `--verbose` action line (human mode only; JSON and global `-q`
/// keep stdout to the machine envelope).
fn verbose_line(output: &OutputConfig, ctx: &PathContext, line: &str) {
    if ctx.verbose && !output.is_json() && !output.quiet {
        let mut stdout = io::stdout().lock();
        let _ = writeln!(stdout, "{line}");
    }
}

/// Parse a `--cacheinfo` spec `<mode>,<object>,<path>` into an [`IndexEntry`].
/// Validates the mode, the object id (length must match the repository hash
/// kind), and rejects worktree-escaping paths.
fn parse_cacheinfo(spec: &str) -> Result<IndexEntry, String> {
    let mut parts = spec.splitn(3, ',');
    let (Some(mode_str), Some(oid_str), Some(path_str)) =
        (parts.next(), parts.next(), parts.next())
    else {
        return Err(format!(
            "invalid --cacheinfo '{spec}': expected <mode>,<object>,<path>"
        ));
    };

    let mode = u32::from_str_radix(mode_str.trim_start_matches("0o"), 8)
        .map_err(|_| format!("invalid mode '{mode_str}' in --cacheinfo"))?;
    if !matches!(mode, 0o100644 | 0o100755 | 0o120000 | 0o160000) {
        return Err(format!(
            "unsupported mode {mode:o} in --cacheinfo (expected 100644, 100755, 120000, or 160000)"
        ));
    }

    let hash = crate::internal::object_format::parse_repo_oid(oid_str)
        .map_err(|_| format!("invalid object id '{oid_str}' in --cacheinfo"))?;
    let expected_len = git_internal::hash::get_hash_kind().hex_len();
    if oid_str.len() != expected_len {
        return Err(format!(
            "object id '{oid_str}' does not match the repository hash format (expected {expected_len} hex chars)"
        ));
    }

    validate_index_path(path_str)?;
    let mut entry = IndexEntry::new_from_blob(path_str.to_string(), hash, 0);
    entry.mode = mode;
    Ok(entry)
}

/// Reject paths that can never be index keys: absolute (POSIX `/...` and
/// Windows `C:\...`), empty, or containing `..` / empty components, so an
/// entry can never escape the worktree.
fn validate_index_path(path_str: &str) -> Result<(), String> {
    if path_str.is_empty() {
        return Err("empty path".to_string());
    }
    let normalized = path_str.replace('\\', "/");
    let bytes = normalized.as_bytes();
    let has_drive_prefix = bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':';
    if normalized.starts_with('/')
        || has_drive_prefix
        || normalized
            .split('/')
            .any(|component| component == ".." || component.is_empty())
    {
        return Err(format!(
            "invalid path '{path_str}' (absolute or `..` traversal not allowed)"
        ));
    }
    Ok(())
}

/// Resolve a positional path against the worktree and ensure it stays inside.
fn resolve_within_worktree(path_str: &str, workdir: &Path) -> Result<PathBuf, String> {
    let absolute = if Path::new(path_str).is_absolute() {
        PathBuf::from(path_str)
    } else {
        workdir.join(path_str)
    };
    if !util::is_sub_path(&absolute, workdir) {
        return Err(format!("path '{path_str}' is outside the repository"));
    }
    Ok(absolute)
}

/// Stage a single working-tree path into an [`IndexEntry`], panic-free.
///
/// Directories and other non-regular, non-symlink entries are rejected with
/// exit 128 rather than panicking inside the blob readers (which `unwrap`
/// `File::open`). Symlinks are stored as a mode-`120000` blob whose content is
/// the link target text (matching Git). Regular files are read into a blob
/// (LFS-aware); an unreadable file is surfaced as a 128 error. With
/// `info_only` the blob hash is computed but the object is never written
/// (git's `--info-only`: `index_path(..., 0)`).
fn stage_working_tree_path(
    path_str: &str,
    absolute: &Path,
    workdir: &Path,
    info_only: bool,
) -> CliResult<IndexEntry> {
    let fatal = |message: String| {
        CliError::fatal(message)
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidTarget)
    };

    let metadata = fs::symlink_metadata(absolute)
        .map_err(|error| fatal(format!("cannot stage '{path_str}': {error}")))?;
    let file_type = metadata.file_type();

    if file_type.is_symlink() {
        let target = fs::read_link(absolute)
            .map_err(|error| fatal(format!("cannot stage symlink '{path_str}': {error}")))?;
        let blob = Blob::from_content_bytes(symlink_target_blob_bytes(&target));
        if !info_only {
            blob.try_save().map_err(|error| {
                CliError::fatal(format!("failed to store object for '{path_str}': {error}"))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::IoWriteFailed)
            })?;
        }
        let mut entry = IndexEntry::new_from_blob(path_str.to_string(), blob.id, 0);
        entry.mode = 0o120000;
        return Ok(entry);
    }

    if !file_type.is_file() {
        return Err(fatal(format!(
            "cannot stage '{path_str}': not a regular file or symlink"
        )));
    }

    // Surface an unreadable regular file as a 128 error instead of a panic in
    // the blob reader.
    fs::File::open(absolute)
        .map_err(|error| fatal(format!("cannot stage '{path_str}': {error}")))?;

    // Stat BEFORE reading: the entry's stat must describe the hashed
    // content, or it is smudged (2026-08-06 R0-8 review).
    let pre_read = std::fs::symlink_metadata(absolute).ok();
    let entry = if info_only {
        let hash = crate::command::calc_file_blob_hash(absolute)
            .map_err(|error| fatal(format!("cannot stage '{path_str}': {error}")))?;
        crate::command::verified_index_entry(Path::new(path_str), hash, workdir, pre_read.as_ref())
            .map_err(|error| {
                CliError::fatal(format!("failed to stage '{path_str}': {error}"))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::IoReadFailed)
            })?
    } else {
        let blob = if lfs::is_lfs_tracked(absolute) {
            Blob::from_lfs_file(absolute)
        } else {
            Blob::from_file(absolute)
        };
        blob.try_save().map_err(|error| {
            CliError::fatal(format!("failed to store object for '{path_str}': {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoWriteFailed)
        })?;
        crate::command::verified_index_entry(
            Path::new(path_str),
            blob.id,
            workdir,
            pre_read.as_ref(),
        )
        .map_err(|error| {
            CliError::fatal(format!("failed to stage '{path_str}': {error}"))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::IoReadFailed)
        })?
    };
    Ok(entry)
}

/// Every path a `--refresh` pass must consider, in index order: the stage-0
/// files plus every unmerged (stage 1-3) name. Like git 2.56, `update-index
/// --refresh` refreshes the WHOLE index — positional paths are (re)staged at
/// their argv positions, they never limit the refresh candidates (verified
/// against `/usr/bin/git`: `git update-index --refresh <path>` still reports
/// every modified file).
fn all_refresh_candidates(index: &Index) -> Vec<PathBuf> {
    let mut names: std::collections::BTreeSet<PathBuf> = index
        .tracked_files()
        .into_iter()
        .collect::<std::collections::BTreeSet<PathBuf>>(
    );
    for stage in [1u8, 2, 3] {
        for entry in index.tracked_entries(stage) {
            names.insert(PathBuf::from(&entry.name));
        }
    }
    names.into_iter().collect()
}

/// Render the git-matching refresh lines in index order: `<path>: needs
/// update` and `<path>: needs merge` (missing files render as needs-update,
/// exactly like git's `deleted_fmt`).
fn render_refresh_report(
    report: &refresh::RefreshReport,
    output: &OutputConfig,
    suppress_refresh_output: bool,
) {
    if output.is_json() || suppress_refresh_output {
        return;
    }
    let mut stdout = io::stdout().lock();
    for (path, kind) in &report.events {
        let suffix = match kind {
            refresh::RefreshEventKind::NeedsMerge => "needs merge",
            refresh::RefreshEventKind::NeedsUpdate | refresh::RefreshEventKind::Missing => {
                "needs update"
            }
        };
        let _ = writeln!(stdout, "{}: {suffix}", path.display());
    }
}

/// One parsed `--index-info` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum IndexInfoLine {
    /// `<mode> SP <oid> TAB <path>` etc.: register the entry.
    Entry {
        path: String,
        mode: u32,
        stage: u8,
        oid: git_internal::hash::ObjectHash,
    },
    /// `0 SP <oid> TAB <path>`: remove every stage of the path.
    Remove { path: String },
    /// A path git's `verify_path` rejects: skipped with a stderr note.
    Ignored { path: String },
}

/// Parse one `--index-info` record in git's three accepted formats
/// (`read_index_info`, git 2.56):
///
/// 1. `<mode> SP <oid> TAB <path>`
/// 2. `<mode> SP <type> SP <oid> TAB <path>` (the type token is ignored)
/// 3. `<mode> SP <oid> SP <stage> TAB <path>` (stage 0-3)
///
/// Disambiguation mirrors git byte-for-byte: when the two bytes before the
/// TAB are `SP <digit 0-3>` the line is the stage form, otherwise the last
/// `hexsz` hex bytes are the oid and the byte before them must be a space.
/// The oid is validated (hex + repository hash length) even for `mode 0`.
/// `Err` marks a malformed line: git dies with `malformed index info <line>`
/// and applies nothing.
pub(crate) fn parse_index_info_line(
    line: &str,
    nul_terminated: bool,
) -> Result<IndexInfoLine, String> {
    let hexsz = git_internal::hash::get_hash_kind().hex_len();
    let bytes = line.as_bytes();

    // Octal mode up to the first space (git's `strtoul(..., 8)` plus the
    // explicit `*ptr == ' '` check; leading whitespace and signs are
    // intentionally NOT tolerated — libra is stricter).
    let mode_end = bytes
        .iter()
        .position(|byte| !(b'0'..=b'7').contains(byte))
        .unwrap_or(bytes.len());
    if mode_end == 0 || mode_end >= bytes.len() || bytes[mode_end] != b' ' {
        return Err("malformed mode".to_string());
    }
    let mode =
        u32::from_str_radix(&line[..mode_end], 8).map_err(|_| "malformed mode".to_string())?;
    let body = &line[mode_end + 1..];

    let Some(tab) = body.find('\t') else {
        return Err("missing TAB".to_string());
    };
    let head = &body[..tab];
    let raw_path = &body[tab + 1..];
    // git: `if (!tab || tab - ptr < hexsz + 1) goto bad_line;` — the head
    // must at least hold a bare oid (format 1); the stage form checks its
    // longer minimum below.
    if head.len() < hexsz {
        return Err("object id too short".to_string());
    }

    let head_bytes = head.as_bytes();
    let (oid_range, stage) = if head_bytes.len() >= 2
        && head_bytes[head_bytes.len() - 2] == b' '
        && (b'0'..=b'3').contains(&head_bytes[head_bytes.len() - 1])
    {
        // Stage form: `<oid> SP <stage>`; the oid ends two bytes before TAB.
        if head.len() < hexsz + 2 {
            return Err("object id too short".to_string());
        }
        let start = head.len() - hexsz - 2;
        (
            start..start + hexsz,
            (head_bytes[head_bytes.len() - 1] - b'0'),
        )
    } else {
        // Formats 1 and 2: the oid is the last `hexsz` bytes.
        let start = head.len() - hexsz;
        (start..head.len(), 0u8)
    };
    // git: `if (get_oid_hex(tab - hexsz, &oid) || tab[-(hexsz + 1)] != ' ')`
    // — the oid must be valid hex and preceded by a space (the mode separator
    // for format 1, the type-token separator for format 2/3).
    if oid_range.start > 0 && head_bytes[oid_range.start - 1] != b' ' {
        return Err("object id must be preceded by a space".to_string());
    }
    let oid = crate::internal::object_format::parse_repo_oid(&head[oid_range.clone()])
        .map_err(|_| "invalid object id".to_string())?;
    if head[oid_range.clone()].len() != hexsz {
        return Err("invalid object id".to_string());
    }

    // C-style unquoting for LF records whose path starts with `"` (git's
    // `unquote_c_style`; NUL records are never quoted).
    let path = if !nul_terminated && raw_path.starts_with('"') {
        match decode_c_quoted(raw_path) {
            Ok(Some(decoded)) => decoded,
            Ok(None) => raw_path.to_string(),
            Err(_) => return Err("bad quoting of path name".to_string()),
        }
    } else {
        raw_path.to_string()
    };

    // git's `verify_path` rejects the path softly (stderr note, line skipped).
    if validate_index_path(&path).is_err() {
        return Ok(IndexInfoLine::Ignored { path });
    }

    if mode == 0 {
        return Ok(IndexInfoLine::Remove { path });
    }
    // Stricter than git (which accepts any mode bits): the same mode set
    // `--cacheinfo` enforces. A mode outside it makes the line malformed.
    if !matches!(mode, 0o100644 | 0o100755 | 0o120000 | 0o160000) {
        return Err(format!("unsupported mode {mode:o}"));
    }
    Ok(IndexInfoLine::Entry {
        path,
        mode,
        stage,
        oid,
    })
}

/// Apply one parsed `--index-info` record. Returns (added, removed).
fn apply_index_info_line(
    index: &mut Index,
    line: IndexInfoLine,
    verbose: bool,
    output: &OutputConfig,
) -> (usize, usize) {
    match line {
        IndexInfoLine::Entry {
            path,
            mode,
            stage,
            oid,
        } => {
            // git's `add_index_entry`: inserting stage 0 removes every
            // same-name entry; inserting stage 1-3 keeps stage 0 and the
            // other stages (verified against `/usr/bin/git` — M-INPUT N6).
            if stage == 0 {
                for other in [1u8, 2, 3] {
                    index.remove(&path, other);
                }
            }
            let mut entry = IndexEntry::new_from_blob(path.clone(), oid, 0);
            entry.mode = mode;
            entry.flags.stage = stage;
            index.add(entry);
            if verbose {
                verbose_line(
                    output,
                    &PathContext {
                        verbose: true,
                        ..Default::default()
                    },
                    &format!("add '{path}'"),
                );
            }
            (1, 0)
        }
        IndexInfoLine::Remove { path } => {
            let mut removed = 0usize;
            for stage in [0u8, 1, 2, 3] {
                if index.remove(&path, stage).is_some() {
                    removed = 1;
                }
            }
            (0, removed)
        }
        IndexInfoLine::Ignored { path } => {
            eprintln!("Ignoring path {path}");
            (0, 0)
        }
    }
}

/// Read and apply `--index-info` records from stdin. Parsing and application
/// happen strictly per record against the in-memory index; any malformed
/// record returns a fatal (exit 128) BEFORE the index is saved, so the
/// on-disk index stays byte-identical (GC-PL-02).
fn apply_index_info_stream(
    index: &mut Index,
    nul: bool,
    ctx: &PathContext,
    output: &OutputConfig,
) -> CliResult<(usize, usize)> {
    let mut updated = 0usize;
    let mut removed = 0usize;
    for record in read_stdin_records(nul).map_err(|error| {
        CliError::fatal(format!("failed to read stdin: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        match parse_index_info_line(&record, nul) {
            Ok(line) => {
                let (added, dropped) = apply_index_info_line(index, line, ctx.verbose, output);
                updated += added;
                removed += dropped;
            }
            Err(reason) if reason == "bad quoting of path name" => {
                return Err(
                    CliError::fatal("git update-index: bad quoting of path name")
                        .with_exit_code(128)
                        .with_stable_code(StableErrorCode::CliInvalidTarget),
                );
            }
            Err(reason) => {
                return Err(CliError::fatal(format!(
                    "malformed index info {}",
                    truncate_for_error(&record)
                ))
                .with_exit_code(128)
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint(reason));
            }
        }
    }
    Ok((updated, removed))
}

/// Read and apply `--stdin` path records. Like git, a blank line or a path
/// `verify_path` rejects is skipped with a stderr note (exit 0), and each
/// accepted path goes through the same `update_one` as a positional path
/// (with the flag state current at the `--stdin` position).
fn apply_stdin_stream(
    index: &mut Index,
    nul: bool,
    workdir: &Path,
    file_mode: bool,
    ctx: &PathContext,
    output: &OutputConfig,
) -> CliResult<(usize, usize)> {
    let mut updated = 0usize;
    let mut removed = 0usize;
    for raw in read_stdin_records(nul).map_err(|error| {
        CliError::fatal(format!("failed to read stdin: {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        // git unquotes `"`-prefixed records in LF mode (`strbuf_getline`).
        let path = if !nul && raw.starts_with('"') {
            match decode_c_quoted(&raw) {
                Ok(Some(decoded)) => decoded,
                Ok(None) => raw.clone(),
                Err(_) => {
                    return Err(CliError::fatal("line is badly quoted")
                        .with_exit_code(128)
                        .with_stable_code(StableErrorCode::CliInvalidTarget));
                }
            }
        } else {
            raw.clone()
        };
        if validate_index_path(&path).is_err() {
            eprintln!("Ignoring path {path}");
            continue;
        }
        let (added, dropped) = process_one_path(index, &path, ctx, workdir, file_mode, output)?;
        updated += added;
        removed += dropped;
    }
    Ok((updated, removed))
}

/// Stream stdin records line-by-line (LF- or NUL-terminated, stripping the
/// terminator) so peak memory stays O(longest record), not O(input).
fn read_stdin_records(nul: bool) -> io::Result<Vec<String>> {
    let delimiter = if nul { 0u8 } else { b'\n' };
    let stdin = io::stdin();
    let mut reader = stdin.lock();
    let mut records = Vec::new();
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(delimiter, &mut line)?;
        if read == 0 {
            break;
        }
        if line.last() == Some(&delimiter) {
            line.pop();
        }
        records.push(String::from_utf8_lossy(&line).into_owned());
    }
    Ok(records)
}

/// Bound the line echoed in a `malformed index info` error (plan issues/479
/// security note: never echo unbounded input).
fn truncate_for_error(line: &str) -> String {
    const LIMIT: usize = 200;
    if line.len() <= LIMIT {
        line.to_string()
    } else {
        format!("{}…", &line[..LIMIT])
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{IndexInfoLine, UpdateIndexArgs, parse_index_info_line};

    #[test]
    fn refresh_quiet_provenance_respects_values_and_path_boundary() {
        use super::{Op, ordered_operations_from_tokens};
        use crate::utils::{error::CliError, output::OutputConfig};

        let cases: &[(&[&str], bool, &[bool])] = &[
            (
                &["update-index", "--refresh", "-q", "--really-refresh"],
                false,
                &[false, true],
            ),
            (
                &["--quiet", "update-index", "-q", "--refresh"],
                true,
                &[true],
            ),
            (&["-q", "update-index", "--refresh"], true, &[false]),
            (&["update-index", "--quiet", "--refresh"], true, &[false]),
            (
                &["update-index", "-q", "--refresh", "--quiet"],
                true,
                &[true],
            ),
            (
                &["update-index", "-q", "--refresh", "--", "--quiet"],
                false,
                &[true],
            ),
            (&["update-index", "--refresh", "--", "-q"], true, &[false]),
            (
                &[
                    "update-index",
                    "--index-file",
                    "update-index",
                    "-q",
                    "--refresh",
                ],
                false,
                &[true],
            ),
            (
                &["update-index", "--index-file=--quiet", "-q", "--refresh"],
                false,
                &[true],
            ),
            (
                &[
                    "--progress",
                    "none",
                    "--max-connections",
                    "1",
                    "update-index",
                    "-q",
                    "--refresh",
                ],
                false,
                &[true],
            ),
            (
                &["update-index", "--color", "--json", "-q", "--refresh"],
                false,
                &[true],
            ),
            (&["-qJ", "update-index", "-q", "--refresh"], true, &[true]),
            (&["update-index", "-zq", "--refresh"], false, &[true]),
            (&[], true, &[]),
        ];
        let output = OutputConfig {
            quiet: true,
            ..OutputConfig::default()
        };
        for (argv, suppressed, switches) in cases {
            let tokens: Vec<String> = argv.iter().map(|arg| (*arg).to_owned()).collect();
            let ordered = ordered_operations_from_tokens(&tokens, &CliError::command_usage)
                .expect("ordered arguments");
            assert_eq!(
                ordered.suppress_refresh_output(&output),
                *suppressed,
                "{argv:?}"
            );
            let actual: Vec<bool> = ordered
                .ops
                .iter()
                .filter_map(|op| match op {
                    Op::Refresh { switches } => Some(switches.quiet),
                    _ => None,
                })
                .collect();
            assert_eq!(actual, *switches, "{argv:?}");
            if argv.contains(&"--") {
                assert!(
                    matches!(ordered.ops.last(), Some(Op::Path { .. })),
                    "{argv:?}"
                );
            }
        }
    }

    #[test]
    fn force_remove_flag_parses_and_defaults_off() {
        let args = UpdateIndexArgs::try_parse_from(["update-index", "--force-remove", "a.txt"])
            .expect("--force-remove parses");
        assert!(args.force_remove);
        assert_eq!(args.paths, vec!["a.txt"]);
        assert!(!args.add && !args.remove);

        let plain = UpdateIndexArgs::try_parse_from(["update-index", "a.txt"])
            .expect("plain invocation parses");
        assert!(!plain.force_remove);
    }

    /// SW-07: `--skip-worktree`/`--no-skip-worktree` parse independently and
    /// default off.
    #[test]
    fn skip_worktree_flags_parse_and_default_off() {
        let args = UpdateIndexArgs::try_parse_from(["update-index", "--skip-worktree", "a.txt"])
            .expect("--skip-worktree parses");
        assert!(args.skip_worktree && !args.no_skip_worktree);

        let args = UpdateIndexArgs::try_parse_from(["update-index", "--no-skip-worktree", "a.txt"])
            .expect("--no-skip-worktree parses");
        assert!(args.no_skip_worktree && !args.skip_worktree);

        let plain = UpdateIndexArgs::try_parse_from(["update-index", "a.txt"])
            .expect("plain invocation parses");
        assert!(!plain.skip_worktree && !plain.no_skip_worktree);
    }

    /// UI-01: the new batch/refresh/chmod surface parses and defaults off.
    #[test]
    fn ui_flags_parse_and_default_off() {
        let args = UpdateIndexArgs::try_parse_from([
            "update-index",
            "--refresh",
            "--really-refresh",
            "--ignore-missing",
            "--unmerged",
            "--info-only",
            "--verbose",
            "--chmod=+x",
            "--index-info",
            "-z",
            "--stdin",
        ])
        .expect("UI flags parse");
        assert!(args.refresh && args.really_refresh && args.ignore_missing);
        assert!(args.unmerged && args.info_only && args.verbose);
        assert_eq!(args.chmod, vec!["+x"]);
        assert!(args.index_info && args.stdin_list && args.nul_terminated);

        let plain = UpdateIndexArgs::try_parse_from(["update-index", "a.txt"])
            .expect("plain invocation parses");
        assert!(
            !plain.refresh
                && !plain.really_refresh
                && !plain.ignore_missing
                && !plain.unmerged
                && !plain.info_only
                && !plain.verbose
                && !plain.index_info
                && !plain.stdin_list
                && !plain.nul_terminated
                && plain.chmod.is_empty()
        );
    }

    /// UI-01 unit: the three `--index-info` line formats parse into the
    /// expected entries (sha1 repo hex length 40).
    #[test]
    fn index_info_line_parses_three_formats() {
        let oid = "a".repeat(git_internal::hash::get_hash_kind().hex_len());
        // Format 1: mode SP oid TAB path.
        match parse_index_info_line(&format!("100644 {oid}\tdir/f.txt"), false) {
            Ok(IndexInfoLine::Entry {
                path, mode, stage, ..
            }) => {
                assert_eq!(path, "dir/f.txt");
                assert_eq!(mode, 0o100644);
                assert_eq!(stage, 0);
            }
            other => panic!("format 1 must parse as a stage-0 entry: {other:?}"),
        }
        // Format 2: the type token is ignored (even when the oid's last hex
        // char is 0-3 — git's stage sniff only sees the two bytes before TAB).
        match parse_index_info_line(&format!("100755 blob {oid}\ttool.sh"), false) {
            Ok(IndexInfoLine::Entry { mode, stage, .. }) => {
                assert_eq!(mode, 0o100755);
                assert_eq!(stage, 0);
            }
            other => panic!("format 2 must parse as a stage-0 entry: {other:?}"),
        }
        // Format 3: mode SP oid SP stage TAB path.
        match parse_index_info_line(&format!("100644 {oid} 3\tf.txt"), false) {
            Ok(IndexInfoLine::Entry { stage, mode, .. }) => {
                assert_eq!(stage, 3);
                assert_eq!(mode, 0o100644);
            }
            other => panic!("format 3 must parse as a stage-3 entry: {other:?}"),
        }
        // A type token AND a stage (git tolerates `mode SP type SP oid SP stage`).
        match parse_index_info_line(&format!("100644 blob {oid} 1\tf.txt"), false) {
            Ok(IndexInfoLine::Entry { stage, .. }) => assert_eq!(stage, 1),
            other => panic!("type + stage form must parse: {other:?}"),
        }
    }

    /// UI-01 unit: `mode 0` deletes all stages (still validating the oid),
    /// and `"`-quoted paths decode in LF mode only.
    #[test]
    fn index_info_line_mode_zero_and_quoting() {
        let oid = "b".repeat(git_internal::hash::get_hash_kind().hex_len());
        assert_eq!(
            parse_index_info_line(&format!("0 {oid}\tf.txt"), false),
            Ok(IndexInfoLine::Remove {
                path: "f.txt".to_string()
            })
        );
        assert_eq!(
            parse_index_info_line(&format!("0 {oid} 3\tf.txt"), false),
            Ok(IndexInfoLine::Remove {
                path: "f.txt".to_string()
            })
        );
        // The oid must still be valid hex for mode 0.
        assert!(parse_index_info_line("0 zzzz\tf.txt", false).is_err());
        // Quoted path decodes in LF mode; NUL mode never quotes.
        match parse_index_info_line(&format!("100644 {oid}\t\"we ird.txt\""), false) {
            Ok(IndexInfoLine::Entry { path, .. }) => assert_eq!(path, "we ird.txt"),
            other => panic!("quoted path must decode: {other:?}"),
        }
        match parse_index_info_line(&format!("100644 {oid}\t\"we ird.txt\""), true) {
            Ok(IndexInfoLine::Entry { path, .. }) => assert_eq!(path, "\"we ird.txt\""),
            other => panic!("NUL mode must keep the raw path: {other:?}"),
        }
    }

    /// UI-01 unit: malformed lines fail closed, and a bad path is the soft
    /// `Ignoring path` skip (git's `verify_path`).
    #[test]
    fn index_info_line_rejects_malformed_input() {
        let hexsz = git_internal::hash::get_hash_kind().hex_len();
        let oid = "c".repeat(hexsz);
        assert!(parse_index_info_line("garbage", false).is_err());
        assert!(parse_index_info_line(&format!("100644 {oid} f.txt"), false).is_err());
        assert!(parse_index_info_line(&format!("100644 {oid}"), false).is_err());
        assert!(parse_index_info_line(&format!("100644 {oid} 4\tf.txt"), false).is_err());
        assert!(parse_index_info_line(&format!("10064z {oid}\tf.txt"), false).is_err());
        assert!(parse_index_info_line(&format!("100600 {oid}\tf.txt"), false).is_err());
        assert!(
            parse_index_info_line(&format!("100644 {}\tf.txt", "0".repeat(39)), false).is_err()
        );
        assert!(parse_index_info_line(&format!("100644 {} no-space\tf.txt", oid), false).is_err());
        match parse_index_info_line(&format!("100644 {oid}\t../escape.txt"), false) {
            Ok(IndexInfoLine::Ignored { .. }) => {}
            other => panic!("`..` path must soft-skip: {other:?}"),
        }
        match parse_index_info_line(&format!("100644 {oid}\t"), false) {
            Ok(IndexInfoLine::Ignored { .. }) => {}
            other => panic!("empty path must soft-skip: {other:?}"),
        }
    }
}

#[cfg(test)]
mod info_only_tests {
    use std::fs;

    use super::stage_working_tree_path;

    /// UI-03 C4 (staging-path pin): `stage_working_tree_path` with
    /// `info_only = true` hashes the content but performs no object write.
    /// The blob must not appear in the loose object store even though the
    /// returned entry carries the correct oid.
    #[test]
    fn info_only_stages_entry_without_writing_object() {
        let dir = tempfile::tempdir().expect("tempdir");
        let workdir = dir.path();
        fs::write(workdir.join("q.txt"), "q\n").expect("write q.txt");
        let absolute = workdir.join("q.txt");

        let entry = stage_working_tree_path("q.txt", &absolute, workdir, true)
            .expect("info-only staging succeeds");
        assert_eq!(entry.mode, 0o100644);
        let oid = entry.hash.to_string();
        let loose = workdir.join("objects").join(&oid[..2]).join(&oid[2..]);
        assert!(!loose.exists(), "info-only must not write the loose object");
        // The oid is the real git blob id for "q\n".
        assert_eq!(
            oid, "bca70f35318f31dd1d1d1d2d2e64c19b880899ff",
            "blob hash of q.txt"
        );
    }
}
