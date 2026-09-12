//! No-follow physical identities for one bounded worktree scan.

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use super::{RawLstat, dup_file, is_root_rel, open_beneath, reject_dotdot};

/// Unix device/inode or Windows volume/full 128-bit file ID. These keys are
/// scan-local: filesystems may reuse identifiers after an entry is deleted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(crate) struct EntryIdentityKey {
    pub(crate) volume: u64,
    pub(crate) file_id: [u8; 16],
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum EntryKind {
    Directory,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct EntryIdentity {
    pub(crate) key: EntryIdentityKey,
    pub(crate) kind: EntryKind,
}

/// Identify an entry through its pinned root, without following a symlink
/// leaf or intermediate component. Empty relative paths identify the root.
pub(crate) fn entry_identity_beneath(root: &fs::File, rel: &Path) -> io::Result<EntryIdentity> {
    if rel.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "entry identity path must be worktree-relative",
        ));
    }
    reject_dotdot(rel)?;
    if is_root_rel(rel) {
        return identity_for_file(root);
    }
    let name = rel.file_name().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "entry identity path has no file name",
        )
    })?;
    let directory = match rel.parent().filter(|parent| !is_root_rel(parent)) {
        Some(parent) => open_beneath(root, parent)?,
        None => dup_file(root)?,
    };
    identity_in_directory(root, &directory, name)
}

fn entry_kind(stat: &RawLstat) -> EntryKind {
    match (stat.is_symlink, stat.is_dir, stat.is_file) {
        (true, _, _) => EntryKind::Symlink,
        (false, true, _) => EntryKind::Directory,
        (false, false, true) => EntryKind::File,
        (false, false, false) => EntryKind::Other,
    }
}

#[cfg(unix)]
fn identity_for_file(file: &fs::File) -> io::Result<EntryIdentity> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    Ok(EntryIdentity {
        key: EntryIdentityKey {
            volume: metadata.dev(),
            file_id: u128::from(metadata.ino()).to_le_bytes(),
        },
        kind: entry_kind(&RawLstat::from_metadata(&metadata)?),
    })
}

#[cfg(unix)]
fn identity_in_directory(
    _root: &fs::File,
    directory: &fs::File,
    name: &std::ffi::OsStr,
) -> io::Result<EntryIdentity> {
    let stat = super::fstatat_value(directory, name)?;
    Ok(EntryIdentity {
        key: EntryIdentityKey {
            volume: unix_identifier(stat.st_dev)?,
            file_id: u128::from(unix_identifier(stat.st_ino)?).to_le_bytes(),
        },
        kind: entry_kind(&RawLstat::from_libc_stat(&stat)),
    })
}

#[cfg(unix)]
fn unix_identifier(value: impl TryInto<u64>) -> io::Result<u64> {
    value.try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "filesystem identity exceeds its supported range",
        )
    })
}

#[cfg(windows)]
fn identity_in_directory(
    root: &fs::File,
    directory: &fs::File,
    name: &std::ffi::OsStr,
) -> io::Result<EntryIdentity> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::Foundation::HANDLE;

    let root_path = super::final_path_name(root.as_raw_handle() as HANDLE)?;
    let directory_path = super::final_path_name(directory.as_raw_handle() as HANDLE)?;
    let entry = super::open_windows_nofollow(&directory_path.join(name), true, false, false)?;
    super::assert_handle_beneath(&entry, &root_path)?;
    identity_for_file(&entry)
}

#[cfg(windows)]
fn identity_for_file(file: &fs::File) -> io::Result<EntryIdentity> {
    use std::os::windows::io::AsRawHandle;

    use windows_sys::Win32::{
        Foundation::HANDLE,
        Storage::FileSystem::{
            FILE_ID_128, FILE_ID_INFO, FileIdInfo, GetFileInformationByHandleEx,
        },
    };

    let mut info = FILE_ID_INFO {
        VolumeSerialNumber: 0,
        FileId: FILE_ID_128 {
            Identifier: [0; 16],
        },
    };
    let size = u32::try_from(std::mem::size_of::<FILE_ID_INFO>()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows identity buffer is too large",
        )
    })?;
    // SAFETY: the borrowed handle remains live, and info is an aligned,
    // initialized FILE_ID_INFO output buffer with its exact byte size.
    let ok = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle() as HANDLE,
            FileIdInfo,
            (&mut info as *mut FILE_ID_INFO).cast(),
            size,
        )
    };
    if ok == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(EntryIdentity {
        key: EntryIdentityKey {
            volume: info.VolumeSerialNumber,
            file_id: info.FileId.Identifier,
        },
        kind: entry_kind(&RawLstat::from_metadata(&file.metadata()?)?),
    })
}

#[cfg(test)]
mod tests;
