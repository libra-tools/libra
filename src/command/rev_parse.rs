//! Implements `rev-parse` to resolve revision names and print basic repository paths.

use std::{
    fs,
    io::{self, Write},
    path::PathBuf,
};

use clap::Parser;
use git_internal::hash::ObjectHash;
use serde::Serialize;

use crate::{
    internal::{
        branch::{Branch, BranchStoreError},
        config::ConfigKv,
        head::Head,
        tag,
    },
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        path,
        text::SHORT_HASH_LEN,
        util::{self, CommitBaseError},
    },
};

/// `--help` examples shown in `libra rev-parse --help` output.
///
/// `rev-parse` is the canonical script bridge: resolve a revision spec
/// to a commit hash, a short hash, a branch name, or print the
/// repository top-level. The banner pins the four mutually-exclusive
/// modes plus a JSON variant for agents so users see all supported
/// forms without reading the design doc. Cross-cutting `--help`
/// EXAMPLES rollout per `docs/development/commands/_general.md` item B.
pub const REV_PARSE_EXAMPLES: &str = "\
EXAMPLES:
    libra rev-parse HEAD                Print the full 40-char hash for HEAD
    libra rev-parse main~3              Resolve any revision spec to a full hash
    libra rev-parse --short HEAD        Print a non-ambiguous short hash
    libra rev-parse --sq HEAD           Print the resolved object name, shell-quoted
    libra rev-parse --abbrev-ref HEAD   Print the branch name (or HEAD when detached)
    libra rev-parse --symbolic-full-name HEAD  Print HEAD's full ref name (refs/heads/...)
    libra rev-parse --symbolic main     Echo a resolvable spec verbatim (main, not refs/heads/main)
    libra rev-parse --revs-only \"$@\"     Print only the arguments that are revisions (drop flags/paths)
    libra rev-parse --show-toplevel     Print the absolute path of the repository root
    libra rev-parse --verify HEAD       Assert HEAD resolves to one object (exit 128 if not)
    libra rev-parse --is-inside-work-tree  Print true/false for working-tree context
    libra rev-parse --is-inside-git-dir    Print true/false for .libra-directory context
    libra rev-parse --is-shallow-repository  Print true/false for shallow repository state
    libra rev-parse --absolute-git-dir  Print the canonicalized absolute .libra path
    libra rev-parse --json HEAD         Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = REV_PARSE_EXAMPLES)]
pub struct RevParseArgs {
    /// Show a non-ambiguous short object name. Accepts an optional length
    /// (e.g. `--short=8`) to request a specific abbreviation.
    #[clap(long, num_args = 0..=1, require_equals = true, default_missing_value = "7", conflicts_with_all = ["abbrev_ref", "show_toplevel"])]
    pub short: Option<String>,

    /// Show the branch name instead of the commit hash.
    #[clap(long = "abbrev-ref", conflicts_with_all = ["show_toplevel", "short"])]
    pub abbrev_ref: bool,

    /// Resolve SPEC to its full ref name (`refs/heads/…` / `refs/tags/…` /
    /// `refs/remotes/…`, or `HEAD` when detached). A valid object that is not a
    /// ref prints nothing (exit 0); an unresolvable name fails (exit 128).
    #[clap(long = "symbolic-full-name", conflicts_with_all = ["show_toplevel", "short", "abbrev_ref", "is_inside_work_tree", "is_inside_git_dir", "is_bare_repository", "git_dir", "absolute_git_dir", "show_prefix", "show_cdup"])]
    pub symbolic_full_name: bool,

    /// Print SPEC in symbolic form, as close to the original input as possible
    /// (Git's `--symbolic`): a resolvable ref / revision / object id is echoed
    /// verbatim, an unresolvable name fails (exit 128). Differs from
    /// `--symbolic-full-name`, which expands a ref to its full `refs/…` name.
    #[clap(long = "symbolic", conflicts_with_all = ["symbolic_full_name", "show_toplevel", "short", "abbrev_ref", "is_inside_work_tree", "is_inside_git_dir", "is_bare_repository", "git_dir", "absolute_git_dir", "show_prefix", "show_cdup"])]
    pub symbolic: bool,

    /// Show the absolute path of the top-level working tree.
    #[clap(long = "show-toplevel", conflicts_with_all = ["abbrev_ref", "short", "spec"])]
    pub show_toplevel: bool,

    /// Verify that the revision resolves to exactly one object; fail (exit 128) otherwise.
    /// With the global `-q`/`--quiet`, failure is silent with exit code 1.
    #[clap(long, conflicts_with_all = ["show_toplevel", "abbrev_ref", "is_inside_work_tree", "is_inside_git_dir", "is_bare_repository", "git_dir", "absolute_git_dir"])]
    pub verify: bool,

    /// Use this revision when no SPEC is given (Git's `--default <arg>`).
    #[clap(long, value_name = "ARG", conflicts_with_all = ["show_toplevel", "is_inside_work_tree", "is_inside_git_dir", "is_bare_repository", "git_dir", "absolute_git_dir"])]
    pub default: Option<String>,

    /// Shell-quote the resolved object name for safe shell consumption
    /// (Git's `--sq`). Only affects the resolved-revision output, not the
    /// repository-query modes (e.g. `--show-toplevel`).
    #[clap(long = "sq")]
    pub sq: bool,

    /// Print "true" when run inside a working tree, "false" otherwise.
    #[clap(long = "is-inside-work-tree", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec", "is_bare_repository", "git_dir", "absolute_git_dir"])]
    pub is_inside_work_tree: bool,

    /// Print "true" when the current directory is inside the `.libra` directory, "false" otherwise.
    #[clap(long = "is-inside-git-dir", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec", "is_inside_work_tree", "is_bare_repository", "git_dir", "absolute_git_dir"])]
    pub is_inside_git_dir: bool,

    /// Print "true" when the repository is bare, "false" otherwise.
    #[clap(long = "is-bare-repository", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec", "git_dir", "absolute_git_dir"])]
    pub is_bare_repository: bool,

    /// Print "true" when `.libra/shallow` contains at least one shallow boundary.
    #[clap(long = "is-shallow-repository", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec", "symbolic_full_name", "symbolic", "verify", "default", "is_inside_work_tree", "is_inside_git_dir", "is_bare_repository", "git_dir", "absolute_git_dir", "show_prefix", "show_cdup"])]
    pub is_shallow_repository: bool,

    /// Print the path to the `.libra` directory.
    #[clap(long = "git-dir", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec"])]
    pub git_dir: bool,

    /// Print the canonicalized absolute path to the `.libra` directory (like
    /// `--git-dir`, but always absolute).
    #[clap(long = "absolute-git-dir", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec", "git_dir"])]
    pub absolute_git_dir: bool,

    /// Print the path relative from the current directory to the repository root.
    #[clap(long = "show-cdup", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec"])]
    pub show_cdup: bool,

    /// Print the path of the current directory relative to the repository root.
    #[clap(long = "show-prefix", conflicts_with_all = ["short", "abbrev_ref", "show_toplevel", "spec"])]
    pub show_prefix: bool,

    /// Output-filter mode (Git's `--flags`): print only the SPEC arguments that
    /// are flags (begin with `-`), plus any revisions, dropping non-flag paths.
    #[clap(long = "flags")]
    pub flags: bool,

    /// Output-filter mode (Git's `--no-flags`): drop flag arguments from the
    /// output, keeping revisions (resolved) and non-flag paths.
    #[clap(long = "no-flags")]
    pub no_flags: bool,

    /// Output-filter mode (Git's `--revs-only`): print only the arguments that
    /// resolve to revisions (as object names), dropping flags and paths.
    #[clap(long = "revs-only")]
    pub revs_only: bool,

    /// Output-filter mode (Git's `--no-revs`): drop revision arguments, keeping
    /// flags and non-revision paths.
    #[clap(long = "no-revs")]
    pub no_revs: bool,

    /// Revisions / arguments to parse (everything BEFORE a `--` separator).
    /// Defaults to HEAD when omitted. Multiple arguments are each resolved (and,
    /// with the output-filter flags above, classified as flag / rev / path).
    /// `allow_hyphen_values` lets unknown `-`-prefixed arguments (e.g. `-x`) be
    /// captured for classification, like `git rev-parse`. (Defined options must
    /// precede the SPECs.)
    #[clap(value_name = "SPEC", allow_hyphen_values = true)]
    pub spec: Vec<String>,

    /// Arguments AFTER a `--` separator. clap strips the `--` itself, so this
    /// dedicated `last = true` field is what tells the output-filter modes that a
    /// `--` was present and that these arguments are paths (never revisions),
    /// matching `git rev-parse --revs-only -- <path>`.
    #[clap(last = true, value_name = "PATH", allow_hyphen_values = true)]
    pub after_dashdash: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RevParseOutput {
    mode: &'static str,
    input: Option<String>,
    value: String,
}

pub async fn execute(args: RevParseArgs) -> Result<(), String> {
    execute_safe(args, &OutputConfig::default())
        .await
        .map_err(|err| err.render())
}

/// Recover a value-less LEADING `--` that clap discards entirely.
///
/// clap routes a `--` separator three ways depending on position: a `--` after a
/// positional stays verbatim inside `spec` (via `allow_hyphen_values`); a leading
/// `--` followed by paths routes those paths to the `last = true` `after_dashdash`
/// field; but a leading `--` with *nothing* after it leaves no trace in the parsed
/// struct. Git still prints that `--` (so an argv can be reconstructed), so we
/// recover its presence from the raw process arguments.
///
/// Guarded by a preceding `rev-parse` token so the test harness's own argv (which
/// never contains that token) can never spuriously report a separator — this is
/// only consulted when `spec` is empty and `after_dashdash` is empty.
fn leading_bare_dashdash_in_argv() -> bool {
    let argv: Vec<String> = std::env::args().collect();
    match argv.iter().position(|a| a == "rev-parse") {
        Some(idx) => argv[idx + 1..].iter().any(|a| a == "--"),
        None => false,
    }
}

/// Split the positional arguments at the first `--` separator into the
/// revision specs (before `--`) and the paths (after `--`), plus whether a
/// separator was present at all. Normalises clap's three `--` routings (see
/// [`leading_bare_dashdash_in_argv`]) into one consistent view.
fn split_positionals(args: &RevParseArgs) -> (Vec<String>, Vec<String>, bool) {
    if let Some(pos) = args.spec.iter().position(|a| a == "--") {
        // `--` after a positional: kept verbatim inside `spec`.
        (
            args.spec[..pos].to_vec(),
            args.spec[pos + 1..].to_vec(),
            true,
        )
    } else if !args.after_dashdash.is_empty() {
        // Leading `--` followed by paths.
        (args.spec.clone(), args.after_dashdash.clone(), true)
    } else if args.spec.is_empty() && leading_bare_dashdash_in_argv() {
        // Value-less leading `--` that clap discarded.
        (Vec::new(), Vec::new(), true)
    } else {
        (args.spec.clone(), Vec::new(), false)
    }
}

pub async fn execute_safe(args: RevParseArgs, output: &OutputConfig) -> CliResult<()> {
    if !args.show_toplevel {
        util::require_repo().map_err(|_| CliError::repo_not_found())?;
    }

    let any_filter = args.flags || args.no_flags || args.revs_only || args.no_revs;

    // `--verify` / `--short` are single-revision modes; Git's behavior when they
    // are combined with the output-filter flags is ill-defined (e.g. `--short
    // --no-revs HEAD -- file` prints nothing at all), so Libra rejects that
    // combination with a clear usage error rather than guessing.
    if (args.verify || args.short.is_some()) && any_filter {
        return Err(CliError::command_usage(
            "the output-filter flags (--flags/--no-flags/--revs-only/--no-revs) \
             cannot be combined with --verify or --short"
                .to_string(),
        ));
    }

    // Output-filter modes (`--flags`/`--no-flags`/`--revs-only`/`--no-revs`)
    // classify each positional argument as a flag / revision / path and print a
    // filtered subset, rather than resolving a single revision.
    if any_filter {
        return run_rev_parse_filter(&args, output).await;
    }

    // Split the positional args at the `--` separator (revisions vs paths),
    // matching `git rev-parse <rev> -- <path>`.
    let (rev_specs, post_paths, saw_dashdash) = split_positionals(&args);

    // Git groups the non-filter modes into three categories that handle the `--`
    // separator differently:
    //   * REPOSITORY-QUERY modes (`--show-toplevel`, `--git-dir`, `--is-*`, …)
    //     ignore the revision and print exactly one query value; the `--` and
    //     paths are still echoed.
    //   * SINGLE-REVISION modes (`--verify`, `--short`) require EXACTLY one
    //     revision and print ONLY that object — never the post-`--` paths.
    //   * PER-REVISION modes (`--abbrev-ref`, `--symbolic`, `--symbolic-full-name`,
    //     and plain resolve) resolve each revision and echo the `--` and paths.
    let is_query_mode = args.show_toplevel
        || args.git_dir
        || args.absolute_git_dir
        || args.is_inside_work_tree
        || args.is_inside_git_dir
        || args.is_bare_repository
        || args.is_shallow_repository
        || args.show_prefix
        || args.show_cdup;
    let single_revision = (args.verify || args.short.is_some()) && !is_query_mode;

    // Resolve one revision per pre-`--` SPEC. Defaulting rules (matching Git):
    //   * a query mode runs exactly once with a placeholder it ignores;
    //   * explicit `--default <arg>` supplies the revision whenever there are no
    //     revision args before the separator — even when paths follow a `--`;
    //   * the implicit HEAD default applies only when there is no SPEC and no `--`
    //     at all (`rev-parse` alone).
    let specs: Vec<String> = if is_query_mode {
        vec!["HEAD".to_string()]
    } else if !rev_specs.is_empty() {
        rev_specs
    } else if let Some(default) = &args.default {
        vec![default.clone()]
    } else if !saw_dashdash {
        vec!["HEAD".to_string()]
    } else {
        Vec::new()
    };

    // The single-revision modes require EXACTLY one revision (paths after `--` do
    // not count); zero or more than one is an error ("Needed a single revision").
    if single_revision && specs.len() != 1 {
        if output.quiet {
            return Err(CliError::silent_exit(1));
        }
        return Err(CliError::fatal("Needed a single revision".to_string())
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::CliInvalidTarget));
    }

    let mut results = Vec::with_capacity(specs.len());
    for spec in &specs {
        match resolve_rev_parse(spec, &args).await {
            Ok(result) => results.push(result),
            Err(error) => {
                // The single-revision modes fail with exit 128 when the argument
                // does not name exactly one object; with the global `-q`/`--quiet`
                // they fail silently with exit code 1 instead of printing.
                if single_revision {
                    if output.quiet {
                        return Err(CliError::silent_exit(1));
                    }
                    return Err(CliError::fatal(format!(
                        "Needed a single revision (could not resolve '{spec}')"
                    ))
                    .with_exit_code(128)
                    .with_stable_code(StableErrorCode::CliInvalidTarget));
                }
                return Err(error);
            }
        }
    }

    // JSON: one envelope for a single spec (the unchanged single-spec contract),
    // a JSON array of results for multiple specs. Text: one line per result.
    if output.is_json() {
        return match results.as_slice() {
            [single] => emit_json_data("rev-parse", single, output),
            many => emit_json_data("rev-parse", &many.to_vec(), output),
        };
    }
    if output.quiet {
        return Ok(());
    }
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    for result in &results {
        if result.mode == "symbolic_full_name" && result.value.is_empty() {
            // `--symbolic-full-name` of a valid object that is not a ref prints
            // nothing at all (not even a blank line), matching Git.
            continue;
        }
        // `--sq` shell-quotes the resolved object name (the `resolve`/`short`
        // modes), matching Git; the repository-query modes are left verbatim.
        let value = if args.sq && matches!(result.mode, "resolve" | "short") {
            sq_quote(&result.value)
        } else {
            result.value.clone()
        };
        write_rev_parse_output(&mut writer, &value)?;
    }
    // After the resolved revisions, Git prints the `--` separator and each path
    // after it verbatim (so an argv can be reconstructed). The single-revision
    // modes (`--verify`/`--short`) are the exception: they print only the single
    // object, never the paths.
    if saw_dashdash && !single_revision {
        write_rev_parse_output(&mut writer, "--")?;
        for p in &post_paths {
            write_rev_parse_output(&mut writer, p)?;
        }
    }
    Ok(())
}

/// Handle the output-filter modes. Each positional argument is classified as a
/// flag (begins with `-`, before any `--` terminator), a revision (resolves to a
/// commit-ish object), or a path (anything else). What prints is then gated by
/// the active flags: a revision prints (as its object name) unless `--no-revs`;
/// a flag prints verbatim unless `--no-flags`/`--revs-only`; a path prints
/// verbatim unless `--flags`/`--revs-only`. Matches `git rev-parse`.
async fn run_rev_parse_filter(args: &RevParseArgs, output: &OutputConfig) -> CliResult<()> {
    let print_rev = !args.no_revs;
    let print_flag = !args.no_flags && !args.revs_only;
    let print_path = !args.flags && !args.revs_only;

    // Reconstruct the logical argument sequence as typed. `split_positionals`
    // normalises clap's three `--` routings into a pre-`--` revision segment, the
    // post-`--` paths, and whether a separator was present. With no revision arg
    // before the separator, `--default <arg>` supplies it (applies even with a
    // leading `--`). The `--` is restored so a single classification loop with an
    // `after_dashdash` toggle handles every case.
    let (pre, post, saw_dashdash) = split_positionals(args);
    let mut seq: Vec<String> = if pre.is_empty() {
        args.default.clone().into_iter().collect()
    } else {
        pre
    };
    if saw_dashdash {
        seq.push("--".to_string());
        seq.extend(post);
    }

    // Collect the filtered output tokens first, so `--json` can emit them as one
    // array (and `--quiet` simply drops them) rather than streaming text.
    let mut tokens: Vec<String> = Vec::new();
    let mut after_dashdash = false;
    for arg in &seq {
        // The first `--` terminates flag/revision detection: everything after it
        // is a path. Git emits the `--` itself when path output is enabled (so an
        // argv can be reconstructed).
        if !after_dashdash && arg == "--" {
            after_dashdash = true;
            if print_path {
                tokens.push("--".to_string());
            }
            continue;
        }
        // A leading `-` (other than a lone `-`), before `--`, is a flag.
        if !after_dashdash && arg.len() > 1 && arg.starts_with('-') {
            if print_flag {
                tokens.push(arg.clone());
            }
            continue;
        }
        // Otherwise it is a revision if it resolves to a commit-ish object, else a
        // path. Args after `--` are always paths (never re-resolved, so a real
        // branch name after `--` stays a path). A genuine read/corruption failure
        // is NOT silently reclassified as a path — it propagates.
        let resolved = if after_dashdash {
            None
        } else {
            match util::get_commit_base_typed(arg).await {
                Ok(commit) => Some(commit),
                // Expected "not a revision" → treat as a path.
                Err(CommitBaseError::InvalidReference(_) | CommitBaseError::HeadUnborn) => None,
                // Real I/O / corruption failures surface with their own code.
                Err(CommitBaseError::ReadFailure(detail)) => {
                    return Err(
                        CliError::fatal(format!("failed to resolve '{arg}': {detail}"))
                            .with_stable_code(StableErrorCode::IoReadFailed),
                    );
                }
                Err(CommitBaseError::CorruptReference(detail)) => {
                    return Err(
                        CliError::fatal(format!("failed to resolve '{arg}': {detail}"))
                            .with_stable_code(StableErrorCode::RepoCorrupt),
                    );
                }
            }
        };
        match resolved {
            Some(commit) => {
                if print_rev {
                    tokens.push(commit.to_string());
                }
            }
            None => {
                if print_path {
                    tokens.push(arg.clone());
                }
            }
        }
    }

    if output.is_json() {
        return emit_json_data("rev-parse", &tokens, output);
    }
    if output.quiet {
        return Ok(());
    }
    let stdout = std::io::stdout();
    let mut writer = stdout.lock();
    for token in &tokens {
        write_rev_parse_output(&mut writer, token)?;
    }
    Ok(())
}

/// Single-quote a value for safe shell consumption (Git's `--sq`): wrap the
/// whole value in single quotes and escape any embedded single quote as
/// `'\''`. Applied unconditionally (Git quotes even values with no special
/// characters).
fn sq_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn write_rev_parse_output<W: Write>(writer: &mut W, value: &str) -> CliResult<()> {
    match writeln!(writer, "{value}") {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(
            CliError::fatal(format!("failed to write rev-parse output: {error}"))
                .with_stable_code(StableErrorCode::IoWriteFailed),
        ),
    }
}

async fn resolve_rev_parse(spec: &str, args: &RevParseArgs) -> CliResult<RevParseOutput> {
    if args.show_toplevel {
        let workdir = resolve_show_toplevel_path().await?;
        return Ok(RevParseOutput {
            mode: "show_toplevel",
            input: None,
            value: util::path_to_string(&workdir),
        });
    }

    if args.is_inside_work_tree {
        // A non-bare Libra repository always has a working tree we operate inside.
        let inside = !is_bare_repository().await?;
        return Ok(RevParseOutput {
            mode: "is_inside_work_tree",
            input: None,
            value: inside.to_string(),
        });
    }

    if args.is_inside_git_dir {
        // "true" when the current directory is inside `.libra` (Libra's
        // equivalent of Git's GIT_DIR), "false" anywhere else in the worktree.
        let storage = util::try_get_storage_path(None).map_err(map_repo_path_error)?;
        let cwd = util::cur_dir();
        return Ok(RevParseOutput {
            mode: "is_inside_git_dir",
            input: None,
            value: util::is_sub_path(&cwd, &storage).to_string(),
        });
    }

    if args.is_bare_repository {
        return Ok(RevParseOutput {
            mode: "is_bare_repository",
            input: None,
            value: is_bare_repository().await?.to_string(),
        });
    }

    if args.is_shallow_repository {
        return Ok(RevParseOutput {
            mode: "is_shallow_repository",
            input: None,
            value: is_shallow_repository()?.to_string(),
        });
    }

    if args.git_dir {
        let dir = util::try_get_storage_path(None).map_err(map_repo_path_error)?;
        return Ok(RevParseOutput {
            mode: "git_dir",
            input: None,
            value: util::path_to_string(&dir),
        });
    }

    if args.absolute_git_dir {
        let dir = util::try_get_storage_path(None).map_err(map_repo_path_error)?;
        // `--git-dir` already yields an absolute path in Libra; canonicalize to
        // guarantee Git's "canonicalized absolute path" contract, falling back
        // to the resolved path if canonicalization fails.
        let abs = std::fs::canonicalize(&dir).unwrap_or(dir);
        return Ok(RevParseOutput {
            mode: "absolute_git_dir",
            input: None,
            value: util::path_to_string(&abs),
        });
    }

    if args.show_prefix {
        let storage = util::try_get_storage_path(None).map_err(map_repo_path_error)?;
        let cwd = util::cur_dir();
        let prefix = cwd
            .strip_prefix(storage.parent().unwrap_or(&storage))
            .unwrap_or(&cwd);
        let value = if prefix.as_os_str().is_empty() {
            String::new()
        } else {
            format!("{}/", prefix.display())
        };
        return Ok(RevParseOutput {
            mode: "show_prefix",
            input: None,
            value,
        });
    }

    if args.show_cdup {
        let storage = util::try_get_storage_path(None).map_err(map_repo_path_error)?;
        let worktree_root = storage.parent().unwrap_or(&storage);
        let cwd = util::cur_dir();
        let value = if cwd == *worktree_root {
            String::new()
        } else {
            let rel = cwd.strip_prefix(worktree_root).unwrap_or(&cwd);
            let depth = rel.components().count();
            "../".repeat(depth)
        };
        return Ok(RevParseOutput {
            mode: "show_cdup",
            input: None,
            value,
        });
    }

    // The revision to resolve is supplied by the caller (one per positional SPEC,
    // defaulting to HEAD / `--default`).
    if args.abbrev_ref {
        let value = resolve_abbrev_ref(spec).await?;
        return Ok(RevParseOutput {
            mode: "abbrev_ref",
            input: Some(spec.to_string()),
            value,
        });
    }

    if args.symbolic_full_name {
        let value = resolve_symbolic_full_name(spec).await?;
        return Ok(RevParseOutput {
            mode: "symbolic_full_name",
            input: Some(spec.to_string()),
            value,
        });
    }

    if args.symbolic {
        let value = resolve_symbolic(spec).await?;
        return Ok(RevParseOutput {
            mode: "symbolic",
            input: Some(spec.to_string()),
            value,
        });
    }

    let commit = util::get_commit_base_typed(spec)
        .await
        .map_err(|err| rev_parse_target_error(spec, err))?;
    let value = if let Some(short_len) = &args.short {
        let requested_len: usize = short_len.parse().map_err(|_| {
            CliError::command_usage(format!("invalid --short length: '{short_len}'"))
                .with_stable_code(StableErrorCode::CliInvalidArguments)
        })?;
        resolve_short_commit(&commit, Some(requested_len)).await?
    } else {
        commit.to_string()
    };

    Ok(RevParseOutput {
        mode: if args.short.is_some() {
            "short"
        } else {
            "resolve"
        },
        input: Some(spec.to_string()),
        value,
    })
}

async fn resolve_abbrev_ref(spec: &str) -> CliResult<String> {
    if spec == "HEAD" {
        return match Head::current_result().await {
            Ok(Head::Branch(name)) => Ok(name),
            Ok(Head::Detached(_)) => Ok("HEAD".to_string()),
            Err(error) => Err(map_head_resolution_error(error)),
        };
    }

    if let Some(branch_name) = spec.strip_prefix("refs/heads/")
        && let Some(branch) = Branch::find_branch_result(branch_name, None)
            .await
            .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
    {
        return Ok(branch.name);
    }

    if let Some(short_name) = spec.strip_prefix("refs/remotes/")
        && resolve_remote_tracking_ref(spec, short_name).await?
    {
        return Ok(short_name.to_string());
    }

    if let Some(branch) = Branch::find_branch_result(spec, None)
        .await
        .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
    {
        return Ok(branch.name);
    }

    if resolve_remote_tracking_ref(spec, spec).await? {
        return Ok(spec.to_string());
    }

    Err(CliError::failure(format!("not a symbolic ref: '{spec}'"))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("use 'libra rev-parse <rev>' to resolve it to a commit hash."))
}

/// Resolve `spec` for `--symbolic`: print it in a form as close to the original
/// input as possible. Git echoes any spec it can parse — a ref, a revision
/// expression, or a (possibly abbreviated) object id — verbatim, and keeps SHAs
/// as SHAs. We gate validity through the same resolver `--symbolic-full-name`
/// uses (so an unresolvable name fails with exit 128, and a valid non-ref object
/// is still accepted), then echo the spec verbatim rather than expanding it to a
/// full ref name.
///
/// Intentional divergence from Git (shared with `--symbolic-full-name`): an
/// unresolvable spec fails on stderr with exit 128 instead of being echoed to
/// stdout.
async fn resolve_symbolic(spec: &str) -> CliResult<String> {
    // Validity gate only — the returned full-ref / empty value is discarded; an
    // unresolvable spec propagates its fatal exit-128 error.
    resolve_symbolic_full_name(spec).await?;
    Ok(spec.to_string())
}

async fn resolve_remote_tracking_ref(spec: &str, short_name: &str) -> CliResult<bool> {
    for (remote, branch_name) in util::remote_tracking_candidates(short_name) {
        let full_ref = format!("refs/remotes/{remote}/{branch_name}");

        if Branch::find_branch_result(&full_ref, Some(remote))
            .await
            .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
            .is_some()
        {
            return Ok(true);
        }

        if Branch::find_branch_result(branch_name, Some(remote))
            .await
            .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
            .is_some()
        {
            return Ok(true);
        }
    }

    Ok(false)
}

/// Resolve `spec` to its full ref name for `--symbolic-full-name`, in Git's
/// precedence (branch → tag → remote-tracking). `HEAD` yields the branch it points
/// to (or `"HEAD"` when detached). A valid object that is not a ref yields an empty
/// string (Git prints nothing, exit 0); a name that is neither a ref nor a valid
/// object is an unresolvable spec (fatal, exit 128).
async fn resolve_symbolic_full_name(spec: &str) -> CliResult<String> {
    if spec == "HEAD" {
        return match Head::current_result().await {
            Ok(Head::Branch(name)) => Ok(format!("refs/heads/{name}")),
            Ok(Head::Detached(_)) => Ok("HEAD".to_string()),
            Err(error) => Err(map_head_resolution_error(error)),
        };
    }

    // A fully-qualified ref that exists is returned verbatim.
    if let Some(branch_name) = spec.strip_prefix("refs/heads/")
        && Branch::find_branch_result(branch_name, None)
            .await
            .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
            .is_some()
    {
        return Ok(spec.to_string());
    }
    if let Some(tag_name) = spec.strip_prefix("refs/tags/")
        && find_tag_ref_named(spec, tag_name).await?
    {
        return Ok(spec.to_string());
    }
    if let Some(short_name) = spec.strip_prefix("refs/remotes/")
        && let Some(full) = resolve_remote_tracking_full(spec, short_name).await?
    {
        return Ok(full);
    }

    // Short forms, in Git's precedence: branch, then tag, then remote-tracking.
    if let Some(branch) = Branch::find_branch_result(spec, None)
        .await
        .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
    {
        return Ok(format!("refs/heads/{}", branch.name));
    }
    if find_tag_ref_named(spec, spec).await? {
        return Ok(format!("refs/tags/{spec}"));
    }
    if let Some(full) = resolve_remote_tracking_full(spec, spec).await? {
        return Ok(full);
    }

    // Not a ref. A valid object prints nothing (exit 0); a genuine read/corruption
    // failure propagates with its own code; anything else is an unresolvable spec
    // (fatal, exit 128). First try a commit-ish revision expression (HEAD~3, main^,
    // a commit id), then fall back to an object id of any type (tree/blob/tag).
    match util::get_commit_base_typed(spec).await {
        Ok(_) => return Ok(String::new()),
        Err(CommitBaseError::ReadFailure(detail)) => {
            return Err(
                CliError::fatal(format!("failed to resolve '{spec}': {detail}"))
                    .with_stable_code(StableErrorCode::IoReadFailed),
            );
        }
        Err(CommitBaseError::CorruptReference(detail)) => {
            return Err(
                CliError::fatal(format!("failed to resolve '{spec}': {detail}"))
                    .with_stable_code(StableErrorCode::RepoCorrupt),
            );
        }
        // Not commit-ish: fall through to the any-type object-id check.
        Err(CommitBaseError::HeadUnborn | CommitBaseError::InvalidReference(_)) => {}
    }

    // A valid object of ANY type, addressed by a BARE object id (full or
    // abbreviated hex), prints nothing (exit 0) — matching Git for
    // `--symbolic-full-name <tree-or-blob-sha>`. We restrict the fallback to bare
    // hex ids so a malformed revision expression the strict parser already rejected
    // (e.g. `HEAD^garbage`, `HEAD^{tree}`) is NOT permissively re-resolved by
    // `search_result`'s parent-navigation path — it stays unresolvable (exit 128).
    if is_bare_object_id(spec) {
        let storage = ClientStorage::init(path::objects());
        match storage.search_result(spec).await {
            Ok(matches) if matches.len() == 1 => return Ok(String::new()),
            Ok(_) => return Err(unresolvable_symbolic_spec(spec)),
            Err(error) => {
                return Err(
                    CliError::fatal(format!("failed to resolve '{spec}': {error}"))
                        .with_stable_code(StableErrorCode::IoReadFailed),
                );
            }
        }
    }

    Err(unresolvable_symbolic_spec(spec))
}

/// Whether `spec` is a bare object-id prefix (4..=64 hex chars, no revision
/// operators) — the only non-ref form that may name a raw tree/blob/commit object.
fn is_bare_object_id(spec: &str) -> bool {
    (4..=64).contains(&spec.len()) && spec.bytes().all(|b| b.is_ascii_hexdigit())
}

/// Fatal error (exit 128) for a `--symbolic-full-name` spec that is neither a ref
/// nor a valid object — Git's "ambiguous argument" diagnostic. Libra reports on
/// stderr rather than echoing the spec to stdout.
fn unresolvable_symbolic_spec(spec: &str) -> CliError {
    CliError::fatal(format!(
        "ambiguous argument '{spec}': unknown revision or path not in the working tree"
    ))
    .with_exit_code(128)
    .with_stable_code(StableErrorCode::CliInvalidTarget)
}

/// Whether `tag_name` names an existing tag ref (mapping the lookup error to a
/// CLI failure tagged with `spec`).
async fn find_tag_ref_named(spec: &str, tag_name: &str) -> CliResult<bool> {
    tag::find_tag_ref(tag_name)
        .await
        .map(|found| found.is_some())
        .map_err(|error| {
            // A tag lookup fails on a database/query error, i.e. a storage read
            // failure — not an internal invariant.
            CliError::fatal(format!("failed to look up tag '{spec}': {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        })
}

/// The full `refs/remotes/<remote>/<branch>` name for a remote-tracking spec, or
/// `None` when no candidate exists.
async fn resolve_remote_tracking_full(spec: &str, short_name: &str) -> CliResult<Option<String>> {
    for (remote, branch_name) in util::remote_tracking_candidates(short_name) {
        let full_ref = format!("refs/remotes/{remote}/{branch_name}");
        let exists = Branch::find_branch_result(&full_ref, Some(remote))
            .await
            .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
            .is_some()
            || Branch::find_branch_result(branch_name, Some(remote))
                .await
                .map_err(|error| map_symbolic_ref_resolution_error(spec, error))?
                .is_some();
        if exists {
            return Ok(Some(full_ref));
        }
    }
    Ok(None)
}

async fn resolve_short_commit(
    commit: &ObjectHash,
    requested_len: Option<usize>,
) -> CliResult<String> {
    let full = commit.to_string();
    let storage = util::objects_storage();

    let min_len = requested_len.unwrap_or(SHORT_HASH_LEN).max(1);

    for len in min_len..=full.len() {
        let prefix = &full[..len];
        let matches = storage.search_result(prefix).await.map_err(|error| {
            CliError::fatal(format!(
                "failed to search objects while abbreviating '{full}': {error}"
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;

        if matches.len() == 1 && matches[0] == *commit {
            return Ok(prefix.to_string());
        }
    }

    Ok(full)
}

async fn is_bare_repository() -> CliResult<bool> {
    fn parse_git_bool(value: &str) -> Option<bool> {
        match value.trim() {
            v if v.eq_ignore_ascii_case("true")
                || v.eq_ignore_ascii_case("yes")
                || v.eq_ignore_ascii_case("on")
                || v == "1" =>
            {
                Some(true)
            }
            v if v.eq_ignore_ascii_case("false")
                || v.eq_ignore_ascii_case("no")
                || v.eq_ignore_ascii_case("off")
                || v == "0" =>
            {
                Some(false)
            }
            _ => None,
        }
    }

    match ConfigKv::get("core.bare").await {
        Ok(Some(entry)) => parse_git_bool(&entry.value).ok_or_else(|| {
            CliError::fatal(format!(
                "Invalid core.bare value: '{}'. Expected true/false/yes/no/on/off/1/0",
                entry.value
            ))
            .with_stable_code(StableErrorCode::RepoCorrupt)
        }),
        Ok(None) => Ok(false),
        Err(error) => Err(
            CliError::fatal(format!("Failed to read core.bare config: {error}"))
                .with_stable_code(StableErrorCode::IoReadFailed),
        ),
    }
}

fn is_shallow_repository() -> CliResult<bool> {
    let storage = util::try_get_storage_path(None).map_err(map_repo_path_error)?;
    let shallow = storage.join("shallow");
    match fs::read_to_string(&shallow) {
        Ok(contents) => Ok(contents.lines().any(|line| !line.trim().is_empty())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(CliError::fatal(format!(
            "failed to read shallow metadata '{}': {error}",
            shallow.display()
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
        .with_hint("check repository metadata permissions and retry")),
    }
}

async fn resolve_show_toplevel_path() -> CliResult<PathBuf> {
    let workdir = util::try_working_dir().map_err(map_repo_path_error)?;
    let storage = util::try_get_storage_path(None).map_err(map_repo_path_error)?;
    if workdir == storage {
        if is_bare_repository().await? {
            return Err(CliError::fatal("this operation must be run in a work tree")
                .with_stable_code(StableErrorCode::RepoStateInvalid));
        }

        let storage = fs::canonicalize(&storage).map_err(|error| {
            CliError::io(format!(
                "failed to resolve repository storage path '{}': {error}",
                storage.display()
            ))
            .with_stable_code(StableErrorCode::IoReadFailed)
        })?;

        return storage
            .parent()
            .map(PathBuf::from)
            .ok_or_else(CliError::repo_not_found);
    }
    Ok(workdir)
}

fn map_repo_path_error(err: std::io::Error) -> CliError {
    match err.kind() {
        std::io::ErrorKind::NotFound => CliError::repo_not_found(),
        _ => CliError::io(format!("failed to determine repository root: {err}"))
            .with_stable_code(StableErrorCode::IoReadFailed),
    }
}

fn map_head_resolution_error(error: BranchStoreError) -> CliError {
    map_symbolic_ref_resolution_error("HEAD", error)
}

fn map_symbolic_ref_resolution_error(spec: &str, error: BranchStoreError) -> CliError {
    match error {
        BranchStoreError::Corrupt { detail, .. } => {
            CliError::fatal(format!("failed to resolve symbolic ref '{spec}': {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        }
        BranchStoreError::Query(detail)
        | BranchStoreError::NotFound(detail)
        | BranchStoreError::Delete { detail, .. } => {
            CliError::fatal(format!("failed to resolve symbolic ref '{spec}': {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
    }
}

fn rev_parse_target_error(spec: &str, error: CommitBaseError) -> CliError {
    match error {
        CommitBaseError::HeadUnborn => CliError::failure(format!(
            "not a valid object name: '{spec}' (HEAD does not point to a commit)"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("create a commit before resolving HEAD."),
        CommitBaseError::InvalidReference(detail) => {
            CliError::failure(format!("not a valid object name: '{spec}' ({detail})"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        }
        CommitBaseError::ReadFailure(detail) => {
            CliError::fatal(format!("failed to resolve '{spec}': {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        CommitBaseError::CorruptReference(detail) => {
            CliError::fatal(format!("failed to resolve '{spec}': {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::{self, Write};

    use clap::Parser;

    use super::{RevParseArgs, write_rev_parse_output};
    use crate::utils::error::StableErrorCode;

    struct FailingWriter {
        kind: io::ErrorKind,
    }

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(self.kind, "test write failure"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn test_rev_parse_args_default() {
        let args = RevParseArgs::try_parse_from(["rev-parse"]).unwrap();
        assert!(args.short.is_none());
        assert!(!args.abbrev_ref);
        assert!(!args.show_toplevel);
        assert!(args.spec.is_empty());
    }

    #[test]
    fn test_rev_parse_args_short_head() {
        let args = RevParseArgs::try_parse_from(["rev-parse", "--short", "HEAD"]).unwrap();
        // `--short` without `=<n>` takes its default_missing_value ("7"); "HEAD"
        // is consumed as the positional spec.
        assert_eq!(args.short.as_deref(), Some("7"));
        assert_eq!(args.spec, vec!["HEAD".to_string()]);
    }

    #[test]
    fn test_rev_parse_args_abbrev_ref() {
        let args = RevParseArgs::try_parse_from(["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        assert!(args.abbrev_ref);
        assert_eq!(args.spec, vec!["HEAD".to_string()]);
    }

    #[test]
    fn test_rev_parse_args_show_toplevel() {
        let args = RevParseArgs::try_parse_from(["rev-parse", "--show-toplevel"]).unwrap();
        assert!(args.show_toplevel);
    }

    #[test]
    fn test_rev_parse_args_verify() {
        let args = RevParseArgs::try_parse_from(["rev-parse", "--verify", "HEAD"]).unwrap();
        assert!(args.verify);
        assert_eq!(args.spec, vec!["HEAD".to_string()]);
    }

    #[test]
    fn test_rev_parse_args_default_revision() {
        let args = RevParseArgs::try_parse_from(["rev-parse", "--default", "main"]).unwrap();
        assert_eq!(args.default.as_deref(), Some("main"));
        assert!(args.spec.is_empty());
    }

    #[test]
    fn test_rev_parse_args_is_inside_work_tree() {
        let args = RevParseArgs::try_parse_from(["rev-parse", "--is-inside-work-tree"]).unwrap();
        assert!(args.is_inside_work_tree);
    }

    #[test]
    fn test_rev_parse_args_repo_query_modes() {
        let bare = RevParseArgs::try_parse_from(["rev-parse", "--is-bare-repository"]).unwrap();
        assert!(bare.is_bare_repository);
        let git_dir = RevParseArgs::try_parse_from(["rev-parse", "--git-dir"]).unwrap();
        assert!(git_dir.git_dir);
        let inside_git_dir =
            RevParseArgs::try_parse_from(["rev-parse", "--is-inside-git-dir"]).unwrap();
        assert!(inside_git_dir.is_inside_git_dir);
    }

    #[test]
    fn test_rev_parse_args_is_inside_git_dir_conflicts_with_spec() {
        let err = RevParseArgs::try_parse_from(["rev-parse", "--is-inside-git-dir", "HEAD"])
            .expect_err("--is-inside-git-dir should reject SPEC");
        let rendered = err.to_string();
        assert!(
            rendered.contains("cannot be used with") || rendered.contains("unexpected argument"),
            "unexpected clap error: {rendered}"
        );
    }

    #[test]
    fn test_rev_parse_args_is_inside_git_dir_conflicts_with_other_modes() {
        // Like the sibling query flags, `--is-inside-git-dir` must be rejected
        // (not silently ignored) when combined with --verify or --default.
        for combo in [
            vec!["rev-parse", "--verify", "--is-inside-git-dir"],
            vec!["rev-parse", "--default", "HEAD", "--is-inside-git-dir"],
            vec!["rev-parse", "--is-inside-git-dir", "--git-dir"],
        ] {
            let err = RevParseArgs::try_parse_from(combo.clone())
                .expect_err(&format!("{combo:?} should be rejected"));
            assert!(
                err.to_string().contains("cannot be used with"),
                "expected a conflict error for {combo:?}, got: {err}"
            );
        }
    }

    #[test]
    fn test_rev_parse_args_absolute_git_dir_conflicts_mirror_git_dir() {
        // `--absolute-git-dir` must share `--git-dir`'s conflict set so invalid
        // mode combinations are rejected rather than silently bypassed.
        for combo in [
            vec!["rev-parse", "--verify", "--absolute-git-dir"],
            vec!["rev-parse", "--default", "HEAD", "--absolute-git-dir"],
            vec!["rev-parse", "--is-inside-work-tree", "--absolute-git-dir"],
            vec!["rev-parse", "--is-inside-git-dir", "--absolute-git-dir"],
            vec!["rev-parse", "--is-bare-repository", "--absolute-git-dir"],
            vec!["rev-parse", "--git-dir", "--absolute-git-dir"],
            vec!["rev-parse", "--short", "--absolute-git-dir"],
            vec!["rev-parse", "--abbrev-ref", "--absolute-git-dir"],
            vec!["rev-parse", "--show-toplevel", "--absolute-git-dir"],
        ] {
            let err = RevParseArgs::try_parse_from(combo.clone())
                .expect_err(&format!("{combo:?} should be rejected"));
            assert!(
                err.to_string().contains("cannot be used with"),
                "expected a conflict error for {combo:?}, got: {err}"
            );
        }
    }

    #[test]
    fn test_rev_parse_args_is_inside_work_tree_conflicts_with_spec() {
        let err = RevParseArgs::try_parse_from(["rev-parse", "--is-inside-work-tree", "HEAD"])
            .expect_err("--is-inside-work-tree should reject SPEC");
        let rendered = err.to_string();
        assert!(
            rendered.contains("cannot be used with") || rendered.contains("unexpected argument"),
            "unexpected clap error: {rendered}"
        );
    }

    #[test]
    fn test_rev_parse_args_show_toplevel_conflicts_with_spec() {
        let err = RevParseArgs::try_parse_from(["rev-parse", "--show-toplevel", "HEAD"])
            .expect_err("--show-toplevel should reject SPEC");
        let rendered = err.to_string();
        assert!(
            rendered.contains("cannot be used with") || rendered.contains("unexpected argument"),
            "unexpected clap error: {rendered}"
        );
    }

    #[test]
    fn test_rev_parse_args_abbrev_ref_conflicts_with_short() {
        let err = RevParseArgs::try_parse_from(["rev-parse", "--abbrev-ref", "--short", "HEAD"])
            .expect_err("--abbrev-ref should reject --short");
        let rendered = err.to_string();
        assert!(
            rendered.contains("cannot be used with"),
            "unexpected clap error: {rendered}"
        );
    }

    #[test]
    fn test_rev_parse_args_symbolic_full_name_conflicts_with_query_modes() {
        // `--symbolic-full-name` must not silently coexist with the repository-query
        // modes (evaluated first) or the other revision-output modes.
        for other in [
            "--git-dir",
            "--show-prefix",
            "--show-cdup",
            "--is-inside-work-tree",
            "--is-inside-git-dir",
            "--is-bare-repository",
            "--absolute-git-dir",
            "--show-toplevel",
            "--abbrev-ref",
        ] {
            let err = RevParseArgs::try_parse_from(["rev-parse", "--symbolic-full-name", other])
                .expect_err(&format!("--symbolic-full-name should reject {other}"));
            assert!(
                err.to_string().contains("cannot be used with"),
                "unexpected clap error for {other}: {err}"
            );
        }
    }

    #[test]
    fn test_rev_parse_args_show_toplevel_conflicts_with_short() {
        let err = RevParseArgs::try_parse_from(["rev-parse", "--show-toplevel", "--short"])
            .expect_err("--show-toplevel should reject --short");
        let rendered = err.to_string();
        assert!(
            rendered.contains("cannot be used with"),
            "unexpected clap error: {rendered}"
        );
    }

    #[test]
    fn test_write_rev_parse_output_maps_write_failure_to_write_code() {
        let mut writer = FailingWriter {
            kind: io::ErrorKind::PermissionDenied,
        };

        let error = write_rev_parse_output(&mut writer, "abc123").expect_err("write should fail");

        assert_eq!(error.stable_code(), StableErrorCode::IoWriteFailed);
    }

    #[test]
    fn test_write_rev_parse_output_ignores_broken_pipe() {
        let mut writer = FailingWriter {
            kind: io::ErrorKind::BrokenPipe,
        };

        write_rev_parse_output(&mut writer, "abc123").expect("broken pipe should be ignored");
    }
}
