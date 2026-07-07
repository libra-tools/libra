//! CLI entry for Libra.
//!
//! Defines the clap subcommand grammar, performs cross-cutting preflight (locating the
//! repository database and pinning the global hash algorithm to whatever is recorded
//! in `core.objectformat`), and dispatches every parsed command to its `command::*`
//! handler.
//!
//! Because every subcommand reads or writes objects whose hash kind must match the
//! repository's recorded `core.objectformat`, this module is the single point where
//! that global is configured before any handler runs.

use std::{env, io::Write, path::Path};

use clap::{
    CommandFactory, Parser, Subcommand,
    error::{ContextKind, ContextValue, ErrorKind},
};
use git_internal::hash::{HashKind, set_hash_kind};
use sea_orm::{ConnectionTrait, Statement};

use crate::{
    command,
    internal::{config::ConfigKv, db},
    utils,
    utils::{
        error::{CliError, CliResult},
        output::OutputConfig,
    },
};

// The `media` command (lore.md §6) is a cfg-gated `Commands` variant, so its
// Command-Groups entry must appear ONLY under `--features fastcdc` — otherwise
// the default-features compat matrix (which cross-checks every listed command
// against the real CLI surface) would see a command that does not exist. A
// cfg-selected macro fragment splices it into the Working Tree row.
#[cfg(feature = "fastcdc")]
macro_rules! media_group_entry {
    () => {
        ", media"
    };
}
#[cfg(not(feature = "fastcdc"))]
macro_rules! media_group_entry {
    () => {
        ""
    };
}

const ROOT_AFTER_HELP: &str = concat!(
    "\
Command Groups:
  Repository Setup        init, clone, config, completions
  Working Tree            status, add, rm, mv, restore, clean, stash, dirty, layer, sparse-view, hydrate",
    media_group_entry!(),
    ", lfs, ls-files, check-ignore, check-attr, check-mailmap, worktree
  History Inspection      log, shortlog, show, show-ref, format-patch, ls-remote, ls-tree, diff, grep, blame, describe, notes, archive, revision
  Commit And Branching    commit, branch, switch, checkout, tag, merge, rebase, reset, cherry-pick, revert, rerere, metadata
  Remote And Cloud        remote, fetch, pull, push, open, cloud, cache, publish, credential, bundle, auth, login, logout, whoami
  AI And Automation       code, code-control, automation, usage, graph, sandbox, agent, review, investigate, service
  Maintenance And Plumbing fsck, maintenance, repack, logfile, cat-file, hash-object, write-tree, read-tree, update-index, update-ref, merge-file, merge-base, apply, diff-tree, diff-index, diff-files, fast-export, fast-import, replace, verify-pack, rev-parse, rev-list, symbolic-ref, reflog, bisect, for-each-ref, commit-tree, file, alternates, deps

Help Topics:
  error-codes  Print the stable CLI error code table (`libra help error-codes`)

Output Examples:
  libra --json status                  Pretty JSON envelope on stdout
  libra --json=ndjson log              One-line-per-event newline-delimited JSON
  libra --machine status               Compact JSON; suppresses progress/decoration
  libra --quiet --exit-code-on-warning Silent run; non-zero exit (9) if warnings occurred
  libra --color=never log              Force-disable colors (also via NO_COLOR=1)

For per-command flags, see `libra <cmd> --help`.
"
);

const ERROR_CODES_HELP: &str = include_str!("../docs/error-codes.md");

/// Read the repository's `core.objectformat` and pin the global hash algorithm.
///
/// Functional scope:
/// - Opens the SQLite database at `<storage>/<DATABASE>` and reads
///   `core.objectformat`, defaulting to `"sha1"` when the row is absent.
/// - Calls `git_internal::hash::set_hash_kind` so every object hashed by the rest of
///   the process matches the repository's storage format.
///
/// Boundary conditions:
/// - Returns a fatal error when the database file is missing — every non-`init`,
///   non-`clone` command requires a repository, and silently continuing would hash
///   objects with the wrong algorithm.
/// - Returns a fatal error when the database cannot be opened (permissions, disk
///   corruption) so the user sees the underlying message instead of a downstream
///   panic.
/// - Currently accepts only `"sha1"` and `"sha256"`; anything else is rejected with a
///   fatal error.
async fn set_local_hash_kind_for_storage(storage: &Path) -> CliResult<()> {
    let db_path = storage.join(utils::util::DATABASE);
    if !db_path.exists() {
        return Err(CliError::fatal(format!(
            "repository database not found at '{}'",
            db_path.display()
        )));
    }

    let db_conn = db::get_db_conn_instance_for_path(&db_path)
        .await
        .map_err(|e| {
            CliError::fatal(format!(
                "failed to open repository database '{}': {}",
                db_path.display(),
                e
            ))
        })?;
    let object_format = ConfigKv::get_with_conn(&db_conn, "core.objectformat")
        .await
        .map_err(|e| {
            CliError::fatal(format!(
                "failed to read core.objectformat from repository database '{}': {}",
                db_path.display(),
                e
            ))
        })?
        .map(|e| e.value)
        .unwrap_or_else(|| "sha1".to_string());

    set_hash_kind_from_object_format(object_format)
}

async fn set_local_hash_kind_for_storage_without_schema_guard(storage: &Path) -> CliResult<()> {
    let db_path = storage.join(utils::util::DATABASE);
    if !db_path.exists() {
        return Err(CliError::fatal(format!(
            "repository database not found at '{}'",
            db_path.display()
        )));
    }

    let db_conn = db::open_database_without_migrations(&db_path)
        .await
        .map_err(|e| {
            CliError::fatal(format!(
                "failed to open repository database '{}': {}",
                db_path.display(),
                e
            ))
        })?;
    let object_format = read_schema_free_object_format(&db_conn, &db_path).await?;

    set_hash_kind_from_object_format(object_format)
}

async fn read_schema_free_object_format(
    db_conn: &sea_orm::DatabaseConnection,
    db_path: &Path,
) -> CliResult<String> {
    let has_config_kv = db_conn
        .query_one(Statement::from_sql_and_values(
            db_conn.get_database_backend(),
            "SELECT 1 FROM sqlite_master WHERE type = ? AND name = ? LIMIT 1",
            ["table".into(), "config_kv".into()],
        ))
        .await
        .map_err(|e| {
            CliError::fatal(format!(
                "failed to inspect repository database '{}': {}",
                db_path.display(),
                e
            ))
        })?
        .is_some();

    if !has_config_kv {
        return Ok("sha1".to_string());
    }

    let row = db_conn
        .query_one(Statement::from_sql_and_values(
            db_conn.get_database_backend(),
            "SELECT value FROM config_kv WHERE key = ? ORDER BY id DESC LIMIT 1",
            ["core.objectformat".into()],
        ))
        .await
        .map_err(|e| {
            CliError::fatal(format!(
                "failed to read core.objectformat from repository database '{}': {}",
                db_path.display(),
                e
            ))
        })?;

    match row {
        Some(row) => row.try_get_by_index(0).map_err(|e| {
            CliError::fatal(format!(
                "failed to decode core.objectformat from repository database '{}': {}",
                db_path.display(),
                e
            ))
        }),
        None => Ok("sha1".to_string()),
    }
}

fn set_hash_kind_from_object_format(object_format: String) -> CliResult<()> {
    let hash_kind = match object_format.as_str() {
        "sha1" => HashKind::Sha1,
        "sha256" => HashKind::Sha256,
        _ => {
            return Err(CliError::fatal(format!(
                "unsupported object format: '{object_format}'"
            )));
        }
    };
    set_hash_kind(hash_kind);
    Ok(())
}

// The Cli struct represents the root of the command line interface.
#[derive(Parser, Debug)]
#[command(
    about = "Libra: An AI native version control system for monorepo and trunk-based development.",
    version = env!("CARGO_PKG_VERSION"),
    after_help = ROOT_AFTER_HELP,
    arg_required_else_help = true,
)]
struct Cli {
    /// Emit machine-readable JSON to stdout.
    /// Use `--json` alone for pretty output, or `--json=compact` / `--json=ndjson`
    /// to select an alternative layout.  The `=` is required when specifying a format
    /// so that the subcommand name is not consumed as the value.
    #[arg(
        long,
        short = 'J',
        global = true,
        value_name = "FORMAT",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "pretty",
        value_parser = ["pretty", "compact", "ndjson"],
    )]
    json: Option<String>,

    /// Strict machine mode.
    /// Implies --json=ndjson --no-pager --color=never --quiet.
    /// Disables all prompts and decorative text.
    #[arg(long, global = true)]
    machine: bool,

    /// Disable automatic pager (less) for long output.
    #[arg(long, global = true)]
    no_pager: bool,

    /// When to use terminal colors.
    /// Also respects the NO_COLOR environment variable (see <https://no-color.org>).
    #[arg(
        long,
        global = true,
        value_name = "WHEN",
        default_value = "auto",
        value_parser = ["auto", "never", "always"],
    )]
    color: String,

    /// Disable terminal colors.
    /// Equivalent to --color=never and takes precedence over --color.
    #[arg(long, global = true)]
    no_color: bool,

    /// Suppress standard stdout output; keep warnings/errors on stderr.
    /// This includes primary command results, unlike some Git per-command
    /// `--quiet` flags that only suppress informational chatter.
    #[arg(long, short = 'q', global = true)]
    quiet: bool,

    /// Return non-zero exit code (exit 9) when a warning is emitted.
    #[arg(long, global = true)]
    exit_code_on_warning: bool,

    /// Control progress output for long-running operations.
    /// `json` emits NDJSON progress events; `text` shows a human-friendly bar;
    /// `none` suppresses progress entirely.
    #[arg(
        long,
        global = true,
        value_name = "MODE",
        default_value = "auto",
        value_parser = ["json", "text", "none", "auto"],
    )]
    progress: String,

    /// fsync object writes (and their parent directories) for power-loss
    /// durability, at the cost of write throughput. Recovery-critical sequencer
    /// state is always fsynced regardless of this flag. Also settable via
    /// `LIBRA_SYNC_DATA=1`.
    #[arg(long, global = true)]
    sync_data: bool,

    /// Read objects from the local store only; never fetch from the configured
    /// durable tier (a needed remote object becomes a clear error). This is
    /// Libra's spelling of Lore's `--offline`/`--local` read policy as a single
    /// collision-free global flag (a global `--local`/`--remote` would clash with
    /// `config`/`clone`/`agent` options). For the `remote`-refresh policy use
    /// `LIBRA_READ_POLICY=remote`. No-op for local-only repositories.
    #[arg(long, global = true)]
    offline: bool,

    /// Maximum number of concurrent remote connections/requests (bounds fan-out
    /// on large repos / CI so connections are not exhausted). A positive integer;
    /// `0` is treated as `1`. Also settable via `LIBRA_MAX_CONNECTIONS`
    /// (flag wins). Default 16. No-op for purely local operations.
    #[arg(long, global = true, value_name = "N")]
    max_connections: Option<usize>,

    #[command(subcommand)]
    command: Commands,
}

/// The Commands enum represents the subcommands that can be used with the CLI.
/// subcommand's execute and args are defined in `command` module
#[derive(Subcommand, Debug)]
enum Commands {
    // Each variant of the enum represents a subcommand.
    // The about attribute provides a brief description of the subcommand.
    // The arguments of the subcommand are defined in the command module.
    #[command(about = "Initialize a new repository")]
    Init(command::init::InitArgs),
    #[command(about = "Clone a repository into a new directory")]
    Clone(command::clone::CloneArgs),
    #[command(about = "Manage repository configurations", alias = "cfg")]
    Config(command::config::ConfigArgs),

    #[command(about = "Show the working tree status", alias = "st")]
    Status(command::status::StatusArgs),
    #[command(about = "Add file contents to the index")]
    Add(command::add::AddArgs),
    #[command(
        about = "Remove files from the working tree and from the index",
        alias = "remove",
        alias = "delete"
    )]
    Rm(command::remove::RemoveArgs),
    #[command(about = "Move or rename a file, a directory, or a symlink")]
    Mv(command::mv::MvArgs),
    #[command(about = "Restore working tree files", alias = "unstage")]
    Restore(command::restore::RestoreArgs),
    #[command(about = "Remove untracked files from the working tree")]
    Clean(command::clean::CleanArgs),
    #[command(
        subcommand,
        about = "Stash the changes in a dirty working directory away",
        after_help = command::stash::STASH_EXAMPLES
    )]
    Stash(Stash),
    #[command(
        subcommand,
        about = "Large File Storage",
        after_help = command::lfs::LFS_EXAMPLES
    )]
    Lfs(command::lfs::LfsCmds),
    #[command(
        about = "Show information about tracked and untracked files",
        after_help = command::ls_files::LS_FILES_EXAMPLES
    )]
    LsFiles(command::ls_files::LsFilesArgs),
    #[command(
        about = "Manage multiple working trees attached to this repository",
        alias = "wt",
        after_help = command::worktree::WORKTREE_EXAMPLES
    )]
    Worktree(command::worktree::WorktreeArgs),

    #[command(about = "Show commit logs", alias = "hist", alias = "history")]
    Log(command::log::LogArgs),
    #[command(
        about = "Inspect the tracing log-file configuration",
        after_help = command::logfile::LOGFILE_EXAMPLES
    )]
    Logfile(command::logfile::LogfileArgs),
    #[command(
        about = "Inspect the tiered-storage / LRU cache configuration",
        after_help = command::cache::CACHE_EXAMPLES
    )]
    Cache(command::cache::CacheArgs),
    #[command(
        about = "Manage local, never-committed working-tree overlays (Libra extension)",
        after_help = command::layer::LAYER_EXAMPLES
    )]
    Layer(command::layer::LayerArgs),
    #[command(
        about = "Object-level operations incl. payload obliteration (Libra extension)",
        after_help = command::file::FILE_EXAMPLES
    )]
    File(command::file::FileArgs),
    #[command(
        about = "Manage object alternates — borrow objects from a shared store (Libra extension)",
        after_help = command::alternates::ALTERNATES_EXAMPLES
    )]
    Alternates(command::alternates::AlternatesArgs),
    #[command(
        about = "Manage the file dependency graph (Libra extension)",
        after_help = command::deps::DEPS_EXAMPLES
    )]
    Deps(command::deps::DepsArgs),
    #[command(
        about = "Hydrate working-tree content on demand (Libra extension)",
        after_help = command::hydrate::HYDRATE_EXAMPLES
    )]
    Hydrate(command::hydrate::HydrateArgs),
    #[cfg(feature = "fastcdc")]
    #[command(
        about = "FastCDC LFS media chunking client (Libra extension, lore.md §6)",
        after_help = command::media::MEDIA_EXAMPLES
    )]
    Media(command::media::MediaArgs),
    #[command(
        name = "sparse-view",
        about = "Manage the read-only sparse view filter over ls-files/diff (Libra extension)",
        after_help = command::sparse_view::SPARSE_VIEW_EXAMPLES
    )]
    SparseView(command::sparse_view::SparseViewArgs),
    #[command(
        about = "Branch/repo metadata key-value store (Libra extension)",
        after_help = command::metadata::METADATA_EXAMPLES
    )]
    Metadata(command::metadata::MetadataArgs),
    #[command(
        about = "Mark paths dirty in the dirty-set cache, or list it (Libra extension)",
        after_help = command::dirty::DIRTY_EXAMPLES
    )]
    Dirty(command::dirty::DirtyArgs),
    #[command(
        about = "Manage host-scoped HTTP tokens: login, status, logout (Libra extension)",
        after_help = command::auth::AUTH_EXAMPLES
    )]
    Auth(command::auth::AuthArgs),
    #[command(about = "Log in to Libra website account via browser")]
    Login(command::account::LoginArgs),
    #[command(about = "Show the current Libra website account session")]
    Whoami(command::account::WhoamiArgs),
    #[command(about = "Log out of the Libra website account session")]
    Logout(command::account::LogoutArgs),
    #[command(
        about = "Look up revisions by ordinal on a branch's first-parent chain (Libra extension)",
        after_help = command::revision::REVISION_EXAMPLES
    )]
    Revision(command::revision::RevisionArgs),
    #[command(
        about = "Run a headless local service: notification bus + dirty-mark ingestion (Libra extension)",
        after_help = command::service::SERVICE_EXAMPLES
    )]
    Service(command::service::ServiceArgs),
    #[command(about = "Summarize commit history by author", alias = "slog")]
    Shortlog(command::shortlog::ShortlogArgs),
    #[command(about = "Show various types of objects")]
    Show(command::show::ShowArgs),
    #[command(about = "List references in a local repository")]
    ShowRef(command::show_ref::ShowRefArgs),
    #[command(
        about = "Generate mbox-formatted patch files from commits",
        after_help = command::format_patch::FORMAT_PATCH_EXAMPLES
    )]
    FormatPatch(command::format_patch::FormatPatchArgs),
    #[command(
        about = "Iterate over refs in a local repository with formatting and filtering",
        after_help = command::for_each_ref::FOR_EACH_REF_EXAMPLES
    )]
    ForEachRef(command::for_each_ref::ForEachRefArgs),
    #[command(about = "List references in a remote repository")]
    LsRemote(command::ls_remote::LsRemoteArgs),
    #[command(
        about = "List the contents of a tree object",
        after_help = command::ls_tree::LS_TREE_EXAMPLES
    )]
    LsTree(command::ls_tree::LsTreeArgs),
    #[command(about = "Read or update the symbolic HEAD ref")]
    SymbolicRef(command::symbolic_ref::SymbolicRefArgs),
    #[command(about = "Parse and normalize revision names and repository paths")]
    RevParse(command::rev_parse::RevParseArgs),
    #[command(about = "List commit objects reachable from a revision")]
    RevList(command::rev_list::RevListArgs),
    #[command(about = "Show changes between commits, commit and working tree, etc")]
    Diff(command::diff::DiffArgs),
    #[command(about = "Search for patterns in tracked files")]
    Grep(command::grep::GrepArgs),
    #[command(about = "Show author and history of each line of a file")]
    Blame(command::blame::BlameArgs),
    #[command(
        about = "Give an object a human readable name based on an available ref",
        alias = "desc"
    )]
    Describe(command::describe::DescribeArgs),
    #[command(
        about = "Add, show, list, or remove notes attached to commits",
        after_help = command::notes::NOTES_EXAMPLES
    )]
    Notes(command::notes::NotesArgs),
    #[command(about = "Provide content, type or size info for repository objects")]
    CatFile(command::cat_file::CatFileArgs),
    #[command(
        about = "Report pathnames excluded by .libraignore rules",
        after_help = command::check_ignore::CHECK_IGNORE_EXAMPLES
    )]
    CheckIgnore(command::check_ignore::CheckIgnoreArgs),
    #[command(
        about = "Report .libra_attributes attributes for pathnames",
        after_help = command::check_attr::CHECK_ATTR_EXAMPLES
    )]
    CheckAttr(command::check_attr::CheckAttrArgs),
    #[command(
        about = "Resolve Name <email> contacts through .mailmap",
        after_help = command::check_mailmap::CHECK_MAILMAP_EXAMPLES
    )]
    CheckMailmap(command::check_mailmap::CheckMailmapArgs),
    #[command(
        about = "Emit history as a fast-import stream (git fast-export)",
        after_help = command::fast_export::FAST_EXPORT_EXAMPLES
    )]
    FastExport(command::fast_export::FastExportArgs),
    #[command(
        about = "Create and inspect Git v2 bundle files",
        after_help = command::bundle::BUNDLE_EXAMPLES
    )]
    Bundle(command::bundle::BundleArgs),
    #[command(
        about = "Import a git fast-import stream",
        after_help = command::fast_import::FAST_IMPORT_EXAMPLES
    )]
    FastImport(command::fast_import::FastImportArgs),
    #[command(
        about = "Generate a shell completion script",
        after_help = command::completions::COMPLETIONS_EXAMPLES
    )]
    Completions(command::completions::CompletionsArgs),
    #[command(
        about = "Create an archive of files from a named tree",
        after_help = command::archive::ARCHIVE_EXAMPLES
    )]
    Archive(command::archive::ArchiveArgs),
    #[command(about = "Compute Git-compatible object IDs")]
    HashObject(command::hash_object::HashObjectArgs),
    #[command(
        about = "Write the current index out as a tree object",
        after_help = command::write_tree::WRITE_TREE_EXAMPLES
    )]
    WriteTree(command::write_tree::WriteTreeArgs),
    #[command(
        about = "Create a commit object from an existing tree (plumbing; no ref updates)",
        after_help = command::commit_tree::COMMIT_TREE_EXAMPLES,
        name = "commit-tree"
    )]
    CommitTree(command::commit_tree::CommitTreeArgs),
    #[command(
        about = "Read a tree object into the index",
        after_help = command::read_tree::READ_TREE_EXAMPLES
    )]
    ReadTree(command::read_tree::ReadTreeArgs),
    #[command(
        about = "Modify the index directly (add/remove/cacheinfo)",
        after_help = command::update_index::UPDATE_INDEX_EXAMPLES
    )]
    UpdateIndex(command::update_index::UpdateIndexArgs),
    #[command(
        about = "Safely update, create, or delete a refs/heads/<branch> ref",
        after_help = command::update_ref::UPDATE_REF_EXAMPLES
    )]
    UpdateRef(command::update_ref::UpdateRefArgs),
    #[command(about = "Validate pack index files against pack archives")]
    VerifyPack(command::verify_pack::VerifyPackArgs),

    #[command(about = "Record changes to the repository", alias = "ci")]
    Commit(command::commit::CommitArgs),
    #[command(about = "List, create, or delete branches", alias = "br")]
    Branch(command::branch::BranchArgs),
    #[command(about = "Switch branches", alias = "sw")]
    Switch(command::switch::SwitchArgs),
    #[command(
        about = "Branch compatibility surface; prefer 'switch' for branches and 'restore' for files"
    )]
    Checkout(command::checkout::CheckoutArgs),
    #[command(about = "Create a new tag")]
    Tag(command::tag::TagArgs),
    #[command(about = "Merge changes")]
    Merge(command::merge::MergeArgs),
    #[command(
        about = "Three-way merge files (git merge-file)",
        after_help = command::merge_file::MERGE_FILE_EXAMPLES
    )]
    MergeFile(command::merge_file::MergeFileArgs),
    #[command(
        about = "Find the best common ancestor(s) of two commits",
        after_help = command::merge_base::MERGE_BASE_EXAMPLES
    )]
    MergeBase(command::merge_base::MergeBaseArgs),
    #[command(
        about = "Check whether a patch applies (git apply --check)",
        after_help = command::apply::APPLY_EXAMPLES
    )]
    Apply(command::apply::ApplyArgs),
    #[command(
        about = "Diff between two trees (git diff-tree)",
        after_help = command::diff_plumbing::DIFF_TREE_EXAMPLES
    )]
    DiffTree(command::diff_plumbing::DiffTreeArgs),
    #[command(
        about = "Diff a tree against the working tree (git diff-index)",
        after_help = command::diff_plumbing::DIFF_INDEX_EXAMPLES
    )]
    DiffIndex(command::diff_plumbing::DiffIndexArgs),
    #[command(
        about = "Diff the index against the working tree (git diff-files)",
        after_help = command::diff_plumbing::DIFF_FILES_EXAMPLES
    )]
    DiffFiles(command::diff_plumbing::DiffFilesArgs),
    #[command(
        about = "Vault-backed Git credential helper (fill/store/erase)",
        after_help = command::credential::CREDENTIAL_EXAMPLES
    )]
    Credential(command::credential::CredentialArgs),
    #[command(
        about = "Reuse recorded conflict resolutions (git rerere)",
        after_help = command::rerere::RERERE_EXAMPLES
    )]
    Rerere(command::rerere::RerereArgs),
    #[command(about = "Reapply commits on top of another base tip", alias = "rb")]
    Rebase(command::rebase::RebaseArgs),
    #[command(about = "Reset current HEAD to specified state")]
    Reset(command::reset::ResetArgs),
    #[command(
        about = "Apply the changes introduced by some existing commits",
        alias = "cp"
    )]
    CherryPick(command::cherry_pick::CherryPickArgs),
    #[command(about = "Update remote refs along with associated objects")]
    Push(command::push::PushArgs),
    #[command(about = "Download objects and refs from another repository")]
    Fetch(command::fetch::FetchArgs),
    #[command(about = "Fetch from and integrate with another repository or a local branch")]
    Pull(command::pull::PullArgs),
    #[command(about = "Verify the integrity of objects, refs, and index")]
    Fsck(command::fsck::FsckArgs),
    #[command(
        about = "Run tasks to optimize Git repository data",
        after_help = command::maintenance::MAINTENANCE_EXAMPLES
    )]
    Maintenance(command::maintenance::MaintenanceArgs),
    #[command(
        about = "Combine repository objects into a single pack",
        after_help = command::repack::REPACK_EXAMPLES
    )]
    Repack(command::repack::RepackArgs),
    #[command(about = "Revert some existing commits")]
    Revert(command::revert::RevertArgs),
    #[command(
        about = "Create, list, or delete object replacements (refs/replace)",
        after_help = command::replace::REPLACE_EXAMPLES
    )]
    Replace(command::replace::ReplaceArgs),
    #[command(about = "Manage the log of reference changes (e.g., HEAD, branches)")]
    Reflog(command::reflog::ReflogArgs),
    #[command(about = "View and restore command-level operation history")]
    Op(command::op::OpArgs),
    #[command(
        subcommand,
        about = "Use binary search to find the commit that introduced a bug",
        after_help = command::bisect::BISECT_EXAMPLES
    )]
    Bisect(Bisect),

    #[command(
        subcommand,
        about = "Manage set of tracked repositories",
        after_help = command::remote::REMOTE_EXAMPLES
    )]
    Remote(command::remote::RemoteCmds),
    #[command(about = "Open the repository in the browser")]
    Open(command::open::OpenArgs),
    #[command(about = "Cloud backup and restore operations (D1/R2)")]
    Cloud(command::cloud::CloudArgs),
    #[command(about = "Manage read-only Cloudflare Worker publishing")]
    Publish(command::publish::PublishArgs),

    #[command(about = "Start Libra Code interactive TUI (with background web server)")]
    Code(command::code::CodeArgs),
    #[command(about = "Drive a local Libra Code TUI automation control session")]
    CodeControl(command::code_control::CodeControlArgs),
    #[command(about = "Manage AI automation rules and history")]
    Automation(command::automation::AutomationArgs),
    #[command(about = "Report AI provider/model usage")]
    Usage(command::usage::UsageArgs),
    #[command(about = "Inspect an AI thread version graph in a TUI")]
    Graph(command::graph::GraphArgs),
    #[command(about = "Inspect AI sandbox diagnostics")]
    Sandbox(command::sandbox::SandboxArgs),
    #[command(about = "Manage external-agent capture (Claude Code, Gemini, …)")]
    Agent(command::agent::AgentArgs),
    #[command(about = "Run read-only external-agent code reviews (AG-22)")]
    Review(command::agent::review::ReviewArgs),
    #[command(about = "Run read-only round-robin agent investigations (AG-23)")]
    Investigate(command::agent::investigate::InvestigateArgs),
    #[command(
        about = "Build pack index file for an existing packed archive",
        hide = true
    )]
    IndexPack(command::index_pack::IndexPackArgs),
    #[command(
        about = "Create a pack from object ids read on stdin (internal plumbing)",
        after_help = command::pack_objects::PACK_OBJECTS_EXAMPLES,
        hide = true
    )]
    PackObjects(command::pack_objects::PackObjectsArgs),
    #[command(
        about = "Compatibility entry for hook configurations installed by `libra agent enable`",
        hide = true
    )]
    Hooks(command::hooks::HooksArgs),
}

#[derive(Subcommand, Debug)]
pub enum Stash {
    #[command(about = "Save your local modifications to a new stash")]
    Push {
        #[arg(short, long, help = "The message to display for the stash")]
        message: Option<String>,
        #[arg(
            short = 'u',
            long = "include-untracked",
            help = "Include untracked files in the stash",
            overrides_with = "no_include_untracked"
        )]
        include_untracked: bool,
        #[arg(
            long = "no-include-untracked",
            help = "Do not include untracked files (the default); countermands an earlier -u/--include-untracked (last one wins)",
            overrides_with = "include_untracked"
        )]
        no_include_untracked: bool,
        #[arg(
            short = 'a',
            long = "all",
            help = "Include untracked and ignored files in the stash"
        )]
        all: bool,
        #[arg(
            short = 'k',
            long = "keep-index",
            help = "Keep staged changes in the index and working tree"
        )]
        keep_index: bool,
        #[arg(
            value_name = "pathspec",
            help = "Stash only the changes to the given paths, leaving the rest of the working tree intact"
        )]
        pathspec: Vec<String>,
    },
    #[command(about = "Remove a single stashed state from the stash list")]
    Pop {
        #[arg(help = "The stash to pop")]
        stash: Option<String>,
    },
    #[command(about = "List the stashes that you currently have")]
    List,
    #[command(about = "Like pop, but do not remove the state from the stash list")]
    Apply {
        #[arg(help = "The stash to apply")]
        stash: Option<String>,
    },
    #[command(about = "Remove a single stashed state from the stash list")]
    Drop {
        #[arg(help = "The stash to drop")]
        stash: Option<String>,
    },
    #[command(
        about = "Show the changes recorded in the stash as a file-level summary or a unified diff (-p)"
    )]
    Show {
        #[arg(help = "Stash reference (default: stash@{0})")]
        stash: Option<String>,
        #[arg(long, help = "Show only the file names that changed")]
        name_only: bool,
        #[arg(long, help = "Show only file names with their status code")]
        name_status: bool,
        #[arg(
            short = 'p',
            long = "patch",
            help = "Show the stashed changes as a unified diff (patch)"
        )]
        patch: bool,
    },
    #[command(about = "Create and check out a new branch from the stash, then drop it")]
    Branch {
        #[arg(help = "Name of the new branch to create")]
        branch: String,
        #[arg(help = "Stash reference (default: stash@{0})")]
        stash: Option<String>,
    },
    #[command(about = "Remove all stashed entries")]
    Clear {
        #[arg(
            long,
            help = "Skip confirmation; required outside JSON / machine modes"
        )]
        force: bool,
    },
}

#[derive(Subcommand, Debug)]
pub enum Bisect {
    #[command(about = "Start a new bisect session")]
    Start {
        #[arg(help = "Bad commit to start from")]
        bad: Option<String>,
        #[arg(long, short, help = "Good commit to mark")]
        good: Option<String>,
        #[arg(
            long = "first-parent",
            help = "Follow only the first parent of merge commits while bisecting"
        )]
        first_parent: bool,
    },
    #[command(about = "Mark the current or given commit as bad")]
    Bad {
        #[arg(help = "Commit to mark as bad")]
        rev: Option<String>,
    },
    #[command(about = "Mark the current or given commit as good")]
    Good {
        #[arg(help = "Commit to mark as good")]
        rev: Option<String>,
    },
    #[command(about = "End bisect session and restore original HEAD")]
    Reset {
        #[arg(help = "Commit to reset to (optional)")]
        rev: Option<String>,
    },
    #[command(about = "Skip current commit and move to next")]
    Skip {
        #[arg(help = "Commit to skip")]
        rev: Option<String>,
    },
    #[command(about = "Show bisect log")]
    Log,
    #[command(about = "Run a script for each commit until convergence")]
    Run {
        #[arg(
            help = "Command to run for each commit; first arg is the executable",
            required = true,
            trailing_var_arg = true,
            allow_hyphen_values = true
        )]
        cmd: Vec<String>,
    },
    #[command(
        about = "Show the current bisect state and remaining candidates",
        visible_alias = "visualize"
    )]
    View,
}

/// Synchronous CLI entry — used by both the `libra` binary and embedders that cannot
/// (or do not wish to) own their own Tokio runtime.
///
/// Functional scope:
/// - Builds a multi-thread Tokio runtime, then drives [`parse_async`] to completion.
/// - When `args` is `None`, the underlying parser falls back to `std::env::args`.
///
/// Boundary conditions:
/// - Calling this from inside an existing Tokio runtime panics; embedders that are
///   already async must call [`parse_async`] directly. See the embedding contract in
///   [`crate::exec`].
/// - Returns `CliError::fatal` if the runtime itself cannot be constructed (extremely
///   unlikely outside of OOM scenarios).
pub fn parse(args: Option<&[&str]>) -> CliResult<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| CliError::fatal(format!("failed to create tokio runtime: {e}")))?;

    // The one vetted telemetry span (lore.md 1.7): canonical subcommand name,
    // duration, and on failure the stable LBR-* code — NOTHING else. Plain
    // `tracing`, so it is a no-op without a matching layer; the OTLP layer is
    // feature+endpoint gated, and the fmt layer excludes this target so
    // LIBRA_LOG output is byte-unchanged. Library embedders calling
    // parse_async/exec_async directly bypass it (documented).
    let command_name = canonical_command_name(args);
    let span = tracing::info_span!(
        target: "libra::telemetry",
        "libra.command",
        libra.command = command_name.as_deref().unwrap_or("<none>"),
        otel.status_code = tracing::field::Empty,
        libra.error_code = tracing::field::Empty,
    );
    let result = span.in_scope(|| runtime.block_on(Box::pin(parse_async(args))));
    if let Err(error) = &result {
        // tracing-opentelemetry maps `otel.status_code` to the OTel status.
        span.record("otel.status_code", "ERROR");
        span.record("libra.error_code", error.stable_code().as_str());
    }
    result
}

/// The CANONICAL subcommand name for telemetry: the raw argv token resolved
/// through clap's own metadata (aliases like `br` canonicalize to `branch`).
/// Never derived from user argv content beyond the subcommand token itself.
fn canonical_command_name(args: Option<&[&str]>) -> Option<String> {
    let argv: Vec<String> = match args {
        Some(args) => args.iter().map(|s| s.to_string()).collect(),
        None => env::args().collect(),
    };
    let (index, _) = find_subcommand_index(&argv)?;
    let token = argv.get(index)?;
    let cli = <Cli as clap::CommandFactory>::command();
    cli.get_subcommands()
        .find(|candidate| {
            candidate.get_name() == token.as_str()
                || candidate
                    .get_all_aliases()
                    .any(|alias| alias == token.as_str())
        })
        .map(|candidate| candidate.get_name().to_string())
}

/// Rewrite Git-style `-<n>` shortcuts into the long-form `-n <n>` flag, but only when
/// the active subcommand is `log`.
///
/// Git accepts `git log -3` as shorthand for `git log -n 3`, but clap cannot express a
/// purely numeric flag without conflicting with positional revisions. This helper
/// patches argv before clap sees it so users keep the familiar shortcut.
///
/// Boundary conditions:
/// - The rewrite only fires for arguments before any `--` separator inside the `log`
///   subcommand, so paths or revisions that happen to look like `-3` are preserved
///   verbatim once the user explicitly closes the option list.
/// - When `log` is not the active subcommand the original argv is returned unchanged,
///   leaving every other command's `-<n>` semantics untouched.
///
/// See: [`tests::clap_alias_br_resolves_to_branch`] and friends for related parser
/// behaviour. The exact rewrite is exercised end-to-end by the integration tests in
/// `tests/command/log_test.rs`.
fn rewrite_log_short_number_args(args: Vec<String>) -> Vec<String> {
    // Detect the real subcommand position to avoid rewriting positional args for other commands.
    let subcommand = find_subcommand_index(&args);
    let Some((log_index, from_double_dash)) = subcommand else {
        return args;
    };
    if !matches!(args.get(log_index), Some(name) if name == "log") {
        return args;
    }

    let mut out: Vec<String> = Vec::with_capacity(args.len() + 2);
    if from_double_dash {
        // Drop the `--` that was used to separate global args from the subcommand.
        for (idx, arg) in args.iter().enumerate().take(log_index + 1) {
            if idx + 1 == log_index && arg == "--" {
                continue;
            }
            out.push(arg.clone());
        }
    } else {
        out.extend(args.iter().take(log_index + 1).cloned());
    }

    // Respect `--` inside the log subcommand: stop rewriting after it.
    let mut after_double_dash = false;
    for arg in args.into_iter().skip(log_index + 1) {
        if after_double_dash {
            out.push(arg);
            continue;
        }

        if arg == "--" {
            after_double_dash = true;
            out.push(arg);
            continue;
        }

        if is_short_number_flag(&arg) {
            out.push("-n".to_string());
            out.push(arg[1..].to_string());
        } else {
            out.push(arg);
        }
    }

    out
}

fn rewrite_index_pack_progress_args(args: Vec<String>) -> Vec<String> {
    let subcommand = find_subcommand_index(&args);
    let Some((index_pack_index, from_double_dash)) = subcommand else {
        return args;
    };
    if !matches!(args.get(index_pack_index), Some(name) if name == "index-pack") {
        return args;
    }

    let mut out: Vec<String> = Vec::with_capacity(args.len());
    if from_double_dash {
        for (idx, arg) in args.iter().enumerate().take(index_pack_index + 1) {
            if idx + 1 == index_pack_index && arg == "--" {
                continue;
            }
            out.push(arg.clone());
        }
    } else {
        out.extend(args.iter().take(index_pack_index + 1).cloned());
    }

    let mut after_double_dash = false;
    for arg in args.into_iter().skip(index_pack_index + 1) {
        if after_double_dash {
            out.push(arg);
            continue;
        }
        if arg == "--" {
            after_double_dash = true;
            out.push(arg);
            continue;
        }
        match arg.as_str() {
            "--progress" => out.push("--progress=text".to_string()),
            "--no-progress" => out.push("--progress=none".to_string()),
            _ => out.push(arg),
        }
    }

    out
}

fn rewrite_reset_pathspec_separator_args(args: Vec<String>) -> Vec<String> {
    let subcommand = find_subcommand_index(&args);
    let Some((reset_index, from_double_dash)) = subcommand else {
        return args;
    };
    if !matches!(args.get(reset_index), Some(name) if name == "reset") {
        return args;
    }

    let separator_index = args
        .iter()
        .enumerate()
        .skip(reset_index + 1)
        .find_map(|(index, arg)| (arg == "--").then_some(index));
    let Some(separator_index) = separator_index else {
        return args;
    };

    let has_target_before_separator =
        reset_has_positional_target_before_separator(&args, reset_index + 1, separator_index);
    let mut out = Vec::with_capacity(args.len() + usize::from(!has_target_before_separator));
    if from_double_dash {
        for (idx, arg) in args.iter().enumerate().take(reset_index + 1) {
            if idx + 1 == reset_index && arg == "--" {
                continue;
            }
            out.push(arg.clone());
        }
    } else {
        out.extend(args.iter().take(reset_index + 1).cloned());
    }

    for arg in args.iter().take(separator_index).skip(reset_index + 1) {
        out.push(arg.clone());
    }
    out.push(format!(
        "--{}",
        command::reset::RESET_PATHSPEC_SEPARATOR_FLAG
    ));
    if !has_target_before_separator && args.get(separator_index + 1).is_some() {
        out.push(command::reset::DEFAULT_RESET_TARGET.to_string());
    }
    out.push("--".to_string());
    out.extend(args.iter().skip(separator_index + 1).cloned());
    out
}

fn reset_has_positional_target_before_separator(
    args: &[String],
    start: usize,
    separator_index: usize,
) -> bool {
    let mut index = start;
    while index < separator_index {
        let arg = &args[index];
        if reset_flag_takes_separate_value(arg) {
            index += 2;
            continue;
        }
        if arg.starts_with('-') {
            index += 1;
            continue;
        }
        return true;
    }
    false
}

fn reset_flag_takes_separate_value(arg: &str) -> bool {
    matches!(
        arg,
        "--pathspec-from-file" | "--color" | "--progress" | "--max-connections"
    )
}

/// Locate the first non-flag token in `args` and return its index plus whether it was
/// produced by an explicit `--` separator.
///
/// Boundary conditions:
/// - Skips over any leading flags (`-x`, `--long`) so `libra --json status` still
///   identifies `status` as the subcommand.
/// - When `--` appears, the *next* argument is treated as the subcommand and the
///   returned `bool` is `true` to signal the caller to drop the separator. Returns
///   `None` if `--` is the last token.
/// - Returns `None` when no non-flag token exists (e.g. argv is `["libra"]` or
///   `["libra", "--help"]`).
fn find_subcommand_index(args: &[String]) -> Option<(usize, bool)> {
    let mut i = 1;
    while i < args.len() {
        let arg = &args[i];
        if arg == "--" {
            return if i + 1 < args.len() {
                Some((i + 1, true))
            } else {
                None
            };
        }
        if matches!(arg.as_str(), "--color" | "--progress") {
            i = (i + 2).min(args.len());
            continue;
        }
        if arg.starts_with("--color=")
            || arg.starts_with("--progress=")
            || arg.starts_with("--json=")
        {
            i += 1;
            continue;
        }
        if !arg.starts_with('-') {
            return Some((i, false));
        }
        i += 1;
    }
    None
}

fn is_short_number_flag(arg: &str) -> bool {
    if !arg.starts_with('-') || arg.len() < 2 {
        return false;
    }
    let rest = &arg[1..];
    rest.chars().all(|c| c.is_ascii_digit())
}

/// Inputs that look like top-level subcommands but should be redirected elsewhere.
/// Each entry is (input, hint_message).  Only needed for words that cannot be
/// expressed as a clap `alias` (e.g. they map to a *flag* of another command).
const REDIRECTED_COMMANDS: &[(&str, &str)] =
    &[("import", "You probably want `libra config --import`.")];

/// Build extra hint lines for an unrecognised-subcommand error.
///
/// The hints supplement (never duplicate) clap's built-in "tip: a similar
/// subcommand exists" message.  We only emit our own hints for cases that
/// clap cannot know about – e.g. redirecting `libra import` to
/// `libra config --import`.
fn parse_error_hints(err: &clap::Error) -> Vec<String> {
    let mut hints = Vec::new();

    if let Some(ContextValue::String(cmd)) = err.get(ContextKind::InvalidSubcommand) {
        let cmd_lower = cmd.to_lowercase();

        // Check redirected commands (e.g. `libra import` → `libra config --import`).
        for &(input, message) in REDIRECTED_COMMANDS {
            if cmd_lower == input {
                hints.push(message.to_string());
            }
        }
    }
    hints
}

fn push_unique_hint(hints: &mut Vec<String>, hint: String) {
    if !hint.is_empty() && !hints.iter().any(|existing| existing == &hint) {
        hints.push(hint);
    }
}

fn top_level_unknown_command_hints(err: &clap::Error) -> Vec<String> {
    let mut hints = parse_error_hints(err);

    if let Some(ContextValue::Strings(suggestions)) = err.get(ContextKind::SuggestedSubcommand) {
        match suggestions.as_slice() {
            [] => {}
            [suggestion] => push_unique_hint(
                &mut hints,
                format!("a similar subcommand exists: '{suggestion}'"),
            ),
            suggestions => {
                let suggestions = suggestions
                    .iter()
                    .map(|suggestion| format!("'{suggestion}'"))
                    .collect::<Vec<_>>()
                    .join(", ");
                push_unique_hint(
                    &mut hints,
                    format!("similar subcommands exist: {suggestions}"),
                );
            }
        }
    }

    if let Some(ContextValue::StyledStrs(suggestions)) = err.get(ContextKind::Suggested) {
        for suggestion in suggestions {
            push_unique_hint(&mut hints, suggestion.to_string().trim().to_string());
        }
    }

    hints
}

const REMOVED_CODE_CLAUDECODE_FLAGS: &[&str] = &[
    "--resume-session",
    "--fork-session",
    "--session-id",
    "--resume-at",
    "--helper-path",
    "--python-binary",
    "--timeout-seconds",
    "--permission-mode",
];

fn removed_code_claudecode_hints(argv: &[String]) -> Vec<String> {
    let Some((subcommand_index, _)) = find_subcommand_index(argv) else {
        return Vec::new();
    };
    if !matches!(argv.get(subcommand_index).map(String::as_str), Some("code")) {
        return Vec::new();
    }

    let mut hints = Vec::new();
    let has_removed_provider = argv.windows(2).any(
        |window| matches!(window, [flag, value] if flag == "--provider" && value == "claudecode"),
    ) || argv.iter().any(|arg| arg == "--provider=claudecode");
    if has_removed_provider {
        hints.push(
            "`libra code --provider claudecode` was removed; use `--provider codex` for the managed agent runtime or `--provider anthropic` for direct Anthropic chat completions."
                .to_string(),
        );
    }

    let has_removed_flag = argv.iter().any(|arg| {
        REMOVED_CODE_CLAUDECODE_FLAGS
            .iter()
            .any(|flag| arg == flag || arg.starts_with(&format!("{flag}=")))
    });
    if has_removed_flag {
        hints.push(
            "Claude Code provider-session flags were removed with the managed runtime; start a new Codex or generic-provider session and use Libra's canonical `--resume <thread_id>` flow."
                .to_string(),
        );
    }

    hints
}

fn parse_error_components(err: &clap::Error) -> (String, Option<String>, Vec<String>) {
    let rendered = err.to_string();
    let mut message = None;
    let mut usage_lines = Vec::new();
    let mut hints = Vec::new();

    for line in rendered.lines() {
        let trimmed = line.trim_start();
        if let Some(tip) = trimmed.strip_prefix("tip:") {
            hints.push(tip.trim().to_string());
            continue;
        }
        if message.is_none() {
            if let Some(msg) = trimmed.strip_prefix("error:") {
                message = Some(msg.trim().to_string());
                continue;
            }
            if !trimmed.is_empty() {
                message = Some(trimmed.to_string());
                continue;
            }
        }
        usage_lines.push(line.to_string());
    }

    hints.extend(parse_error_hints(err));

    let usage = if usage_lines.is_empty() {
        None
    } else {
        Some(usage_lines.join("\n").trim().to_string())
    };

    (
        message.unwrap_or_else(|| rendered.trim().to_string()),
        usage,
        hints,
    )
}

fn shell_quote_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    shell_quote_text(&raw)
}

#[cfg(windows)]
fn shell_quote_text(raw: &str) -> String {
    format!("\"{}\"", raw)
}

#[cfg(not(windows))]
fn shell_quote_text(raw: &str) -> String {
    format!("'{}'", raw.replace('\'', "'\"'\"'"))
}

fn git_conversion_hint(location: &utils::util::GitRepositoryLocation) -> String {
    let command = if location.is_bare {
        "libra init --bare --from-git-repository ."
    } else {
        "libra init --from-git-repository ."
    };
    let current = utils::util::cur_dir()
        .canonicalize()
        .unwrap_or_else(|_| utils::util::cur_dir());

    if current == location.root {
        format!("run '{command}' to convert this Git repository to Libra.")
    } else {
        format!("run: cd {} && {command}", shell_quote_path(&location.root))
    }
}

fn repo_not_found_error(path: Option<&Path>) -> CliError {
    let mut error = CliError::repo_not_found();
    if let Some(location) = utils::util::find_git_repository(path) {
        error = error.with_priority_hint(git_conversion_hint(&location));
    }
    error
}

struct CommandPreflight {
    storage: Option<std::path::PathBuf>,
    /// When `true`, the repository database is opened through the pooled,
    /// schema-aware connection ([`set_local_hash_kind_for_storage`]), which
    /// auto-applies any pending migrations. When `false`, a read-only raw
    /// connection is used instead so the on-disk schema is left untouched
    /// (e.g. read-only `hash-object` / `verify-pack`).
    upgrade_schema: bool,
    set_hash_kind: bool,
}

impl CommandPreflight {
    fn none() -> Self {
        Self {
            storage: None,
            upgrade_schema: false,
            set_hash_kind: false,
        }
    }

    fn sha1_without_repo() -> Self {
        Self {
            storage: None,
            upgrade_schema: false,
            set_hash_kind: true,
        }
    }

    fn repo(storage: std::path::PathBuf) -> Self {
        Self {
            storage: Some(storage),
            upgrade_schema: true,
            set_hash_kind: true,
        }
    }

    fn repo_hash_kind_without_schema_guard(storage: std::path::PathBuf) -> Self {
        Self {
            storage: Some(storage),
            upgrade_schema: false,
            set_hash_kind: true,
        }
    }
}

fn command_preflight(command: &Commands) -> CliResult<CommandPreflight> {
    match command {
        Commands::Init(_)
        | Commands::Clone(_)
        | Commands::Open(_)
        | Commands::CodeControl(_)
        | Commands::LsRemote(_)
        // `merge-file` is a standalone three-way text merge over files on disk;
        // it touches no objects and works outside a repository, like Git.
        | Commands::MergeFile(_)
        // `credential` is a Git credential helper: `fill` must be a clean miss
        // (exit 0, no output) even outside a repository, so it touches no objects
        // and skips the hash-kind preflight. It resolves the repo vault lazily.
        | Commands::Credential(_)
        // `completions` renders a shell script from the clap command tree; it
        // reads no objects and works outside a repository.
        | Commands::Completions(_)
        // `logfile` only inspects env-derived tracing configuration.
        | Commands::Logfile(_)
        // `auth` manages host-global tokens in the GLOBAL store; it works
        // outside a repository and touches no objects.
        | Commands::Auth(_)
        | Commands::Login(_)
        | Commands::Whoami(_)
        | Commands::Logout(_)
        | Commands::Sandbox(_) => Ok(CommandPreflight::none()),
        // `cache info` only inspects env/config-derived storage tunables and
        // works outside a repository; `cache evict` deletes local objects, so
        // it takes the standard repo + hash-kind preflight.
        Commands::Cache(cache_args)
            if matches!(cache_args.command, command::cache::CacheCommand::Info) =>
        {
            Ok(CommandPreflight::none())
        }
        Commands::HashObject(args) if !args.write => {
            match utils::util::try_get_storage_path(None) {
                Ok(storage) => Ok(CommandPreflight::repo_hash_kind_without_schema_guard(
                    storage,
                )),
                Err(_) => Ok(CommandPreflight::sha1_without_repo()),
            }
        }
        Commands::VerifyPack(_) => match utils::util::try_get_storage_path(None) {
            Ok(storage) => Ok(CommandPreflight::repo_hash_kind_without_schema_guard(
                storage,
            )),
            Err(_) => Ok(CommandPreflight::sha1_without_repo()),
        },
        // `grep --no-index` searches the filesystem directly and works outside a
        // repository, so it needs no storage/hash-kind preflight.
        Commands::Grep(args) if args.no_index => Ok(CommandPreflight::none()),
        Commands::Archive(args) if args.list => Ok(CommandPreflight::none()),
        #[cfg(unix)]
        Commands::Worktree(command::worktree::WorktreeArgs {
            command: command::worktree::WorktreeSubcommand::Umount { .. },
        }) => Ok(CommandPreflight::none()),
        // Config global/system scopes don't require a repository.
        Commands::Config(cfg) if cfg.global || cfg.system => Ok(CommandPreflight::none()),
        Commands::Code(code_args) => {
            let working_dir = command::code::resolve_code_preflight_working_dir(code_args)?;
            let storage = utils::util::try_get_storage_path(Some(working_dir.clone()))
                .map_err(|_| repo_not_found_error(Some(&working_dir)))?;
            Ok(CommandPreflight::repo(storage))
        }
        Commands::Graph(graph_args) => {
            let storage = utils::util::try_get_storage_path(graph_args.repo.clone())
                .map_err(|_| repo_not_found_error(graph_args.repo.as_deref()))?;
            Ok(CommandPreflight::repo(storage))
        }
        _ => {
            let storage =
                utils::util::try_get_storage_path(None).map_err(|_| repo_not_found_error(None))?;
            Ok(CommandPreflight::repo(storage))
        }
    }
}

fn is_error_codes_help_topic(argv: &[String]) -> bool {
    let Some((index, _)) = find_subcommand_index(argv) else {
        return false;
    };
    if !matches!(argv.get(index).map(String::as_str), Some("help")) {
        return false;
    }
    if !matches!(
        argv.get(index + 1).map(String::as_str),
        Some("error-codes" | "errors")
    ) {
        return false;
    }
    index + 2 == argv.len()
}

fn print_error_codes_help() -> CliResult<()> {
    let mut stdout = std::io::stdout().lock();
    stdout
        .write_all(ERROR_CODES_HELP.as_bytes())
        .map_err(|e| CliError::fatal(format!("failed to write error code help: {e}")))?;
    stdout
        .flush()
        .map_err(|e| CliError::fatal(format!("failed to flush error code help: {e}")))?;
    Ok(())
}

fn apply_global_runtime_flags(args: &Cli) -> CliResult<()> {
    // `--sync-data` forces object-write fsync on for this run, layering over the
    // `LIBRA_SYNC_DATA` env default. The flag can only turn it on; absence leaves
    // whatever the env initialised.
    if args.sync_data {
        utils::atomic_write::set_sync_data(true);
    }

    // Object read policy (lore.md §0.8): the `LIBRA_READ_POLICY` env var is the
    // baseline (auto/offline/local/remote); the `--offline` flag overrides it to
    // local-only. ALWAYS set it (resolving to Auto when nothing is requested) so
    // a reused process — TUI, tests — never inherits a stale policy. An
    // unrecognized env value is a hard error rather than a silent Auto fallback,
    // so a typo cannot quietly re-enable durable-tier reads.
    let read_policy = if args.offline {
        utils::read_policy::ReadPolicy::LocalOnly
    } else {
        utils::read_policy::read_policy_from_env().map_err(|message| {
            CliError::command_usage(format!("invalid LIBRA_READ_POLICY: {message}"))
                .with_stable_code(utils::error::StableErrorCode::CliInvalidArguments)
                .with_exit_code(128)
        })?
    };
    utils::read_policy::set_read_policy(read_policy);

    // Resource limits (lore.md §0.9): `--max-connections` flag wins over the
    // `LIBRA_MAX_CONNECTIONS` env baseline, else the default. Always set so a
    // reused process never inherits a stale limit; an invalid env value errors.
    let max_connections = match args.max_connections {
        Some(limit) => limit,
        None => utils::resource_limits::max_connections_from_env()
            .map_err(|message| {
                CliError::command_usage(format!("invalid LIBRA_MAX_CONNECTIONS: {message}"))
                    .with_stable_code(utils::error::StableErrorCode::CliInvalidArguments)
                    .with_exit_code(128)
            })?
            .unwrap_or(utils::resource_limits::DEFAULT_MAX_CONNECTIONS),
    };
    utils::resource_limits::set_max_connections(max_connections);

    Ok(())
}

fn prepare_cli_invocation_state() {
    utils::output::reset_warning_tracker();
    // Pick up `LIBRA_SYNC_DATA` so atomic object writes fsync when requested
    // (lore.md §7.7; the `--sync-data` flag of §0.5 layers on top).
    utils::atomic_write::init_sync_data_from_env();
}

fn is_top_level_unknown_command(argv: &[String], err: &clap::Error) -> Option<String> {
    let invalid = match err.get(ContextKind::InvalidSubcommand) {
        Some(ContextValue::String(cmd)) => cmd,
        _ => return None,
    };

    let (index, _) = find_subcommand_index(argv)?;
    if argv.get(index).is_some_and(|arg| arg == invalid) {
        return Some(invalid.to_string());
    }

    None
}

fn classify_parse_error(argv: &[String], err: &clap::Error) -> CliError {
    if let Some(cmd) = is_top_level_unknown_command(argv, err) {
        let hints = top_level_unknown_command_hints(err);
        let mut cli_error = CliError::unknown_command(format!(
            "libra: '{cmd}' is not a libra command. See 'libra --help'."
        ));
        for hint in hints {
            cli_error = cli_error.with_hint(hint);
        }
        return cli_error;
    }

    let (message, usage, mut hints) = parse_error_components(err);
    hints.extend(removed_code_claudecode_hints(argv));
    let mut cli_error = if find_subcommand_index(argv).is_some() {
        match err.kind() {
            ErrorKind::DisplayHelp | ErrorKind::DisplayVersion => CliError::parse_usage(message),
            _ => CliError::command_usage(message),
        }
    } else {
        CliError::parse_usage(message)
    };

    if let Some(usage) = usage {
        cli_error = cli_error.with_usage(usage);
    }
    for hint in hints {
        cli_error = cli_error.with_hint(hint);
    }

    cli_error
}

/// Async CLI dispatcher — the actual orchestrator behind every Libra invocation.
///
/// Functional scope:
/// 1. Normalises argv (rewrites `log -<n>` shortcuts, strips a leading `--`).
/// 2. Resets the per-process warning tracker so `--exit-code-on-warning` cannot be
///    polluted by a previous invocation in long-lived processes (TUI, tests).
/// 3. Short-circuits the `help error-codes` topic before clap parsing because it
///    would otherwise be treated as an unknown subcommand.
/// 4. Parses with clap and translates every parse failure into a structured
///    [`CliError`] (see [`classify_parse_error`]).
/// 5. Validates command-specific arg constraints that clap cannot express (e.g.
///    [`command::tag::validate_cli_args`]).
/// 6. For commands that operate on a repository, runs [`command_preflight_storage`]
///    and primes the global hash kind via [`set_local_hash_kind_for_storage`].
/// 7. Resolves the global output flags into a single [`OutputConfig`] and dispatches
///    to the matching `command::*::execute_safe` handler.
/// 8. After the command returns, waits for any background storage tasks (object
///    indexing, cache flushes) so they cannot be killed by process exit.
///
/// Boundary conditions:
/// - `--help` / `--version` are still rendered through clap so output matches user
///   expectations exactly; the function then returns `Ok(())` without dispatching.
/// - The `Init` arm explicitly restores the original CWD afterwards because the
///   handler may `cd` into a freshly-created repo and downstream callers (notably
///   the integration test suite and `--from-git-repository`) rely on the CWD being
///   stable across invocations.
/// - When `--exit-code-on-warning` is set and at least one warning was recorded, the
///   function returns a `CliError::failure` with stable code `WarningEmitted` even
///   though the underlying command succeeded.
pub async fn parse_async(args: Option<&[&str]>) -> CliResult<()> {
    let argv = match args {
        Some(args) => args.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        None => env::args().collect::<Vec<_>>(),
    };
    let argv = rewrite_log_short_number_args(argv);
    let argv = rewrite_index_pack_progress_args(argv);
    let argv = rewrite_reset_pathspec_separator_args(argv);
    prepare_cli_invocation_state();
    if is_error_codes_help_topic(&argv) {
        return print_error_codes_help();
    }
    let args = match Cli::try_parse_from(argv.clone()) {
        Ok(args) => args,
        Err(err) => match err.kind() {
            ErrorKind::DisplayHelp
            | ErrorKind::DisplayVersion
            | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                err.print().map_err(|print_err| {
                    CliError::fatal(format!("failed to write clap output: {print_err}"))
                })?;
                return Ok(());
            }
            _ => return Err(classify_parse_error(&argv, &err)),
        },
    };
    apply_global_runtime_flags(&args)?;
    if let Commands::Tag(tag_args) = &args.command {
        command::tag::validate_cli_args(tag_args)?;
    }
    let preflight = command_preflight(&args.command)?;
    if let Some(storage) = preflight.storage.as_deref() {
        if preflight.set_hash_kind {
            if preflight.upgrade_schema {
                // Opening the pooled connection here auto-applies any pending
                // schema migrations (see `db::establish_connection`), so an
                // older repository is brought up to date transparently.
                set_local_hash_kind_for_storage(storage).await?;
            } else {
                set_local_hash_kind_for_storage_without_schema_guard(storage).await?;
            }
        }
    } else if preflight.set_hash_kind {
        set_hash_kind(HashKind::Sha1);
    }
    // Resolve global output flags into a single config before dispatching.
    let color = if args.no_color {
        "never"
    } else {
        args.color.as_str()
    };
    let output = OutputConfig::resolve(
        args.json.as_deref(),
        args.machine,
        args.no_pager,
        color,
        args.quiet,
        args.exit_code_on_warning,
        &args.progress,
    );
    output.apply_color_override();

    // parse the command and execute the corresponding function with it's args
    match args.command {
        Commands::Init(cmd_args) => {
            let original_dir = utils::util::cur_dir();
            let init_target = if Path::new(&cmd_args.repo_directory).is_absolute() {
                Path::new(&cmd_args.repo_directory).to_path_buf()
            } else {
                original_dir.join(&cmd_args.repo_directory)
            };
            let storage = if cmd_args.bare {
                init_target
            } else {
                init_target.join(utils::util::ROOT_DIR)
            };

            command::init::execute_safe(cmd_args, &output).await?;
            set_local_hash_kind_for_storage(&storage).await?;
            #[cfg(test)]
            let _cwd_lock = crate::utils::test::cwd_lock_guard();
            env::set_current_dir(&original_dir).map_err(|e| {
                CliError::fatal(format!(
                    "failed to restore working directory '{}': {}",
                    original_dir.display(),
                    e
                ))
            })?;
        }
        Commands::Clone(cmd_args) => command::clone::execute_safe(cmd_args, &output).await?,
        Commands::Code(cmd_args) => command::code::execute(cmd_args, &output).await?,
        Commands::CodeControl(cmd_args) => command::code_control::execute(cmd_args).await?,
        Commands::Automation(cmd_args) => {
            command::automation::execute_safe(cmd_args, &output).await?
        }
        Commands::Usage(cmd_args) => command::usage::execute_safe(cmd_args, &output).await?,
        Commands::Graph(cmd_args) => command::graph::execute_safe(cmd_args, &output).await?,
        Commands::Sandbox(cmd_args) => command::sandbox::execute_safe(cmd_args, &output).await?,
        Commands::Add(cmd_args) => command::add::execute_safe(cmd_args, &output).await?,
        Commands::Rm(cmd_args) => command::remove::execute_safe(cmd_args, &output).await?,
        Commands::Restore(cmd_args) => command::restore::execute_safe(cmd_args, &output).await?,
        Commands::Status(cmd_args) => command::status::execute_safe(cmd_args, &output).await?,
        Commands::Clean(cmd_args) => command::clean::execute_safe(cmd_args, &output).await?,
        Commands::Stash(cmd) => command::stash::execute_safe(cmd, &output).await?,
        Commands::Lfs(cmd) => command::lfs::execute_safe(cmd, &output).await?,
        Commands::LsFiles(cmd_args) => command::ls_files::execute_safe(cmd_args, &output).await?,
        Commands::Log(cmd_args) => command::log::execute_safe(cmd_args, &output).await?,
        Commands::Logfile(cmd_args) => command::logfile::execute_safe(cmd_args, &output).await?,
        Commands::Cache(cmd_args) => command::cache::execute_safe(cmd_args, &output).await?,
        Commands::Layer(cmd_args) => command::layer::execute_safe(cmd_args, &output).await?,
        Commands::File(cmd_args) => command::file::execute_safe(cmd_args, &output).await?,
        Commands::Alternates(cmd_args) => {
            command::alternates::execute_safe(cmd_args, &output).await?
        }
        Commands::Deps(cmd_args) => command::deps::execute_safe(cmd_args, &output).await?,
        Commands::Hydrate(cmd_args) => command::hydrate::execute_safe(cmd_args, &output).await?,
        #[cfg(feature = "fastcdc")]
        Commands::Media(cmd_args) => command::media::execute_safe(cmd_args, &output).await?,
        Commands::SparseView(cmd_args) => {
            command::sparse_view::execute_safe(cmd_args, &output).await?
        }
        Commands::Metadata(cmd_args) => command::metadata::execute_safe(cmd_args, &output).await?,
        Commands::Dirty(cmd_args) => command::dirty::execute_safe(cmd_args, &output).await?,
        Commands::Auth(cmd_args) => command::auth::execute_safe(cmd_args, &output).await?,
        Commands::Login(cmd_args) => command::account::login(cmd_args, &output).await?,
        Commands::Whoami(cmd_args) => command::account::whoami(cmd_args, &output).await?,
        Commands::Logout(cmd_args) => command::account::logout(cmd_args, &output).await?,
        Commands::Revision(cmd_args) => command::revision::execute_safe(cmd_args, &output).await?,
        Commands::Service(cmd_args) => command::service::execute_safe(cmd_args, &output).await?,
        Commands::Shortlog(cmd_args) => command::shortlog::execute_safe(cmd_args, &output).await?,
        Commands::Show(cmd_args) => command::show::execute_safe(cmd_args, &output).await?,
        Commands::ShowRef(cmd_args) => command::show_ref::execute_safe(cmd_args, &output).await?,
        Commands::FormatPatch(cmd_args) => {
            command::format_patch::execute_safe(cmd_args, &output).await?
        }
        Commands::ForEachRef(cmd_args) => {
            command::for_each_ref::execute_safe(cmd_args, &output).await?
        }
        Commands::LsRemote(cmd_args) => command::ls_remote::execute_safe(cmd_args, &output).await?,
        Commands::LsTree(cmd_args) => command::ls_tree::execute_safe(cmd_args, &output).await?,
        Commands::SymbolicRef(cmd_args) => {
            command::symbolic_ref::execute_safe(cmd_args, &output).await?
        }
        Commands::Branch(cmd_args) => command::branch::execute_safe(cmd_args, &output).await?,
        Commands::Tag(cmd_args) => command::tag::execute_safe(cmd_args, &output).await?,
        Commands::Commit(cmd_args) => command::commit::execute_safe(cmd_args, &output).await?,
        Commands::Switch(cmd_args) => command::switch::execute_safe(cmd_args, &output).await?,
        Commands::Rebase(cmd_args) => command::rebase::execute_safe(cmd_args, &output).await?,
        Commands::Merge(cmd_args) => command::merge::execute_safe(cmd_args, &output).await?,
        Commands::MergeFile(cmd_args) => {
            command::merge_file::execute_safe(cmd_args, &output).await?
        }
        Commands::MergeBase(cmd_args) => {
            command::merge_base::execute_safe(cmd_args, &output).await?
        }
        Commands::Apply(cmd_args) => command::apply::execute_safe(cmd_args, &output).await?,
        Commands::DiffTree(cmd_args) => {
            command::diff_plumbing::execute_tree_safe(cmd_args, &output).await?
        }
        Commands::DiffIndex(cmd_args) => {
            command::diff_plumbing::execute_index_safe(cmd_args, &output).await?
        }
        Commands::DiffFiles(cmd_args) => {
            command::diff_plumbing::execute_files_safe(cmd_args, &output).await?
        }
        Commands::Credential(cmd_args) => {
            command::credential::execute_safe(cmd_args, &output).await?
        }
        Commands::Rerere(cmd_args) => command::rerere::execute_safe(cmd_args, &output).await?,
        Commands::Reset(cmd_args) => command::reset::execute_safe(cmd_args, &output).await?,
        Commands::RevParse(cmd_args) => command::rev_parse::execute_safe(cmd_args, &output).await?,
        Commands::RevList(cmd_args) => command::rev_list::execute_safe(cmd_args, &output).await?,
        Commands::Mv(cmd_args) => command::mv::execute_safe(cmd_args, &output).await?,
        Commands::Describe(cmd_args) => command::describe::execute_safe(cmd_args, &output).await?,
        Commands::Notes(cmd_args) => command::notes::execute_safe(cmd_args, &output, &argv).await?,
        Commands::CherryPick(cmd_args) => {
            command::cherry_pick::execute_safe(cmd_args, &output).await?
        }
        Commands::Push(cmd_args) => command::push::execute_safe(cmd_args, &output).await?,
        Commands::CatFile(cmd_args) => command::cat_file::execute_safe(cmd_args, &output).await?,
        Commands::CheckIgnore(cmd_args) => {
            command::check_ignore::execute_safe(cmd_args, &output).await?
        }
        Commands::CheckAttr(cmd_args) => {
            command::check_attr::execute_safe(cmd_args, &output).await?
        }
        Commands::CheckMailmap(cmd_args) => {
            command::check_mailmap::execute_safe(cmd_args, &output).await?
        }
        Commands::FastExport(cmd_args) => {
            command::fast_export::execute_safe(cmd_args, &output).await?
        }
        Commands::Bundle(cmd_args) => command::bundle::execute_safe(cmd_args, &output).await?,
        Commands::FastImport(cmd_args) => {
            command::fast_import::execute_safe(cmd_args, &output).await?
        }
        Commands::Completions(cmd_args) => {
            command::completions::execute_safe(cmd_args, Cli::command(), &output)?
        }
        Commands::WriteTree(cmd_args) => {
            command::write_tree::execute_safe(cmd_args, &output).await?
        }
        Commands::CommitTree(cmd_args) => {
            command::commit_tree::execute_safe(cmd_args, &output).await?
        }
        Commands::ReadTree(cmd_args) => command::read_tree::execute_safe(cmd_args, &output).await?,
        Commands::UpdateIndex(cmd_args) => {
            command::update_index::execute_safe(cmd_args, &output).await?
        }
        Commands::UpdateRef(cmd_args) => {
            command::update_ref::execute_safe(cmd_args, &output).await?
        }
        Commands::Archive(cmd_args) => command::archive::execute_safe(cmd_args, &output).await?,
        Commands::HashObject(cmd_args) => {
            command::hash_object::execute_safe(cmd_args, &output).await?
        }
        Commands::VerifyPack(cmd_args) => {
            command::verify_pack::execute_safe(cmd_args, &output).await?
        }
        Commands::IndexPack(cmd_args) => command::index_pack::execute_safe(cmd_args, &output)?,
        Commands::PackObjects(cmd_args) => {
            command::pack_objects::execute_safe(cmd_args, &output).await?
        }
        Commands::Fetch(cmd_args) => command::fetch::execute_safe(cmd_args, &output).await?,
        Commands::Fsck(cmd_args) => command::fsck::execute_safe(cmd_args, &output).await?,
        Commands::Maintenance(cmd_args) => {
            command::maintenance::execute_safe(cmd_args, &output).await?
        }
        Commands::Repack(cmd_args) => command::repack::execute_safe(cmd_args, &output).await?,
        Commands::Diff(cmd_args) => command::diff::execute_safe(cmd_args, &output).await?,
        Commands::Grep(cmd_args) => command::grep::execute_safe(cmd_args, &output).await?,
        Commands::Blame(cmd_args) => command::blame::execute_safe(cmd_args, &output).await?,
        Commands::Revert(cmd_args) => command::revert::execute_safe(cmd_args, &output).await?,
        Commands::Replace(cmd_args) => command::replace::execute_safe(cmd_args, &output).await?,
        Commands::Remote(cmd) => command::remote::execute_safe(cmd, &output).await?,
        Commands::Open(cmd_args) => command::open::execute_safe(cmd_args, &output).await?,
        Commands::Pull(cmd_args) => command::pull::execute_safe(cmd_args, &output).await?,
        Commands::Config(cmd_args) => command::config::execute_safe(cmd_args, &output).await?,
        Commands::Checkout(cmd_args) => command::checkout::execute_safe(cmd_args, &output).await?,
        Commands::Reflog(cmd_args) => command::reflog::execute_safe(cmd_args, &output).await?,
        Commands::Op(cmd_args) => command::op::execute_safe(cmd_args, &output).await?,
        Commands::Worktree(cmd_args) => command::worktree::execute_safe(cmd_args, &output).await?,
        Commands::Cloud(cmd_args) => command::cloud::execute_safe(cmd_args, &output).await?,
        Commands::Publish(cmd_args) => command::publish::execute_safe(cmd_args, &output).await?,
        Commands::Agent(cmd_args) => command::agent::execute_safe(cmd_args, &output).await?,
        Commands::Review(cmd_args) => {
            command::agent::review::execute_safe(cmd_args, &output).await?
        }
        Commands::Investigate(cmd_args) => {
            command::agent::investigate::execute_safe(cmd_args, &output).await?
        }
        Commands::Hooks(cmd_args) => command::hooks::execute_safe(cmd_args, &output).await?,
        Commands::Bisect(bisect_cmd) => command::bisect::execute_safe(bisect_cmd, &output).await?,
    }

    // Check for warnings when --exit-code-on-warning is active.
    if output.exit_code_on_warning && utils::output::warning_was_emitted() {
        return Err(CliError::failure("command completed with warnings")
            .with_stable_code(utils::error::StableErrorCode::WarningEmitted));
    }

    // Wait for any background storage tasks (e.g. object indexing) to complete
    // This prevents tasks from being killed when the process exits
    let _ = tokio::task::spawn_blocking(|| {
        utils::client_storage::ClientStorage::wait_for_background_tasks();
    })
    .await;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serial_test::serial;

    use super::*;
    use crate::utils::{output, test};

    fn apply_runtime_flags_for_test(argv: &[&str]) -> CliResult<()> {
        prepare_cli_invocation_state();
        let cli = Cli::try_parse_from(argv).unwrap();
        apply_global_runtime_flags(&cli)
    }

    /// Scenario: running `libra` with no arguments should show usage information without
    /// an `error:` prefix, matching the behaviour of `git` and other standard tools.
    /// The underlying `arg_required_else_help = true` flag triggers clap's
    /// `DisplayHelpOnMissingArgumentOrSubcommand` path, which we treat the same as
    /// `DisplayHelp` — i.e. print and return `Ok(())`.
    #[test]
    fn no_subcommand_shows_help_without_error_prefix() {
        let err = Cli::try_parse_from(["libra"]).unwrap_err();
        assert_eq!(
            err.kind(),
            ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        );
    }

    /// Scenario: clap's `debug_assert` walks the entire command tree and panics on any
    /// structural mistake (duplicate flags, conflicting aliases, malformed value
    /// parsers). This test is the cheapest way to keep the giant `Commands` enum
    /// honest as new subcommands are added.
    /// See: <https://docs.rs/clap/latest/clap/_derive/_tutorial/chapter_4/index.html>
    #[test]
    fn verify_cli() {
        use clap::CommandFactory;

        Cli::command().debug_assert()
    }

    /// Scenario: `libra import` is intentionally not a subcommand because importing
    /// is exposed via `libra config --import`. This test guards the redirect hint
    /// emitted by [`parse_error_hints`] / [`REDIRECTED_COMMANDS`] so users typing the
    /// natural-but-wrong word are pointed at the real flag.
    #[tokio::test]
    async fn parse_error_shows_import_hint() {
        let argv = vec!["libra".to_string(), "import".to_string()];
        let clap_err = Cli::try_parse_from(argv.clone()).unwrap_err();
        let err = classify_parse_error(&argv, &clap_err);
        let msg = err.render();
        assert!(
            msg.contains("You probably want `libra config --import`."),
            "got: {msg}"
        );
    }

    /// Scenario: the `branch` command advertises a `br` alias for ergonomics. This
    /// test ensures the alias keeps resolving even after the `Commands` enum is
    /// reordered or extended.
    #[test]
    fn clap_alias_br_resolves_to_branch() {
        let cli = Cli::try_parse_from(["libra", "br"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Branch(_)),
            "`br` should parse as the branch subcommand"
        );
    }

    /// Scenario: the `config` command advertises a `cfg` alias. Mirrors
    /// [`clap_alias_br_resolves_to_branch`] for the second alias that tends to break
    /// when the subcommand list is touched.
    #[test]
    fn clap_alias_cfg_resolves_to_config() {
        let cli = Cli::try_parse_from(["libra", "cfg"]).unwrap();
        assert!(
            matches!(cli.command, Commands::Config(_)),
            "`cfg` should parse as the config subcommand"
        );
    }

    #[test]
    fn index_pack_progress_rewrite_keeps_pack_positional() {
        let rewritten = rewrite_index_pack_progress_args(vec![
            "libra".to_string(),
            "index-pack".to_string(),
            "--progress".to_string(),
            "fixture.pack".to_string(),
        ]);

        assert_eq!(
            rewritten,
            vec!["libra", "index-pack", "--progress=text", "fixture.pack"]
        );
    }

    #[test]
    fn index_pack_no_progress_rewrite_uses_global_none_mode() {
        let rewritten = rewrite_index_pack_progress_args(vec![
            "libra".to_string(),
            "--progress".to_string(),
            "none".to_string(),
            "index-pack".to_string(),
            "--no-progress".to_string(),
            "fixture.pack".to_string(),
        ]);

        assert_eq!(
            rewritten,
            vec![
                "libra",
                "--progress",
                "none",
                "index-pack",
                "--progress=none",
                "fixture.pack"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn worktree_umount_preflight_does_not_require_repo() {
        let cli = Cli::try_parse_from(["libra", "worktree", "umount", "/tmp/libra-task"]).unwrap();

        let preflight = command_preflight(&cli.command).unwrap();
        assert!(preflight.storage.is_none());
        assert!(!preflight.upgrade_schema);
        assert!(!preflight.set_hash_kind);
    }

    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn hash_object_read_only_preflight_skips_schema_guard() {
        let repo = tempfile::tempdir().expect("failed to create test repo");
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());
        let cli = Cli::try_parse_from(["libra", "hash-object", "hello.txt"]).unwrap();

        let preflight = command_preflight(&cli.command).unwrap();
        assert!(preflight.storage.is_some());
        assert!(!preflight.upgrade_schema);
        assert!(preflight.set_hash_kind);
    }

    /// Scenario: every visible command in [`Commands`] must appear in the
    /// `Command Groups:` section of `ROOT_AFTER_HELP`. Hidden commands
    /// (e.g. `index-pack`, `hooks`) are intentionally excluded. This
    /// guards against new visible commands being added without an
    /// accompanying group entry, which would make them invisible in
    /// scenario-grouped `libra --help` output even though they remain
    /// callable.
    #[test]
    fn root_after_help_lists_every_visible_command() {
        use clap::CommandFactory;

        // Curated allowlist of hidden commands (mirrors `hide = true`
        // attributes on `Commands::*` variants in this file).
        const HIDDEN_COMMANDS: &[&str] = &["index-pack", "hooks", "pack-objects"];

        let cli = Cli::command();
        for subcommand in cli.get_subcommands() {
            let name = subcommand.get_name();
            if HIDDEN_COMMANDS.contains(&name) || subcommand.is_hide_set() {
                continue;
            }
            // `--help` is registered as an alias; skip it.
            if name == "help" {
                continue;
            }
            assert!(
                ROOT_AFTER_HELP.contains(name),
                "ROOT_AFTER_HELP must list every visible command in some \
                 'Command Groups:' row; missing: `{name}`. Either add it to \
                 the appropriate group in src/cli.rs:ROOT_AFTER_HELP or, if \
                 it should be hidden, mark it `hide = true` and add it to \
                 HIDDEN_COMMANDS in this test."
            );
        }
    }

    /// Scenario: clap's built-in Levenshtein matcher should suggest `init` for the
    /// typo `initt`. We accept either "Hint:" (Libra-formatted) or "similar"
    /// (clap-formatted) so the test survives clap upgrades that re-word the message.
    #[tokio::test]
    async fn clap_fuzzy_suggests_similar_command() {
        // "initt" is close enough to "init" for clap's built-in fuzzy match.
        let argv = vec!["libra".to_string(), "initt".to_string()];
        let clap_err = Cli::try_parse_from(argv.clone()).unwrap_err();
        let err = classify_parse_error(&argv, &clap_err);
        let msg = err.render();
        // Clap should include its own "tip: a similar subcommand exists: 'init'".
        assert!(
            msg.contains("Hint:") || msg.contains("similar"),
            "expected clap fuzzy-match suggestion, got: {msg}"
        );
    }

    /// Scenario: the warning tracker is a process-global static. In long-lived
    /// processes (TUI, tests) a previously-recorded warning would otherwise leak
    /// into the next invocation and silently flip the exit code under
    /// `--exit-code-on-warning`. This test seeds a stale warning, then verifies that
    /// [`prepare_cli_invocation_state`] clears it before dispatch.
    #[test]
    #[serial]
    fn parse_async_resets_warning_tracker_before_dispatch() {
        output::record_warning();
        assert!(output::warning_was_emitted());

        prepare_cli_invocation_state();

        assert!(
            !output::warning_was_emitted(),
            "top-level CLI dispatch should clear stale warning state before running"
        );
    }

    /// Scenario: the global `--sync-data` flag (lore.md §0.5) must actually flip
    /// the process-global durability hook, from either flag placement, and a run
    /// without it (env disabled) must leave the hook off. Uses `logfile info`
    /// as a benign, repo-free command that still runs the post-parse flag
    /// override before dispatch.
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn sync_data_flag_enables_durability_hook() {
        use crate::utils::atomic_write::{set_sync_data, sync_data_enabled};

        // Pin the env baseline off so we measure the flag's effect, not ambient
        // LIBRA_SYNC_DATA.
        let _env = test::ScopedEnvVar::set("LIBRA_SYNC_DATA", "0");

        // No flag: `parse_async` re-inits the hook from env (0), leaving it off
        // even though we seed the opposite here.
        set_sync_data(true);
        apply_runtime_flags_for_test(&["libra", "logfile", "info"]).unwrap();
        assert!(
            !sync_data_enabled(),
            "no --sync-data (env 0) should leave object fsync off"
        );

        // Both global placements enable it.
        for argv in [
            &["libra", "--sync-data", "logfile", "info"][..],
            &["libra", "logfile", "info", "--sync-data"][..],
        ] {
            set_sync_data(false);
            apply_runtime_flags_for_test(argv).unwrap();
            assert!(
                sync_data_enabled(),
                "--sync-data ({argv:?}) should enable object fsync"
            );
        }

        set_sync_data(false);
    }

    /// Scenario: the read policy (lore.md §0.8) resolves from `--offline` (→
    /// LocalOnly, overriding env) and `LIBRA_READ_POLICY` (baseline), and a run
    /// with neither resets to Auto.
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn read_policy_resolves_from_flag_and_env() {
        use crate::utils::read_policy::{ReadPolicy, read_policy, set_read_policy};

        // No meaningful env (auto), no flag → Auto (also proves reset from a
        // stale value).
        {
            let _env = test::ScopedEnvVar::set("LIBRA_READ_POLICY", "auto");
            set_read_policy(ReadPolicy::LocalOnly);
            apply_runtime_flags_for_test(&["libra", "logfile", "info"]).unwrap();
            assert_eq!(
                read_policy(),
                ReadPolicy::Auto,
                "no flag/env resets to Auto"
            );

            // `--offline` → LocalOnly.
            set_read_policy(ReadPolicy::Auto);
            apply_runtime_flags_for_test(&["libra", "--offline", "logfile", "info"]).unwrap();
            assert_eq!(
                read_policy(),
                ReadPolicy::LocalOnly,
                "--offline → LocalOnly"
            );
        }

        // `LIBRA_READ_POLICY=remote` (no flag) → Remote.
        {
            let _env = test::ScopedEnvVar::set("LIBRA_READ_POLICY", "remote");
            set_read_policy(ReadPolicy::Auto);
            apply_runtime_flags_for_test(&["libra", "logfile", "info"]).unwrap();
            assert_eq!(read_policy(), ReadPolicy::Remote, "env remote → Remote");

            // `--offline` overrides the env baseline.
            set_read_policy(ReadPolicy::Auto);
            apply_runtime_flags_for_test(&["libra", "--offline", "logfile", "info"]).unwrap();
            assert_eq!(
                read_policy(),
                ReadPolicy::LocalOnly,
                "--offline overrides env remote"
            );
        }

        // A typo'd LIBRA_READ_POLICY is a hard error (must not silently be Auto).
        {
            let _env = test::ScopedEnvVar::set("LIBRA_READ_POLICY", "offilne");
            set_read_policy(ReadPolicy::Auto);
            assert!(
                apply_runtime_flags_for_test(&["libra", "logfile", "info"]).is_err(),
                "an invalid LIBRA_READ_POLICY must be a usage error"
            );
        }

        set_read_policy(ReadPolicy::Auto);
    }

    /// Scenario: `--max-connections` (lore.md §0.9) resolves flag > env >
    /// default, always resets, and rejects an invalid env value.
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn max_connections_resolves_from_flag_and_env() {
        use crate::utils::resource_limits::{
            DEFAULT_MAX_CONNECTIONS, max_connections, set_max_connections,
        };

        // No flag / no env → default (also proves reset from a stale value).
        {
            let _env = test::ScopedEnvVar::set("LIBRA_MAX_CONNECTIONS", "");
            set_max_connections(3);
            apply_runtime_flags_for_test(&["libra", "logfile", "info"]).unwrap();
            assert_eq!(
                max_connections(),
                DEFAULT_MAX_CONNECTIONS,
                "reset to default"
            );

            // Flag wins.
            apply_runtime_flags_for_test(&["libra", "--max-connections", "5", "logfile", "info"])
                .unwrap();
            assert_eq!(max_connections(), 5, "--max-connections wins");
        }

        // Env baseline (no flag).
        {
            let _env = test::ScopedEnvVar::set("LIBRA_MAX_CONNECTIONS", "9");
            apply_runtime_flags_for_test(&["libra", "logfile", "info"]).unwrap();
            assert_eq!(max_connections(), 9, "env baseline");

            // Flag overrides env.
            apply_runtime_flags_for_test(&["libra", "--max-connections", "2", "logfile", "info"])
                .unwrap();
            assert_eq!(max_connections(), 2, "flag overrides env");
        }

        // Invalid env → usage error.
        {
            let _env = test::ScopedEnvVar::set("LIBRA_MAX_CONNECTIONS", "bogus");
            assert!(
                apply_runtime_flags_for_test(&["libra", "logfile", "info"]).is_err(),
                "invalid LIBRA_MAX_CONNECTIONS must error"
            );
        }

        set_max_connections(DEFAULT_MAX_CONNECTIONS);
    }

    /// Scenario: `libra code --repo <path>` should perform repository preflight
    /// against `<path>`, *not* the process CWD. The test arranges for the CWD to be
    /// outside any repo, sets `--repo` to a freshly-initialised one, and confirms
    /// preflight resolves that repository instead of reporting "not a libra
    /// repository" from the process CWD. This guards a regression where preflight
    /// was hitting CWD before honoring `--repo`.
    #[tokio::test(flavor = "current_thread")]
    #[serial]
    async fn code_repo_flag_uses_target_repo_during_preflight() {
        let root = tempfile::tempdir().expect("failed to create test root");
        let repo = root.path().join("linked");
        let outside = root.path().join("outside");
        fs::create_dir_all(&repo).expect("failed to create repo dir");
        fs::create_dir_all(&outside).expect("failed to create outside dir");
        test::setup_with_new_libra_in(&repo).await;

        let _guard = test::ChangeDirGuard::new(&outside);
        let repo_arg = repo
            .to_str()
            .expect("temporary repo path should be valid UTF-8");
        let cli = Cli::try_parse_from(["libra", "code", "--repo", repo_arg]).unwrap();
        let preflight = command_preflight(&cli.command).expect("--repo should drive preflight");

        let expected_storage = repo
            .join(".libra")
            .canonicalize()
            .expect("test repository storage should exist");
        assert_eq!(
            preflight.storage.as_deref(),
            Some(expected_storage.as_path())
        );
        assert!(preflight.upgrade_schema);
        assert!(preflight.set_hash_kind);
    }

    /// Scenario: `libra help error-codes` (and its `errors` alias) should bypass
    /// clap and stream the bundled error-code reference. Tests cover the two valid
    /// spellings plus two negative cases — a different `help <topic>` and the global
    /// `--help` flag — so the matcher in [`is_error_codes_help_topic`] stays tight
    /// enough that we don't accidentally swallow other help requests.
    #[test]
    fn detects_help_error_codes_topic() {
        assert!(is_error_codes_help_topic(&[
            "libra".to_string(),
            "help".to_string(),
            "error-codes".to_string(),
        ]));
        assert!(is_error_codes_help_topic(&[
            "libra".to_string(),
            "help".to_string(),
            "errors".to_string(),
        ]));
        assert!(!is_error_codes_help_topic(&[
            "libra".to_string(),
            "help".to_string(),
            "status".to_string(),
        ]));
        assert!(!is_error_codes_help_topic(&[
            "libra".to_string(),
            "--help".to_string(),
        ]));
    }

    /// Scenario (Unix): paths embedded in conversion-hint messages must be
    /// shell-safe. POSIX shells require `'...'` quoting with `'\'\''` escapes for
    /// embedded single quotes; this test pins that rule using a path containing an
    /// apostrophe, the canonical breakage case.
    #[cfg(not(windows))]
    #[test]
    fn shell_quote_path_uses_posix_single_quote_escaping() {
        assert_eq!(
            shell_quote_path(Path::new("repo's path")),
            "'repo'\"'\"'s path'"
        );
    }

    /// Scenario (Windows): cmd.exe and PowerShell expect double-quoted paths and
    /// tolerate spaces inside them. This test pins the simpler Windows behaviour and
    /// exists as a sibling to the POSIX test so both platforms have explicit
    /// coverage when [`shell_quote_path`] is touched.
    #[cfg(windows)]
    #[test]
    fn shell_quote_path_uses_windows_double_quotes() {
        assert_eq!(
            shell_quote_path(Path::new(r"C:\Program Files\repo")),
            r#""C:\Program Files\repo""#
        );
    }
}
