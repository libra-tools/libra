//! Implements `ls-tree` to list entries from a Git tree object.

use std::{
    collections::BTreeSet,
    io::{self, Write},
    path::{Component, Path},
};

use clap::Parser;
use git_internal::{
    hash::ObjectHash,
    internal::object::{
        commit::Commit,
        tree::{Tree, TreeItemMode},
        types::ObjectType,
    },
};
use serde::Serialize;

use crate::{
    command::load_object,
    utils::{
        client_storage::ClientStorage,
        error::{CliError, CliResult, StableErrorCode},
        output::{OutputConfig, emit_json_data},
        util::{self, CommitBaseError},
    },
};

/// `--help` examples shown in `libra ls-tree --help` output.
pub const LS_TREE_EXAMPLES: &str = "\
EXAMPLES:
    libra ls-tree HEAD                         List entries in HEAD's root tree
    libra ls-tree HEAD:src                     List the src subtree via REV:path
    libra ls-tree -r HEAD src                  Recursively list entries under src
    libra ls-tree -l HEAD README.md            Include blob sizes
    libra ls-tree --name-only HEAD src/        Print paths only
    libra ls-tree --full-name HEAD             Print repository-relative paths
    libra ls-tree --full-tree HEAD             List from the repository root
    libra ls-tree --object-only --abbrev HEAD  Print abbreviated object IDs only
    libra ls-tree -z HEAD                      Use NUL-terminated records for scripts
    libra ls-tree --json HEAD                  Structured JSON output for agents";

#[derive(Parser, Debug)]
#[command(after_help = LS_TREE_EXAMPLES)]
pub struct LsTreeArgs {
    /// Recurse into subtrees.
    #[arg(short = 'r', long = "recursive")]
    pub recursive: bool,

    /// Show tree entries when recursing, and the trees traversed to reach a path argument.
    #[arg(short = 't')]
    pub show_trees: bool,

    /// Show only matching tree entries, not their children.
    #[arg(short = 'd')]
    pub tree_only: bool,

    /// Show object size for blob entries.
    #[arg(short = 'l', long = "long")]
    pub long: bool,

    /// Terminate output records with NUL instead of newline.
    #[arg(short = 'z')]
    pub nul: bool,

    /// Print only entry paths.
    #[arg(long = "name-only", conflicts_with_all = ["name_status", "object_only"])]
    pub name_only: bool,

    /// Print only entry paths; accepted as a Git-compatible alias.
    #[arg(long = "name-status", conflicts_with_all = ["name_only", "object_only"])]
    pub name_status: bool,

    /// Print only object IDs.
    #[arg(long = "object-only", conflicts_with_all = ["name_only", "name_status"])]
    pub object_only: bool,

    /// Render each entry with a custom format string. Supports the atoms
    /// `%(objectmode)`, `%(objecttype)`, `%(objectname)`, `%(objectsize)`,
    /// `%(objectsize:padded)`, `%(path)`, and the escapes `%x09` (tab) / `%x0a` (newline).
    #[arg(long = "format", value_name = "FORMAT", conflicts_with_all = ["name_only", "name_status", "object_only", "long"])]
    pub format: Option<String>,

    /// Print paths relative to the repository root when invoked from a subdirectory.
    #[arg(long = "full-name")]
    pub full_name: bool,

    /// List from the repository root and interpret paths relative to the repository root.
    #[arg(long = "full-tree")]
    pub full_tree: bool,

    /// Abbreviate object IDs to N hex characters, or 7 when no value is supplied.
    #[arg(
        long = "abbrev",
        value_name = "N",
        num_args = 0..=1,
        require_equals = true,
        default_missing_value = "7"
    )]
    pub abbrev: Option<usize>,

    /// Commit, tag, branch, HEAD, or tree object hash to inspect.
    #[arg(value_name = "TREE-ISH")]
    pub treeish: String,

    /// Optional path prefixes to list; relative to the current directory unless --full-tree is set.
    #[arg(value_name = "PATH")]
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
struct LsTreeOutput {
    treeish: String,
    root_tree: String,
    recursive: bool,
    entries: Vec<LsTreeEntry>,
}

#[derive(Debug, Clone, Serialize)]
struct LsTreeEntry {
    mode: String,
    object_type: String,
    object: String,
    path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<usize>,
}

#[derive(Debug, Clone)]
struct RawLsTreeEntry {
    mode: TreeItemMode,
    object_id: ObjectHash,
    path: String,
}

#[derive(Debug)]
struct LsTreePathFilter {
    path: String,
    list_contents: bool,
}

#[derive(Debug, Clone)]
struct LsTreePathContext {
    current_dir_prefix: String,
    display_repository_paths: bool,
    root_relative_filters: bool,
}

pub async fn execute(args: LsTreeArgs) -> Result<(), String> {
    execute_safe(args, &OutputConfig::default())
        .await
        .map_err(|err| err.render())
}

/// # Side Effects
///
/// Reads commit/tree/blob objects from the current repository object database and
/// writes a listing to stdout. It never mutates refs, the index, the worktree, or
/// object storage.
///
/// # Errors
///
/// Returns structured CLI errors for `REV:path` targets that are not trees,
/// invalid revisions, corrupt tree objects, path filters that do not match, and
/// stdout write failures.
pub async fn execute_safe(args: LsTreeArgs, output: &OutputConfig) -> CliResult<()> {
    util::require_repo().map_err(|_| CliError::repo_not_found())?;
    let include_size = args.long
        || (!output.is_json()
            && args.format.as_deref().is_some_and(|format| {
                format.contains("%(objectsize)") || format.contains("%(objectsize:padded)")
            }));
    let result = resolve_ls_tree(&args, include_size).await?;

    if output.is_json() {
        emit_json_data("ls-tree", &result, output)
    } else if output.quiet {
        Ok(())
    } else {
        let stdout = io::stdout();
        let mut writer = stdout.lock();
        write_ls_tree_output(&mut writer, &result.entries, &args)
    }
}

async fn resolve_ls_tree(args: &LsTreeArgs, include_size: bool) -> CliResult<LsTreeOutput> {
    validate_args(args)?;
    let storage = util::objects_storage()?;
    let root_tree = resolve_treeish_to_tree(&args.treeish, &storage).await?;
    let path_context = LsTreePathContext::from_args(args)?;
    let filters = normalize_path_filters(&args.paths, &path_context)?;
    let raw_entries = collect_matching_entries(root_tree, &filters, args, &path_context)?;
    let entries = raw_entries
        .into_iter()
        .map(|entry| format_entry(entry, args.abbrev, include_size, &storage, &path_context))
        .collect::<CliResult<Vec<_>>>()?;

    Ok(LsTreeOutput {
        treeish: args.treeish.clone(),
        root_tree: root_tree.to_string(),
        recursive: args.recursive,
        entries,
    })
}

fn validate_args(args: &LsTreeArgs) -> CliResult<()> {
    if let Some(width) = args.abbrev
        && width < 4
    {
        return Err(CliError::command_usage(format!(
            "--abbrev must be at least 4 characters, got {width}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidArguments));
    }

    Ok(())
}

async fn resolve_treeish_to_tree(treeish: &str, storage: &ClientStorage) -> CliResult<ObjectHash> {
    // `REV:path` resolves REV's root tree and then navigates to the (sub)tree at
    // `path`, matching `git ls-tree REV:path`. The first `:` is the separator;
    // an empty path (`REV:`) lists REV's root tree.
    if let Some((rev, path)) = treeish.split_once(':') {
        let root = resolve_plain_treeish_to_tree(rev, storage).await?;
        if path.is_empty() {
            return Ok(root);
        }
        let (entry, subtree_id) = find_tree_entry(root, path)?;
        if entry.mode != TreeItemMode::Tree {
            return Err(CliError::failure(format!("not a tree object: '{treeish}'"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("REV:path must name a directory (tree); use `cat-file` for blobs."));
        }
        return Ok(subtree_id);
    }
    resolve_plain_treeish_to_tree(treeish, storage).await
}

async fn resolve_plain_treeish_to_tree(
    treeish: &str,
    storage: &ClientStorage,
) -> CliResult<ObjectHash> {
    match util::get_commit_base_typed(treeish).await {
        Ok(commit_id) => {
            let commit: Commit = load_object(&commit_id).map_err(|error| {
                CliError::fatal(format!(
                    "failed to read commit '{treeish}' ({commit_id}): {error}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            return Ok(commit.tree_id);
        }
        Err(CommitBaseError::InvalidReference(_)) => {}
        Err(error) => return Err(treeish_resolution_error(treeish, error)),
    }

    let matches = storage.search_result(treeish).await.map_err(|error| {
        CliError::fatal(format!("failed to search objects for '{treeish}': {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })?;

    match matches.as_slice() {
        [] => Err(
            CliError::failure(format!("not a valid tree-ish: '{treeish}'"))
                .with_stable_code(StableErrorCode::CliInvalidTarget),
        ),
        [object_id] => resolve_direct_object_to_tree(treeish, *object_id, storage),
        _ => Err(
            CliError::failure(format!("ambiguous tree-ish: '{treeish}'"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
                .with_hint("use a longer object ID prefix."),
        ),
    }
}

fn resolve_direct_object_to_tree(
    treeish: &str,
    object_id: ObjectHash,
    storage: &ClientStorage,
) -> CliResult<ObjectHash> {
    match storage.get_object_type(&object_id).map_err(|error| {
        CliError::fatal(format!(
            "failed to inspect object '{treeish}' ({object_id}): {error}"
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })? {
        ObjectType::Tree => Ok(object_id),
        ObjectType::Commit => {
            let commit: Commit = load_object(&object_id).map_err(|error| {
                CliError::fatal(format!(
                    "failed to read commit '{treeish}' ({object_id}): {error}"
                ))
                .with_stable_code(StableErrorCode::IoReadFailed)
            })?;
            Ok(commit.tree_id)
        }
        other => Err(CliError::failure(format!(
            "object '{treeish}' ({object_id}) is not a tree-ish; found {other}"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)),
    }
}

fn treeish_resolution_error(treeish: &str, error: CommitBaseError) -> CliError {
    match error {
        CommitBaseError::HeadUnborn => CliError::failure(format!(
            "not a valid tree-ish: '{treeish}' (HEAD does not point to a commit)"
        ))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
        .with_hint("create a commit before resolving HEAD."),
        CommitBaseError::InvalidReference(detail) => {
            CliError::failure(format!("not a valid tree-ish: '{treeish}' ({detail})"))
                .with_stable_code(StableErrorCode::CliInvalidTarget)
        }
        CommitBaseError::Ambiguous { name, candidates } => {
            util::ambiguous_short_oid_error(&name, &candidates)
        }
        CommitBaseError::ReadFailure(detail) => {
            CliError::fatal(format!("failed to resolve '{treeish}': {detail}"))
                .with_stable_code(StableErrorCode::IoReadFailed)
        }
        CommitBaseError::CorruptReference(detail) => {
            CliError::fatal(format!("failed to resolve '{treeish}': {detail}"))
                .with_stable_code(StableErrorCode::RepoCorrupt)
        }
    }
}

impl LsTreePathContext {
    fn from_args(args: &LsTreeArgs) -> CliResult<Self> {
        let current_dir_prefix = if args.full_tree {
            String::new()
        } else {
            current_directory_prefix()?
        };

        Ok(Self {
            current_dir_prefix,
            display_repository_paths: args.full_name || args.full_tree,
            root_relative_filters: args.full_tree,
        })
    }
}

fn current_directory_prefix() -> CliResult<String> {
    let workdir = util::try_working_dir().map_err(|error| {
        CliError::fatal(format!("failed to locate repository root: {error}"))
            .with_stable_code(StableErrorCode::RepoCorrupt)
    })?;
    let cwd = util::cur_dir();
    let relative = pathdiff::diff_paths(&cwd, &workdir).ok_or_else(|| {
        CliError::fatal(format!(
            "failed to resolve current directory '{}' relative to repository root '{}'",
            cwd.display(),
            workdir.display()
        ))
        .with_stable_code(StableErrorCode::IoReadFailed)
    })?;
    normalize_workdir_prefix(&relative)
}

fn normalize_workdir_prefix(path: &Path) -> CliResult<String> {
    let mut components = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(part) => {
                let Some(part) = part.to_str() else {
                    return Err(CliError::command_usage(
                        "current directory contains non-UTF-8 path components",
                    )
                    .with_stable_code(StableErrorCode::CliInvalidArguments));
                };
                components.push(part.to_string());
            }
            Component::ParentDir => {
                return Err(CliError::command_usage(
                    "current directory is outside the repository worktree",
                )
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(CliError::fatal(format!(
                    "failed to normalize current directory path '{}'",
                    path.display()
                ))
                .with_stable_code(StableErrorCode::IoReadFailed));
            }
        }
    }
    Ok(components.join("/"))
}

fn normalize_path_filters(
    paths: &[String],
    path_context: &LsTreePathContext,
) -> CliResult<Vec<LsTreePathFilter>> {
    let mut filters = Vec::new();
    let mut traversed = BTreeSet::new();
    for path in paths {
        let normalized = normalize_one_path_filter_with_context(path, path_context)?;
        // Preserve the final slash/dot before normalization removes it. A
        // directory also expands when another argument traverses through it.
        let list_contents = path.ends_with('/') || path.rsplit('/').next() == Some(".");
        for (index, _) in normalized.match_indices('/') {
            traversed.insert(normalized[..index].to_string());
        }
        if list_contents {
            traversed.insert(normalized.clone());
        }
        filters.push(LsTreePathFilter {
            path: normalized,
            list_contents,
        });
    }
    for filter in &mut filters {
        filter.list_contents |= traversed.contains(&filter.path);
    }
    Ok(filters)
}

#[cfg(test)]
fn normalize_one_path_filter(path: &str) -> CliResult<String> {
    normalize_relative_path_filter(path)
}

fn normalize_one_path_filter_with_context(
    path: &str,
    path_context: &LsTreePathContext,
) -> CliResult<String> {
    let normalized = normalize_relative_path_filter(path)?;
    if path_context.root_relative_filters || path_context.current_dir_prefix.is_empty() {
        return Ok(normalized);
    }
    if normalized.is_empty() {
        return Ok(path_context.current_dir_prefix.clone());
    }
    Ok(join_tree_path(
        &path_context.current_dir_prefix,
        &normalized,
    ))
}

fn normalize_relative_path_filter(path: &str) -> CliResult<String> {
    if path.starts_with('/') {
        return Err(
            CliError::command_usage(format!("path filter '{path}' must be relative"))
                .with_stable_code(StableErrorCode::CliInvalidArguments),
        );
    }

    let mut components = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                return Err(CliError::command_usage(format!(
                    "path filter '{path}' must not contain '..'"
                ))
                .with_stable_code(StableErrorCode::CliInvalidArguments));
            }
            other => components.push(other),
        }
    }
    Ok(components.join("/"))
}

fn collect_matching_entries(
    root_tree: ObjectHash,
    filters: &[LsTreePathFilter],
    args: &LsTreeArgs,
    path_context: &LsTreePathContext,
) -> CliResult<Vec<RawLsTreeEntry>> {
    let mut entries = Vec::new();
    if filters.is_empty() {
        collect_current_scope_contents(
            root_tree,
            &path_context.current_dir_prefix,
            args,
            &mut entries,
        )?;
    } else {
        for filter in filters {
            collect_one_filter(root_tree, filter, args, path_context, &mut entries)?;
        }
    }
    Ok(dedupe_entries(entries))
}

fn collect_current_scope_contents(
    root_tree: ObjectHash,
    current_dir_prefix: &str,
    args: &LsTreeArgs,
    entries: &mut Vec<RawLsTreeEntry>,
) -> CliResult<()> {
    if current_dir_prefix.is_empty() {
        collect_tree_contents(root_tree, "", args, entries)?;
        return Ok(());
    }

    let (entry, subtree) = find_tree_entry(root_tree, current_dir_prefix)?;
    if entry.mode != TreeItemMode::Tree {
        return Err(path_not_found_error(current_dir_prefix));
    }
    collect_tree_contents(subtree, current_dir_prefix, args, entries)
}

fn collect_one_filter(
    root_tree: ObjectHash,
    filter: &LsTreePathFilter,
    args: &LsTreeArgs,
    path_context: &LsTreePathContext,
    entries: &mut Vec<RawLsTreeEntry>,
) -> CliResult<()> {
    if filter.path.is_empty() {
        collect_tree_contents(root_tree, "", args, entries)?;
        return Ok(());
    }

    let mut parents = Vec::new();
    let (entry, subtree) = find_tree_entry_with_parents(
        root_tree,
        &filter.path,
        args.show_trees.then_some(&mut parents),
    )?;
    entries.extend(parents.into_iter().filter(|parent| {
        path_context.current_dir_prefix.is_empty()
            || parent
                .path
                .strip_prefix(&path_context.current_dir_prefix)
                .is_some_and(|suffix| suffix.starts_with('/'))
    }));
    match entry.mode {
        TreeItemMode::Tree => {
            if args.tree_only {
                entries.push(entry.clone());
                if args.recursive {
                    collect_tree_contents(subtree, &entry.path, args, entries)?;
                }
            } else if args.recursive || filter.list_contents {
                if args.show_trees {
                    entries.push(entry.clone());
                }
                collect_tree_contents(subtree, &entry.path, args, entries)?;
            } else {
                entries.push(entry);
            }
        }
        _ => {
            if !args.tree_only {
                entries.push(entry);
            }
        }
    }
    Ok(())
}

fn find_tree_entry(root_tree: ObjectHash, filter: &str) -> CliResult<(RawLsTreeEntry, ObjectHash)> {
    find_tree_entry_with_parents(root_tree, filter, None)
}

fn find_tree_entry_with_parents(
    root_tree: ObjectHash,
    filter: &str,
    mut parents: Option<&mut Vec<RawLsTreeEntry>>,
) -> CliResult<(RawLsTreeEntry, ObjectHash)> {
    let mut current_tree = load_tree(root_tree)?;
    let mut current_path = String::new();
    let mut components = filter.split('/').peekable();

    while let Some(component) = components.next() {
        let Some(item) = current_tree
            .tree_items
            .iter()
            .find(|candidate| candidate.name == component)
        else {
            return Err(path_not_found_error(filter));
        };

        current_path = join_tree_path(&current_path, &item.name);
        let entry = RawLsTreeEntry {
            mode: item.mode,
            object_id: item.id,
            path: current_path.clone(),
        };
        if components.peek().is_none() {
            return Ok((entry, item.id));
        }
        if item.mode != TreeItemMode::Tree {
            return Err(path_not_found_error(filter));
        }
        if let Some(parents) = parents.as_deref_mut() {
            parents.push(entry);
        }
        current_tree = load_tree(item.id)?;
    }

    Err(path_not_found_error(filter))
}

fn collect_tree_contents(
    tree_id: ObjectHash,
    prefix: &str,
    args: &LsTreeArgs,
    entries: &mut Vec<RawLsTreeEntry>,
) -> CliResult<()> {
    let tree = load_tree(tree_id)?;
    for item in tree.tree_items {
        let path = join_tree_path(prefix, &item.name);
        let entry = RawLsTreeEntry {
            mode: item.mode,
            object_id: item.id,
            path: path.clone(),
        };

        if item.mode == TreeItemMode::Tree {
            if args.tree_only {
                entries.push(entry);
                if args.recursive {
                    collect_tree_contents(item.id, &path, args, entries)?;
                }
                continue;
            }

            if !args.recursive || args.show_trees {
                entries.push(entry);
            }
            if args.recursive {
                collect_tree_contents(item.id, &path, args, entries)?;
            }
        } else if !args.tree_only {
            entries.push(entry);
        }
    }
    Ok(())
}

fn load_tree(tree_id: ObjectHash) -> CliResult<Tree> {
    load_object(&tree_id).map_err(|error| {
        CliError::fatal(format!("failed to read tree object {tree_id}: {error}"))
            .with_stable_code(StableErrorCode::IoReadFailed)
    })
}

fn path_not_found_error(path: &str) -> CliError {
    CliError::failure(format!("path '{path}' does not exist in the selected tree"))
        .with_stable_code(StableErrorCode::CliInvalidTarget)
}

fn join_tree_path(prefix: &str, name: &str) -> String {
    if prefix.is_empty() {
        name.to_string()
    } else {
        format!("{prefix}/{name}")
    }
}

fn dedupe_entries(entries: Vec<RawLsTreeEntry>) -> Vec<RawLsTreeEntry> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for entry in entries {
        if seen.insert(entry.path.clone()) {
            deduped.push(entry);
        }
    }
    deduped
}

fn format_entry(
    entry: RawLsTreeEntry,
    abbrev: Option<usize>,
    include_size: bool,
    storage: &ClientStorage,
    path_context: &LsTreePathContext,
) -> CliResult<LsTreeEntry> {
    let object_type = object_type_for_mode(entry.mode);
    let size = if include_size && object_type == "blob" {
        Some(
            storage
                .get(&entry.object_id)
                .map_err(|error| {
                    CliError::fatal(format!(
                        "failed to read blob '{}': {error}",
                        entry.object_id
                    ))
                    .with_stable_code(StableErrorCode::IoReadFailed)
                })?
                .len(),
        )
    } else {
        None
    };

    Ok(LsTreeEntry {
        mode: mode_string(entry.mode).to_string(),
        object_type: object_type.to_string(),
        object: abbreviate_hash(&entry.object_id.to_string(), abbrev),
        path: display_path(&entry.path, path_context),
        size,
    })
}

fn display_path(path: &str, path_context: &LsTreePathContext) -> String {
    if path_context.display_repository_paths || path_context.current_dir_prefix.is_empty() {
        return path.to_string();
    }

    if path == path_context.current_dir_prefix {
        return ".".to_string();
    }
    path.strip_prefix(&format!("{}/", path_context.current_dir_prefix))
        .unwrap_or(path)
        .to_string()
}

pub(crate) fn object_type_for_mode(mode: TreeItemMode) -> &'static str {
    match mode {
        TreeItemMode::Tree => "tree",
        TreeItemMode::Commit => "commit",
        _ => "blob",
    }
}

pub(crate) fn mode_string(mode: TreeItemMode) -> &'static str {
    match mode {
        TreeItemMode::Blob => "100644",
        TreeItemMode::BlobExecutable => "100755",
        TreeItemMode::Tree => "040000",
        TreeItemMode::Commit => "160000",
        TreeItemMode::Link => "120000",
    }
}

fn abbreviate_hash(hash: &str, abbrev: Option<usize>) -> String {
    let Some(width) = abbrev else {
        return hash.to_string();
    };
    let width = width.min(hash.len());
    hash[..width].to_string()
}

fn write_ls_tree_output<W: Write>(
    writer: &mut W,
    entries: &[LsTreeEntry],
    args: &LsTreeArgs,
) -> CliResult<()> {
    let separator = if args.nul { b'\0' } else { b'\n' };
    for entry in entries {
        let line = render_human_entry(entry, args);
        if !write_record(writer, line.as_bytes(), separator)? {
            return Ok(());
        }
    }
    Ok(())
}

/// Render an entry through a `--format` string, substituting the supported
/// atoms and `%x09`/`%x0a` escapes. Unknown atoms are left verbatim (lenient).
fn render_format_entry(entry: &LsTreeEntry, format: &str) -> String {
    let size = entry
        .size
        .map(|value| value.to_string())
        .unwrap_or_else(|| "-".to_string());
    format
        .replace("%(objectmode)", &entry.mode)
        .replace("%(objecttype)", &entry.object_type)
        .replace("%(objectname)", &entry.object)
        .replace("%(objectsize)", &size)
        .replace("%(objectsize:padded)", &format!("{size:>7}"))
        .replace("%(path)", &entry.path)
        .replace("%x09", "\t")
        .replace("%x0a", "\n")
}

fn render_human_entry(entry: &LsTreeEntry, args: &LsTreeArgs) -> String {
    if let Some(format) = &args.format {
        return render_format_entry(entry, format);
    }
    if args.object_only {
        return entry.object.clone();
    }
    if args.name_only || args.name_status {
        return entry.path.clone();
    }
    if args.long {
        let size = entry
            .size
            .map(|value| value.to_string())
            .unwrap_or_else(|| "-".to_string());
        return format!(
            "{} {} {} {:>7}\t{}",
            entry.mode, entry.object_type, entry.object, size, entry.path
        );
    }
    format!(
        "{} {} {}\t{}",
        entry.mode, entry.object_type, entry.object, entry.path
    )
}

fn write_record<W: Write>(writer: &mut W, bytes: &[u8], separator: u8) -> CliResult<bool> {
    match writer.write_all(bytes) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => return Ok(false),
        Err(error) => {
            return Err(
                CliError::fatal(format!("failed to write ls-tree output: {error}"))
                    .with_stable_code(StableErrorCode::IoWriteFailed),
            );
        }
    }
    match writer.write_all(&[separator]) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(false),
        Err(error) => Err(
            CliError::fatal(format!("failed to write ls-tree output: {error}"))
                .with_stable_code(StableErrorCode::IoWriteFailed),
        ),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;
    use git_internal::{hash::ObjectHash, internal::object::tree::TreeItemMode};

    use super::{
        LsTreeArgs, RawLsTreeEntry, abbreviate_hash, mode_string, normalize_one_path_filter,
        object_type_for_mode, render_human_entry,
    };

    fn entry(path: &str) -> super::LsTreeEntry {
        super::LsTreeEntry {
            mode: "100644".to_string(),
            object_type: "blob".to_string(),
            object: "1234567890abcdef".to_string(),
            path: path.to_string(),
            size: Some(12),
        }
    }

    #[test]
    fn parse_abbrev_without_value_uses_default_width() {
        let args =
            LsTreeArgs::try_parse_from(["ls-tree", "--abbrev", "HEAD"]).expect("args should parse");
        assert_eq!(args.abbrev, Some(7));
    }

    #[test]
    fn parse_output_modes_are_mutually_exclusive() {
        let result =
            LsTreeArgs::try_parse_from(["ls-tree", "--name-only", "--object-only", "HEAD"]);
        assert!(result.is_err());
    }

    #[test]
    fn render_format_substitutes_atoms() {
        let args = LsTreeArgs::try_parse_from([
            "ls-tree",
            "--format",
            "%(objectmode) %(objecttype) %(objectname) %(objectsize)%x09%(path)",
            "HEAD",
        ])
        .expect("args should parse");
        let rendered = render_human_entry(&entry("src/main.rs"), &args);
        assert_eq!(rendered, "100644 blob 1234567890abcdef 12\tsrc/main.rs");
    }

    #[test]
    fn parse_format_conflicts_with_name_only() {
        let result =
            LsTreeArgs::try_parse_from(["ls-tree", "--format", "%(path)", "--name-only", "HEAD"]);
        assert!(result.is_err());
    }

    #[test]
    fn normalize_path_filter_rejects_parent_escape() {
        assert!(normalize_one_path_filter("../secret").is_err());
    }

    #[test]
    fn normalize_path_filter_collapses_current_dir_segments() {
        let normalized =
            normalize_one_path_filter("./src//command/.").expect("path should normalize");
        assert_eq!(normalized, "src/command");
    }

    #[test]
    fn object_type_maps_gitlink_to_commit() {
        assert_eq!(object_type_for_mode(TreeItemMode::Commit), "commit");
    }

    #[test]
    fn mode_string_pads_tree_mode_like_git() {
        assert_eq!(mode_string(TreeItemMode::Tree), "040000");
    }

    #[test]
    fn render_long_entry_formats_blob_size() {
        let args = LsTreeArgs::try_parse_from(["ls-tree", "-l", "HEAD"]).expect("args parse");
        let line = render_human_entry(&entry("README.md"), &args);
        assert!(line.contains("     12\tREADME.md"));
    }

    #[test]
    fn render_name_only_suppresses_metadata() {
        let args =
            LsTreeArgs::try_parse_from(["ls-tree", "--name-only", "HEAD"]).expect("args parse");
        assert_eq!(render_human_entry(&entry("README.md"), &args), "README.md");
    }

    #[test]
    fn abbreviate_hash_uses_requested_width() {
        assert_eq!(abbreviate_hash("1234567890abcdef", Some(10)), "1234567890");
    }

    #[test]
    fn raw_entry_is_cloneable_for_directory_filtering() {
        let hash = ObjectHash::from_bytes_for_kind(
            git_internal::hash::get_hash_kind(),
            &vec![1; git_internal::hash::get_hash_kind().size()],
        )
        .expect("test hash bytes should match active hash kind");
        let raw = RawLsTreeEntry {
            mode: TreeItemMode::Tree,
            object_id: hash,
            path: "src".to_string(),
        };
        assert_eq!(raw.clone().path, "src");
    }
    // These bytes come from the pinned Git control, independently of Libra.
    const ISSUE1057_GOLDEN: &[(&str, usize, &str)] = &[
        (
            "size",
            102,
            "464f4b734a2a36eff08fcf505082d50df527900825c84143a3f42f15f4062cc3",
        ),
        (
            "size-six",
            34,
            "ee027933988caf703904b93fa400a34024f4186d1f95fcf43570ba3e83645ee0",
        ),
    ];
    fn issue1057_golden(name: &str) -> &'static [u8] {
        use sha2::{Digest, Sha256};
        let bytes: &'static [u8] = match name {
"size" => b"      -|-|      -|dir\n      0|0|      0|empty\n10000000|10000000|10000000|large\n      -|-|      -|link\n",
"size-six" => b"      6|6|      6|dir/nested/file\n",

            _ => panic!("unknown issue1057 golden: {name}"),
        };
        let (_, len, sha) = ISSUE1057_GOLDEN
            .iter()
            .find(|row| row.0 == name)
            .expect("registered golden");
        assert_eq!(bytes.len(), *len);
        assert_eq!(format!("{:x}", Sha256::digest(bytes)), *sha);
        bytes
    }
    fn issue1057_size_line(path: &str) -> &'static str {
        let bytes = if path == "dir/nested/file" {
            issue1057_golden("size-six")
        } else {
            issue1057_golden("size")
        };
        std::str::from_utf8(bytes)
            .expect("UTF-8 golden")
            .lines()
            .find(|line| line.rsplit('|').next() == Some(path))
            .expect("size golden path")
    }
    const ISSUE1057_SIZE_FORMAT: &str =
        "%(objectsize:padded)|%(objectsize)|%(objectsize:padded)|%(path)";

    #[test]
    fn ls_tree_issue1057_padded_blob() {
        let mut item = entry("dir/nested/file");
        item.size = Some(6);
        item.object_type = "blob".to_string();
        assert_eq!(
            super::render_format_entry(&item, ISSUE1057_SIZE_FORMAT),
            issue1057_size_line("dir/nested/file")
        );
    }

    #[test]
    fn ls_tree_issue1057_padded_empty() {
        let mut item = entry("empty");
        item.size = Some(0);
        item.object_type = "blob".to_string();
        assert_eq!(
            super::render_format_entry(&item, ISSUE1057_SIZE_FORMAT),
            issue1057_size_line("empty")
        );
    }

    #[test]
    fn ls_tree_issue1057_padded_large() {
        let mut item = entry("large");
        item.size = Some(10_000_000);
        item.object_type = "blob".to_string();
        assert_eq!(
            super::render_format_entry(&item, ISSUE1057_SIZE_FORMAT),
            issue1057_size_line("large")
        );
    }

    #[test]
    fn ls_tree_issue1057_padded_tree() {
        let mut item = entry("dir");
        item.size = None;
        item.object_type = "tree".to_string();
        assert_eq!(
            super::render_format_entry(&item, ISSUE1057_SIZE_FORMAT),
            issue1057_size_line("dir")
        );
    }

    #[test]
    fn ls_tree_issue1057_padded_gitlink() {
        let mut item = entry("link");
        item.size = None;
        item.object_type = "commit".to_string();
        assert_eq!(
            super::render_format_entry(&item, ISSUE1057_SIZE_FORMAT),
            issue1057_size_line("link")
        );
    }

    #[test]
    fn ls_tree_issue1057_padded_repeated() {
        let mut item = entry("empty");
        item.size = Some(0);
        let columns: Vec<_> = issue1057_size_line("empty").split('|').collect();
        assert_eq!(
            super::render_format_entry(&item, "%(objectsize:padded)|%(objectsize:padded)"),
            format!("{}|{}", columns[0], columns[2])
        );
    }
    #[test]
    fn ls_tree_issue1057_padded_literal_path() {
        let path = "file-%(objectsize:padded)";
        let mut item = entry(path);
        item.size = Some(6);
        let padded = issue1057_size_line("dir/nested/file")
            .split('|')
            .next()
            .expect("size");
        assert_eq!(
            super::render_format_entry(&item, "%(path)|%(objectsize:padded)"),
            format!("{path}|{padded}")
        );
    }
}
