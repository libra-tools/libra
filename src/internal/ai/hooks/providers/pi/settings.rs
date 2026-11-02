//! The Libra-owned Pi project extension asset.
//!
//! Pi loads project extensions from `<project>/.pi/extensions/` only while
//! the project is trusted (ADR-PI-03). Libra owns one file there,
//! `libra-hooks.ts`. Its first line is the ownership marker and its second
//! line records the sha256 of the rest of the file. A file without the
//! marker is the user's; a file whose bytes no longer match the recorded
//! sha256 has drifted. No existing file is ever overwritten: the writer only
//! creates the asset in an empty slot, through directory handles opened
//! without following links, and refuses creation on platforms without them.
//! Installing and removing the asset through `libra agent` belongs to PI-14;
//! this module renders, classifies and creates it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// First line of every Libra-owned Pi extension asset.
pub const PI_ASSET_MARKER: &str = "// libra capture: libra-pi-hooks v1 (managed by libra; local edits stop libra from updating or removing it)";
const DIGEST_PREFIX: &str = "// libra-sha256: ";
const COMMAND_PLACEHOLDER: &str = "__LIBRA_COMMAND_JSON__";
const TEMPLATE: &str = include_str!("libra-hooks.ts");
/// A Libra-owned asset is a few kilobytes; a larger file in the slot is not ours.
const MAX_ASSET_BYTES: u64 = 256 * 1024;

/// Project-relative location of the asset, `.pi/extensions/libra-hooks.ts`.
pub const PI_ASSET_DIR: [&str; 2] = [".pi", "extensions"];
pub const PI_ASSET_FILE: &str = "libra-hooks.ts";

/// Who owns the file currently in the asset slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiAssetOwnership {
    /// No file is there.
    Absent,
    /// The marker is present and the bytes match the recorded sha256.
    Owned,
    /// The marker is present but the bytes no longer match.
    Drifted,
    /// No marker, or not a regular file: the user's.
    Unmanaged,
}

/// What [`write_owned_pi_asset`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PiAssetWrite {
    Created,
    Unchanged,
}

/// Render the asset for `binary`, which must be an absolute path: the
/// extension starts that executable directly, without a shell or `PATH`.
pub fn render_pi_asset(binary: &Path) -> Result<String> {
    if !binary.is_absolute() {
        bail!(
            "the Pi extension needs the absolute path of the libra binary; got {}",
            binary.display()
        );
    }
    let binary = binary.to_str().with_context(|| {
        format!(
            "the libra binary path {} is not valid UTF-8, so the Pi extension cannot name it",
            binary.display()
        )
    })?;
    let command = serde_json::to_string(binary).context("encode the libra binary path")?;
    if TEMPLATE.matches(COMMAND_PLACEHOLDER).count() != 1 {
        bail!("the bundled Pi extension template is malformed");
    }
    let body = TEMPLATE.replacen(COMMAND_PLACEHOLDER, &command, 1);
    Ok(format!(
        "{PI_ASSET_MARKER}\n{DIGEST_PREFIX}{}\n{body}",
        hex_sha256(body.as_bytes())
    ))
}

/// Classify the bytes of a file found in the asset slot.
pub fn classify_pi_asset(bytes: &[u8]) -> PiAssetOwnership {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return PiAssetOwnership::Unmanaged;
    };
    let Some(rest) = text.strip_prefix(PI_ASSET_MARKER) else {
        return PiAssetOwnership::Unmanaged;
    };
    let Some(rest) = rest.strip_prefix('\n') else {
        return PiAssetOwnership::Unmanaged;
    };
    let Some((digest_line, body)) = rest.split_once('\n') else {
        return PiAssetOwnership::Drifted;
    };
    match digest_line.strip_prefix(DIGEST_PREFIX) {
        Some(recorded) if recorded == hex_sha256(body.as_bytes()) => PiAssetOwnership::Owned,
        _ => PiAssetOwnership::Drifted,
    }
}

/// The asset path under `project_root`.
pub fn pi_asset_path(project_root: &Path) -> PathBuf {
    let mut path = project_root.to_path_buf();
    for part in PI_ASSET_DIR {
        path.push(part);
    }
    path.push(PI_ASSET_FILE);
    path
}

/// Inspect the asset slot under `project_root` without following links. A
/// `.pi` or `extensions` entry that is not a plain directory makes the slot
/// the user's.
pub fn pi_asset_ownership(project_root: &Path) -> Result<PiAssetOwnership> {
    #[cfg(unix)]
    {
        unix_slot::ownership(project_root)
    }
    #[cfg(not(unix))]
    {
        path_slot::ownership(project_root)
    }
}

/// Create the asset for `binary` under `project_root` in an empty slot. The
/// same unmodified asset is left as it is. Any other existing entry (a user
/// file, a drifted or older Libra asset, a non-regular entry) and a `.pi` or
/// `extensions` entry that is not a plain directory are left exactly as they
/// are and reported as an error, because replacing a path cannot prove its
/// owner at that moment. On Unix both directories are opened without
/// following links and the file is created through those handles, so a
/// directory swapped for a link after the check does not redirect the write.
/// Other platforms have no such handle here, so creation is refused there
/// and nothing is written.
pub fn write_owned_pi_asset(project_root: &Path, binary: &Path) -> Result<PiAssetWrite> {
    write_owned_pi_asset_inner(project_root, binary, &mut || {})
}

/// [`write_owned_pi_asset`] with `pause` run after the slot is checked and
/// before the file is created, so a test can change the tree in between.
#[cfg(any(test, debug_assertions))]
#[doc(hidden)]
pub fn write_owned_pi_asset_pausing_for_test(
    project_root: &Path,
    binary: &Path,
    pause: &mut dyn FnMut(),
) -> Result<PiAssetWrite> {
    write_owned_pi_asset_inner(project_root, binary, pause)
}

fn write_owned_pi_asset_inner(
    project_root: &Path,
    binary: &Path,
    pause: &mut dyn FnMut(),
) -> Result<PiAssetWrite> {
    let rendered = render_pi_asset(binary)?;
    #[cfg(unix)]
    {
        unix_slot::create(project_root, &rendered, pause)
    }
    #[cfg(not(unix))]
    {
        path_slot::create(project_root, &rendered, pause)
    }
}

/// Refuse every occupied slot except the same unmodified asset.
#[cfg(unix)]
fn refuse_occupied(
    path: &Path,
    ownership: PiAssetOwnership,
    current: Option<&[u8]>,
    rendered: &str,
) -> Result<Option<PiAssetWrite>> {
    match ownership {
        PiAssetOwnership::Absent => Ok(None),
        PiAssetOwnership::Owned if current == Some(rendered.as_bytes()) => {
            Ok(Some(PiAssetWrite::Unchanged))
        }
        PiAssetOwnership::Owned => bail!(
            "{} holds a libra-owned Pi extension for another libra binary; libra does not replace an existing file. Remove it to let libra write the current one",
            path.display()
        ),
        PiAssetOwnership::Drifted => bail!(
            "{} was changed after libra wrote it; libra leaves it as it is. Remove the file to let libra write it again",
            path.display()
        ),
        PiAssetOwnership::Unmanaged => bail!(
            "{} is not a libra-managed Pi extension; libra leaves it as it is. Rename or remove it to let libra write its own",
            path.display()
        ),
    }
}

/// Classify bytes read from the slot, or the slot's absence.
fn classify_slot(bytes: Option<&[u8]>) -> PiAssetOwnership {
    match bytes {
        None => PiAssetOwnership::Absent,
        Some(bytes) if bytes.len() as u64 > MAX_ASSET_BYTES => PiAssetOwnership::Unmanaged,
        Some(bytes) => classify_pi_asset(bytes),
    }
}

#[cfg(unix)]
mod unix_slot {
    use std::{
        ffi::CString,
        fs,
        io::{ErrorKind, Read, Write},
        os::fd::{AsRawFd, FromRawFd},
        path::Path,
    };

    use anyhow::{Context, Result, bail};

    use super::{
        MAX_ASSET_BYTES, PI_ASSET_DIR, PI_ASSET_FILE, PiAssetOwnership, PiAssetWrite,
        classify_slot, pi_asset_path, refuse_occupied,
    };

    /// What the slot holds, read through the pinned extensions directory.
    enum Slot {
        Absent,
        /// A link, a non-regular entry or an oversized file.
        Foreign,
        File(Vec<u8>),
    }

    /// Open `.pi/extensions` beneath `project_root` without following links,
    /// creating missing directories when `create` is set. `Ok(None)` means a
    /// directory is missing (and was not created); an entry that is not a
    /// plain directory is an error.
    fn open_dirs(project_root: &Path, create: bool) -> Result<Option<fs::File>> {
        let mut directory = crate::utils::beneath::open_root(project_root).with_context(|| {
            format!(
                "failed to open the project directory {}",
                project_root.display()
            )
        })?;
        for part in PI_ASSET_DIR {
            if create {
                let name = CString::new(part).context("invalid Pi extension directory name")?;
                // SAFETY: `directory` owns a live descriptor and `name` is NUL-terminated.
                let made = unsafe { libc::mkdirat(directory.as_raw_fd(), name.as_ptr(), 0o755) };
                if made != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.kind() != ErrorKind::AlreadyExists {
                        return Err(error).with_context(|| {
                            format!(
                                "failed to create the Pi extension directory {part} under {}",
                                project_root.display()
                            )
                        });
                    }
                }
            }
            directory = match crate::utils::beneath::open_beneath(&directory, Path::new(part)) {
                Ok(next) => next,
                Err(error) if error.kind() == ErrorKind::NotFound && !create => return Ok(None),
                Err(_) => bail!(
                    "{part} under {} is not a plain directory, so libra will not write the Pi extension there",
                    project_root.display()
                ),
            };
        }
        Ok(Some(directory))
    }

    fn read_slot(directory: &fs::File) -> Result<Slot> {
        let name = CString::new(PI_ASSET_FILE).context("invalid Pi extension file name")?;
        // SAFETY: `directory` owns a live descriptor and `name` is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return match error.raw_os_error() {
                Some(libc::ENOENT) => Ok(Slot::Absent),
                Some(libc::ELOOP) => Ok(Slot::Foreign),
                _ => Err(error).context("failed to open the Pi extension"),
            };
        }
        // SAFETY: a successful openat returned a new descriptor that we now own.
        let file = unsafe { fs::File::from_raw_fd(fd) };
        let metadata = file
            .metadata()
            .context("failed to inspect the Pi extension")?;
        if !metadata.is_file() || metadata.len() > MAX_ASSET_BYTES {
            return Ok(Slot::Foreign);
        }
        let mut bytes = Vec::new();
        file.take(MAX_ASSET_BYTES + 1)
            .read_to_end(&mut bytes)
            .context("failed to read the Pi extension")?;
        if bytes.len() as u64 > MAX_ASSET_BYTES {
            return Ok(Slot::Foreign);
        }
        Ok(Slot::File(bytes))
    }

    fn ownership_of(slot: &Slot) -> PiAssetOwnership {
        match slot {
            Slot::Absent => PiAssetOwnership::Absent,
            Slot::Foreign => PiAssetOwnership::Unmanaged,
            Slot::File(bytes) => classify_slot(Some(bytes)),
        }
    }

    pub(super) fn ownership(project_root: &Path) -> Result<PiAssetOwnership> {
        let directory = match open_dirs(project_root, false) {
            Ok(Some(directory)) => directory,
            Ok(None) => return Ok(PiAssetOwnership::Absent),
            // A `.pi` or `extensions` entry that is not a plain directory.
            Err(_) => return Ok(PiAssetOwnership::Unmanaged),
        };
        Ok(ownership_of(&read_slot(&directory)?))
    }

    pub(super) fn create(
        project_root: &Path,
        rendered: &str,
        pause: &mut dyn FnMut(),
    ) -> Result<PiAssetWrite> {
        let path = pi_asset_path(project_root);
        let Some(directory) = open_dirs(project_root, true)? else {
            bail!(
                "the Pi extension directory under {} could not be opened",
                project_root.display()
            );
        };
        let slot = read_slot(&directory)?;
        let current = match &slot {
            Slot::File(bytes) => Some(bytes.as_slice()),
            _ => None,
        };
        if let Some(done) = refuse_occupied(&path, ownership_of(&slot), current, rendered)? {
            return Ok(done);
        }
        pause();
        let temporary = CString::new(format!(".libra-hooks-{}.tmp", uuid::Uuid::new_v4()))
            .context("invalid temporary Pi extension name")?;
        let target = CString::new(PI_ASSET_FILE).context("invalid Pi extension file name")?;
        // SAFETY: `directory` owns a live descriptor and the name is NUL-terminated.
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                temporary.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o644,
            )
        };
        if fd < 0 {
            return Err(std::io::Error::last_os_error()).with_context(|| {
                format!(
                    "failed to create a temporary file next to {}",
                    path.display()
                )
            });
        }
        // SAFETY: a successful openat returned a new descriptor that we now own.
        let mut file = unsafe { fs::File::from_raw_fd(fd) };
        let published = (|| -> Result<()> {
            file.write_all(rendered.as_bytes())
                .with_context(|| format!("failed to write the Pi extension {}", path.display()))?;
            file.sync_all()
                .with_context(|| format!("failed to flush the Pi extension {}", path.display()))?;
            // SAFETY: both names are relative to the same pinned directory. A
            // hard link never replaces an existing destination.
            let linked = unsafe {
                libc::linkat(
                    directory.as_raw_fd(),
                    temporary.as_ptr(),
                    directory.as_raw_fd(),
                    target.as_ptr(),
                    0,
                )
            };
            if linked != 0 {
                return Err(std::io::Error::last_os_error()).with_context(|| {
                    format!(
                        "failed to create the Pi extension {} without replacing another file",
                        path.display()
                    )
                });
            }
            Ok(())
        })();
        // SAFETY: remove only our own temporary name in the pinned directory.
        unsafe {
            libc::unlinkat(directory.as_raw_fd(), temporary.as_ptr(), 0);
        }
        published?;
        directory.sync_all().with_context(|| {
            format!(
                "failed to persist the Pi extension directory for {}",
                path.display()
            )
        })?;
        Ok(PiAssetWrite::Created)
    }
}

#[cfg(not(unix))]
mod path_slot {
    use std::{
        fs,
        io::{ErrorKind, Read},
        path::Path,
    };

    use anyhow::{Context, Result, bail};

    use super::{
        MAX_ASSET_BYTES, PI_ASSET_DIR, PiAssetOwnership, PiAssetWrite, classify_slot, pi_asset_path,
    };

    /// `Ok(false)` when a directory is missing; an entry that is not a plain
    /// directory is an error.
    fn plain_dirs(project_root: &Path) -> Result<bool> {
        let mut dir = project_root.to_path_buf();
        for part in PI_ASSET_DIR {
            dir.push(part);
            match fs::symlink_metadata(&dir) {
                Ok(metadata) if metadata.file_type().is_dir() => {}
                Ok(_) => bail!("{} is not a plain directory", dir.display()),
                Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
                Err(error) => {
                    return Err(error).with_context(|| {
                        format!(
                            "failed to inspect the Pi extension directory {}",
                            dir.display()
                        )
                    });
                }
            }
        }
        Ok(true)
    }

    pub(super) fn ownership(project_root: &Path) -> Result<PiAssetOwnership> {
        match plain_dirs(project_root) {
            Ok(true) => {}
            Ok(false) => return Ok(PiAssetOwnership::Absent),
            Err(_) => return Ok(PiAssetOwnership::Unmanaged),
        }
        let path = pi_asset_path(project_root);
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == ErrorKind::NotFound => Ok(PiAssetOwnership::Absent),
            Err(error) => Err(error)
                .with_context(|| format!("failed to inspect the Pi extension {}", path.display())),
            Ok(metadata) if !metadata.file_type().is_file() => Ok(PiAssetOwnership::Unmanaged),
            Ok(metadata) if metadata.len() > MAX_ASSET_BYTES => Ok(PiAssetOwnership::Unmanaged),
            Ok(_) => {
                let mut bytes = Vec::new();
                fs::File::open(&path)
                    .and_then(|file| file.take(MAX_ASSET_BYTES + 1).read_to_end(&mut bytes))
                    .with_context(|| {
                        format!("failed to read the Pi extension {}", path.display())
                    })?;
                Ok(classify_slot(Some(&bytes)))
            }
        }
    }

    /// Creation is refused here: without a directory handle that can be
    /// pinned against a swapped junction, a write by path could land
    /// outside the project. Nothing is written.
    pub(super) fn create(
        project_root: &Path,
        _rendered: &str,
        _pause: &mut dyn FnMut(),
    ) -> Result<PiAssetWrite> {
        bail!(
            "libra cannot create the Pi extension under {} on this platform: it writes the file only through directory handles pinned against links, which are available on Unix-like systems only. Nothing was written",
            project_root.display()
        )
    }
}

#[cfg(all(test, not(unix)))]
mod tests {
    use super::*;

    // Without pinned directory handles creation is refused and nothing,
    // not even the `.pi` directory, is written.
    #[test]
    fn pi_observe_asset_creation_is_refused_without_pinned_directories() {
        let temp = tempfile::tempdir().expect("temp project");
        let binary = temp.path().join("libra.exe");
        let error = write_owned_pi_asset(temp.path(), &binary)
            .expect_err("creation must be refused on this platform");
        assert!(
            error.to_string().contains("Nothing was written"),
            "{error:#}"
        );
        assert!(!temp.path().join(".pi").exists());
        assert_eq!(
            pi_asset_ownership(temp.path()).expect("inspect slot"),
            PiAssetOwnership::Absent
        );
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
