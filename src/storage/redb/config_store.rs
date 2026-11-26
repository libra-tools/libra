//! `redb 5.0` implementation of [`ConfigStore`] and [`MetadataStore`] (`RB-03`).
//!
//! All values are persisted using the versioned `[schema_ver: u16_le][postcard]`
//! envelope (`crate::storage::codec`). Every read and write transaction validates
//! `META_SCHEMA` (role, reader version, and `u32::MAX` migration tombstone fence)
//! as its first step (`ADR-RB-03` #3). Read-only handles open via
//! [`open_redb_read_only`] (`16 MiB` cache, `MultiWriter`) and never upgrade to
//! writable (`ADR-RB-03` #2).

use std::{
    fs::File,
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use chrono::Utc;
use redb::{
    Builder, ConcurrencyMode, Database, ReadOnlyDatabase, ReadTransaction, ReadableTable,
    TableDefinition, TableHandle, WriteTransaction, backends::FileBackend,
};
use serde::{Deserialize, Serialize};

use crate::{
    internal::db::schema::{DatabaseRole, migrations_for_role},
    storage::{
        backend::StorageBackendKind,
        codec::{decode_versioned, encode_versioned},
        config_store::{
            ConfigEntry, ConfigStore, LegacyConfigEntry, MetadataCasOutcome, MetadataEntryRecord,
            MetadataStore, SchemaVersionReceipt, config_redb_path_in_scope_dir, key_in_section,
            validate_config_role, validate_metadata_role, validate_section_name,
        },
        error::{StorageBackendError, StorageResult},
        meta::{begin_checked_read, begin_checked_write_blocking, init_meta_schema},
        opener::{
            CACHE_SIZE_STANDARD_WRITE, RedbRepairContext, begin_write_bounded_blocking,
            open_redb_read_only, open_redb_writable,
        },
        redb::{
            map_redb_commit_err, map_redb_database_err, map_redb_storage_err, map_redb_table_err,
        },
    },
};

/// Current postcard codec version for `config_kv` rows.
pub const CONFIG_KV_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `config_kv_by_key` secondary index entries.
pub const CONFIG_KV_INDEX_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for legacy `config` rows.
pub const CONFIG_LEGACY_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for `metadata_kv` rows.
pub const METADATA_KV_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for schema ledger receipts.
pub const SCHEMA_RECEIPT_CODEC_VERSION: u16 = 1;
/// Current postcard codec version for monotonic sequence counter entries.
pub const CONFIG_SEQ_CODEC_VERSION: u16 = 1;

/// Default bounded write-lock acquisition timeout for `RedbConfigStore`.
pub const DEFAULT_CONFIG_WRITE_TIMEOUT: Duration = Duration::from_secs(30);

const CONFIG_KV_TABLE_NAME: &str = "config_kv";
const CONFIG_KV_BY_KEY_INDEX_NAME: &str = "config_kv_by_key";
const CONFIG_LEGACY_TABLE_NAME: &str = "config";
const METADATA_KV_TABLE_NAME: &str = "metadata_kv";
const SCHEMA_VERSIONS_TABLE_NAME: &str = "schema_versions";
const CONFIG_SCHEMA_VERSIONS_TABLE_NAME: &str = "configuration_schema_versions";
const CONFIG_SEQ_TABLE_NAME: &str = "config_sequences";

/// Monotonic primary table for `config_kv` (`id -> versioned ConfigKvStoredRow`).
pub const CONFIG_KV_TABLE: TableDefinition<'static, u64, &[u8]> =
    TableDefinition::new(CONFIG_KV_TABLE_NAME);

/// Same-transaction secondary index for `config_kv`
/// (`(key, id) -> versioned ConfigKvIndexMarker`).
pub const CONFIG_KV_BY_KEY_INDEX: TableDefinition<'static, (&str, u64), &[u8]> =
    TableDefinition::new(CONFIG_KV_BY_KEY_INDEX_NAME);

/// Monotonic primary table for legacy 3-column `config` rows (`id -> versioned LegacyConfigEntry`).
pub const CONFIG_LEGACY_TABLE: TableDefinition<'static, u64, &[u8]> =
    TableDefinition::new(CONFIG_LEGACY_TABLE_NAME);

/// Primary table for `metadata_kv` (`(scope, target, key) -> versioned MetadataStoredRow`).
pub const METADATA_KV_TABLE: TableDefinition<'static, (&str, &str, &str), &[u8]> =
    TableDefinition::new(METADATA_KV_TABLE_NAME);

/// Repository schema ledger table (`version -> versioned SchemaReceiptStoredRow`).
pub const SCHEMA_VERSIONS_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new(SCHEMA_VERSIONS_TABLE_NAME);

/// Global/System configuration schema ledger table (`version -> versioned SchemaReceiptStoredRow`).
pub const CONFIG_SCHEMA_VERSIONS_TABLE: TableDefinition<'static, i64, &[u8]> =
    TableDefinition::new(CONFIG_SCHEMA_VERSIONS_TABLE_NAME);

/// Monotonic sequence counters (`table_name -> versioned ConfigSequenceValue`).
pub const CONFIG_SEQ_TABLE: TableDefinition<'static, &str, &[u8]> =
    TableDefinition::new(CONFIG_SEQ_TABLE_NAME);

const SEQ_KEY_CONFIG_KV: &str = "config_kv";
const SEQ_KEY_CONFIG_LEGACY: &str = "config";

#[cfg(unix)]
mod unix_create {
    use std::{
        ffi::CStr,
        fs::{self, File, OpenOptions},
        io,
        os::unix::{
            fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
            io::{AsRawFd, FromRawFd},
        },
        path::Path,
    };

    use crate::storage::error::{StorageBackendError, StorageResult};

    #[cfg(target_vendor = "apple")]
    unsafe extern "C" {
        fn fclonefileat(
            srcfd: libc::c_int,
            dst_dirfd: libc::c_int,
            dst: *const libc::c_char,
            flags: u32,
        ) -> libc::c_int;
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct FileIdentity {
        pub dev: u64,
        pub ino: u64,
    }

    pub(super) fn open_parent_dir(parent: &Path) -> StorageResult<(File, FileIdentity)> {
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|err| StorageBackendError::io("open parent dir for redb config store", err))?;
        let meta = dir.metadata().map_err(|err| {
            StorageBackendError::io("fstat parent dir for redb config store", err)
        })?;
        Ok((
            dir,
            FileIdentity {
                dev: meta.dev(),
                ino: meta.ino(),
            },
        ))
    }

    pub(super) fn ensure_target_absent(parent_fd: &File, target_cname: &CStr) -> StorageResult<()> {
        let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `parent_fd` is a valid directory fd, `target_cname` is a valid C string, `st` is writable storage.
        let rc = unsafe {
            libc::fstatat(
                parent_fd.as_raw_fd(),
                target_cname.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc == 0 {
            return Err(StorageBackendError::Io {
                context: "create redb config store",
                kind: io::ErrorKind::AlreadyExists,
                raw_os_error: None,
            });
        }
        let err = io::Error::last_os_error();
        if err.kind() == io::ErrorKind::NotFound {
            Ok(())
        } else {
            Err(StorageBackendError::io(
                "fstatat redb config store path",
                err,
            ))
        }
    }

    /// Create an anonymous (unlinked / `O_TMPFILE`) inode on the same filesystem
    /// as `parent_fd` with exact role permissions applied via `fchmod(fd)`.
    ///
    /// Because no directory entry exists for the returned file descriptor,
    /// neither other users nor same-UID processes can observe or substitute the
    /// staged file before publication, and any failure or crash prior to
    /// publication leaves zero residue at `target` or in `parent`.
    #[cfg(target_os = "linux")]
    pub(super) fn create_anonymous_staged_file(
        parent_fd: &File,
        _file_name: &str,
        target_mode: u32,
    ) -> StorageResult<(File, FileIdentity)> {
        // SAFETY: `parent_fd` is a valid directory fd; `O_TMPFILE` without `O_EXCL`
        // creates an unnamed inode that can be materialized once via `/proc/self/fd`.
        let fd = unsafe {
            libc::openat(
                parent_fd.as_raw_fd(),
                c".".as_ptr(),
                libc::O_RDWR | libc::O_TMPFILE | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        };
        if fd < 0 {
            return Err(StorageBackendError::io(
                "create anonymous staged redb config store",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fd` is a newly created, exclusively owned file descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        file.set_permissions(fs::Permissions::from_mode(target_mode))
            .map_err(|err| StorageBackendError::io("fchmod anonymous staged redb store", err))?;
        let meta = file
            .metadata()
            .map_err(|err| StorageBackendError::io("fstat anonymous staged redb store", err))?;
        Ok((
            file,
            FileIdentity {
                dev: meta.dev(),
                ino: meta.ino(),
            },
        ))
    }

    #[cfg(target_vendor = "apple")]
    pub(super) fn create_anonymous_staged_file(
        parent_fd: &File,
        file_name: &str,
        target_mode: u32,
    ) -> StorageResult<(File, FileIdentity)> {
        use std::{
            ffi::CString,
            hash::{BuildHasher, RandomState},
            sync::atomic::{AtomicU64, Ordering},
        };

        const MAX_ATTEMPTS: usize = 64;
        static ANON_SEQ: AtomicU64 = AtomicU64::new(0);

        for _ in 0..MAX_ATTEMPTS {
            let seq = ANON_SEQ.fetch_add(1, Ordering::Relaxed);
            let rand_a = RandomState::new().hash_one((std::process::id(), seq));
            let rand_b = RandomState::new().hash_one((seq, "rb03-anon"));
            let name = format!(".{file_name}.anon.{rand_a:016x}{rand_b:016x}.tmp");
            let c_name =
                CString::new(name.as_str()).map_err(|_| StorageBackendError::BackendInternal {
                    detail: "redb anonymous staging filename contains NUL byte".to_string(),
                })?;
            // SAFETY: `parent_fd` is a valid directory fd, `c_name` is a valid C string,
            // and `0o600 as libc::c_uint` obeys C variadic integer promotion on macOS.
            let fd = unsafe {
                libc::openat(
                    parent_fd.as_raw_fd(),
                    c_name.as_ptr(),
                    libc::O_RDWR
                        | libc::O_CREAT
                        | libc::O_EXCL
                        | libc::O_NOFOLLOW
                        | libc::O_CLOEXEC,
                    0o600 as libc::c_uint,
                )
            };
            if fd < 0 {
                let err = io::Error::last_os_error();
                if err.kind() == io::ErrorKind::AlreadyExists {
                    continue;
                }
                return Err(StorageBackendError::io(
                    "create anonymous staged redb config store",
                    err,
                ));
            }
            // SAFETY: `fd` is a newly created, exclusively owned file descriptor.
            let file = unsafe { File::from_raw_fd(fd) };
            // Immediately unlink the temporary directory entry while holding `file`
            // open so the inode becomes strictly anonymous before any bytes are written.
            // SAFETY: `parent_fd` and `c_name` are valid; best-effort immediate unlink
            // of the entry we just created via `O_CREAT | O_EXCL`.
            let unlink_rc = unsafe { libc::unlinkat(parent_fd.as_raw_fd(), c_name.as_ptr(), 0) };
            if unlink_rc != 0 {
                return Err(StorageBackendError::io(
                    "unlink anonymous staged redb config entry",
                    io::Error::last_os_error(),
                ));
            }
            file.set_permissions(fs::Permissions::from_mode(target_mode))
                .map_err(|err| {
                    StorageBackendError::io("fchmod anonymous staged redb store", err)
                })?;
            let meta = file
                .metadata()
                .map_err(|err| StorageBackendError::io("fstat anonymous staged redb store", err))?;
            return Ok((
                file,
                FileIdentity {
                    dev: meta.dev(),
                    ino: meta.ino(),
                },
            ));
        }

        Err(StorageBackendError::BackendInternal {
            detail: "exhausted anonymous staging attempts while creating redb config store"
                .to_string(),
        })
    }

    #[cfg(target_os = "linux")]
    pub(super) fn publish_anonymous_fd(
        raw_fd: libc::c_int,
        parent_fd: &File,
        target_cname: &CStr,
    ) -> StorageResult<()> {
        use std::ffi::CString;

        let proc_path = CString::new(format!("/proc/self/fd/{raw_fd}")).map_err(|_| {
            StorageBackendError::BackendInternal {
                detail: "procfs fd path contains NUL byte".to_string(),
            }
        })?;
        // SAFETY: `raw_fd` belongs to the live `db` FileBackend; `AT_SYMLINK_FOLLOW`
        // follows `/proc/self/fd/<N>` to the anonymous inode and atomically links it
        // into `parent_fd` without overwriting (`EEXIST -> AlreadyExists`).
        let rc = unsafe {
            libc::linkat(
                libc::AT_FDCWD,
                proc_path.as_ptr(),
                parent_fd.as_raw_fd(),
                target_cname.as_ptr(),
                libc::AT_SYMLINK_FOLLOW,
            )
        };
        if rc != 0 {
            return Err(StorageBackendError::io(
                "create redb config store",
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    #[cfg(target_vendor = "apple")]
    pub(super) fn publish_anonymous_fd_clone(
        raw_fd: libc::c_int,
        parent_fd: &File,
        target_cname: &CStr,
    ) -> StorageResult<()> {
        // SAFETY: `raw_fd` is a valid open file descriptor flushed by `wtx.commit()`,
        // `parent_fd` is a valid directory fd, and `target_cname` is a valid C string;
        // `fclonefileat` atomically publishes a non-overwriting clone (`EEXIST` if present)
        // directly from `raw_fd` with zero source pathname lookup.
        let rc = unsafe { fclonefileat(raw_fd, parent_fd.as_raw_fd(), target_cname.as_ptr(), 0) };
        if rc != 0 {
            return Err(StorageBackendError::io(
                "create redb config store",
                io::Error::last_os_error(),
            ));
        }
        Ok(())
    }

    #[cfg(target_vendor = "apple")]
    pub(super) fn open_published_regular(
        parent_fd: &File,
        target_cname: &CStr,
        expected_mode: u32,
    ) -> StorageResult<(File, FileIdentity)> {
        // SAFETY: `parent_fd` is a valid directory fd and `target_cname` is a valid C string;
        // `O_NOFOLLOW` guarantees a symlink fails closed with `ELOOP`.
        let fd = unsafe {
            libc::openat(
                parent_fd.as_raw_fd(),
                target_cname.as_ptr(),
                libc::O_RDWR | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(StorageBackendError::io(
                "open published redb config store",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fd` is a newly opened, exclusively owned file descriptor.
        let file = unsafe { File::from_raw_fd(fd) };
        let meta = file
            .metadata()
            .map_err(|err| StorageBackendError::io("fstat published redb config store", err))?;
        // SAFETY: `geteuid` has no preconditions and always succeeds.
        let euid = unsafe { libc::geteuid() };
        if !meta.file_type().is_file()
            || meta.uid() != euid
            || (meta.permissions().mode() & 0o777) != expected_mode
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "published redb config store failed ownership/mode verification"
                    .to_string(),
            });
        }
        Ok((
            file,
            FileIdentity {
                dev: meta.dev(),
                ino: meta.ino(),
            },
        ))
    }

    pub(super) fn verify_requested_path_identity(
        parent: &Path,
        parent_fd: &File,
        requested_path: &Path,
        target_cname: &CStr,
        expected_parent: FileIdentity,
        expected_file: FileIdentity,
    ) -> StorageResult<()> {
        // 1. Verify `target_cname` inside our pinned `parent_fd` still refers
        //    to our continuously-open inode (not unlinked or swapped).
        let mut st = std::mem::MaybeUninit::<libc::stat>::zeroed();
        // SAFETY: `parent_fd` is a valid directory fd, `target_cname` is a valid C string, `st` is writable storage.
        let rc = unsafe {
            libc::fstatat(
                parent_fd.as_raw_fd(),
                target_cname.as_ptr(),
                st.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        };
        if rc != 0 {
            return Err(StorageBackendError::io(
                "verify created redb config store identity",
                io::Error::last_os_error(),
            ));
        }
        // SAFETY: `fstatat` succeeded and initialized `st`.
        let st = unsafe { st.assume_init() };
        let is_regular = (st.st_mode & libc::S_IFMT) == libc::S_IFREG;
        #[allow(clippy::unnecessary_cast)]
        let dev = st.st_dev as u64;
        #[allow(clippy::unnecessary_cast)]
        let ino = st.st_ino as u64;
        if !is_regular || dev != expected_file.dev || ino != expected_file.ino {
            return Err(StorageBackendError::BackendInternal {
                detail: "created redb config store was replaced before create returned".to_string(),
            });
        }

        // 2. Verify the requested parent pathname still resolves to the pinned
        //    parent directory inode (rejecting parent rename/replacement).
        let parent_meta = fs::metadata(parent).map_err(|err| {
            StorageBackendError::io("verify parent dir for redb config store", err)
        })?;
        if !parent_meta.is_dir()
            || parent_meta.dev() != expected_parent.dev
            || parent_meta.ino() != expected_parent.ino
        {
            return Err(StorageBackendError::BackendInternal {
                detail: "parent directory for redb config store was replaced during create"
                    .to_string(),
            });
        }

        // 3. Verify the requested target pathname itself (without following a
        //    final symlink) still resolves to our continuously-open inode.
        let path_meta = fs::symlink_metadata(requested_path).map_err(|err| {
            StorageBackendError::io("verify requested redb config store path", err)
        })?;
        if !path_meta.file_type().is_file()
            || path_meta.dev() != expected_file.dev
            || path_meta.ino() != expected_file.ino
        {
            return Err(StorageBackendError::BackendInternal {
                detail:
                    "requested redb config store path no longer resolves to the created database"
                        .to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(windows)]
mod windows_create {
    use std::{
        ffi::{OsStr, c_void},
        fs::{File, OpenOptions},
        hash::{BuildHasher, RandomState},
        io,
        mem::{offset_of, size_of},
        os::windows::{
            ffi::OsStrExt,
            fs::OpenOptionsExt,
            io::{AsRawHandle, FromRawHandle},
        },
        path::Path,
        ptr,
        sync::atomic::{AtomicU64, Ordering},
    };

    use windows_sys::{
        Wdk::{
            Foundation::OBJECT_ATTRIBUTES,
            Storage::FileSystem::{
                FILE_CREATE, FILE_DISPOSITION_INFORMATION, FILE_NON_DIRECTORY_FILE, FILE_OPEN,
                FILE_OPEN_REPARSE_POINT, FILE_RENAME_INFORMATION, FILE_RENAME_INFORMATION_0,
                FILE_SYNCHRONOUS_IO_NONALERT, FileDispositionInformation, FileRenameInformation,
                NtCreateFile, NtSetInformationFile,
            },
        },
        Win32::{
            Foundation::{
                GENERIC_READ, GENERIC_WRITE, HANDLE, LocalFree, NTSTATUS, OBJ_CASE_INSENSITIVE,
                RtlNtStatusToDosError, STATUS_OBJECT_NAME_COLLISION, STATUS_OBJECT_NAME_NOT_FOUND,
                UNICODE_STRING,
            },
            Security::{
                Authorization::ConvertStringSecurityDescriptorToSecurityDescriptorW,
                SECURITY_DESCRIPTOR,
            },
            Storage::FileSystem::{
                BY_HANDLE_FILE_INFORMATION, DELETE, FILE_ATTRIBUTE_NORMAL,
                FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS,
                FILE_FLAG_OPEN_REPARSE_POINT, FILE_READ_ATTRIBUTES, FILE_SHARE_DELETE,
                FILE_SHARE_READ, FILE_SHARE_WRITE, GetFileInformationByHandle, SYNCHRONIZE,
            },
            System::IO::IO_STATUS_BLOCK,
        },
    };

    use crate::{
        internal::db::schema::DatabaseRole,
        storage::error::{StorageBackendError, StorageResult},
        utils::windows_fs,
    };

    const MAX_STAGE_ATTEMPTS: usize = 64;
    static STAGE_SEQ: AtomicU64 = AtomicU64::new(0);

    struct OwnedSecurityDescriptor(*mut c_void);

    impl OwnedSecurityDescriptor {
        fn for_role(role: DatabaseRole) -> StorageResult<Self> {
            let sid = windows_fs::current_user_sid_string().map_err(|err| {
                StorageBackendError::io("resolve current user SID for redb config ACL", err)
            })?;
            // `D:P` sets `SE_DACL_PROTECTED`, blocking permissive inherited ACLs
            // from the parent directory during staging and after publication.
            let sddl = match role {
                DatabaseRole::SystemConfig => {
                    format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FA;;;SY)(A;;FR;;;AU)(A;;FR;;;BU)")
                }
                DatabaseRole::Repository
                | DatabaseRole::GlobalConfig
                | DatabaseRole::Capture
                | DatabaseRole::Derived => format!("O:{sid}D:P(A;;FA;;;{sid})(A;;FA;;;SY)"),
            };
            let wide_sddl: Vec<u16> = OsStr::new(&sddl).encode_wide().chain(Some(0)).collect();
            let mut raw_sd: *mut c_void = ptr::null_mut();
            // SAFETY: `wide_sddl` is NUL-terminated UTF-16 and lives across the FFI call;
            // on success Windows allocates `raw_sd` via LocalAlloc and we free it in `Drop`.
            let ok = unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    wide_sddl.as_ptr(),
                    1,
                    &mut raw_sd,
                    ptr::null_mut(),
                )
            };
            if ok == 0 || raw_sd.is_null() {
                return Err(StorageBackendError::io(
                    "build protected security descriptor for redb config store",
                    io::Error::last_os_error(),
                ));
            }
            Ok(Self(raw_sd))
        }

        fn as_ptr(&self) -> *const SECURITY_DESCRIPTOR {
            self.0.cast()
        }
    }

    impl Drop for OwnedSecurityDescriptor {
        fn drop(&mut self) {
            if !self.0.is_null() {
                // SAFETY: `self.0` was allocated by `ConvertStringSecurityDescriptorToSecurityDescriptorW`.
                unsafe {
                    LocalFree(self.0);
                }
                self.0 = ptr::null_mut();
            }
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub(super) struct FileIdentity {
        volume_serial: u32,
        index_high: u32,
        index_low: u32,
    }

    fn ntstatus_to_io_error(status: NTSTATUS) -> io::Error {
        if status == STATUS_OBJECT_NAME_COLLISION {
            return io::Error::new(io::ErrorKind::AlreadyExists, "object name collision");
        }
        if status == STATUS_OBJECT_NAME_NOT_FOUND {
            return io::Error::new(io::ErrorKind::NotFound, "object name not found");
        }
        // SAFETY: `RtlNtStatusToDosError` converts any NTSTATUS code into a Win32 error code.
        let win32 = unsafe { RtlNtStatusToDosError(status) };
        io::Error::from_raw_os_error(win32 as i32)
    }

    fn encode_relative_utf16(name: &str) -> StorageResult<(Vec<u16>, u16)> {
        let wide: Vec<u16> = OsStr::new(name).encode_wide().collect();
        let byte_len = wide
            .len()
            .checked_mul(2)
            .and_then(|n| u16::try_from(n).ok())
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "redb config filename exceeds Windows UNICODE_STRING limit".to_string(),
            })?;
        Ok((wide, byte_len))
    }

    fn file_identity(file: &File) -> StorageResult<(FileIdentity, u32)> {
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: `file` is a valid open Windows file handle and `info` is valid writable storage.
        let ok = unsafe { GetFileInformationByHandle(file.as_raw_handle() as HANDLE, &mut info) };
        if ok == 0 {
            return Err(StorageBackendError::io(
                "query redb config store handle identity",
                io::Error::last_os_error(),
            ));
        }
        Ok((
            FileIdentity {
                volume_serial: info.dwVolumeSerialNumber,
                index_high: info.nFileIndexHigh,
                index_low: info.nFileIndexLow,
            },
            info.dwFileAttributes,
        ))
    }

    pub(super) fn open_parent_dir(parent: &Path) -> StorageResult<(File, FileIdentity)> {
        let dir = OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS)
            .open(parent)
            .map_err(|err| StorageBackendError::io("open parent dir for redb config store", err))?;
        let (id, _) = file_identity(&dir)?;
        Ok((dir, id))
    }

    fn nt_open_relative(
        parent_dir: &File,
        name: &str,
        desired_access: u32,
        disposition: u32,
        create_options: u32,
        security_descriptor: *const SECURITY_DESCRIPTOR,
    ) -> Result<File, NTSTATUS> {
        let (mut wide, byte_len) = encode_relative_utf16(name).map_err(|_| -1i32)?;
        let unicode = UNICODE_STRING {
            Length: byte_len,
            MaximumLength: byte_len,
            Buffer: wide.as_mut_ptr(),
        };
        let attrs = OBJECT_ATTRIBUTES {
            Length: size_of::<OBJECT_ATTRIBUTES>() as u32,
            RootDirectory: parent_dir.as_raw_handle() as HANDLE,
            ObjectName: &unicode,
            Attributes: OBJ_CASE_INSENSITIVE,
            SecurityDescriptor: security_descriptor,
            SecurityQualityOfService: ptr::null(),
        };
        let mut iosb = IO_STATUS_BLOCK::default();
        let mut raw_handle: HANDLE = ptr::null_mut();
        // SAFETY: `parent_dir` is a valid directory handle; `unicode`, `attrs`, `iosb`, `raw_handle`,
        // and any non-null `security_descriptor` live for the entire `NtCreateFile` call.
        let status = unsafe {
            NtCreateFile(
                &mut raw_handle,
                desired_access,
                &attrs,
                &mut iosb,
                ptr::null(),
                FILE_ATTRIBUTE_NORMAL,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                disposition,
                create_options,
                ptr::null(),
                0,
            )
        };
        if status < 0 {
            return Err(status);
        }
        // SAFETY: `raw_handle` is a newly created, exclusively owned kernel file handle.
        Ok(unsafe { File::from_raw_handle(raw_handle as _) })
    }

    pub(super) fn ensure_target_absent(parent_dir: &File, file_name: &str) -> StorageResult<()> {
        match nt_open_relative(
            parent_dir,
            file_name,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
        ) {
            Ok(existing) => {
                drop(existing);
                Err(StorageBackendError::Io {
                    context: "create redb config store",
                    kind: io::ErrorKind::AlreadyExists,
                    raw_os_error: None,
                })
            }
            Err(STATUS_OBJECT_NAME_NOT_FOUND) => Ok(()),
            Err(status) => Err(StorageBackendError::io(
                "probe redb config store path via parent handle",
                ntstatus_to_io_error(status),
            )),
        }
    }

    pub(super) struct PinnedStagedFile {
        file: File,
        published: bool,
    }

    impl PinnedStagedFile {
        pub(super) fn create_in(
            parent_dir: &File,
            file_name: &str,
            role: DatabaseRole,
        ) -> StorageResult<(Self, FileIdentity)> {
            let sd = OwnedSecurityDescriptor::for_role(role)?;
            for _ in 0..MAX_STAGE_ATTEMPTS {
                let seq = STAGE_SEQ.fetch_add(1, Ordering::Relaxed);
                let rand_a = RandomState::new().hash_one((std::process::id(), seq));
                let rand_b = RandomState::new().hash_one((seq, "rb03-win-stage"));
                let stage_name = format!(".{file_name}.stage.{rand_a:016x}{rand_b:016x}.tmp");
                match nt_open_relative(
                    parent_dir,
                    &stage_name,
                    GENERIC_READ | GENERIC_WRITE | DELETE | SYNCHRONIZE,
                    FILE_CREATE,
                    FILE_NON_DIRECTORY_FILE
                        | FILE_SYNCHRONOUS_IO_NONALERT
                        | FILE_OPEN_REPARSE_POINT,
                    sd.as_ptr(),
                ) {
                    Ok(file) => {
                        // Arm handle-bound `FileDispositionInformation` cleanup
                        // IMMEDIATELY upon creation so any subsequent ACL
                        // validation or identity query error also cleans up the
                        // staging file via handle on drop.
                        let staged = Self {
                            file,
                            published: false,
                        };
                        if role != DatabaseRole::SystemConfig {
                            windows_fs::validate_private(&staged.file).map_err(|err| {
                                StorageBackendError::io(
                                    "verify private Windows ACL on staged redb config store",
                                    err,
                                )
                            })?;
                        }
                        let (identity, _) = file_identity(&staged.file)?;
                        return Ok((staged, identity));
                    }
                    Err(STATUS_OBJECT_NAME_COLLISION) => continue,
                    Err(status) => {
                        return Err(StorageBackendError::io(
                            "create staged redb config file",
                            ntstatus_to_io_error(status),
                        ));
                    }
                }
            }
            Err(StorageBackendError::BackendInternal {
                detail: "exhausted staging filename attempts while creating redb config store"
                    .to_string(),
            })
        }

        pub(super) fn clone_for_db(&self) -> StorageResult<File> {
            self.file
                .try_clone()
                .map_err(|err| StorageBackendError::io("duplicate staged redb file handle", err))
        }

        pub(super) fn publish_into_parent(
            &mut self,
            parent_dir: &File,
            target_file_name: &str,
        ) -> StorageResult<()> {
            let (wide, byte_len_u16) = encode_relative_utf16(target_file_name)?;
            let file_name_bytes = u32::from(byte_len_u16);
            let header_offset = offset_of!(FILE_RENAME_INFORMATION, FileName);
            let total_bytes = header_offset
                .checked_add(file_name_bytes as usize)
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "FILE_RENAME_INFORMATION buffer size overflow".to_string(),
                })?;
            let total_bytes_u32 =
                u32::try_from(total_bytes).map_err(|_| StorageBackendError::BackendInternal {
                    detail: "FILE_RENAME_INFORMATION buffer exceeds u32 limit".to_string(),
                })?;

            let word_count = total_bytes.div_ceil(size_of::<usize>());
            let mut aligned = vec![0usize; word_count];
            let base_ptr = aligned.as_mut_ptr().cast::<u8>();
            let mut iosb = IO_STATUS_BLOCK::default();
            // SAFETY: `aligned` is `usize`-aligned (sufficient for `FILE_RENAME_INFORMATION`), sized to
            // hold the header plus `wide`, and both `self.file` and `parent_dir` are valid handles.
            let status = unsafe {
                let info_ptr = base_ptr.cast::<FILE_RENAME_INFORMATION>();
                ptr::write(
                    info_ptr,
                    FILE_RENAME_INFORMATION {
                        Anonymous: FILE_RENAME_INFORMATION_0 {
                            ReplaceIfExists: false,
                        },
                        RootDirectory: parent_dir.as_raw_handle() as HANDLE,
                        FileNameLength: file_name_bytes,
                        FileName: [0],
                    },
                );
                let name_dst = base_ptr.add(header_offset).cast::<u16>();
                ptr::copy_nonoverlapping(wide.as_ptr(), name_dst, wide.len());

                NtSetInformationFile(
                    self.file.as_raw_handle() as HANDLE,
                    &mut iosb,
                    base_ptr.cast(),
                    total_bytes_u32,
                    FileRenameInformation,
                )
            };
            if status < 0 {
                return Err(StorageBackendError::io(
                    "create redb config store",
                    ntstatus_to_io_error(status),
                ));
            }
            self.published = true;
            Ok(())
        }
    }

    impl Drop for PinnedStagedFile {
        fn drop(&mut self) {
            if !self.published {
                let disp = FILE_DISPOSITION_INFORMATION { DeleteFile: true };
                let mut iosb = IO_STATUS_BLOCK::default();
                // SAFETY: Best-effort handle-bound deletion of our unpublished staging file.
                unsafe {
                    NtSetInformationFile(
                        self.file.as_raw_handle() as HANDLE,
                        &mut iosb,
                        (&disp as *const FILE_DISPOSITION_INFORMATION).cast(),
                        size_of::<FILE_DISPOSITION_INFORMATION>() as u32,
                        FileDispositionInformation,
                    );
                }
            }
        }
    }

    pub(super) fn verify_requested_path_identity(
        parent: &Path,
        parent_dir: &File,
        requested_path: &Path,
        file_name: &str,
        expected_parent: FileIdentity,
        expected_file: FileIdentity,
    ) -> StorageResult<()> {
        let rel = nt_open_relative(
            parent_dir,
            file_name,
            FILE_READ_ATTRIBUTES | SYNCHRONIZE,
            FILE_OPEN,
            FILE_NON_DIRECTORY_FILE | FILE_OPEN_REPARSE_POINT | FILE_SYNCHRONOUS_IO_NONALERT,
            ptr::null(),
        )
        .map_err(|status| {
            StorageBackendError::io(
                "verify created redb config store identity",
                ntstatus_to_io_error(status),
            )
        })?;
        let (rel_id, rel_attrs) = file_identity(&rel)?;
        if (rel_attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0 || rel_id != expected_file {
            return Err(StorageBackendError::BackendInternal {
                detail: "created redb config store was replaced before create returned".to_string(),
            });
        }

        let (_, cur_parent_id) = open_parent_dir(parent)?;
        if cur_parent_id != expected_parent {
            return Err(StorageBackendError::BackendInternal {
                detail: "parent directory for redb config store was replaced during create"
                    .to_string(),
            });
        }

        let direct = OpenOptions::new()
            .access_mode(FILE_READ_ATTRIBUTES)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_OPEN_REPARSE_POINT)
            .open(requested_path)
            .map_err(|err| {
                StorageBackendError::io("verify requested redb config store path", err)
            })?;
        let (direct_id, direct_attrs) = file_identity(&direct)?;
        if (direct_attrs & FILE_ATTRIBUTE_REPARSE_POINT) != 0 || direct_id != expected_file {
            return Err(StorageBackendError::BackendInternal {
                detail:
                    "requested redb config store path no longer resolves to the created database"
                        .to_string(),
            });
        }
        Ok(())
    }
}
fn build_standard_write_builder() -> Builder {
    let mut builder = Builder::new();
    builder
        .set_concurrency_mode(ConcurrencyMode::MultiWriter)
        .set_cache_size(CACHE_SIZE_STANDARD_WRITE);
    builder
}

fn create_database_from_file(file: File, role: DatabaseRole) -> StorageResult<Database> {
    build_standard_write_builder()
        .create_with_backend(FileBackend::new(file))
        .map_err(|err| map_redb_database_err(err, role, "create redb config store from file"))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ConfigKvStoredRow {
    key: String,
    value: String,
    encrypted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ConfigKvIndexMarker {}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct ConfigSequenceValue {
    last_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MetadataStoredRow {
    value: String,
    value_type: String,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SchemaReceiptStoredRow {
    name: String,
    applied_at: String,
}

#[derive(Clone)]
enum RedbHandle {
    ReadWrite(Arc<Database>),
    ReadOnly(Arc<ReadOnlyDatabase>),
}

/// `redb 5.0`-backed implementation of [`ConfigStore`] (and [`MetadataStore`]
/// for [`DatabaseRole::Repository`]).
#[derive(Clone)]
pub struct RedbConfigStore {
    handle: RedbHandle,
    role: DatabaseRole,
    write_timeout: Duration,
}

impl std::fmt::Debug for RedbConfigStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedbConfigStore")
            .field("role", &self.role)
            .field("read_only", &matches!(self.handle, RedbHandle::ReadOnly(_)))
            .field("write_timeout", &self.write_timeout)
            .finish_non_exhaustive()
    }
}

impl RedbConfigStore {
    /// Create a brand-new `redb` config store at `path` for `role` (`Repository`,
    /// `GlobalConfig`, or `SystemConfig`), initializing `META_SCHEMA`, all
    /// required role tables, and initial migration receipts atomically.
    ///
    /// Uses OS-native handle-bound atomic publication from an anonymous or
    /// exclusively-owned descriptor so that:
    /// - Any initialization error prior to publication leaves ZERO file at
    ///   `path`, allowing immediate retry via `create`.
    /// - Publication happens strictly AFTER `META_SCHEMA`, required tables,
    ///   and initial migration receipts have durably committed (`wtx.commit()`).
    ///   If a post-publication error occurs (e.g., `parent_dir_fd.sync_all()`
    ///   or requested-path identity drift), `create` fails closed WITHOUT
    ///   unlinking `path` (preventing accidental deletion of a concurrent
    ///   replacement); any valid already-published store remains openable via
    ///   [`Self::open_read_write`] / [`Self::open_read_only`], while higher-level
    ///   `config-format.json` fresh-create state recovery (`RB-03e`/`RB-03i`)
    ///   reconciles crash boundaries.
    /// - Both the pinned parent directory and the requested `path` are verified
    ///   against our continuously-open database handle before returning `Ok`.
    ///
    /// Platform mechanisms:
    /// - **Linux:** `openat(O_TMPFILE, 0600)` → `fchmod(role_mode)` → init &
    ///   commit directly on `FileBackend` → non-overwriting
    ///   `linkat("/proc/self/fd/<fd>", parent_fd, file_name, AT_SYMLINK_FOLLOW)`
    ///   → `parent_fd.sync_all()` → requested-path identity verify.
    /// - **macOS:** anonymous unlinked fd (`openat(O_CREAT|O_EXCL)` + immediate
    ///   `unlinkat`) → `fchmod(role_mode)` → init & commit directly on
    ///   `FileBackend` → non-overwriting `fclonefileat(raw_fd, parent_fd, file_name, 0)`
    ///   → `parent_fd.sync_all()` → `openat(O_NOFOLLOW)` verifying UID/mode,
    ///   exact initial receipt `applied_at`, and requested-path identity.
    /// - **Windows:** `NtCreateFile(RootDirectory = parent_dir, FILE_CREATE)`
    ///   with explicit `D:P` (`SE_DACL_PROTECTED`) role SDDL → init & commit →
    ///   handle-bound `NtSetInformationFile(FileRenameInformation, RootDirectory =
    ///   parent_dir, ReplaceIfExists = false)` → requested-path identity verify.
    /// - **Other platforms:** fail closed (`BackendInternal`).
    pub fn create(path: impl AsRef<Path>, role: DatabaseRole) -> StorageResult<Self> {
        Self::create_inner(path.as_ref(), role, || Ok(()), |_| {})
    }

    /// Create a brand-new `config.redb` inside an existing `GlobalConfig` or
    /// `SystemConfig` scope directory with exact role permissions (`0600` for
    /// `GlobalConfig`, `0644` for `SystemConfig` on Unix; protected role ACL on
    /// Windows) (`GC-13`, `RB-03j`).
    ///
    /// Rejects non-scope roles (`Repository`, `Capture`, `Derived`) so
    /// repository mutations can never target global or system configuration
    /// directories.
    pub fn create_in_scope_dir(
        scope_dir: impl AsRef<Path>,
        role: DatabaseRole,
    ) -> StorageResult<Self> {
        if !matches!(
            role,
            DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig
        ) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "{role} role cannot create a scoped config.redb (expected GlobalConfig or SystemConfig)"
                ),
            });
        }
        Self::create(config_redb_path_in_scope_dir(scope_dir.as_ref()), role)
    }

    #[cfg(test)]
    pub(crate) fn create_with_test_hooks<F1, F2>(
        path: impl AsRef<Path>,
        role: DatabaseRole,
        before_publish: F1,
        after_publish: F2,
    ) -> StorageResult<Self>
    where
        F1: FnOnce() -> StorageResult<()>,
        F2: FnOnce(&Path),
    {
        Self::create_inner(path.as_ref(), role, before_publish, after_publish)
    }

    #[cfg(target_os = "linux")]
    fn create_inner<F1, F2>(
        path: &Path,
        role: DatabaseRole,
        before_publish: F1,
        after_publish: F2,
    ) -> StorageResult<Self>
    where
        F1: FnOnce() -> StorageResult<()>,
        F2: FnOnce(&Path),
    {
        use std::{ffi::CString, os::unix::io::AsRawFd};

        validate_config_role(role)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "redb config store path must end with a valid UTF-8 filename".to_string(),
            })?;
        let target_cname =
            CString::new(file_name).map_err(|_| StorageBackendError::BackendInternal {
                detail: "redb config store filename contains NUL byte".to_string(),
            })?;
        let parent = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };

        let (parent_dir_fd, parent_id) = unix_create::open_parent_dir(parent)?;
        unix_create::ensure_target_absent(&parent_dir_fd, &target_cname)?;
        let target_mode = crate::storage::config_store::expected_config_file_mode(role)
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: format!("{role} database role has no defined config store file mode"),
            })?;
        let (anon_file, expected_id) =
            unix_create::create_anonymous_staged_file(&parent_dir_fd, file_name, target_mode)?;
        let raw_fd = anon_file.as_raw_fd();

        let db = create_database_from_file(anon_file, role)?;
        {
            let wtx = begin_write_bounded_blocking(&db, role, DEFAULT_CONFIG_WRITE_TIMEOUT)?;
            init_meta_schema(&wtx, role)?;
            let _ = initialize_config_tables(&wtx, role)?;
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit initial redb config schema"))?;
        }
        before_publish()?;

        unix_create::publish_anonymous_fd(raw_fd, &parent_dir_fd, &target_cname)?;
        parent_dir_fd
            .sync_all()
            .map_err(|err| StorageBackendError::io("fsync parent directory", err))?;

        after_publish(path);

        unix_create::verify_requested_path_identity(
            parent,
            &parent_dir_fd,
            path,
            &target_cname,
            parent_id,
            expected_id,
        )?;

        let rtx = begin_checked_read(&db, role)?;
        verify_required_tables_read(&rtx, role)?;
        drop(rtx);

        Ok(Self {
            handle: RedbHandle::ReadWrite(Arc::new(db)),
            role,
            write_timeout: DEFAULT_CONFIG_WRITE_TIMEOUT,
        })
    }

    #[cfg(target_vendor = "apple")]
    fn create_inner<F1, F2>(
        path: &Path,
        role: DatabaseRole,
        before_publish: F1,
        after_publish: F2,
    ) -> StorageResult<Self>
    where
        F1: FnOnce() -> StorageResult<()>,
        F2: FnOnce(&Path),
    {
        use std::{ffi::CString, os::unix::io::AsRawFd};

        validate_config_role(role)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "redb config store path must end with a valid UTF-8 filename".to_string(),
            })?;
        let target_cname =
            CString::new(file_name).map_err(|_| StorageBackendError::BackendInternal {
                detail: "redb config store filename contains NUL byte".to_string(),
            })?;
        let parent = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };

        let (parent_dir_fd, parent_id) = unix_create::open_parent_dir(parent)?;
        unix_create::ensure_target_absent(&parent_dir_fd, &target_cname)?;
        let target_mode = crate::storage::config_store::expected_config_file_mode(role)
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: format!("{role} database role has no defined config store file mode"),
            })?;
        let (anon_file, _anon_id) =
            unix_create::create_anonymous_staged_file(&parent_dir_fd, file_name, target_mode)?;
        let raw_fd = anon_file.as_raw_fd();

        let expected_applied_at = {
            let tmp_db = create_database_from_file(anon_file, role)?;
            let wtx = begin_write_bounded_blocking(&tmp_db, role, DEFAULT_CONFIG_WRITE_TIMEOUT)?;
            init_meta_schema(&wtx, role)?;
            let applied_at = initialize_config_tables(&wtx, role)?;
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit initial redb config schema"))?;
            before_publish()?;

            // Publish directly from our anonymous unlinked `raw_fd` while `tmp_db`
            // keeps `raw_fd` alive (zero source pathname, atomic `EEXIST`).
            unix_create::publish_anonymous_fd_clone(raw_fd, &parent_dir_fd, &target_cname)?;
            applied_at
        };
        parent_dir_fd
            .sync_all()
            .map_err(|err| StorageBackendError::io("fsync parent directory", err))?;

        after_publish(path);

        // Open the published clone via pinned `parent_dir_fd` (`O_NOFOLLOW`),
        // verify ownership/mode, keep its descriptor continuously open inside
        // `db`, verify initial receipts match our exact `expected_applied_at`,
        // and confirm both pinned parent and requested `path` match.
        let (published_file, published_id) =
            unix_create::open_published_regular(&parent_dir_fd, &target_cname, target_mode)?;
        let db = create_database_from_file(published_file, role)?;
        {
            let rtx = begin_checked_read(&db, role)?;
            verify_required_tables_read(&rtx, role)?;
            verify_initial_receipt_timestamp(&rtx, role, &expected_applied_at)?;
        }
        unix_create::verify_requested_path_identity(
            parent,
            &parent_dir_fd,
            path,
            &target_cname,
            parent_id,
            published_id,
        )?;

        Ok(Self {
            handle: RedbHandle::ReadWrite(Arc::new(db)),
            role,
            write_timeout: DEFAULT_CONFIG_WRITE_TIMEOUT,
        })
    }

    #[cfg(windows)]
    fn create_inner<F1, F2>(
        path: &Path,
        role: DatabaseRole,
        before_publish: F1,
        after_publish: F2,
    ) -> StorageResult<Self>
    where
        F1: FnOnce() -> StorageResult<()>,
        F2: FnOnce(&Path),
    {
        validate_config_role(role)?;
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| StorageBackendError::BackendInternal {
                detail: "redb config store path must end with a valid UTF-8 filename".to_string(),
            })?;
        let parent = match path.parent() {
            Some(dir) if !dir.as_os_str().is_empty() => dir,
            _ => Path::new("."),
        };

        let (parent_dir, parent_id) = windows_create::open_parent_dir(parent)?;
        windows_create::ensure_target_absent(&parent_dir, file_name)?;

        let (mut staged, expected_id) =
            windows_create::PinnedStagedFile::create_in(&parent_dir, file_name, role)?;
        let db_file = staged.clone_for_db()?;

        let db = create_database_from_file(db_file, role)?;
        {
            let wtx = begin_write_bounded_blocking(&db, role, DEFAULT_CONFIG_WRITE_TIMEOUT)?;
            init_meta_schema(&wtx, role)?;
            let _ = initialize_config_tables(&wtx, role)?;
            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit initial redb config schema"))?;
        }
        before_publish()?;

        staged.publish_into_parent(&parent_dir, file_name)?;
        drop(staged);

        after_publish(path);

        windows_create::verify_requested_path_identity(
            parent,
            &parent_dir,
            path,
            file_name,
            parent_id,
            expected_id,
        )?;

        let rtx = begin_checked_read(&db, role)?;
        verify_required_tables_read(&rtx, role)?;
        drop(rtx);

        Ok(Self {
            handle: RedbHandle::ReadWrite(Arc::new(db)),
            role,
            write_timeout: DEFAULT_CONFIG_WRITE_TIMEOUT,
        })
    }

    #[cfg(not(any(target_os = "linux", target_vendor = "apple", windows)))]
    fn create_inner<F1, F2>(
        _path: &Path,
        role: DatabaseRole,
        _before_publish: F1,
        _after_publish: F2,
    ) -> StorageResult<Self>
    where
        F1: FnOnce() -> StorageResult<()>,
        F2: FnOnce(&Path),
    {
        validate_config_role(role)?;
        Err(StorageBackendError::BackendInternal {
            detail:
                "atomic handle-pinned redb config store creation is unsupported on this platform"
                    .to_string(),
        })
    }

    /// Open an existing writable `redb` config store at `path` for `role`,
    /// verifying `META_SCHEMA` and all required role tables immediately.
    pub fn open_read_write(path: impl AsRef<Path>, role: DatabaseRole) -> StorageResult<Self> {
        validate_config_role(role)?;
        let db = open_redb_writable(path, role, RedbRepairContext::WriteCommand)?;
        let rtx = begin_checked_read(&db, role)?;
        verify_required_tables_read(&rtx, role)?;
        drop(rtx);
        Ok(Self {
            handle: RedbHandle::ReadWrite(Arc::new(db)),
            role,
            write_timeout: DEFAULT_CONFIG_WRITE_TIMEOUT,
        })
    }

    /// Open an existing `redb` config store strictly read-only (`open_read_only`,
    /// `16 MiB` cache, `MultiWriter`, zero write upgrades) and verify `META_SCHEMA`
    /// plus all required role tables.
    pub fn open_read_only(path: impl AsRef<Path>, role: DatabaseRole) -> StorageResult<Self> {
        validate_config_role(role)?;
        let db = open_redb_read_only(path, role)?;
        let rtx = begin_checked_read(&db, role)?;
        verify_required_tables_read(&rtx, role)?;
        drop(rtx);
        Ok(Self {
            handle: RedbHandle::ReadOnly(Arc::new(db)),
            role,
            write_timeout: DEFAULT_CONFIG_WRITE_TIMEOUT,
        })
    }

    /// Wrap an existing writable `Arc<redb::Database>` whose `META_SCHEMA` and
    /// required role tables are already initialized for `role`.
    pub fn from_database(db: Arc<Database>, role: DatabaseRole) -> StorageResult<Self> {
        validate_config_role(role)?;
        let rtx = begin_checked_read(db.as_ref(), role)?;
        verify_required_tables_read(&rtx, role)?;
        drop(rtx);
        Ok(Self {
            handle: RedbHandle::ReadWrite(db),
            role,
            write_timeout: DEFAULT_CONFIG_WRITE_TIMEOUT,
        })
    }

    /// Return a cloned handle using a custom bounded write-lock timeout.
    #[must_use]
    pub fn with_write_timeout(mut self, timeout: Duration) -> Self {
        self.write_timeout = timeout;
        self
    }

    /// Returns `true` if this store handle was opened strictly read-only.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        matches!(self.handle, RedbHandle::ReadOnly(_))
    }

    fn writable_db(&self) -> StorageResult<Arc<Database>> {
        match &self.handle {
            RedbHandle::ReadWrite(db) => Ok(Arc::clone(db)),
            RedbHandle::ReadOnly(_) => Err(StorageBackendError::BackendInternal {
                detail: "cannot mutate a read-only config store".to_string(),
            }),
        }
    }

    async fn run_read<T, F>(&self, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&ReadTransaction) -> StorageResult<T> + Send + 'static,
    {
        let handle = self.handle.clone();
        let role = self.role;
        tokio::task::spawn_blocking(move || {
            let rtx = match &handle {
                RedbHandle::ReadWrite(db) => begin_checked_read(db.as_ref(), role)?,
                RedbHandle::ReadOnly(db) => begin_checked_read(db.as_ref(), role)?,
            };
            verify_required_tables_read(&rtx, role)?;
            op(&rtx)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("redb config read task failed: {err}"),
        })?
    }

    /// Execute a write transaction off the Tokio executor with cooperative
    /// cancellation protection.
    ///
    /// **Cancellation linearization point:** the atomic `Pending -> Committing`
    /// mutex transition immediately preceding `wtx.commit()`. Dropping the
    /// returned future at any point BEFORE that transition (while waiting on
    /// the write lock, queued in `spawn_blocking`, or executing `op(&wtx)`)
    /// transitions the shared state to `Cancelled` so the blocking worker
    /// aborts `wtx` with zero committed writes. Once the worker atomically
    /// transitions `Pending -> Committing`, cancellation no longer aborts the
    /// transaction and `wtx.commit()` runs to completion (atomic durability);
    /// callers retrying non-idempotent mutations (`add`) after an external
    /// timeout should inspect current state or use [`MetadataStore::cas_metadata`].
    async fn run_write<T, F>(&self, op: F) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&WriteTransaction) -> StorageResult<T> + Send + 'static,
    {
        self.run_write_with_pre_commit_hook(op, || {}).await
    }

    async fn run_write_with_pre_commit_hook<T, F, H>(
        &self,
        op: F,
        pre_commit_hook: H,
    ) -> StorageResult<T>
    where
        T: Send + 'static,
        F: FnOnce(&WriteTransaction) -> StorageResult<T> + Send + 'static,
        H: FnOnce() + Send + 'static,
    {
        #[derive(Clone, Copy, PartialEq, Eq)]
        enum WriteCancelState {
            Pending,
            Cancelled,
            Committing,
        }

        struct CancelOnDrop {
            state: Arc<Mutex<WriteCancelState>>,
            armed: bool,
        }

        impl Drop for CancelOnDrop {
            fn drop(&mut self) {
                if self.armed
                    && let Ok(mut guard) = self.state.lock()
                    && *guard == WriteCancelState::Pending
                {
                    *guard = WriteCancelState::Cancelled;
                }
            }
        }

        let db = self.writable_db()?;
        let role = self.role;
        let timeout = self.write_timeout;
        let state = Arc::new(Mutex::new(WriteCancelState::Pending));
        let mut cancel_guard = CancelOnDrop {
            state: Arc::clone(&state),
            armed: true,
        };
        let worker_state = Arc::clone(&state);

        let result = tokio::task::spawn_blocking(move || {
            let is_cancelled = || -> bool {
                worker_state
                    .lock()
                    .map(|guard| *guard == WriteCancelState::Cancelled)
                    .unwrap_or(true)
            };

            if is_cancelled() {
                return Err(StorageBackendError::BackendInternal {
                    detail: "redb config write transaction cancelled before lock acquisition"
                        .to_string(),
                });
            }

            // Poll `begin_checked_write_blocking` in short slices so a
            // cancelled caller stops waiting promptly without acquiring or
            // holding the write lock after cancellation.
            let deadline = Instant::now() + timeout;
            let wtx = loop {
                if is_cancelled() {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "redb config write transaction cancelled while waiting for lock"
                            .to_string(),
                    });
                }
                let remaining = deadline.saturating_duration_since(Instant::now());
                let slice = remaining.min(Duration::from_millis(25));
                match begin_checked_write_blocking(db.as_ref(), role, slice) {
                    Ok(wtx) => break wtx,
                    Err(StorageBackendError::WriteLockBusy { .. }) if Instant::now() < deadline => {
                        continue;
                    }
                    Err(StorageBackendError::WriteLockBusy { role, .. }) => {
                        return Err(StorageBackendError::WriteLockBusy {
                            role,
                            waited_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                        });
                    }
                    Err(err) => return Err(err),
                }
            };

            if is_cancelled() {
                let _ = wtx.abort();
                return Err(StorageBackendError::BackendInternal {
                    detail: "redb config write transaction cancelled before execution".to_string(),
                });
            }

            verify_required_tables_write(&wtx, role)?;
            let output = op(&wtx)?;
            pre_commit_hook();

            {
                let mut guard =
                    worker_state
                        .lock()
                        .map_err(|_| StorageBackendError::BackendInternal {
                            detail: "redb config write cancellation state poisoned".to_string(),
                        })?;
                if *guard == WriteCancelState::Cancelled {
                    drop(guard);
                    let _ = wtx.abort();
                    return Err(StorageBackendError::BackendInternal {
                        detail: "redb config write transaction cancelled before commit".to_string(),
                    });
                }
                *guard = WriteCancelState::Committing;
            }

            wtx.commit()
                .map_err(|err| map_redb_commit_err(err, "commit redb config transaction"))?;
            Ok(output)
        })
        .await
        .map_err(|err| StorageBackendError::BackendInternal {
            detail: format!("redb config write task failed: {err}"),
        })?;

        cancel_guard.armed = false;
        result
    }

    #[cfg(test)]
    pub(crate) async fn add_with_pre_commit_hook<H>(
        &self,
        key: &str,
        value: &str,
        encrypted: bool,
        pre_commit_hook: H,
    ) -> StorageResult<()>
    where
        H: FnOnce() + Send + 'static,
    {
        let key_owned = key.to_owned();
        let value_owned = value.to_owned();
        self.run_write_with_pre_commit_hook(
            move |wtx| {
                let exact_ids = collect_exact_key_ids_wtx(wtx, &key_owned)?;
                let mut has_encrypted = false;
                let mut has_plaintext = false;
                {
                    let primary = wtx
                        .open_table(CONFIG_KV_TABLE)
                        .map_err(|err| map_redb_table_err(err, "open config_kv for add check"))?;
                    for id in &exact_ids {
                        if let Some(raw) = primary
                            .get(*id)
                            .map_err(|err| map_redb_storage_err(err, "read config_kv for add"))?
                        {
                            let row = decode_config_row(raw.value())?;
                            if row.encrypted {
                                has_encrypted = true;
                            } else {
                                has_plaintext = true;
                            }
                        }
                    }
                }
                if !exact_ids.is_empty()
                    && ((encrypted && has_plaintext) || (!encrypted && has_encrypted))
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: "cannot mix encrypted and plaintext values for the same config key"
                            .to_string(),
                    });
                }
                let new_id = next_sequence_id(wtx, SEQ_KEY_CONFIG_KV, CONFIG_KV_TABLE)?;
                let encoded = encode_config_row(&key_owned, &value_owned, encrypted)?;
                let marker = encode_index_marker()?;
                let mut primary = wtx
                    .open_table(CONFIG_KV_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open config_kv for add insert"))?;
                let mut index = wtx.open_table(CONFIG_KV_BY_KEY_INDEX).map_err(|err| {
                    map_redb_table_err(err, "open config_kv_by_key for add insert")
                })?;
                primary
                    .insert(new_id, encoded.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert config_kv add row"))?;
                index
                    .insert((key_owned.as_str(), new_id), marker.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert config_kv add index"))?;
                Ok(())
            },
            pre_commit_hook,
        )
        .await
    }
}

fn required_table_names_for_role(role: DatabaseRole) -> &'static [&'static str] {
    if role == DatabaseRole::Repository {
        &[
            CONFIG_KV_TABLE_NAME,
            CONFIG_KV_BY_KEY_INDEX_NAME,
            CONFIG_LEGACY_TABLE_NAME,
            CONFIG_SEQ_TABLE_NAME,
            METADATA_KV_TABLE_NAME,
            SCHEMA_VERSIONS_TABLE_NAME,
        ]
    } else {
        &[
            CONFIG_KV_TABLE_NAME,
            CONFIG_KV_BY_KEY_INDEX_NAME,
            CONFIG_LEGACY_TABLE_NAME,
            CONFIG_SEQ_TABLE_NAME,
            CONFIG_SCHEMA_VERSIONS_TABLE_NAME,
        ]
    }
}

fn verify_required_tables_read(rtx: &ReadTransaction, role: DatabaseRole) -> StorageResult<()> {
    let existing: std::collections::BTreeSet<String> = rtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list tables during redb config read verify"))?
        .map(|handle| handle.name().to_owned())
        .collect();
    for required in required_table_names_for_role(role) {
        if !existing.contains(*required) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "missing required redb table `{required}` in {role} config store; database appears damaged"
                ),
            });
        }
    }
    Ok(())
}

fn verify_required_tables_write(wtx: &WriteTransaction, role: DatabaseRole) -> StorageResult<()> {
    let existing: std::collections::BTreeSet<String> = wtx
        .list_tables()
        .map_err(|err| map_redb_storage_err(err, "list tables during redb config write verify"))?
        .map(|handle| handle.name().to_owned())
        .collect();
    for required in required_table_names_for_role(role) {
        if !existing.contains(*required) {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "missing required redb table `{required}` in {role} config store; refusing to recreate table on damaged database"
                ),
            });
        }
    }
    Ok(())
}

pub(crate) fn initialize_config_tables(
    wtx: &WriteTransaction,
    role: DatabaseRole,
) -> StorageResult<String> {
    let _ = wtx
        .open_table(CONFIG_KV_TABLE)
        .map_err(|err| map_redb_table_err(err, "init config_kv table"))?;
    let _ = wtx
        .open_table(CONFIG_KV_BY_KEY_INDEX)
        .map_err(|err| map_redb_table_err(err, "init config_kv_by_key index"))?;
    let _ = wtx
        .open_table(CONFIG_LEGACY_TABLE)
        .map_err(|err| map_redb_table_err(err, "init legacy config table"))?;
    let mut seq_table = wtx
        .open_table(CONFIG_SEQ_TABLE)
        .map_err(|err| map_redb_table_err(err, "init config_sequences table"))?;
    let zero_seq = encode_versioned(
        CONFIG_SEQ_CODEC_VERSION,
        &ConfigSequenceValue { last_id: 0 },
    )?;
    seq_table
        .insert(SEQ_KEY_CONFIG_KV, zero_seq.as_slice())
        .map_err(|err| map_redb_storage_err(err, "seed config_kv sequence counter"))?;
    seq_table
        .insert(SEQ_KEY_CONFIG_LEGACY, zero_seq.as_slice())
        .map_err(|err| map_redb_storage_err(err, "seed config sequence counter"))?;
    drop(seq_table);

    let now = Utc::now().to_rfc3339();
    let ledger_def = if role == DatabaseRole::Repository {
        let _ = wtx
            .open_table(METADATA_KV_TABLE)
            .map_err(|err| map_redb_table_err(err, "init metadata_kv table"))?;
        SCHEMA_VERSIONS_TABLE
    } else {
        CONFIG_SCHEMA_VERSIONS_TABLE
    };
    let mut ledger_table = wtx
        .open_table(ledger_def)
        .map_err(|err| map_redb_table_err(err, "init schema ledger table"))?;
    for migration in migrations_for_role(role) {
        let stored = SchemaReceiptStoredRow {
            name: migration.name.to_owned(),
            applied_at: now.clone(),
        };
        let encoded = encode_versioned(SCHEMA_RECEIPT_CODEC_VERSION, &stored)?;
        ledger_table
            .insert(migration.version, encoded.as_slice())
            .map_err(|err| map_redb_storage_err(err, "seed initial schema ledger receipt"))?;
    }
    Ok(now)
}

#[cfg(target_vendor = "apple")]
fn verify_initial_receipt_timestamp(
    rtx: &ReadTransaction,
    role: DatabaseRole,
    expected_applied_at: &str,
) -> StorageResult<()> {
    let ledger_def = if role == DatabaseRole::Repository {
        SCHEMA_VERSIONS_TABLE
    } else {
        CONFIG_SCHEMA_VERSIONS_TABLE
    };
    let table = open_required_read_table(rtx, ledger_def, "open required schema ledger")?;
    let expected_migrations = migrations_for_role(role);
    let mut matched = 0usize;
    let iter = table
        .iter()
        .map_err(|err| map_redb_storage_err(err, "iterate schema ledger after publish"))?;
    for item in iter {
        let (_, v_guard) =
            item.map_err(|err| map_redb_storage_err(err, "read schema ledger after publish"))?;
        let stored: SchemaReceiptStoredRow =
            decode_versioned(v_guard.value(), SCHEMA_RECEIPT_CODEC_VERSION)?;
        if stored.applied_at != expected_applied_at {
            return Err(StorageBackendError::BackendInternal {
                detail: "published redb config store receipts do not match initialized database"
                    .to_string(),
            });
        }
        matched += 1;
    }
    if matched != expected_migrations.len() {
        return Err(StorageBackendError::BackendInternal {
            detail: "published redb config store receipt count does not match initialized database"
                .to_string(),
        });
    }
    Ok(())
}

fn open_required_read_table<'a, K: redb::Key + 'static, V: redb::Value + 'static>(
    rtx: &'a ReadTransaction,
    def: TableDefinition<'a, K, V>,
    context: &'static str,
) -> StorageResult<redb::ReadOnlyTable<K, V>> {
    rtx.open_table(def)
        .map_err(|err| map_redb_table_err(err, context))
}

fn encode_index_marker() -> StorageResult<Vec<u8>> {
    encode_versioned(CONFIG_KV_INDEX_CODEC_VERSION, &ConfigKvIndexMarker {})
}

fn verify_index_marker(bytes: &[u8]) -> StorageResult<()> {
    let _marker: ConfigKvIndexMarker = decode_versioned(bytes, CONFIG_KV_INDEX_CODEC_VERSION)?;
    Ok(())
}

fn next_sequence_id(
    wtx: &WriteTransaction,
    seq_key: &str,
    primary_def: TableDefinition<'static, u64, &[u8]>,
) -> StorageResult<u64> {
    let mut seq_table = wtx
        .open_table(CONFIG_SEQ_TABLE)
        .map_err(|err| map_redb_table_err(err, "open config_sequences"))?;
    let prev = match seq_table
        .get(seq_key)
        .map_err(|err| map_redb_storage_err(err, "read config sequence"))?
    {
        Some(raw) => {
            let decoded: ConfigSequenceValue =
                decode_versioned(raw.value(), CONFIG_SEQ_CODEC_VERSION)?;
            decoded.last_id
        }
        None => {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "missing required config sequence counter `{seq_key}`; database appears damaged"
                ),
            });
        }
    };
    let next = prev
        .checked_add(1)
        .ok_or_else(|| StorageBackendError::BackendInternal {
            detail: "config sequence counter overflow".to_string(),
        })?;
    {
        let primary = wtx
            .open_table(primary_def)
            .map_err(|err| map_redb_table_err(err, "open primary table for sequence validation"))?;
        if let Some((last_key, _)) = primary
            .last()
            .map_err(|err| map_redb_storage_err(err, "inspect max primary id"))?
            && last_key.value() >= next
        {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "config sequence counter `{seq_key}` regressed behind existing primary row id"
                ),
            });
        }
        if primary
            .get(next)
            .map_err(|err| map_redb_storage_err(err, "check allocated primary id uniqueness"))?
            .is_some()
        {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "allocated config sequence id already exists in `{seq_key}` primary table"
                ),
            });
        }
    }
    let encoded = encode_versioned(
        CONFIG_SEQ_CODEC_VERSION,
        &ConfigSequenceValue { last_id: next },
    )?;
    seq_table
        .insert(seq_key, encoded.as_slice())
        .map_err(|err| map_redb_storage_err(err, "update config sequence"))?;
    Ok(next)
}

fn decode_config_row(bytes: &[u8]) -> StorageResult<ConfigEntry> {
    let stored: ConfigKvStoredRow = decode_versioned(bytes, CONFIG_KV_CODEC_VERSION)?;
    Ok(ConfigEntry {
        key: stored.key,
        value: stored.value,
        encrypted: stored.encrypted,
    })
}

fn encode_config_row(key: &str, value: &str, encrypted: bool) -> StorageResult<Vec<u8>> {
    let stored = ConfigKvStoredRow {
        key: key.to_owned(),
        value: value.to_owned(),
        encrypted,
    };
    encode_versioned(CONFIG_KV_CODEC_VERSION, &stored)
}

fn decode_metadata_row(
    scope: &str,
    target: &str,
    key: &str,
    bytes: &[u8],
) -> StorageResult<MetadataEntryRecord> {
    let stored: MetadataStoredRow = decode_versioned(bytes, METADATA_KV_CODEC_VERSION)?;
    Ok(MetadataEntryRecord {
        scope: scope.to_owned(),
        target: target.to_owned(),
        key: key.to_owned(),
        value: stored.value,
        value_type: stored.value_type,
        created_at: stored.created_at,
        updated_at: stored.updated_at,
    })
}

fn encode_metadata_row(
    value: &str,
    value_type: &str,
    created_at: &str,
    updated_at: &str,
) -> StorageResult<Vec<u8>> {
    let stored = MetadataStoredRow {
        value: value.to_owned(),
        value_type: value_type.to_owned(),
        created_at: created_at.to_owned(),
        updated_at: updated_at.to_owned(),
    };
    encode_versioned(METADATA_KV_CODEC_VERSION, &stored)
}

fn collect_exact_key_ids_wtx(wtx: &WriteTransaction, key: &str) -> StorageResult<Vec<u64>> {
    let index = wtx
        .open_table(CONFIG_KV_BY_KEY_INDEX)
        .map_err(|err| map_redb_table_err(err, "open config_kv_by_key for lookup"))?;
    let mut ids = Vec::new();
    let range = index
        .range((key, 0u64)..=(key, u64::MAX))
        .map_err(|err| map_redb_storage_err(err, "range config_kv_by_key exact"))?;
    for item in range {
        let (k_guard, v_guard) =
            item.map_err(|err| map_redb_storage_err(err, "iterate config_kv_by_key exact"))?;
        verify_index_marker(v_guard.value())?;
        let (_, id) = k_guard.value();
        ids.push(id);
    }
    Ok(ids)
}

#[async_trait]
impl ConfigStore for RedbConfigStore {
    fn role(&self) -> DatabaseRole {
        self.role
    }

    fn backend(&self) -> StorageBackendKind {
        StorageBackendKind::Redb
    }

    async fn get(&self, key: &str) -> StorageResult<Option<ConfigEntry>> {
        let key_owned = key.to_owned();
        self.run_read(move |rtx| {
            let index = open_required_read_table(
                rtx,
                CONFIG_KV_BY_KEY_INDEX,
                "open required config_kv_by_key",
            )?;
            let primary =
                open_required_read_table(rtx, CONFIG_KV_TABLE, "open required config_kv")?;
            let mut range = index
                .range((key_owned.as_str(), 0u64)..=(key_owned.as_str(), u64::MAX))
                .map_err(|err| map_redb_storage_err(err, "range config_kv_by_key get"))?;
            let Some(last_item) = range.next_back() else {
                return Ok(None);
            };
            let (k_guard, v_guard) =
                last_item.map_err(|err| map_redb_storage_err(err, "read last config_kv id"))?;
            verify_index_marker(v_guard.value())?;
            let (_, id) = k_guard.value();
            let raw = primary
                .get(id)
                .map_err(|err| map_redb_storage_err(err, "read config_kv row by id"))?
                .ok_or_else(|| StorageBackendError::BackendInternal {
                    detail: "config_kv secondary index points to missing primary row".to_string(),
                })?;
            Ok(Some(decode_config_row(raw.value())?))
        })
        .await
    }

    async fn get_all(&self, key: &str) -> StorageResult<Vec<ConfigEntry>> {
        let key_owned = key.to_owned();
        self.run_read(move |rtx| {
            let index = open_required_read_table(
                rtx,
                CONFIG_KV_BY_KEY_INDEX,
                "open required config_kv_by_key",
            )?;
            let primary =
                open_required_read_table(rtx, CONFIG_KV_TABLE, "open required config_kv")?;
            let range = index
                .range((key_owned.as_str(), 0u64)..=(key_owned.as_str(), u64::MAX))
                .map_err(|err| map_redb_storage_err(err, "range config_kv_by_key get_all"))?;
            let mut entries = Vec::new();
            for item in range {
                let (k_guard, v_guard) =
                    item.map_err(|err| map_redb_storage_err(err, "iterate config_kv_by_key"))?;
                verify_index_marker(v_guard.value())?;
                let (_, id) = k_guard.value();
                let raw = primary
                    .get(id)
                    .map_err(|err| map_redb_storage_err(err, "read config_kv row"))?
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "config_kv secondary index points to missing primary row"
                            .to_string(),
                    })?;
                entries.push(decode_config_row(raw.value())?);
            }
            Ok(entries)
        })
        .await
    }

    async fn get_by_prefix(&self, prefix: &str) -> StorageResult<Vec<ConfigEntry>> {
        let prefix_owned = prefix.to_owned();
        self.run_read(move |rtx| {
            let index = open_required_read_table(
                rtx,
                CONFIG_KV_BY_KEY_INDEX,
                "open required config_kv_by_key",
            )?;
            let primary =
                open_required_read_table(rtx, CONFIG_KV_TABLE, "open required config_kv")?;
            let range = index
                .range((prefix_owned.as_str(), 0u64)..)
                .map_err(|err| map_redb_storage_err(err, "range config_kv_by_key prefix"))?;
            let mut entries = Vec::new();
            for item in range {
                let (k_guard, v_guard) = item
                    .map_err(|err| map_redb_storage_err(err, "iterate config_kv_by_key prefix"))?;
                let (k, id) = k_guard.value();
                if !k.starts_with(&prefix_owned) {
                    break;
                }
                verify_index_marker(v_guard.value())?;
                let raw = primary
                    .get(id)
                    .map_err(|err| map_redb_storage_err(err, "read config_kv prefix row"))?
                    .ok_or_else(|| StorageBackendError::BackendInternal {
                        detail: "config_kv secondary index points to missing primary row"
                            .to_string(),
                    })?;
                entries.push(decode_config_row(raw.value())?);
            }
            Ok(entries)
        })
        .await
    }

    async fn list_all(&self) -> StorageResult<Vec<ConfigEntry>> {
        self.run_read(move |rtx| {
            let primary =
                open_required_read_table(rtx, CONFIG_KV_TABLE, "open required config_kv")?;
            let mut entries = Vec::new();
            let iter = primary
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate config_kv list_all"))?;
            for item in iter {
                let (_, v_guard) =
                    item.map_err(|err| map_redb_storage_err(err, "read config_kv list_all item"))?;
                entries.push(decode_config_row(v_guard.value())?);
            }
            Ok(entries)
        })
        .await
    }

    async fn set(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()> {
        let key_owned = key.to_owned();
        let value_owned = value.to_owned();
        self.run_write(move |wtx| {
            let exact_ids = collect_exact_key_ids_wtx(wtx, &key_owned)?;
            if exact_ids.len() > 1 {
                return Err(StorageBackendError::BackendInternal {
                    detail: "cannot set multi-valued config key; use unset_all or add".to_string(),
                });
            }
            let exact_id = exact_ids.into_iter().next();

            // Always scan for ASCII-case-insensitive variants so `set` removes
            // other-cased rows and inherits encryption even when an exact row
            // already exists.
            let mut exact_encrypted = false;
            let mut other_cased: Vec<(u64, String, bool)> = Vec::new();
            {
                let primary = wtx
                    .open_table(CONFIG_KV_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open config_kv for case scan"))?;
                let iter = primary
                    .iter()
                    .map_err(|err| map_redb_storage_err(err, "iterate config_kv for case scan"))?;
                for item in iter {
                    let (k_guard, v_guard) =
                        item.map_err(|err| map_redb_storage_err(err, "read config_kv case scan"))?;
                    let row = decode_config_row(v_guard.value())?;
                    if row.key == key_owned {
                        exact_encrypted |= row.encrypted;
                    } else if row.key.eq_ignore_ascii_case(&key_owned) {
                        other_cased.push((k_guard.value(), row.key, row.encrypted));
                    }
                }
            }
            let has_existing_encrypted =
                exact_encrypted || other_cased.iter().any(|(_, _, enc)| *enc);
            if !encrypted && has_existing_encrypted {
                return Err(StorageBackendError::BackendInternal {
                    detail: "cannot replace an encrypted configuration value with plaintext"
                        .to_string(),
                });
            }
            let encoded = encode_config_row(&key_owned, &value_owned, encrypted)?;

            if let Some(id) = exact_id {
                let mut primary = wtx
                    .open_table(CONFIG_KV_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open config_kv for set update"))?;
                let mut index = wtx.open_table(CONFIG_KV_BY_KEY_INDEX).map_err(|err| {
                    map_redb_table_err(err, "open config_kv_by_key for set update")
                })?;
                for (old_id, old_key, _) in &other_cased {
                    primary.remove(*old_id).map_err(|err| {
                        map_redb_storage_err(err, "remove other-cased config_kv row")
                    })?;
                    index.remove((old_key.as_str(), *old_id)).map_err(|err| {
                        map_redb_storage_err(err, "remove other-cased index entry")
                    })?;
                }
                primary
                    .insert(id, encoded.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "update config_kv row"))?;
                return Ok(());
            }

            let new_id = next_sequence_id(wtx, SEQ_KEY_CONFIG_KV, CONFIG_KV_TABLE)?;
            let marker = encode_index_marker()?;
            let mut primary = wtx
                .open_table(CONFIG_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open config_kv for set insert"))?;
            let mut index = wtx
                .open_table(CONFIG_KV_BY_KEY_INDEX)
                .map_err(|err| map_redb_table_err(err, "open config_kv_by_key for set insert"))?;
            for (old_id, old_key, _) in &other_cased {
                primary
                    .remove(*old_id)
                    .map_err(|err| map_redb_storage_err(err, "remove other-cased config_kv row"))?;
                index
                    .remove((old_key.as_str(), *old_id))
                    .map_err(|err| map_redb_storage_err(err, "remove other-cased index entry"))?;
            }
            primary
                .insert(new_id, encoded.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert config_kv row"))?;
            index
                .insert((key_owned.as_str(), new_id), marker.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert config_kv index entry"))?;
            Ok(())
        })
        .await
    }

    async fn add(&self, key: &str, value: &str, encrypted: bool) -> StorageResult<()> {
        let key_owned = key.to_owned();
        let value_owned = value.to_owned();
        self.run_write(move |wtx| {
            let exact_ids = collect_exact_key_ids_wtx(wtx, &key_owned)?;
            let mut has_encrypted = false;
            let mut has_plaintext = false;
            {
                let primary = wtx
                    .open_table(CONFIG_KV_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open config_kv for add check"))?;
                for id in &exact_ids {
                    if let Some(raw) = primary
                        .get(*id)
                        .map_err(|err| map_redb_storage_err(err, "read config_kv for add check"))?
                    {
                        let row = decode_config_row(raw.value())?;
                        if row.encrypted {
                            has_encrypted = true;
                        } else {
                            has_plaintext = true;
                        }
                    }
                }
            }
            if !exact_ids.is_empty()
                && ((encrypted && has_plaintext) || (!encrypted && has_encrypted))
            {
                return Err(StorageBackendError::BackendInternal {
                    detail: "cannot mix encrypted and plaintext values for the same config key"
                        .to_string(),
                });
            }

            let new_id = next_sequence_id(wtx, SEQ_KEY_CONFIG_KV, CONFIG_KV_TABLE)?;
            let encoded = encode_config_row(&key_owned, &value_owned, encrypted)?;
            let marker = encode_index_marker()?;
            let mut primary = wtx
                .open_table(CONFIG_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open config_kv for add insert"))?;
            let mut index = wtx
                .open_table(CONFIG_KV_BY_KEY_INDEX)
                .map_err(|err| map_redb_table_err(err, "open config_kv_by_key for add insert"))?;
            primary
                .insert(new_id, encoded.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert config_kv add row"))?;
            index
                .insert((key_owned.as_str(), new_id), marker.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert config_kv add index"))?;
            Ok(())
        })
        .await
    }

    async fn unset(&self, key: &str) -> StorageResult<usize> {
        let key_owned = key.to_owned();
        self.run_write(move |wtx| {
            let exact_ids = collect_exact_key_ids_wtx(wtx, &key_owned)?;
            if exact_ids.len() > 1 {
                return Err(StorageBackendError::BackendInternal {
                    detail: "cannot unset multi-valued config key; use unset_all".to_string(),
                });
            }
            let Some(id) = exact_ids.into_iter().next() else {
                return Ok(0);
            };
            let mut primary = wtx
                .open_table(CONFIG_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open config_kv for unset"))?;
            let mut index = wtx
                .open_table(CONFIG_KV_BY_KEY_INDEX)
                .map_err(|err| map_redb_table_err(err, "open config_kv_by_key for unset"))?;
            primary
                .remove(id)
                .map_err(|err| map_redb_storage_err(err, "remove config_kv row"))?;
            index
                .remove((key_owned.as_str(), id))
                .map_err(|err| map_redb_storage_err(err, "remove config_kv index"))?;
            Ok(1)
        })
        .await
    }

    async fn unset_all(&self, key: &str) -> StorageResult<usize> {
        let key_owned = key.to_owned();
        self.run_write(move |wtx| {
            let exact_ids = collect_exact_key_ids_wtx(wtx, &key_owned)?;
            if exact_ids.is_empty() {
                return Ok(0);
            }
            let mut primary = wtx
                .open_table(CONFIG_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open config_kv for unset_all"))?;
            let mut index = wtx
                .open_table(CONFIG_KV_BY_KEY_INDEX)
                .map_err(|err| map_redb_table_err(err, "open config_kv_by_key for unset_all"))?;
            let removed = exact_ids.len();
            for id in exact_ids {
                primary
                    .remove(id)
                    .map_err(|err| map_redb_storage_err(err, "remove config_kv unset_all row"))?;
                index
                    .remove((key_owned.as_str(), id))
                    .map_err(|err| map_redb_storage_err(err, "remove config_kv unset_all index"))?;
            }
            Ok(removed)
        })
        .await
    }

    async fn remove_section(&self, section: &str) -> StorageResult<usize> {
        validate_section_name(section)?;
        let section_owned = section.to_owned();
        let prefix = format!("{section}.");
        self.run_write(move |wtx| {
            let mut targets: Vec<(String, u64)> = Vec::new();
            {
                let index = wtx.open_table(CONFIG_KV_BY_KEY_INDEX).map_err(|err| {
                    map_redb_table_err(err, "open config_kv_by_key for remove_section")
                })?;
                let range = index
                    .range((prefix.as_str(), 0u64)..)
                    .map_err(|err| map_redb_storage_err(err, "range remove_section"))?;
                for item in range {
                    let (k_guard, v_guard) =
                        item.map_err(|err| map_redb_storage_err(err, "iterate remove_section"))?;
                    let (k, id) = k_guard.value();
                    if !k.starts_with(&prefix) {
                        break;
                    }
                    verify_index_marker(v_guard.value())?;
                    if key_in_section(k, &section_owned) {
                        targets.push((k.to_owned(), id));
                    }
                }
            }
            if targets.is_empty() {
                return Err(StorageBackendError::NotFound {
                    resource: "config section".to_string(),
                });
            }
            let mut primary = wtx
                .open_table(CONFIG_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open config_kv for remove_section"))?;
            let mut index = wtx.open_table(CONFIG_KV_BY_KEY_INDEX).map_err(|err| {
                map_redb_table_err(err, "open config_kv_by_key for remove_section delete")
            })?;
            let removed = targets.len();
            for (k, id) in targets {
                primary
                    .remove(id)
                    .map_err(|err| map_redb_storage_err(err, "remove section primary row"))?;
                index
                    .remove((k.as_str(), id))
                    .map_err(|err| map_redb_storage_err(err, "remove section index entry"))?;
            }
            Ok(removed)
        })
        .await
    }

    async fn rename_section(&self, old_section: &str, new_section: &str) -> StorageResult<usize> {
        validate_section_name(old_section)?;
        validate_section_name(new_section)?;
        if old_section == new_section {
            return Err(StorageBackendError::BackendInternal {
                detail: "source and destination config sections are identical".to_string(),
            });
        }
        let old_owned = old_section.to_owned();
        let new_owned = new_section.to_owned();
        let old_prefix = format!("{old_section}.");
        let new_prefix = format!("{new_section}.");

        self.run_write(move |wtx| {
            // Secondary index is sorted by `(key ASC, id ASC)`, matching SQLite's
            // `ORDER BY key ASC, id ASC` in `rename_section`.
            let mut source_members: Vec<(u64, ConfigEntry)> = Vec::new();
            {
                let index = wtx.open_table(CONFIG_KV_BY_KEY_INDEX).map_err(|err| {
                    map_redb_table_err(err, "open config_kv_by_key for rename_section")
                })?;
                let primary = wtx
                    .open_table(CONFIG_KV_TABLE)
                    .map_err(|err| map_redb_table_err(err, "open config_kv for rename_section"))?;

                let range = index
                    .range((old_prefix.as_str(), 0u64)..)
                    .map_err(|err| map_redb_storage_err(err, "range source section"))?;
                for item in range {
                    let (k_guard, v_guard) =
                        item.map_err(|err| map_redb_storage_err(err, "iterate source section"))?;
                    let (k, id) = k_guard.value();
                    if !k.starts_with(&old_prefix) {
                        break;
                    }
                    verify_index_marker(v_guard.value())?;
                    if key_in_section(k, &old_owned) {
                        let raw = primary
                            .get(id)
                            .map_err(|err| map_redb_storage_err(err, "read source section row"))?
                            .ok_or_else(|| StorageBackendError::BackendInternal {
                                detail: "config_kv secondary index points to missing primary row"
                                    .to_string(),
                            })?;
                        source_members.push((id, decode_config_row(raw.value())?));
                    }
                }

                if source_members.is_empty() {
                    return Err(StorageBackendError::NotFound {
                        resource: "config section".to_string(),
                    });
                }

                let dest_range = index
                    .range((new_prefix.as_str(), 0u64)..)
                    .map_err(|err| map_redb_storage_err(err, "range destination section"))?;
                for item in dest_range {
                    let (k_guard, v_guard) = item
                        .map_err(|err| map_redb_storage_err(err, "iterate destination section"))?;
                    let (k, _) = k_guard.value();
                    if !k.starts_with(&new_prefix) {
                        break;
                    }
                    verify_index_marker(v_guard.value())?;
                    if key_in_section(k, &new_owned) {
                        return Err(StorageBackendError::BackendInternal {
                            detail: "destination config section already exists".to_string(),
                        });
                    }
                }
            }

            let moved = source_members.len();
            let marker = encode_index_marker()?;
            let mut new_rows: Vec<(u64, String, Vec<u8>)> = Vec::with_capacity(moved);
            for (_, entry) in &source_members {
                let var_name = entry.key.strip_prefix(&old_prefix).ok_or_else(|| {
                    StorageBackendError::BackendInternal {
                        detail: "config key failed section prefix extraction".to_string(),
                    }
                })?;
                let new_key = format!("{new_owned}.{var_name}");
                let new_id = next_sequence_id(wtx, SEQ_KEY_CONFIG_KV, CONFIG_KV_TABLE)?;
                let encoded = encode_config_row(&new_key, &entry.value, entry.encrypted)?;
                new_rows.push((new_id, new_key, encoded));
            }

            let mut primary = wtx.open_table(CONFIG_KV_TABLE).map_err(|err| {
                map_redb_table_err(err, "open config_kv for rename_section commit")
            })?;
            let mut index = wtx.open_table(CONFIG_KV_BY_KEY_INDEX).map_err(|err| {
                map_redb_table_err(err, "open config_kv_by_key for rename_section commit")
            })?;

            for (new_id, new_key, encoded) in &new_rows {
                primary
                    .insert(*new_id, encoded.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert renamed config row"))?;
                index
                    .insert((new_key.as_str(), *new_id), marker.as_slice())
                    .map_err(|err| map_redb_storage_err(err, "insert renamed config index"))?;
            }
            for (old_id, entry) in source_members {
                primary
                    .remove(old_id)
                    .map_err(|err| map_redb_storage_err(err, "remove old renamed config row"))?;
                index
                    .remove((entry.key.as_str(), old_id))
                    .map_err(|err| map_redb_storage_err(err, "remove old renamed config index"))?;
            }
            Ok(moved)
        })
        .await
    }

    async fn list_legacy_entries(&self) -> StorageResult<Vec<LegacyConfigEntry>> {
        self.run_read(move |rtx| {
            let table = open_required_read_table(
                rtx,
                CONFIG_LEGACY_TABLE,
                "open required legacy config table",
            )?;
            let mut out = Vec::new();
            let iter = table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate legacy config table"))?;
            for item in iter {
                let (_, v_guard) =
                    item.map_err(|err| map_redb_storage_err(err, "read legacy config row"))?;
                let row: LegacyConfigEntry =
                    decode_versioned(v_guard.value(), CONFIG_LEGACY_CODEC_VERSION)?;
                out.push(row);
            }
            Ok(out)
        })
        .await
    }

    async fn insert_legacy_entry(&self, entry: &LegacyConfigEntry) -> StorageResult<()> {
        let owned = entry.clone();
        self.run_write(move |wtx| {
            let id = next_sequence_id(wtx, SEQ_KEY_CONFIG_LEGACY, CONFIG_LEGACY_TABLE)?;
            let encoded = encode_versioned(CONFIG_LEGACY_CODEC_VERSION, &owned)?;
            let mut table = wtx
                .open_table(CONFIG_LEGACY_TABLE)
                .map_err(|err| map_redb_table_err(err, "open legacy config table for insert"))?;
            table
                .insert(id, encoded.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert legacy config row"))?;
            Ok(())
        })
        .await
    }

    async fn list_schema_receipts(&self) -> StorageResult<Vec<SchemaVersionReceipt>> {
        let role = self.role;
        self.run_read(move |rtx| {
            let def = if role == DatabaseRole::Repository {
                SCHEMA_VERSIONS_TABLE
            } else {
                CONFIG_SCHEMA_VERSIONS_TABLE
            };
            let table = open_required_read_table(rtx, def, "open required schema ledger table")?;
            let mut out = Vec::new();
            let iter = table
                .iter()
                .map_err(|err| map_redb_storage_err(err, "iterate schema ledger"))?;
            for item in iter {
                let (k_guard, v_guard) =
                    item.map_err(|err| map_redb_storage_err(err, "read schema ledger row"))?;
                let stored: SchemaReceiptStoredRow =
                    decode_versioned(v_guard.value(), SCHEMA_RECEIPT_CODEC_VERSION)?;
                out.push(SchemaVersionReceipt {
                    version: k_guard.value(),
                    name: stored.name,
                    applied_at: stored.applied_at,
                });
            }
            Ok(out)
        })
        .await
    }

    async fn record_schema_receipt(&self, receipt: &SchemaVersionReceipt) -> StorageResult<()> {
        let role = self.role;
        let owned = receipt.clone();
        self.run_write(move |wtx| {
            let def = if role == DatabaseRole::Repository {
                SCHEMA_VERSIONS_TABLE
            } else {
                CONFIG_SCHEMA_VERSIONS_TABLE
            };
            let stored = SchemaReceiptStoredRow {
                name: owned.name,
                applied_at: owned.applied_at,
            };
            let encoded = encode_versioned(SCHEMA_RECEIPT_CODEC_VERSION, &stored)?;
            let mut table = wtx
                .open_table(def)
                .map_err(|err| map_redb_table_err(err, "open schema ledger for write"))?;
            table
                .insert(owned.version, encoded.as_slice())
                .map_err(|err| map_redb_storage_err(err, "insert schema ledger receipt"))?;
            Ok(())
        })
        .await
    }
}

#[async_trait]
impl MetadataStore for RedbConfigStore {
    async fn get_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
    ) -> StorageResult<Option<MetadataEntryRecord>> {
        validate_metadata_role(self.role)?;
        let scope_owned = scope.to_owned();
        let target_owned = target.to_owned();
        let key_owned = key.to_owned();
        self.run_read(move |rtx| {
            let table =
                open_required_read_table(rtx, METADATA_KV_TABLE, "open required metadata_kv")?;
            let Some(raw) = table
                .get((
                    scope_owned.as_str(),
                    target_owned.as_str(),
                    key_owned.as_str(),
                ))
                .map_err(|err| map_redb_storage_err(err, "get metadata_kv"))?
            else {
                return Ok(None);
            };
            Ok(Some(decode_metadata_row(
                &scope_owned,
                &target_owned,
                &key_owned,
                raw.value(),
            )?))
        })
        .await
    }

    async fn set_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        value: &str,
        value_type: &str,
    ) -> StorageResult<Option<String>> {
        validate_metadata_role(self.role)?;
        let scope_owned = scope.to_owned();
        let target_owned = target.to_owned();
        let key_owned = key.to_owned();
        let value_owned = value.to_owned();
        let vtype_owned = value_type.to_owned();
        self.run_write(move |wtx| {
            let mut table = wtx
                .open_table(METADATA_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open metadata_kv for set"))?;
            let existing = table
                .get((
                    scope_owned.as_str(),
                    target_owned.as_str(),
                    key_owned.as_str(),
                ))
                .map_err(|err| map_redb_storage_err(err, "read metadata_kv for set"))?
                .map(|g| decode_metadata_row(&scope_owned, &target_owned, &key_owned, g.value()))
                .transpose()?;
            let now = Utc::now().to_rfc3339();
            let (previous, created_at) = match existing {
                Some(prev) => (Some(prev.value), prev.created_at),
                None => (None, now.clone()),
            };
            let encoded = encode_metadata_row(&value_owned, &vtype_owned, &created_at, &now)?;
            table
                .insert(
                    (
                        scope_owned.as_str(),
                        target_owned.as_str(),
                        key_owned.as_str(),
                    ),
                    encoded.as_slice(),
                )
                .map_err(|err| map_redb_storage_err(err, "upsert metadata_kv"))?;
            Ok(previous)
        })
        .await
    }

    async fn cas_metadata(
        &self,
        scope: &str,
        target: &str,
        key: &str,
        expected: Option<&str>,
        new_value: Option<(&str, &str)>,
    ) -> StorageResult<MetadataCasOutcome> {
        validate_metadata_role(self.role)?;
        let scope_owned = scope.to_owned();
        let target_owned = target.to_owned();
        let key_owned = key.to_owned();
        let expected_owned = expected.map(str::to_owned);
        let new_owned = new_value.map(|(v, vt)| (v.to_owned(), vt.to_owned()));

        self.run_write(move |wtx| {
            let mut table = wtx
                .open_table(METADATA_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open metadata_kv for cas"))?;
            let existing = table
                .get((
                    scope_owned.as_str(),
                    target_owned.as_str(),
                    key_owned.as_str(),
                ))
                .map_err(|err| map_redb_storage_err(err, "read metadata_kv for cas"))?
                .map(|g| decode_metadata_row(&scope_owned, &target_owned, &key_owned, g.value()))
                .transpose()?;

            let current_val = existing.as_ref().map(|r| r.value.as_str());
            if current_val != expected_owned.as_deref() {
                return Ok(MetadataCasOutcome::Conflict { actual: existing });
            }

            match new_owned {
                Some((val, vtype)) => {
                    let now = Utc::now().to_rfc3339();
                    let created_at = existing
                        .as_ref()
                        .map(|r| r.created_at.clone())
                        .unwrap_or_else(|| now.clone());
                    let encoded = encode_metadata_row(&val, &vtype, &created_at, &now)?;
                    table
                        .insert(
                            (
                                scope_owned.as_str(),
                                target_owned.as_str(),
                                key_owned.as_str(),
                            ),
                            encoded.as_slice(),
                        )
                        .map_err(|err| map_redb_storage_err(err, "cas_metadata write"))?;
                    let current = MetadataEntryRecord {
                        scope: scope_owned,
                        target: target_owned,
                        key: key_owned,
                        value: val,
                        value_type: vtype,
                        created_at,
                        updated_at: now,
                    };
                    Ok(MetadataCasOutcome::Swapped {
                        previous: existing,
                        current: Some(current),
                    })
                }
                None => {
                    if existing.is_some() {
                        table
                            .remove((
                                scope_owned.as_str(),
                                target_owned.as_str(),
                                key_owned.as_str(),
                            ))
                            .map_err(|err| map_redb_storage_err(err, "cas_metadata delete"))?;
                    }
                    Ok(MetadataCasOutcome::Swapped {
                        previous: existing,
                        current: None,
                    })
                }
            }
        })
        .await
    }

    async fn unset_metadata(&self, scope: &str, target: &str, key: &str) -> StorageResult<bool> {
        validate_metadata_role(self.role)?;
        let scope_owned = scope.to_owned();
        let target_owned = target.to_owned();
        let key_owned = key.to_owned();
        self.run_write(move |wtx| {
            let mut table = wtx
                .open_table(METADATA_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open metadata_kv for unset"))?;
            let removed = table
                .remove((
                    scope_owned.as_str(),
                    target_owned.as_str(),
                    key_owned.as_str(),
                ))
                .map_err(|err| map_redb_storage_err(err, "remove metadata_kv"))?
                .is_some();
            Ok(removed)
        })
        .await
    }

    async fn list_metadata(
        &self,
        scope: &str,
        target: &str,
        key_prefix: Option<&str>,
    ) -> StorageResult<Vec<MetadataEntryRecord>> {
        validate_metadata_role(self.role)?;
        let scope_owned = scope.to_owned();
        let target_owned = target.to_owned();
        let prefix_owned = key_prefix.map(str::to_owned);
        self.run_read(move |rtx| {
            let table =
                open_required_read_table(rtx, METADATA_KV_TABLE, "open required metadata_kv")?;
            let start_key = prefix_owned.as_deref().unwrap_or("");
            let range = table
                .range((scope_owned.as_str(), target_owned.as_str(), start_key)..)
                .map_err(|err| map_redb_storage_err(err, "range metadata_kv target"))?;
            let mut out = Vec::new();
            for item in range {
                let (k_guard, v_guard) =
                    item.map_err(|err| map_redb_storage_err(err, "iterate metadata_kv target"))?;
                let (s, t, k) = k_guard.value();
                if s != scope_owned || t != target_owned {
                    break;
                }
                if let Some(prefix) = &prefix_owned
                    && !k.starts_with(prefix)
                {
                    break;
                }
                out.push(decode_metadata_row(s, t, k, v_guard.value())?);
            }
            Ok(out)
        })
        .await
    }

    async fn list_metadata_scope(&self, scope: &str) -> StorageResult<Vec<MetadataEntryRecord>> {
        validate_metadata_role(self.role)?;
        let scope_owned = scope.to_owned();
        self.run_read(move |rtx| {
            let table =
                open_required_read_table(rtx, METADATA_KV_TABLE, "open required metadata_kv")?;
            let range = table
                .range((scope_owned.as_str(), "", "")..)
                .map_err(|err| map_redb_storage_err(err, "range metadata_kv scope"))?;
            let mut out = Vec::new();
            for item in range {
                let (k_guard, v_guard) =
                    item.map_err(|err| map_redb_storage_err(err, "iterate metadata_kv scope"))?;
                let (s, t, k) = k_guard.value();
                if s != scope_owned {
                    break;
                }
                out.push(decode_metadata_row(s, t, k, v_guard.value())?);
            }
            Ok(out)
        })
        .await
    }

    async fn delete_metadata_target(&self, scope: &str, target: &str) -> StorageResult<u64> {
        validate_metadata_role(self.role)?;
        let scope_owned = scope.to_owned();
        let target_owned = target.to_owned();
        self.run_write(move |wtx| {
            let mut table = wtx
                .open_table(METADATA_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open metadata_kv for target delete"))?;
            let mut keys = Vec::new();
            {
                let range = table
                    .range((scope_owned.as_str(), target_owned.as_str(), "")..)
                    .map_err(|err| map_redb_storage_err(err, "range metadata_kv for delete"))?;
                for item in range {
                    let (k_guard, _) = item.map_err(|err| {
                        map_redb_storage_err(err, "iterate metadata_kv for delete")
                    })?;
                    let (s, t, k) = k_guard.value();
                    if s != scope_owned || t != target_owned {
                        break;
                    }
                    keys.push(k.to_owned());
                }
            }
            let count = keys.len() as u64;
            for k in keys {
                table
                    .remove((scope_owned.as_str(), target_owned.as_str(), k.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "delete metadata_kv row"))?;
            }
            Ok(count)
        })
        .await
    }

    async fn rename_metadata_target(
        &self,
        scope: &str,
        old_target: &str,
        new_target: &str,
    ) -> StorageResult<()> {
        validate_metadata_role(self.role)?;
        if old_target == new_target {
            return Ok(());
        }
        let scope_owned = scope.to_owned();
        let old_owned = old_target.to_owned();
        let new_owned = new_target.to_owned();
        self.run_write(move |wtx| {
            let mut table = wtx
                .open_table(METADATA_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open metadata_kv for target rename"))?;

            let mut dest_keys = Vec::new();
            {
                let range = table
                    .range((scope_owned.as_str(), new_owned.as_str(), "")..)
                    .map_err(|err| map_redb_storage_err(err, "range dest metadata target"))?;
                for item in range {
                    let (k_guard, _) = item
                        .map_err(|err| map_redb_storage_err(err, "iterate dest metadata target"))?;
                    let (s, t, k) = k_guard.value();
                    if s != scope_owned || t != new_owned {
                        break;
                    }
                    dest_keys.push(k.to_owned());
                }
            }
            for k in dest_keys {
                table
                    .remove((scope_owned.as_str(), new_owned.as_str(), k.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "clear dest metadata row"))?;
            }

            let mut source_rows: Vec<(String, Vec<u8>)> = Vec::new();
            {
                let range = table
                    .range((scope_owned.as_str(), old_owned.as_str(), "")..)
                    .map_err(|err| map_redb_storage_err(err, "range source metadata target"))?;
                for item in range {
                    let (k_guard, v_guard) = item.map_err(|err| {
                        map_redb_storage_err(err, "iterate source metadata target")
                    })?;
                    let (s, t, k) = k_guard.value();
                    if s != scope_owned || t != old_owned {
                        break;
                    }
                    source_rows.push((k.to_owned(), v_guard.value().to_vec()));
                }
            }
            for (k, raw) in &source_rows {
                table
                    .insert(
                        (scope_owned.as_str(), new_owned.as_str(), k.as_str()),
                        raw.as_slice(),
                    )
                    .map_err(|err| map_redb_storage_err(err, "insert renamed metadata row"))?;
            }
            for (k, _) in source_rows {
                table
                    .remove((scope_owned.as_str(), old_owned.as_str(), k.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "remove old metadata row"))?;
            }
            Ok(())
        })
        .await
    }

    async fn copy_metadata_target(
        &self,
        scope: &str,
        from_target: &str,
        to_target: &str,
    ) -> StorageResult<()> {
        validate_metadata_role(self.role)?;
        if from_target == to_target {
            return Ok(());
        }
        let scope_owned = scope.to_owned();
        let from_owned = from_target.to_owned();
        let to_owned = to_target.to_owned();
        self.run_write(move |wtx| {
            let mut table = wtx
                .open_table(METADATA_KV_TABLE)
                .map_err(|err| map_redb_table_err(err, "open metadata_kv for target copy"))?;

            let mut dest_keys = Vec::new();
            {
                let range = table
                    .range((scope_owned.as_str(), to_owned.as_str(), "")..)
                    .map_err(|err| map_redb_storage_err(err, "range copy dest metadata target"))?;
                for item in range {
                    let (k_guard, _) = item.map_err(|err| {
                        map_redb_storage_err(err, "iterate copy dest metadata target")
                    })?;
                    let (s, t, k) = k_guard.value();
                    if s != scope_owned || t != to_owned {
                        break;
                    }
                    dest_keys.push(k.to_owned());
                }
            }
            for k in dest_keys {
                table
                    .remove((scope_owned.as_str(), to_owned.as_str(), k.as_str()))
                    .map_err(|err| map_redb_storage_err(err, "clear copy dest metadata row"))?;
            }

            let mut source_rows: Vec<MetadataEntryRecord> = Vec::new();
            {
                let range = table
                    .range((scope_owned.as_str(), from_owned.as_str(), "")..)
                    .map_err(|err| {
                        map_redb_storage_err(err, "range copy source metadata target")
                    })?;
                for item in range {
                    let (k_guard, v_guard) = item.map_err(|err| {
                        map_redb_storage_err(err, "iterate copy source metadata target")
                    })?;
                    let (s, t, k) = k_guard.value();
                    if s != scope_owned || t != from_owned {
                        break;
                    }
                    source_rows.push(decode_metadata_row(s, t, k, v_guard.value())?);
                }
            }

            let now = Utc::now().to_rfc3339();
            for entry in source_rows {
                let encoded = encode_metadata_row(&entry.value, &entry.value_type, &now, &now)?;
                table
                    .insert(
                        (scope_owned.as_str(), to_owned.as_str(), entry.key.as_str()),
                        encoded.as_slice(),
                    )
                    .map_err(|err| map_redb_storage_err(err, "insert copied metadata row"))?;
            }
            Ok(())
        })
        .await
    }
}
