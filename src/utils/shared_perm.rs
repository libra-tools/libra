//! Shared repository permissions helper aligning with Git's `path.c:calc_shared_perm`.

use std::{
    collections::HashMap,
    fs, io,
    path::{Path, PathBuf},
    sync::{LazyLock, RwLock},
};

static REPO_SHARED_CACHE: LazyLock<RwLock<HashMap<PathBuf, Option<SharedMode>>>> =
    LazyLock::new(|| RwLock::new(HashMap::new()));

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum SharedModeError {
    #[error("invalid shared mode '{0}': file owner must have read and write permissions")]
    OwnerPermissionsMissing(String),
    #[error("invalid shared mode '{0}'")]
    Invalid(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedMode {
    Umask,
    Group,
    All,
    Numeric { raw: String, bits: u32 },
}

impl SharedMode {
    pub fn parse(raw: &str) -> Result<Self, SharedModeError> {
        let trimmed = raw.trim();
        let lower = trimmed.to_ascii_lowercase();
        match lower.as_str() {
            "umask" | "false" | "no" | "off" | "0" => Ok(Self::Umask),
            "group" | "true" | "yes" | "on" | "1" => Ok(Self::Group),
            "all" | "world" | "everybody" | "2" => Ok(Self::All),
            _ => {
                if let Ok(val) = u32::from_str_radix(trimmed, 8) {
                    if val == 0 {
                        return Ok(Self::Umask);
                    }
                    if val == 1 {
                        return Ok(Self::Group);
                    }
                    if val == 2 {
                        return Ok(Self::All);
                    }
                    if val & 0o600 != 0o600 {
                        return Err(SharedModeError::OwnerPermissionsMissing(raw.to_string()));
                    }
                    let bits = val & 0o666;
                    return Ok(Self::Numeric {
                        raw: format!("0{bits:o}"),
                        bits,
                    });
                }
                Err(SharedModeError::Invalid(raw.to_string()))
            }
        }
    }

    pub fn config_value(&self) -> &str {
        match self {
            Self::Umask => "0",
            Self::Group => "1",
            Self::All => "2",
            Self::Numeric { raw, .. } => raw.as_str(),
        }
    }

    pub fn should_persist(&self) -> bool {
        !matches!(self, Self::Umask)
    }

    #[cfg(not(target_os = "windows"))]
    pub fn permission_bits(&self) -> Option<u32> {
        match self {
            Self::Umask => None,
            Self::Group => Some(0o2775),
            Self::All => Some(0o2777),
            Self::Numeric { bits, .. } => Some(*bits),
        }
    }
}

/// Ascend from `path` to find the repository storage root (`.libra` or bare root).
pub fn find_storage_root(path: &Path) -> Option<PathBuf> {
    let start = if path.is_dir() { path } else { path.parent()? };
    for ancestor in start.ancestors() {
        if ancestor.file_name() == Some(std::ffi::OsStr::new(".libra")) && ancestor.is_dir() {
            return Some(ancestor.to_path_buf());
        }
        if ancestor.join("libra.db").is_file() {
            return Some(ancestor.to_path_buf());
        }
        let dot_libra = ancestor.join(".libra");
        if dot_libra.join("libra.db").is_file() {
            return Some(dot_libra);
        }
    }
    None
}

/// Retrieve the cached repository shared mode, or query and cache it once.
pub fn get_repo_shared_mode(storage_root: &Path) -> io::Result<Option<SharedMode>> {
    {
        let cache = REPO_SHARED_CACHE
            .read()
            .map_err(|e| io::Error::other(e.to_string()))?;
        if let Some(mode) = cache.get(storage_root) {
            return Ok(mode.clone());
        }
    }

    let mode = read_repo_shared_mode_uncached(storage_root)?;
    let mut cache = REPO_SHARED_CACHE
        .write()
        .map_err(|e| io::Error::other(e.to_string()))?;
    cache.insert(storage_root.to_path_buf(), mode.clone());
    Ok(mode)
}

fn read_repo_shared_mode_uncached(storage_root: &Path) -> io::Result<Option<SharedMode>> {
    let db_path = storage_root.join("libra.db");
    match db_path.try_exists() {
        Ok(true) => {}
        Ok(false) => return Ok(None),
        Err(e) => return Err(e),
    }

    let db_path_buf = db_path.clone();
    let res = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| io::Error::other(e.to_string()))?;
        rt.block_on(async move {
            let val = crate::internal::config::read_local_config_value_from_db_path(
                &db_path_buf,
                "core.sharedRepository",
            )
            .await;
            Ok::<Option<String>, io::Error>(val.unwrap_or(None))
        })
    })
    .join()
    .map_err(|_| io::Error::other("thread panicked reading sharedRepository"))??;

    match res {
        Some(raw) => {
            let mode = SharedMode::parse(&raw)
                .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err.to_string()))?;
            Ok(Some(mode))
        }
        None => Ok(None),
    }
}

/// Get the shared mode for the repository containing `path`, or `None` if outside a repo.
pub fn get_shared_mode_for_path(path: &Path) -> io::Result<Option<SharedMode>> {
    if let Some(root) = find_storage_root(path) {
        get_repo_shared_mode(&root)
    } else {
        Ok(None)
    }
}

/// Set a cached repository shared mode (used by init and testing).
pub fn set_cached_repo_shared_mode(storage_root: PathBuf, mode: Option<SharedMode>) {
    if let Ok(mut cache) = REPO_SHARED_CACHE.write() {
        cache.insert(storage_root, mode);
    }
}

/// Clear the repository shared mode cache (used by tests).
pub fn clear_cached_repo_shared_mode(storage_root: Option<&Path>) {
    if let Ok(mut cache) = REPO_SHARED_CACHE.write() {
        if let Some(root) = storage_root {
            cache.remove(root);
        } else {
            cache.clear();
        }
    }
}

/// Calculate the resulting file or directory mode for `mode` given `shared`.
///
/// Follows Git's `calc_shared_perm` in `path.c`:
/// - When `shared` is `Umask`, returns `mode` unchanged.
/// - For non-numeric shared modes:
///   - Group: overlay `0660` (or `0664` for everybody / All)
///   - If owner cannot write, strip group write (`!(mode & 0200) -> mode &= ~0020`).
///   - If owner can execute, copy read bits to execute bits (`mode |= (mode & 0444) >> 2`).
/// - For numeric modes: replace lower 9 bits (`mode &= !0777; mode |= (bits & 0777);`).
/// - For directories:
///   - Copy read bits to execute bits (`mode |= (mode & 0444) >> 2`).
///   - If `Group`, `All`, or numeric mode with group read/write (`bits & 0060 != 0`), add setgid (`02000`).
pub fn calc_shared_perm(mode: u32, is_dir: bool, shared: &SharedMode) -> u32 {
    let mut perm = mode;

    match shared {
        SharedMode::Umask => return mode,
        SharedMode::Group => {
            perm |= 0o664;
            if (perm & 0o200) == 0 {
                perm &= !0o020;
            }
            if (perm & 0o100) != 0 {
                perm |= (perm & 0o444) >> 2;
            }
        }
        SharedMode::All => {
            perm |= 0o664;
            if (perm & 0o200) == 0 {
                perm &= !0o020;
            }
            if (perm & 0o100) != 0 {
                perm |= (perm & 0o444) >> 2;
            }
        }
        SharedMode::Numeric { bits, .. } => {
            let was_executable = (mode & 0o100) != 0;
            perm = (perm & !0o777) | (bits & 0o777);
            if was_executable {
                perm |= (perm & 0o444) >> 2;
            }
        }
    }

    if is_dir {
        perm |= (perm & 0o444) >> 2;
        let has_group = match shared {
            SharedMode::Group | SharedMode::All => true,
            SharedMode::Numeric { bits, .. } => (bits & 0o060) != 0,
            SharedMode::Umask => false,
        };
        if has_group {
            perm |= 0o2000;
        }
    }

    perm
}

/// Calculate the permissions for a loose object or pack/index file.
///
/// Follows Git's read-only object file semantics:
/// - Group and All get 0444
/// - Numeric modes retain group read if bit 0040 is set, and other read if bit 0004 is set
pub fn calc_shared_object_perm(shared: &SharedMode) -> u32 {
    match shared {
        SharedMode::Umask => 0o444,
        SharedMode::Group | SharedMode::All => 0o444,
        SharedMode::Numeric { bits, .. } => {
            let mut mode = 0o444;
            if (bits & 0o040) == 0 {
                mode &= !0o040;
            }
            if (bits & 0o004) == 0 {
                mode &= !0o004;
            }
            mode
        }
    }
}

/// Returns true if `path` is an allowlisted owner-only file that must NEVER be widened.
pub fn is_owner_only_path(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    matches!(
        name,
        "vault.db" | "vault.db-wal" | "vault.db-shm" | "vault.db-journal"
    ) || name.starts_with(".vault")
        || name.starts_with("credentials")
        || name.starts_with("auth")
}

/// Adjust filesystem permissions of `path` based on `shared`.
///
/// On Windows, this is an explicit no-op. On Unix, this applies `calc_shared_perm`
/// without following symlinks.
#[cfg(unix)]
pub fn adjust_shared_perm(path: &Path, shared: &SharedMode) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if matches!(shared, SharedMode::Umask) {
        return Ok(());
    }

    if is_owner_only_path(path) {
        return Ok(());
    }

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }

    let is_dir = metadata.is_dir();
    let current_mode = metadata.permissions().mode();
    let target_mode = calc_shared_perm(current_mode, is_dir, shared);

    if (current_mode & 0o7777) != (target_mode & 0o7777) {
        let mut perms = metadata.permissions();
        perms.set_mode(target_mode);
        fs::set_permissions(path, perms)?;
    }

    Ok(())
}

#[cfg(not(unix))]
pub fn adjust_shared_perm(_path: &Path, _shared: &SharedMode) -> io::Result<()> {
    Ok(())
}

/// Adjust permissions for a loose object file and its parent fanout directory.
pub fn adjust_shared_perm_for_object(path: &Path, shared: &SharedMode) -> io::Result<()> {
    if matches!(shared, SharedMode::Umask) {
        return Ok(());
    }
    if let Some(parent) = path.parent() {
        adjust_shared_perm(parent, shared)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let obj_mode = calc_shared_object_perm(shared);
        if let Ok(metadata) = fs::symlink_metadata(path)
            && !metadata.file_type().is_symlink()
            && (metadata.permissions().mode() & 0o7777) != obj_mode
        {
            let mut perms = metadata.permissions();
            perms.set_mode(obj_mode);
            fs::set_permissions(path, perms)?;
        }
    }
    Ok(())
}

/// Adjust permissions for a pack file, its index, and the parent pack directory.
pub fn adjust_shared_perm_for_pack(pack_path: &Path, shared: &SharedMode) -> io::Result<()> {
    if matches!(shared, SharedMode::Umask) {
        return Ok(());
    }
    if let Some(parent) = pack_path.parent() {
        adjust_shared_perm(parent, shared)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let obj_mode = calc_shared_object_perm(shared);
        let idx_path = pack_path.with_extension("idx");
        for p in [pack_path, &idx_path] {
            if let Ok(metadata) = fs::symlink_metadata(p)
                && !metadata.file_type().is_symlink()
                && (metadata.permissions().mode() & 0o7777) != obj_mode
            {
                let mut perms = metadata.permissions();
                perms.set_mode(obj_mode);
                let _ = fs::set_permissions(p, perms);
            }
        }
    }
    Ok(())
}

/// Adjust permissions for the SQLite database and its sidecars (`-wal`, `-shm`, `-journal`).
pub fn adjust_shared_perm_for_db(db_path: &Path, shared: &SharedMode) -> io::Result<()> {
    if matches!(shared, SharedMode::Umask) {
        return Ok(());
    }
    adjust_shared_perm(db_path, shared)?;
    let db_str = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default();
    for suffix in ["-wal", "-shm", "-journal"] {
        let sidecar = db_path.with_file_name(format!("{db_str}{suffix}"));
        match adjust_shared_perm(&sidecar, shared) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

/// Adjust permissions for the repository index file (`.libra/index`).
pub fn adjust_shared_perm_for_index(index_path: &Path) -> io::Result<()> {
    if let Some(shared_mode) = get_shared_mode_for_path(index_path)? {
        adjust_shared_perm(index_path, &shared_mode)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calc_shared_perm_table() {
        // Group mode (shared == PERM_GROUP in Git)
        let group = SharedMode::Group;
        // Non-executable regular file (0644 -> 0664)
        assert_eq!(calc_shared_perm(0o644, false, &group), 0o664);
        assert_eq!(calc_shared_perm(0o600, false, &group), 0o660);
        // Executable regular file (0755 -> 0775)
        assert_eq!(calc_shared_perm(0o755, false, &group), 0o775);
        // Read-only regular file (0444 -> 0664)
        assert_eq!(calc_shared_perm(0o444, false, &group), 0o664);
        // Directory (0755 -> 02775)
        assert_eq!(calc_shared_perm(0o755, true, &group), 0o2775);
        assert_eq!(calc_shared_perm(0o700, true, &group), 0o2770);

        // All mode (shared == PERM_EVERYBODY in Git)
        let all = SharedMode::All;
        assert_eq!(calc_shared_perm(0o644, false, &all), 0o664);
        assert_eq!(calc_shared_perm(0o755, false, &all), 0o775);
        assert_eq!(calc_shared_perm(0o755, true, &all), 0o2775);

        // Numeric modes (0660 -> dir 02770, file 0660)
        let num_660 = SharedMode::Numeric {
            raw: "0660".to_string(),
            bits: 0o660,
        };
        assert_eq!(calc_shared_perm(0o644, false, &num_660), 0o660);
        assert_eq!(calc_shared_perm(0o755, true, &num_660), 0o2770);

        // Numeric modes (0640 -> dir 02750, file 0640)
        let num_640 = SharedMode::Numeric {
            raw: "0640".to_string(),
            bits: 0o640,
        };
        assert_eq!(calc_shared_perm(0o644, false, &num_640), 0o640);
        assert_eq!(calc_shared_perm(0o755, true, &num_640), 0o2750);

        // Numeric modes (0600 -> dir 0700, file 0600)
        let num_600 = SharedMode::Numeric {
            raw: "0600".to_string(),
            bits: 0o600,
        };
        assert_eq!(calc_shared_perm(0o644, false, &num_600), 0o600);
        assert_eq!(calc_shared_perm(0o755, true, &num_600), 0o700);

        // Umask leaves modes unchanged
        let umask = SharedMode::Umask;
        assert_eq!(calc_shared_perm(0o644, false, &umask), 0o644);
        assert_eq!(calc_shared_perm(0o755, true, &umask), 0o755);
    }

    #[test]
    fn test_calc_shared_object_perm() {
        assert_eq!(calc_shared_object_perm(&SharedMode::Group), 0o444);
        assert_eq!(calc_shared_object_perm(&SharedMode::All), 0o444);
        assert_eq!(calc_shared_object_perm(&SharedMode::Umask), 0o444);

        let num_640 = SharedMode::Numeric {
            raw: "0640".to_string(),
            bits: 0o640,
        };
        assert_eq!(calc_shared_object_perm(&num_640), 0o440);

        let num_660 = SharedMode::Numeric {
            raw: "0660".to_string(),
            bits: 0o660,
        };
        assert_eq!(calc_shared_object_perm(&num_660), 0o440);

        let num_600 = SharedMode::Numeric {
            raw: "0600".to_string(),
            bits: 0o600,
        };
        assert_eq!(calc_shared_object_perm(&num_600), 0o400);
    }

    #[test]
    fn test_process_cache_and_short_circuit() {
        let dummy_root = PathBuf::from("/nonexistent/repo/.libra");
        clear_cached_repo_shared_mode(Some(&dummy_root));

        // Cache insertion and retrieval
        set_cached_repo_shared_mode(dummy_root.clone(), Some(SharedMode::Group));
        assert_eq!(
            get_repo_shared_mode(&dummy_root).unwrap(),
            Some(SharedMode::Group)
        );

        // Clear specific cache entry
        clear_cached_repo_shared_mode(Some(&dummy_root));

        // Non-shared short-circuit
        assert_eq!(calc_shared_perm(0o644, false, &SharedMode::Umask), 0o644);
        assert!(matches!(
            adjust_shared_perm(Path::new("/nonexistent"), &SharedMode::Umask),
            Ok(())
        ));
    }
}
