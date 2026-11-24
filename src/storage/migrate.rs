//! Two-phase journaled-fence bidirectional storage migration engine (`ADR-RB-07`).
//!
//! All migration logic lives exclusively under `src/storage/` and is compiled
//! only when both `storage-sqlite` and `storage-redb` features are enabled.
//!
//! # SQLite Lock-Safety Protocol (`ADR-RB-07` #1)
//!
//! On Unix, closing any non-SQLite file descriptor pointing to a live SQLite
//! database file in the same process silently releases all POSIX `fcntl`
//! advisory locks held by SQLite on that inode (`howtocorrupt.html` §2.2.1).
//! Therefore:
//! - Migration code **never** opens raw `std::fs::File` descriptors on live
//!   `.db` paths (all path inspection uses `lstat` / `fs::symlink_metadata`
//!   only, and all database operations go exclusively through SQLite's own VFS
//!   whose `unixClose` defers `close(fd)` via `pInode->pUnused` whenever any
//!   SQLite lock is held on that inode).
//! - Every source SQLite database is opened through a **single non-pooled
//!   `SqliteConnection`** (`c_mig`) configured with:
//!   - `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE = 1` (prevents close-time WAL
//!     checkpointing on rejected WAL sources)
//!   - `busy_timeout = 0`
//!   - `PRAGMA locking_mode = EXCLUSIVE;` (set before `BEGIN EXCLUSIVE`; does
//!     not read schema or acquire locks, forces `WAL_HEAPMEMORY_MODE` so `-shm`
//!     is never created on a WAL database, and retains the OS `EXCLUSIVE` lock
//!     across `ROLLBACK` until `c_mig` is closed)
//!   - No-write `BEGIN EXCLUSIVE; ROLLBACK;` (in `DELETE` mode), proving zero
//!     active external writers or `DELETE`-mode readers (`LBR-IO-001` on busy)
//!   - `PRAGMA synchronous = FULL;` (verified `== 2` while holding `EXCLUSIVE`).

use std::{
    ffi::OsString,
    fs, io,
    path::{Path, PathBuf},
    time::Duration,
};

use sea_orm::sqlx::{
    Connection as _, Executor as _, SqliteConnection, sqlite::SqliteConnectOptions,
};

use crate::{
    internal::db::schema::DatabaseRole,
    storage::{
        error::{StorageBackendError, StorageResult},
        manifest::{LEGACY_SQLITE_DB_FILE, PRIVATE_SUBDIR},
    },
};

/// Canonical advisory lock filename for repository storage migrations under `<common_storage>`.
pub const MIGRATION_LOCK_FILE: &str = "migration.lock";

/// Canonical relative path under `<common_storage>` for the legacy SQLite Capture database.
pub const LEGACY_CAPTURE_SQLITE_REL_PATH: &str = "private/capture.db";

/// SQLite `sqlite3_db_config` op code that disables automatic WAL checkpointing
/// when closing the last connection to a WAL database (`sqlite3.h`).
const SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE: std::ffi::c_int = 1006;

/// SQLite `sqlite3_file_control` op code that returns SQLite's own open
/// `sqlite3_file*` handle for a database (zero extra OS file descriptor opened
/// or closed, `sqlite3.h`).
const SQLITE_FCNTL_FILE_POINTER: std::ffi::c_int = 7;

/// SQLite `sqlite3_file_control` op code that checks whether the underlying
/// open database file descriptor still matches the inode currently at its path
/// (`sqlite3.h`).
const SQLITE_FCNTL_HAS_MOVED: std::ffi::c_int = 20;

/// SQLite `SQLITE_OK` return code.
const SQLITE_OK: std::ffi::c_int = 0;

/// SQLite `SQLITE_NOTFOUND` return code (e.g. VFS does not support an optional file_control).
const SQLITE_NOTFOUND: std::ffi::c_int = 12;

/// Sanitized stage identifier for SQLite Phase 1 quiescence failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SqliteQuiesceStage {
    /// Target role validation (repository forward migration accepts only `Repository` / `Capture`).
    RoleValidation,
    /// Non-opening `lstat` inspection of the database path or WAL companions.
    PathValidation,
    /// Opening the single non-pooled migration `SqliteConnection`.
    Connect,
    /// Configuring `busy_timeout = 0`, `NO_CKPT_ON_CLOSE`, `locking_mode = EXCLUSIVE`, and `synchronous = FULL`.
    ConfigurePragmas,
    /// Inspecting `PRAGMA journal_mode` and companion `-wal`/`-shm` presence.
    InspectJournalMode,
    /// Transitioning a WAL/non-DELETE database directly via `PRAGMA journal_mode = DELETE;`.
    SwitchJournalModeDelete,
    /// Acquiring the OS-level `EXCLUSIVE` lock via no-write `BEGIN EXCLUSIVE`.
    BeginExclusive,
    /// Ending the no-write probe transaction via `ROLLBACK` while retaining `EXCLUSIVE`.
    RollbackProbe,
}

impl SqliteQuiesceStage {
    /// Human-readable sanitized stage label.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RoleValidation => "role validation",
            Self::PathValidation => "path validation",
            Self::Connect => "single-connection open",
            Self::ConfigurePragmas => "configure synchronous=FULL and locking_mode=EXCLUSIVE",
            Self::InspectJournalMode => "inspect journal_mode",
            Self::SwitchJournalModeDelete => "PRAGMA journal_mode = DELETE",
            Self::BeginExclusive => "BEGIN EXCLUSIVE",
            Self::RollbackProbe => "ROLLBACK probe transaction",
        }
    }
}

/// Return the canonical legacy Git SQLite path `<common_storage>/libra.db`.
#[must_use]
pub fn git_sqlite_db_path(common_storage: &Path) -> PathBuf {
    common_storage.join(LEGACY_SQLITE_DB_FILE)
}

/// Return the canonical legacy Capture SQLite path `<common_storage>/private/capture.db`.
#[must_use]
pub fn capture_sqlite_db_path(common_storage: &Path) -> PathBuf {
    common_storage.join(PRIVATE_SUBDIR).join("capture.db")
}

/// Build the SQLite sidecar path by appending `suffix` (e.g. `"-wal"` or `"-shm"`)
/// directly to `db_path` without altering extensions.
fn sqlite_sidecar_path(db_path: &Path, suffix: &str) -> PathBuf {
    let mut os = OsString::from(db_path.as_os_str());
    os.push(suffix);
    PathBuf::from(os)
}

/// Toggle SQLite's automatic close-time WAL checkpoint (`SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE`)
/// on `conn`.
///
/// Setting `no_ckpt = true` ensures closing a rejected WAL connection never
/// checkpoints pending WAL frames into `.db`. Setting `no_ckpt = false` is
/// used strictly when a cleanly-closed WAL source had no `-wal` file before
/// `BEGIN EXCLUSIVE` and `sqlite3WalOpen` created an empty 0-byte `-wal` under
/// `WAL_HEAPMEMORY_MODE` so `sqlite3WalClose` atomically unlinks that empty
/// sidecar while still holding the OS `EXCLUSIVE` lock (0 frames -> 0 bytes
/// written to `.db`).
async fn set_wal_checkpoint_on_close(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
    no_ckpt: bool,
) -> StorageResult<()> {
    let mut handle = conn.lock_handle().await.map_err(|err| {
        map_quiesce_sqlite_error(role, SqliteQuiesceStage::ConfigurePragmas, &err)
    })?;
    unsafe extern "C" {
        fn sqlite3_db_config(
            db: *mut std::ffi::c_void,
            op: std::ffi::c_int,
            ...
        ) -> std::ffi::c_int;
    }
    let desired: std::ffi::c_int = i32::from(no_ckpt);
    let mut current: std::ffi::c_int = -1;
    // SAFETY: `lock_handle()` holds the SQLx worker lock exclusively for this
    // live `sqlite3*` handle; `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE` (1006) takes
    // `(int enable, int *pOut)` per the stable SQLite 3 C API.
    let rc = unsafe {
        sqlite3_db_config(
            handle.as_raw_handle().as_ptr().cast(),
            SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
            desired,
            &mut current as *mut std::ffi::c_int,
        )
    };
    if rc != SQLITE_OK || current != desired {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence failed for {role} at {}: could not set SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE={desired} (rc={rc}, current={current})",
                SqliteQuiesceStage::ConfigurePragmas.as_str()
            ),
        });
    }
    Ok(())
}

/// Verify via `SQLITE_FCNTL_HAS_MOVED` (zero extra fd opened) that SQLite's
/// live open file descriptor for `"main"` still refers to the exact inode
/// currently reachable at `db_path` (catching rename/replace/swap-back TOCTOU
/// between `lstat` and `connect_with`).
async fn verify_connection_not_moved(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
) -> StorageResult<()> {
    let mut handle = conn
        .lock_handle()
        .await
        .map_err(|err| map_quiesce_sqlite_error(role, SqliteQuiesceStage::PathValidation, &err))?;
    unsafe extern "C" {
        fn sqlite3_file_control(
            db: *mut std::ffi::c_void,
            z_db_name: *const std::ffi::c_char,
            op: std::ffi::c_int,
            p_arg: *mut std::ffi::c_void,
        ) -> std::ffi::c_int;
    }
    let mut moved: std::ffi::c_int = 1;
    // SAFETY: `lock_handle()` holds the SQLx worker lock exclusively for this
    // live `sqlite3*` handle; `"main\0"` is a static NUL-terminated C string;
    // `SQLITE_FCNTL_HAS_MOVED` (20) writes `0` (unmoved) or `1` (moved) to the
    // `c_int*` output argument per the stable SQLite 3 C API.
    let rc = unsafe {
        sqlite3_file_control(
            handle.as_raw_handle().as_ptr().cast(),
            c"main".as_ptr(),
            SQLITE_FCNTL_HAS_MOVED,
            (&mut moved as *mut std::ffi::c_int).cast(),
        )
    };
    if rc == SQLITE_NOTFOUND {
        return Ok(());
    }
    if rc != SQLITE_OK || moved != 0 {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: opened sqlite descriptor no longer matches validated source path (moved={moved}, rc={rc})",
                SqliteQuiesceStage::PathValidation.as_str()
            ),
        });
    }
    Ok(())
}

/// Read the 20-byte SQLite database header directly through SQLite's own open
/// `"main"` `sqlite3_file` descriptor (`SQLITE_FCNTL_FILE_POINTER` + `xRead`),
/// opening ZERO extra OS file descriptors, and return `true` iff the on-disk
/// database header declares WAL format (`write_version == 2 && read_version == 2`).
///
/// This distinguishes a genuine WAL database from a `DELETE`-mode database
/// (`1/1`) that merely has a stray `-wal` file on disk (which SQLite's
/// `pagerOpenWalIfPresent()` otherwise reports as `PRAGMA journal_mode = wal`).
async fn read_sqlite_header_is_wal(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
) -> StorageResult<bool> {
    #[repr(C)]
    struct Sqlite3File {
        p_methods: *const Sqlite3IoMethods,
    }
    #[repr(C)]
    struct Sqlite3IoMethods {
        i_version: std::ffi::c_int,
        x_close: Option<unsafe extern "C" fn(*mut Sqlite3File) -> std::ffi::c_int>,
        x_read: Option<
            unsafe extern "C" fn(
                *mut Sqlite3File,
                *mut std::ffi::c_void,
                std::ffi::c_int,
                i64,
            ) -> std::ffi::c_int,
        >,
    }

    let mut handle = conn.lock_handle().await.map_err(|err| {
        map_quiesce_sqlite_error(role, SqliteQuiesceStage::InspectJournalMode, &err)
    })?;
    unsafe extern "C" {
        fn sqlite3_file_control(
            db: *mut std::ffi::c_void,
            z_db_name: *const std::ffi::c_char,
            op: std::ffi::c_int,
            p_arg: *mut std::ffi::c_void,
        ) -> std::ffi::c_int;
    }

    let mut p_file: *mut Sqlite3File = std::ptr::null_mut();
    // SAFETY: `lock_handle()` holds the SQLx worker lock exclusively for this
    // live `sqlite3*` handle; `"main\0"` is a static NUL-terminated C string;
    // `SQLITE_FCNTL_FILE_POINTER` (7) writes SQLite's open `sqlite3_file*` to
    // `p_arg` per the stable SQLite 3 C API.
    let rc = unsafe {
        sqlite3_file_control(
            handle.as_raw_handle().as_ptr().cast(),
            c"main".as_ptr(),
            SQLITE_FCNTL_FILE_POINTER,
            (&mut p_file as *mut *mut Sqlite3File).cast(),
        )
    };
    if rc != SQLITE_OK || p_file.is_null() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence failed for {role} at {}: unable to inspect sqlite main file pointer (rc={rc})",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    }

    let mut header = [0u8; 20];
    // SAFETY: `p_file` is owned by the locked live `sqlite3*` pager; its
    // `p_methods` table conforms to SQLite 3's stable `sqlite3_io_methods` ABI
    // (`iVersion`, `xClose`, `xRead`), and `header` is a valid 20-byte buffer.
    let read_rc = unsafe {
        let methods = (*p_file).p_methods;
        if methods.is_null() {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence failed for {role} at {}: sqlite main file methods unavailable",
                    SqliteQuiesceStage::InspectJournalMode.as_str()
                ),
            });
        }
        let Some(x_read) = (*methods).x_read else {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence failed for {role} at {}: sqlite main file xRead unavailable",
                    SqliteQuiesceStage::InspectJournalMode.as_str()
                ),
            });
        };
        x_read(p_file, header.as_mut_ptr().cast(), 20, 0)
    };
    if read_rc != SQLITE_OK || &header[..16] != b"SQLite format 3\0" {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: invalid sqlite database header (rc={read_rc})",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    }
    Ok(header[18] == 2 && header[19] == 2)
}

/// Validate that `role` is eligible for repository forward migration quiescence.
///
/// Strictly restricts `RB-10` to `DatabaseRole::Repository` and
/// `DatabaseRole::Capture`; rejects `GlobalConfig`, `SystemConfig`, and
/// `Derived` before any filesystem path inspection occurs.
fn validate_repository_forward_role(role: DatabaseRole) -> StorageResult<()> {
    match role {
        DatabaseRole::Repository | DatabaseRole::Capture => Ok(()),
        DatabaseRole::GlobalConfig | DatabaseRole::SystemConfig | DatabaseRole::Derived => {
            Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected for {role} at {}: role is not eligible for repository forward migration",
                    SqliteQuiesceStage::RoleValidation.as_str()
                ),
            })
        }
    }
}

/// Inspect `db_path` via `lstat` (zero fd opened) and validate that it is a
/// regular, non-symlink file.
fn validate_sqlite_file_metadata(
    db_path: &Path,
    role: DatabaseRole,
) -> StorageResult<fs::Metadata> {
    let meta = match fs::symlink_metadata(db_path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Err(StorageBackendError::NotFound {
                resource: format!("{role} sqlite database"),
            });
        }
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect source sqlite database metadata",
                err,
            ));
        }
    };
    if meta.file_type().is_symlink() || !meta.file_type().is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: source sqlite path must be a regular non-symlink file",
                SqliteQuiesceStage::PathValidation.as_str()
            ),
        });
    }
    Ok(meta)
}

/// Return `true` when `a` and `b` alias the same underlying inode/file
/// (checked via `lstat` on Unix with zero file descriptors opened).
fn same_file_identity(
    a_path: &Path,
    a_meta: &fs::Metadata,
    b_path: &Path,
    b_meta: &fs::Metadata,
) -> StorageResult<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        let _ = (a_path, b_path);
        Ok(a_meta.dev() == b_meta.dev() && a_meta.ino() == b_meta.ino())
    }
    #[cfg(not(unix))]
    {
        let _ = (a_meta, b_meta);
        let canon_a = fs::canonicalize(a_path)
            .map_err(|err| StorageBackendError::io("canonicalize git sqlite path", err))?;
        let canon_b = fs::canonicalize(b_path)
            .map_err(|err| StorageBackendError::io("canonicalize capture sqlite path", err))?;
        Ok(canon_a == canon_b)
    }
}

/// Check whether a path exists using `lstat` (`symlink_metadata`) without ever
/// opening or closing a file descriptor on the target inode.
fn path_exists_no_open(path: &Path) -> StorageResult<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(err) => Err(StorageBackendError::io(
            "inspect sqlite sidecar metadata",
            err,
        )),
    }
}

/// Return `true` when `err` represents SQLite lock contention (`SQLITE_BUSY` = 5
/// or `SQLITE_LOCKED` = 6, including extended result codes).
fn is_sqlite_busy_error(err: &sea_orm::sqlx::Error) -> bool {
    if let Some(db_err) = err.as_database_error() {
        if let Some(code) = db_err.code()
            && let Ok(numeric) = code.parse::<i32>()
        {
            let primary = numeric & 0xff;
            if primary == 5 || primary == 6 {
                return true;
            }
        }
        let msg = db_err.message();
        if msg.contains("database is locked")
            || msg.contains("database table is locked")
            || msg.contains("SQLITE_BUSY")
            || msg.contains("SQLITE_LOCKED")
        {
            return true;
        }
    }
    let rendered = err.to_string();
    rendered.contains("database is locked")
        || rendered.contains("database table is locked")
        || rendered.contains("SQLITE_BUSY")
        || rendered.contains("SQLITE_LOCKED")
}

/// Map a SQLite quiescence error into a sanitized `LBR-IO-001` error without
/// leaking private filesystem paths, SQL parameters, or database contents.
///
/// Intentionally emits [`StorageBackendError::BackendInternal`] rather than
/// `WriteLockBusy` because `WriteLockBusy(Capture)` maps to `LBR-AGENT-045`,
/// whereas migration quiescence must fail closed with `LBR-IO-001` across all
/// database roles (`ADR-RB-07`).
fn map_quiesce_sqlite_error(
    role: DatabaseRole,
    stage: SqliteQuiesceStage,
    err: &sea_orm::sqlx::Error,
) -> StorageBackendError {
    if is_sqlite_busy_error(err) {
        return StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: active SQLite writer or reader holds a conflicting lock (SQLITE_BUSY)",
                stage.as_str()
            ),
        };
    }
    StorageBackendError::BackendInternal {
        detail: format!(
            "forward migration quiescence failed for {role} at {}",
            stage.as_str()
        ),
    }
}

/// A single non-pooled migration `SqliteConnection` (`c_mig`) holding an active
/// OS-level `EXCLUSIVE` lock in `DELETE` journal mode with `synchronous = FULL`
/// and `busy_timeout = 0` (`ADR-RB-07` #1).
pub struct QuiescedSqliteConnection {
    conn: SqliteConnection,
    role: DatabaseRole,
    path: PathBuf,
    journal_mode: String,
}

impl std::fmt::Debug for QuiescedSqliteConnection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("QuiescedSqliteConnection")
            .field("role", &self.role)
            .field("journal_mode", &self.journal_mode)
            .finish_non_exhaustive()
    }
}

impl QuiescedSqliteConnection {
    /// Database role owned by this quiesced connection.
    #[must_use]
    pub const fn role(&self) -> DatabaseRole {
        self.role
    }

    /// Database file path bound to this quiesced connection.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Verified SQLite journal mode after quiescence (always `"delete"`).
    #[must_use]
    pub fn journal_mode(&self) -> &str {
        &self.journal_mode
    }

    /// Mutable access to the sole non-pooled migration `SqliteConnection`.
    #[cfg(test)]
    pub(crate) fn connection_mut(&mut self) -> &mut SqliteConnection {
        &mut self.conn
    }

    /// Deterministically close `c_mig`, releasing SQLite's OS-level `EXCLUSIVE`
    /// lock synchronously before returning.
    pub async fn close(self) -> StorageResult<()> {
        let role = self.role;
        self.conn
            .close()
            .await
            .map_err(|err| StorageBackendError::BackendInternal {
                detail: format!(
                    "failed to close quiesced sqlite connection for {role} ({})",
                    if is_sqlite_busy_error(&err) {
                        "SQLITE_BUSY"
                    } else {
                        "close error"
                    }
                ),
            })
    }
}

/// Pair of quiesced non-pooled SQLite connections held for repository forward
/// migration (`Repository` always present; `Capture` present for Layouts B/C
/// and `None` for Layout A pre-split repositories).
#[derive(Debug)]
pub struct QuiescedRepositorySqliteSources {
    /// Quiesced connection to `<common_storage>/libra.db`.
    pub git: QuiescedSqliteConnection,
    /// Optional quiesced connection to `<common_storage>/private/capture.db`.
    pub capture: Option<QuiescedSqliteConnection>,
}

impl QuiescedRepositorySqliteSources {
    /// Deterministically close all held quiesced SQLite connections.
    pub async fn close(self) -> StorageResult<()> {
        let capture_res = match self.capture {
            Some(cap) => cap.close().await,
            None => Ok(()),
        };
        let git_res = self.git.close().await;
        capture_res?;
        git_res
    }
}

/// Open a single non-pooled `SqliteConnection` (`c_mig`) to `db_path`, configure
/// `busy_timeout = 0`, `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE = 1`,
/// `PRAGMA locking_mode = EXCLUSIVE`, and `PRAGMA synchronous = FULL`, quiesce
/// WAL mode (when present) by directly executing `PRAGMA journal_mode = DELETE;`
/// without any prior `wal_checkpoint(TRUNCATE)` (`RB-10m`), and execute a
/// no-write `BEGIN EXCLUSIVE; ROLLBACK;` in `DELETE` mode to prove zero
/// concurrent SQLite writers, active readers, or idle WAL connections
/// (`ADR-RB-07` #1, `RB-10`, `RB-10m`).
///
/// Strictly accepts only `DatabaseRole::Repository` and `DatabaseRole::Capture`
/// (rejecting `GlobalConfig`, `SystemConfig`, and `Derived` before path
/// inspection). Never opens raw `std::fs::File` descriptors on `db_path`, so
/// SQLite's own POSIX inode lock manager (`unixClose` / `pUnused`) preserves
/// live OS locks across re-entrant calls, hard-link path substitutions, and
/// asynchronous worker drops.
///
/// On any lock contention (`SQLITE_BUSY`) or invariant violation, `c_mig` is
/// explicitly closed before returning `Err` (`LBR-IO-001`) with zero byte
/// mutations to `db_path`, `-wal`, or `-shm`.
/// Canonical sidecar suffix for the durable WAL->DELETE stale `-shm` cleanup
/// resume marker written ONLY inside Phase B after all repository source locks
/// are held (`RB-10m`).
const WAL_SHM_CLEANUP_MARKER_SUFFIX: &str = "-libra-wal-shm-cleanup.pending";

/// Temporary staging suffix for atomic durable writes of the stale `-shm`
/// cleanup marker.
const WAL_SHM_CLEANUP_MARKER_TMP_SUFFIX: &str = "-libra-wal-shm-cleanup.pending.tmp";

/// Versioned header inside the durable WAL->DELETE stale `-shm` cleanup marker.
const WAL_SHM_CLEANUP_MARKER_HEADER: &str = "LIBRA_WAL_SHM_CLEANUP_V1";

/// Maximum accepted byte length when reading a stale `-shm` cleanup marker.
const WAL_SHM_CLEANUP_MARKER_MAX_BYTES: u64 = 512;

/// Parsed status of the durable WAL->DELETE stale `-shm` cleanup marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ShmCleanupMarkerState {
    /// No marker exists on disk.
    Absent,
    /// A valid marker matches the current regular non-symlink `-shm` identity.
    ValidForCurrentShm,
    /// A valid marker exists after `-shm` was already removed (crash before
    /// marker unlink).
    ValidStaleAfterShmGone,
}

/// Phase B finalization action decided strictly during non-mutating Phase A
/// exclusive lock acquisition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PostAcquireAction {
    /// Source is already in clean `DELETE` mode with no sidecars or marker.
    AlreadyCleanDelete,
    /// Source is already in clean `DELETE` mode with an obsolete marker left
    /// behind after a post-`-shm`-unlink crash.
    CleanupObsoleteMarkerOnly,
    /// Source transitioned to `DELETE` before a prior crash/error interrupted
    /// stale `-shm` removal; verified by the durable cleanup marker.
    ResumeStaleShmCleanup,
    /// Source is an offline non-`DELETE` (WAL) database holding OS EXCLUSIVE
    /// lock under verified `synchronous = FULL`, ready for direct
    /// `PRAGMA journal_mode = DELETE;`.
    TransitionOfflineWal,
}

/// Optional `#[cfg(test)]` fault-injection hooks for Phase A/B quiescence tests.
#[cfg(test)]
#[derive(Default)]
struct QuiesceTestHooks<'a> {
    before_begin: Option<&'a dyn Fn(&Path)>,
    after_begin: Option<fn(&Path)>,
    after_delete_before_shm_remove: Option<&'a dyn Fn(&Path) -> StorageResult<()>>,
    shm_remove_override: Option<&'a dyn Fn(&Path) -> io::Result<()>>,
}

/// Non-mutating Phase A lock state held across all repository sources before
/// Phase B executes any offline WAL->DELETE transition or stale `-shm` cleanup.
struct PreparedSqliteQuiescence {
    conn: SqliteConnection,
    role: DatabaseRole,
    path: PathBuf,
    expected_meta: fs::Metadata,
    action: PostAcquireAction,
}

impl PreparedSqliteQuiescence {
    async fn close_quietly(self) {
        let _ = self.conn.close().await;
    }
}

fn wal_shm_cleanup_marker_path(db_path: &Path) -> PathBuf {
    sqlite_sidecar_path(db_path, WAL_SHM_CLEANUP_MARKER_SUFFIX)
}

fn wal_shm_cleanup_marker_tmp_path(db_path: &Path) -> PathBuf {
    sqlite_sidecar_path(db_path, WAL_SHM_CLEANUP_MARKER_TMP_SUFFIX)
}

/// Durably fsync the parent directory of `path` (opens the directory only,
/// never any live `.db` file descriptor).
fn fsync_parent_dir(path: &Path) -> StorageResult<()> {
    let Some(parent) = path.parent() else {
        return Ok(());
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        let dir = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC)
            .open(parent)
            .map_err(|err| StorageBackendError::io("open parent directory for fsync", err))?;
        dir.sync_all()
            .map_err(|err| StorageBackendError::io("fsync parent directory", err))?;
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
    }
    Ok(())
}

/// Format the non-opening `lstat` identity of `shm_meta` into the versioned
/// cleanup marker payload (contains zero private paths or secrets).
fn format_wal_shm_cleanup_marker(shm_meta: &fs::Metadata) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        format!(
            "{WAL_SHM_CLEANUP_MARKER_HEADER}\nshm_len={}\nshm_dev={}\nshm_ino={}\n",
            shm_meta.len(),
            shm_meta.dev(),
            shm_meta.ino()
        )
    }
    #[cfg(not(unix))]
    {
        format!(
            "{WAL_SHM_CLEANUP_MARKER_HEADER}\nshm_len={}\n",
            shm_meta.len()
        )
    }
}

/// Atomically persist the WAL->DELETE stale `-shm` cleanup marker bound to the
/// current regular non-symlink `-shm` identity (via `lstat` only, zero raw fd
/// opened on `.db` or `-shm`).
fn write_wal_shm_cleanup_marker(
    db_path: &Path,
    shm_path: &Path,
    role: DatabaseRole,
) -> StorageResult<()> {
    let shm_meta = fs::symlink_metadata(shm_path).map_err(|err| {
        StorageBackendError::io("inspect sqlite -shm sidecar before marker persist", err)
    })?;
    if shm_meta.file_type().is_symlink() || !shm_meta.file_type().is_file() {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: -shm sidecar is not a regular non-symlink file",
                SqliteQuiesceStage::SwitchJournalModeDelete.as_str()
            ),
        });
    }

    let payload = format_wal_shm_cleanup_marker(&shm_meta);
    let marker_path = wal_shm_cleanup_marker_path(db_path);
    let tmp_path = wal_shm_cleanup_marker_tmp_path(db_path);

    if let Ok(existing_tmp) = fs::symlink_metadata(&tmp_path) {
        if existing_tmp.file_type().is_symlink() || !existing_tmp.file_type().is_file() {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected for {role} at {}: staging cleanup marker path is not a regular non-symlink file",
                    SqliteQuiesceStage::SwitchJournalModeDelete.as_str()
                ),
            });
        }
        let _ = fs::remove_file(&tmp_path);
    }

    let mut opts = fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        opts.mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC);
    }
    let mut file = opts.open(&tmp_path).map_err(|err| {
        StorageBackendError::io("create WAL->DELETE cleanup marker staging file", err)
    })?;
    use std::io::Write as _;
    if let Err(err) = file
        .write_all(payload.as_bytes())
        .and_then(|()| file.sync_data())
    {
        drop(file);
        let _ = fs::remove_file(&tmp_path);
        return Err(StorageBackendError::io(
            "write and fsync WAL->DELETE cleanup marker",
            err,
        ));
    }
    drop(file);

    if let Err(err) = fs::rename(&tmp_path, &marker_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(StorageBackendError::io(
            "publish WAL->DELETE cleanup marker",
            err,
        ));
    }
    fsync_parent_dir(&marker_path)?;
    Ok(())
}

/// Inspect the durable WAL->DELETE stale `-shm` cleanup marker without mutating
/// disk, verifying strict identity binding when `-shm` is still present.
fn inspect_wal_shm_cleanup_marker(
    db_path: &Path,
    shm_path: &Path,
    role: DatabaseRole,
) -> StorageResult<ShmCleanupMarkerState> {
    let marker_path = wal_shm_cleanup_marker_path(db_path);
    let marker_meta = match fs::symlink_metadata(&marker_path) {
        Ok(meta) => meta,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(ShmCleanupMarkerState::Absent);
        }
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect WAL->DELETE cleanup marker metadata",
                err,
            ));
        }
    };
    if marker_meta.file_type().is_symlink()
        || !marker_meta.file_type().is_file()
        || marker_meta.len() > WAL_SHM_CLEANUP_MARKER_MAX_BYTES
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: invalid WAL->DELETE cleanup marker file",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    }

    let contents = fs::read_to_string(&marker_path).map_err(|_| {
        StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: unreadable WAL->DELETE cleanup marker",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        }
    })?;
    let mut lines = contents.lines();
    if lines.next() != Some(WAL_SHM_CLEANUP_MARKER_HEADER) {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: unrecognized WAL->DELETE cleanup marker header",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    }

    let mut expected_len: Option<u64> = None;
    #[cfg(unix)]
    let mut expected_dev: Option<u64> = None;
    #[cfg(unix)]
    let mut expected_ino: Option<u64> = None;
    for line in lines {
        if let Some(v) = line.strip_prefix("shm_len=") {
            expected_len = v.parse::<u64>().ok();
        } else {
            #[cfg(unix)]
            if let Some(v) = line.strip_prefix("shm_dev=") {
                expected_dev = v.parse::<u64>().ok();
            } else if let Some(v) = line.strip_prefix("shm_ino=") {
                expected_ino = v.parse::<u64>().ok();
            } else {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected for {role} at {}: unexpected field in WAL->DELETE cleanup marker",
                        SqliteQuiesceStage::InspectJournalMode.as_str()
                    ),
                });
            }
            #[cfg(not(unix))]
            if !line.starts_with("shm_dev=") && !line.starts_with("shm_ino=") {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected for {role} at {}: unexpected field in WAL->DELETE cleanup marker",
                        SqliteQuiesceStage::InspectJournalMode.as_str()
                    ),
                });
            }
        }
    }

    let Some(expected_len) = expected_len else {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: malformed WAL->DELETE cleanup marker",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    };
    #[cfg(unix)]
    let (Some(expected_dev), Some(expected_ino)) = (expected_dev, expected_ino) else {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: incomplete Unix WAL->DELETE cleanup marker",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    };

    match fs::symlink_metadata(shm_path) {
        Ok(shm_meta) => {
            if shm_meta.file_type().is_symlink() || !shm_meta.file_type().is_file() {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected for {role} at {}: -shm sidecar is not a regular non-symlink file",
                        SqliteQuiesceStage::InspectJournalMode.as_str()
                    ),
                });
            }
            #[cfg(unix)]
            let matches = {
                use std::os::unix::fs::MetadataExt as _;
                shm_meta.len() == expected_len
                    && shm_meta.dev() == expected_dev
                    && shm_meta.ino() == expected_ino
            };
            #[cfg(not(unix))]
            let matches = shm_meta.len() == expected_len;
            if !matches {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected for {role} at {}: stray -shm sidecar does not match interrupted WAL->DELETE cleanup marker",
                        SqliteQuiesceStage::InspectJournalMode.as_str()
                    ),
                });
            }
            Ok(ShmCleanupMarkerState::ValidForCurrentShm)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            Ok(ShmCleanupMarkerState::ValidStaleAfterShmGone)
        }
        Err(err) => Err(StorageBackendError::io(
            "inspect sqlite -shm sidecar against cleanup marker",
            err,
        )),
    }
}

/// Remove the durable WAL->DELETE stale `-shm` cleanup marker once stale `-shm`
/// cleanup has completed.
fn remove_wal_shm_cleanup_marker(db_path: &Path) -> StorageResult<()> {
    let marker_path = wal_shm_cleanup_marker_path(db_path);
    match fs::remove_file(&marker_path) {
        Ok(()) => fsync_parent_dir(&marker_path),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(StorageBackendError::io(
            "remove WAL->DELETE cleanup marker",
            err,
        )),
    }
}

/// Classify a `DELETE`-mode database's sidecars and cleanup marker strictly
/// under the held OS `EXCLUSIVE` lock without mutating disk.
fn classify_delete_mode_post_action(
    db_path: &Path,
    wal_path: &Path,
    shm_path: &Path,
    role: DatabaseRole,
) -> StorageResult<PostAcquireAction> {
    let wal_exists = path_exists_no_open(wal_path)?;
    let shm_exists = path_exists_no_open(shm_path)?;
    if wal_exists {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: unexpected -wal sidecar present alongside DELETE-mode database",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    }
    let marker = inspect_wal_shm_cleanup_marker(db_path, shm_path, role)?;
    match (shm_exists, marker) {
        (false, ShmCleanupMarkerState::Absent) => Ok(PostAcquireAction::AlreadyCleanDelete),
        (false, ShmCleanupMarkerState::ValidStaleAfterShmGone) => {
            Ok(PostAcquireAction::CleanupObsoleteMarkerOnly)
        }
        (true, ShmCleanupMarkerState::ValidForCurrentShm) => {
            Ok(PostAcquireAction::ResumeStaleShmCleanup)
        }
        (true, ShmCleanupMarkerState::Absent) => Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: unexpected -shm sidecar present alongside DELETE-mode database",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        }),
        _ => Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: inconsistent -shm sidecar and cleanup marker state",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        }),
    }
}

/// Open a single non-pooled `SqliteConnection` (`c_mig`) to `db_path`, configure
/// `busy_timeout = 0`, `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE = 1`,
/// `PRAGMA locking_mode = EXCLUSIVE`, and `PRAGMA synchronous = FULL`, quiesce
/// WAL mode (when present) by directly executing `PRAGMA journal_mode = DELETE;`
/// without any prior `wal_checkpoint(TRUNCATE)` (`RB-10m`), and execute a
/// no-write `BEGIN EXCLUSIVE; ROLLBACK;` in `DELETE` mode to prove zero
/// concurrent SQLite writers, active readers, or idle WAL connections
/// (`ADR-RB-07` #1, `RB-10`, `RB-10m`).
///
/// Strictly accepts only `DatabaseRole::Repository` and `DatabaseRole::Capture`
/// (rejecting `GlobalConfig`, `SystemConfig`, and `Derived` before path
/// inspection). Never opens raw `std::fs::File` descriptors on `db_path`, so
/// SQLite's own POSIX inode lock manager (`unixClose` / `pUnused`) preserves
/// live OS locks across re-entrant calls, hard-link path substitutions, and
/// asynchronous worker drops.
///
/// On any lock contention (`SQLITE_BUSY`) or invariant violation, `c_mig` is
/// explicitly closed before returning `Err` (`LBR-IO-001`) with zero byte
/// mutations to `db_path`, `-wal`, or `-shm`.
pub async fn quiesce_sqlite_source(
    db_path: &Path,
    role: DatabaseRole,
) -> StorageResult<QuiescedSqliteConnection> {
    validate_repository_forward_role(role)?;
    let expected_meta = validate_sqlite_file_metadata(db_path, role)?;

    quiesce_sqlite_connection_only(db_path, role, &expected_meta).await
}

/// Open the sole non-pooled `SqliteConnection` and run Phase A (non-mutating
/// exclusive lock acquisition) followed by Phase B (offline WAL->DELETE
/// transition / stale `-shm` cleanup) with zero raw `File::open` descriptors
/// touched on `db_path`.
async fn quiesce_sqlite_connection_only(
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
) -> StorageResult<QuiescedSqliteConnection> {
    #[cfg(test)]
    {
        quiesce_sqlite_connection_only_with_hooks(
            db_path,
            role,
            expected_meta,
            &QuiesceTestHooks::default(),
        )
        .await
    }
    #[cfg(not(test))]
    {
        let prepared = prepare_sqlite_quiescence_no_mutate(db_path, role, expected_meta).await?;
        finalize_prepared_sqlite_quiescence(prepared).await
    }
}

#[cfg(test)]
async fn quiesce_sqlite_connection_only_with_hooks(
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
    hooks: &QuiesceTestHooks<'_>,
) -> StorageResult<QuiescedSqliteConnection> {
    let prepared =
        prepare_sqlite_quiescence_no_mutate_inner(db_path, role, expected_meta, hooks).await?;
    finalize_prepared_sqlite_quiescence_inner(prepared, hooks).await
}

/// Phase A: acquire and retain the OS-level `EXCLUSIVE` lock on `db_path` with
/// ZERO bytes modified on `.db`, `-wal`, or `-shm`, verify `synchronous = FULL`
/// and inode binding, and classify the required Phase B transition.
#[cfg(not(test))]
async fn prepare_sqlite_quiescence_no_mutate(
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
) -> StorageResult<PreparedSqliteQuiescence> {
    prepare_sqlite_quiescence_no_mutate_inner(db_path, role, expected_meta).await
}

async fn prepare_sqlite_quiescence_no_mutate_inner(
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
    #[cfg(test)] hooks: &QuiesceTestHooks<'_>,
) -> StorageResult<PreparedSqliteQuiescence> {
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        .read_only(false)
        .shared_cache(false)
        .busy_timeout(Duration::ZERO);

    let mut conn = SqliteConnection::connect_with(&options)
        .await
        .map_err(|err| map_quiesce_sqlite_error(role, SqliteQuiesceStage::Connect, &err))?;

    let action_res = prepare_open_connection_no_mutate(
        &mut conn,
        db_path,
        role,
        expected_meta,
        #[cfg(test)]
        hooks,
    )
    .await;
    match action_res {
        Ok(action) => Ok(PreparedSqliteQuiescence {
            conn,
            role,
            path: db_path.to_path_buf(),
            expected_meta: expected_meta.clone(),
            action,
        }),
        Err(err) => {
            let _ = conn.close().await;
            Err(err)
        }
    }
}

/// Bind `conn`'s live `"main"` descriptor to `expected_meta` via non-opening
/// `lstat` + `SQLITE_FCNTL_HAS_MOVED` before any mutating operation and again
/// after lock acquisition.
async fn verify_bound_source_identity(
    conn: &mut SqliteConnection,
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
) -> StorageResult<()> {
    let current_meta = validate_sqlite_file_metadata(db_path, role)?;
    if !same_file_identity(db_path, expected_meta, db_path, &current_meta)? {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: source sqlite path identity changed during open",
                SqliteQuiesceStage::PathValidation.as_str()
            ),
        });
    }
    verify_connection_not_moved(conn, role).await
}

/// Explicitly configure and verify `PRAGMA synchronous = FULL;` (`2`) on `conn`.
async fn configure_and_verify_synchronous_full(
    conn: &mut SqliteConnection,
    role: DatabaseRole,
) -> StorageResult<()> {
    conn.execute("PRAGMA synchronous = FULL;")
        .await
        .map_err(|err| {
            map_quiesce_sqlite_error(role, SqliteQuiesceStage::ConfigurePragmas, &err)
        })?;

    let sync_level: i64 = sea_orm::sqlx::query_scalar("PRAGMA synchronous;")
        .fetch_one(&mut *conn)
        .await
        .map_err(|err| {
            map_quiesce_sqlite_error(role, SqliteQuiesceStage::ConfigurePragmas, &err)
        })?;
    if sync_level != 2 {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence failed for {role} at {}: expected synchronous=FULL (2), got {sync_level}",
                SqliteQuiesceStage::ConfigurePragmas.as_str()
            ),
        });
    }
    Ok(())
}

/// Phase A non-mutating acquisition when `-wal` or `-shm` already exists on
/// disk (under `PRAGMA locking_mode = EXCLUSIVE` + `NO_CKPT_ON_CLOSE = 1`).
async fn prepare_with_existing_wal_sidecars(
    conn: &mut SqliteConnection,
    db_path: &Path,
    wal_path: &Path,
    shm_path: &Path,
    role: DatabaseRole,
    #[cfg(test)] hooks: &QuiesceTestHooks<'_>,
) -> StorageResult<PostAcquireAction> {
    // Because `PRAGMA locking_mode = EXCLUSIVE` is already active, SQLite
    // opens any WAL database in `WAL_HEAPMEMORY_MODE` (zero `-shm` access) and
    // calls `pagerExclusiveLock` inside `pagerOpenWal` on the very first
    // schema-reading pragma, failing immediately with `SQLITE_BUSY` on any
    // active WAL reader, active WAL writer, or idle WAL connection with ZERO
    // bytes modified on `.db`, `-wal`, or `-shm`.
    configure_and_verify_synchronous_full(conn, role).await?;
    let prior_mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
        .fetch_one(&mut *conn)
        .await
        .map_err(|err| {
            map_quiesce_sqlite_error(role, SqliteQuiesceStage::InspectJournalMode, &err)
        })?;
    if prior_mode.eq_ignore_ascii_case("wal") {
        // Distinguish a genuine WAL database (header format version 2/2) from
        // an already-DELETE database (header 1/1) that merely has a stray `-wal`
        // file on disk (SQLite's `pagerOpenWalIfPresent()` reports `"wal"`
        // whenever `-wal` exists even on a 1/1 DELETE database!).
        if !read_sqlite_header_is_wal(conn, role).await? {
            return Err(StorageBackendError::BackendInternal {
                detail: format!(
                    "forward migration quiescence rejected for {role} at {}: unexpected -wal sidecar present alongside DELETE-mode database",
                    SqliteQuiesceStage::InspectJournalMode.as_str()
                ),
            });
        }
        // Offline WAL database: OS EXCLUSIVE lock is already acquired and
        // retained by `pagerOpenWal -> pagerExclusiveLock` under
        // `WAL_HEAPMEMORY_MODE` with zero bytes written to disk and
        // `NO_CKPT_ON_CLOSE = 1` protecting abort invariance.
        return Ok(PostAcquireAction::TransitionOfflineWal);
    }

    // Non-WAL database (e.g. already `DELETE` with a stray/resumable sidecar):
    // schema read only took `SHARED`, so explicitly acquire and retain OS
    // `EXCLUSIVE` via no-write `BEGIN EXCLUSIVE; ROLLBACK;` before classifying
    // sidecars so any external `DELETE` reader/writer fails closed immediately.
    conn.execute("BEGIN EXCLUSIVE;")
        .await
        .map_err(|err| map_quiesce_sqlite_error(role, SqliteQuiesceStage::BeginExclusive, &err))?;
    #[cfg(test)]
    if let Some(hook) = hooks.after_begin {
        hook(db_path);
    }
    conn.execute("ROLLBACK;")
        .await
        .map_err(|err| map_quiesce_sqlite_error(role, SqliteQuiesceStage::RollbackProbe, &err))?;

    if prior_mode.eq_ignore_ascii_case("delete") {
        classify_delete_mode_post_action(db_path, wal_path, shm_path, role)
    } else {
        Ok(PostAcquireAction::TransitionOfflineWal)
    }
}

/// Phase A core on an already opened non-pooled `SqliteConnection`: acquires
/// and retains the OS `EXCLUSIVE` lock without modifying `.db`, `-wal`, or
/// `-shm` (`ADR-RB-07` #1, `RB-10`, `RB-10m`).
async fn prepare_open_connection_no_mutate(
    conn: &mut SqliteConnection,
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
    #[cfg(test)] hooks: &QuiesceTestHooks<'_>,
) -> StorageResult<PostAcquireAction> {
    let wal_path = sqlite_sidecar_path(db_path, "-wal");
    let shm_path = sqlite_sidecar_path(db_path, "-shm");
    let wal_sidecar_present = path_exists_no_open(&wal_path)? || path_exists_no_open(&shm_path)?;

    // Configure close-time WAL checkpoint guard BEFORE any lock-acquiring
    // statement:
    // - When `-wal`/`-shm` already exist on disk, set `NO_CKPT_ON_CLOSE = 1`
    //   so any rejected or pair-aborted attempt closes `c_mig` with zero
    //   checkpointing or sidecar mutation.
    // - When no WAL sidecar exists yet, keep `NO_CKPT_ON_CLOSE = 0` so if
    //   `BEGIN EXCLUSIVE` opens a cleanly-closed WAL database (creating an
    //   empty 0-byte `-wal` under `WAL_HEAPMEMORY_MODE`) and is aborted before
    //   Phase B, `sqlite3WalClose` unlinks that empty 0-byte `-wal` with zero
    //   writes to `.db`.
    set_wal_checkpoint_on_close(conn, role, wal_sidecar_present).await?;

    conn.execute("PRAGMA busy_timeout = 0;")
        .await
        .map_err(|err| {
            map_quiesce_sqlite_error(role, SqliteQuiesceStage::ConfigurePragmas, &err)
        })?;

    verify_bound_source_identity(conn, db_path, role, expected_meta).await?;

    // Set `PRAGMA locking_mode = EXCLUSIVE` BEFORE any schema-reading or
    // lock-acquiring statement (`PragTyp_LOCKING_MODE` does not read page 1,
    // forces `WAL_HEAPMEMORY_MODE` / zero `-shm` access, and retains OS
    // `EXCLUSIVE` lock across `ROLLBACK`).
    let locking_mode: String = sea_orm::sqlx::query_scalar("PRAGMA locking_mode = EXCLUSIVE;")
        .fetch_one(&mut *conn)
        .await
        .map_err(|err| {
            map_quiesce_sqlite_error(role, SqliteQuiesceStage::ConfigurePragmas, &err)
        })?;
    if !locking_mode.eq_ignore_ascii_case("exclusive") {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence failed for {role} at {}: expected locking_mode=exclusive, got `{locking_mode}`",
                SqliteQuiesceStage::ConfigurePragmas.as_str()
            ),
        });
    }

    let action = if wal_sidecar_present {
        prepare_with_existing_wal_sidecars(
            conn,
            db_path,
            &wal_path,
            &shm_path,
            role,
            #[cfg(test)]
            hooks,
        )
        .await?
    } else {
        #[cfg(test)]
        if let Some(hook) = hooks.before_begin {
            hook(db_path);
        }

        if path_exists_no_open(&wal_path)? || path_exists_no_open(&shm_path)? {
            set_wal_checkpoint_on_close(conn, role, true).await?;
            prepare_with_existing_wal_sidecars(
                conn,
                db_path,
                &wal_path,
                &shm_path,
                role,
                #[cfg(test)]
                hooks,
            )
            .await?
        } else {
            match conn.execute("BEGIN EXCLUSIVE;").await {
                Ok(_) => {}
                Err(err) => {
                    if is_sqlite_busy_error(&err)
                        && (path_exists_no_open(&wal_path)? || path_exists_no_open(&shm_path)?)
                    {
                        set_wal_checkpoint_on_close(conn, role, true).await?;
                        return Err(map_quiesce_sqlite_error(
                            role,
                            SqliteQuiesceStage::SwitchJournalModeDelete,
                            &err,
                        ));
                    }
                    return Err(map_quiesce_sqlite_error(
                        role,
                        SqliteQuiesceStage::BeginExclusive,
                        &err,
                    ));
                }
            }

            if fs::symlink_metadata(&wal_path).is_ok_and(|meta| meta.len() > 0) {
                set_wal_checkpoint_on_close(conn, role, true).await?;
            }

            #[cfg(test)]
            if let Some(hook) = hooks.after_begin {
                hook(db_path);
            }

            let journal_mode_res: Result<String, sea_orm::sqlx::Error> =
                sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
                    .fetch_one(&mut *conn)
                    .await;
            let rollback_res = conn.execute("ROLLBACK;").await;

            let journal_mode = journal_mode_res.map_err(|err| {
                map_quiesce_sqlite_error(role, SqliteQuiesceStage::InspectJournalMode, &err)
            })?;
            rollback_res.map_err(|err| {
                map_quiesce_sqlite_error(role, SqliteQuiesceStage::RollbackProbe, &err)
            })?;

            configure_and_verify_synchronous_full(conn, role).await?;

            if journal_mode.eq_ignore_ascii_case("delete") {
                classify_delete_mode_post_action(db_path, &wal_path, &shm_path, role)?
            } else {
                if journal_mode.eq_ignore_ascii_case("wal")
                    && !read_sqlite_header_is_wal(conn, role).await?
                {
                    return Err(StorageBackendError::BackendInternal {
                        detail: format!(
                            "forward migration quiescence rejected for {role} at {}: unexpected -wal sidecar present alongside DELETE-mode database",
                            SqliteQuiesceStage::InspectJournalMode.as_str()
                        ),
                    });
                }
                PostAcquireAction::TransitionOfflineWal
            }
        }
    };

    verify_bound_source_identity(conn, db_path, role, expected_meta).await?;
    Ok(action)
}

/// Phase B: after ALL repository sources hold OS `EXCLUSIVE` locks with zero
/// contention, execute any required offline WAL->DELETE transition or resumed
/// stale `-shm` cleanup and return the ready `DELETE`-mode connection.
#[cfg(not(test))]
async fn finalize_prepared_sqlite_quiescence(
    prepared: PreparedSqliteQuiescence,
) -> StorageResult<QuiescedSqliteConnection> {
    finalize_prepared_sqlite_quiescence_inner(prepared).await
}

async fn finalize_prepared_sqlite_quiescence_inner(
    mut prepared: PreparedSqliteQuiescence,
    #[cfg(test)] hooks: &QuiesceTestHooks<'_>,
) -> StorageResult<QuiescedSqliteConnection> {
    let wal_path = sqlite_sidecar_path(&prepared.path, "-wal");
    let shm_path = sqlite_sidecar_path(&prepared.path, "-shm");
    let result: StorageResult<()> = async {
        match prepared.action {
            PostAcquireAction::AlreadyCleanDelete => {}
            PostAcquireAction::CleanupObsoleteMarkerOnly => {
                verify_bound_source_identity(
                    &mut prepared.conn,
                    &prepared.path,
                    prepared.role,
                    &prepared.expected_meta,
                )
                .await?;
                remove_wal_shm_cleanup_marker(&prepared.path)?;
            }
            PostAcquireAction::ResumeStaleShmCleanup => {
                verify_bound_source_identity(
                    &mut prepared.conn,
                    &prepared.path,
                    prepared.role,
                    &prepared.expected_meta,
                )
                .await?;
                remove_stale_shm_under_exclusive_lock(
                    &wal_path,
                    &shm_path,
                    prepared.role,
                    #[cfg(test)]
                    hooks.shm_remove_override,
                )?;
                remove_wal_shm_cleanup_marker(&prepared.path)?;
                verify_bound_source_identity(
                    &mut prepared.conn,
                    &prepared.path,
                    prepared.role,
                    &prepared.expected_meta,
                )
                .await?;
            }
            PostAcquireAction::TransitionOfflineWal => {
                verify_bound_source_identity(
                    &mut prepared.conn,
                    &prepared.path,
                    prepared.role,
                    &prepared.expected_meta,
                )
                .await?;

                let had_shm = path_exists_no_open(&shm_path)?;
                if had_shm {
                    write_wal_shm_cleanup_marker(&prepared.path, &shm_path, prepared.role)?;
                }

                switch_journal_mode_to_delete_directly(
                    &mut prepared.conn,
                    &wal_path,
                    prepared.role,
                )
                .await?;

                prepared
                    .conn
                    .execute("BEGIN EXCLUSIVE;")
                    .await
                    .map_err(|err| {
                        map_quiesce_sqlite_error(
                            prepared.role,
                            SqliteQuiesceStage::BeginExclusive,
                            &err,
                        )
                    })?;
                prepared.conn.execute("ROLLBACK;").await.map_err(|err| {
                    map_quiesce_sqlite_error(prepared.role, SqliteQuiesceStage::RollbackProbe, &err)
                })?;

                #[cfg(test)]
                if let Some(hook) = hooks.after_delete_before_shm_remove {
                    hook(&prepared.path)?;
                }

                remove_stale_shm_under_exclusive_lock(
                    &wal_path,
                    &shm_path,
                    prepared.role,
                    #[cfg(test)]
                    hooks.shm_remove_override,
                )?;
                remove_wal_shm_cleanup_marker(&prepared.path)?;
                verify_bound_source_identity(
                    &mut prepared.conn,
                    &prepared.path,
                    prepared.role,
                    &prepared.expected_meta,
                )
                .await?;
            }
        }
        Ok(())
    }
    .await;

    match result {
        Ok(()) => Ok(QuiescedSqliteConnection {
            conn: prepared.conn,
            role: prepared.role,
            path: prepared.path,
            journal_mode: "delete".to_string(),
        }),
        Err(err) => {
            let _ = prepared.conn.close().await;
            Err(err)
        }
    }
}

/// Directly switch a WAL (or non-DELETE) database to `DELETE` mode via
/// `PRAGMA journal_mode = DELETE;` without any prior `wal_checkpoint(...)`,
/// verifying that SQLite returns `"delete"` and that `-wal` is no longer
/// present on disk (`ADR-RB-07` #1, `RB-10m`).
///
/// Stale `-shm` removal is intentionally deferred to
/// [`remove_stale_shm_under_exclusive_lock`] AFTER `BEGIN EXCLUSIVE; ROLLBACK;`
/// proves the OS `EXCLUSIVE` lock is held.
async fn switch_journal_mode_to_delete_directly(
    conn: &mut SqliteConnection,
    wal_path: &Path,
    role: DatabaseRole,
) -> StorageResult<()> {
    let switched: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode = DELETE;")
        .fetch_one(&mut *conn)
        .await
        .map_err(|err| {
            map_quiesce_sqlite_error(role, SqliteQuiesceStage::SwitchJournalModeDelete, &err)
        })?;
    if !switched.eq_ignore_ascii_case("delete") {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: expected journal_mode=delete, got `{switched}`",
                SqliteQuiesceStage::SwitchJournalModeDelete.as_str()
            ),
        });
    }
    if path_exists_no_open(wal_path)? {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: -wal sidecar remained present after switching to DELETE",
                SqliteQuiesceStage::SwitchJournalModeDelete.as_str()
            ),
        });
    }
    Ok(())
}

/// Remove a regular non-symlink post-crash stale `-shm` file strictly AFTER
/// `PRAGMA journal_mode = DELETE;` AND `BEGIN EXCLUSIVE; ROLLBACK;` have both
/// succeeded (proving `c_mig` holds the OS `EXCLUSIVE` lock on `.db` in
/// `DELETE` mode and `-wal` is absent).
fn remove_stale_shm_under_exclusive_lock(
    wal_path: &Path,
    shm_path: &Path,
    role: DatabaseRole,
    #[cfg(test)] shm_remove_override: Option<&dyn Fn(&Path) -> io::Result<()>>,
) -> StorageResult<()> {
    if path_exists_no_open(wal_path)? {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: -wal sidecar remained present after DELETE lock acquisition",
                SqliteQuiesceStage::SwitchJournalModeDelete.as_str()
            ),
        });
    }
    match fs::symlink_metadata(shm_path) {
        Ok(meta) => {
            if meta.file_type().is_symlink() || !meta.file_type().is_file() {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected for {role} at {}: stale -shm path is not a regular non-symlink file",
                        SqliteQuiesceStage::SwitchJournalModeDelete.as_str()
                    ),
                });
            }
            #[cfg(test)]
            let remove_res = match shm_remove_override {
                Some(override_fn) => override_fn(shm_path),
                None => fs::remove_file(shm_path),
            };
            #[cfg(not(test))]
            let remove_res = fs::remove_file(shm_path);
            if let Err(err) = remove_res
                && err.kind() != io::ErrorKind::NotFound
            {
                return Err(StorageBackendError::io(
                    "remove stale sqlite -shm sidecar after DELETE transition",
                    err,
                ));
            }
            fsync_parent_dir(shm_path)?;
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect sqlite -shm sidecar after DELETE transition",
                err,
            ));
        }
    }
    if path_exists_no_open(wal_path)? || path_exists_no_open(shm_path)? {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: WAL sidecar remained present after switching to DELETE",
                SqliteQuiesceStage::SwitchJournalModeDelete.as_str()
            ),
        });
    }
    Ok(())
}

/// Quiesce the repository's SQLite source databases (`<common_storage>/libra.db`
/// and, when present, `<common_storage>/private/capture.db`) using single
/// non-pooled connections (`c_mig`).
///
/// Acquires non-mutating OS `EXCLUSIVE` quiescence on **both** sources first
/// (Phase A) before performing any offline WAL->DELETE transition or stale
/// `-shm` cleanup (Phase B), so contention on `capture.db` can never mutate or
/// checkpoint an offline WAL `libra.db` (and vice versa).
///
/// Never opens raw `std::fs::File` descriptors on live `.db` paths and rejects
/// hard-linked `libra.db`/`capture.db` aliases up front via `lstat`. Strictly
/// scoped to `DatabaseRole::Repository` and `DatabaseRole::Capture`: never
/// opens, creates, or modifies `GlobalConfig` or `SystemConfig` stores.
pub async fn quiesce_forward_repository_sqlite_sources(
    common_storage: &Path,
) -> StorageResult<QuiescedRepositorySqliteSources> {
    let storage_meta = fs::symlink_metadata(common_storage)
        .map_err(|err| StorageBackendError::io("inspect repository storage directory", err))?;
    if storage_meta.file_type().is_symlink() || !storage_meta.file_type().is_dir() {
        return Err(StorageBackendError::BackendInternal {
            detail: "forward migration quiescence rejected: storage root must be a non-symlink directory"
                .to_string(),
        });
    }

    let git_path = git_sqlite_db_path(common_storage);
    let git_meta = validate_sqlite_file_metadata(&git_path, DatabaseRole::Repository)?;

    let capture_path = capture_sqlite_db_path(common_storage);
    let capture_meta = match fs::symlink_metadata(&capture_path) {
        Ok(_) => {
            let cap_meta = validate_sqlite_file_metadata(&capture_path, DatabaseRole::Capture)?;
            if same_file_identity(&git_path, &git_meta, &capture_path, &cap_meta)? {
                return Err(StorageBackendError::BackendInternal {
                    detail: format!(
                        "forward migration quiescence rejected for {} at {}: capture database aliases repository database inode",
                        DatabaseRole::Capture,
                        SqliteQuiesceStage::PathValidation.as_str()
                    ),
                });
            }
            Some(cap_meta)
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => None,
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect capture sqlite database metadata",
                err,
            ));
        }
    };

    // Phase A: acquire non-mutating OS EXCLUSIVE quiescence on BOTH sources
    // before mutating or transitioning either source.
    #[cfg(test)]
    let default_hooks = QuiesceTestHooks::default();
    #[cfg(test)]
    let git_prepared = prepare_sqlite_quiescence_no_mutate_inner(
        &git_path,
        DatabaseRole::Repository,
        &git_meta,
        &default_hooks,
    )
    .await?;
    #[cfg(not(test))]
    let git_prepared =
        prepare_sqlite_quiescence_no_mutate(&git_path, DatabaseRole::Repository, &git_meta).await?;

    let cap_prepared = if let Some(cap_meta) = capture_meta {
        #[cfg(test)]
        let res = prepare_sqlite_quiescence_no_mutate_inner(
            &capture_path,
            DatabaseRole::Capture,
            &cap_meta,
            &default_hooks,
        )
        .await;
        #[cfg(not(test))]
        let res =
            prepare_sqlite_quiescence_no_mutate(&capture_path, DatabaseRole::Capture, &cap_meta)
                .await;
        match res {
            Ok(prepared) => Some(prepared),
            Err(err) => {
                git_prepared.close_quietly().await;
                return Err(err);
            }
        }
    } else {
        None
    };

    // Phase B: zero contention across all repository sources -> finalize both.
    #[cfg(test)]
    let git = match finalize_prepared_sqlite_quiescence_inner(git_prepared, &default_hooks).await {
        Ok(conn) => conn,
        Err(err) => {
            if let Some(cap) = cap_prepared {
                cap.close_quietly().await;
            }
            return Err(err);
        }
    };
    #[cfg(not(test))]
    let git = match finalize_prepared_sqlite_quiescence(git_prepared).await {
        Ok(conn) => conn,
        Err(err) => {
            if let Some(cap) = cap_prepared {
                cap.close_quietly().await;
            }
            return Err(err);
        }
    };

    let capture = if let Some(cap_prepared) = cap_prepared {
        #[cfg(test)]
        let res = finalize_prepared_sqlite_quiescence_inner(cap_prepared, &default_hooks).await;
        #[cfg(not(test))]
        let res = finalize_prepared_sqlite_quiescence(cap_prepared).await;
        match res {
            Ok(conn) => Some(conn),
            Err(err) => {
                let _ = git.close().await;
                return Err(err);
            }
        }
    } else {
        None
    };

    Ok(QuiescedRepositorySqliteSources { git, capture })
}

#[cfg(test)]
mod tests {
    use std::{
        io::{BufRead, BufReader, Read, Write},
        process::{Child, ChildStdin, ChildStdout, Command, Stdio},
        time::Instant,
    };

    use sea_orm::sqlx::sqlite::SqliteSynchronous;
    use tempfile::tempdir;

    use super::*;

    const RB10_CHILD_MODE_ENV: &str = "LIBRA_RB10_CHILD_MODE";
    const RB10_CHILD_DB_ENV: &str = "LIBRA_RB10_CHILD_DB";
    const RB10_READY_MARKER: &str = "LIBRA_RB10_READY";
    const RB10_BUSY_MARKER: &str = "LIBRA_RB10_BUSY_OK";

    /// RAII guard ensuring spawned external SQLite lock-holder children are
    /// always terminated and reaped even when a test assertion fails.
    struct ChildLockHolder {
        child: Option<Child>,
        stdin: Option<ChildStdin>,
        stdout: Option<BufReader<ChildStdout>>,
    }

    impl ChildLockHolder {
        fn spawn(mode: &str, db_path: &Path) -> Self {
            let exe = std::env::current_exe().expect("locate current test executable");
            let mut child = Command::new(exe)
                .arg("--exact")
                .arg("storage::migrate::tests::forward_phase1_rejects_writer_and_delete_active_reader")
                .arg("--nocapture")
                .env(RB10_CHILD_MODE_ENV, mode)
                .env(RB10_CHILD_DB_ENV, db_path)
                .env("LIBRA_TEST", "1")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn external sqlite lock-holder child");

            let stdin = child.stdin.take().expect("capture child stdin");
            let stdout = child.stdout.take().expect("capture child stdout");
            let mut reader = BufReader::new(stdout);
            let mut saw_ready = false;
            let mut captured = String::new();
            loop {
                let mut line = String::new();
                let bytes = reader
                    .read_line(&mut line)
                    .expect("read external child stdout");
                if bytes == 0 {
                    break;
                }
                captured.push_str(&line);
                if line.trim() == RB10_READY_MARKER {
                    saw_ready = true;
                    break;
                }
            }
            if !saw_ready {
                let mut stderr_buf = String::new();
                if let Some(mut stderr) = child.stderr.take() {
                    let _ = stderr.read_to_string(&mut stderr_buf);
                }
                let status = child.wait().expect("wait for failed child");
                panic!(
                    "external child mode `{mode}` exited before emitting {RB10_READY_MARKER} (status={status:?})\nstdout:\n{captured}\nstderr:\n{stderr_buf}"
                );
            }

            Self {
                child: Some(child),
                stdin: Some(stdin),
                stdout: Some(reader),
            }
        }

        fn release_and_wait(mut self) {
            // Closing stdin signals the child to ROLLBACK and exit cleanly.
            drop(self.stdin.take());
            let mut trailing_stdout = String::new();
            if let Some(mut reader) = self.stdout.take() {
                let _ = reader.read_to_string(&mut trailing_stdout);
            }
            let output = self
                .child
                .take()
                .expect("child handle must be present")
                .wait_with_output()
                .expect("wait for external child exit");
            assert!(
                output.status.success(),
                "external lock-holder child exited abnormally ({:?}):\nstdout:\n{trailing_stdout}\nstderr:\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    impl Drop for ChildLockHolder {
        fn drop(&mut self) {
            drop(self.stdin.take());
            drop(self.stdout.take());
            if let Some(child) = self.child.as_mut()
                && let Ok(None) = child.try_wait()
            {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    fn run_child_busy_probe(mode: &str, db_path: &Path) {
        let exe = std::env::current_exe().expect("locate current test executable");
        let output = Command::new(exe)
            .arg("--exact")
            .arg("storage::migrate::tests::forward_phase1_rejects_writer_and_delete_active_reader")
            .arg("--nocapture")
            .env(RB10_CHILD_MODE_ENV, mode)
            .env(RB10_CHILD_DB_ENV, db_path)
            .env("LIBRA_TEST", "1")
            .output()
            .expect("spawn external sqlite probe child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success() && stdout.contains(RB10_BUSY_MARKER),
            "external probe `{mode}` did not confirm SQLITE_BUSY (status={:?}):\nstdout:\n{stdout}\nstderr:\n{stderr}",
            output.status
        );
    }

    async fn run_rb10_child_mode(mode: &str, db_path: &Path) {
        let opts = SqliteConnectOptions::new()
            .filename(db_path)
            .create_if_missing(false)
            .read_only(false)
            .busy_timeout(Duration::ZERO);
        let mut conn = SqliteConnection::connect_with(&opts)
            .await
            .expect("child connect sqlite");

        match mode {
            "writer_immediate" => {
                conn.execute("BEGIN IMMEDIATE;")
                    .await
                    .expect("child BEGIN IMMEDIATE");
                conn.execute(
                    "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/inflight', 'deadbeef');",
                )
                .await
                .expect("child INSERT inside write transaction");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let _ = tokio::task::spawn_blocking(|| {
                    let mut sink = Vec::new();
                    let _ = io::stdin().read_to_end(&mut sink);
                })
                .await;
                conn.execute("ROLLBACK;").await.expect("child ROLLBACK");
            }
            "writer_exclusive" => {
                conn.execute("BEGIN EXCLUSIVE;")
                    .await
                    .expect("child BEGIN EXCLUSIVE");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let _ = tokio::task::spawn_blocking(|| {
                    let mut sink = Vec::new();
                    let _ = io::stdin().read_to_end(&mut sink);
                })
                .await;
                conn.execute("ROLLBACK;").await.expect("child ROLLBACK");
            }
            "delete_reader" => {
                conn.execute("BEGIN;").await.expect("child BEGIN");
                let count: i64 = sea_orm::sqlx::query_scalar("SELECT count(*) FROM reference;")
                    .fetch_one(&mut conn)
                    .await
                    .expect("child SELECT holding SHARED lock");
                assert!(count >= 1, "expected seeded row in fixture database");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let _ = tokio::task::spawn_blocking(|| {
                    let mut sink = Vec::new();
                    let _ = io::stdin().read_to_end(&mut sink);
                })
                .await;
                conn.execute("ROLLBACK;").await.expect("child ROLLBACK");
            }
            "wal_active_reader" => {
                let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
                    .fetch_one(&mut conn)
                    .await
                    .expect("child inspect WAL journal_mode");
                assert_eq!(mode.to_ascii_lowercase(), "wal");
                conn.execute("PRAGMA wal_autocheckpoint = 0;")
                    .await
                    .expect("child disable wal_autocheckpoint");
                // Commit one pending frame into `-wal` (without checkpointing
                // into main `.db`) so `-wal` and `-shm` both contain real live
                // data before starting the active read transaction.
                conn.execute(
                    "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/wal-pending-reader', '1111111111111111111111111111111111111111');",
                )
                .await
                .expect("child commit pending WAL row");
                conn.execute("BEGIN;").await.expect("child WAL BEGIN");
                let count: i64 = sea_orm::sqlx::query_scalar("SELECT count(*) FROM reference;")
                    .fetch_one(&mut conn)
                    .await
                    .expect("child WAL active reader SELECT");
                assert!(count >= 2, "expected seeded + pending WAL rows");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let _ = tokio::task::spawn_blocking(|| {
                    let mut sink = Vec::new();
                    let _ = io::stdin().read_to_end(&mut sink);
                })
                .await;
                conn.execute("ROLLBACK;").await.expect("child WAL ROLLBACK");
            }
            "wal_active_writer" => {
                let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
                    .fetch_one(&mut conn)
                    .await
                    .expect("child inspect WAL journal_mode");
                assert_eq!(mode.to_ascii_lowercase(), "wal");
                conn.execute("PRAGMA wal_autocheckpoint = 0;")
                    .await
                    .expect("child disable wal_autocheckpoint");
                conn.execute(
                    "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/wal-pending-writer', '2222222222222222222222222222222222222222');",
                )
                .await
                .expect("child commit pending WAL row");
                conn.execute("BEGIN IMMEDIATE;")
                    .await
                    .expect("child WAL BEGIN IMMEDIATE");
                conn.execute(
                    "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/wal-inflight-write', '3333333333333333333333333333333333333333');",
                )
                .await
                .expect("child WAL active INSERT");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let _ = tokio::task::spawn_blocking(|| {
                    let mut sink = Vec::new();
                    let _ = io::stdin().read_to_end(&mut sink);
                })
                .await;
                conn.execute("ROLLBACK;").await.expect("child WAL ROLLBACK");
            }
            "wal_idle_conn" => {
                let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
                    .fetch_one(&mut conn)
                    .await
                    .expect("child inspect WAL journal_mode");
                assert_eq!(mode.to_ascii_lowercase(), "wal");
                conn.execute("PRAGMA wal_autocheckpoint = 0;")
                    .await
                    .expect("child disable wal_autocheckpoint");
                // Commit pending WAL frames and then execute an autocommit read
                // so the connection sits 100% IDLE (no open transaction) while
                // keeping uncheckpointed frames in `-wal` and `-shm`.
                conn.execute(
                    "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/wal-pending-idle', '4444444444444444444444444444444444444444');",
                )
                .await
                .expect("child commit pending WAL row");
                let count: i64 = sea_orm::sqlx::query_scalar("SELECT count(*) FROM reference;")
                    .fetch_one(&mut conn)
                    .await
                    .expect("child WAL autocommit SELECT");
                assert!(count >= 2, "expected seeded + pending WAL rows");
                println!("{RB10_READY_MARKER}");
                let _ = io::stdout().flush();
                let _ = tokio::task::spawn_blocking(|| {
                    let mut sink = Vec::new();
                    let _ = io::stdin().read_to_end(&mut sink);
                })
                .await;
            }
            "probe_read_busy" => {
                let err = sea_orm::sqlx::query_scalar::<_, i64>("SELECT count(*) FROM reference;")
                    .fetch_one(&mut conn)
                    .await
                    .expect_err("external read must be blocked by held EXCLUSIVE lock");
                assert!(
                    is_sqlite_busy_error(&err),
                    "expected SQLITE_BUSY for external read, got {err:?}"
                );
                println!("{RB10_BUSY_MARKER}");
                let _ = io::stdout().flush();
            }
            "probe_write_busy" => {
                let err = conn
                    .execute("BEGIN IMMEDIATE;")
                    .await
                    .expect_err("external write must be blocked by held EXCLUSIVE lock");
                assert!(
                    is_sqlite_busy_error(&err),
                    "expected SQLITE_BUSY for external write, got {err:?}"
                );
                println!("{RB10_BUSY_MARKER}");
                let _ = io::stdout().flush();
            }
            other => panic!("unknown LIBRA_RB10_CHILD_MODE: {other}"),
        }

        conn.close().await.expect("child close sqlite");
    }

    async fn create_delete_mode_fixture_db(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create sqlite fixture parent");
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .read_only(false)
            .busy_timeout(Duration::from_secs(5))
            .synchronous(SqliteSynchronous::Full);
        let mut conn = SqliteConnection::connect_with(&opts)
            .await
            .expect("create fixture sqlite db");
        let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode = DELETE;")
            .fetch_one(&mut conn)
            .await
            .expect("set DELETE journal mode");
        assert_eq!(mode.to_ascii_lowercase(), "delete");
        conn.execute(
            "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, commit_id TEXT NOT NULL);",
        )
        .await
        .expect("create fixture table");
        conn.execute(
            "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/main', '0123456789abcdef0123456789abcdef01234567');",
        )
        .await
        .expect("seed fixture row");
        conn.close().await.expect("close fixture sqlite db");
    }

    #[tokio::test]
    async fn forward_phase1_rejects_writer_and_delete_active_reader() {
        if let Some(mode) = std::env::var_os(RB10_CHILD_MODE_ENV) {
            let db_path = PathBuf::from(
                std::env::var_os(RB10_CHILD_DB_ENV).expect("LIBRA_RB10_CHILD_DB must be set"),
            );
            run_rb10_child_mode(&mode.to_string_lossy(), &db_path).await;
            return;
        }

        let dir = tempdir().expect("create temp repo storage");
        let storage = dir.path().join(".libra");
        let git_db = git_sqlite_db_path(&storage);
        let capture_db = capture_sqlite_db_path(&storage);

        create_delete_mode_fixture_db(&git_db).await;
        create_delete_mode_fixture_db(&capture_db).await;

        let baseline_git_bytes = fs::read(&git_db).expect("read baseline libra.db");
        let baseline_cap_bytes = fs::read(&capture_db).expect("read baseline capture.db");

        // AC1 & AC2: External OS process holding an active write transaction
        // (BEGIN IMMEDIATE + INSERT -> RESERVED lock) on `libra.db` must be
        // rejected at BEGIN EXCLUSIVE with LBR-IO-001 and zero byte mutation.
        {
            let holder = ChildLockHolder::spawn("writer_immediate", &git_db);
            let before_git = fs::read(&git_db).expect("snapshot libra.db before gate");
            let before_cap = fs::read(&capture_db).expect("snapshot capture.db before gate");

            let err = quiesce_forward_repository_sqlite_sources(&storage)
                .await
                .expect_err("active external writer on libra.db must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            let rendered = err.to_string();
            assert!(
                rendered.contains("BEGIN EXCLUSIVE") && rendered.contains("SQLITE_BUSY"),
                "expected BEGIN EXCLUSIVE SQLITE_BUSY diagnostic, got: {rendered}"
            );
            assert!(
                !rendered.contains(&git_db.to_string_lossy().into_owned()),
                "error message must not leak private db path: {rendered}"
            );

            let after_git = fs::read(&git_db).expect("snapshot libra.db after gate");
            let after_cap = fs::read(&capture_db).expect("snapshot capture.db after gate");
            assert_eq!(
                before_git, after_git,
                "AC2: libra.db bytes must be identical before and after writer rejection"
            );
            assert_eq!(
                before_cap, after_cap,
                "capture.db bytes must remain identical after libra.db writer rejection"
            );

            holder.release_and_wait();
            assert_eq!(
                fs::read(&git_db).expect("read libra.db after child rollback"),
                baseline_git_bytes
            );
        }

        // AC1 & AC2 (exclusive writer variant on `libra.db`): must also fail
        // specifically at `BEGIN EXCLUSIVE` with `SQLITE_BUSY` -> `LBR-IO-001`.
        {
            let holder = ChildLockHolder::spawn("writer_exclusive", &git_db);
            let before_git = fs::read(&git_db).expect("snapshot libra.db before exclusive gate");

            let err = quiesce_forward_repository_sqlite_sources(&storage)
                .await
                .expect_err("exclusive external writer on libra.db must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            let rendered = err.to_string();
            assert!(
                rendered.contains("BEGIN EXCLUSIVE") && rendered.contains("SQLITE_BUSY"),
                "expected BEGIN EXCLUSIVE SQLITE_BUSY diagnostic for exclusive writer, got: {rendered}"
            );

            let after_git = fs::read(&git_db).expect("snapshot libra.db after exclusive gate");
            assert_eq!(
                before_git, after_git,
                "AC2: libra.db bytes must be identical before and after exclusive writer rejection"
            );
            holder.release_and_wait();
        }

        // AC1 & AC2 on `capture.db` (proves Capture role also fails closed with
        // LBR-IO-001 rather than LBR-AGENT-045, and cleanly releases git c_mig).
        {
            let holder = ChildLockHolder::spawn("writer_immediate", &capture_db);
            let before_git = fs::read(&git_db).expect("snapshot libra.db before capture writer");
            let before_cap =
                fs::read(&capture_db).expect("snapshot capture.db before capture writer");

            let err = quiesce_forward_repository_sqlite_sources(&storage)
                .await
                .expect_err("active external writer on capture.db must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            let rendered = err.to_string();
            assert!(
                rendered.contains("BEGIN EXCLUSIVE") && rendered.contains("SQLITE_BUSY"),
                "expected BEGIN EXCLUSIVE SQLITE_BUSY diagnostic for capture.db, got: {rendered}"
            );

            let after_git = fs::read(&git_db).expect("snapshot libra.db after capture writer");
            let after_cap =
                fs::read(&capture_db).expect("snapshot capture.db after capture writer");
            assert_eq!(before_git, after_git);
            assert_eq!(before_cap, after_cap);

            holder.release_and_wait();
        }

        // AC3 & AC4: External OS process holding an active DELETE-mode read
        // transaction (SHARED lock) on `libra.db` must be rejected at
        // BEGIN EXCLUSIVE with LBR-IO-001 and zero byte mutation.
        {
            let holder = ChildLockHolder::spawn("delete_reader", &git_db);
            let before_git = fs::read(&git_db).expect("snapshot libra.db before reader gate");
            let before_cap = fs::read(&capture_db).expect("snapshot capture.db before reader gate");

            let err = quiesce_forward_repository_sqlite_sources(&storage)
                .await
                .expect_err("active DELETE-mode reader on libra.db must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            let rendered = err.to_string();
            assert!(
                rendered.contains("BEGIN EXCLUSIVE") && rendered.contains("SQLITE_BUSY"),
                "expected BEGIN EXCLUSIVE SQLITE_BUSY diagnostic for reader, got: {rendered}"
            );

            let after_git = fs::read(&git_db).expect("snapshot libra.db after reader gate");
            let after_cap = fs::read(&capture_db).expect("snapshot capture.db after reader gate");
            assert_eq!(
                before_git, after_git,
                "AC4: libra.db bytes must be identical before and after DELETE reader rejection"
            );
            assert_eq!(before_cap, after_cap);

            holder.release_and_wait();
        }

        // AC3 & AC4 on `capture.db` as well.
        {
            let holder = ChildLockHolder::spawn("delete_reader", &capture_db);
            let before_git = fs::read(&git_db).expect("snapshot libra.db before capture reader");
            let before_cap =
                fs::read(&capture_db).expect("snapshot capture.db before capture reader");

            let err = quiesce_forward_repository_sqlite_sources(&storage)
                .await
                .expect_err("active DELETE-mode reader on capture.db must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            let rendered = err.to_string();
            assert!(
                rendered.contains("BEGIN EXCLUSIVE") && rendered.contains("SQLITE_BUSY"),
                "expected BEGIN EXCLUSIVE SQLITE_BUSY diagnostic for capture reader, got: {rendered}"
            );

            let after_git = fs::read(&git_db).expect("snapshot libra.db after capture reader");
            let after_cap =
                fs::read(&capture_db).expect("snapshot capture.db after capture reader");
            assert_eq!(before_git, after_git);
            assert_eq!(before_cap, after_cap);

            holder.release_and_wait();
        }

        // Controlled path-substitution regressions (`SQLITE_FCNTL_HAS_MOVED` +
        // post-lock identity binding): both persistent rename AND
        // swap-and-swap-back between pre-open metadata validation and
        // `connect_with` must fail closed with `LBR-IO-001`.
        {
            let sub_dir = tempdir().expect("create substitution temp dir");
            let target_db = sub_dir.path().join("target.db");
            let decoy_db = sub_dir.path().join("decoy.db");
            let backup_target = sub_dir.path().join("target.orig.db");
            create_delete_mode_fixture_db(&target_db).await;
            create_delete_mode_fixture_db(&decoy_db).await;

            // Persistent substitution: validate original `target_db`, then
            // swap `decoy_db` into `target_db` before SQLite opens it.
            let orig_meta = validate_sqlite_file_metadata(&target_db, DatabaseRole::Repository)
                .expect("validate original target_db");
            fs::rename(&target_db, &backup_target).expect("move original target aside");
            fs::rename(&decoy_db, &target_db).expect("substitute decoy at target path");

            let sub_err =
                quiesce_sqlite_connection_only(&target_db, DatabaseRole::Repository, &orig_meta)
                    .await
                    .expect_err("persistent path substitution must fail closed");
            assert_eq!(sub_err.stable_code_str(), "LBR-IO-001");

            // Swap-and-swap-back: SQLite opens `decoy` (currently at
            // `target_db`), then in `after_begin_hook` the original file is
            // moved back to `target_db` so pre-lstat == post-lstat, while
            // `SQLITE_FCNTL_HAS_MOVED` detects that SQLite's live fd does
            // NOT match the inode at `target_db`!
            fn restore_original_hook(path: &Path) {
                let parent = path.parent().expect("target parent");
                let backup = parent.join("target.orig.db");
                let moved_decoy = parent.join("decoy.moved.db");
                fs::rename(path, &moved_decoy).expect("move decoy aside");
                fs::rename(&backup, path).expect("restore original inode at target path");
            }
            let swap_back_err = quiesce_sqlite_connection_only_with_hooks(
                &target_db,
                DatabaseRole::Repository,
                &orig_meta,
                &QuiesceTestHooks {
                    after_begin: Some(restore_original_hook),
                    ..Default::default()
                },
            )
            .await
            .expect_err("swap-and-swap-back substitution must be caught by SQLITE_FCNTL_HAS_MOVED");
            assert_eq!(swap_back_err.stable_code_str(), "LBR-IO-001");
        }

        // Lock-safety regressions (zero raw File::open -> SQLite's unixClose
        // preserves POSIX locks across every re-entrant call, hard-link alias,
        // path substitution, and asynchronous Drop shutdown):
        {
            let alias_dir = tempdir().expect("create hard-link alias temp dir");
            let alias_storage = alias_dir.path().join(".libra");
            let alias_git = git_sqlite_db_path(&alias_storage);
            let alias_cap = capture_sqlite_db_path(&alias_storage);
            create_delete_mode_fixture_db(&alias_git).await;
            fs::create_dir_all(alias_cap.parent().expect("private parent"))
                .expect("create private dir for alias fixture");
            fs::hard_link(&alias_git, &alias_cap).expect("hard-link capture.db to libra.db");
            let before_alias = fs::read(&alias_git).expect("read aliased db before");

            // 1. Repository batch rejects aliased hard-linked capture.db/libra.db up front.
            let alias_err = quiesce_forward_repository_sqlite_sources(&alias_storage)
                .await
                .expect_err("aliased hard-linked capture.db/libra.db must fail closed");
            assert_eq!(alias_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(fs::read(&alias_git).expect("read aliased db"), before_alias);

            // 2. Retain a live guard on `alias_git` and invoke a second
            //    `quiesce_sqlite_source` on BOTH the same path AND a hard-link
            //    substituted path: both fail closed at BEGIN EXCLUSIVE, and
            //    external OS probes confirm `first_guard`'s POSIX EXCLUSIVE
            //    lock remains 100% intact.
            let first_guard = quiesce_sqlite_source(&alias_git, DatabaseRole::Repository)
                .await
                .expect("first standalone quiesce must succeed");

            let dup_same_err = quiesce_sqlite_source(&alias_git, DatabaseRole::Repository)
                .await
                .expect_err("second quiesce on same live path must fail closed");
            assert_eq!(dup_same_err.stable_code_str(), "LBR-IO-001");

            let swapped_path = alias_dir.path().join("substituted.db");
            fs::hard_link(&alias_git, &swapped_path).expect("hard-link substituted path");
            let dup_hardlink_err = quiesce_sqlite_source(&swapped_path, DatabaseRole::Capture)
                .await
                .expect_err("second quiesce on hard-linked substituted path must fail closed");
            assert_eq!(dup_hardlink_err.stable_code_str(), "LBR-IO-001");

            run_child_busy_probe("probe_read_busy", &alias_git);
            run_child_busy_probe("probe_write_busy", &alias_git);

            // 3. Asynchronous Drop (without awaiting `.close()`): drop
            //    `first_guard`, re-acquire `second_guard` once worker releases,
            //    and confirm via external OS probes that `first_guard`'s async
            //    worker shutdown NEVER releases `second_guard`'s POSIX lock.
            drop(first_guard);
            let deadline = Instant::now() + Duration::from_secs(2);
            let second_guard = loop {
                match quiesce_sqlite_source(&alias_git, DatabaseRole::Repository).await {
                    Ok(guard) => break guard,
                    Err(_) if Instant::now() < deadline => {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                    Err(err) => panic!(
                        "failed to re-acquire quiesce lock after async drop within 2s: {err}"
                    ),
                }
            };
            run_child_busy_probe("probe_read_busy", &alias_git);
            run_child_busy_probe("probe_write_busy", &alias_git);
            second_guard
                .close()
                .await
                .expect("close second standalone quiesce");

            // 4. Role-scope zero-touch regression: GlobalConfig, SystemConfig,
            //    and Derived must all be rejected before touching disk.
            for forbidden_role in [
                DatabaseRole::GlobalConfig,
                DatabaseRole::SystemConfig,
                DatabaseRole::Derived,
            ] {
                let role_err = quiesce_sqlite_source(&alias_git, forbidden_role)
                    .await
                    .expect_err("non-repository/capture role must fail closed");
                assert_eq!(role_err.stable_code_str(), "LBR-IO-001");
            }
            assert_eq!(
                fs::read(&alias_git).expect("read fixture after forbidden role rejections"),
                before_alias
            );
        }

        // Positive control: once all external readers/writers are gone, the
        // quiescence gate succeeds within the 50ms uncontended budget, retains
        // OS-level EXCLUSIVE locks across `BEGIN EXCLUSIVE; ROLLBACK;`, and
        // leaves source files untouched.
        let mut samples = Vec::with_capacity(5);
        for _ in 0..5 {
            let started = Instant::now();
            let q = quiesce_forward_repository_sqlite_sources(&storage)
                .await
                .expect("uncontended DELETE-mode quiescence must succeed");
            samples.push(started.elapsed());
            q.close().await.expect("close timed quiesced connections");
        }
        samples.sort();
        let median_elapsed = samples[samples.len() / 2];
        assert!(
            median_elapsed <= Duration::from_millis(50),
            "RB-10 performance budget exceeded: median uncontended quiescence {median_elapsed:?} > 50ms (samples={samples:?})"
        );

        let mut quiesced = quiesce_forward_repository_sqlite_sources(&storage)
            .await
            .expect("uncontended DELETE-mode quiescence must succeed");
        assert_eq!(quiesced.git.role(), DatabaseRole::Repository);
        assert_eq!(quiesced.git.path(), git_db.as_path());
        assert_eq!(quiesced.git.journal_mode(), "delete");
        let cap = quiesced
            .capture
            .as_mut()
            .expect("capture.db must be quiesced when present");
        assert_eq!(cap.role(), DatabaseRole::Capture);
        assert_eq!(cap.path(), capture_db.as_path());
        assert_eq!(cap.journal_mode(), "delete");

        // Confirm single-connection read works through `connection_mut()`.
        let row_count: i64 = sea_orm::sqlx::query_scalar("SELECT count(*) FROM reference;")
            .fetch_one(quiesced.git.connection_mut())
            .await
            .expect("read through held c_mig");
        assert_eq!(row_count, 1);

        // Confirm OS-level EXCLUSIVE locks remain held across ROLLBACK by
        // probing both databases from independent OS child processes.
        run_child_busy_probe("probe_read_busy", &git_db);
        run_child_busy_probe("probe_write_busy", &git_db);
        run_child_busy_probe("probe_read_busy", &capture_db);
        run_child_busy_probe("probe_write_busy", &capture_db);

        quiesced.close().await.expect("close quiesced connections");

        assert_eq!(
            fs::read(&git_db).expect("read libra.db after quiesce close"),
            baseline_git_bytes,
            "uncontended DELETE-mode quiescence must not mutate libra.db"
        );
        assert_eq!(
            fs::read(&capture_db).expect("read capture.db after quiesce close"),
            baseline_cap_bytes,
            "uncontended DELETE-mode quiescence must not mutate capture.db"
        );
    }

    async fn create_wal_mode_fixture_db(path: &Path) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create sqlite WAL fixture parent");
        }
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .read_only(false)
            .busy_timeout(Duration::from_secs(5))
            .synchronous(SqliteSynchronous::Full);
        let mut conn = SqliteConnection::connect_with(&opts)
            .await
            .expect("create WAL fixture sqlite db");
        let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode = WAL;")
            .fetch_one(&mut conn)
            .await
            .expect("set WAL journal mode");
        assert_eq!(mode.to_ascii_lowercase(), "wal");
        conn.execute(
            "CREATE TABLE reference (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL, commit_id TEXT NOT NULL);",
        )
        .await
        .expect("create WAL fixture table");
        conn.execute(
            "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/main', '0123456789abcdef0123456789abcdef01234567');",
        )
        .await
        .expect("seed WAL fixture row");
        conn.close().await.expect("close WAL fixture sqlite db");
    }

    #[tokio::test]
    async fn forward_phase1_rejects_wal_active_reader_writer_and_idle_conn() {
        // Test all three WAL contention classes (AC1-AC2 active WAL reader,
        // AC3-AC4 active WAL writer `BEGIN IMMEDIATE; INSERT ...`, AC5-AC6
        // open idle WAL connection) on BOTH `libra.db` (`Repository`) and
        // `private/capture.db` (`Capture`).
        for (mode, label) in [
            ("wal_active_reader", "AC1/AC2 WAL active reader"),
            ("wal_active_writer", "AC3/AC4 WAL active writer"),
            ("wal_idle_conn", "AC5/AC6 WAL idle connection"),
        ] {
            for target_capture in [false, true] {
                let dir = tempdir().expect("create temp WAL repo storage");
                let storage = dir.path().join(".libra");
                let git_db = git_sqlite_db_path(&storage);
                let capture_db = capture_sqlite_db_path(&storage);

                if target_capture {
                    create_delete_mode_fixture_db(&git_db).await;
                    create_wal_mode_fixture_db(&capture_db).await;
                } else {
                    create_wal_mode_fixture_db(&git_db).await;
                    create_delete_mode_fixture_db(&capture_db).await;
                }

                let target_db = if target_capture { &capture_db } else { &git_db };
                let other_db = if target_capture { &git_db } else { &capture_db };
                let target_wal = sqlite_sidecar_path(target_db, "-wal");
                let target_shm = sqlite_sidecar_path(target_db, "-shm");

                let holder = ChildLockHolder::spawn(mode, target_db);

                // Verify live WAL sidecars genuinely exist and contain pending
                // uncheckpointed frames + WAL index before calling the gate.
                let before_db = fs::read(target_db).expect("snapshot WAL .db before gate");
                let before_wal = fs::read(&target_wal).expect("snapshot -wal before gate");
                let before_shm = fs::read(&target_shm).expect("snapshot -shm before gate");
                let before_other = fs::read(other_db).expect("snapshot peer .db before gate");
                assert!(
                    !before_wal.is_empty(),
                    "{label}: expected non-empty -wal with pending frames before gate"
                );
                assert!(
                    !before_shm.is_empty(),
                    "{label}: expected non-empty -shm before gate"
                );

                let err = quiesce_forward_repository_sqlite_sources(&storage)
                    .await
                    .expect_err("contended WAL source must fail closed");
                assert_eq!(
                    err.stable_code_str(),
                    "LBR-IO-001",
                    "{label} (capture={target_capture}): must return LBR-IO-001"
                );
                let rendered = err.to_string();
                assert!(
                    rendered.contains("SQLITE_BUSY"),
                    "{label} (capture={target_capture}): expected SQLITE_BUSY diagnostic, got: {rendered}"
                );
                assert!(
                    !rendered.contains(&target_db.to_string_lossy().into_owned()),
                    "{label}: error message must not leak private db path: {rendered}"
                );

                // Strict 3-file byte-for-byte zero-mutation assertions (AC2, AC4, AC6).
                let after_db = fs::read(target_db).expect("snapshot WAL .db after gate");
                let after_wal = fs::read(&target_wal).expect("snapshot -wal after gate");
                let after_shm = fs::read(&target_shm).expect("snapshot -shm after gate");
                let after_other = fs::read(other_db).expect("snapshot peer .db after gate");
                assert_eq!(
                    before_db, after_db,
                    "{label} (capture={target_capture}): .db bytes must be identical before and after rejection"
                );
                assert_eq!(
                    before_wal, after_wal,
                    "{label} (capture={target_capture}): -wal bytes must be identical before and after rejection (no pre-checkpoint)"
                );
                assert_eq!(
                    before_shm, after_shm,
                    "{label} (capture={target_capture}): -shm bytes must be identical before and after rejection"
                );
                assert_eq!(
                    before_other, after_other,
                    "{label} (capture={target_capture}): peer .db bytes must remain identical"
                );

                holder.release_and_wait();
            }
        }

        // Positive control: once zero external WAL connections exist (genuine
        // offline WAL with pending frames in `-wal`), direct
        // `PRAGMA journal_mode = DELETE;` succeeds within the 50ms budget,
        // checkpoints all pending WAL frames into standalone `.db`, removes
        // `-wal` and `-shm`, and retains the OS EXCLUSIVE lock.
        {
            let dir = tempdir().expect("create offline WAL positive control dir");
            let builder_db = dir.path().join("builder.db");
            create_wal_mode_fixture_db(&builder_db).await;

            // Write an uncheckpointed row into `-wal` while keeping builder open,
            // then copy `.db` + `-wal` into our test repository as a genuine
            // offline WAL database with zero open connections.
            let builder_opts = SqliteConnectOptions::new()
                .filename(&builder_db)
                .create_if_missing(false)
                .read_only(false)
                .busy_timeout(Duration::from_secs(5))
                .synchronous(SqliteSynchronous::Full);
            let mut bconn = SqliteConnection::connect_with(&builder_opts)
                .await
                .expect("reopen WAL builder");
            bconn
                .execute("PRAGMA wal_autocheckpoint = 0;")
                .await
                .expect("disable autocheckpoint");
            bconn
                .execute(
                    "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/offline-wal-only', '5555555555555555555555555555555555555555');",
                )
                .await
                .expect("insert offline WAL pending row");

            let storage = dir.path().join(".libra");
            fs::create_dir_all(&storage).expect("create .libra dir");
            let offline_git = git_sqlite_db_path(&storage);
            let offline_wal = sqlite_sidecar_path(&offline_git, "-wal");
            let offline_shm = sqlite_sidecar_path(&offline_git, "-shm");
            // Copy ALL THREE files (`.db`, `-wal`, AND stale `-shm`) to
            // faithfully simulate a post-crash offline WAL database with zero
            // active connections (R3 stale `-shm` regression).
            fs::copy(&builder_db, &offline_git).expect("copy offline main db");
            fs::copy(sqlite_sidecar_path(&builder_db, "-wal"), &offline_wal)
                .expect("copy offline -wal");
            fs::copy(sqlite_sidecar_path(&builder_db, "-shm"), &offline_shm)
                .expect("copy offline stale -shm");
            bconn.close().await.expect("close WAL builder");
            assert!(
                fs::metadata(&offline_wal)
                    .expect("offline -wal metadata")
                    .len()
                    > 0
            );
            assert!(
                fs::metadata(&offline_shm)
                    .expect("offline -shm metadata")
                    .len()
                    > 0
            );

            let started = Instant::now();
            let mut quiesced = quiesce_forward_repository_sqlite_sources(&storage)
                .await
                .expect(
                    "offline WAL quiescence via direct PRAGMA journal_mode = DELETE must succeed",
                );
            let elapsed = started.elapsed();
            assert!(
                elapsed <= Duration::from_millis(50),
                "RB-10m offline WAL quiescence exceeded 50ms budget: {elapsed:?}"
            );
            assert_eq!(quiesced.git.journal_mode(), "delete");
            assert!(
                !offline_wal.exists(),
                "offline WAL quiescence must eliminate -wal"
            );
            assert!(
                !offline_shm.exists(),
                "offline WAL quiescence must eliminate -shm"
            );

            let count: i64 = sea_orm::sqlx::query_scalar(
                "SELECT count(*) FROM reference WHERE name = 'refs/heads/offline-wal-only';",
            )
            .fetch_one(quiesced.git.connection_mut())
            .await
            .expect("verify pending WAL row flushed into main .db");
            assert_eq!(count, 1);

            // Verify OS-level EXCLUSIVE lock is held after WAL -> DELETE transition.
            run_child_busy_probe("probe_read_busy", &offline_git);
            run_child_busy_probe("probe_write_busy", &offline_git);

            quiesced
                .close()
                .await
                .expect("close quiesced WAL->DELETE db");
        }

        // R1 regressions:
        // 1. Race where an external WAL connection opens a cleanly-closed WAL
        //    database AFTER the initial no-sidecar check and BEFORE `BEGIN EXCLUSIVE`:
        //    must route to direct `PRAGMA journal_mode = DELETE;` (reporting
        //    `PRAGMA journal_mode = DELETE` + `SQLITE_BUSY` -> `LBR-IO-001`)
        //    with `.db`, `-wal`, and `-shm` 100% byte-identical.
        {
            let race_dir = tempdir().expect("create raced WAL dir");
            let clean_wal_db = race_dir.path().join("clean_wal.db");
            let clean_wal_sidecar = sqlite_sidecar_path(&clean_wal_db, "-wal");
            let clean_shm_sidecar = sqlite_sidecar_path(&clean_wal_db, "-shm");
            create_wal_mode_fixture_db(&clean_wal_db).await;
            assert!(!clean_wal_sidecar.exists());
            assert!(!clean_shm_sidecar.exists());

            struct WalTriple {
                db: Vec<u8>,
                wal: Vec<u8>,
                shm: Vec<u8>,
            }
            let orig_meta = validate_sqlite_file_metadata(&clean_wal_db, DatabaseRole::Repository)
                .expect("validate clean WAL metadata");
            let holder_slot: std::sync::Mutex<Option<ChildLockHolder>> =
                std::sync::Mutex::new(None);
            let before_slot: std::sync::Mutex<Option<WalTriple>> = std::sync::Mutex::new(None);
            let spawn_wal_race = |db: &Path| {
                let holder = ChildLockHolder::spawn("wal_idle_conn", db);
                let b_db = fs::read(db).expect("read raced .db");
                let b_wal = fs::read(sqlite_sidecar_path(db, "-wal")).expect("read raced -wal");
                let b_shm = fs::read(sqlite_sidecar_path(db, "-shm")).expect("read raced -shm");
                *before_slot.lock().expect("lock before_slot") = Some(WalTriple {
                    db: b_db,
                    wal: b_wal,
                    shm: b_shm,
                });
                *holder_slot.lock().expect("lock holder_slot") = Some(holder);
            };

            let race_err = quiesce_sqlite_connection_only_with_hooks(
                &clean_wal_db,
                DatabaseRole::Repository,
                &orig_meta,
                &QuiesceTestHooks {
                    before_begin: Some(&spawn_wal_race),
                    ..Default::default()
                },
            )
            .await
            .expect_err("raced external WAL opener must fail closed");
            assert_eq!(race_err.stable_code_str(), "LBR-IO-001");
            let rendered = race_err.to_string();
            assert!(
                rendered.contains("SQLITE_BUSY"),
                "raced WAL opener must fail with SQLITE_BUSY, got: {rendered}"
            );
            let before = before_slot
                .lock()
                .expect("lock before_slot")
                .take()
                .expect("before snapshot recorded");
            assert_eq!(
                fs::read(&clean_wal_db).expect("read .db after race"),
                before.db
            );
            assert_eq!(
                fs::read(&clean_wal_sidecar).expect("read -wal after race"),
                before.wal
            );
            assert_eq!(
                fs::read(&clean_shm_sidecar).expect("read -shm after race"),
                before.shm
            );
            holder_slot
                .lock()
                .expect("lock holder_slot")
                .take()
                .expect("holder present")
                .release_and_wait();
        }

        // 2. Cancellation immediately after `BEGIN EXCLUSIVE` on a cleanly-closed
        //    WAL database (no initial sidecars) must still unlink any empty
        //    0-byte `-wal` opened by `sqlite3WalOpen`, leaving `.db` identical
        //    and neither `-wal` nor `-shm` behind.
        {
            let cancel_dir = tempdir().expect("create clean WAL cancel dir");
            let clean_wal_db = cancel_dir.path().join("clean_cancel_wal.db");
            let clean_wal_sidecar = sqlite_sidecar_path(&clean_wal_db, "-wal");
            let clean_shm_sidecar = sqlite_sidecar_path(&clean_wal_db, "-shm");
            create_wal_mode_fixture_db(&clean_wal_db).await;
            assert!(!clean_wal_sidecar.exists());
            assert!(!clean_shm_sidecar.exists());
            let before_db = fs::read(&clean_wal_db).expect("read clean WAL db before cancel");

            let opts = SqliteConnectOptions::new()
                .filename(&clean_wal_db)
                .create_if_missing(false)
                .read_only(false)
                .shared_cache(false)
                .busy_timeout(Duration::ZERO);
            let mut cancel_conn = SqliteConnection::connect_with(&opts)
                .await
                .expect("open cleanly-closed WAL for cancel test");
            set_wal_checkpoint_on_close(&mut cancel_conn, DatabaseRole::Repository, false)
                .await
                .expect("keep NO_CKPT_ON_CLOSE=0 when no sidecar existed");
            let lm: String = sea_orm::sqlx::query_scalar("PRAGMA locking_mode = EXCLUSIVE;")
                .fetch_one(&mut cancel_conn)
                .await
                .expect("set exclusive locking_mode");
            assert_eq!(lm.to_ascii_lowercase(), "exclusive");
            cancel_conn
                .execute("BEGIN EXCLUSIVE;")
                .await
                .expect("BEGIN EXCLUSIVE opens empty -wal under WAL_HEAPMEMORY_MODE");
            drop(cancel_conn);
            let deadline = Instant::now() + Duration::from_secs(2);
            while clean_wal_sidecar.exists() && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
            assert!(
                !clean_wal_sidecar.exists(),
                "cancellation after BEGIN EXCLUSIVE on clean WAL must unlink empty -wal"
            );
            assert!(
                !clean_shm_sidecar.exists(),
                "cancellation after BEGIN EXCLUSIVE on clean WAL must not create -shm"
            );
            assert_eq!(
                fs::read(&clean_wal_db).expect("read clean WAL db after cancel"),
                before_db
            );
        }

        // 3. WAL path-substitution before `PRAGMA journal_mode = DELETE;`:
        //    must reject via `verify_bound_source_identity` BEFORE any
        //    checkpoint or mode switch can mutate the substituted WAL database.
        {
            let sub_dir = tempdir().expect("create WAL substitution dir");
            let orig_db = sub_dir.path().join("orig.db");
            let decoy_builder = sub_dir.path().join("decoy_builder.db");
            let decoy_wal_db = sub_dir.path().join("decoy_wal.db");
            let decoy_wal_sidecar = sqlite_sidecar_path(&decoy_wal_db, "-wal");
            create_delete_mode_fixture_db(&orig_db).await;
            create_wal_mode_fixture_db(&decoy_builder).await;
            {
                let b_opts = SqliteConnectOptions::new()
                    .filename(&decoy_builder)
                    .create_if_missing(false)
                    .read_only(false)
                    .busy_timeout(Duration::from_secs(5))
                    .synchronous(SqliteSynchronous::Full);
                let mut bconn = SqliteConnection::connect_with(&b_opts)
                    .await
                    .expect("open decoy WAL builder");
                bconn
                    .execute("PRAGMA wal_autocheckpoint = 0;")
                    .await
                    .expect("disable decoy autocheckpoint");
                bconn
                    .execute(
                        "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/decoy-wal', '6666666666666666666666666666666666666666');",
                    )
                    .await
                    .expect("insert decoy pending WAL row");
                fs::copy(&decoy_builder, &decoy_wal_db).expect("copy decoy main db");
                fs::copy(
                    sqlite_sidecar_path(&decoy_builder, "-wal"),
                    &decoy_wal_sidecar,
                )
                .expect("copy decoy -wal");
                bconn.close().await.expect("close decoy builder");
            }

            let orig_meta = validate_sqlite_file_metadata(&orig_db, DatabaseRole::Repository)
                .expect("validate orig_db metadata");
            let target_wal = sqlite_sidecar_path(&orig_db, "-wal");
            let before_decoy_db = fs::read(&decoy_wal_db).expect("read decoy db before");
            let before_decoy_wal = fs::read(&decoy_wal_sidecar).expect("read decoy -wal before");
            fs::rename(&decoy_wal_db, &orig_db).expect("substitute WAL decoy into orig_db");
            fs::rename(&decoy_wal_sidecar, &target_wal).expect("move decoy -wal alongside orig_db");

            let sub_err =
                quiesce_sqlite_connection_only(&orig_db, DatabaseRole::Repository, &orig_meta)
                    .await
                    .expect_err("substituted WAL database must fail closed BEFORE mode switch");
            assert_eq!(sub_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&orig_db).expect("read substituted WAL .db after"),
                before_decoy_db,
                "substituted WAL .db must NOT be mutated or checkpointed"
            );
            assert_eq!(
                fs::read(&target_wal).expect("read substituted WAL -wal after"),
                before_decoy_wal,
                "substituted WAL -wal must NOT be checkpointed or unlinked"
            );
        }

        // 4. R4 regression: already-DELETE database with a stray stale `-shm`
        //    and an active external DELETE reader holding SHARED lock must fail
        //    closed with `LBR-IO-001` WITHOUT removing or mutating `-shm` or
        //    `.db` (stale `-shm` cleanup is strictly deferred until AFTER
        //    `BEGIN EXCLUSIVE; ROLLBACK;` succeeds).
        {
            let shm_dir = tempdir().expect("create delete+stale-shm dir");
            let del_db = shm_dir.path().join("delete_with_stale_shm.db");
            let stale_shm = sqlite_sidecar_path(&del_db, "-shm");
            create_delete_mode_fixture_db(&del_db).await;
            fs::write(&stale_shm, b"stale-shm-payload-bytes").expect("plant stale -shm");

            let holder = ChildLockHolder::spawn("delete_reader", &del_db);
            let before_db = fs::read(&del_db).expect("snapshot .db before");
            let before_shm = fs::read(&stale_shm).expect("snapshot stale -shm before");

            let err = quiesce_sqlite_source(&del_db, DatabaseRole::Repository)
                .await
                .expect_err("external DELETE reader with stale -shm must fail closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&del_db).expect("snapshot .db after"),
                before_db,
                "DELETE + stale -shm contention must not mutate .db"
            );
            assert_eq!(
                fs::read(&stale_shm).expect("snapshot stale -shm after"),
                before_shm,
                "DELETE + stale -shm contention must NOT remove or mutate stale -shm before EXCLUSIVE lock"
            );
            holder.release_and_wait();

            // R5 regression: repository-pair abort invariance when `libra.db`
            // is already in DELETE mode with a stray stale `-shm` AND
            // `capture.db` is contended — must fail closed with `LBR-IO-001`
            // without deleting or altering `libra.db-shm`, `libra.db`, or
            // `capture.db`.
            let repo_dir = tempdir().expect("create repo stale-shm dir");
            let repo_storage = repo_dir.path().join(".libra");
            let repo_git = git_sqlite_db_path(&repo_storage);
            let repo_cap = capture_sqlite_db_path(&repo_storage);
            let repo_git_shm = sqlite_sidecar_path(&repo_git, "-shm");
            create_delete_mode_fixture_db(&repo_git).await;
            create_delete_mode_fixture_db(&repo_cap).await;
            fs::write(&repo_git_shm, b"stray-delete-repo-shm").expect("plant stray repo -shm");

            let cap_holder = ChildLockHolder::spawn("delete_reader", &repo_cap);
            let before_repo_git = fs::read(&repo_git).expect("read repo libra.db before");
            let before_repo_shm = fs::read(&repo_git_shm).expect("read repo -shm before");
            let before_repo_cap = fs::read(&repo_cap).expect("read repo capture.db before");

            let pair_err = quiesce_forward_repository_sqlite_sources(&repo_storage)
                .await
                .expect_err("DELETE repo with stray -shm + contended capture.db must fail closed");
            assert_eq!(pair_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&repo_git).expect("read repo libra.db after"),
                before_repo_git
            );
            assert_eq!(
                fs::read(&repo_git_shm).expect("read repo -shm after"),
                before_repo_shm,
                "DELETE repo stray -shm must remain byte-identical on abort"
            );
            assert_eq!(
                fs::read(&repo_cap).expect("read repo capture.db after"),
                before_repo_cap
            );
            cap_holder.release_and_wait();
        }

        // 5. R6 Finding 1 regression: repository pair where `libra.db` is an
        //    offline 3-file WAL (`.db` + `-wal` + `-shm` with pending row) AND
        //    `capture.db` has an active WAL reader, WAL writer, or idle WAL
        //    connection (plus DELETE reader). Phase A must acquire both locks
        //    before transitioning either DB, so `libra.db` + `-wal` + `-shm`
        //    AND `capture.db` + `-wal` + `-shm` ALL remain 100% byte-identical!
        for (cap_mode, cap_is_wal) in [
            ("wal_active_reader", true),
            ("wal_active_writer", true),
            ("wal_idle_conn", true),
            ("delete_reader", false),
        ] {
            let mix_dir = tempdir().expect("create mixed offline-WAL + contended-capture dir");
            let builder_db = mix_dir.path().join("builder.db");
            create_wal_mode_fixture_db(&builder_db).await;
            {
                let b_opts = SqliteConnectOptions::new()
                    .filename(&builder_db)
                    .create_if_missing(false)
                    .read_only(false)
                    .busy_timeout(Duration::from_secs(5))
                    .synchronous(SqliteSynchronous::Full);
                let mut bconn = SqliteConnection::connect_with(&b_opts)
                    .await
                    .expect("open offline WAL builder");
                bconn
                    .execute("PRAGMA wal_autocheckpoint = 0;")
                    .await
                    .expect("disable autocheckpoint");
                bconn
                    .execute(
                        "INSERT INTO reference (name, commit_id) VALUES ('refs/heads/mix-offline-wal', '7777777777777777777777777777777777777777');",
                    )
                    .await
                    .expect("insert offline WAL pending row");
                let mix_storage = mix_dir.path().join(".libra");
                fs::create_dir_all(&mix_storage).expect("create .libra");
                let mix_git = git_sqlite_db_path(&mix_storage);
                fs::copy(&builder_db, &mix_git).expect("copy offline git .db");
                fs::copy(
                    sqlite_sidecar_path(&builder_db, "-wal"),
                    sqlite_sidecar_path(&mix_git, "-wal"),
                )
                .expect("copy offline git -wal");
                fs::copy(
                    sqlite_sidecar_path(&builder_db, "-shm"),
                    sqlite_sidecar_path(&mix_git, "-shm"),
                )
                .expect("copy offline git -shm");
                bconn.close().await.expect("close offline WAL builder");
            }

            let mix_storage = mix_dir.path().join(".libra");
            let mix_git = git_sqlite_db_path(&mix_storage);
            let mix_git_wal = sqlite_sidecar_path(&mix_git, "-wal");
            let mix_git_shm = sqlite_sidecar_path(&mix_git, "-shm");
            let mix_cap = capture_sqlite_db_path(&mix_storage);
            let mix_cap_wal = sqlite_sidecar_path(&mix_cap, "-wal");
            let mix_cap_shm = sqlite_sidecar_path(&mix_cap, "-shm");

            if cap_is_wal {
                create_wal_mode_fixture_db(&mix_cap).await;
            } else {
                create_delete_mode_fixture_db(&mix_cap).await;
            }
            let cap_holder = ChildLockHolder::spawn(cap_mode, &mix_cap);

            let before_git_db = fs::read(&mix_git).expect("read mix git .db before");
            let before_git_wal = fs::read(&mix_git_wal).expect("read mix git -wal before");
            let before_git_shm = fs::read(&mix_git_shm).expect("read mix git -shm before");
            let before_cap_db = fs::read(&mix_cap).expect("read mix cap .db before");
            let before_cap_wal =
                cap_is_wal.then(|| fs::read(&mix_cap_wal).expect("read mix cap -wal before"));
            let before_cap_shm =
                cap_is_wal.then(|| fs::read(&mix_cap_shm).expect("read mix cap -shm before"));

            let pair_err = quiesce_forward_repository_sqlite_sources(&mix_storage)
                .await
                .expect_err("offline WAL git + contended capture must fail closed");
            assert_eq!(pair_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&mix_git).expect("read mix git .db after"),
                before_git_db,
                "R6 F1 ({cap_mode}): offline WAL libra.db must NOT be checkpointed when capture.db fails"
            );
            assert_eq!(
                fs::read(&mix_git_wal).expect("read mix git -wal after"),
                before_git_wal,
                "R6 F1 ({cap_mode}): offline WAL libra.db-wal must remain byte-identical when capture.db fails"
            );
            assert_eq!(
                fs::read(&mix_git_shm).expect("read mix git -shm after"),
                before_git_shm,
                "R6 F1 ({cap_mode}): offline WAL libra.db-shm must remain byte-identical when capture.db fails"
            );
            assert!(
                !wal_shm_cleanup_marker_path(&mix_git).exists(),
                "R6 F1 ({cap_mode}): no cleanup marker may be written on Phase A contention"
            );
            assert_eq!(
                fs::read(&mix_cap).expect("read mix cap .db after"),
                before_cap_db
            );
            if let Some(expected_wal) = before_cap_wal {
                assert_eq!(
                    fs::read(&mix_cap_wal).expect("read mix cap -wal after"),
                    expected_wal
                );
            }
            if let Some(expected_shm) = before_cap_shm {
                assert_eq!(
                    fs::read(&mix_cap_shm).expect("read mix cap -shm after"),
                    expected_shm
                );
            }
            cap_holder.release_and_wait();
        }

        // 6. R6 Finding 2 regressions: interrupted stale `-shm` cleanup between
        //    `PRAGMA journal_mode = DELETE;` and `-shm` unlink (both crash and
        //    I/O removal error) persists the durable identity-bound marker so
        //    retry automatically resumes and cleans up `-shm` + marker, while
        //    preserving fail-closed rejection on contention and unrelated/forged
        //    stray `-shm`.
        {
            async fn build_offline_3file_wal(target_db: &Path, row_name: &str) {
                let parent = target_db.parent().expect("target parent");
                fs::create_dir_all(parent).expect("create target parent");
                let builder_db = parent.join(format!("builder_{row_name}.db"));
                create_wal_mode_fixture_db(&builder_db).await;
                let opts = SqliteConnectOptions::new()
                    .filename(&builder_db)
                    .create_if_missing(false)
                    .read_only(false)
                    .busy_timeout(Duration::from_secs(5))
                    .synchronous(SqliteSynchronous::Full);
                let mut bconn = SqliteConnection::connect_with(&opts)
                    .await
                    .expect("open 3file builder");
                bconn
                    .execute("PRAGMA wal_autocheckpoint = 0;")
                    .await
                    .expect("disable autocheckpoint");
                let ref_name = format!("refs/heads/{row_name}");
                sea_orm::sqlx::query(
                    "INSERT INTO reference (name, commit_id) VALUES (?, '8888888888888888888888888888888888888888');",
                )
                .bind(ref_name)
                .execute(&mut bconn)
                .await
                .expect("insert pending row");
                fs::copy(&builder_db, target_db).expect("copy 3file .db");
                fs::copy(
                    sqlite_sidecar_path(&builder_db, "-wal"),
                    sqlite_sidecar_path(target_db, "-wal"),
                )
                .expect("copy 3file -wal");
                fs::copy(
                    sqlite_sidecar_path(&builder_db, "-shm"),
                    sqlite_sidecar_path(target_db, "-shm"),
                )
                .expect("copy 3file -shm");
                bconn.close().await.expect("close 3file builder");
                let _ = fs::remove_file(&builder_db);
                let _ = fs::remove_file(sqlite_sidecar_path(&builder_db, "-wal"));
                let _ = fs::remove_file(sqlite_sidecar_path(&builder_db, "-shm"));
            }

            // (a) Crash right after `PRAGMA journal_mode = DELETE;` and before
            //     removing stale `-shm`.
            let crash_dir = tempdir().expect("create crash resume dir");
            let crash_db = crash_dir.path().join("crash_resume.db");
            let crash_wal = sqlite_sidecar_path(&crash_db, "-wal");
            let crash_shm = sqlite_sidecar_path(&crash_db, "-shm");
            let crash_marker = wal_shm_cleanup_marker_path(&crash_db);
            build_offline_3file_wal(&crash_db, "crash-resume").await;

            let meta = validate_sqlite_file_metadata(&crash_db, DatabaseRole::Repository)
                .expect("validate crash_db");
            let abort_after_delete = |_p: &Path| -> StorageResult<()> {
                Err(StorageBackendError::BackendInternal {
                    detail: "simulated crash after DELETE transition before -shm unlink"
                        .to_string(),
                })
            };
            let crash_err = quiesce_sqlite_connection_only_with_hooks(
                &crash_db,
                DatabaseRole::Repository,
                &meta,
                &QuiesceTestHooks {
                    after_delete_before_shm_remove: Some(&abort_after_delete),
                    ..Default::default()
                },
            )
            .await
            .expect_err("simulated crash before -shm unlink must fail");
            assert_eq!(crash_err.stable_code_str(), "LBR-IO-001");
            assert!(!crash_wal.exists(), "-wal was already removed by SQLite");
            assert!(
                crash_shm.exists(),
                "stale -shm remains after simulated crash"
            );
            assert!(
                crash_marker.exists(),
                "durable cleanup marker must persist on crash"
            );

            // While an external DELETE reader holds SHARED lock, retry MUST
            // fail closed without removing `-shm` or the marker.
            let reader_holder = ChildLockHolder::spawn("delete_reader", &crash_db);
            let snap_db = fs::read(&crash_db).expect("snap crash_db");
            let snap_shm = fs::read(&crash_shm).expect("snap crash_shm");
            let snap_marker = fs::read(&crash_marker).expect("snap crash_marker");
            let busy_retry_err = quiesce_sqlite_source(&crash_db, DatabaseRole::Repository)
                .await
                .expect_err("contended retry during cleanup resume must fail closed");
            assert_eq!(busy_retry_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(fs::read(&crash_db).expect("check db"), snap_db);
            assert_eq!(fs::read(&crash_shm).expect("check shm"), snap_shm);
            assert_eq!(fs::read(&crash_marker).expect("check marker"), snap_marker);
            reader_holder.release_and_wait();

            // Once contention clears, retry automatically resumes cleanup,
            // deletes `-shm` and marker, preserves pending row, and holds
            // OS EXCLUSIVE lock.
            let mut resumed = quiesce_sqlite_source(&crash_db, DatabaseRole::Repository)
                .await
                .expect("retry after post-DELETE crash must resume and succeed");
            assert_eq!(resumed.journal_mode(), "delete");
            assert!(!crash_shm.exists(), "resumed retry must remove stale -shm");
            assert!(
                !crash_marker.exists(),
                "resumed retry must remove cleanup marker"
            );
            let row_present: i64 = sea_orm::sqlx::query_scalar(
                "SELECT count(*) FROM reference WHERE name = 'refs/heads/crash-resume';",
            )
            .fetch_one(resumed.connection_mut())
            .await
            .expect("verify pending row survived crash+resume");
            assert_eq!(row_present, 1);
            run_child_busy_probe("probe_read_busy", &crash_db);
            run_child_busy_probe("probe_write_busy", &crash_db);
            resumed.close().await.expect("close resumed db");

            // (b) Cleanup I/O error during `-shm` unlink leaves marker intact;
            //     next retry after error clears succeeds.
            let io_dir = tempdir().expect("create shm io-error dir");
            let io_db = io_dir.path().join("io_resume.db");
            let io_shm = sqlite_sidecar_path(&io_db, "-shm");
            let io_marker = wal_shm_cleanup_marker_path(&io_db);
            build_offline_3file_wal(&io_db, "io-resume").await;

            let io_meta = validate_sqlite_file_metadata(&io_db, DatabaseRole::Repository)
                .expect("validate io_db");
            let fail_remove = |_p: &Path| -> io::Result<()> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "simulated shm unlink EACCES",
                ))
            };
            let io_err = quiesce_sqlite_connection_only_with_hooks(
                &io_db,
                DatabaseRole::Repository,
                &io_meta,
                &QuiesceTestHooks {
                    shm_remove_override: Some(&fail_remove),
                    ..Default::default()
                },
            )
            .await
            .expect_err("simulated shm unlink error must fail closed");
            assert_eq!(io_err.stable_code_str(), "LBR-IO-001");
            assert!(io_shm.exists());
            assert!(io_marker.exists());

            let io_resumed = quiesce_sqlite_source(&io_db, DatabaseRole::Repository)
                .await
                .expect("retry after shm unlink I/O error must resume and succeed");
            assert!(!io_shm.exists());
            assert!(!io_marker.exists());
            io_resumed.close().await.expect("close io-resumed db");

            // (c) Crash after `-shm` was removed but before marker was unlinked
            //     must clean up the obsolete marker on retry.
            let obs_marker = wal_shm_cleanup_marker_path(&io_db);
            fs::write(
                &obs_marker,
                format!("{WAL_SHM_CLEANUP_MARKER_HEADER}\nshm_len=32768\nshm_dev=1\nshm_ino=2\n"),
            )
            .expect("plant obsolete marker after shm gone");
            let obs_resumed = quiesce_sqlite_source(&io_db, DatabaseRole::Repository)
                .await
                .expect("obsolete marker after shm removal must be cleaned cleanly");
            assert!(!obs_marker.exists());
            obs_resumed.close().await.expect("close obsolete-marker db");

            // (d) Mismatched/forged marker alongside an unrelated stray `-shm`
            //     must still fail closed with ZERO disk mutation.
            let forge_dir = tempdir().expect("create forged marker dir");
            let forge_db = forge_dir.path().join("forged.db");
            let forge_shm = sqlite_sidecar_path(&forge_db, "-shm");
            let forge_marker = wal_shm_cleanup_marker_path(&forge_db);
            create_delete_mode_fixture_db(&forge_db).await;
            fs::write(&forge_shm, b"unrelated-stray-shm").expect("plant unrelated -shm");
            fs::write(
                &forge_marker,
                format!("{WAL_SHM_CLEANUP_MARKER_HEADER}\nshm_len=999999\nshm_dev=0\nshm_ino=0\n"),
            )
            .expect("plant mismatched marker");
            let before_forge_db = fs::read(&forge_db).expect("read forge_db");
            let before_forge_shm = fs::read(&forge_shm).expect("read forge_shm");
            let before_forge_marker = fs::read(&forge_marker).expect("read forge_marker");
            let forge_err = quiesce_sqlite_source(&forge_db, DatabaseRole::Repository)
                .await
                .expect_err("mismatched cleanup marker must fail closed");
            assert_eq!(forge_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&forge_db).expect("after forge_db"),
                before_forge_db
            );
            assert_eq!(
                fs::read(&forge_shm).expect("after forge_shm"),
                before_forge_shm
            );
            assert_eq!(
                fs::read(&forge_marker).expect("after forge_marker"),
                before_forge_marker
            );
        }

        // 7. R6 Finding 3 regressions: DELETE-mode database with stray `-wal`
        //    alone, stray `-wal` + `-shm` together, and uncontended stray `-shm`
        //    alone (without marker) must ALL fail closed with `LBR-IO-001` and
        //    100% byte-identical `.db` / `-wal` / `-shm`.
        for (plant_wal, plant_shm, case_label) in [
            (true, false, "DELETE + stray -wal alone"),
            (true, true, "DELETE + stray -wal and -shm together"),
            (
                false,
                true,
                "DELETE + uncontended stray -shm alone (no marker)",
            ),
        ] {
            let stray_dir = tempdir().expect("create stray DELETE sidecar dir");
            let stray_db = stray_dir.path().join("stray_delete.db");
            let stray_wal = sqlite_sidecar_path(&stray_db, "-wal");
            let stray_shm = sqlite_sidecar_path(&stray_db, "-shm");
            create_delete_mode_fixture_db(&stray_db).await;
            if plant_wal {
                fs::write(&stray_wal, b"stray-wal-bytes-do-not-touch").expect("plant stray -wal");
            }
            if plant_shm {
                fs::write(&stray_shm, b"stray-shm-bytes-do-not-touch").expect("plant stray -shm");
            }
            let before_db = fs::read(&stray_db).expect("snapshot stray .db before");
            let before_wal =
                plant_wal.then(|| fs::read(&stray_wal).expect("snapshot stray -wal before"));
            let before_shm =
                plant_shm.then(|| fs::read(&stray_shm).expect("snapshot stray -shm before"));

            let stray_err = quiesce_sqlite_source(&stray_db, DatabaseRole::Repository)
                .await
                .expect_err("stray sidecar alongside DELETE db must fail closed");
            assert_eq!(stray_err.stable_code_str(), "LBR-IO-001", "{case_label}");
            assert_eq!(
                fs::read(&stray_db).expect("snapshot stray .db after"),
                before_db,
                "{case_label}: .db must remain byte-identical"
            );
            if let Some(expected_wal) = before_wal {
                assert_eq!(
                    fs::read(&stray_wal).expect("snapshot stray -wal after"),
                    expected_wal,
                    "{case_label}: stray -wal must remain byte-identical"
                );
            }
            if let Some(expected_shm) = before_shm {
                assert_eq!(
                    fs::read(&stray_shm).expect("snapshot stray -shm after"),
                    expected_shm,
                    "{case_label}: stray -shm must remain byte-identical"
                );
            }
        }
    }
}
