//! Shared Git/Libra attributes source resolution and matching.

use std::{
    cell::RefCell,
    collections::{BTreeMap, HashMap, HashSet},
    fs, io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::SystemTime,
};

use ignore::{
    Match,
    gitignore::{Gitignore, GitignoreBuilder},
};
use once_cell::sync::Lazy;

use crate::utils::util;

const LIBRA_ATTRIBUTES_FILE: &str = ".libra_attributes";
const GIT_ATTRIBUTES_FILE: &str = ".gitattributes";
const CORE_ATTRIBUTES_FILE_KEY: &str = "core.attributesFile";
const BENEATH_ATTRIBUTE_CACHE_CAP: usize = 512;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AttributeState {
    Set,
    Value(String),
    Unset,
    Unspecified,
}

impl AttributeState {
    pub fn check_attr_value(&self) -> Option<String> {
        match self {
            Self::Set => Some("set".to_string()),
            Self::Value(value) => Some(value.clone()),
            Self::Unset => Some("unset".to_string()),
            Self::Unspecified => None,
        }
    }
}

#[derive(Debug, Clone)]
struct AttributeAssignment {
    name: String,
    state: AttributeState,
}

struct AttributeRule {
    matcher: Gitignore,
    assignments: Vec<AttributeAssignment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct AttributeCacheKey {
    source: PathBuf,
    base: PathBuf,
}

struct CachedAttributes {
    len: u64,
    modified: SystemTime,
    rules: Arc<Vec<AttributeRule>>,
}

struct AttributeSource {
    path: PathBuf,
    base: PathBuf,
}

static ATTRIBUTES_CACHE: Lazy<Mutex<HashMap<AttributeCacheKey, CachedAttributes>>> =
    Lazy::new(|| Mutex::new(HashMap::new()));

/// Identity of the descriptor-backed root used by beneath attribute reads.
/// The path is part of the key because callers may deliberately retain a
/// pinned descriptor after its original pathname has been replaced; the
/// lexical matcher must not reuse rules compiled against a different base.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BeneathRootIdentity {
    path: PathBuf,
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(windows)]
    volume: u32,
    #[cfg(windows)]
    index: u64,
    #[cfg(not(any(unix, windows)))]
    length: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct BeneathSourceKey {
    source: PathBuf,
    base: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BeneathSourceFingerprint {
    len: u64,
    mode: u32,
    ctime_sec: i64,
    ctime_nsec: i64,
    mtime_sec: i64,
    mtime_nsec: i64,
}

impl From<&crate::utils::beneath::RawLstat> for BeneathSourceFingerprint {
    fn from(stat: &crate::utils::beneath::RawLstat) -> Self {
        Self {
            len: stat.len,
            mode: stat.mode,
            ctime_sec: stat.ctime_sec,
            ctime_nsec: stat.ctime_nsec,
            mtime_sec: stat.mtime_sec,
            mtime_nsec: stat.mtime_nsec,
        }
    }
}

struct BeneathCachedSource {
    fingerprint: BeneathSourceFingerprint,
    rules: Arc<Vec<AttributeRule>>,
}

struct BeneathAttributeCache {
    root: BeneathRootIdentity,
    session: Option<u64>,
    sources: HashMap<BeneathSourceKey, BeneathCachedSource>,
    missing_sources: HashSet<BeneathSourceKey>,
    #[cfg(test)]
    parse_count: usize,
    #[cfg(test)]
    probe_count: usize,
}

impl BeneathAttributeCache {
    fn new(root: BeneathRootIdentity, session: Option<u64>) -> Self {
        Self {
            root,
            session,
            sources: HashMap::new(),
            missing_sources: HashSet::new(),
            #[cfg(test)]
            parse_count: 0,
            #[cfg(test)]
            probe_count: 0,
        }
    }

    fn regular_source_rules(
        &mut self,
        root: &fs::File,
        source: &Path,
        base: &Path,
        fingerprint: BeneathSourceFingerprint,
    ) -> io::Result<Arc<Vec<AttributeRule>>> {
        let key = BeneathSourceKey {
            source: source.to_path_buf(),
            base: base.to_path_buf(),
        };
        if let Some(cached) = self.sources.get(&key)
            && cached.fingerprint == fingerprint
        {
            return Ok(Arc::clone(&cached.rules));
        }

        let (contents, descriptor_fingerprint) =
            read_regular_beneath_attribute_source(root, source)?;
        let contents = String::from_utf8_lossy(&contents);
        let rules = Arc::new(parse_attribute_contents(&contents, base));
        #[cfg(test)]
        {
            self.parse_count += 1;
        }
        if self.sources.len() + self.missing_sources.len() < BENEATH_ATTRIBUTE_CACHE_CAP {
            self.sources.insert(
                key,
                BeneathCachedSource {
                    fingerprint: descriptor_fingerprint,
                    rules: Arc::clone(&rules),
                },
            );
        }
        Ok(rules)
    }

    fn remember_missing_source(&mut self, key: BeneathSourceKey) {
        if self.sources.len() + self.missing_sources.len() < BENEATH_ATTRIBUTE_CACHE_CAP {
            self.missing_sources.insert(key);
        }
    }
}

thread_local! {
    /// Attribute rules are reused only while the pinned root identity and
    /// lexical base remain the same. Positive sources are lstat'ed before a
    /// hit, so an in-place edit or replacement invalidates parsed rules;
    /// negative sources are reused only for one explicit invocation nonce.
    static BENEATH_ATTRIBUTE_CACHE: RefCell<Option<BeneathAttributeCache>> =
        const { RefCell::new(None) };
}

fn is_absent_beneath_attribute_source(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
    ) {
        return true;
    }
    #[cfg(unix)]
    {
        error.raw_os_error() == Some(libc::ELOOP)
    }
    #[cfg(not(unix))]
    {
        false
    }
}

fn beneath_root_identity(root_path: &Path, root: &fs::File) -> io::Result<BeneathRootIdentity> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = root.metadata()?;
        Ok(BeneathRootIdentity {
            path: root_path.to_path_buf(),
            dev: metadata.dev(),
            ino: metadata.ino(),
        })
    }
    #[cfg(windows)]
    {
        // `std::os::windows::fs::MetadataExt::{volume_serial_number,
        // file_index}` are nightly-only (`windows_by_handle`, rust#63010), so
        // the stable release build asks the Win32 API directly for the same
        // (volume, file index) identity pair.
        use std::os::windows::io::AsRawHandle;

        use windows_sys::Win32::{
            Foundation::HANDLE,
            Storage::FileSystem::{BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle},
        };

        // SAFETY: the handle is owned by `root` and stays open across the
        // call; the info struct is plain output memory zeroed beforehand.
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let ok = unsafe { GetFileInformationByHandle(root.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return Err(io::Error::other(format!(
                "cannot identify pinned worktree root: {}",
                io::Error::last_os_error()
            )));
        }
        Ok(BeneathRootIdentity {
            path: root_path.to_path_buf(),
            volume: info.dwVolumeSerialNumber,
            index: (u64::from(info.nFileIndexHigh) << 32) | u64::from(info.nFileIndexLow),
        })
    }
    #[cfg(not(any(unix, windows)))]
    {
        let metadata = root.metadata()?;
        Ok(BeneathRootIdentity {
            path: root_path.to_path_buf(),
            length: metadata.len(),
        })
    }
}

pub fn attribute_state_for_path(attr: &str, path: &Path) -> Option<AttributeState> {
    // Pure merge helpers are also used before a Libra repository is initialized.
    // Outside one there are no repository attribute sources to apply.
    let workdir = util::try_working_dir().ok()?;
    let absolute = absolute_in_workdir(path, &workdir)?;
    let mut state = None;
    for source in attribute_sources_for_path(&workdir, &absolute) {
        for rule in cached_attribute_file(&source.path, &source.base).iter() {
            if !attribute_rule_matches(rule, &absolute) {
                continue;
            }
            for assignment in &rule.assignments {
                if assignment.name == attr {
                    state = Some(assignment.state.clone());
                }
            }
        }
    }
    state.and_then(|value| match value {
        AttributeState::Unspecified => None,
        other => Some(other),
    })
}

/// Where `check-attr` reads its `.gitattributes`/`.libraattributes` sources
/// from (issues/481 MX-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttributeSourceKind<'a> {
    /// The working tree (the default).
    Worktree,
    /// The index, as staged right now (`check-attr --cached`).
    Index,
    /// One tree object (`check-attr --source=<tree>`).
    Tree(&'a git_internal::hash::ObjectHash),
}

/// Read every attribute source that applies to `path` from `kind` and merge
/// them, mirroring [`all_attribute_states_for_path`] but for a non-worktree
/// source. Used by `check-attr --cached` / `--source=<tree>`.
pub fn all_attribute_states_for_path_from(
    path: &Path,
    kind: AttributeSourceKind<'_>,
) -> BTreeMap<String, AttributeState> {
    if kind == AttributeSourceKind::Worktree {
        return all_attribute_states_for_path(path);
    }
    let Ok(workdir) = util::try_working_dir() else {
        return BTreeMap::new();
    };
    let Some(absolute) = absolute_in_workdir(path, &workdir) else {
        return BTreeMap::new();
    };
    // `.git/info/attributes` and the configured global file are still read from
    // disk (they are not part of any tree), matching Git's resolution order.
    let mut sources = Vec::new();
    if let Some(configured) =
        util::optional_cascaded_config_path(CORE_ATTRIBUTES_FILE_KEY, &workdir)
    {
        push_attribute_source(&mut sources, configured, workdir.to_path_buf());
    }
    for info_attributes in util::worktree_info_file_paths(&workdir, "attributes") {
        push_attribute_source(&mut sources, info_attributes, workdir.to_path_buf());
    }
    let mut states = BTreeMap::new();
    for source in &sources {
        for rule in cached_attribute_file(&source.path, &source.base).iter() {
            if !attribute_rule_matches(rule, &absolute) {
                continue;
            }
            for assignment in &rule.assignments {
                if matches!(assignment.state, AttributeState::Unspecified) {
                    states.remove(&assignment.name);
                } else {
                    states.insert(assignment.name.clone(), assignment.state.clone());
                }
            }
        }
    }
    // Tree/index attribute files, outermost directory first so a deeper file
    // wins (Git's precedence).
    for dir in attribute_dirs(&workdir, &absolute) {
        for name in [GIT_ATTRIBUTES_FILE, LIBRA_ATTRIBUTES_FILE] {
            let Some(relative) = dir
                .strip_prefix(&workdir)
                .ok()
                .map(|prefix| prefix.join(name))
            else {
                continue;
            };
            let key = relative.to_string_lossy().replace('\\', "/");
            let Some(contents) = read_attribute_source_contents(&key, kind) else {
                continue;
            };
            for rule in parse_attribute_contents(&contents, &dir) {
                if !attribute_rule_matches(&rule, &absolute) {
                    continue;
                }
                for assignment in &rule.assignments {
                    if matches!(assignment.state, AttributeState::Unspecified) {
                        states.remove(&assignment.name);
                    } else {
                        states.insert(assignment.name.clone(), assignment.state.clone());
                    }
                }
            }
        }
    }
    states
}

/// One attribute file's text from a non-worktree source, or `None` when the
/// path is not present there.
fn read_attribute_source_contents(key: &str, kind: AttributeSourceKind<'_>) -> Option<String> {
    let storage = util::objects_storage().ok()?;
    let oid = match kind {
        AttributeSourceKind::Worktree => None?,
        AttributeSourceKind::Index => {
            let index =
                git_internal::internal::index::Index::load(crate::utils::path::index()).ok()?;
            index.get(key, 0).map(|entry| entry.hash)
        }
        AttributeSourceKind::Tree(tree) => {
            use crate::utils::object_ext::TreeExt;
            let loaded = git_internal::internal::object::tree::Tree::try_load(tree)?;
            loaded
                .get_plain_items_with_mode()
                .ok()?
                .into_iter()
                .find(|(path, _, mode)| {
                    path.to_string_lossy() == key
                        && *mode == git_internal::internal::object::tree::TreeItemMode::Blob
                })
                .map(|(_, id, _)| id)
        }
    }?;
    let bytes = storage.get(&oid).ok()?;
    String::from_utf8(bytes).ok()
}

pub fn all_attribute_states_for_path(path: &Path) -> BTreeMap<String, AttributeState> {
    let Ok(workdir) = util::try_working_dir() else {
        return BTreeMap::new();
    };
    let Some(absolute) = absolute_in_workdir(path, &workdir) else {
        return BTreeMap::new();
    };
    let mut states = BTreeMap::new();
    for source in attribute_sources_for_path(&workdir, &absolute) {
        for rule in cached_attribute_file(&source.path, &source.base).iter() {
            if !attribute_rule_matches(rule, &absolute) {
                continue;
            }
            for assignment in &rule.assignments {
                if matches!(assignment.state, AttributeState::Unspecified) {
                    states.remove(&assignment.name);
                } else {
                    states.insert(assignment.name.clone(), assignment.state.clone());
                }
            }
        }
    }
    states
}

/// Resolve attributes for a blob write without treating a failed source read
/// as an absent rule. Source precedence and parsing match the ordinary reader;
/// worktree sources are opened beneath one pinned worktree root.
pub(crate) fn try_all_attribute_states_beneath(
    workdir: &Path,
    root: &fs::File,
    relative: &Path,
) -> io::Result<BTreeMap<String, AttributeState>> {
    if relative.is_absolute()
        || relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "attribute lookup requires a canonical relative file path",
        ));
    }
    attribute_states_beneath_with_context(
        workdir,
        root,
        relative,
        &ConversionAttributeContext::capture(workdir)?,
        None,
        false,
    )
}

/// Captured by the repository caller, never rediscovered from a helper's cwd.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct ConversionAttributeContext {
    pub configured: Option<PathBuf>,
    pub info: Vec<PathBuf>,
}

impl ConversionAttributeContext {
    pub(crate) fn capture(workdir: &Path) -> io::Result<Self> {
        let mut info = util::worktree_info_file_paths(workdir, "attributes");
        let native = workdir.join(util::ROOT_DIR).join("info/attributes");
        let native_position = info.iter().position(|path| path == &native);
        let gitdir = match crate::internal::worktree_scope::WorktreeScope::request_scope()
            .filter(|scope| scope.worktree_root == workdir)
        {
            Some(scope) => Some(scope.gitdir),
            None if native_position.is_some() => Some(
                fs::canonicalize(workdir.join(util::ROOT_DIR)).map_err(|error| {
                    io::Error::new(
                        error.kind(),
                        format!(
                            "resolve attributes metadata for '{}': {error}; check the worktree layout",
                            workdir.display()
                        ),
                    )
                })?,
            ),
            None => None,
        };
        if let Some(gitdir) = gitdir {
            // Repository discovery accepts legacy .libra symlinks. Use that
            // resolved gitdir; do not traverse the symlink beneath a worktree
            // root or relax no-follow checks for attribute files themselves.
            // The unpinned fallback resolves only this exact metadata path,
            // never a parent repository if the marker disappears meanwhile.
            let source = gitdir.join("info/attributes");
            match native_position {
                Some(position) => info[position] = source,
                None => info.insert(0, source),
            }
        }
        Ok(Self {
            configured: util::optional_cascaded_config_path(CORE_ATTRIBUTES_FILE_KEY, workdir),
            info,
        })
    }
}

pub(crate) fn attribute_states_beneath_with_context(
    workdir: &Path,
    root: &fs::File,
    relative: &Path,
    context: &ConversionAttributeContext,
    session: Option<u64>,
    killable: bool,
) -> io::Result<BTreeMap<String, AttributeState>> {
    if relative.is_absolute()
        || relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|part| !matches!(part, std::path::Component::Normal(_)))
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "conversion requires a canonical relative path",
        ));
    }
    let absolute = workdir.join(relative);
    let mut sources = Vec::new();
    if let Some(configured) = &context.configured {
        sources.push(AttributeSource {
            path: configured.clone(),
            base: workdir.to_path_buf(),
        });
    }
    for directory in attribute_dirs(workdir, &absolute) {
        for name in [GIT_ATTRIBUTES_FILE, LIBRA_ATTRIBUTES_FILE] {
            sources.push(AttributeSource {
                path: directory.join(name),
                base: directory.clone(),
            });
        }
    }
    sources.extend(context.info.iter().map(|path| AttributeSource {
        path: path.clone(),
        base: workdir.to_path_buf(),
    }));
    let root_identity = beneath_root_identity(workdir, root)?;
    BENEATH_ATTRIBUTE_CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        if slot.as_ref().is_none_or(|cache| cache.root != root_identity || cache.session != session) {
            *slot = Some(BeneathAttributeCache::new(root_identity, session));
        }
        let cache = slot.as_mut().ok_or_else(|| io::Error::other("conversion attribute cache unavailable"))?;
        let mut states = BTreeMap::new();
        for source in sources {
            let rules = if let Ok(relative) = source.path.strip_prefix(workdir) {
                strict_source_rules(root, relative, &source.base, cache, killable)
            } else {
                // The caller explicitly supplies external config/info sources.
                // Worktree rules cannot choose an external file capability.
                let parent = source.path.parent().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "attributes file has no parent"))?;
                let name = source.path.file_name().ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "attributes file has no name"))?;
                match crate::utils::beneath::open_root(parent) {
                    Ok(external_root) => {
                        let contents = read_strict_attribute_source(&external_root, Path::new(name))?;
                        Ok(contents.map(|bytes| Arc::new(parse_attribute_contents(&String::from_utf8_lossy(&bytes), &source.base))))
                    },
                    Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
                    Err(error) => Err(error),
                }
            }.map_err(|error| io::Error::new(error.kind(), format!("read attributes source '{}': {error}; check that it is a readable regular file", source.path.display())))?;
            let Some(rules) = rules else { continue; };
            for rule in rules.iter() {
                if !attribute_rule_matches_file(rule, &absolute) { continue; }
                for assignment in &rule.assignments {
                    if assignment.state == AttributeState::Unspecified {
                        states.remove(&assignment.name);
                    } else {
                        states.insert(assignment.name.clone(), assignment.state.clone());
                    }
                }
            }
        }
        Ok(states)
    })
}

/// Metadata for every possible source, including absent files. Directory
/// enumeration comes from tracked paths, so no unbounded worktree walk is
/// needed, and deleting a source changes the digest just like editing it.
pub(crate) fn conversion_sources_fingerprint(
    workdir: &Path,
    root: &fs::File,
    directories: &[PathBuf],
    context: &ConversionAttributeContext,
) -> io::Result<String> {
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    hash.update(format!("conversion-v1:{:?}", beneath_root_identity(workdir, root)?).as_bytes());
    let mut sources = Vec::new();
    sources.extend(context.configured.iter().cloned());
    for directory in directories {
        for name in [GIT_ATTRIBUTES_FILE, LIBRA_ATTRIBUTES_FILE] {
            sources.push(workdir.join(directory).join(name));
        }
    }
    sources.extend(context.info.iter().cloned());
    for source in sources {
        let bytes = crate::internal::worktree_io::protocol::path_to_bytes(&source);
        hash.update(&(bytes.len() as u64).to_le_bytes());
        hash.update(&bytes);
        let stat = if let Ok(relative) = source.strip_prefix(workdir) {
            crate::utils::beneath::lstat_beneath(root, relative)
        } else {
            let parent = source.parent().ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "attributes source has no parent",
                )
            })?;
            let name = source.file_name().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidInput, "attributes source has no name")
            })?;
            crate::utils::beneath::open_root(parent).and_then(|external_root| {
                hash.update(
                    format!("{:?}", beneath_root_identity(parent, &external_root)?).as_bytes(),
                );
                crate::utils::beneath::lstat_beneath(&external_root, Path::new(name))
            })
        };
        match stat {
            Ok(stat) if stat.is_file => {
                hash.update(format!("{:?}", BeneathSourceFingerprint::from(&stat)).as_bytes())
            }
            Ok(_) => {
                // Inspect the source using the killable path, retaining FIFO
                // timeout behavior instead of treating non-regular as absent.
                if let Ok(relative) = source.strip_prefix(workdir) {
                    read_beneath_attribute_source(root, relative)?;
                }
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "attributes source '{}' is not a readable regular file",
                        source.display()
                    ),
                ));
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
                ) =>
            {
                hash.update(b"absent")
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!(
                        "cannot inspect attributes source '{}': {error}",
                        source.display()
                    ),
                ));
            }
        }
    }
    Ok(hex::encode(hash.finish().as_ref()))
}

fn strict_source_rules(
    root: &fs::File,
    source: &Path,
    base: &Path,
    cache: &mut BeneathAttributeCache,
    killable: bool,
) -> io::Result<Option<Arc<Vec<AttributeRule>>>> {
    let stat = match crate::utils::beneath::lstat_beneath(root, source) {
        Ok(stat) => stat,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if !stat.is_file {
        if !killable || stat.is_symlink || stat.mode & 0o170000 == 0o040000 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a regular attributes file; replace it with a readable regular file",
            ));
        }
        // Preserve FIFO deadline behavior: only the recyclable helper can
        // attempt this open. No failed source becomes an empty rule set.
        return read_beneath_attribute_source(root, source).map(|bytes| {
            Some(Arc::new(parse_attribute_contents(
                &String::from_utf8_lossy(&bytes),
                base,
            )))
        });
    }
    cache
        .regular_source_rules(root, source, base, BeneathSourceFingerprint::from(&stat))
        .map(Some)
}

fn read_strict_attribute_source(root: &fs::File, source: &Path) -> io::Result<Option<Vec<u8>>> {
    let stat = match crate::utils::beneath::lstat_beneath(root, source) {
        Ok(stat) => stat,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::NotADirectory
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    if !stat.is_file {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a regular attributes file; replace it with a readable regular file",
        ));
    }
    read_regular_beneath_attribute_source(root, source).map(|(contents, _)| Some(contents))
}

pub fn is_lfs_tracked(path: &Path) -> bool {
    matches!(
        attribute_state_for_path("filter", path),
        Some(AttributeState::Value(value)) if value == "lfs"
    )
}

/// Resolve `filter=lfs` beneath a pinned root, optionally reusing missing
/// source results for one parent invocation. Positive rules remain guarded by
/// source metadata so an in-place edit is observed even within an invocation.
pub(crate) fn is_lfs_tracked_beneath_session(
    root_path: &Path,
    root: &fs::File,
    relative: &Path,
    session: Option<u64>,
) -> io::Result<bool> {
    if relative.is_absolute() || relative.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LFS attribute lookup requires a non-empty relative file path",
        ));
    }
    if relative.components().any(|component| {
        matches!(
            component,
            std::path::Component::ParentDir
                | std::path::Component::RootDir
                | std::path::Component::Prefix(_)
        )
    }) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "LFS attribute lookup path must be canonical and relative",
        ));
    }

    let target = root_path.join(relative);
    let parent = relative.parent().unwrap_or_else(|| Path::new(""));
    let mut directories = vec![PathBuf::new()];
    let mut current = PathBuf::new();
    for component in parent.components() {
        let std::path::Component::Normal(name) = component else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "LFS attribute lookup parent must be canonical and relative",
            ));
        };
        current.push(name);
        directories.push(current.clone());
    }

    let root_identity = beneath_root_identity(root_path, root)?;
    BENEATH_ATTRIBUTE_CACHE.with(|slot| {
        let mut slot = slot.borrow_mut();
        let replace = match slot.as_ref() {
            Some(cache) => cache.root != root_identity || cache.session != session,
            None => true,
        };
        if replace {
            *slot = Some(BeneathAttributeCache::new(root_identity, session));
        }
        let Some(cache) = slot.as_mut() else {
            return Err(io::Error::other("beneath attribute cache unavailable"));
        };

        let mut filter = None;
        for directory in directories {
            for name in [GIT_ATTRIBUTES_FILE, LIBRA_ATTRIBUTES_FILE] {
                let source = if directory.as_os_str().is_empty() {
                    PathBuf::from(name)
                } else {
                    directory.join(name)
                };
                apply_beneath_attribute_source(
                    root,
                    &source,
                    &root_path.join(&directory),
                    &target,
                    &mut filter,
                    cache,
                )?;
            }
        }

        // `worktree_info_file_paths` can resolve a gitdir-file to a directory
        // outside the worktree.  Only the two literal, root-contained layouts are
        // eligible here; a `.git` file or a symlink is rejected by beneath I/O
        // and cannot redirect attribute reads.
        for source in [
            Path::new(crate::utils::util::ROOT_DIR)
                .join("info")
                .join("attributes"),
            Path::new(crate::utils::util::GIT_DIR)
                .join("info")
                .join("attributes"),
        ] {
            apply_beneath_attribute_source(root, &source, root_path, &target, &mut filter, cache)?;
        }

        Ok(matches!(
            filter,
            Some(AttributeState::Value(value)) if value == "lfs"
        ))
    })
}

fn apply_beneath_attribute_source(
    root: &fs::File,
    source: &Path,
    base: &Path,
    target: &Path,
    filter: &mut Option<AttributeState>,
    cache: &mut BeneathAttributeCache,
) -> io::Result<()> {
    let key = BeneathSourceKey {
        source: source.to_path_buf(),
        base: base.to_path_buf(),
    };
    if cache.session.is_some() && cache.missing_sources.contains(&key) {
        return Ok(());
    }
    #[cfg(test)]
    {
        cache.probe_count += 1;
    }
    let source_kind = match crate::utils::beneath::lstat_beneath(root, source) {
        Ok(stat) if stat.is_file => Some(BeneathSourceFingerprint::from(&stat)),
        Ok(_) => None,
        Err(error) if is_absent_beneath_attribute_source(&error) => {
            if cache.session.is_some() {
                cache.remember_missing_source(key);
            }
            return Ok(());
        }
        Err(error) => {
            return Err(io::Error::new(
                error.kind(),
                format!("read attributes source '{}': {error}", source.display()),
            ));
        }
    };

    let rules = match source_kind {
        Some(fingerprint) => cache.regular_source_rules(root, source, base, fingerprint)?,
        None => {
            // Preserve the old open/read behavior for non-regular sources.
            // In particular, a FIFO must still block in the killable helper;
            // classifying it from lstat alone would change timeout semantics.
            let contents = match read_beneath_attribute_source(root, source) {
                Ok(contents) => contents,
                Err(error) if is_absent_beneath_attribute_source(&error) => {
                    return Ok(());
                }
                Err(error) => return Err(error),
            };
            let contents = String::from_utf8_lossy(&contents);
            Arc::new(parse_attribute_contents(&contents, base))
        }
    };
    for rule in rules.iter() {
        if !attribute_rule_matches_file(rule, target) {
            continue;
        }
        for assignment in &rule.assignments {
            if assignment.name == "filter" {
                *filter = Some(assignment.state.clone());
            }
        }
    }
    Ok(())
}

fn read_beneath_attribute_source(root: &fs::File, source: &Path) -> io::Result<Vec<u8>> {
    match crate::utils::beneath::read_file_beneath(root, source) {
        Ok(contents) => Ok(contents),
        Err(error) => Err(io::Error::new(
            error.kind(),
            format!("read attributes source '{}': {error}", source.display()),
        )),
    }
}

fn read_regular_beneath_attribute_source(
    root: &fs::File,
    source: &Path,
) -> io::Result<(Vec<u8>, BeneathSourceFingerprint)> {
    let mut file = crate::utils::beneath::open_file_beneath(root, source).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("read attributes source '{}': {error}", source.display()),
        )
    })?;
    let before = crate::utils::beneath::RawLstat::from_metadata(&file.metadata()?)?;
    let mut contents = Vec::new();
    io::Read::read_to_end(
        &mut io::Read::take(&mut file, before.len.saturating_add(1)),
        &mut contents,
    )?;
    let after = crate::utils::beneath::RawLstat::from_metadata(&file.metadata()?)?;
    let before_fingerprint = BeneathSourceFingerprint::from(&before);
    let after_fingerprint = BeneathSourceFingerprint::from(&after);
    if before_fingerprint != after_fingerprint || contents.len() as u64 != before.len {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "attributes source '{}' changed while it was being read",
                source.display()
            ),
        ));
    }
    Ok((contents, after_fingerprint))
}

pub fn diff_driver_for_path(path: &Path) -> Option<String> {
    match attribute_state_for_path("diff", path) {
        Some(AttributeState::Value(driver)) if !driver.is_empty() => Some(driver),
        _ => None,
    }
}

pub fn is_export_ignored(path: &Path) -> bool {
    matches!(
        attribute_state_for_path("export-ignore", path),
        Some(AttributeState::Set | AttributeState::Value(_))
    )
}

fn absolute_in_workdir(path: &Path, workdir: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        workdir.join(path)
    };
    util::is_sub_path(&absolute, workdir).then_some(absolute)
}

fn attribute_sources_for_path(workdir: &Path, absolute: &Path) -> Vec<AttributeSource> {
    let mut sources = Vec::new();
    if let Some(configured) = util::optional_cascaded_config_path(CORE_ATTRIBUTES_FILE_KEY, workdir)
    {
        push_attribute_source(&mut sources, configured, workdir.to_path_buf());
    }
    for dir in attribute_dirs(workdir, absolute) {
        push_attribute_source(&mut sources, dir.join(GIT_ATTRIBUTES_FILE), dir.clone());
        push_attribute_source(&mut sources, dir.join(LIBRA_ATTRIBUTES_FILE), dir);
    }
    for info_attributes in util::worktree_info_file_paths(workdir, "attributes") {
        push_attribute_source(&mut sources, info_attributes, workdir.to_path_buf());
    }
    sources
}

fn attribute_dirs(workdir: &Path, absolute: &Path) -> Vec<PathBuf> {
    let parent = absolute.parent().unwrap_or(workdir);
    let Ok(relative_parent) = parent.strip_prefix(workdir) else {
        return vec![workdir.to_path_buf()];
    };
    let mut dirs = vec![workdir.to_path_buf()];
    let mut current = workdir.to_path_buf();
    for component in relative_parent.components() {
        current.push(component.as_os_str());
        dirs.push(current.clone());
    }
    dirs
}

fn push_attribute_source(sources: &mut Vec<AttributeSource>, path: PathBuf, base: PathBuf) {
    if path.exists() {
        sources.push(AttributeSource { path, base });
    }
}

fn cached_attribute_file(path: &Path, base: &Path) -> Arc<Vec<AttributeRule>> {
    let Ok(metadata) = fs::metadata(path) else {
        return Arc::new(Vec::new());
    };
    let Ok(modified) = metadata.modified() else {
        return Arc::new(parse_attribute_file(path, base));
    };
    let len = metadata.len();
    let key = AttributeCacheKey {
        source: path.to_path_buf(),
        base: base.to_path_buf(),
    };
    let mut cache = match ATTRIBUTES_CACHE.lock() {
        Ok(cache) => cache,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(cached) = cache.get(&key)
        && cached.len == len
        && cached.modified == modified
    {
        return Arc::clone(&cached.rules);
    }

    let rules = Arc::new(parse_attribute_file(path, base));
    cache.insert(
        key,
        CachedAttributes {
            len,
            modified,
            rules: Arc::clone(&rules),
        },
    );
    rules
}

/// Does this attributes file contribute at least one rule the ENGINE would
/// actually apply? Parser-backed on purpose (W0 §C.4.1.1 origin inventory):
/// a hand-rolled "non-comment line" heuristic both over- and under-reports —
/// `*.dat` with no assignment parses to nothing, while an indented `#…`
/// line is a real pattern.
pub fn file_defines_any_rule(path: &Path, base: &Path) -> bool {
    !parse_attribute_file(path, base).is_empty()
}

fn parse_attribute_file(path: &Path, base: &Path) -> Vec<AttributeRule> {
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };
    parse_attribute_contents(&contents, base)
}

fn parse_attribute_contents(contents: &str, base: &Path) -> Vec<AttributeRule> {
    let mut rules = Vec::new();
    for line in contents.lines() {
        let Some(tokens) = split_attribute_line(line) else {
            continue;
        };
        if tokens.len() < 2 {
            continue;
        }
        let pattern = &tokens[0];
        let assignments = tokens[1..]
            .iter()
            .filter_map(|token| parse_assignment(token))
            .collect::<Vec<_>>();
        if assignments.is_empty() {
            continue;
        }
        if let Some(matcher) = compile_attribute_pattern(pattern, base) {
            rules.push(AttributeRule {
                matcher,
                assignments,
            });
        }
    }
    rules
}

fn split_attribute_line(line: &str) -> Option<Vec<String>> {
    let line = line.trim_end_matches('\r');
    let trimmed = line.trim_start();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }

    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut escaped = false;
    for ch in trimmed.chars() {
        if escaped {
            current.push(ch);
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch.is_whitespace() {
            if !current.is_empty() {
                tokens.push(std::mem::take(&mut current));
            }
        } else {
            current.push(ch);
        }
    }
    if escaped {
        current.push('\\');
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    (!tokens.is_empty()).then_some(tokens)
}

fn parse_assignment(token: &str) -> Option<AttributeAssignment> {
    let (name, state) = if let Some(name) = token.strip_prefix('-') {
        (name, AttributeState::Unset)
    } else if let Some(name) = token.strip_prefix('!') {
        (name, AttributeState::Unspecified)
    } else if let Some((name, value)) = token.split_once('=') {
        (name, AttributeState::Value(value.to_string()))
    } else {
        (token, AttributeState::Set)
    };
    (!name.is_empty()).then(|| AttributeAssignment {
        name: name.to_string(),
        state,
    })
}

fn compile_attribute_pattern(pattern: &str, base: &Path) -> Option<Gitignore> {
    let mut builder = GitignoreBuilder::new(base);
    if builder.add_line(None, pattern).is_err() {
        return None;
    }
    builder.build().ok()
}

fn attribute_rule_matches(rule: &AttributeRule, path: &Path) -> bool {
    !matches!(rule.matcher.matched(path, path.is_dir()), Match::None)
}

fn attribute_rule_matches_file(rule: &AttributeRule, path: &Path) -> bool {
    !matches!(rule.matcher.matched(path, false), Match::None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_beneath_attribute_cache() {
        BENEATH_ATTRIBUTE_CACHE.with(|slot| {
            *slot.borrow_mut() = None;
        });
    }

    fn beneath_attribute_parse_count() -> usize {
        BENEATH_ATTRIBUTE_CACHE.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|cache| cache.parse_count)
                .unwrap_or(0)
        })
    }

    fn beneath_attribute_probe_count() -> usize {
        BENEATH_ATTRIBUTE_CACHE.with(|slot| {
            slot.borrow()
                .as_ref()
                .map(|cache| cache.probe_count)
                .unwrap_or(0)
        })
    }

    #[test]
    fn beneath_attribute_cache_reuses_rules_and_is_invalidated_safely() -> io::Result<()> {
        clear_beneath_attribute_cache();
        let root_dir = tempfile::tempdir()?;
        fs::write(
            root_dir.path().join(GIT_ATTRIBUTES_FILE),
            "*.bin filter=lfs\n",
        )?;
        fs::write(root_dir.path().join("first.bin"), b"first")?;
        fs::write(root_dir.path().join("second.bin"), b"second")?;
        let root = crate::utils::beneath::open_root(root_dir.path())?;

        assert!(is_lfs_tracked_beneath_session(
            root_dir.path(),
            &root,
            Path::new("first.bin"),
            None
        )?);
        let first_parse_count = beneath_attribute_parse_count();
        assert_eq!(first_parse_count, 1, "the root source is parsed once");
        assert!(is_lfs_tracked_beneath_session(
            root_dir.path(),
            &root,
            Path::new("second.bin"),
            None
        )?);
        assert_eq!(
            beneath_attribute_parse_count(),
            first_parse_count,
            "a second file reuses the parsed root source"
        );

        fs::write(
            root_dir.path().join(GIT_ATTRIBUTES_FILE),
            "*.txt filter=lfs\n# changed\n",
        )?;
        assert!(!is_lfs_tracked_beneath_session(
            root_dir.path(),
            &root,
            Path::new("first.bin"),
            None
        )?);
        assert!(is_lfs_tracked_beneath_session(
            root_dir.path(),
            &root,
            Path::new("new.txt"),
            None
        )?);
        assert_eq!(
            beneath_attribute_parse_count(),
            first_parse_count + 1,
            "source metadata changes force a fresh parse"
        );

        let other_dir = tempfile::tempdir()?;
        fs::write(
            other_dir.path().join(GIT_ATTRIBUTES_FILE),
            "*.bin -filter\n",
        )?;
        fs::write(other_dir.path().join("first.bin"), b"other")?;
        let other_root = crate::utils::beneath::open_root(other_dir.path())?;
        assert!(!is_lfs_tracked_beneath_session(
            other_dir.path(),
            &other_root,
            Path::new("first.bin"),
            None
        )?);
        assert_eq!(
            beneath_attribute_parse_count(),
            1,
            "a different pinned root cannot reuse the first root's rules"
        );
        Ok(())
    }

    #[test]
    fn beneath_attribute_cache_reuses_missing_sources_only_within_session() -> io::Result<()> {
        clear_beneath_attribute_cache();
        let root_dir = tempfile::tempdir()?;
        fs::write(root_dir.path().join("first.bin"), b"first")?;
        fs::write(root_dir.path().join("second.bin"), b"second")?;
        let root = crate::utils::beneath::open_root(root_dir.path())?;

        assert!(!is_lfs_tracked_beneath_session(
            root_dir.path(),
            &root,
            Path::new("first.bin"),
            Some(11)
        )?);
        let first_probe_count = beneath_attribute_probe_count();
        assert!(first_probe_count > 0);
        assert!(!is_lfs_tracked_beneath_session(
            root_dir.path(),
            &root,
            Path::new("second.bin"),
            Some(11)
        )?);
        assert_eq!(
            beneath_attribute_probe_count(),
            first_probe_count,
            "missing sources are not re-probed for another file in one invocation"
        );

        fs::write(
            root_dir.path().join(GIT_ATTRIBUTES_FILE),
            "*.bin filter=lfs\n",
        )?;
        assert!(is_lfs_tracked_beneath_session(
            root_dir.path(),
            &root,
            Path::new("second.bin"),
            Some(12)
        )?);
        assert_eq!(
            beneath_attribute_probe_count(),
            first_probe_count,
            "a new invocation must probe again so newly-created attributes are visible"
        );
        Ok(())
    }

    #[test]
    fn beneath_attribute_cache_has_a_fixed_entry_cap() -> io::Result<()> {
        clear_beneath_attribute_cache();
        let root_dir = tempfile::tempdir()?;
        let root = crate::utils::beneath::open_root(root_dir.path())?;
        let mut cache =
            BeneathAttributeCache::new(beneath_root_identity(root_dir.path(), &root)?, Some(1));
        for index in 0..(BENEATH_ATTRIBUTE_CACHE_CAP + 17) {
            cache.remember_missing_source(BeneathSourceKey {
                source: PathBuf::from(format!("dir-{index}/.gitattributes")),
                base: root_dir.path().join(format!("dir-{index}")),
            });
        }
        assert_eq!(
            cache.missing_sources.len(),
            BENEATH_ATTRIBUTE_CACHE_CAP,
            "attribute cache entries must remain bounded"
        );
        Ok(())
    }
}
