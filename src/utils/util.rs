//! Core utility toolbox for repo detection, path conversion, ignore checking, storage access, hashing helpers, and miscellaneous formatting/time utilities.

use std::{
    collections::{HashMap, HashSet},
    env,
    ffi::OsStr,
    fs, io,
    io::Write,
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use git_internal::{
    hash::ObjectHash,
    internal::object::{commit::Commit, types::ObjectType},
};
use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use path_absolutize::*;

use crate::{
    command::load_object,
    internal::{
        branch::{Branch, BranchStoreError},
        config::{ConfigKv, LocalIdentityTarget, read_cascaded_config_value},
        head::Head,
        tag,
    },
    utils::{client_storage::ClientStorage, path, path_ext::PathExt},
};

// SAFETY: The unwrap() and expect() calls in this module are documented with safety
// justifications where used. These are intentional panics for unrecoverable errors
// or cases where invariants are guaranteed by the code structure.

pub const ROOT_DIR: &str = ".libra";
/// The Git metadata directory. Like Git, Libra always force-ignores any `.git`
/// directory in the worktree so a nested Git repository is never surfaced as
/// untracked or staged, and it cannot be un-ignored via `.libraignore`.
pub const GIT_DIR: &str = ".git";
const LIBRAIGNORE_FILE: &str = ".libraignore";
const GITIGNORE_FILE: &str = ".gitignore";
const CORE_EXCLUDES_FILE_KEY: &str = "core.excludesFile";
pub const DATABASE: &str = "libra.db";
pub const ATTRIBUTES: &str = ".libra_attributes";

static OBJECTS_STORAGE_CACHE: Lazy<Mutex<HashMap<PathBuf, ClientStorage>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static LIBRAIGNORE_CACHE: Lazy<Mutex<HashMap<IgnoreCacheKey, CachedGitignore>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));
static CONFIG_PATH_CACHE: Lazy<Mutex<HashMap<ConfigPathCacheKey, Option<PathBuf>>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct IgnoreCacheKey {
    source: PathBuf,
    base: PathBuf,
}

struct CachedGitignore {
    len: u64,
    modified: SystemTime,
    matcher: Arc<Gitignore>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ConfigPathCacheKey {
    workdir: PathBuf,
    key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitRepositoryLocation {
    pub root: PathBuf,
    pub is_bare: bool,
}

/// Returns the current working directory as a `PathBuf`.
///
/// This function wraps the `std::env::current_dir()` function and provides
/// robust fallback behavior when the current directory is not available.
///
/// # Returns
///
/// A `PathBuf` representing the current working directory. If the current
/// directory cannot be determined, this function uses the following fallbacks:
/// 1. The `PWD` environment variable (if set and points to a valid directory)
/// 2. The parent directory of the current executable (if available)
/// 3. The root directory `/` as a last resort
pub fn cur_dir() -> PathBuf {
    match env::current_dir() {
        Ok(dir) => dir,
        Err(_) => {
            // Fallback 1: use PWD if present and valid
            if let Ok(pwd) = env::var("PWD") {
                let p = PathBuf::from(&pwd);
                if p.exists() && p.is_dir() {
                    return p;
                }
            }

            // Fallback 2: directory of the current executable if available
            if let Ok(exec) = env::current_exe()
                && let Some(parent) = exec.parent()
                && parent.exists()
                && parent.is_dir()
            {
                return parent.to_path_buf();
            }

            // Fallback 3: root directory to ensure a stable, existing path
            PathBuf::from("/")
        }
    }
}

fn is_valid_storage_dir(path: &Path) -> bool {
    if path.join(DATABASE).exists() {
        return true;
    }
    // lore.md 2.1: a linked worktree's `.libra` holds only `commondir` +
    // `worktree_id` + `index` (db/objects live in the common storage), so
    // recognize it by its commondir pointer.
    if path.join("commondir").exists() {
        return true;
    }

    ["objects", "info/exclude", "hooks"]
        .iter()
        .filter(|marker| path.join(marker).exists())
        .count()
        >= 2
}

fn read_gitdir_file(path: &Path, worktree: &Path) -> Option<PathBuf> {
    let contents = fs::read_to_string(path).ok()?;
    let line = contents.lines().next()?.trim();
    let raw = line.strip_prefix("gitdir:")?.trim();
    if raw.is_empty() {
        return None;
    }

    let git_dir = Path::new(raw);
    Some(if git_dir.is_absolute() {
        git_dir.to_path_buf()
    } else {
        worktree.join(git_dir)
    })
}

fn resolve_dot_git_dir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    let metadata = fs::metadata(&dot_git).ok()?;
    if metadata.is_dir() {
        return Some(dot_git);
    }
    if metadata.is_file() {
        return read_gitdir_file(&dot_git, worktree);
    }
    None
}

/// Resolve a Git-standard `.git/info/<name>` file for a worktree, including
/// linked-worktree `.git` files. Returns `None` when the worktree has no Git
/// metadata directory.
pub fn git_info_file_path(worktree: &Path, name: &str) -> Option<PathBuf> {
    let git_dir = resolve_dot_git_dir(worktree)?;
    let common_dir = git_common_dir(&git_dir).unwrap_or(git_dir);
    Some(common_dir.join("info").join(name))
}

fn git_common_dir(git_dir: &Path) -> Option<PathBuf> {
    let contents = fs::read_to_string(git_dir.join("commondir")).ok()?;
    let raw = contents.lines().next()?.trim();
    if raw.is_empty() {
        return None;
    }

    let common_dir = Path::new(raw);
    let path = if common_dir.is_absolute() {
        common_dir.to_path_buf()
    } else {
        git_dir.join(common_dir)
    };
    Some(normalize_lexical_path(&path))
}

fn normalize_lexical_path(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(Path::new(component.as_os_str())),
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                    out.pop();
                } else if !matches!(
                    out.components().next_back(),
                    Some(Component::RootDir | Component::Prefix(_))
                ) {
                    out.push("..");
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    out
}

fn git_dir_marker_exists(git_dir: &Path, common_dir: Option<&Path>, marker: &str) -> bool {
    git_dir.join(marker).exists() || common_dir.is_some_and(|dir| dir.join(marker).exists())
}

fn is_valid_git_storage_dir(git_dir: &Path) -> bool {
    let common_dir = git_common_dir(git_dir);
    git_dir.join("HEAD").exists()
        && git_dir_marker_exists(git_dir, common_dir.as_deref(), "config")
        && git_dir_marker_exists(git_dir, common_dir.as_deref(), "objects")
}

fn git_config_declares_bare(git_dir: &Path) -> bool {
    fs::read_to_string(git_dir.join("config")).is_ok_and(|config| {
        config.lines().any(|line| {
            line.trim().split_once('=').is_some_and(|(key, value)| {
                key.trim() == "bare" && value.trim().eq_ignore_ascii_case("true")
            })
        })
    })
}

fn worktree_root_for_dot_git_storage(candidate: &Path) -> Option<PathBuf> {
    if candidate.file_name() != Some(OsStr::new(".git"))
        || !is_valid_git_storage_dir(candidate)
        || git_config_declares_bare(candidate)
    {
        return None;
    }
    let parent = candidate.parent()?;
    Some(fs::canonicalize(parent).unwrap_or_else(|_| parent.to_path_buf()))
}

pub fn find_git_repository(path: Option<&Path>) -> Option<GitRepositoryLocation> {
    let mut candidate = path.map(Path::to_path_buf).unwrap_or_else(cur_dir);
    if candidate.is_file() {
        candidate.pop();
    }

    loop {
        if let Some(git_dir) = resolve_dot_git_dir(&candidate)
            && is_valid_git_storage_dir(&git_dir)
        {
            return Some(GitRepositoryLocation {
                root: fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone()),
                is_bare: false,
            });
        }

        if let Some(root) = worktree_root_for_dot_git_storage(&candidate) {
            return Some(GitRepositoryLocation {
                root,
                is_bare: false,
            });
        }

        if is_valid_git_storage_dir(&candidate) {
            return Some(GitRepositoryLocation {
                root: fs::canonicalize(&candidate).unwrap_or_else(|_| candidate.clone()),
                is_bare: true,
            });
        }

        if !candidate.pop() {
            return None;
        }
    }
}

/// The shared/common storage for a worktree gitdir (lore.md 2.1): follow a
/// `commondir` pointer if present (a linked worktree borrows the main repo's
/// db/objects/hooks), else the gitdir itself (the main worktree). The
/// per-worktree `index` and `worktree_id` always live in the local gitdir.
fn worktree_common_storage(gitdir: &Path) -> PathBuf {
    if let Ok(contents) = fs::read_to_string(gitdir.join("commondir"))
        && let Some(raw) = contents
            .lines()
            .next()
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        let common = Path::new(raw);
        let resolved = if common.is_absolute() {
            common.to_path_buf()
        } else {
            gitdir.join(common)
        };
        return fs::canonicalize(&resolved).unwrap_or(resolved);
    }
    gitdir.to_path_buf()
}

/// Resolve `(common_storage, workdir, worktree_gitdir)` for a path.
/// - `worktree_gitdir`: the LOCAL `.libra` for this working tree (holds the
///   private `index` and `worktree_id`).
/// - `common_storage`: the SHARED `.libra` (db / objects / hooks). Equals the
///   gitdir for the main worktree; the `commondir` target for a linked one.
fn try_get_paths_full(path: Option<PathBuf>) -> Result<(PathBuf, PathBuf, PathBuf), io::Error> {
    let mut path = path.clone().unwrap_or_else(cur_dir);
    let orig = path.clone();

    loop {
        let standard_repo = path.join(ROOT_DIR);
        if standard_repo.is_dir() && is_valid_storage_dir(&standard_repo) {
            // unwrap_or is safe here: if canonicalize fails, we use the original path
            let gitdir = fs::canonicalize(&standard_repo).unwrap_or(standard_repo);
            let common = worktree_common_storage(&gitdir);
            return Ok((common, path.clone(), gitdir));
        }

        if path.join(DATABASE).exists() && path.join("objects").exists() {
            return Ok((path.clone(), path.clone(), path.clone()));
        }

        if !path.pop() {
            return Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("{orig:?} is not a libra repository"),
            ));
        }
    }
}

/// `(common_storage, workdir)` — db/objects/hooks storage + the working dir.
fn try_get_paths(path: Option<PathBuf>) -> Result<(PathBuf, PathBuf), io::Error> {
    let (common, workdir, _gitdir) = try_get_paths_full(path)?;
    Ok((common, workdir))
}

/// The LOCAL `.libra` gitdir for this working tree (lore.md 2.1): where the
/// per-worktree `index` + `worktree_id` live. Equals the common storage for the
/// main worktree; the linked worktree's own `.libra` otherwise.
pub fn try_get_worktree_gitdir(path: Option<PathBuf>) -> Result<PathBuf, io::Error> {
    let (_common, _workdir, gitdir) = try_get_paths_full(path)?;
    Ok(gitdir)
}

/// The current worktree's gitdir (panics outside a repo — mirrors `storage_path`).
pub fn worktree_gitdir() -> PathBuf {
    try_get_worktree_gitdir(None).expect("worktree_gitdir() called outside a libra repository")
}

/// The current worktree's stable instance id (lore.md 2.1), read ambiently from
/// `<gitdir>/worktree_id`. `None` = the MAIN worktree (HEAD/index/reflog rows
/// with `worktree_id IS NULL`). Resolved from the process cwd exactly like
/// `path::index()`; a single process operates in one worktree, so this is
/// stable for its lifetime.
/// Whether the current process runs in a LINKED worktree (lore.md 2.1). v1
/// refuses in-progress sequencer operations (merge/rebase/cherry-pick/revert/
/// bisect) here because their state (rebase_state / sequence_state /
/// MERGE_HEAD) is still shared across worktrees.
pub fn is_linked_worktree() -> bool {
    current_worktree_id().is_some()
}

pub fn current_worktree_id() -> Option<String> {
    let gitdir = try_get_worktree_gitdir(None).ok()?;
    if let Ok(id) = fs::read_to_string(gitdir.join("worktree_id")) {
        let id = id.trim();
        if !id.is_empty() {
            return Some(id.to_string());
        }
    }
    // FAIL-CLOSED (Codex P1): a LINKED worktree is identified by its
    // `commondir` pointer. If its `worktree_id` file is missing/empty/unreadable
    // (corruption), we must NEVER return `None` — that aliases to the MAIN
    // worktree and would graft the main HEAD. Synthesize a stable id from the
    // canonical workdir (matching creation's derivation) so a recovered
    // worktree stays isolated from main and keeps its rows.
    if gitdir.join("commondir").exists() {
        let workdir = gitdir.parent().unwrap_or(&gitdir);
        let canonical = fs::canonicalize(workdir).unwrap_or_else(|_| workdir.to_path_buf());
        return Some(worktree_instance_id(&canonical));
    }
    None
}

/// A stable, unique instance id for a worktree (lore.md 2.1), derived from its
/// canonical path so it is deterministic across invocations. FNV-1a keeps it
/// dependency-free and filesystem-safe. Shared by worktree creation and the
/// fail-closed fallback in [`current_worktree_id`].
pub fn worktree_instance_id(canonical_path: &Path) -> String {
    let bytes = canonical_path.to_string_lossy();
    let mut hash: u64 = 0xcbf29ce484222325;
    for b in bytes.as_bytes() {
        hash ^= *b as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let base = canonical_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "wt".to_string());
    let sanitized: String = base
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    format!("{sanitized}-{hash:016x}")
}

/// Try to get the storage path of the repository, which is the path of the `.libra` directory
/// - if the current directory or given path is not a repository, return an error
pub fn try_get_storage_path(path: Option<PathBuf>) -> Result<PathBuf, io::Error> {
    let (storage, _) = try_get_paths(path)?;
    Ok(storage)
}

/// Load the storage path with optional given repository
pub fn storage_path() -> PathBuf {
    // INVARIANT: this function is documented to panic when called outside a
    // repository. Callers that handle the missing-repo case should use
    // `try_get_storage_path` directly.
    try_get_storage_path(None).expect("storage_path() called outside a libra repository")
}

/// Return an error instead of printing when the current directory is not a repository.
pub fn require_repo() -> io::Result<()> {
    try_get_storage_path(None).map(|_| ())
}

/// Get `ClientStorage` for the `objects` directory
pub fn objects_storage() -> ClientStorage {
    cached_objects_storage(path::objects())
}

/// Get `ClientStorage` for the `objects` directory, returning a Result
pub fn try_objects_storage() -> io::Result<ClientStorage> {
    // Check if we are in a valid repo first to avoid panic in path::objects() if possible,
    // though path::objects() currently panics if storage_path() fails.
    // Ideally path::objects() should also be fallible.
    // For now, let's wrap the panic-prone call if we can, or just rely on try_get_storage_path check.
    if try_get_storage_path(None).is_err() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "not a libra repository",
        ));
    }
    Ok(cached_objects_storage(path::objects()))
}

fn cached_objects_storage(base_path: PathBuf) -> ClientStorage {
    let mut cache = OBJECTS_STORAGE_CACHE
        .lock()
        .expect("objects storage cache mutex poisoned"); // panic is intentional: if poisoned, we cannot recover
    if let Some(storage) = cache.get(&base_path) {
        return storage.clone();
    }

    let storage = ClientStorage::init(base_path.clone());
    cache.insert(base_path, storage.clone());
    storage
}

pub fn reset_objects_storage_cache_for_path(base_path: &Path) {
    if let Ok(mut cache) = OBJECTS_STORAGE_CACHE.lock() {
        cache.remove(base_path);
    }
}

/// Get the working directory of the repository
/// - panics if the current directory is not a repository
pub fn working_dir() -> PathBuf {
    // INVARIANT: this function is documented to panic when called outside a
    // repository. Callers that handle the missing-repo case should use
    // `try_working_dir` directly.
    let (_, workdir) =
        try_get_paths(None).expect("working_dir() called outside a libra repository");
    workdir
}

/// Get the working directory of the repository.
pub fn try_working_dir() -> io::Result<PathBuf> {
    let (_, workdir) = try_get_paths(None)?;
    Ok(workdir)
}

/// Read a path-valued config key from local-then-global config and expand it
/// relative to `workdir`. This is best-effort for optional Git compatibility
/// sources such as `core.excludesFile`: unreadable or absent config is treated
/// as no configured path so hot-path commands do not fail because an optional
/// global config DB is unavailable.
pub fn optional_cascaded_config_path(key: &str, workdir: &Path) -> Option<PathBuf> {
    let cache_key = ConfigPathCacheKey {
        workdir: workdir.to_path_buf(),
        key: key.to_string(),
    };
    if let Ok(cache) = CONFIG_PATH_CACHE.lock()
        && let Some(cached) = cache.get(&cache_key)
    {
        return cached.clone();
    }

    let value = read_cascaded_config_value_sync(key).map(|raw| expand_config_path(&raw, workdir));
    if let Ok(mut cache) = CONFIG_PATH_CACHE.lock() {
        cache.insert(cache_key, value.clone());
    }
    value
}

fn read_cascaded_config_value_sync(key: &str) -> Option<String> {
    let key = key.to_string();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .ok()?;
        runtime
            .block_on(read_cascaded_config_value(
                LocalIdentityTarget::CurrentRepo,
                &key,
            ))
            .ok()
            .flatten()
    })
    .join()
    .ok()
    .flatten()
}

fn expand_config_path(raw: &str, workdir: &Path) -> PathBuf {
    let expanded = if raw == "~" {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(raw))
    } else if let Some(rest) = raw.strip_prefix("~/") {
        dirs::home_dir()
            .map(|home| home.join(rest))
            .unwrap_or_else(|| PathBuf::from(raw))
    } else {
        PathBuf::from(raw)
    };

    if expanded.is_absolute() {
        expanded
    } else {
        workdir.join(expanded)
    }
}

/// Get the working directory of the repository as a string, panics if the path is not valid utf-8
pub fn working_dir_string() -> String {
    // INVARIANT: this function is documented to panic on non-UTF-8 paths.
    // Callers that handle non-UTF-8 paths should use `try_working_dir_string`.
    working_dir()
        .to_str()
        .expect("working_dir_string() called with non-UTF-8 working directory path")
        .to_string()
}

/// Get the working directory of the repository as UTF-8.
pub fn try_working_dir_string() -> io::Result<String> {
    let workdir = try_working_dir()?;
    workdir.to_str().map(str::to_string).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("path '{}' is not valid UTF-8", workdir.display()),
        )
    })
}

/// Turn a path to a relative path to the working directory
/// - not check existence
pub fn to_workdir_path(path: impl AsRef<Path>) -> PathBuf {
    to_relative(path, working_dir())
}

/// Turn a workdir path to absolute path
pub fn workdir_to_absolute(path: impl AsRef<Path>) -> PathBuf {
    working_dir().join(path.as_ref())
}

/// Turn a workdir path to absolute path.
pub fn try_workdir_to_absolute(path: impl AsRef<Path>) -> io::Result<PathBuf> {
    Ok(try_working_dir()?.join(path.as_ref()))
}

/// Judge if the path is a sub path of the parent path
/// - Not check existence
/// - `true` if path == parent
pub fn is_sub_path<P, B>(path: P, parent: B) -> bool
where
    P: AsRef<Path>,
    B: AsRef<Path>,
{
    fn normalize_abs_path(path: &Path) -> PathBuf {
        use std::path::Component;

        let mut out = PathBuf::new();
        for comp in path.components() {
            match comp {
                Component::Prefix(prefix) => out.push(prefix.as_os_str()),
                Component::RootDir => out.push(Path::new(comp.as_os_str())),
                Component::CurDir => {}
                Component::ParentDir => {
                    // Never allow `..` to escape above filesystem root/prefix.
                    if matches!(out.components().next_back(), Some(Component::Normal(_))) {
                        out.pop();
                    }
                }
                Component::Normal(part) => out.push(part),
            }
        }
        out
    }

    // Avoid panics and avoid depending on a valid current directory when inputs are absolute.
    let path_abs = if path.as_ref().is_absolute() {
        normalize_abs_path(path.as_ref())
    } else {
        match path.as_ref().absolutize() {
            Ok(p) => p.to_path_buf(),
            Err(_) => return false,
        }
    };

    let parent_abs = if parent.as_ref().is_absolute() {
        normalize_abs_path(parent.as_ref())
    } else {
        match parent.as_ref().absolutize() {
            Ok(p) => p.to_path_buf(),
            Err(_) => return false,
        }
    };

    path_abs.starts_with(parent_abs)
}

/// Judge if the `path` is sub-path of `paths`(include sub-dirs)
/// - absolute path or relative path to the current dir
/// - Not check existence
pub fn is_sub_of_paths<P, U>(path: impl AsRef<Path>, paths: U) -> bool
where
    P: AsRef<Path>,
    U: IntoIterator<Item = P>,
{
    for p in paths {
        if is_sub_path(path.as_ref(), p.as_ref()) {
            return true;
        }
    }
    false
}

/// Filter paths to fit the given paths, include sub-dirs
/// - return the paths that are sub-path of the fit paths
/// - `paths`: to workdir
/// - `fit_paths`: abs or rel
/// - Not check existence
pub fn filter_to_fit_paths<P>(paths: &[P], fit_paths: &Vec<P>) -> Vec<P>
where
    P: AsRef<Path> + Clone,
{
    paths
        .iter()
        .filter(|p| {
            let p = workdir_to_absolute(p.as_ref());
            is_sub_of_paths(p, fit_paths)
        })
        .cloned()
        .collect()
}

/// `path` & `base` must be absolute or relative (to current dir)
/// <br> return "." if `path` == `base`
pub fn to_relative<P, B>(path: P, base: B) -> PathBuf
where
    P: AsRef<Path>,
    B: AsRef<Path>,
{
    // Snapshot the current directory once so both inputs resolve against the same base
    // even if another test or thread changes the process cwd concurrently.
    let cwd = cur_dir();
    let path_abs = match path.as_ref().absolutize_from(&cwd) {
        Ok(p) => p.into_owned(),
        Err(_) => cwd.join(path.as_ref()),
    };
    let base_abs = match base.as_ref().absolutize_from(&cwd) {
        Ok(b) => b.into_owned(),
        Err(_) => cwd.join(base.as_ref()),
    };

    if let Some(rel_path) = pathdiff::diff_paths(path_abs, base_abs) {
        if rel_path.to_string_lossy() == "" {
            PathBuf::from(".")
        } else {
            rel_path
        }
    } else {
        // panic is intentional: this indicates a bug in path resolution logic
        panic!(
            "fatal: path {:?} cannot convert to relative based on {:?}",
            path.as_ref(),
            base.as_ref()
        );
    }
}

#[allow(dead_code)]
/// Convert a path to relative path to the current directory
/// - `path` must be absolute or relative (to current dir)
pub fn to_current_dir<P>(path: P) -> PathBuf
where
    P: AsRef<Path>,
{
    to_relative(path, cur_dir())
}

/// Convert a workdir path to relative path
/// - `base` must be absolute or relative (to current dir)
pub fn workdir_to_relative<P, B>(path: P, base: B) -> PathBuf
where
    P: AsRef<Path>,
    B: AsRef<Path>,
{
    let path_abs = workdir_to_absolute(path);
    to_relative(path_abs, base)
}

/// Convert a workdir path to relative path to the current directory
pub fn workdir_to_current<P>(path: P) -> PathBuf
where
    P: AsRef<Path>,
{
    workdir_to_relative(path, cur_dir())
}

/// List all files in the given dir and its sub_dir, except `.libra`
/// - input `path`: absolute path or relative path to the current dir
/// - output: to workdir path
pub fn list_files(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    if path.is_dir() {
        // unwrap_or_default is safe: returns empty string which won't match ROOT_DIR/GIT_DIR
        let name = path.file_name().unwrap_or_default();
        if name == OsStr::new(ROOT_DIR) || name == OsStr::new(GIT_DIR) {
            // ignore `.libra` (Libra metadata) and `.git` (always, like Git)
            return Ok(files);
        }
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                files.extend(list_files(&path)?);
            } else {
                files.push(to_workdir_path(&path));
            }
        }
    }
    Ok(files)
}

/// list all non-ignored files in the working dir(include sub_dir)
/// - output: to workdir path
pub fn list_workdir_files() -> io::Result<Vec<PathBuf>> {
    list_files_respecting_libraignore(&working_dir())
}

/// list all files in the working dir(include sub_dir), including ignored files
/// - output: to workdir path
pub fn list_workdir_files_unfiltered() -> io::Result<Vec<PathBuf>> {
    list_files(&working_dir())
}

fn list_files_respecting_libraignore(path: &Path) -> io::Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let name = path.file_name().unwrap_or_default();
    if !path.is_dir() || name == OsStr::new(ROOT_DIR) || name == OsStr::new(GIT_DIR) {
        return Ok(files);
    }

    fn visit(workdir: &Path, dir: &Path, files: &mut Vec<PathBuf>) -> io::Result<()> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let entry_path = entry.path();
            let file_name = entry.file_name();
            if file_name == OsStr::new(ROOT_DIR) || file_name == OsStr::new(GIT_DIR) {
                continue;
            }

            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                if !check_gitignore(workdir, &entry_path) {
                    visit(workdir, &entry_path, files)?;
                }
            } else if (file_type.is_file() || file_type.is_symlink())
                && !check_gitignore(workdir, &entry_path)
            {
                files.push(to_workdir_path(&entry_path));
            }
        }
        Ok(())
    }

    visit(path, path, &mut files)?;

    Ok(files)
}

/// Integrate the input paths (relative, absolute, file, dir) to workdir paths.
/// Only existing files are expanded; directory traversal failures are surfaced
/// to the caller so command code can report them instead of panicking.
pub fn integrate_pathspec(paths: &[PathBuf]) -> io::Result<HashSet<PathBuf>> {
    let mut workdir_paths = HashSet::new();
    for path in paths {
        if path.is_dir() {
            let files = list_files(path)?;
            workdir_paths.extend(files);
        } else {
            workdir_paths.insert(path.to_workdir());
        }
    }
    Ok(workdir_paths)
}

/// write content to file
/// - create parent directory if not exist
pub fn write_file(content: &[u8], file: &PathBuf) -> io::Result<()> {
    let mut parent = file.clone();
    parent.pop();
    fs::create_dir_all(parent)?;
    let mut file = fs::File::create(file)?;
    file.write_all(content)
}

/// Removing the empty directories in cascade until meet the root of workdir or the current dir
pub fn clear_empty_dir(dir: &Path) {
    let mut dir = if dir.is_dir() {
        dir.to_path_buf()
    } else {
        let Some(parent) = dir.parent() else {
            return;
        };
        parent.to_path_buf()
    };

    let repo = storage_path();
    // CAN NOT remove .libra & current dir
    while !is_sub_path(&repo, &dir) && !is_cur_dir(&dir) {
        if is_empty_dir(&dir) {
            if fs::remove_dir(&dir).is_err() {
                break;
            }
        } else {
            break; // once meet a non-empty dir, stop
        }
        dir.pop();
    }
}

pub fn is_empty_dir(dir: &Path) -> bool {
    if !dir.is_dir() {
        return false;
    }
    match fs::read_dir(dir) {
        Ok(mut entries) => entries.next().is_none(),
        Err(_) => false,
    }
}

pub fn is_cur_dir(dir: &Path) -> bool {
    let Ok(dir) = dir.absolutize() else {
        return false;
    };
    let current_dir = cur_dir();
    let Ok(current) = current_dir.absolutize() else {
        return false;
    };
    dir == current
}

/// Transform a path to a string.
///
/// This function uses `to_string_lossy()` which converts the path to a string,
/// replacing any invalid UTF-8 sequences with the Unicode replacement character (U+FFFD).
/// This is preferred over `into_os_string().into_string().unwrap()` which would panic
/// on non-UTF-8 paths. The path separators are preserved as-is by the underlying OS
/// path representation; this function does not perform separator normalization.
pub fn path_to_string(path: &Path) -> String {
    path.to_string_lossy().to_string()
}

#[derive(Debug, thiserror::Error)]
pub enum CommitBaseError {
    #[error("HEAD does not point to a commit")]
    HeadUnborn,
    #[error("{0}")]
    InvalidReference(String),
    #[error("{0}")]
    ReadFailure(String),
    #[error("{0}")]
    CorruptReference(String),
}

impl CommitBaseError {
    fn from_branch_store_error(context: String, error: BranchStoreError) -> Self {
        let message = format!("{context}: {error}");
        match error {
            BranchStoreError::Query(_) | BranchStoreError::Delete { .. } => {
                Self::ReadFailure(message)
            }
            BranchStoreError::Corrupt { .. } => Self::CorruptReference(message),
            BranchStoreError::NotFound(_) => Self::InvalidReference(message),
        }
    }

    fn classify_storage_failure(message: String) -> Self {
        let lower = message.to_ascii_lowercase();
        if lower.contains("database is locked")
            || lower.contains("database schema is locked")
            || lower.contains("permission denied")
            || lower.contains("input/output error")
            || lower.contains("failed to read")
            || lower.contains("could not read")
            || lower.contains("failed to query")
        {
            Self::ReadFailure(message)
        } else {
            Self::CorruptReference(message)
        }
    }
}

async fn resolve_branch_commit_typed(
    branch_name: &str,
    remote: Option<&str>,
    display_name: &str,
) -> Result<Option<ObjectHash>, CommitBaseError> {
    let context = match remote {
        Some(remote_name) => {
            format!("failed to resolve branch '{display_name}' on remote '{remote_name}'")
        }
        None => format!("failed to resolve branch '{display_name}'"),
    };

    match Branch::find_branch_result(branch_name, remote).await {
        Ok(Some(branch)) => Ok(Some(branch.commit)),
        Ok(None) => match Branch::exists_result(branch_name, remote).await {
            Ok(true) => Err(CommitBaseError::InvalidReference(format!(
                "branch '{display_name}' does not point to a commit"
            ))),
            Ok(false) => Ok(None),
            Err(error) => Err(CommitBaseError::from_branch_store_error(context, error)),
        },
        Err(error) => Err(CommitBaseError::from_branch_store_error(context, error)),
    }
}

fn split_revision_navigation(name: &str) -> Option<(&str, &str)> {
    name.char_indices()
        .find(|(_, ch)| *ch == '~' || *ch == '^')
        .map(|(index, _)| name.split_at(index))
}

pub(crate) fn remote_tracking_candidates(name: &str) -> impl Iterator<Item = (&str, &str)> + '_ {
    name.char_indices().filter_map(|(index, ch)| {
        if ch != '/' {
            return None;
        }

        let remote = &name[..index];
        let branch_name = &name[index + 1..];
        (!remote.is_empty() && !branch_name.is_empty()).then_some((remote, branch_name))
    })
}

fn nth_parent_commit_typed(
    commit_id: &ObjectHash,
    n: usize,
    display_name: &str,
) -> Result<ObjectHash, CommitBaseError> {
    let commit: Commit = load_object(commit_id).map_err(|error| {
        CommitBaseError::classify_storage_failure(format!(
            "failed to load commit object while resolving '{display_name}': {error}"
        ))
    })?;

    if n == 0 || n > commit.parent_commit_ids.len() {
        return Err(CommitBaseError::InvalidReference(format!(
            "invalid reference: {display_name}"
        )));
    }

    Ok(commit.parent_commit_ids[n - 1])
}

fn navigate_commit_path_typed(
    mut current: ObjectHash,
    path: &str,
    display_name: &str,
) -> Result<ObjectHash, CommitBaseError> {
    let mut chars = path.chars().peekable();

    while let Some(symbol) = chars.next() {
        if symbol != '^' && symbol != '~' {
            return Err(CommitBaseError::InvalidReference(format!(
                "invalid reference: {display_name}"
            )));
        }

        let mut digits = String::new();
        while let Some(ch) = chars.peek() {
            if ch.is_ascii_digit() {
                digits.push(*ch);
                chars.next();
            } else {
                break;
            }
        }

        let step = if digits.is_empty() {
            1
        } else {
            digits.parse::<usize>().map_err(|_| {
                CommitBaseError::InvalidReference(format!("invalid reference: {display_name}"))
            })?
        };

        if step == 0 {
            // `~0` is identity. `^0` is also identity here because
            // `resolve_commit_base_atom_typed()` already peels named tags and
            // direct tag-object hashes to their referenced object before
            // navigation runs.
            continue;
        }

        match symbol {
            '^' => {
                current = nth_parent_commit_typed(&current, step, display_name)?;
            }
            '~' => {
                for _ in 0..step {
                    current = nth_parent_commit_typed(&current, 1, display_name)?;
                }
            }
            // INVARIANT: the leading `symbol != '^' && symbol != '~'` guard at
            // line 727 returns InvalidReference for every other character, so
            // by this match the only reachable values are '^' and '~'.
            _ => unreachable!("symbol guard above rejects all other chars"),
        }
    }

    Ok(current)
}

async fn resolve_commit_base_atom_typed(name: &str) -> Result<ObjectHash, CommitBaseError> {
    // 1. Check for HEAD
    if name == "HEAD" {
        return match Head::current_commit_result().await {
            Ok(Some(commit_id)) => Ok(commit_id),
            Ok(None) => Err(CommitBaseError::HeadUnborn),
            Err(error) => Err(CommitBaseError::from_branch_store_error(
                "failed to resolve HEAD".to_string(),
                error,
            )),
        };
    }

    // 2. Check for a local branch
    if let Some(commit) = resolve_branch_commit_typed(name, None, name).await? {
        return Ok(commit);
    }

    // Support both short remote branches (`origin/main`) and fetched
    // remote-tracking refs (`refs/remotes/origin/main`), including multi-segment
    // remotes like `upstream/origin/main`.
    if let Some(short_name) = name.strip_prefix("refs/remotes/") {
        if let Some(commit) = resolve_branch_commit_typed(name, None, name).await? {
            return Ok(commit);
        }

        for (remote, branch_name) in remote_tracking_candidates(short_name) {
            if let Some(commit) = resolve_branch_commit_typed(name, Some(remote), name).await? {
                return Ok(commit);
            }

            if let Some(commit) =
                resolve_branch_commit_typed(branch_name, Some(remote), name).await?
            {
                return Ok(commit);
            }
        }
    } else {
        for (remote, branch_name) in remote_tracking_candidates(name) {
            if let Some(commit) = resolve_branch_commit_typed(
                &format!("refs/remotes/{remote}/{branch_name}"),
                Some(remote),
                name,
            )
            .await?
            {
                return Ok(commit);
            }

            if let Some(commit) =
                resolve_branch_commit_typed(branch_name, Some(remote), name).await?
            {
                return Ok(commit);
            }
        }
    }

    // 3. Check for a tag
    match tag::find_tag_and_commit(name).await {
        Ok(Some((_tag_object, commit))) => return Ok(commit.id),
        Ok(None) => {}
        Err(error) => {
            return Err(CommitBaseError::classify_storage_failure(format!(
                "failed to resolve tag '{name}': {error}"
            )));
        }
    }

    // 4. Check for a hash prefix
    let storage = objects_storage();
    let commits = storage.search_result(name).await.map_err(|error| {
        CommitBaseError::classify_storage_failure(format!(
            "failed to search objects while resolving '{name}': {error}"
        ))
    })?;
    if commits.is_empty() {
        return Err(CommitBaseError::InvalidReference(format!(
            "invalid reference: {name}"
        )));
    } else if commits.len() > 1 {
        return Err(CommitBaseError::InvalidReference(format!(
            "ambiguous argument: {name}"
        )));
    }

    let object_id = commits[0];
    let object_type = storage.get_object_type(&object_id).map_err(|e| {
        CommitBaseError::classify_storage_failure(format!(
            "could not read object type for {name}: {e}"
        ))
    })?;

    match object_type {
        ObjectType::Commit => Ok(object_id),
        ObjectType::Tag => peel_tag_hash_to_commit(&storage, object_id, name),
        _ => Err(CommitBaseError::InvalidReference(format!(
            "reference is not a commit: {name}, is {object_type}"
        ))),
    }
}

fn peel_tag_hash_to_commit(
    storage: &ClientStorage,
    object_id: ObjectHash,
    display_name: &str,
) -> Result<ObjectHash, CommitBaseError> {
    let mut current = object_id;
    let mut seen = HashSet::new();

    loop {
        if !seen.insert(current) {
            return Err(CommitBaseError::CorruptReference(format!(
                "tag cycle detected while resolving '{display_name}'"
            )));
        }

        let tag_obj: git_internal::internal::object::tag::Tag =
            load_object(&current).map_err(|error| {
                CommitBaseError::classify_storage_failure(format!(
                    "failed to load tag object while resolving '{display_name}': {error}"
                ))
            })?;
        let target_type = storage
            .get_object_type(&tag_obj.object_hash)
            .map_err(|error| {
                CommitBaseError::classify_storage_failure(format!(
                    "could not read tag target type while resolving '{display_name}': {error}"
                ))
            })?;

        match target_type {
            ObjectType::Commit => return Ok(tag_obj.object_hash),
            ObjectType::Tag => current = tag_obj.object_hash,
            _ => {
                return Err(CommitBaseError::InvalidReference(format!(
                    "reference is not a commit: {display_name}, tag points to {target_type}"
                )));
            }
        }
    }
}

pub async fn get_commit_base_typed(name: &str) -> Result<ObjectHash, CommitBaseError> {
    if let Some((base_ref, path)) = split_revision_navigation(name) {
        if base_ref.is_empty() {
            return Err(CommitBaseError::InvalidReference(format!(
                "invalid reference: {name}"
            )));
        }

        let base_commit = resolve_commit_base_atom_typed(base_ref).await?;
        return navigate_commit_path_typed(base_commit, path, name);
    }

    resolve_commit_base_atom_typed(name).await
}

/// Resolve a string to a commit [`ObjectHash`].
/// The string can be a local branch name, a remote-tracking branch name
/// (such as `origin/main`), a tag name, or a commit hash prefix.
/// Order of resolution:
/// 1. HEAD
/// 2. Local branch
/// 3. Remote-tracking branch (e.g. `origin/main`)
/// 4. Tag
/// 5. Commit hash prefix
pub async fn get_commit_base(name: &str) -> Result<ObjectHash, String> {
    get_commit_base_typed(name)
        .await
        .map_err(|error| format!("fatal: {error}"))
}

/// Get the repository name from the url
/// - e.g. `https://github.com/libra-tools/mega.git/` -> mega
/// - e.g. `https://github.com/libra-tools/mega.git` -> mega
pub fn get_repo_name_from_url(mut url: &str) -> Option<&str> {
    if url.ends_with('/') {
        url = &url[..url.len() - 1];
    }

    let repo_start = url.rfind('/')? + 1;
    let repo = &url[repo_start..];
    if repo.is_empty() {
        return None;
    }

    let repo = repo.strip_suffix(".git").unwrap_or(repo);
    if repo.is_empty() { None } else { Some(repo) }
}

/// Find the appropriate unit and value for Bytes.
/// ### Examples
/// - 1024 bytes -> 1 KiB
/// - 1024 * 1024 bytes -> 1 MiB
pub fn auto_unit_bytes(bytes: u64) -> byte_unit::AdjustedByte {
    let bytes = byte_unit::Byte::from(bytes);
    bytes.get_appropriate_unit(byte_unit::UnitType::Binary)
}
/// Create a default style progress bar
pub fn default_progress_bar(len: u64) -> ProgressBar {
    let progress_bar = ProgressBar::new(len);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template("{spinner:.magenta} [{elapsed_precise}] [{bar:40.green/white}] {bytes}/{total_bytes} ({eta}) {bytes_per_sec}")
            // INVARIANT: the template string is a compile-time literal whose
            // placeholders are validated by indicatif at parse time; this is
            // covered by every command that uses default_progress_bar().
            .expect("default progress bar template is a valid indicatif format string")
            .progress_chars("=> "),
    );
    progress_bar
}

/// Returns `true` when any component of `target_file`, taken relative to
/// `work_dir`, is a literal `.git` entry (see [`GIT_DIR`]). Only the portion
/// below `work_dir` is inspected, so a repository that merely lives under an
/// ancestor path containing `.git` is unaffected.
fn path_has_git_dir_component(work_dir: &Path, target_file: &Path) -> bool {
    let relative = target_file.strip_prefix(work_dir).unwrap_or(target_file);
    relative
        .components()
        .any(|component| component.as_os_str() == OsStr::new(GIT_DIR))
}

/// Check each directory level from `work_dir` to `target_file` to see if an
/// ignore source matches `target_file`.
///
/// `.git` is always treated as ignored (like Git) regardless of any ignore
/// rule.
///
/// Low-level helper historically used by status/add flows. Prefer the higher-level wrappers in
/// `crate::utils::ignore::{should_ignore, filter_workdir_paths}` so that ignore policies and index
/// awareness stay consistent. Call this directly only when you explicitly need raw ignore-source
/// parsing.
///
/// Assume `target_file` is in `work_dir`.
pub fn check_gitignore(work_dir: &Path, target_file: &Path) -> bool {
    assert!(target_file.starts_with(work_dir));

    // Git hardcodes ignoring `.git`. Mirror that here, before consulting any
    // `.libraignore` file, so a nested Git repository's metadata is always
    // treated as ignored and can never be un-ignored by a `.libraignore`
    // whitelist rule (e.g. `!.git`).
    if path_has_git_dir_component(work_dir, target_file) {
        return true;
    }

    // lore.md 2.4: a materialized layer-overlay path is UN-NEGATABLY excluded
    // (above every `.libraignore` rule) so a purely-local overlay is never
    // swept into `status`/`add`. Zero overhead with no layers (empty
    // snapshot). The `add` staging guard is the airtight backstop for
    // `--force` (which extends ignored files back into the staged set).
    if let Ok(relative) = target_file.strip_prefix(work_dir)
        && let Some(key) = crate::internal::layer::normalize_key(relative)
        && crate::internal::layer::is_layer_owned(&key)
    {
        return true;
    }

    for source in ignore_sources_for_target(work_dir, target_file) {
        let ignore = cached_ignore_file(&source.path, &source.base);
        if let Some(verdict) = ignore_verdict(&ignore, work_dir, target_file) {
            return verdict;
        }
    }

    false
}

/// The deciding ignore pattern for a path, produced by
/// [`check_gitignore_match`] for `check-ignore -v` output.
pub struct IgnoreMatchInfo {
    /// The ignore file that supplied the deciding pattern. `None` for
    /// the built-in `.git` rule.
    pub source: Option<PathBuf>,
    /// 1-based line number of the pattern within `source`, recovered by scanning
    /// the file (the matcher engine does not expose it). `None` when it cannot
    /// be located or for the built-in rule.
    pub line: Option<usize>,
    /// The deciding pattern exactly as written in the source (e.g. `*.tmp` or
    /// `!keep`). Empty for the built-in `.git` rule.
    pub pattern: String,
    /// `true` when the deciding pattern ignores the path; `false` for a
    /// whitelist (`!`) override.
    pub ignored: bool,
}

/// Like [`check_gitignore`] but returns the deciding pattern's source file,
/// line, and text — the detail `check-ignore -v` reports. Returns `Some(info)`
/// when an ignore pattern (or the built-in `.git` rule) decides the
/// path's status (`info.ignored` distinguishes an ignore match from a whitelist
/// override) and `None` when no pattern applies. The walk order matches
/// [`check_gitignore`] exactly (nearest per-directory ignore source first, last
/// matching pattern within a file wins), so the two never disagree on the verdict.
pub fn check_gitignore_match(work_dir: &Path, target_file: &Path) -> Option<IgnoreMatchInfo> {
    assert!(target_file.starts_with(work_dir));

    if path_has_git_dir_component(work_dir, target_file) {
        return Some(IgnoreMatchInfo {
            source: None,
            line: None,
            pattern: String::new(),
            ignored: true,
        });
    }

    for source in ignore_sources_for_target(work_dir, target_file) {
        let ignore = cached_ignore_file(&source.path, &source.base);
        if let Some(info) = ignore_match_info(&ignore, work_dir, target_file) {
            return Some(info);
        }
    }

    None
}

struct IgnoreSource {
    path: PathBuf,
    base: PathBuf,
}

fn ignore_sources_for_target(work_dir: &Path, target_file: &Path) -> Vec<IgnoreSource> {
    let mut sources = Vec::new();
    let mut dir = target_file.to_path_buf();
    dir.pop();

    while dir.starts_with(work_dir) {
        push_ignore_source(&mut sources, dir.join(LIBRAIGNORE_FILE), dir.clone());
        push_ignore_source(&mut sources, dir.join(GITIGNORE_FILE), dir.clone());
        dir.pop();
    }

    if let Some(info_exclude) = git_info_file_path(work_dir, "exclude") {
        push_ignore_source(&mut sources, info_exclude, work_dir.to_path_buf());
    }
    if let Some(configured) = optional_cascaded_config_path(CORE_EXCLUDES_FILE_KEY, work_dir) {
        push_ignore_source(&mut sources, configured, work_dir.to_path_buf());
    }
    sources
}

fn push_ignore_source(sources: &mut Vec<IgnoreSource>, path: PathBuf, base: PathBuf) {
    if path.exists() {
        sources.push(IgnoreSource { path, base });
    }
}

fn ignore_verdict(ignore: &Gitignore, work_dir: &Path, target_file: &Path) -> Option<bool> {
    ignore_match_info(ignore, work_dir, target_file).map(|info| info.ignored)
}

fn ignore_match_info(
    ignore: &Gitignore,
    work_dir: &Path,
    target_file: &Path,
) -> Option<IgnoreMatchInfo> {
    if let Some(info) = glob_match_info(ignore.matched(target_file, target_file.is_dir())) {
        return Some(info);
    }

    let mut parent_dir = if target_file.is_dir() {
        target_file.to_path_buf()
    } else {
        target_file
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| work_dir.to_path_buf())
    };
    while parent_dir.starts_with(work_dir) {
        if let Some(info) = glob_match_info(ignore.matched(&parent_dir, true)) {
            return Some(info);
        }
        parent_dir.pop();
    }

    None
}

/// Convert an `ignore` crate match into [`IgnoreMatchInfo`], recovering the
/// source/line/pattern of the deciding glob. `Match::None` yields `None`.
fn glob_match_info(m: Match<&ignore::gitignore::Glob>) -> Option<IgnoreMatchInfo> {
    let (glob, ignored) = match m {
        Match::Ignore(glob) => (glob, true),
        Match::Whitelist(glob) => (glob, false),
        Match::None => return None,
    };
    let source = glob.from().map(Path::to_path_buf);
    let pattern = glob.original().to_string();
    let line = source
        .as_deref()
        .and_then(|path| find_pattern_line(path, &pattern));
    Some(IgnoreMatchInfo {
        source,
        line,
        pattern,
        ignored,
    })
}

/// Best-effort recovery of a pattern's 1-based line number in an ignore
/// file: the first non-blank, non-comment line whose trimmed content equals
/// `pattern`. The matcher engine does not expose line numbers, so `check-ignore
/// -v` reconstructs them here. Returns `None` if the file cannot be read or the
/// pattern is not found verbatim.
fn find_pattern_line(source: &Path, pattern: &str) -> Option<usize> {
    let contents = fs::read_to_string(source).ok()?;
    // Return the LAST matching line: within one `.libraignore`, the last pattern
    // that matches a path is the deciding one, so on duplicate identical
    // patterns the later line is the rule the matcher actually applied.
    contents
        .lines()
        .enumerate()
        .filter(|(_, line)| {
            let trimmed = line.trim();
            !trimmed.is_empty() && !trimmed.starts_with('#') && trimmed == pattern
        })
        .last()
        .map(|(idx, _)| idx + 1)
}

fn cached_ignore_file(ignore_path: &Path, base: &Path) -> Arc<Gitignore> {
    let Ok(metadata) = fs::metadata(ignore_path) else {
        return load_ignore_file(ignore_path, base);
    };
    let Ok(modified) = metadata.modified() else {
        return load_ignore_file(ignore_path, base);
    };
    let len = metadata.len();
    let key = IgnoreCacheKey {
        source: ignore_path.to_path_buf(),
        base: base.to_path_buf(),
    };

    let mut cache = match LIBRAIGNORE_CACHE.lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cached) = cache.get(&key)
        && cached.len == len
        && cached.modified == modified
    {
        return Arc::clone(&cached.matcher);
    }

    let matcher = load_ignore_file(ignore_path, base);
    cache.insert(
        key,
        CachedGitignore {
            len,
            modified,
            matcher: Arc::clone(&matcher),
        },
    );
    matcher
}

/// Build an in-memory gitignore matcher from explicit exclude patterns supplied
/// on the command line (e.g. `ls-files -x <pattern>` / `-X <file>`), rooted at
/// `work_dir`. Returns `None` when there are no patterns, so callers can skip the
/// match entirely. Pattern syntax matches Git ignore files (the same engine).
pub fn build_exclude_matcher(
    work_dir: &Path,
    patterns: &[String],
) -> Result<Option<Gitignore>, String> {
    if patterns.is_empty() {
        return Ok(None);
    }
    let mut builder = GitignoreBuilder::new(work_dir);
    for pattern in patterns {
        builder
            .add_line(None, pattern)
            .map_err(|error| format!("invalid exclude pattern '{pattern}': {error}"))?;
    }
    builder
        .build()
        .map(Some)
        .map_err(|error| format!("failed to compile exclude patterns: {error}"))
}

/// Three-state verdict of an explicit-exclude matcher built by
/// [`build_exclude_matcher`] for `abs_path` (under `work_dir`). `Some(true)`
/// means excluded; `Some(false)` means explicitly re-included by a negation (a
/// higher-precedence source — the caller should let this override lower-priority
/// standard excludes); `None` means no explicit pattern matched
/// (defer to the standard excludes).
///
/// Honors Git's parent-directory dominance: once an ancestor directory is
/// excluded, the path is excluded regardless of a child negation (e.g. the pair
/// `build/` and `!build/keep.txt` still excludes `build/keep.txt`). Ancestors
/// are walked top-down; the first ancestor whose last-matching rule is `Ignore`
/// excludes everything beneath it.
pub fn exclude_matcher_verdict(
    matcher: &Gitignore,
    work_dir: &Path,
    abs_path: &Path,
    is_dir: bool,
) -> Option<bool> {
    if let Ok(rel) = abs_path.strip_prefix(work_dir) {
        let comps: Vec<_> = rel.components().collect();
        let mut cur = work_dir.to_path_buf();
        for comp in comps.iter().take(comps.len().saturating_sub(1)) {
            cur.push(comp.as_os_str());
            if matches!(matcher.matched(&cur, true), Match::Ignore(_)) {
                return Some(true);
            }
        }
    }
    match matcher.matched(abs_path, is_dir) {
        Match::Ignore(_) => Some(true),
        Match::Whitelist(_) => Some(false),
        Match::None => None,
    }
}

fn load_ignore_file(ignore_path: &Path, base: &Path) -> Arc<Gitignore> {
    let mut builder = GitignoreBuilder::new(base);
    match fs::read_to_string(ignore_path) {
        Ok(contents) => {
            for line in contents.lines() {
                if let Err(error) = builder.add_line(Some(ignore_path.to_path_buf()), line) {
                    eprintln!(
                        "warning: invalid ignore pattern in {}: {error}",
                        ignore_path.display()
                    );
                }
            }
        }
        Err(error) => {
            eprintln!(
                "warning: failed to read ignore file {}: {error}",
                ignore_path.display()
            );
        }
    }
    match builder.build() {
        Ok(ignore) => Arc::new(ignore),
        Err(error) => {
            eprintln!(
                "warning: failed to compile ignore file {}: {error}",
                ignore_path.display()
            );
            let (empty, _) = Gitignore::new(base.join(".libra-empty-ignore-fallback"));
            Arc::new(empty)
        }
    }
}

use git_internal::internal::object::signature::{Signature, SignatureType};

pub async fn create_signatures() -> (Signature, Signature) {
    let user_name = ConfigKv::get("user.name")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| "Stasher".to_string());
    let user_email = ConfigKv::get("user.email")
        .await
        .ok()
        .flatten()
        .map(|e| e.value)
        .unwrap_or_else(|| "stasher@example.com".to_string());

    let author = Signature::new(SignatureType::Author, user_name.clone(), user_email.clone());
    let committer = Signature::new(SignatureType::Committer, user_name, user_email);
    (author, committer)
}

/// Compute the minimum prefix length at which all commit IDs are uniquely identifiable.
///
/// This function inspects the textual object IDs of all `commits` and searches for the
/// smallest prefix length `len` such that the first `len` characters of every commit ID
/// are pairwise distinct. The search range is from `7` (inclusive) up to the maximum
/// hash string length present in `commits` (inclusive).
///
/// Return value semantics:
/// - If `commits` is empty or contains only a single commit, this returns `7`. In these
///   cases, there is no ambiguity, and the conventional minimal prefix length is used.
/// - Otherwise, it returns the smallest `len >= 7` for which all commit ID prefixes of
///   length `len` are unique.
/// - If no such `len` exists before the end of the hash strings, the full hash length
///   (i.e., the maximum ID length observed) is returned.
///
/// This is useful for producing short, Git-style abbreviated IDs that remain unambiguous
/// across the given set of reachable commits.
pub fn get_min_unique_hash_length(commits: &[Commit]) -> usize {
    // Get all commit IDs.
    let hashes: Vec<String> = commits.iter().map(|commit| commit.id.to_string()).collect();
    // If there is no commit or only one commit, return 7.
    if hashes.is_empty() || hashes.len() == 1 {
        7
    } else {
        // Get the maximum length of all commit IDs.
        let max_length = hashes.iter().map(|h| h.len()).max().unwrap_or(0);
        (7..=max_length)
            .find(|&len| {
                let mut prefixes = HashSet::new();
                hashes
                    .iter()
                    // unwrap_or is safe: returns the full hash if slice fails
                    .all(|hash| prefixes.insert(hash.get(0..len).unwrap_or(hash)))
            })
            .unwrap_or(max_length) // Worst case: use full hash length
    }
}

/// Compare two ref names with "version" ordering (`--sort=version:refname`):
/// runs of digits compare numerically (so `v1.9` sorts before `v1.10`) while
/// other runs compare lexically. Shared by `ls-remote` and `for-each-ref`.
pub(crate) fn version_refname_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;

    let mut left_chars = left.chars().peekable();
    let mut right_chars = right.chars().peekable();

    while left_chars.peek().is_some() && right_chars.peek().is_some() {
        let left_is_digit = left_chars.peek().is_some_and(|ch| ch.is_ascii_digit());
        let right_is_digit = right_chars.peek().is_some_and(|ch| ch.is_ascii_digit());
        let left_run = take_char_run(&mut left_chars, left_is_digit);
        let right_run = take_char_run(&mut right_chars, right_is_digit);
        let ordering = if left_is_digit && right_is_digit {
            numeric_run_cmp(&left_run, &right_run)
        } else {
            left_run.cmp(&right_run)
        };
        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    left_chars
        .peek()
        .is_some()
        .cmp(&right_chars.peek().is_some())
}

fn take_char_run<I>(chars: &mut std::iter::Peekable<I>, want_digit: bool) -> String
where
    I: Iterator<Item = char>,
{
    let mut run = String::new();
    while chars
        .peek()
        .is_some_and(|ch| ch.is_ascii_digit() == want_digit)
    {
        if let Some(ch) = chars.next() {
            run.push(ch);
        }
    }
    run
}

fn numeric_run_cmp(left: &str, right: &str) -> std::cmp::Ordering {
    let left_trimmed = left.trim_start_matches('0');
    let right_trimmed = right.trim_start_matches('0');
    let left_norm = if left_trimmed.is_empty() {
        "0"
    } else {
        left_trimmed
    };
    let right_norm = if right_trimmed.is_empty() {
        "0"
    } else {
        right_trimmed
    };

    left_norm
        .len()
        .cmp(&right_norm.len())
        .then_with(|| left_norm.cmp(right_norm))
        .then_with(|| left.len().cmp(&right.len()))
}

/// Validate a full reference name against Git's `check-ref-format` rules.
///
/// Accepts `HEAD` and any `refs/<...>` name; rejects names that Git would
/// reject: an empty/leading-slash/trailing-slash body, a trailing `.`, a
/// `.lock` suffix, `//`, `..`, `@{`, any path component that is empty / starts
/// with `.` / ends with `.lock`, and ASCII control bytes, the ASCII space, or
/// any of `: \ ~ ^ ? * [`. Bytes above ASCII (incl. Unicode whitespace) are
/// accepted, matching Git. Used wherever a user supplies a ref name (e.g.
/// `show-ref --exclude-existing`, `format-patch --notes=<ref>`).
pub fn is_valid_refname(refname: &str) -> bool {
    if refname == "HEAD" {
        return true;
    }

    let Some(short) = refname.strip_prefix("refs/") else {
        return false;
    };
    if short.is_empty()
        || short.starts_with('/')
        || short.ends_with('/')
        || short.ends_with('.')
        || short.ends_with(".lock")
        || short.contains("//")
        || short.contains("..")
        || short.contains("@{")
    {
        return false;
    }
    if short.split('/').any(|component| {
        component.is_empty() || component.starts_with('.') || component.ends_with(".lock")
    }) {
        return false;
    }

    // Git's `check-ref-format` forbids ASCII control bytes and the ASCII space,
    // plus the punctuation below — but it accepts bytes above ASCII, so Unicode
    // whitespace (NBSP, EM SPACE, …) is allowed. Use an ASCII-only space test
    // rather than `char::is_whitespace`, which would over-reject those.
    !short.chars().any(|c| {
        c.is_ascii_control() || c == ' ' || matches!(c, ':' | '\\' | '~' | '^' | '?' | '*' | '[')
    })
}

#[cfg(test)]
mod test {
    use std::{env, path::PathBuf};

    use git_internal::internal::object::{
        signature::{Signature, SignatureType},
        tag::Tag as GitTag,
    };
    use sea_orm::{ActiveModelTrait, Set};
    use serial_test::serial;
    use tempfile::tempdir;

    use super::*;
    use crate::{
        command::{
            add::{self, AddArgs},
            commit::{self, CommitArgs},
            save_object,
        },
        internal::{db::get_db_conn_instance, head::Head, model::reference, tag as internal_tag},
        utils::test,
    };

    #[test]
    fn is_valid_refname_matches_git_check_ref_format() {
        // Accepted.
        assert!(is_valid_refname("HEAD"));
        assert!(is_valid_refname("refs/heads/main"));
        assert!(is_valid_refname("refs/notes/commits"));
        assert!(is_valid_refname("refs/notes/team/review"));
        assert!(is_valid_refname("refs/notes/my-notes"));
        // Git accepts bytes above ASCII, including Unicode whitespace.
        assert!(is_valid_refname("refs/notes/foo\u{a0}bar"));
        assert!(is_valid_refname("refs/notes/foo\u{2003}bar"));

        // Rejected: not under refs/, structural, and forbidden punctuation.
        assert!(!is_valid_refname("main"));
        assert!(!is_valid_refname("refs/"));
        assert!(!is_valid_refname("refs/notes/"));
        assert!(!is_valid_refname("refs/heads/bad name")); // ASCII space
        assert!(!is_valid_refname("refs/notes/bad..ref"));
        assert!(!is_valid_refname("refs/notes/bad~ref"));
        assert!(!is_valid_refname("refs/notes/.hidden"));
        assert!(!is_valid_refname("refs/notes/foo.lock"));
        assert!(!is_valid_refname("refs/notes/bad@{ref"));
        assert!(!is_valid_refname("refs/notes/foo/"));
        assert!(!is_valid_refname("refs/notes/foo."));
    }

    fn test_tag_object(object_hash: ObjectHash, object_type: ObjectType, name: &str) -> GitTag {
        GitTag::new(
            object_hash,
            object_type,
            name.to_string(),
            Signature {
                signature_type: SignatureType::Tagger,
                name: "tester".to_string(),
                email: "tester@example.com".to_string(),
                timestamp: 1,
                timezone: "+0000".to_string(),
            },
            format!("{name} message"),
        )
    }

    #[test]
    ///Test get current directory success.
    fn cur_dir_returns_current_directory() {
        match env::current_dir() {
            Ok(expected) => {
                let actual = cur_dir();
                assert_eq!(actual, expected);
            }
            Err(_) => {
                // On some Linux/CI environments, current_dir can fail if the working
                // directory was removed. In that case, ensure cur_dir still returns
                // a stable, existing directory via its fallback logic.
                let actual = cur_dir();
                assert!(actual.exists(), "cur_dir should return an existing path");
                assert!(actual.is_dir(), "cur_dir should point to a directory");
            }
        }
    }

    #[test]
    #[serial]
    ///Test the function of is_sub_path.
    fn test_is_sub_path() {
        let _guard = test::ChangeDirGuard::new(Path::new(env!("CARGO_MANIFEST_DIR")));

        assert!(is_sub_path("src/main.rs", "src"));
        assert!(is_sub_path("src/main.rs", "src/"));
        assert!(is_sub_path("src/main.rs", "src/main.rs"));
        assert!(is_sub_path("src/main.rs", "."));
    }

    /// Containment is **component-wise**, never a byte prefix: a sibling
    /// whose name merely starts with the parent's name (`srcfoo` vs
    /// `src`) must NOT be treated as inside the parent, and an unrelated
    /// sibling is likewise rejected. If `is_sub_path` ever regressed to
    /// a string `starts_with`, `srcfoo/x` would falsely read as inside
    /// `src` — a scope-escape. Pin the rejection.
    #[test]
    #[serial]
    fn test_is_sub_path_rejects_byte_prefix_sibling_and_unrelated_paths() {
        let _guard = test::ChangeDirGuard::new(Path::new(env!("CARGO_MANIFEST_DIR")));

        // Positive control: a genuine child IS inside, so a regression
        // that returned `false` for everything can't make the negative
        // assertions below pass vacuously.
        assert!(is_sub_path("src/main.rs", "src"), "genuine child is inside");

        assert!(
            !is_sub_path("srcfoo/x.rs", "src"),
            "a byte-prefix sibling must not be inside the parent",
        );
        assert!(
            !is_sub_path("srcfoo", "src"),
            "the byte-prefix sibling dir itself must not be inside the parent",
        );
        assert!(
            !is_sub_path("lib/x.rs", "src"),
            "an unrelated sibling must not be inside the parent",
        );
    }

    /// Interior `..` is resolved before the containment check: a
    /// `..` that stays within the parent keeps the path in scope, while
    /// a `..` that climbs out to a sibling escapes it. Pins that the
    /// normalization happens before `starts_with`, not after.
    ///
    /// Covers both input branches: relative inputs (resolved by
    /// `.absolutize()`) and absolute inputs (resolved by the internal
    /// `normalize_abs_path`), so the absolute branch — exercised
    /// otherwise only by the root-escape test — is pinned for the
    /// in-scope / sibling-escape cases too.
    #[test]
    #[serial]
    fn test_is_sub_path_resolves_interior_parent_dir() {
        let _guard = test::ChangeDirGuard::new(Path::new(env!("CARGO_MANIFEST_DIR")));

        // Relative inputs (the `.absolutize()` branch):
        // src/sub/../main.rs -> src/main.rs, still under src.
        assert!(is_sub_path("src/sub/../main.rs", "src"));
        // src/../lib/x.rs -> lib/x.rs, NOT under src.
        assert!(!is_sub_path("src/../lib/x.rs", "src"));

        // Absolute inputs (the `normalize_abs_path` branch): same
        // interior-`..` semantics, independent of the current dir.
        assert!(is_sub_path("/repo/src/sub/../main.rs", "/repo/src"));
        assert!(!is_sub_path("/repo/src/../lib/x.rs", "/repo/src"));
    }

    #[test]
    fn test_is_sub_path_parent_dir_cannot_escape_root() {
        assert!(!is_sub_path("/../../etc/passwd", "/tmp"));
    }

    #[cfg(windows)]
    #[test]
    fn test_is_sub_path_preserves_windows_prefix() {
        assert!(is_sub_path(r"C:\repo\sub\..\file.txt", r"C:\repo"));
        assert!(!is_sub_path(r"C:\repo\..\Windows\System32", r"C:\repo"));
    }

    #[test]
    ///Test the function of to_relative.
    fn test_to_relative() {
        assert_eq!(to_relative("src/main.rs", "src"), PathBuf::from("main.rs"));
        assert_eq!(to_relative(".", "src"), PathBuf::from(".."));
    }

    #[tokio::test]
    #[serial]
    async fn list_workdir_files_prunes_libraignored_directories() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        fs::write(".libraignore", "target/\n").unwrap();
        fs::create_dir_all("target/debug/deps").unwrap();
        fs::write("target/debug/deps/ignored.rlib", "ignored").unwrap();
        fs::write("visible.txt", "visible").unwrap();

        let files = list_workdir_files().unwrap();

        assert!(files.contains(&PathBuf::from(".libraignore")));
        assert!(files.contains(&PathBuf::from("visible.txt")));
        assert!(!files.contains(&PathBuf::from("target/debug/deps/ignored.rlib")));
    }

    #[test]
    fn is_empty_dir_returns_false_for_missing_directory() {
        let temp = tempdir().unwrap();
        let missing = temp.path().join("missing");
        assert!(!is_empty_dir(&missing));
    }

    #[test]
    fn clear_empty_dir_ignores_parentless_paths() {
        clear_empty_dir(Path::new(""));
    }

    #[tokio::test]
    #[serial]
    async fn get_commit_base_typed_rejects_unborn_branch_before_hash_fallback() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let branch_name = head_commit.to_string()[..7].to_string();

        let db = get_db_conn_instance().await;
        reference::ActiveModel {
            name: Set(Some(branch_name.clone())),
            kind: Set(reference::ConfigKind::Branch),
            commit: Set(None),
            remote: Set(None),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("failed to insert unborn branch");

        let error = get_commit_base_typed(&branch_name)
            .await
            .expect_err("unborn branch must not fall back to hash prefix resolution");
        assert!(matches!(error, CommitBaseError::InvalidReference(_)));
        assert!(
            error.to_string().contains(&format!(
                "branch '{branch_name}' does not point to a commit"
            )),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    #[serial]
    async fn get_commit_base_typed_head_navigation_reports_unborn_head() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        let error = get_commit_base_typed("HEAD~1")
            .await
            .expect_err("unborn HEAD navigation must not panic");
        assert!(matches!(error, CommitBaseError::HeadUnborn));
    }

    #[tokio::test]
    #[serial]
    async fn get_commit_base_typed_tag_object_hash_with_caret_zero_resolves_commit() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let created = internal_tag::create("v1.0.0", Some("release".into()), false, false)
            .await
            .expect("failed to create annotated tag");

        let resolved = get_commit_base_typed(&format!("{}^0", created.target))
            .await
            .expect("tag object hash ^0 should resolve to the tagged commit");
        assert_eq!(resolved, head_commit);
    }

    #[tokio::test]
    #[serial]
    async fn get_commit_base_typed_peels_nested_tag_object_hash_to_commit() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let inner = internal_tag::create("inner", Some("inner tag".into()), false, false)
            .await
            .expect("failed to create inner tag");
        let outer = test_tag_object(inner.target, ObjectType::Tag, "outer");
        save_object(&outer, &outer.id).expect("failed to save outer tag object");

        let resolved = get_commit_base_typed(&outer.id.to_string())
            .await
            .expect("nested tag object hash should resolve to commit");

        assert_eq!(resolved, head_commit);
    }

    #[tokio::test]
    #[serial]
    async fn get_commit_base_typed_reports_tag_cycle_as_corruption() {
        let repo = tempdir().unwrap();
        test::setup_with_new_libra_in(repo.path()).await;
        let _guard = test::ChangeDirGuard::new(repo.path());

        test::ensure_file("tracked.txt", Some("tracked\n"));
        add::execute(AddArgs {
            pathspec: vec!["tracked.txt".into()],
            all: false,
            update: false,
            refresh: false,
            verbose: false,
            force: false,
            dry_run: false,
            ignore_errors: false,
            pathspec_from_file: None,
            pathspec_file_nul: false,
            chmod: None,
            renormalize: false,
            ignore_missing: false,
        })
        .await;
        commit::execute(CommitArgs {
            message: Some("base".into()),
            disable_pre: true,
            no_verify: true,
            ..Default::default()
        })
        .await;

        let head_commit = Head::current_commit()
            .await
            .expect("expected committed HEAD");
        let tag_a = test_tag_object(head_commit, ObjectType::Commit, "tag-a");
        let tag_b = test_tag_object(tag_a.id, ObjectType::Tag, "tag-b");
        let tag_a_cycle = test_tag_object(tag_b.id, ObjectType::Tag, "tag-a");
        save_object(&tag_b, &tag_b.id).expect("failed to save tag-b");
        save_object(&tag_a_cycle, &tag_a.id).expect("failed to save cyclic tag-a");

        let error = get_commit_base_typed(&tag_a.id.to_string())
            .await
            .expect_err("tag cycle should fail");

        assert!(matches!(error, CommitBaseError::CorruptReference(_)));
        assert!(error.to_string().contains("tag cycle detected"));
    }

    #[tokio::test]
    #[serial]
    ///Test the function of to_workdir_path.
    async fn test_to_workdir_path() {
        let temp_path = tempdir().unwrap();
        test::setup_with_new_libra_in(temp_path.path()).await;
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        assert_eq!(
            to_workdir_path("./src/abc/../main.rs"),
            PathBuf::from("src/main.rs")
        );
        assert_eq!(to_workdir_path("."), PathBuf::from("."));
        assert_eq!(to_workdir_path("./"), PathBuf::from("."));
        assert_eq!(to_workdir_path(""), PathBuf::from("."));
    }

    #[test]
    #[serial]
    /// Tests that files matching patterns in .libraignore are correctly identified as ignored.
    fn test_check_gitignore_ignore_files() {
        let temp_path = tempdir().unwrap();
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        let mut gitignore_file = fs::File::create(".libraignore").unwrap();
        gitignore_file.write_all(b"*.bar").unwrap();

        let target = temp_path.path().join("tmp/foo.bar");
        assert!(check_gitignore(&temp_path.keep(), &target));
    }

    #[test]
    #[serial]
    /// Tests that directories matching patterns in .libraignore are correctly identified as ignored.
    fn test_check_gitignore_ignore_directory() {
        let temp_path = tempdir().unwrap();
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        let mut gitignore_file = fs::File::create(".libraignore").unwrap();
        gitignore_file.write_all(b"foo/").unwrap();

        let target = temp_path.path().join("foo/bar");
        assert!(check_gitignore(&temp_path.keep(), &target));
    }

    #[test]
    #[serial]
    /// Tests ignore pattern matching in subdirectories with .libraignore files at different directory levels.
    fn test_check_gitignore_ignore_subdirectory_files() {
        let temp_path = tempdir().unwrap();
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        fs::create_dir_all("tmp").unwrap();
        fs::create_dir_all("tmp/tmp1").unwrap();
        fs::create_dir_all("tmp/tmp1/tmp2").unwrap();
        let mut gitignore_file1 = fs::File::create("tmp/.libraignore").unwrap();
        gitignore_file1.write_all(b"*.bar").unwrap();
        let workdir = env::current_dir().unwrap();
        let target = workdir.join("tmp/tmp1/tmp2/foo.bar");
        assert!(check_gitignore(&workdir, &target));
        fs::remove_dir_all(workdir.join("tmp")).unwrap();
    }

    #[test]
    #[serial]
    /// Tests that files not matching patterns in .libraignore are correctly identified as not ignored.
    fn test_check_gitignore_not_ignore() {
        let temp_path = tempdir().unwrap();
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        let mut gitignore_file = fs::File::create(".libraignore").unwrap();
        gitignore_file.write_all(b"*.bar").unwrap();
        let workdir = env::current_dir().unwrap();
        let target = workdir.join("tmp/bar.foo");
        assert!(!check_gitignore(&workdir, &target));
        fs::remove_file(workdir.join(".libraignore")).unwrap();
    }

    #[test]
    #[serial]
    /// Tests that files not matching subdirectory-specific patterns in .libraignore are correctly identified as not ignored.
    fn test_check_gitignore_not_ignore_subdirectory_files() {
        let temp_path = tempdir().unwrap();
        let _guard = test::ChangeDirGuard::new(temp_path.path());

        fs::create_dir_all("tmp").unwrap();
        fs::create_dir_all("tmp/tmp1").unwrap();
        fs::create_dir_all("tmp/tmp1/tmp2").unwrap();
        let mut gitignore_file1 = fs::File::create("tmp/.libraignore").unwrap();
        gitignore_file1.write_all(b"tmp/tmp1/tmp2/*.bar").unwrap();
        let workdir = env::current_dir().unwrap();
        let target = workdir.join("tmp/tmp1/tmp2/foo.bar");
        assert!(!check_gitignore(&workdir, &target));
        fs::remove_dir_all(workdir.join("tmp")).unwrap();
    }

    /// `.git` must always be treated as ignored, like Git, even when a
    /// `.libraignore` whitelist rule (`!.git`) tries to un-ignore it. A nested
    /// `.git` at any depth is covered, while a normal file is left untouched.
    #[test]
    fn test_check_gitignore_force_ignores_git_dir_even_with_whitelist() {
        let temp_path = tempdir().unwrap();
        let workdir = temp_path.path().to_path_buf();
        // A whitelist that tries to un-ignore `.git` must not win.
        fs::write(workdir.join(".libraignore"), "!.git\n!.git/**\n").unwrap();

        assert!(check_gitignore(&workdir, &workdir.join(".git")));
        assert!(check_gitignore(
            &workdir,
            &workdir.join(".git").join("config")
        ));
        // Nested git repository (e.g. a submodule checkout) is covered too.
        assert!(check_gitignore(
            &workdir,
            &workdir.join("sub").join(".git").join("HEAD")
        ));
        // A normal file is still not ignored.
        assert!(!check_gitignore(
            &workdir,
            &workdir.join("src").join("main.rs")
        ));
    }

    #[test]
    fn test_get_repo_name_from_url_with_git_suffix() {
        assert_eq!(
            get_repo_name_from_url("https://example.com/owner/repo.git"),
            Some("repo")
        );
    }

    #[test]
    fn test_get_repo_name_from_url_without_suffix() {
        assert_eq!(
            get_repo_name_from_url("https://example.com/owner/repo"),
            Some("repo")
        );
    }

    #[test]
    fn test_get_repo_name_from_file_url_without_suffix() {
        assert_eq!(
            get_repo_name_from_url("file:///home/user/projects/repo"),
            Some("repo")
        );
    }

    #[test]
    #[serial]
    fn test_try_get_storage_path_ignores_global_libra_dir_without_repo_markers() {
        let temp = tempdir().unwrap();
        let home_like = temp.path();
        let global_libra = home_like.join(".libra");
        fs::create_dir_all(global_libra.join("vault-keys")).unwrap();
        fs::write(global_libra.join("config.db"), b"not a repo db").unwrap();

        let workdir = home_like.join("workspace").join("project");
        fs::create_dir_all(&workdir).unwrap();

        let _guard = test::ChangeDirGuard::new(&workdir);
        let result = try_get_storage_path(None);

        assert!(
            result.is_err(),
            "global ~/.libra directory without repo markers must not be treated as a repository"
        );
    }
    #[test]
    #[serial]
    fn test_try_get_storage_path_accepts_valid_repo_under_ancestor_with_global_libra_dir() {
        let temp = tempdir().unwrap();
        let home_like = temp.path();
        let global_libra = home_like.join(".libra");
        fs::create_dir_all(global_libra.join("vault-keys")).unwrap();
        fs::write(global_libra.join("config.db"), b"not a repo db").unwrap();

        let repo = home_like.join("workspace").join("repo");
        let storage = repo.join(ROOT_DIR);
        fs::create_dir_all(storage.join("objects")).unwrap();
        fs::create_dir_all(storage.join("hooks")).unwrap();
        fs::create_dir_all(storage.join("info")).unwrap();
        fs::write(storage.join(DATABASE), b"repo db").unwrap();
        fs::write(storage.join("info").join("exclude"), b"").unwrap();

        let nested = repo.join("src");
        fs::create_dir_all(&nested).unwrap();

        let _guard = test::ChangeDirGuard::new(&nested);
        let resolved = try_get_storage_path(None).unwrap();

        assert_eq!(
            resolved.canonicalize().unwrap(),
            storage.canonicalize().unwrap()
        );
    }
    #[test]
    #[serial]
    fn test_try_get_storage_path_rejects_libra_dir_with_only_hooks() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("project");
        fs::create_dir_all(&repo).unwrap();

        let libra = repo.join(".libra");
        fs::create_dir_all(libra.join("hooks")).unwrap();

        let _guard = test::ChangeDirGuard::new(&repo);
        let result = try_get_storage_path(None);

        assert!(
            result.is_err(),
            ".libra with only hooks/ should not be treated as a valid repository"
        );
    }
    #[test]
    #[serial]
    fn test_try_get_storage_path_rejects_libra_dir_with_only_objects() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("project");
        fs::create_dir_all(&repo).unwrap();

        let libra = repo.join(".libra");
        fs::create_dir_all(libra.join("objects")).unwrap();

        let _guard = test::ChangeDirGuard::new(&repo);
        let result = try_get_storage_path(None);

        assert!(
            result.is_err(),
            ".libra with only objects/ should not be treated as a valid repository"
        );
    }

    #[test]
    fn test_find_git_repository_detects_worktree_ancestor() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("project");
        let git = repo.join(".git");
        fs::create_dir_all(git.join("objects")).unwrap();
        fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(
            git.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n",
        )
        .unwrap();
        let nested = repo.join("src").join("lib");
        fs::create_dir_all(&nested).unwrap();

        let location = find_git_repository(Some(&nested)).expect("should detect Git repository");

        assert_eq!(location.root, repo.canonicalize().unwrap());
        assert!(!location.is_bare);
    }

    #[test]
    fn test_find_git_repository_detects_gitdir_file_worktree() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("linked-worktree");
        let common = temp.path().join("main.git");
        let worktree_git = common.join("worktrees").join("linked-worktree");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(common.join("objects")).unwrap();
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(
            repo.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();
        fs::write(
            common.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n",
        )
        .unwrap();
        fs::write(worktree_git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(worktree_git.join("commondir"), b"../..\n").unwrap();

        let location = find_git_repository(Some(&repo)).expect("should detect Git worktree");

        assert_eq!(location.root, repo.canonicalize().unwrap());
        assert!(!location.is_bare);
    }

    #[test]
    fn test_git_info_file_path_uses_common_dir_for_linked_worktree() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("linked-worktree");
        let common = temp.path().join("main.git");
        let worktree_git = common.join("worktrees").join("linked-worktree");
        fs::create_dir_all(&repo).unwrap();
        fs::create_dir_all(common.join("info")).unwrap();
        fs::create_dir_all(&worktree_git).unwrap();
        fs::write(
            repo.join(".git"),
            format!("gitdir: {}\n", worktree_git.display()),
        )
        .unwrap();
        fs::write(worktree_git.join("commondir"), b"../..\n").unwrap();

        let info_path = git_info_file_path(&repo, "exclude").expect("resolve info path");

        assert_eq!(info_path, common.join("info").join("exclude"));
    }

    #[test]
    fn test_find_git_repository_treats_dot_git_directory_as_worktree() {
        let temp = tempdir().unwrap();
        let repo = temp.path().join("project");
        let git = repo.join(".git");
        fs::create_dir_all(git.join("objects").join("aa")).unwrap();
        fs::write(git.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(
            git.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = false\n",
        )
        .unwrap();

        let location = find_git_repository(Some(&git.join("objects")))
            .expect("should detect parent Git worktree from inside .git");

        assert_eq!(location.root, repo.canonicalize().unwrap());
        assert!(!location.is_bare);
    }

    #[test]
    fn test_find_git_repository_keeps_bare_dot_git_directory_bare() {
        let temp = tempdir().unwrap();
        let bare = temp.path().join(".git");
        fs::create_dir_all(bare.join("objects")).unwrap();
        fs::write(bare.join("HEAD"), b"ref: refs/heads/main\n").unwrap();
        fs::write(
            bare.join("config"),
            b"[core]\n\trepositoryformatversion = 0\n\tbare = true\n",
        )
        .unwrap();

        let location = find_git_repository(Some(&bare))
            .expect("should detect a bare repository named .git as bare");

        assert_eq!(location.root, bare.canonicalize().unwrap());
        assert!(location.is_bare);
    }
}
