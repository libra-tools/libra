//! Windows filesystem capabilities. Every component is opened without
//! following reparse points. Ordinary directory pins deny write/delete sharing;
//! private mutation directories use handle-relative operations and deny deletion.

use std::{
    ffi::{OsStr, OsString},
    fs::File,
    io,
    os::windows::{
        ffi::{OsStrExt, OsStringExt},
        io::{AsRawHandle, FromRawHandle, OwnedHandle},
    },
    path::{Component, Path, PathBuf, Prefix},
    ptr::{null, null_mut},
};

use windows_sys::{
    Wdk::{
        Foundation::OBJECT_ATTRIBUTES,
        Storage::FileSystem::{
            FILE_CREATE, FILE_DIRECTORY_FILE, FILE_DISPOSITION_DELETE,
            FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE, FILE_DISPOSITION_INFORMATION,
            FILE_DISPOSITION_INFORMATION_EX, FILE_DISPOSITION_POSIX_SEMANTICS,
            FILE_NAMES_INFORMATION, FILE_NON_DIRECTORY_FILE, FILE_OPEN, FILE_OPEN_REPARSE_POINT,
            FILE_RENAME_INFORMATION, FILE_SYNCHRONOUS_IO_NONALERT, FileDispositionInformation,
            FileDispositionInformationEx, FileNamesInformation, FileRenameInformation,
            NtCreateFile, NtQueryDirectoryFile, NtSetInformationFile,
        },
    },
    Win32::{
        Foundation::{
            ERROR_LOCK_VIOLATION, GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE, LocalFree,
            OBJ_CASE_INSENSITIVE, OBJ_DONT_REPARSE, RtlNtStatusToDosError, STATUS_BUFFER_OVERFLOW,
            STATUS_NO_MORE_FILES, UNICODE_STRING,
        },
        Security::{
            ACCESS_ALLOWED_ACE, ACE_HEADER, ACL,
            Authorization::{
                ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW,
                GetSecurityInfo, SE_FILE_OBJECT,
            },
            DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
            GetTokenInformation, IsValidAcl, IsValidSid, IsWellKnownSid,
            OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, SECURITY_ATTRIBUTES, TOKEN_QUERY,
            TOKEN_USER, TokenUser, WinLocalSystemSid,
        },
        Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, CreateDirectoryW, CreateFileW, DELETE,
            FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
            FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE, FILE_SHARE_READ,
            FILE_SHARE_WRITE, FILE_TYPE_DISK, GetFileInformationByHandle, GetFileType,
            GetFinalPathNameByHandleW, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY,
            LockFileEx, OPEN_EXISTING, SYNCHRONIZE,
        },
        System::{
            IO::{IO_STATUS_BLOCK, OVERLAPPED},
            Threading::{GetCurrentProcess, OpenProcessToken},
        },
    },
};

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

pub fn wide(value: &OsStr) -> io::Result<Vec<u16>> {
    let mut result: Vec<_> = value.encode_wide().collect();
    if result.contains(&0) {
        return Err(invalid("Windows path contains NUL"));
    }
    result.push(0);
    Ok(result)
}

pub fn validate_component(name: &OsStr) -> io::Result<()> {
    let units: Vec<_> = name.encode_wide().collect();
    let normal = matches!(
        Path::new(name).components().next(),
        Some(Component::Normal(_))
    ) && Path::new(name).components().count() == 1;
    if !normal
        || units.is_empty()
        || units
            .iter()
            .any(|c| *c < 32 || b"\\/:*?\"<>|".contains(&(*c as u8)) && *c < 128)
        || matches!(units.last(), Some(32 | 46))
    {
        return Err(invalid(
            "Windows child name must be one normal component without streams or aliases",
        ));
    }
    let stem = name
        .to_string_lossy()
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    if matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || (stem.len() == 4
        && (stem.starts_with("COM") || stem.starts_with("LPT"))
        && stem.as_bytes()[3].is_ascii_digit())
    {
        return Err(invalid("Windows device names cannot be capture sources"));
    }
    Ok(())
}

pub fn information(file: &File) -> io::Result<BY_HANDLE_FILE_INFORMATION> {
    // SAFETY: the output is initialized by the successful call, and the handle is live.
    unsafe {
        let mut info = std::mem::zeroed();
        if GetFileInformationByHandle(file.as_raw_handle(), &mut info) == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(info)
    }
}

pub fn identity(file: &File) -> io::Result<(u64, u64)> {
    let info = information(file)?;
    Ok((
        info.dwVolumeSerialNumber as u64,
        ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64,
    ))
}

pub fn handle_path(file: &File) -> io::Result<PathBuf> {
    let mut buffer = vec![0u16; 512];
    loop {
        // SAFETY: the output buffer has the declared capacity and the file is live.
        let size = unsafe {
            GetFinalPathNameByHandleW(
                file.as_raw_handle(),
                buffer.as_mut_ptr(),
                buffer.len() as u32,
                0,
            )
        };
        if size == 0 {
            return Err(io::Error::last_os_error());
        }
        if (size as usize) < buffer.len() {
            buffer.truncate(size as usize);
            return Ok(PathBuf::from(OsString::from_wide(&buffer)));
        }
        if size > 32768 {
            return Err(invalid("Windows capture path exceeds its length limit"));
        }
        buffer.resize(size as usize + 1, 0);
    }
}

fn open(
    path: &Path,
    directory: bool,
    access: u32,
    disposition: u32,
    attributes: *const SECURITY_ATTRIBUTES,
) -> io::Result<File> {
    open_with_share(
        path,
        directory,
        access,
        disposition,
        attributes,
        FILE_SHARE_READ,
    )
}

fn open_with_share(
    path: &Path,
    directory: bool,
    access: u32,
    disposition: u32,
    attributes: *const SECURITY_ATTRIBUTES,
    sharing: u32,
) -> io::Result<File> {
    let path = wide(path.as_os_str())?;
    // SAFETY: path is terminated, security attributes (when supplied) live for this call.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            access,
            sharing,
            attributes,
            disposition,
            FILE_FLAG_OPEN_REPARSE_POINT | FILE_FLAG_BACKUP_SEMANTICS,
            null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: CreateFileW returned a new owned handle; ownership transfers exactly once.
    let file = unsafe { File::from_raw_handle(handle) };
    let info = information(&file)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || file.metadata()?.is_dir() != directory
        // SAFETY: GetFileType only inspects this live handle.
        || unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture source must be a disk file/directory, without reparse points",
        ));
    }
    Ok(file)
}

/// Retain the entire ancestry until a child has been opened. Denying write and
/// delete sharing prevents junction conversion and rename/swap during traversal.
pub fn pin_tree(path: &Path) -> io::Result<Vec<File>> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut parts = absolute.components();
    let Some(Component::Prefix(prefix)) = parts.next() else {
        return Err(invalid(
            "capture directory must have an absolute Windows drive",
        ));
    };
    if !matches!(prefix.kind(), Prefix::Disk(_) | Prefix::VerbatimDisk(_)) {
        return Err(invalid(
            "capture requires a local Windows drive; UNC and device paths are unsupported",
        ));
    }
    if !matches!(parts.next(), Some(Component::RootDir)) {
        return Err(invalid("capture directory must be absolute"));
    }
    let mut current = PathBuf::from(prefix.as_os_str());
    current.push("\\");
    let mut handles = vec![open(&current, true, GENERIC_READ, OPEN_EXISTING, null())?];
    for component in parts {
        let name = match component {
            Component::CurDir => continue,
            Component::Normal(name) => name,
            _ => return Err(invalid("capture path contains a non-normal component")),
        };
        validate_component(name)?;
        current.push(name);
        handles.push(open(&current, true, GENERIC_READ, OPEN_EXISTING, null())?);
    }
    Ok(handles)
}

pub fn open_tree(path: &Path) -> io::Result<File> {
    pin_tree(path)?
        .pop()
        .ok_or_else(|| invalid("capture directory is empty"))
}

/// Open a directory for handle-relative workspace mutations. The strict pin
/// proves the final identity before the mutable handle permits child writes;
/// deletion sharing remains denied so the directory itself cannot be renamed.
pub fn open_mutable_tree(path: &Path) -> io::Result<File> {
    let pinned = open_tree(path)?;
    let resolved = handle_path(&pinned)?;
    let mutable = open_with_share(
        &resolved,
        true,
        GENERIC_READ,
        OPEN_EXISTING,
        null(),
        FILE_SHARE_READ | FILE_SHARE_WRITE,
    )?;
    if identity(&mutable)? != identity(&pinned)? {
        return Err(io::Error::other(
            "workspace directory changed while opening; retry the command",
        ));
    }
    Ok(mutable)
}

pub struct PinnedParent {
    _ancestry: Vec<File>,
    path: PathBuf,
}

impl PinnedParent {
    pub fn new(parent: &File) -> io::Result<Self> {
        let path = handle_path(parent)?;
        let ancestry = pin_tree(&path)?;
        let reopened = ancestry
            .last()
            .ok_or_else(|| invalid("capture parent has no directory"))?;
        if identity(reopened)? != identity(parent)? {
            return Err(io::Error::other(
                "capture parent changed while opening; retry the command",
            ));
        }
        Ok(Self {
            _ancestry: ancestry,
            path,
        })
    }

    pub fn child(&self, name: &OsStr) -> io::Result<PathBuf> {
        validate_component(name)?;
        Ok(self.path.join(name))
    }
}

pub fn open_at(parent: &File, name: &OsStr, directory: bool) -> io::Result<File> {
    open_relative(
        parent,
        name,
        directory,
        GENERIC_READ,
        FILE_OPEN,
        FILE_SHARE_READ,
        null(),
    )
}

pub fn open_mutable_at(parent: &File, name: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        name,
        true,
        GENERIC_READ,
        FILE_OPEN,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        null(),
    )
}

// RootDirectory is a capability, not a pathname. This also keeps private
// mutations safe while their parent permits write sharing for atomic rename.
#[allow(clippy::too_many_arguments)]
fn open_relative(
    parent: &File,
    name: &OsStr,
    directory: bool,
    access: u32,
    disposition: u32,
    sharing: u32,
    descriptor: *const windows_sys::Win32::Security::SECURITY_DESCRIPTOR,
) -> io::Result<File> {
    let file = open_relative_handle(
        parent,
        name,
        access,
        disposition,
        sharing,
        descriptor,
        if directory {
            FILE_DIRECTORY_FILE
        } else {
            FILE_NON_DIRECTORY_FILE
        },
        true,
    )?;
    if information(&file)?.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
        || file.metadata()?.is_dir() != directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "capture source must be a disk file/directory, without reparse points",
        ));
    }
    Ok(file)
}

#[allow(clippy::too_many_arguments)]
fn open_relative_handle(
    parent: &File,
    name: &OsStr,
    access: u32,
    disposition: u32,
    sharing: u32,
    descriptor: *const windows_sys::Win32::Security::SECURITY_DESCRIPTOR,
    type_options: u32,
    reject_reparse: bool,
) -> io::Result<File> {
    validate_component(name)?;
    let parent_info = information(parent)?;
    if parent_info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || parent_info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(invalid(
            "Windows parent must be an ordinary pinned directory",
        ));
    }
    let mut name = wide(name)?;
    let length = u16::try_from((name.len() - 1) * 2)
        .map_err(|_| invalid("Windows child name exceeds its length limit"))?;
    let unicode = UNICODE_STRING {
        Length: length,
        MaximumLength: length,
        Buffer: name.as_mut_ptr(),
    };
    let attributes = OBJECT_ATTRIBUTES {
        Length: std::mem::size_of::<OBJECT_ATTRIBUTES>() as u32,
        RootDirectory: parent.as_raw_handle(),
        ObjectName: &unicode,
        Attributes: OBJ_CASE_INSENSITIVE | if reject_reparse { OBJ_DONT_REPARSE } else { 0 },
        SecurityDescriptor: descriptor,
        SecurityQualityOfService: null(),
    };
    let mut handle = null_mut();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: all buffers and the root directory remain live for this synchronous
    // call. Only a validated single component can be resolved beneath the root.
    let status = unsafe {
        NtCreateFile(
            &mut handle,
            access | SYNCHRONIZE,
            &attributes,
            &mut status_block,
            null(),
            0,
            sharing,
            disposition,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT | type_options,
            null(),
            0,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    // SAFETY: the successful call transferred one fresh handle to us.
    let file = unsafe { File::from_raw_handle(handle) };
    // SAFETY: GetFileType only inspects the live handle returned above.
    if unsafe { GetFileType(file.as_raw_handle()) } != FILE_TYPE_DISK {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows child must be a disk file or directory",
        ));
    }
    Ok(file)
}

fn nt_error(status: i32) -> io::Error {
    // SAFETY: this pure conversion takes no pointers.
    io::Error::from_raw_os_error(unsafe { RtlNtStatusToDosError(status) } as i32)
}

pub fn open_private_directory(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| invalid("private directory has no parent"))?;
    let name = path
        .file_name()
        .ok_or_else(|| invalid("private directory has no filename"))?;
    let parent = open_tree(parent)?;
    let file = open_relative(
        &parent,
        name,
        true,
        GENERIC_READ,
        FILE_OPEN,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        null(),
    )?;
    validate_private(&file)?;
    Ok(file)
}

/// Atomically rename the held file beneath the held target directory, without
/// replacing a pre-existing key and without resolving any ancestor pathname.
pub fn publish_private_file(file: &File, target: &File, name: &OsStr) -> io::Result<()> {
    validate_component(name)?;
    let name: Vec<_> = name.encode_wide().collect();
    let length = std::mem::size_of::<FILE_RENAME_INFORMATION>() + name.len() * 2;
    let mut buffer = vec![0usize; length.div_ceil(std::mem::size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: aligned, zeroed storage is large enough for the variable-length
    // name. ReplaceIfExists remains false and both handles are held throughout.
    let status = unsafe {
        (*info).RootDirectory = target.as_raw_handle();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            info.cast(),
            length as u32,
            FileRenameInformation,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    file.sync_all()
}

pub fn remove_private_file_at(parent: &File, name: &OsStr) -> io::Result<()> {
    let file = open_relative(
        parent,
        name,
        false,
        GENERIC_READ | DELETE,
        FILE_OPEN,
        FILE_SHARE_READ,
        null(),
    )?;
    validate_private(&file)?;
    if information(&file)?.nNumberOfLinks != 1 {
        return Err(invalid("private staging file has unexpected hard links"));
    }
    let info = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: the synchronous request references a live owned regular file.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            (&info as *const FILE_DISPOSITION_INFORMATION).cast(),
            std::mem::size_of_val(&info) as u32,
            FileDispositionInformation,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    Ok(())
}

/// Live transcript writers can retain and rotate their own file handles. The
/// reader authorizes this opened file once and consumes only its held handle.
/// Keys, locks and trusted executables continue to use the stricter opener.
pub fn open_live_source_at(parent: &File, name: &OsStr) -> io::Result<File> {
    open_relative(
        parent,
        name,
        false,
        GENERIC_READ,
        FILE_OPEN,
        FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
        null(),
    )
}

pub fn create_directory_at(parent: &File, name: &OsStr, private: bool) -> io::Result<File> {
    if private {
        let descriptor = private_descriptor()?;
        let open_directory = |disposition| {
            open_relative(
                parent,
                name,
                true,
                GENERIC_READ,
                disposition,
                FILE_SHARE_READ | FILE_SHARE_WRITE,
                descriptor.0.cast(),
            )
        };
        let file = match open_directory(FILE_CREATE) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                open_directory(FILE_OPEN)?
            }
            Err(error) => return Err(error),
        };
        validate_private(&file)?;
        return Ok(file);
    }
    let pin = PinnedParent::new(parent)?;
    let path = pin.child(name)?;
    let result = std::fs::create_dir(&path);
    match result {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    let file = open(&path, true, GENERIC_READ, OPEN_EXISTING, null())?;
    Ok(file)
}

/// Reserve a new owned workspace directory; an existing entry is always an
/// error. Child writes are allowed, while renaming or deleting the held
/// directory remains denied.
pub fn exclusive_create_mutable_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    let descriptor = private_descriptor()?;
    let directory = open_relative(
        parent,
        name,
        true,
        GENERIC_READ,
        FILE_CREATE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        descriptor.0.cast(),
    )?;
    validate_private(&directory)?;
    Ok(directory)
}

/// Create or open a shared managed container without reconstructing its path.
/// Existing files and reparse points are rejected, including on network shares.
pub fn ensure_directory_at(parent: &File, name: &OsStr) -> io::Result<File> {
    let open_directory = |disposition| {
        open_relative(
            parent,
            name,
            true,
            GENERIC_READ,
            disposition,
            FILE_SHARE_READ,
            null(),
        )
    };
    match open_directory(FILE_CREATE) {
        Ok(directory) => Ok(directory),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => open_directory(FILE_OPEN),
        Err(error) => Err(error),
    }
}

/// Identity of the directory entry itself, including a junction or symlink.
/// Reparse entries may be unlinked, but must never be traversed as directories.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EntryIdentity {
    pub identity: (u64, u64),
    pub directory: bool,
    pub reparse_point: bool,
}

pub fn inspect_entry_at(parent: &File, name: &OsStr) -> io::Result<EntryIdentity> {
    let file = open_relative_handle(
        parent,
        name,
        FILE_READ_ATTRIBUTES,
        FILE_OPEN,
        FILE_SHARE_READ,
        null(),
        0,
        false,
    )?;
    let info = information(&file)?;
    Ok(EntryIdentity {
        identity: identity(&file)?,
        directory: info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0,
        reparse_point: info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0,
    })
}

/// Move the authorized directory between held parents without replacement.
/// Callers must release earlier source pins before requesting DELETE access.
pub fn rename_entry_at(
    source_parent: &File,
    source_name: &OsStr,
    expected_identity: (u64, u64),
    destination_parent: &File,
    destination_name: &OsStr,
) -> io::Result<()> {
    rename_entry_kind_at(
        source_parent,
        source_name,
        expected_identity,
        destination_parent,
        destination_name,
        true,
    )
}

/// Publish a complete private proof file under a new name without replacing
/// any existing record. The caller closes its staging write handle first.
pub fn rename_file_at(
    source_parent: &File,
    source_name: &OsStr,
    expected_identity: (u64, u64),
    destination_parent: &File,
    destination_name: &OsStr,
) -> io::Result<()> {
    rename_entry_kind_at(
        source_parent,
        source_name,
        expected_identity,
        destination_parent,
        destination_name,
        false,
    )
}

fn rename_entry_kind_at(
    source_parent: &File,
    source_name: &OsStr,
    expected_identity: (u64, u64),
    destination_parent: &File,
    destination_name: &OsStr,
    directory: bool,
) -> io::Result<()> {
    validate_component(destination_name)?;
    let destination_info = information(destination_parent)?;
    if destination_info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || destination_info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(invalid(
            "rename destination must be an ordinary pinned directory",
        ));
    }
    let source = open_relative(
        source_parent,
        source_name,
        directory,
        FILE_READ_ATTRIBUTES | DELETE,
        FILE_OPEN,
        FILE_SHARE_READ,
        null(),
    )?;
    if identity(&source)? != expected_identity {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "workspace entry changed before moving; inspect both locations and retry",
        ));
    }
    let name: Vec<_> = destination_name.encode_wide().collect();
    let length = std::mem::size_of::<FILE_RENAME_INFORMATION>() + name.len() * 2;
    let mut buffer = vec![0usize; length.div_ceil(std::mem::size_of::<usize>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFORMATION>();
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: the aligned buffer holds the complete variable-length name. The
    // checked source and destination handles remain held, and the zeroed
    // ReplaceIfExists field forbids replacing any existing destination entry.
    let status = unsafe {
        (*info).RootDirectory = destination_parent.as_raw_handle();
        (*info).FileNameLength = (name.len() * 2) as u32;
        std::ptr::copy_nonoverlapping(name.as_ptr(), (*info).FileName.as_mut_ptr(), name.len());
        NtSetInformationFile(
            source.as_raw_handle(),
            &mut status_block,
            info.cast(),
            length as u32,
            FileRenameInformation,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    Ok(())
}

/// Delete only the entry with the identity previously authorized by the caller.
/// The parent is a capability and the final component is opened without reparse
/// processing, so removing a junction never visits or deletes its target.
/// Drop any earlier child pin before calling: pins deliberately deny deletion.
pub fn remove_entry_at(
    parent: &File,
    name: &OsStr,
    expected_identity: (u64, u64),
    directory: bool,
) -> io::Result<()> {
    let file = open_relative_handle(
        parent,
        name,
        FILE_READ_ATTRIBUTES | DELETE,
        FILE_OPEN,
        FILE_SHARE_READ,
        null(),
        0,
        false,
    )?;
    let info = information(&file)?;
    if identity(&file)? != expected_identity
        || (info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0) != directory
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "workspace entry changed before removal; retain its ownership record and retry cleanup",
        ));
    }
    // Ignore the entry's readonly bit without changing attributes on an outside
    // hard link to the same file. This matches scoped checkpoint-input cleanup.
    let disposition = FILE_DISPOSITION_INFORMATION_EX {
        Flags: FILE_DISPOSITION_DELETE
            | FILE_DISPOSITION_POSIX_SEMANTICS
            | FILE_DISPOSITION_IGNORE_READONLY_ATTRIBUTE,
    };
    let mut status_block = IO_STATUS_BLOCK::default();
    // SAFETY: the held no-follow handle identifies the checked entry and denies
    // write/delete sharing until close. Windows itself rejects nonempty folders.
    let status = unsafe {
        NtSetInformationFile(
            file.as_raw_handle(),
            &mut status_block,
            (&disposition as *const FILE_DISPOSITION_INFORMATION_EX).cast(),
            std::mem::size_of_val(&disposition) as u32,
            FileDispositionInformationEx,
        )
    };
    if status < 0 {
        return Err(nt_error(status));
    }
    // No further operations are allowed on a handle after delete disposition.
    drop(file);
    Ok(())
}

/// Enumerate through the held directory handle. The caller must serialize scans
/// of this handle and recheck each child's identity before acting on a result.
#[cfg(test)]
pub fn read_directory_names(directory: &File) -> io::Result<Vec<OsString>> {
    read_directory_names_bounded(directory, 1_000_000)
}

pub fn read_directory_names_bounded(
    directory: &File,
    max_entries: usize,
) -> io::Result<Vec<OsString>> {
    let info = information(directory)?;
    if info.dwFileAttributes & FILE_ATTRIBUTE_DIRECTORY == 0
        || info.dwFileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0
    {
        return Err(invalid(
            "workspace listing requires an ordinary pinned directory",
        ));
    }
    // usize storage supplies the native structure alignment. A bounded resize
    // also accommodates a provider returning an unusually long component.
    let mut buffer = vec![0usize; 65_536 / std::mem::size_of::<usize>()];
    let mut restart = true;
    let mut names = Vec::new();
    loop {
        let mut status_block = IO_STATUS_BLOCK::default();
        // SAFETY: the synchronous directory handle and aligned output storage
        // remain live; no pathname is reconstructed or resolved for this query.
        let status = unsafe {
            NtQueryDirectoryFile(
                directory.as_raw_handle(),
                null_mut(),
                None,
                null(),
                &mut status_block,
                buffer.as_mut_ptr().cast(),
                std::mem::size_of_val(buffer.as_slice()) as u32,
                FileNamesInformation,
                false,
                null(),
                restart,
            )
        };
        if status == STATUS_NO_MORE_FILES {
            return Ok(names);
        }
        let written = status_block.Information;
        if (status == STATUS_BUFFER_OVERFLOW || (status >= 0 && written == 0))
            && std::mem::size_of_val(buffer.as_slice()) < 131_072
        {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status < 0 {
            return Err(nt_error(status));
        }
        if written == 0 || written > std::mem::size_of_val(buffer.as_slice()) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "Windows directory enumeration returned an invalid buffer length",
            ));
        }
        // SAFETY: the kernel initialized exactly Information bytes, bounded by
        // our allocation above. Parsing below bounds every variable-length name.
        let bytes = unsafe { std::slice::from_raw_parts(buffer.as_ptr().cast::<u8>(), written) };
        append_directory_names(bytes, &mut names, max_entries)?;
        restart = false;
    }
}

fn append_directory_names(
    mut bytes: &[u8],
    names: &mut Vec<OsString>,
    limit: usize,
) -> io::Result<()> {
    const NAME_OFFSET: usize = std::mem::offset_of!(FILE_NAMES_INFORMATION, FileName);
    loop {
        if bytes.len() < NAME_OFFSET {
            return Err(invalid("Windows directory entry header is truncated"));
        }
        let read_u32 = |offset: usize| {
            // SAFETY: the complete fixed-size header was bounded above. Reading
            // unaligned handles any alignment of this borrowed byte slice.
            unsafe { bytes.as_ptr().add(offset).cast::<u32>().read_unaligned() }
        };
        let next = read_u32(std::mem::offset_of!(
            FILE_NAMES_INFORMATION,
            NextEntryOffset
        )) as usize;
        let length =
            read_u32(std::mem::offset_of!(FILE_NAMES_INFORMATION, FileNameLength)) as usize;
        if length == 0 || !length.is_multiple_of(2) || length > bytes.len() - NAME_OFFSET {
            return Err(invalid("Windows directory entry name is truncated"));
        }
        let end = NAME_OFFSET + length;
        if next != 0 && (next < end || next > bytes.len() || !next.is_multiple_of(4)) {
            return Err(invalid("Windows directory entry offset is invalid"));
        }
        let units: Vec<_> = bytes[NAME_OFFSET..end]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|unit| u16::from_ne_bytes(*unit))
            .collect();
        let name = OsString::from_wide(&units);
        if name != "." && name != ".." {
            validate_component(&name)?;
            if names.len() >= limit {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "workspace directory exceeds the cleanup entry limit; reduce its contents and retry",
                ));
            }
            names.push(name);
        }
        if next == 0 {
            return Ok(());
        }
        bytes = &bytes[next..];
    }
}

struct LocalAllocation(*mut std::ffi::c_void);
impl Drop for LocalAllocation {
    fn drop(&mut self) {
        // SAFETY: all instances own a LocalAlloc allocation returned by a Windows security API.
        unsafe {
            LocalFree(self.0);
        }
    }
}

fn current_user() -> io::Result<Vec<usize>> {
    // SAFETY: token and output buffers are initialized and owned for each call.
    unsafe {
        let mut token = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
            return Err(io::Error::last_os_error());
        }
        let token = OwnedHandle::from_raw_handle(token);
        let mut size = 0;
        GetTokenInformation(token.as_raw_handle(), TokenUser, null_mut(), 0, &mut size);
        let mut buffer = vec![0usize; (size as usize).div_ceil(std::mem::size_of::<usize>())];
        if GetTokenInformation(
            token.as_raw_handle(),
            TokenUser,
            buffer.as_mut_ptr().cast(),
            size,
            &mut size,
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(buffer)
    }
}

/// The current process token's user SID as a string (`S-1-5-21-...`).
pub fn current_user_sid_string() -> io::Result<String> {
    let user = current_user()?;
    // SAFETY: user is an aligned TOKEN_USER buffer, SID and converted strings remain live.
    unsafe {
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        let mut sid_text = null_mut();
        if ConvertSidToStringSidW(user.User.Sid, &mut sid_text) == 0 {
            return Err(io::Error::last_os_error());
        }
        let _sid_text = LocalAllocation(sid_text.cast());
        let mut len = 0;
        while *sid_text.add(len) != 0 {
            len += 1;
        }
        Ok(String::from_utf16_lossy(std::slice::from_raw_parts(
            sid_text, len,
        )))
    }
}

fn private_descriptor() -> io::Result<LocalAllocation> {
    let sid = current_user_sid_string()?;
    let sddl = wide(OsStr::new(&format!(
        "O:{sid}D:P(A;OICI;FA;;;{sid})(A;OICI;FA;;;SY)"
    )))?;
    // SAFETY: the SDDL string stays live through the conversion, which returns
    // one owned descriptor.
    unsafe {
        let mut descriptor = null_mut();
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            1,
            &mut descriptor,
            null_mut(),
        ) == 0
        {
            return Err(io::Error::last_os_error());
        }
        Ok(LocalAllocation(descriptor))
    }
}

fn security_attributes(descriptor: &LocalAllocation) -> SECURITY_ATTRIBUTES {
    SECURITY_ATTRIBUTES {
        nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    }
}

pub fn create_private_directory(path: &Path) -> io::Result<()> {
    let descriptor = private_descriptor()?;
    let attrs = security_attributes(&descriptor);
    let path = wide(path.as_os_str())?;
    // SAFETY: both path and descriptor live throughout CreateDirectoryW.
    if unsafe { CreateDirectoryW(path.as_ptr(), &attrs) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

pub fn create_private_file_at(parent: &File, name: &OsStr) -> io::Result<File> {
    create_private_file_at_with_access(parent, name, GENERIC_READ | GENERIC_WRITE | DELETE)
}

pub fn open_private_lock_at(parent: &File, name: &OsStr) -> io::Result<File> {
    match create_private_file_at_with_access(parent, name, GENERIC_READ) {
        Ok(file) => Ok(file),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            let file = open_at(parent, name, false)?;
            validate_private(&file)?;
            Ok(file)
        }
        Err(e) => Err(e),
    }
}

/// Open an owner-protected regular file for bounded in-place journal IO.
/// Existing bytes and permissions are never repaired or truncated on open.
pub fn open_private_rw_at(parent: &File, name: &OsStr) -> io::Result<File> {
    match create_private_file_at(parent, name) {
        Ok(file) => Ok(file),
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let file = open_relative(
                parent,
                name,
                false,
                GENERIC_READ | GENERIC_WRITE,
                FILE_OPEN,
                FILE_SHARE_READ,
                null(),
            )?;
            validate_private(&file)?;
            Ok(file)
        }
        Err(error) => Err(error),
    }
}

fn create_private_file_at_with_access(
    parent: &File,
    name: &OsStr,
    access: u32,
) -> io::Result<File> {
    let descriptor = private_descriptor()?;
    let file = open_relative(
        parent,
        name,
        false,
        access,
        FILE_CREATE,
        FILE_SHARE_READ,
        descriptor.0.cast(),
    )?;
    validate_private(&file)?;
    Ok(file)
}

/// Visit only bounded allow entries while the caller holds the security descriptor.
///
/// # Safety
/// `acl` must be null or point into a live descriptor returned by Windows. The
/// descriptor must stay allocated until this function and its callback return.
unsafe fn for_each_allowed_ace(
    acl: *const ACL,
    mut check: impl FnMut(&ACCESS_ALLOWED_ACE) -> io::Result<()>,
) -> io::Result<()> {
    // SAFETY: the caller holds the descriptor. Each nullable API result is
    // checked before conversion; IsValidAcl bounds its entries within the ACL.
    unsafe {
        let acl = acl
            .as_ref()
            .ok_or_else(|| invalid("Windows ACL is missing"))?;
        if IsValidAcl(acl) == 0 {
            return Err(invalid("Windows ACL is invalid"));
        }
        for index in 0..acl.AceCount {
            let mut ace = null_mut();
            if GetAce(acl, index as u32, &mut ace) == 0 {
                return Err(io::Error::last_os_error());
            }
            let header = ace
                .cast::<ACE_HEADER>()
                .as_ref()
                .ok_or_else(|| invalid("Windows ACL entry is missing"))?;
            // DENY entries grant no access. Other nonstandard ACEs fail closed.
            if header.AceType == 1 {
                continue;
            }
            if header.AceType != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Windows ACL contains an unsupported entry",
                ));
            }
            let size = usize::from(header.AceSize);
            let sid_offset = std::mem::offset_of!(ACCESS_ALLOWED_ACE, SidStart);
            // A SID has an eight-byte header followed by four bytes per
            // subauthority. Bound it before passing it to the SID APIs.
            if size < sid_offset + 8 {
                return Err(invalid("Windows ACL allow entry is truncated"));
            }
            let sid_bytes =
                std::slice::from_raw_parts(ace.cast::<u8>().add(sid_offset), size - sid_offset);
            if 8 + usize::from(sid_bytes[1]) * 4 > sid_bytes.len() {
                return Err(invalid("Windows ACL entry SID is truncated"));
            }
            let allowed = ace
                .cast::<ACCESS_ALLOWED_ACE>()
                .as_ref()
                .ok_or_else(|| invalid("Windows ACL allow entry is missing"))?;
            let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
            if IsValidSid(sid) == 0 {
                return Err(invalid("Windows ACL entry SID is invalid"));
            }
            check(allowed)?;
        }
    }
    Ok(())
}

/// Refuse unprotected, foreign-owned or broadly readable key material. SYSTEM
/// is the sole additional principal allowed by our creation descriptor.
pub fn validate_private(file: &File) -> io::Result<()> {
    let user = current_user()?;
    // SAFETY: the security API returns one owned descriptor; all SID/ACL pointers
    // remain within it until validation completes. GetAce checks each ACE index.
    unsafe {
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        let mut owner = null_mut();
        let mut acl: *mut ACL = null_mut();
        let mut descriptor = null_mut();
        let result = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        );
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let _descriptor = LocalAllocation(descriptor);
        let mut control = 0;
        let mut revision = 0;
        if GetSecurityDescriptorControl(descriptor, &mut control, &mut revision) == 0 {
            return Err(io::Error::last_os_error());
        }
        if control & SE_DACL_PROTECTED == 0 {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture key requires a protected Windows ACL without inherited permissions",
            ));
        }
        if owner.is_null() || EqualSid(owner, user.User.Sid) == 0 || acl.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "capture key must have an owner-only Windows ACL; run libra agent doctor",
            ));
        }
        for_each_allowed_ace(acl, |allowed| {
            let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
            if EqualSid(sid, user.User.Sid) == 0 && IsWellKnownSid(sid, WinLocalSystemSid) == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "capture key grants access to another Windows user; use an owner-only ACL",
                ));
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// LockFileEx locks are released by handle close and process teardown.
pub fn try_lock(file: &File) -> io::Result<bool> {
    // SAFETY: OVERLAPPED is zeroed for byte zero; synchronous disk handle stays live.
    let result = unsafe {
        let mut offset: OVERLAPPED = std::mem::zeroed();
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut offset,
        )
    };
    if result != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.raw_os_error() == Some(ERROR_LOCK_VIOLATION as i32) {
        return Ok(false);
    }
    Err(error)
}

pub fn ensure_directory_not_publicly_writable(path: &Path) -> io::Result<()> {
    use windows_sys::Win32::Security::WinBuiltinAdministratorsSid;
    let file = open_tree(path)?;
    let user = current_user()?;
    // SAFETY: ACL/SID pointers belong to the held descriptor's security buffer.
    unsafe {
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        let mut acl: *mut ACL = null_mut();
        let mut descriptor = null_mut();
        let result = GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            DACL_SECURITY_INFORMATION,
            null_mut(),
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        );
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let _descriptor = LocalAllocation(descriptor);
        if acl.is_null() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "trusted Windows directory has an unrestricted ACL",
            ));
        }
        for_each_allowed_ace(acl, |allowed| {
            let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
            const WRITE_RIGHTS: u32 = 0x4000_0000 | 0x1000_0000 | 0x000D_0046;
            if allowed.Mask & WRITE_RIGHTS != 0
                && EqualSid(sid, user.User.Sid) == 0
                && IsWellKnownSid(sid, WinLocalSystemSid) == 0
                && IsWellKnownSid(sid, WinBuiltinAdministratorsSid) == 0
            {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "trusted Windows directory is writable by another user; move the binary to a protected directory",
                ));
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// PI-02: whether only the current user, SYSTEM, or Administrators can
/// change `directory`'s entries: one of them owns it, its DACL is present, and
/// no allowed ACE that applies to the directory itself gives write rights to
/// any other principal. A security lookup that fails is an error, so callers
/// fail closed.
pub fn directory_entries_private(directory: &File) -> io::Result<bool> {
    use windows_sys::Win32::Security::WinBuiltinAdministratorsSid;
    let user = current_user()?;
    // SAFETY: the owner SID and ACL pointers belong to the held descriptor's
    // security buffer, which outlives every use below.
    unsafe {
        let user = &*user.as_ptr().cast::<TOKEN_USER>();
        let mut owner = null_mut();
        let mut acl: *mut ACL = null_mut();
        let mut descriptor = null_mut();
        let result = GetSecurityInfo(
            directory.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut acl,
            null_mut(),
            &mut descriptor,
        );
        if result != 0 {
            return Err(io::Error::from_raw_os_error(result as i32));
        }
        let _descriptor = LocalAllocation(descriptor);
        let privileged = |sid: *mut core::ffi::c_void| {
            IsWellKnownSid(sid, WinLocalSystemSid) != 0
                || IsWellKnownSid(sid, WinBuiltinAdministratorsSid) != 0
        };
        if owner.is_null() || acl.is_null() {
            return Ok(false);
        }
        if EqualSid(owner, user.User.Sid) == 0 && !privileged(owner) {
            return Ok(false);
        }
        let mut foreign_write = false;
        for_each_allowed_ace(acl, |allowed| {
            // An inherit-only ACE applies to children, not to this directory.
            const INHERIT_ONLY_ACE: u8 = 0x08;
            const WRITE_RIGHTS: u32 = 0x4000_0000 | 0x1000_0000 | 0x000D_0046;
            let sid = std::ptr::addr_of!(allowed.SidStart).cast_mut().cast();
            if allowed.Header.AceFlags & INHERIT_ONLY_ACE == 0
                && allowed.Mask & WRITE_RIGHTS != 0
                && EqualSid(sid, user.User.Sid) == 0
                && !privileged(sid)
            {
                foreign_write = true;
            }
            Ok(())
        })?;
        Ok(!foreign_write)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn acl_iteration_rejects_missing_and_truncated_entries() {
        // SAFETY: null is explicitly rejected before dereference.
        assert!(unsafe { for_each_allowed_ace(null(), |_| Ok(())) }.is_err());

        #[repr(C)]
        struct TruncatedAcl {
            acl: ACL,
            entry: ACE_HEADER,
        }
        let acl = TruncatedAcl {
            acl: ACL {
                AclRevision: 2,
                Sbz1: 0,
                AclSize: std::mem::size_of::<TruncatedAcl>() as u16,
                AceCount: 1,
                Sbz2: 0,
            },
            entry: ACE_HEADER {
                AceType: 0,
                AceFlags: 0,
                AceSize: std::mem::size_of::<ACE_HEADER>() as u16,
            },
        };
        let mut visited = false;
        // SAFETY: the complete ACL and its deliberately short ACE remain live.
        let result = unsafe {
            for_each_allowed_ace(&acl.acl, |_| {
                visited = true;
                Ok(())
            })
        };
        assert!(result.is_err());
        assert!(
            !visited,
            "a truncated ACE must not reach SID or permission checks"
        );
    }

    #[test]
    fn acl_iteration_rejects_sid_length_outside_its_entry() {
        #[repr(C)]
        struct TruncatedSidAcl {
            acl: ACL,
            entry: ACCESS_ALLOWED_ACE,
            authority_tail: u32,
        }
        let acl = TruncatedSidAcl {
            acl: ACL {
                AclRevision: 2,
                Sbz1: 0,
                AclSize: std::mem::size_of::<TruncatedSidAcl>() as u16,
                AceCount: 1,
                Sbz2: 0,
            },
            entry: ACCESS_ALLOWED_ACE {
                Header: ACE_HEADER {
                    AceType: 0,
                    AceFlags: 0,
                    AceSize: (std::mem::size_of::<TruncatedSidAcl>() - std::mem::size_of::<ACL>())
                        as u16,
                },
                Mask: GENERIC_READ,
                // SID revision 1 declares one subauthority, but the entry
                // contains only its eight-byte SID header.
                SidStart: u32::from_le_bytes([1, 1, 0, 0]),
            },
            authority_tail: 0,
        };
        let mut visited = false;
        // SAFETY: the whole ACL allocation stays live throughout the call.
        let result = unsafe {
            for_each_allowed_ace(&acl.acl, |_| {
                visited = true;
                Ok(())
            })
        };
        assert!(result.is_err());
        assert!(
            !visited,
            "an out-of-bounds SID must not reach permission checks"
        );
    }

    #[test]
    fn rejects_traversal_streams_and_windows_device_aliases() {
        for name in [
            "..",
            "a/b",
            "a\\b",
            "key:secret",
            "NUL",
            "con.txt",
            "COM1",
            "key.",
            "key ",
        ] {
            assert!(
                validate_component(OsStr::new(name)).is_err(),
                "accepted {name}"
            );
        }
        assert!(validate_component(OsStr::new("捕获-key.json")).is_ok());
    }

    #[test]
    fn private_acl_and_lock_exclude_concurrent_owners() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_tree(root.path()).unwrap();
        let private = create_directory_at(&parent, OsStr::new("private"), true).unwrap();
        validate_private(&private).unwrap();
        let first = open_private_lock_at(&private, OsStr::new("lock")).unwrap();
        let second = open_private_lock_at(&private, OsStr::new("lock")).unwrap();
        assert!(try_lock(&first).unwrap());
        assert!(!try_lock(&second).unwrap());
        drop(first);
        assert!(try_lock(&second).unwrap());
    }

    #[test]
    fn private_material_rejects_inheritable_acl_even_when_current_grants_are_private() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("private");
        create_private_directory(&path).unwrap();
        std::fs::write(path.join("inherited"), b"secret").unwrap();
        let directory = open_tree(&path).unwrap();
        validate_private(&directory).unwrap();
        let file = open_at(&directory, OsStr::new("inherited"), false).unwrap();
        assert!(
            validate_private(&file)
                .unwrap_err()
                .to_string()
                .contains("protected Windows ACL")
        );
    }

    #[test]
    fn private_publication_never_replaces_an_existing_key() {
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let parent = open_tree(root.path()).unwrap();
        let private = create_directory_at(&parent, OsStr::new("private"), true).unwrap();
        let mut existing = create_private_file_at(&private, OsStr::new("key")).unwrap();
        existing.write_all(b"original").unwrap();
        existing.sync_all().unwrap();
        drop(existing);
        let mut staged = create_private_file_at(&private, OsStr::new("staged")).unwrap();
        staged.write_all(b"replacement").unwrap();
        staged.sync_all().unwrap();
        assert_eq!(
            publish_private_file(&staged, &private, OsStr::new("key"))
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert_eq!(
            std::fs::read(root.path().join("private/key")).unwrap(),
            b"original"
        );
    }

    #[test]
    fn live_source_allows_a_writer_and_retains_identity_after_rotation() {
        use std::io::{Read, Write};
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("session.jsonl");
        let mut writer = std::fs::File::create(&path).unwrap();
        writer.write_all(b"first\n").unwrap();
        let parent = open_tree(root.path()).unwrap();
        let mut reader = open_live_source_at(&parent, OsStr::new("session.jsonl")).unwrap();
        writer.write_all(b"second\n").unwrap();
        drop(writer);
        drop(parent);
        std::fs::rename(&path, root.path().join("old.jsonl")).unwrap();
        std::fs::write(&path, b"replacement\n").unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"first\nsecond\n");
    }

    #[test]
    fn workspace_creation_is_exclusive_and_pins_its_identity() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_tree(root.path()).unwrap();
        let name = OsStr::new("workspace");
        let directory = exclusive_create_mutable_directory_at(&parent, name).unwrap();
        validate_private(&directory).unwrap();
        let expected = identity(&directory).unwrap();
        assert_eq!(
            exclusive_create_mutable_directory_at(&parent, name)
                .unwrap_err()
                .kind(),
            io::ErrorKind::AlreadyExists
        );
        assert!(std::fs::rename(root.path().join(name), root.path().join("swapped")).is_err());
        assert!(remove_entry_at(&parent, name, expected, true).is_err());

        let mut file = create_private_file_at(&directory, OsStr::new("intent.json")).unwrap();
        std::io::Write::write_all(&mut file, b"owned").unwrap();
        file.sync_all().unwrap();
        drop(file);
        let contents = inspect_entry_at(&directory, OsStr::new("intent.json")).unwrap();
        drop(directory);
        assert!(remove_entry_at(&parent, name, expected, true).is_err());
        let directory = open_at(&parent, name, true).unwrap();
        remove_entry_at(
            &directory,
            OsStr::new("intent.json"),
            contents.identity,
            false,
        )
        .unwrap();
        drop(directory);
        remove_entry_at(&parent, name, expected, true).unwrap();
        assert!(read_directory_names(&parent).unwrap().is_empty());
    }

    #[test]
    fn workspace_proof_file_publish_is_atomic_and_no_replace() {
        use std::io::Write;

        let root = tempfile::tempdir().unwrap();
        let parent = open_mutable_tree(root.path()).unwrap();
        let mut staged = create_private_file_at(&parent, OsStr::new("staged.json")).unwrap();
        staged.write_all(b"complete proof").unwrap();
        staged.sync_all().unwrap();
        let staged_identity = identity(&staged).unwrap();
        drop(staged);
        rename_file_at(
            &parent,
            OsStr::new("staged.json"),
            staged_identity,
            &parent,
            OsStr::new("owned.json"),
        )
        .unwrap();
        assert_eq!(
            std::fs::read(root.path().join("owned.json")).unwrap(),
            b"complete proof"
        );

        let mut second = create_private_file_at(&parent, OsStr::new("second.json")).unwrap();
        second.write_all(b"replacement").unwrap();
        let second_identity = identity(&second).unwrap();
        drop(second);
        assert!(
            rename_file_at(
                &parent,
                OsStr::new("second.json"),
                second_identity,
                &parent,
                OsStr::new("owned.json"),
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("owned.json")).unwrap(),
            b"complete proof"
        );
    }

    #[test]
    fn managed_container_creation_is_idempotent_and_refuses_files() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_tree(root.path()).unwrap();
        let first = ensure_directory_at(&parent, OsStr::new("container")).unwrap();
        let second = ensure_directory_at(&parent, OsStr::new("container")).unwrap();
        assert_eq!(identity(&first).unwrap(), identity(&second).unwrap());
        std::fs::write(root.path().join("file"), b"preserved").unwrap();
        assert!(ensure_directory_at(&parent, OsStr::new("file")).is_err());
        assert_eq!(
            std::fs::read(root.path().join("file")).unwrap(),
            b"preserved"
        );
    }

    #[test]
    fn workspace_cleanup_refuses_a_replacement_identity() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_mutable_tree(root.path()).unwrap();
        let name = OsStr::new("owned.txt");
        std::fs::write(root.path().join(name), b"owned").unwrap();
        let original = inspect_entry_at(&parent, name).unwrap();
        std::fs::rename(root.path().join(name), root.path().join("previous.txt")).unwrap();
        std::fs::write(root.path().join(name), b"replacement").unwrap();
        assert_eq!(
            remove_entry_at(&parent, name, original.identity, false)
                .unwrap_err()
                .kind(),
            io::ErrorKind::InvalidData
        );
        assert_eq!(
            std::fs::read(root.path().join(name)).unwrap(),
            b"replacement"
        );
        assert_eq!(
            std::fs::read(root.path().join("previous.txt")).unwrap(),
            b"owned"
        );
    }

    #[test]
    fn worktree_move_checks_source_identity_and_never_replaces_destination() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_mutable_tree(root.path()).unwrap();
        let source = exclusive_create_mutable_directory_at(&parent, OsStr::new("source")).unwrap();
        let source_id = identity(&source).unwrap();
        drop(source);
        std::fs::write(root.path().join("source/sentinel"), b"source").unwrap();
        rename_entry_at(
            &parent,
            OsStr::new("source"),
            source_id,
            &parent,
            OsStr::new("previous"),
        )
        .unwrap();
        let replacement =
            exclusive_create_mutable_directory_at(&parent, OsStr::new("source")).unwrap();
        let replacement_id = identity(&replacement).unwrap();
        drop(replacement);
        assert_eq!(
            rename_entry_at(
                &parent,
                OsStr::new("source"),
                source_id,
                &parent,
                OsStr::new("target")
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::InvalidData
        );
        assert!(
            rename_entry_at(
                &parent,
                OsStr::new("source"),
                replacement_id,
                &parent,
                OsStr::new("previous")
            )
            .is_err()
        );
        assert_eq!(
            std::fs::read(root.path().join("previous/sentinel")).unwrap(),
            b"source"
        );
        rename_entry_at(
            &parent,
            OsStr::new("source"),
            replacement_id,
            &parent,
            OsStr::new("target"),
        )
        .unwrap();
        assert_eq!(
            inspect_entry_at(&parent, OsStr::new("target"))
                .unwrap()
                .identity,
            replacement_id
        );
        assert!(root.path().join("previous/sentinel").exists());
    }

    #[test]
    fn workspace_cleanup_preserves_readonly_outside_hard_link() {
        let root = tempfile::tempdir().unwrap();
        let outside = root.path().join("outside");
        std::fs::write(&outside, b"unchanged").unwrap();
        let parent = open_mutable_tree(root.path()).unwrap();
        let workspace =
            exclusive_create_mutable_directory_at(&parent, OsStr::new("workspace")).unwrap();
        let link = root.path().join("workspace/link");
        std::fs::hard_link(&outside, &link).unwrap();
        let original_permissions = std::fs::metadata(&outside).unwrap().permissions();
        let mut permissions = original_permissions.clone();
        permissions.set_readonly(true);
        std::fs::set_permissions(&outside, permissions).unwrap();
        let entry = inspect_entry_at(&workspace, OsStr::new("link")).unwrap();
        remove_entry_at(&workspace, OsStr::new("link"), entry.identity, false).unwrap();
        assert!(!link.exists());
        assert_eq!(std::fs::read(&outside).unwrap(), b"unchanged");
        assert!(
            std::fs::metadata(&outside)
                .unwrap()
                .permissions()
                .readonly()
        );
        std::fs::set_permissions(&outside, original_permissions).unwrap();
    }

    #[test]
    fn workspace_listing_uses_multiple_batches_and_restarts_the_held_handle() {
        let root = tempfile::tempdir().unwrap();
        let parent = open_tree(root.path()).unwrap();
        let mut expected = Vec::new();
        for index in 0..400 {
            let name = OsString::from(format!("{index:04}-{}-文件", "x".repeat(180)));
            std::fs::write(root.path().join(&name), []).unwrap();
            expected.push(name);
        }
        expected.sort();
        for _ in 0..2 {
            let mut listed = read_directory_names(&parent).unwrap();
            listed.sort();
            assert_eq!(listed, expected);
        }
    }

    #[test]
    fn directory_name_parser_rejects_truncated_and_nonprogressing_records() {
        fn record(next: u32, length: u32, name: &[u16]) -> Vec<u8> {
            let mut bytes = Vec::new();
            bytes.extend_from_slice(&next.to_ne_bytes());
            bytes.extend_from_slice(&0u32.to_ne_bytes());
            bytes.extend_from_slice(&length.to_ne_bytes());
            for unit in name {
                bytes.extend_from_slice(&unit.to_ne_bytes());
            }
            bytes
        }
        for malformed in [
            vec![0; 11],
            record(0, 1, &[97]),
            record(0, 4, &[97]),
            record(4, 2, &[97]),
            record(16, 2, &[97]),
            record(0, 2, &[0]),
        ] {
            assert!(append_directory_names(&malformed, &mut Vec::new(), 10).is_err());
        }
        let mut names = Vec::new();
        append_directory_names(&record(0, 2, &[97]), &mut names, 1).unwrap();
        assert_eq!(names, vec![OsString::from("a")]);
        append_directory_names(&record(0, 2, &[46]), &mut names, 1).unwrap();
        assert_eq!(names, vec![OsString::from("a")]);
        assert!(append_directory_names(&record(0, 2, &[98]), &mut names, 1).is_err());
        assert_eq!(names, vec![OsString::from("a")]);
    }

    #[test]
    fn workspace_cleanup_unlinks_junction_without_visiting_its_target() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        std::fs::write(external.path().join("sentinel"), b"untouched").unwrap();
        let junction = root.path().join("junction");
        let status = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(external.path())
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "create junction fixture");
        let parent = open_tree(root.path()).unwrap();
        assert!(open_at(&parent, OsStr::new("junction"), true).is_err());
        assert!(ensure_directory_at(&parent, OsStr::new("junction")).is_err());
        let entry = inspect_entry_at(&parent, OsStr::new("junction")).unwrap();
        assert!(entry.directory && entry.reparse_point);
        remove_entry_at(&parent, OsStr::new("junction"), entry.identity, true).unwrap();
        assert!(std::fs::symlink_metadata(&junction).is_err());
        assert_eq!(
            std::fs::read(external.path().join("sentinel")).unwrap(),
            b"untouched"
        );
    }

    #[test]
    fn rejects_junction_and_pins_parent_against_rename() {
        let root = tempfile::tempdir().unwrap();
        let external = tempfile::tempdir().unwrap();
        let junction = root.path().join("junction");
        let status = std::process::Command::new("cmd.exe")
            .args(["/C", "mklink", "/J"])
            .arg(&junction)
            .arg(external.path())
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success(), "create junction fixture");
        assert!(open_tree(&junction).is_err());
        assert!(open_mutable_tree(&junction).is_err());
        let parent = open_tree(root.path()).unwrap();
        assert!(open_at(&parent, OsStr::new("junction"), true).is_err());
        assert!(open_mutable_at(&parent, OsStr::new("junction")).is_err());
        std::fs::write(root.path().join("file"), b"ordinary file").unwrap();
        assert!(open_mutable_at(&parent, OsStr::new("file")).is_err());
        assert!(std::fs::rename(root.path(), root.path().with_extension("moved")).is_err());
        assert!(open_at(&parent, OsStr::new("../outside"), false).is_err());
        drop(parent);
        let mutable = open_mutable_tree(root.path()).unwrap();
        assert!(std::fs::rename(root.path(), root.path().with_extension("moved")).is_err());
        drop(mutable);
        std::fs::remove_dir(&junction).unwrap();
    }
}
