//! `libra update-index` — modify the index directly, a subset of
//! `git update-index`. Companion to `write-tree`: `--cacheinfo` registers an
//! entry from a `(mode, object, path)` triple without reading the working tree,
//! so an index can be built purely from objects.

use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
};

use clap::Parser;
use git_internal::{
    hash::{ObjectHash, get_hash_kind},
    internal::{
        index::{Index, IndexEntry},
        object::blob::Blob,
    },
};
use serde::Serialize;

use crate::utils::{
    error::{CliError, CliResult, StableErrorCode},
    lfs,
    object_ext::BlobExt,
    output::{OutputConfig, emit_json_data},
    path, util,
};

/// `--help` examples (cross-cutting EXAMPLES contract, `_general.md`).
pub const UPDATE_INDEX_EXAMPLES: &str = "\
EXAMPLES:
    libra update-index --add a.txt b.txt        Stage files from the working tree
    libra update-index --remove old.txt         Drop a path from the index
    libra update-index --cacheinfo 100644,<oid>,dir/f.txt
                                                 Register an entry directly from an object id
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

    /// Register an index entry directly from `<mode>,<object>,<path>` without
    /// reading the working tree (the object need not exist yet). Repeatable.
    /// `<mode>` is an octal file mode (`100644`, `100755`, `120000`, `160000`).
    #[clap(long, value_name = "<mode>,<object>,<path>")]
    pub cacheinfo: Vec<String>,

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

/// Safe entry point. Applies `--cacheinfo`, then the positional add/remove
/// operations, then saves the index. Usage/repository errors exit 128.
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

    let mut updated = 0usize;
    let mut removed = 0usize;

    // `--cacheinfo <mode>,<object>,<path>`: register entries directly.
    for spec in &args.cacheinfo {
        let entry = parse_cacheinfo(spec).map_err(usage)?;
        index.update(entry);
        updated += 1;
    }

    // Positional paths: remove, or (re)stage from the working tree.
    let workdir = util::working_dir();
    for path_str in &args.paths {
        if args.remove {
            if index.remove(path_str, 0).is_some() {
                removed += 1;
            }
            continue;
        }

        let tracked = index.tracked(path_str, 0);
        if !tracked && !args.add {
            return Err(usage(format!(
                "cannot add '{path_str}' to the index without --add (it is not already tracked)"
            )));
        }

        let absolute = resolve_within_worktree(path_str, &workdir).map_err(usage)?;
        let entry = stage_working_tree_path(path_str, &absolute, &workdir)?;
        index.update(entry);
        updated += 1;
    }

    index.save(&index_path).map_err(|error| {
        CliError::fatal(format!("failed to save index: {error}"))
            .with_stable_code(StableErrorCode::RepoStateInvalid)
    })?;

    if output.is_json() {
        emit_json_data(
            "update-index",
            &UpdateIndexOutput { updated, removed },
            output,
        )
    } else {
        Ok(())
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

    let hash = ObjectHash::from_str(oid_str)
        .map_err(|_| format!("invalid object id '{oid_str}' in --cacheinfo"))?;
    let expected_len = get_hash_kind().hex_len();
    if oid_str.len() != expected_len {
        return Err(format!(
            "object id '{oid_str}' does not match the repository hash format (expected {expected_len} hex chars)"
        ));
    }

    // The path is an index key; reject absolute paths (POSIX `/...` and Windows
    // `C:\...`) and `..` traversal so an entry can never escape the worktree.
    if path_str.is_empty() {
        return Err("empty path in --cacheinfo".to_string());
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
            "invalid path '{path_str}' in --cacheinfo (absolute or `..` traversal not allowed)"
        ));
    }

    let mut entry = IndexEntry::new_from_blob(normalized, hash, 0);
    entry.mode = mode;
    Ok(entry)
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
/// (LFS-aware); an unreadable file is surfaced as a 128 error.
fn stage_working_tree_path(
    path_str: &str,
    absolute: &Path,
    workdir: &Path,
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
        let blob = Blob::from_content_bytes(target.to_string_lossy().as_bytes().to_vec());
        blob.save();
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

    let blob = if lfs::is_lfs_tracked(absolute) {
        Blob::from_lfs_file(absolute)
    } else {
        Blob::from_file(absolute)
    };
    blob.save();
    IndexEntry::new_from_file(Path::new(path_str), blob.id, workdir).map_err(|error| {
        CliError::fatal(format!("failed to stage '{path_str}': {error}"))
            .with_exit_code(128)
            .with_stable_code(StableErrorCode::IoReadFailed)
    })
}
