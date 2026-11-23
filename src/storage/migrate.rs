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
/// `PRAGMA locking_mode = EXCLUSIVE`, and `PRAGMA synchronous = FULL`, verify
/// `DELETE` journal mode, and execute a no-write `BEGIN EXCLUSIVE; ROLLBACK;`
/// to prove zero concurrent SQLite writers or active `DELETE`-mode readers
/// (`ADR-RB-07` #1, `RB-10`).
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

    // Fast non-opening lstat reject when WAL sidecars already exist on disk
    // (WAL -> DELETE quiescence is implemented in `RB-10m`).
    if path_exists_no_open(&sqlite_sidecar_path(db_path, "-wal"))?
        || path_exists_no_open(&sqlite_sidecar_path(db_path, "-shm"))?
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: WAL sidecar present (requires WAL quiescence)",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    }

    quiesce_sqlite_connection_only(db_path, role, &expected_meta).await
}

/// Open the sole non-pooled `SqliteConnection` and run the Phase 1 pragma +
/// no-write `BEGIN EXCLUSIVE; ROLLBACK;` sequence with zero raw `File::open`
/// descriptors touched on `db_path`, verifying that the opened SQLite file
/// descriptor remains bound to `expected_meta`.
async fn quiesce_sqlite_connection_only(
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
) -> StorageResult<QuiescedSqliteConnection> {
    #[cfg(test)]
    let after_begin_hook: Option<fn(&Path)> = None;
    #[cfg(test)]
    {
        quiesce_sqlite_connection_only_inner(db_path, role, expected_meta, after_begin_hook).await
    }
    #[cfg(not(test))]
    {
        quiesce_sqlite_connection_only_inner(db_path, role, expected_meta).await
    }
}

async fn quiesce_sqlite_connection_only_inner(
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
    #[cfg(test)] after_begin_hook: Option<fn(&Path)>,
) -> StorageResult<QuiescedSqliteConnection> {
    // Open the sole non-pooled connection with busy_timeout = 0 and no implicit
    // schema-reading pragmas (`synchronous` / `journal_mode` are configured in
    // `quiesce_open_connection` AFTER `BEGIN EXCLUSIVE` so an external
    // EXCLUSIVE writer fails deterministically at `BEGIN EXCLUSIVE` rather than
    // during `connect_with`).
    let options = SqliteConnectOptions::new()
        .filename(db_path)
        .create_if_missing(false)
        .read_only(false)
        .shared_cache(false)
        .busy_timeout(Duration::ZERO);

    let mut conn = SqliteConnection::connect_with(&options)
        .await
        .map_err(|err| map_quiesce_sqlite_error(role, SqliteQuiesceStage::Connect, &err))?;

    let quiesce_result = quiesce_open_connection(
        &mut conn,
        db_path,
        role,
        expected_meta,
        #[cfg(test)]
        after_begin_hook,
    )
    .await;
    match quiesce_result {
        Ok(journal_mode) => Ok(QuiescedSqliteConnection {
            conn,
            role,
            path: db_path.to_path_buf(),
            journal_mode,
        }),
        Err(err) => {
            let _ = conn.close().await;
            Err(err)
        }
    }
}

/// Run the Phase 1 pragma + `DELETE`-mode no-write `BEGIN EXCLUSIVE; ROLLBACK;`
/// sequence on an already opened non-pooled `SqliteConnection`.
async fn quiesce_open_connection(
    conn: &mut SqliteConnection,
    db_path: &Path,
    role: DatabaseRole,
    expected_meta: &fs::Metadata,
    #[cfg(test)] after_begin_hook: Option<fn(&Path)>,
) -> StorageResult<String> {
    // Check `-wal` existence via `lstat` BEFORE `BEGIN EXCLUSIVE` (zero fd
    // opened):
    // - If `-wal` ALREADY exists on disk, immediately enable
    //   `SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE = 1` so pending WAL frames can never
    //   be checkpointed on error or future cancellation.
    // - If `-wal` does NOT exist yet, keep `NO_CKPT_ON_CLOSE = 0` across
    //   `BEGIN EXCLUSIVE`: on a cleanly-closed WAL database (header[18..=19] == 2,
    //   0 sidecars), `sqlite3WalOpen` opens an empty 0-byte `-wal` under
    //   `WAL_HEAPMEMORY_MODE` (zero `-shm`), and keeping `NO_CKPT_ON_CLOSE = 0`
    //   guarantees `sqlite3WalClose` atomically unlinks that 0-byte `-wal` with
    //   0 writes to `.db` EVEN IF this future is cancelled or fails immediately
    //   after `BEGIN EXCLUSIVE`.
    let wal_path = sqlite_sidecar_path(db_path, "-wal");
    let wal_existed_before_begin = path_exists_no_open(&wal_path)?;
    set_wal_checkpoint_on_close(conn, role, wal_existed_before_begin).await?;

    // Explicitly enforce busy_timeout = 0 (does not read schema or acquire locks).
    conn.execute("PRAGMA busy_timeout = 0;")
        .await
        .map_err(|err| {
            map_quiesce_sqlite_error(role, SqliteQuiesceStage::ConfigurePragmas, &err)
        })?;

    // Set `PRAGMA locking_mode = EXCLUSIVE` BEFORE `BEGIN EXCLUSIVE`. In SQLite,
    // `PragTyp_LOCKING_MODE` does NOT set `PragFlg_NeedSchema`, so this pragma
    // neither reads page 1 nor acquires any lock (external EXCLUSIVE writers
    // still fail deterministically at `BEGIN EXCLUSIVE` below). Setting it here
    // ALSO forces `WAL_HEAPMEMORY_MODE` (`bNoShm = 1`) on any WAL database,
    // ensuring SQLite NEVER creates or modifies a `-shm` sidecar and immediately
    // fails with `SQLITE_BUSY` if any external WAL connection is open.
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

    // Acquire the OS-level EXCLUSIVE lock as the VERY FIRST lock-acquiring
    // statement on `c_mig` so every active external writer (RESERVED or
    // EXCLUSIVE) and every active DELETE-mode reader (SHARED) fails
    // deterministically at `BEGIN EXCLUSIVE` with `SQLITE_BUSY` -> `LBR-IO-001`
    // (`RB-10` AC1/AC3).
    conn.execute("BEGIN EXCLUSIVE;")
        .await
        .map_err(|err| map_quiesce_sqlite_error(role, SqliteQuiesceStage::BeginExclusive, &err))?;

    // Defensive check: if a concurrent process raced in and left a non-empty
    // `-wal` between `wal_existed_before_begin` and `BEGIN EXCLUSIVE`, protect
    // its frames from close-time checkpointing immediately.
    if !wal_existed_before_begin && fs::symlink_metadata(&wal_path).is_ok_and(|meta| meta.len() > 0)
    {
        set_wal_checkpoint_on_close(conn, role, true).await?;
    }

    #[cfg(test)]
    if let Some(hook) = after_begin_hook {
        hook(db_path);
    }

    // Strictly under the held EXCLUSIVE lock:
    // 1. Inspect `PRAGMA journal_mode` under the lock (zero TOCTOU).
    // 2. End the no-write probe transaction via `ROLLBACK` (zero bytes written;
    //    `PRAGMA locking_mode = EXCLUSIVE` retains the OS EXCLUSIVE lock across
    //    `ROLLBACK` until `conn` is closed).
    let journal_mode_res: Result<String, sea_orm::sqlx::Error> =
        sea_orm::sqlx::query_scalar("PRAGMA journal_mode;")
            .fetch_one(&mut *conn)
            .await;
    let rollback_res = conn.execute("ROLLBACK;").await;

    let journal_mode = journal_mode_res.map_err(|err| {
        map_quiesce_sqlite_error(role, SqliteQuiesceStage::InspectJournalMode, &err)
    })?;
    rollback_res
        .map_err(|err| map_quiesce_sqlite_error(role, SqliteQuiesceStage::RollbackProbe, &err))?;

    // Bind the live locked SQLite descriptor to the pre-validated regular
    // non-symlink inode (catches rename/symlink substitution and swap-back
    // races between pre-open validation and `connect_with`).
    let post_meta = validate_sqlite_file_metadata(db_path, role)?;
    if !same_file_identity(db_path, expected_meta, db_path, &post_meta)? {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: source sqlite path identity changed during open",
                SqliteQuiesceStage::PathValidation.as_str()
            ),
        });
    }
    verify_connection_not_moved(conn, role).await?;

    let wal_companion_exists = path_exists_no_open(&wal_path)?;
    let shm_companion_exists = path_exists_no_open(&sqlite_sidecar_path(db_path, "-shm"))?;
    if !journal_mode.eq_ignore_ascii_case("delete") || wal_companion_exists || shm_companion_exists
    {
        return Err(StorageBackendError::BackendInternal {
            detail: format!(
                "forward migration quiescence rejected for {role} at {}: non-DELETE journal mode (`{journal_mode}`) or WAL sidecar present (requires WAL quiescence)",
                SqliteQuiesceStage::InspectJournalMode.as_str()
            ),
        });
    }

    // Configure `PRAGMA synchronous = FULL` outside the rolled-back transaction
    // (SQLite forbids changing safety level inside a transaction) while the
    // OS-level EXCLUSIVE lock remains continuously held by `locking_mode = EXCLUSIVE`.
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

    Ok(journal_mode.to_ascii_lowercase())
}

/// Quiesce the repository's SQLite source databases (`<common_storage>/libra.db`
/// and, when present, `<common_storage>/private/capture.db`) using single
/// non-pooled connections (`c_mig`).
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
    let capture_present = match fs::symlink_metadata(&capture_path) {
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
            true
        }
        Err(err) if err.kind() == io::ErrorKind::NotFound => false,
        Err(err) => {
            return Err(StorageBackendError::io(
                "inspect capture sqlite database metadata",
                err,
            ));
        }
    };

    let git = quiesce_sqlite_source(&git_path, DatabaseRole::Repository).await?;
    let capture = if capture_present {
        match quiesce_sqlite_source(&capture_path, DatabaseRole::Capture).await {
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

        // Offline WAL & raced WAL zero-mutation regressions:
        // 1. Offline WAL with pending `-wal` frames via `quiesce_sqlite_source`
        //    (rejected via non-opening lstat sidecar check).
        // 2. Raced offline WAL with pending `-wal` frames via
        //    `quiesce_sqlite_connection_only` (bypassing pre-open lstat):
        //    `locking_mode = EXCLUSIVE` forces `WAL_HEAPMEMORY_MODE` (zero `-shm`)
        //    and `NO_CKPT_ON_CLOSE = 1` prevents close-time checkpointing.
        // 3. Cleanly-closed WAL (header[18..=19] == 2, zero sidecars) via
        //    `quiesce_sqlite_source` (zero raw fd opened): leaves `.db`
        //    byte-identical and leaves neither `-wal` nor `-shm` on disk.
        {
            let wal_tmp_dir = tempdir().expect("create offline WAL temp dir");
            let live_wal_db = wal_tmp_dir.path().join("builder.db");
            let opts = SqliteConnectOptions::new()
                .filename(&live_wal_db)
                .create_if_missing(true)
                .read_only(false)
                .busy_timeout(Duration::from_secs(5))
                .synchronous(SqliteSynchronous::Full);
            let mut builder_conn = SqliteConnection::connect_with(&opts)
                .await
                .expect("create offline WAL builder db");
            let mode: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode = WAL;")
                .fetch_one(&mut builder_conn)
                .await
                .expect("enable WAL on builder db");
            assert_eq!(mode.to_ascii_lowercase(), "wal");
            builder_conn
                .execute("PRAGMA wal_autocheckpoint = 0;")
                .await
                .expect("disable wal autocheckpoint");
            builder_conn
                .execute("CREATE TABLE reference (id INTEGER PRIMARY KEY, name TEXT NOT NULL);")
                .await
                .expect("create WAL fixture table");
            builder_conn
                .execute("INSERT INTO reference (id, name) VALUES (1, 'refs/heads/wal-pending');")
                .await
                .expect("insert pending WAL row");

            let offline_wal_db = wal_tmp_dir.path().join("offline_wal.db");
            let offline_wal_sidecar = sqlite_sidecar_path(&offline_wal_db, "-wal");
            let offline_shm_sidecar = sqlite_sidecar_path(&offline_wal_db, "-shm");
            fs::copy(&live_wal_db, &offline_wal_db).expect("copy offline WAL main db");
            fs::copy(
                sqlite_sidecar_path(&live_wal_db, "-wal"),
                &offline_wal_sidecar,
            )
            .expect("copy offline WAL sidecar");
            builder_conn.close().await.expect("close WAL builder db");

            let before_wal_db = fs::read(&offline_wal_db).expect("read offline WAL db before");
            let before_wal_frames =
                fs::read(&offline_wal_sidecar).expect("read offline WAL sidecar before");
            assert!(
                !before_wal_frames.is_empty(),
                "offline WAL fixture must contain pending frames in -wal"
            );
            assert!(!offline_shm_sidecar.exists());

            // Pre-open lstat path
            let err = quiesce_sqlite_source(&offline_wal_db, DatabaseRole::Repository)
                .await
                .expect_err("RB-10 must reject offline WAL source fail-closed");
            assert_eq!(err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&offline_wal_db).expect("read offline WAL db after"),
                before_wal_db
            );
            assert_eq!(
                fs::read(&offline_wal_sidecar).expect("read offline WAL sidecar after"),
                before_wal_frames
            );
            assert!(!offline_shm_sidecar.exists());

            // Direct connection path (simulates TOCTOU transition to WAL after lstat)
            let offline_wal_meta =
                validate_sqlite_file_metadata(&offline_wal_db, DatabaseRole::Repository)
                    .expect("validate offline WAL metadata");
            let toctou_err = quiesce_sqlite_connection_only(
                &offline_wal_db,
                DatabaseRole::Repository,
                &offline_wal_meta,
            )
            .await
            .expect_err("raced offline WAL source must fail closed");
            assert_eq!(toctou_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&offline_wal_db).expect("read offline WAL db after TOCTOU"),
                before_wal_db
            );
            assert_eq!(
                fs::read(&offline_wal_sidecar).expect("read offline WAL sidecar after TOCTOU"),
                before_wal_frames
            );
            assert!(
                !offline_shm_sidecar.exists(),
                "raced offline WAL rejection must NOT create a -shm sidecar"
            );

            // Cleanly-closed WAL database (header[18..=19] == 2, zero sidecars)
            let clean_wal_db = wal_tmp_dir.path().join("clean_closed_wal.db");
            let clean_wal_sidecar = sqlite_sidecar_path(&clean_wal_db, "-wal");
            let clean_shm_sidecar = sqlite_sidecar_path(&clean_wal_db, "-shm");
            {
                let clean_opts = SqliteConnectOptions::new()
                    .filename(&clean_wal_db)
                    .create_if_missing(true)
                    .read_only(false)
                    .busy_timeout(Duration::from_secs(5))
                    .synchronous(SqliteSynchronous::Full);
                let mut clean_conn = SqliteConnection::connect_with(&clean_opts)
                    .await
                    .expect("create clean WAL fixture");
                let m: String = sea_orm::sqlx::query_scalar("PRAGMA journal_mode = WAL;")
                    .fetch_one(&mut clean_conn)
                    .await
                    .expect("enable WAL on clean fixture");
                assert_eq!(m.to_ascii_lowercase(), "wal");
                clean_conn
                    .execute("CREATE TABLE reference (id INTEGER PRIMARY KEY, name TEXT NOT NULL);")
                    .await
                    .expect("create table on clean WAL fixture");
                clean_conn.close().await.expect("cleanly close WAL fixture");
            }
            assert!(!clean_wal_sidecar.exists());
            assert!(!clean_shm_sidecar.exists());
            let before_clean_wal_db = fs::read(&clean_wal_db).expect("read clean WAL db before");

            let clean_wal_err = quiesce_sqlite_source(&clean_wal_db, DatabaseRole::Repository)
                .await
                .expect_err("cleanly-closed WAL source must fail closed");
            assert_eq!(clean_wal_err.stable_code_str(), "LBR-IO-001");
            assert_eq!(
                fs::read(&clean_wal_db).expect("read clean WAL db after"),
                before_clean_wal_db
            );
            assert!(
                !clean_wal_sidecar.exists(),
                "cleanly-closed WAL rejection must not leave a -wal sidecar behind"
            );
            assert!(
                !clean_shm_sidecar.exists(),
                "cleanly-closed WAL rejection must not create a -shm sidecar"
            );

            // R5 regression 2: early failure/cancellation IMMEDIATELY after
            // `BEGIN EXCLUSIVE` on a cleanly-closed WAL database (before
            // `PRAGMA journal_mode` or `ROLLBACK` runs) must STILL unlink any
            // empty 0-byte `-wal` opened by `sqlite3WalOpen` with zero mutation
            // to `.db` and zero `-shm` created.
            {
                let cancel_opts = SqliteConnectOptions::new()
                    .filename(&clean_wal_db)
                    .create_if_missing(false)
                    .read_only(false)
                    .shared_cache(false)
                    .busy_timeout(Duration::ZERO);
                let mut cancel_conn = SqliteConnection::connect_with(&cancel_opts)
                    .await
                    .expect("open cleanly-closed WAL for cancellation test");
                set_wal_checkpoint_on_close(&mut cancel_conn, DatabaseRole::Repository, false)
                    .await
                    .expect("keep NO_CKPT_ON_CLOSE=0 when -wal absent");
                let lm: String = sea_orm::sqlx::query_scalar("PRAGMA locking_mode = EXCLUSIVE;")
                    .fetch_one(&mut cancel_conn)
                    .await
                    .expect("set exclusive locking_mode");
                assert_eq!(lm.to_ascii_lowercase(), "exclusive");
                cancel_conn
                    .execute("BEGIN EXCLUSIVE;")
                    .await
                    .expect("BEGIN EXCLUSIVE opens empty -wal under WAL_HEAPMEMORY_MODE");
                // Simulate cancellation immediately after BEGIN EXCLUSIVE:
                // drop `cancel_conn` without running journal_mode/ROLLBACK/close.
                drop(cancel_conn);
                let deadline = Instant::now() + Duration::from_secs(2);
                while clean_wal_sidecar.exists() && Instant::now() < deadline {
                    tokio::time::sleep(Duration::from_millis(2)).await;
                }
                assert!(
                    !clean_wal_sidecar.exists(),
                    "cancellation immediately after BEGIN EXCLUSIVE must still unlink empty -wal"
                );
                assert!(
                    !clean_shm_sidecar.exists(),
                    "cancellation immediately after BEGIN EXCLUSIVE must never create -shm"
                );
                assert_eq!(
                    fs::read(&clean_wal_db).expect("read clean WAL db after cancel"),
                    before_clean_wal_db,
                    "cancellation cleanup must leave main .db byte-identical"
                );
            }

            // R5 regression 1: controlled path-substitution between pre-open
            // metadata validation and `connect_with` (both persistent rename
            // AND swap-and-swap-back) must be caught by `SQLITE_FCNTL_HAS_MOVED`
            // / post-lock identity binding and fail closed with LBR-IO-001.
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

                let sub_err = quiesce_sqlite_connection_only(
                    &target_db,
                    DatabaseRole::Repository,
                    &orig_meta,
                )
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
                let swap_back_err = quiesce_sqlite_connection_only_inner(
                    &target_db,
                    DatabaseRole::Repository,
                    &orig_meta,
                    Some(restore_original_hook),
                )
                .await
                .expect_err(
                    "swap-and-swap-back substitution must be caught by SQLITE_FCNTL_HAS_MOVED",
                );
                assert_eq!(swap_back_err.stable_code_str(), "LBR-IO-001");
            }
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
}
